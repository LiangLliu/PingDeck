//! Route and key verdicts: whether one `POST {}` answer proves a protocol's route exists, and
//! what the credentialed answers of a scan say about the key. Pure functions over
//! [`SafeResponse`] and plain statuses.

use serde_json::Value;

use super::{
    auth::status_passed_auth,
    types::{KeyCheckStatus, KeyEvidence, RouteVerdict},
};
use crate::llm::{
    failure::{FailureKind, classify},
    safe_http::SafeResponse,
    wire::Protocol,
};

fn is_object_with(value: &Value, key: &str) -> bool {
    value.get(key).is_some_and(Value::is_object)
}

/// Whether `body` is the host's own structured error, whichever protocol the route speaks: a
/// FastAPI/TypeSafe `detail` (a string, an object or the validation array) or a top-level `code`
/// with a `message` (new-api and its forks, and the relay's own envelope).
fn structured_error(body: &Value) -> bool {
    let text = |value: Option<&Value>| {
        value
            .and_then(Value::as_str)
            .is_some_and(|text| !text.trim().is_empty())
    };
    let detail = body
        .get("detail")
        .is_some_and(|detail| detail.is_string() || detail.is_object() || detail.is_array());
    detail || (text(body.get("code")) && text(body.get("message")))
}

/// Whether `body` is the error envelope of `protocol`'s own API family, or the host's own
/// structured error: either proves the route exists.
fn envelope_matches(protocol: Protocol, body: &Value) -> bool {
    if structured_error(body) {
        return true;
    }
    let message = |error: &Value| error.get("message").is_some_and(Value::is_string);
    match protocol {
        Protocol::AnthropicMessages => {
            body.get("type").and_then(Value::as_str) == Some("error")
                && is_object_with(body, "error")
        }
        Protocol::Gemini => body.get("error").is_some_and(|error| {
            message(error) && (error.get("code").is_some() || error.get("status").is_some())
        }),
        _ => body
            .get("error")
            .is_some_and(|error| error.is_object() && message(error)),
    }
}

/// Whether an error body says the model or deployment is unknown: the route itself exists.
fn model_not_found(body: &str) -> bool {
    let body = body.to_ascii_lowercase();
    (body.contains("model") || body.contains("deployment"))
        && [
            "not found",
            "notfound",
            "does not exist",
            "not exist",
            "no such",
            "unknown",
        ]
        .iter()
        .any(|needle| body.contains(needle))
}

/// Whether the response body is JSON or an event stream, the only bodies a route answers with.
fn api_shaped(response: &SafeResponse, json: Option<&Value>) -> bool {
    json.is_some()
        || response
            .header("content-type")
            .is_some_and(|value| value.to_ascii_lowercase().contains("text/event-stream"))
}

/// The verdict of one route probe. `list_answered` says whether a model list of this protocol
/// already answered, which turns a bare 401/403 into "the route exists".
#[must_use]
pub(super) fn classify_route(
    protocol: Protocol,
    response: &SafeResponse,
    json: Option<&Value>,
    list_answered: bool,
) -> RouteVerdict {
    match response.status {
        // Authentication passed and the request reached a handler: that is what proves the
        // route, so this set is exactly `status_passed_auth`.
        status if status_passed_auth(status) => {
            if response.looks_like_html() || !api_shaped(response, json) {
                RouteVerdict::Absent
            } else {
                RouteVerdict::Exists
            }
        }
        401 | 403 => {
            let matches = json.is_some_and(|body| envelope_matches(protocol, body));
            if matches || list_answered {
                RouteVerdict::Exists
            } else {
                RouteVerdict::Unclear
            }
        }
        404 => {
            if json.is_some() && model_not_found(&response.text()) {
                RouteVerdict::Exists
            } else {
                RouteVerdict::Absent
            }
        }
        405 | 501 => RouteVerdict::Absent,
        // 3xx, 5xx and anything else give no verdict.
        _ => RouteVerdict::Unclear,
    }
}

