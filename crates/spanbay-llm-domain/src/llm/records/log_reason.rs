//! The short reason word of a request-log row and its `Retry-After` wait.
//!
//! [`derive_log_reason`] turns a stored row (`outcome`, `failure_kind`, `error`, `attempts_json`) into
//! `reason` and `retry_after_s`. Neither is a column: it runs on every read, so old rows have them
//! too. The gateway journal records the wait as `retry_after_s` on the failed attempt that
//! received a `Retry-After` header.

use serde_json::Value;

use super::log::RequestLogEntry;
use crate::llm::failure::FailureKind;
use crate::llm::usage::RequestOutcome;

/// Fills the read-time `retry_after_s` and `reason` of a stored row.
pub fn derive_log_reason(entry: &mut RequestLogEntry) {
    entry.retry_after_s = retry_after_s(&entry.attempts);
    entry.reason = reason(entry);
}

/// The wait named by the last attempt that carries one: the serving or last failed attempt.
fn retry_after_s(attempts: &Value) -> Option<u64> {
    attempts
        .as_array()?
        .iter()
        .rev()
        .find_map(|attempt| attempt.get("retry_after_s")?.as_u64())
}

/// Why the gateway refused the request itself, from the row's `error` text.
///
/// Coupled to the codes `spanbay-llm` writes: `gateway/errors.rs` (`Refusal::reason`) and
/// `gateway/auth.rs` (`Reason::as_str`). A code missing here reads as a plain failure.
fn refusal(error: &str) -> Option<&'static str> {
    if error.starts_with("Host ") && error.ends_with(" is not allowed") {
        return Some("host not allowed");
    }
    Some(match error {
        "token_missing" => "token missing",
        "token_unknown" => "token unknown",
        "token_revoked" => "token revoked",
        "auth_rate_limited" => "too many token failures",
        "origin_not_allowed" => "origin not allowed",
        "model_not_allowed" => "model not allowed",
        "body_too_large" => "body too large",
        _ => return None,
    })
}

/// No attempt ran: every listed target was skipped and one of them because its circuit is open.
///
/// Coupled to the skip text "circuit open" that `spanbay-llm` writes in `router/plan.rs`,
/// `gateway/walk.rs` and `client/attempts.rs`.
fn circuit_open(attempts: &Value) -> bool {
    let rows = attempts.as_array().map_or(&[][..], Vec::as_slice);
    let skipped = |row: &Value| row.get("outcome").and_then(Value::as_str) == Some("skipped");
    let circuit = |row: &Value| {
        row.get("error")
            .and_then(Value::as_str)
            .is_some_and(|error| error.contains("circuit open"))
    };
    !rows.is_empty() && rows.iter().all(skipped) && rows.iter().any(circuit)
}

fn failure_word(kind: Option<FailureKind>) -> &'static str {
    match kind {
        Some(FailureKind::Auth) => "auth rejected",
        Some(FailureKind::Region) => "region blocked",
        Some(FailureKind::Quota) => "quota exhausted",
        Some(FailureKind::RateLimit) => "rate limited",
        Some(FailureKind::ModelUnavailable) => "model unavailable",
        Some(FailureKind::Timeout) => "timeout",
        Some(FailureKind::EmptyOutput) => "empty output",
        Some(FailureKind::ProtocolMismatch) => "protocol mismatch",
        Some(FailureKind::PolicyBlocked) => "policy blocked",
        Some(FailureKind::Network) => "network error",
        Some(FailureKind::Cancelled) => "canceled",
        Some(FailureKind::ContextLimit) => "context limit",
        Some(FailureKind::Truncated) => "incomplete answer",
        Some(FailureKind::BadRequest) => "bad request",
        Some(FailureKind::ProviderError | FailureKind::Unknown) | None => "upstream error",
    }
}

