//! Model-driven conversation title generation (`CallPurpose::Title`).
//!
//! Enforces the one-shot first-turn title lifecycle:
//! 1. On the user's first question, `run::begin` writes a truncated provisional title.
//! 2. Once, a single background AI request summarizes a concise title within
//!    `GENERATED_TITLE_MAX_CHARS`.
//! 3. On success, the title is updated and permanently locked (`title_locked = true`).
//! 4. On failure (network/upstream error, timeout, missing target, or empty output), the
//!    provisional title is kept untouched and permanently locked (`title_locked = true`) so it
//!    will never be retried or overwritten automatically.
//! 5. Subsequent turns never change the title.

use std::time::Duration;

use pingdeck_base::logging::{LogLevel, log_line};

use crate::agent::model::{AgentEvent, AgentRole, AgentTarget};
use crate::app::{AppState, command_error, current_time_ms};
use crate::error::AppError;
use crate::llm::client::{CallMeta, CallPurpose, ChatTarget};
use crate::llm::hub::{HubPart, HubRequest};

use super::host::blocking;
use super::sessions::load_session;

/// Hard character cap for AI-summarized session titles.
pub(super) const GENERATED_TITLE_MAX_CHARS: usize = 28;
/// Maximum input characters forwarded to the title model so calls stay fast and cheap.
const MAX_PROMPT_CHARS: usize = 1_200;
/// Timeout for a background title-generation model call.
const TITLE_TIMEOUT: Duration = Duration::from_secs(15);

const TITLE_SYSTEM_PROMPT: &str = "\
You generate a concise, clear title for a chat conversation based on the user's first message. \
Rules:
1. Write the title in the exact same language as the user's message (Chinese: within 14 characters; English: 3 to 5 words, within 28 characters).
2. Summarize the core topic or goal as a clean phrase instead of copying the raw question.
3. Output ONLY the plain title text: no quotes, no markdown, no numbering, no prefixes like 'Title:' or '标题：', and no trailing punctuation.";

