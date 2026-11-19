//! The structured failure classes of a run.
//!
//! Single responsibility: turn what the model call reported (the typed [`ChatError`] kind, the
//! provider failure category, the error) into the one class the Chat UI words, so no screen
//! matches on error text. The class rides on `AgentEvent::Error`, on the run summary
//! and on the stored assistant message.

pub use pingdeck_model::agent::failure::{AgentFailure, AgentFailureKind, AgentFailureTarget};

use pingdeck_base::error::AppError;
use pingdeck_llm::llm::client::{AttemptSummary, ChatErrorKind};
use pingdeck_llm::llm::failure::FailureKind;

use super::context::is_context_length_error;
use super::mcp::is_auth_rejection;

/// The class of a failed model call to `target`, `None` for a cancellation.
///
/// `chat` is the typed kind when the error came from the client (`None` for a stall the run
/// loop detected itself), `failure` the provider category of the last attempt, `attempts`
/// every attempt the client made.
///
/// Free function rather than an `impl` on [`AgentFailure`]: the type lives in
/// `pingdeck_model`, and this needs this crate's LLM client types.
#[must_use]
pub fn classify_failure(
    chat: Option<ChatErrorKind>,
    failure: Option<FailureKind>,
    error: &AppError,
    target: &str,
    attempts: &[AttemptSummary],
) -> Option<AgentFailure> {
    if error.is_cancelled() || chat == Some(ChatErrorKind::Cancelled) {
        return None;
    }
    let kind = if is_context_length_error(error) {
        AgentFailureKind::ContextFull
    } else if is_auth_rejection(error) {
        AgentFailureKind::McpNeedsToken
    } else {
        match chat {
            Some(ChatErrorKind::KeyRejected) => AgentFailureKind::KeyRejected,
            Some(ChatErrorKind::Refusal) => AgentFailureKind::RefusedOrEmpty,
            Some(ChatErrorKind::IdleStall) => AgentFailureKind::Stalled,
            _ if failure == Some(FailureKind::Network) => AgentFailureKind::Offline,
            _ => AgentFailureKind::AllTargetsFailed,
        }
    };
    let targets = if attempts.is_empty() {
        vec![AgentFailureTarget {
            target: target.to_owned(),
            reason: error.message().to_owned(),
        }]
    } else {
        attempts
            .iter()
            .map(|attempt| AgentFailureTarget {
                target: attempt.target.clone(),
                reason: attempt.reason.clone(),
            })
            .collect()
    };
    Some(AgentFailure { kind, targets })
}

/// The `AppError` kind name of `error` as it appears on the wire (`Network`, `Cancelled`, ...).
#[must_use]
pub fn error_kind_name(error: &AppError) -> String {
    serde_json::to_value(error)
        .ok()
        .and_then(|value| {
            value
                .get("kind")
                .and_then(serde_json::Value::as_str)
                .map(str::to_owned)
        })
        .unwrap_or_else(|| "Internal".to_owned())
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    fn classify(
        chat: Option<ChatErrorKind>,
        failure: Option<FailureKind>,
        error: &AppError,
    ) -> Option<AgentFailureKind> {
        classify_failure(chat, failure, error, "m", &[]).map(|class| class.kind)
    }

    #[test]
    fn each_typed_failure_has_its_own_class() {
        let network = AppError::network("every upstream attempt failed; last: connect refused");
        let table = [
            (
                Some(ChatErrorKind::KeyRejected),
                None,
                AgentFailureKind::KeyRejected,
            ),
            (
                Some(ChatErrorKind::Refusal),
                None,
                AgentFailureKind::RefusedOrEmpty,
            ),
            (
                Some(ChatErrorKind::IdleStall),
                None,
                AgentFailureKind::Stalled,
            ),
            (
                Some(ChatErrorKind::Failed),
                Some(FailureKind::Network),
                AgentFailureKind::Offline,
            ),
            (
                Some(ChatErrorKind::Failed),
                Some(FailureKind::ProviderError),
                AgentFailureKind::AllTargetsFailed,
            ),
            (
                Some(ChatErrorKind::MidStream),
                None,
                AgentFailureKind::AllTargetsFailed,
            ),
        ];
        for (chat, failure, expected) in table {
            assert_eq!(
                classify(chat, failure, &network),
                Some(expected),
                "{chat:?}"
            );
        }
    }

    #[test]
    fn a_context_error_and_a_cancellation_are_told_apart() {
        let long = AppError::network("This model's maximum context length is 8192 tokens");
        assert_eq!(
            classify(Some(ChatErrorKind::Failed), None, &long),
            Some(AgentFailureKind::ContextFull)
        );
        assert_eq!(classify(None, None, &AppError::cancelled()), None);
        assert_eq!(
            classify(
                Some(ChatErrorKind::Cancelled),
                None,
                &AppError::network("x")
            ),
            None
        );
    }

    #[test]
    fn the_failed_target_and_its_reason_are_listed() {
        let class = classify_failure(
            Some(ChatErrorKind::Failed),
            None,
            &AppError::network("boom"),
            "gpt",
            &[],
        )
        .unwrap();
        assert_eq!(
            class.targets,
            vec![AgentFailureTarget {
                target: "gpt".to_owned(),
                reason: "boom".to_owned()
            }]
        );
    }

    #[test]
    fn every_attempt_of_the_client_is_listed_in_order() {
        let attempt = |target: &str, reason: &str| AttemptSummary {
            target: target.to_owned(),
            reason: reason.to_owned(),
            transient: None,
        };
        let class = classify_failure(
            Some(ChatErrorKind::Failed),
            None,
            &AppError::network("every upstream attempt failed; last: 503"),
            "gpt",
            &[attempt("A/gpt-4", "429"), attempt("B/gpt-4", "503")],
        )
        .unwrap();
        let listed: Vec<(&str, &str)> = class
            .targets
            .iter()
            .map(|t| (t.target.as_str(), t.reason.as_str()))
            .collect();
        assert_eq!(listed, [("A/gpt-4", "429"), ("B/gpt-4", "503")]);
    }
}