/// The word of a row; `None` for a row that succeeded.
fn reason(entry: &RequestLogEntry) -> Option<String> {
    // String comparison on purpose: an unknown outcome has no reason, while
    // `RequestOutcome::from_log` would read it as a failure.
    if entry.outcome == RequestOutcome::Canceled.as_str() {
        return Some("canceled".to_owned());
    }
    if entry.outcome != RequestOutcome::Failure.as_str() {
        return None;
    }
    if let Some(word) = entry.error.as_deref().and_then(refusal) {
        return Some(word.to_owned());
    }
    if let Some(wait) = entry.retry_after_s {
        return Some(format!("retry-after {wait}"));
    }
    if circuit_open(&entry.attempts) {
        return Some("circuit open".to_owned());
    }
    Some(failure_word(entry.failure_kind).to_owned())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn row(outcome: &str, error: Option<&str>, kind: Option<FailureKind>) -> RequestLogEntry {
        RequestLogEntry {
            outcome: outcome.to_owned(),
            error: error.map(str::to_owned),
            failure_kind: kind,
            ..RequestLogEntry::default()
        }
    }

    fn derived(mut entry: RequestLogEntry) -> (Option<String>, Option<u64>) {
        derive_log_reason(&mut entry);
        (entry.reason, entry.retry_after_s)
    }

    #[test]
    fn a_success_has_no_reason_and_a_cancel_says_so() {
        assert_eq!(derived(row("success", None, None)), (None, None));
        assert_eq!(
            derived(row("canceled", None, Some(FailureKind::Cancelled))).0,
            Some("canceled".to_owned())
        );
    }

    #[test]
    fn gateway_refusals_are_named_from_their_error_text() {
        for (error, word) in [
            ("Host evil.test is not allowed", "host not allowed"),
            ("token_missing", "token missing"),
            ("token_unknown", "token unknown"),
            ("token_revoked", "token revoked"),
            ("auth_rate_limited", "too many token failures"),
            ("origin_not_allowed", "origin not allowed"),
            ("model_not_allowed", "model not allowed"),
            ("body_too_large", "body too large"),
        ] {
            let kind = Some(FailureKind::Auth);
            assert_eq!(derived(row("failure", Some(error), kind)).0.unwrap(), word);
        }
    }

    #[test]
    fn the_last_attempt_with_a_retry_after_names_the_wait() {
        let mut entry = row(
            "failure",
            Some("429 retry-after 2"),
            Some(FailureKind::RateLimit),
        );
        entry.attempts = json!([
            {"outcome": "failure", "retry_after_s": 7},
            {"outcome": "failure", "retry_after_s": 2},
            {"outcome": "failure"},
        ]);
        assert_eq!(derived(entry), (Some("retry-after 2".to_owned()), Some(2)));
    }

    #[test]
    fn a_walk_of_skipped_targets_is_a_circuit() {
        let mut entry = row(
            "failure",
            Some("no target"),
            Some(FailureKind::ProviderError),
        );
        entry.attempts = json!([
            {"outcome": "skipped", "error": "disabled"},
            {"outcome": "skipped", "error": "circuit open"},
        ]);
        assert_eq!(derived(entry.clone()).0.unwrap(), "circuit open");
        entry.attempts = json!([
            {"outcome": "failure", "error": "boom"},
            {"outcome": "skipped", "error": "circuit open"},
        ]);
        assert_eq!(derived(entry).0.unwrap(), "upstream error");
    }

    #[test]
    fn other_failures_read_from_their_kind_and_an_unknown_one_is_an_upstream_error() {
        for (kind, word) in [
            (Some(FailureKind::Timeout), "timeout"),
            (Some(FailureKind::Network), "network error"),
            (Some(FailureKind::Quota), "quota exhausted"),
            (Some(FailureKind::ProviderError), "upstream error"),
            (None, "upstream error"),
        ] {
            assert_eq!(derived(row("failure", Some("x"), kind)).0.unwrap(), word);
        }
    }
}
