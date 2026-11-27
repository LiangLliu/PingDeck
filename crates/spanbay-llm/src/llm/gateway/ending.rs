//! How a call that could not be answered with a model's answer ends.
//!
//! Single responsibility: the final answers of the request pipeline when no upstream served the
//! call: a planning error (unknown model, nothing usable, every circuit open, a missing
//! capability), an upstream error that is the client's own answer, an exhausted
//! walk (every attempt failed, or every target refused the request), a local fault, a cancel.
//! Each one ends the call's [`Journal`] with its status, text and failure category and returns the
//! response in the client's dialect, with `Retry-After` where the rules say so.

use bytes::Bytes;
use hyper::Response;
use hyper::header::{HeaderValue, RETRY_AFTER};

use crate::llm::capture::redact_text;
use crate::llm::failover::exhausted_status;
use crate::llm::failure::FailureKind;
use crate::llm::router::{PlanError, PlanFailure};
use crate::llm::upstream::{AttemptError, provider_message};
use crate::llm::wire::Protocol;

use super::errors::error_body;
use super::journal::{Ended, Journal};
use super::reply::{bytes_response, kind_of, upstream_content_type, upstream_error};
use super::server::{Context, GatewayBody};
use super::walk::{Stop, Walk};

/// Ends `journal` as a failure and answers with an error in `protocol`'s dialect.
pub(super) fn fail(
    journal: &mut Journal,
    protocol: Protocol,
    status: u16,
    message: &str,
    retry_after_s: Option<u64>,
    kind: FailureKind,
) -> Response<GatewayBody> {
    let response = answer(journal, protocol, status, message, retry_after_s);
    journal.end(&Ended::Failed {
        status,
        error: message.to_owned(),
        kind,
    });
    response
}

/// The error answer itself; the caller ends the journal.
fn answer(
    journal: &mut Journal,
    protocol: Protocol,
    status: u16,
    message: &str,
    retry_after_s: Option<u64>,
) -> Response<GatewayBody> {
    let body = serde_json::to_vec(&error_body(
        Some(protocol),
        status,
        message,
        kind_of(status),
    ))
    .unwrap_or_default();
    journal.client_answer("application/json", None, &body);
    let mut response = bytes_response(status, "application/json", Bytes::from(body));
    if let Some(seconds) = retry_after_s {
        response
            .headers_mut()
            .insert(RETRY_AFTER, HeaderValue::from(seconds));
    }
    response
}

/// The failure category of a planning error.
fn plan_kind(error: &PlanError) -> FailureKind {
    match error {
        PlanError::UnknownModel(_) => FailureKind::ModelUnavailable,
        PlanError::NoTarget { .. } => FailureKind::ProviderError,
        PlanError::NotAllowed(_) => FailureKind::Auth,
        PlanError::NoUsableTarget(_) | PlanError::MissingCapability { .. } => FailureKind::Unknown,
    }
}

/// The answer to a name that could not be planned.
pub(super) fn plan_failed(
    journal: &mut Journal,
    protocol: Protocol,
    failure: &PlanFailure,
    close: &[String],
) -> Response<GatewayBody> {
    journal.skipped_targets(&failure.skipped);
    let answer = failure.error.answer();
    // A name close to a current one (a typo, or a former name that expired): say which.
    let message = if close.is_empty() {
        answer.message
    } else {
        format!("{}; did you mean {}?", answer.message, close.join(", "))
    };
    fail(
        journal,
        protocol,
        answer.status,
        &message,
        answer.retry_after_s,
        plan_kind(&failure.error),
    )
}

/// The answer after the walk was stopped by `stop`. Text of upstream origin is redacted before
/// the client reads it; an upstream error in the client's own dialect keeps its status and body
/// but not its content type (see [`upstream_error`]), one in another dialect keeps its status and
/// message in the client's error shape.
pub(super) async fn stopped(
    journal: &mut Journal,
    context: &Context,
    protocol: Protocol,
    stop: Stop,
) -> Response<GatewayBody> {
    match stop {
        Stop::Canceled => {
            let response = answer(journal, protocol, 499, "the request was cancelled", None);
            journal.end(&Ended::Canceled);
            response
        }
        Stop::Answer { error, upstream } => match &error {
            AttemptError::Status { status, body, .. } => {
                let shown = redact_text(body, &context.redaction_needles().await).0;
                let message = provider_message(&shown);
                let kind = error.kind();
                // Another dialect's error body would not parse in the client's SDK.
                if upstream.is_some_and(|spoken| spoken != protocol) {
                    return fail(journal, protocol, *status, &message, None, kind);
                }
                journal.client_answer(upstream_content_type(&shown), None, shown.as_bytes());
                journal.end(&Ended::Failed {
                    status: *status,
                    error: message,
                    kind,
                });
                upstream_error(*status, &shown)
            }
            other => {
                let message = redacted(context, &other.message()).await;
                fail(journal, protocol, 502, &message, None, other.kind())
            }
        },
        Stop::Fault(error) => {
            let message = redacted(context, &error.message()).await;
            fail(journal, protocol, 502, &message, None, error.kind())
        }
    }
}

/// `text` with the stored secrets and seen client tokens removed.
async fn redacted(context: &Context, text: &str) -> String {
    redact_text(text, &context.redaction_needles().await).0
}

/// What a client reads of a failed attempt: the provider's own words, redacted, except for a
/// rejected credential (401, 403), whose provider text can name the key or the account and stays
/// in the log row.
fn client_message(error: &AttemptError, needles: &[String]) -> String {
    match error.status() {
        Some(status @ (401 | 403)) => format!(
            "the upstream rejected the provider key (HTTP {status}); check the key in SpanBay"
        ),
        _ => redact_text(&error.message(), needles).0,
    }
}

/// The answer of a walk that ended without serving: the last failure, the first refusal, or
/// nothing to try.
pub(super) async fn exhausted(
    journal: &mut Journal,
    context: &Context,
    protocol: Protocol,
    walk: &mut Walk,
) -> Response<GatewayBody> {
    if let Some(error) = walk.take_last() {
        let status = exhausted_status(error.status());
        let needles = context.redaction_needles().await;
        let shown = format!(
            "every upstream attempt failed; last: {}",
            client_message(&error, &needles)
        );
        // A client that is told when to come back does not hammer a resting account.
        let retry_after = matches!(status, 429 | 503)
            .then(|| {
                error
                    .retry_after()
                    .map(|wait| {
                        u64::try_from(wait.as_millis())
                            .unwrap_or(u64::MAX)
                            .div_ceil(1000)
                            .max(1)
                    })
                    .or_else(|| walk.soonest_rest_s(context))
            })
            .flatten();
        let response = answer(journal, protocol, status, &shown, retry_after);
        journal.end(&Ended::Failed {
            status,
            error: format!("every upstream attempt failed; last: {}", error.message()),
            kind: error.kind(),
        });
        return response;
    }
    if let Some(message) = walk.refusal() {
        let message = message.to_owned();
        return fail(
            journal,
            protocol,
            400,
            &message,
            None,
            FailureKind::ProtocolMismatch,
        );
    }
    plan_failed(journal, protocol, &walk.unavailable(context).into(), &[])
}