/// Generates a concise model-summarized title for `session_id` and locks automatic titling.
///
/// When `expected_current_title` is `Some(provisional)`, this is the automatic one-shot upgrade
/// right after the first user turn:
/// - Requires `ai.agent.auto_title == true`, `!session.title_locked`, and that the stored title
///   still equals `provisional` (protecting against concurrent manual renames).
/// - On success, writes the summarized title (capped at `GENERATED_TITLE_MAX_CHARS`) and sets
///   `title_locked = true`.
/// - On any failure (missing target, empty prompt, timeout, model error, or unusable output),
///   keeps the provisional title unchanged, sets `title_locked = true` so this session is never
///   retried automatically, and returns `Ok(None)`.
///
/// When `expected_current_title` is `None`, it is an explicit one-shot call that propagates model
/// errors and updates + locks the title on success.
///
/// # Errors
/// Returns an error if the session does not exist, a store write fails, or (in explicit mode)
/// the model call fails.
pub async fn agent_session_generate_title_impl(
    state: &AppState,
    session_id: &str,
    expected_current_title: Option<&str>,
) -> Result<Option<String>, AppError> {
    let sid = session_id.to_owned();
    let expected = expected_current_title.map(str::to_owned);
    let automatic = expected.is_some();

    let prep = blocking(state, move |state| {
        let settings = state.settings();
        let mut session = load_session(state, &sid)?;
        if automatic {
            if !settings.ai.agent.auto_title || session.title_locked {
                return Ok::<_, AppError>(None);
            }
            if expected.as_deref().is_some_and(|exp| session.title != exp) {
                return Ok(None);
            }
        }
        let Some(target) = session.target.clone() else {
            if automatic {
                session.title_locked = true;
                state
                    .store()
                    .save_agent_session(&session)
                    .map_err(command_error)?;
            }
            return Ok(None);
        };
        let messages = state
            .store()
            .list_agent_messages(&sid)
            .map_err(command_error)?;
        let prompt = extract_prompt_for_title(&messages);
        if prompt.is_empty() {
            if automatic {
                session.title_locked = true;
                state
                    .store()
                    .save_agent_session(&session)
                    .map_err(command_error)?;
            }
            return Ok(None);
        }
        let chat_target = match &target {
            AgentTarget::Named { name } => ChatTarget::Named {
                name: name.clone(),
                fallbacks: settings.ai.agent.fallbacks.main.clone(),
            },
            AgentTarget::Pinned {
                provider_id,
                key_id,
                model,
            } => ChatTarget::Pinned {
                provider_id: provider_id.clone(),
                key_id: key_id.clone(),
                model: model.clone(),
            },
        };
        let model_label = target.label();
        Ok(Some((chat_target, model_label, prompt)))
    })
    .await??;

    let Some((chat_target, model_label, prompt)) = prep else {
        return Ok(None);
    };

    let meta = CallMeta {
        run_id: format!("title-{session_id}"),
        message_id: None,
        purpose: CallPurpose::Title,
    };
    let client = state.llm_client().for_session(session_id).for_call(&meta);
    let mut request = HubRequest::user_prompt(model_label, prompt);
    request.system = Some(TITLE_SYSTEM_PROMPT.to_owned());
    request.max_tokens = Some(48);
    request.temperature = Some(0.3);

    let call_result =
        tokio::time::timeout(TITLE_TIMEOUT, client.complete(&chat_target, request, None)).await;

    let response = match call_result {
        Ok(Ok(resp)) => resp,
        Ok(Err(err)) => {
            if automatic {
                log_line(
                    LogLevel::Warn,
                    "agent.title",
                    &format!(
                        "auto title generation failed for {session_id}; keeping provisional title: {}",
                        err.message()
                    ),
                );
                lock_provisional_title(state, session_id, expected_current_title).await?;
                return Ok(None);
            }
            return Err(err);
        }
        Err(_) => {
            let err = AppError::network("title generation timed out");
            if automatic {
                log_line(
                    LogLevel::Warn,
                    "agent.title",
                    &format!(
                        "auto title generation timed out for {session_id}; keeping provisional title"
                    ),
                );
                lock_provisional_title(state, session_id, expected_current_title).await?;
                return Ok(None);
            }
            return Err(err);
        }
    };

    let Some(clean_title) = sanitize_generated_title(&response.text()) else {
        if automatic {
            log_line(
                LogLevel::Warn,
                "agent.title",
                &format!(
                    "auto title model returned unusable output for {session_id}; keeping provisional title"
                ),
            );
            lock_provisional_title(state, session_id, expected_current_title).await?;
        }
        return Ok(None);
    };

    let sid_save = session_id.to_owned();
    let expected_save = expected_current_title.map(str::to_owned);
    let title_save = clean_title.clone();
    let changed = blocking(state, move |state| {
        let mut session = load_session(state, &sid_save)?;
        if automatic {
            if session.title_locked {
                return Ok::<_, AppError>(false);
            }
            if expected_save
                .as_deref()
                .is_some_and(|exp| session.title != exp)
            {
                return Ok(false);
            }
        }
        let changed = session.title != title_save;
        session.title = title_save;
        session.title_locked = true;
        if changed {
            session.updated_at_ms = current_time_ms();
        }
        state
            .store()
            .save_agent_session(&session)
            .map_err(command_error)?;
        Ok(changed)
    })
    .await??;

    if !changed {
        return Ok(None);
    }

    // Notify any active run subscriber for this session so UI titles update immediately.
    if let Some(run) = state.agent.runs.active_for_session(session_id) {
        run.emit(AgentEvent::SessionTitleUpdated {
            session_id: session_id.to_owned(),
            title: clean_title.clone(),
        });
    }

    Ok(Some(clean_title))
}

/// Permanently locks the session's provisional title after an automatic generation failure so
/// subsequent events, restarts, or turns never retry or modify it.
async fn lock_provisional_title(
    state: &AppState,
    session_id: &str,
    expected_current_title: Option<&str>,
) -> Result<(), AppError> {
    let sid = session_id.to_owned();
    let expected = expected_current_title.map(str::to_owned);
    blocking(state, move |state| {
        let mut session = load_session(state, &sid)?;
        if session.title_locked {
            return Ok::<_, AppError>(());
        }
        if expected.as_deref().is_some_and(|exp| session.title != exp) {
            return Ok(());
        }
        session.title_locked = true;
        state
            .store()
            .save_agent_session(&session)
            .map_err(command_error)?;
        Ok(())
    })
    .await?
}

