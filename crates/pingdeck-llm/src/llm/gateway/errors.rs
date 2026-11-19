//! The gateway's own refusals and the error bodies clients see.
//!
//! Single responsibility: one place that knows, per refusal, the HTTP status, the client-visible
//! text and code in each dialect (OpenAI, Anthropic, Gemini; PRD 7.11 error table) and the
//! reason recorded in the log row. A missing, unknown and revoked token answer with the same
//! bytes, so a caller learns nothing about which tokens exist; only the log row differs.

use hyper::Response;
use hyper::header::HeaderValue;
use serde_json::{Value, json};

use crate::llm::adapters::codec;
use crate::llm::failure::FailureKind;
use crate::llm::wire::Protocol;

use super::auth::Reason;
use super::server::{GatewayBody, json as json_response};

/// The text of every 401.
const UNAUTHORIZED_MESSAGE: &str = "Missing or invalid gateway token.";

/// An error response in the shape of `family`'s protocol (OpenAI's when there is none).
pub(super) fn error_response(
    family: Option<Protocol>,
    status: u16,
    message: &str,
    kind: &str,
) -> Response<GatewayBody> {
    json_response(status, &error_body(family, status, message, kind))
}

pub(super) fn error_body(
    family: Option<Protocol>,
    status: u16,
    message: &str,
    kind: &str,
) -> Value {
    match family.and_then(codec) {
        Some(codec) => codec.encode_error(status, message, kind),
        None => json!({"error": {"message": message, "type": kind, "param": null, "code": null}}),
    }
}

/// Why the gateway refused a request itself (before any upstream was involved).
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Refusal {
    /// The `Host` header does not name this gateway.
    Host {
        /// The rejected value (`(missing)` or `(several)` when there is not exactly one).
        host: String,
    },
    /// A request that carries an `Origin` header (no CORS: every origin is refused).
    Origin,
    /// No usable token (the reason is for the log only).
    Token(Reason),
    /// Too many failed token attempts from this peer.
    Throttled {
        /// Seconds to wait.
        retry_after_s: u64,
    },
    /// The client's allow-list does not include the requested model.
    ModelNotAllowed {
        /// What the client asked for.
        model: String,
    },
    /// The body is over the size limit.
    BodyTooLarge,
    /// Too many request bodies are being held at once (the global body budget is spent).
    BodyBudget,
    /// The client's token is past its expiry.
    Expired,
    /// A spend or token budget of the client is used up.
    Budget {
        /// Which one (`daily budget`, `monthly tokens`...).
        what: &'static str,
    },
    /// The client's requests-per-minute limit is reached.
    ClientRate {
        /// Seconds until the oldest counted request leaves the minute.
        retry_after_s: u64,
    },
    /// The client's limit of requests in flight is reached.
    ClientBusy,
}

impl Refusal {
    pub(super) fn status(&self) -> u16 {
        match self {
            Self::Host { .. } | Self::Origin | Self::ModelNotAllowed { .. } => 403,
            Self::Token(_) | Self::Expired => 401,
            Self::Throttled { .. } | Self::ClientRate { .. } | Self::ClientBusy => 429,
            Self::Budget { .. } => 402,
            Self::BodyTooLarge => 413,
            Self::BodyBudget => 503,
        }
    }