/// The evidence `conclusion` of a route probe or verification: `route:<protocol>`.
pub(super) fn route_conclusion(protocol: Protocol) -> String {
    format!("route:{}", protocol.as_str())
}

/// Everything the key verdict looks at.
#[derive(Debug, Clone, Copy)]
pub(super) struct KeyFacts<'a> {
    /// Whether a key was typed.
    pub has_key: bool,
    /// The credentialed requests: `(status, error body text)` of the model list and of every
    /// route probe.
    pub responses: &'a [(u16, &'a str)],
    /// Status of the credentialed model-list request.
    pub models_status: Option<u16>,
    /// A `401` could not be retried with the second credential variant for lack of budget.
    pub auth_budget_gap: bool,
    /// The scan stopped early (a web page or a bot challenge).
    pub stopped: bool,
    /// The key was held back until the user confirms sending it.
    pub needs_confirmation: bool,
}

/// The verdict before the scan-level overrides, with the reason of an `Untested` one.
fn key_status(
    facts: &KeyFacts<'_>,
    route_statuses: &[(Protocol, u16)],
) -> (KeyCheckStatus, Option<&'static str>) {
    if !facts.has_key {
        return (KeyCheckStatus::Untested, Some("no_key"));
    }
    if facts.responses.is_empty() {
        return (KeyCheckStatus::Untested, Some("unreachable"));
    }
    let denied = |status: u16| matches!(status, 401 | 403);
    let quota = facts.responses.iter().any(|(status, body)| {
        *status == 402 || (*status >= 400 && classify(Some(*status), body) == FailureKind::Quota)
    });
    if quota {
        return (KeyCheckStatus::QuotaExhausted, None);
    }
    let models_denied = facts.models_status.is_some_and(denied);
    let route_passed = route_statuses
        .iter()
        .any(|(_, status)| matches!(status, 400 | 422 | 429));
    if models_denied && route_passed {
        return (KeyCheckStatus::ModelsBlockedButChatWorks, None);
    }
    if facts
        .responses
        .iter()
        .any(|(status, _)| (200..300).contains(status))
    {
        return (KeyCheckStatus::Accepted, None);
    }
    if facts.responses.iter().all(|(status, _)| denied(*status)) {
        return (KeyCheckStatus::Rejected, None);
    }
    // Some answers were neither success nor denial (400 on a probe, 404 on the list): the
    // credential was not refused anywhere it mattered.
    if route_passed
        || facts
            .responses
            .iter()
            .any(|(status, _)| *status == 400 || *status == 422)
    {
        return (KeyCheckStatus::Accepted, None);
    }
    (KeyCheckStatus::Untested, Some("inconclusive"))
}

/// The key verdict and the statuses behind it. The scan-level overrides apply in order, each
/// on the result of the one before: a rejection that could not try the second credential
/// variant is `Untested` for `budget`; an `Untested` verdict of a stopped scan says `stopped`
/// (replacing `budget`); a held-back key says `needs_confirmation` whatever the status.
pub(super) fn key_verdict(
    facts: KeyFacts<'_>,
    route_statuses: Vec<(Protocol, u16)>,
) -> (KeyCheckStatus, KeyEvidence) {
    let (mut status, mut reason) = key_status(&facts, &route_statuses);
    if facts.has_key && status == KeyCheckStatus::Rejected && facts.auth_budget_gap {
        status = KeyCheckStatus::Untested;
        reason = Some("budget");
    }
    if status == KeyCheckStatus::Untested && facts.stopped && facts.has_key {
        reason = Some("stopped");
    }
    if facts.needs_confirmation {
        reason = Some("needs_confirmation");
    }
    let evidence = KeyEvidence {
        models_status: facts.models_status,
        route_statuses,
        untested_reason: reason.map(str::to_owned),
    };
    (status, evidence)
}

#[cfg(test)]
mod tests;