fn extract_prompt_for_title(messages: &[crate::agent::model::AgentMessage]) -> String {
    let mut user_text = String::new();
    let mut context_label: Option<String> = None;
    for msg in messages {
        if msg.role == AgentRole::User {
            for part in &msg.parts {
                if let HubPart::Text { text, .. } = part {
                    if let Some(label) = super::context_payload::label_of(text) {
                        context_label.get_or_insert_with(|| label.to_owned());
                    } else if !text.trim().is_empty() {
                        if !user_text.is_empty() {
                            user_text.push('\n');
                        }
                        user_text.push_str(text.trim());
                    }
                }
            }
            if !user_text.is_empty() || context_label.is_some() {
                break;
            }
        }
    }
    let raw = if !user_text.is_empty() {
        user_text
    } else {
        context_label.unwrap_or_default()
    };
    raw.trim().chars().take(MAX_PROMPT_CHARS).collect()
}

/// Cleans raw LLM output into a single-line plain title within `GENERATED_TITLE_MAX_CHARS`
/// (`None` when unusable).
#[must_use]
pub(super) fn sanitize_generated_title(raw: &str) -> Option<String> {
    let stripped = strip_think_tags(raw);
    let first_line = stripped
        .lines()
        .map(str::trim)
        .find(|line| !line.is_empty())?;

    let mut s = first_line
        .trim_start_matches(['#', '*', '-', '>', ' ', '\t'])
        .trim();

    for prefix in ["标题：", "标题:", "Title:", "TITLE:", "title:"] {
        if let Some(rest) = s.strip_prefix(prefix) {
            s = rest.trim();
        }
    }

    // Unwrap matching or stray surrounding quotes and markdown emphasis.
    let quote_chars = [
        '"', '\'', '`', '“', '”', '‘', '’', '「', '」', '《', '》', '*',
    ];
    s = s.trim_matches(quote_chars).trim();

    // Remove trailing sentence punctuation so titles read cleanly as labels.
    let trailing_punct = [
        '。', '.', '？', '?', '！', '!', '：', ':', '；', ';', '，', ',',
    ];
    s = s.trim_end_matches(trailing_punct).trim();

    // Collapse internal whitespace into single spaces and enforce the character limit.
    let collapsed: String = s.split_whitespace().collect::<Vec<_>>().join(" ");
    let truncated: String = collapsed.chars().take(GENERATED_TITLE_MAX_CHARS).collect();
    let final_title = truncated
        .trim()
        .trim_end_matches(trailing_punct)
        .trim()
        .to_owned();
    if final_title.chars().count() < 2 {
        None
    } else {
        Some(final_title)
    }
}

fn strip_think_tags(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    let mut rest = raw;
    while !rest.is_empty() {
        let open_a = rest.find("<think>").map(|i| (i, "</think>"));
        let open_b = rest.find("<thinking>").map(|i| (i, "</thinking>"));
        let next = match (open_a, open_b) {
            (Some(a), Some(b)) => Some(if a.0 <= b.0 { a } else { b }),
            (Some(a), None) => Some(a),
            (None, Some(b)) => Some(b),
            (None, None) => None,
        };
        let Some((idx, close_tag)) = next else {
            out.push_str(rest);
            break;
        };
        out.push_str(&rest[..idx]);
        let after = &rest[idx..];
        if let Some(end_idx) = after.find(close_tag) {
            rest = &after[end_idx + close_tag.len()..];
        } else {
            break;
        }
    }
    out
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn sanitizes_quotes_think_tags_prefixes_length_and_punctuation() {
        assert_eq!(
            sanitize_generated_title("<think>analyzing</think>\n标题：「修复网关 429 熔断逻辑？」"),
            Some("修复网关 429 熔断逻辑".to_owned())
        );
        assert_eq!(
            sanitize_generated_title("Title: \"Gateway Retry-After Fix.\"\nExtra explanation"),
            Some("Gateway Retry-After Fix".to_owned())
        );
        let long = "这是一个明显超过指定最大字数上限的冗长模型输出标题内容需要被严格截断";
        let capped = sanitize_generated_title(long).unwrap();
        assert!(capped.chars().count() <= GENERATED_TITLE_MAX_CHARS);
        assert_eq!(sanitize_generated_title("   \n\"\"  "), None);
        assert_eq!(
            sanitize_generated_title("<think>unclosed reasoning only"),
            None
        );
    }
}