    fn kind(&self) -> &'static str {
        match self {
            Self::Host { .. } | Self::Origin | Self::ModelNotAllowed { .. } => "permission_error",
            Self::Token(_) | Self::Expired => "authentication_error",
            Self::Throttled { .. } | Self::ClientRate { .. } | Self::ClientBusy => {
                "rate_limit_error"
            }
            Self::Budget { .. } => "billing_error",
            Self::BodyTooLarge => "invalid_request_error",
            Self::BodyBudget => "overloaded_error",
        }
    }

    /// The machine-readable `code` of OpenAI-shaped bodies.
    fn code(&self) -> &'static str {
        match self {
            Self::Host { .. } => "host_not_allowed",
            Self::Origin => "origin_not_allowed",
            Self::Token(_) => "invalid_gateway_token",
            Self::Throttled { .. } => "too_many_auth_failures",
            Self::ModelNotAllowed { .. } => "model_not_allowed",
            Self::BodyTooLarge => "body_too_large",
            Self::BodyBudget => "gateway_busy",
            Self::Expired => "client_token_expired",
            Self::Budget { .. } => "client_budget_exceeded",
            Self::ClientRate { .. } => "client_rate_limited",
            Self::ClientBusy => "client_concurrency_limited",
        }
    }

    /// The `error` text of the log row; never contains a token.
    pub(super) fn reason(&self) -> String {
        match self {
            Self::Host { host } => format!("Host {host} is not allowed"),
            Self::Token(reason) => reason.as_str().to_owned(),
            Self::Throttled { .. } => "auth_rate_limited".to_owned(),
            Self::Budget { what } => format!("client {what} reached"),
            other => other.code().to_owned(),
        }
    }

    /// The `failure_kind` of the log row.
    pub(super) fn failure_kind(&self) -> FailureKind {
        match self {
            Self::BodyTooLarge | Self::BodyBudget => FailureKind::Unknown,
            Self::Budget { .. } => FailureKind::Quota,
            Self::ClientRate { .. } | Self::ClientBusy => FailureKind::RateLimit,
            _ => FailureKind::Auth,
        }
    }

    fn message(&self) -> String {
        match self {
            Self::Host { host } => {
                format!("Host {host} is not allowed. Add it under Extra allowed hosts in Settings.")
            }
            Self::Origin => "Browser requests (with an Origin header) are not allowed.".to_owned(),
            Self::Token(_) => UNAUTHORIZED_MESSAGE.to_owned(),
            Self::Throttled { .. } => "Too many failed token attempts. Try again later.".to_owned(),
            Self::ModelNotAllowed { model } => format!("This token cannot use model {model}."),
            Self::BodyTooLarge => "The request body is too large.".to_owned(),
            Self::BodyBudget => {
                "The gateway is busy with large requests. Try again shortly.".to_owned()
            }
            Self::Expired => "This gateway token has expired.".to_owned(),
            Self::Budget { what } => {
                format!("This client's {what} on the PingDeck gateway is used up.")
            }
            Self::ClientRate { .. } => {
                "This client sent too many requests this minute. Try again later.".to_owned()
            }
            Self::ClientBusy => {
                "This client has too many requests in progress. Try again shortly.".to_owned()
            }
        }
    }

    /// The body in `family`'s dialect.
    fn body(&self, family: Option<Protocol>) -> Value {
        let mut body = error_body(family, self.status(), &self.message(), self.kind());
        // Anthropic and Gemini bodies have no `code`/`param` slots; OpenAI's get both.
        if !matches!(family, Some(Protocol::AnthropicMessages | Protocol::Gemini))
            && let Some(error) = body.get_mut("error")
        {
            error["code"] = json!(self.code());
            if matches!(self, Self::ModelNotAllowed { .. }) {
                error["param"] = json!("model");
            }
        }
        body
    }

    /// The response in `family`'s dialect with its protocol headers.
    pub(super) fn response(&self, family: Option<Protocol>) -> Response<GatewayBody> {
        let mut response = json_response(self.status(), &self.body(family));
        match self {
            Self::Token(_) => {
                response.headers_mut().insert(
                    hyper::header::WWW_AUTHENTICATE,
                    HeaderValue::from_static("Bearer realm=\"pingdeck\""),
                );
            }
            Self::Throttled { retry_after_s } | Self::ClientRate { retry_after_s } => {
                response.headers_mut().insert(
                    hyper::header::RETRY_AFTER,
                    HeaderValue::from(*retry_after_s),
                );
            }
            Self::BodyBudget | Self::ClientBusy => {
                response
                    .headers_mut()
                    .insert(hyper::header::RETRY_AFTER, HeaderValue::from(1_u64));
            }
            _ => {}
        }
        response
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_three_token_reasons_share_one_body_and_differ_only_in_the_log() {
        let bodies: Vec<Value> = [Reason::Missing, Reason::Unknown, Reason::Revoked]
            .into_iter()
            .map(|reason| Refusal::Token(reason).body(None))
            .collect();
        assert_eq!(bodies[0], bodies[1]);
        assert_eq!(bodies[1], bodies[2]);
        assert_eq!(
            bodies[0],
            json!({"error": {"message": "Missing or invalid gateway token.", "type": "authentication_error", "param": null, "code": "invalid_gateway_token"}})
        );
        assert_eq!(
            [Reason::Missing, Reason::Unknown, Reason::Revoked]
                .map(|reason| Refusal::Token(reason).reason()),
            ["token_missing", "token_unknown", "token_revoked"].map(String::from)
        );
    }
}
