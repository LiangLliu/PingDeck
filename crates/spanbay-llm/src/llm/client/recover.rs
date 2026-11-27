//! Recovery from a provider that refuses replayed reasoning or prompt-cache markers.
//!
//! Single responsibility: spot the HTTP 400 an Anthropic or Gemini upstream answers when old
//! thinking blocks of the history no longer verify (another system prompt, key or model, a session
//! resumed a day later), and build the one retry without them. Without it every later turn of
//! the session fails the same way. Preventing the refusal is the agent context's job. A relay
//! that rejects `cache_control` or `prompt_cache_key` gets one retry without the hint, and the
//! provider is remembered for the session ([`without_cache`]).

use std::sync::PoisonError;

use crate::llm::hub::{HubMessage, HubPart, HubRequest, HubRole};
use crate::llm::runtime::LlmRuntime;
use crate::llm::upstream::AttemptError;

/// Whether `error` is the provider refusing a replayed thinking block or its signature.
fn refuses_old_reasoning(error: &AttemptError) -> bool {
    let AttemptError::Status {
        status: 400, body, ..
    } = error
    else {
        return false;
    };
    let body = body.to_ascii_lowercase();
    (body.contains("thinking") || body.contains("thought"))
        && (body.contains("signature") || body.contains("cannot be modified"))
}

fn is_reasoning(part: &HubPart) -> bool {
    matches!(part, HubPart::Reasoning { .. })
}

/// `request` without replayed reasoning blocks that failed signature verification. Prefers
/// stripping only assistant turns before the newest user message (keeping the active turn's
/// reasoning intact); when the refusal happened mid-turn (e.g., failover across two Anthropic
/// keys/providers during a tool loop), strips reasoning from the current turn too and clears
/// `request.reasoning` for the recovery call so providers that require thinking on the latest
/// assistant turn accept it. Returns `None` when no reasoning block exists to drop.
fn without_old_reasoning(request: &HubRequest) -> Option<HubRequest> {
    let has_reasoning = |message: &HubMessage| {
        message.role == HubRole::Assistant && message.content.iter().any(is_reasoning)
    };
    let current = request
        .messages
        .iter()
        .rposition(|message| message.role == HubRole::User);
    let strip_current_turn = match current {
        Some(idx) => !request.messages[..idx].iter().any(has_reasoning),
        None => true,
    };
    if strip_current_turn && !request.messages.iter().any(has_reasoning) {
        return None;
    }
    let mut stripped = request.clone();
    if strip_current_turn {
        for message in &mut stripped.messages {
            if message.role == HubRole::Assistant {
                message.content.retain(|part| !is_reasoning(part));
            }
        }
        stripped
            .messages
            .retain(|message| message.role != HubRole::Assistant || !message.content.is_empty());
        stripped.reasoning = None;
    } else if let Some(idx) = current {
        let newer = stripped.messages.split_off(idx);
        for message in &mut stripped.messages {
            if message.role == HubRole::Assistant {
                message.content.retain(|part| !is_reasoning(part));
            }
        }
        stripped
            .messages
            .retain(|message| message.role != HubRole::Assistant || !message.content.is_empty());
        stripped.messages.extend(newer);
    }
    Some(stripped)
}

/// Whether `error` is the upstream refusing the cache hint itself.
pub(crate) fn refuses_cache_hint(error: &AttemptError) -> bool {
    let AttemptError::Status {
        status: 400, body, ..
    } = error
    else {
        return false;
    };
    let body = body.to_ascii_lowercase();
    body.contains("cache_control") || body.contains("prompt_cache_key")
}

/// Whether calls to `provider_id` (in-app and through the gateway) go without cache hints this
/// session.
pub(crate) fn cache_refused(runtime: &LlmRuntime, provider_id: &str) -> bool {
    runtime
        .services
        .cache_refused
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .contains(provider_id)
}

/// Remembers for the session that `provider_id` refuses cache hints.
pub(crate) fn refuse_cache(runtime: &LlmRuntime, provider_id: &str) {
    runtime
        .services
        .cache_refused
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(provider_id.to_owned());
}

/// `request` without its cache hint when `error` refused it; `provider_id` is remembered so
/// later calls skip the hint. `None` when the error is another one or there was no hint.
pub(super) fn without_cache(
    runtime: &LlmRuntime,
    provider_id: &str,
    error: &AttemptError,
    request: &HubRequest,
) -> Option<HubRequest> {
    if request.cache.is_none() || !refuses_cache_hint(error) {
        return None;
    }
    refuse_cache(runtime, provider_id);
    Some(HubRequest {
        cache: None,
        ..request.clone()
    })
}

/// The request to send once more to the same target when `error` refused old reasoning.
pub(super) fn retry_request(error: &AttemptError, request: &HubRequest) -> Option<HubRequest> {
    refuses_old_reasoning(error)
        .then(|| without_old_reasoning(request))
        .flatten()
}

#[cfg(test)]
mod tests {
    use super::*;

    fn status(code: u16, body: &str) -> AttemptError {
        AttemptError::Status {
            status: code,
            body: body.to_owned(),
            retry_after_ms: None,
        }
    }

    #[test]
    fn only_a_refused_thinking_block_is_recognised() {
        for body in [
            "messages.1.content.0: Invalid `signature` in `thinking` block",
            "thinking blocks in the latest assistant message cannot be modified",
            "Thought signature is not valid",
        ] {
            assert!(refuses_old_reasoning(&status(400, body)), "{body}");
        }
        assert!(!refuses_old_reasoning(&status(
            400,
            "max_tokens must be greater than thinking.budget_tokens"
        )));
        assert!(!refuses_old_reasoning(&status(
            500,
            "Invalid `signature` in `thinking` block"
        )));
    }
}
