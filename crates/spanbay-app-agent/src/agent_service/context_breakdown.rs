//! What fills a chat's context window, per category.
//!
//! Single responsibility: [`agent_context_breakdown_impl`] builds the system prompt and tool list
//! the next run would send, reads the chat's model history and measures each category with the
//! same estimator the run loop uses ([`estimate_context_tokens`]), so the rows add up to the
//! figure the loop compares with the window.

use serde::{Deserialize, Serialize};

use crate::agent::context::{compaction_cut, estimate_context_tokens};
use crate::agent::model::AgentRole;
use crate::error::AppError;
use crate::llm::hub::{HubMessage, HubPart, HubRole};

use super::host::blocking;
use super::model_info::resolve_model;
use super::resolve::resolve;
use super::sessions::load_session;
use super::tools::Role;
use crate::app::{AppState, command_error};

/// Estimated tokens per part of the next request of one chat.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextBreakdown {
    /// The system prompt without the skills catalogue and the project instructions.
    pub system: u64,
    /// The folder's `AGENTS.md` / `CLAUDE.md`, when the folder is trusted.
    pub instructions: u64,
    /// The skills catalogue.
    pub skills: u64,
    /// The tool definitions the model is offered.
    pub tool_definitions: u64,
    /// User and assistant text, reasoning, tool calls and compaction summaries.
    pub messages: u64,
    /// Tool results.
    pub tool_results: u64,
    /// Images, documents and audio.
    pub attachments: u64,
    /// The sum of the rows above.
    pub total: u64,
    /// The model's context window, when known.
    pub limit: Option<u64>,
    /// "Compact now" would find something to summarise: history before the kept turns (or, in one
    /// long turn, before its last tool steps) that is not already a summary. The same rule the
    /// compaction itself applies (`context::compaction_cut`).
    pub can_compact: bool,
    /// User turns that "Compact now" would summarise (`None` when [`Self::can_compact`] is false).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub compact_turns: Option<u32>,
}

/// The parts of a history that are not plain conversation, split off by moving, not cloning.
#[derive(Default)]
struct Split {
    messages: Vec<HubPart>,
    tool_results: Vec<HubPart>,
    attachments: Vec<HubPart>,
}

fn tokens(parts: Vec<HubPart>) -> u64 {
    let message = HubMessage {
        role: HubRole::User,
        content: parts,
    };
    estimate_context_tokens(None, std::slice::from_ref(&message), &[])
}

/// Measures the context of `session_id` as its next run would send it.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown chat, [`AppError::Database`] when the history cannot be
/// read.
pub async fn agent_context_breakdown_impl(
    state: &AppState,
    session_id: &str,
) -> Result<ContextBreakdown, AppError> {
    let id = session_id.to_owned();
    let (session, history) = blocking(state, move |state| {
        let session = load_session(state, &id)?;
        let history = state
            .store()
            .list_model_history(&id)
            .map_err(command_error)?;
        Ok::<_, AppError>((session, history))
    })
    .await??;
    let model = match &session.target {
        Some(target) => resolve_model(state, target).await,
        None => crate::agent::runner::ModelInfo::default(),
    };
    let resolved = resolve(state, &session, &model, Role::Main, false).await;
    let roles: Vec<HubRole> = history
        .iter()
        .map(|message| crate::agent::runner::hub_role(message.role))
        .collect();
    let keep_turns = usize::from(state.settings().ai.agent.keep_recent_turns);
    let compact_turns = session
        .target
        .as_ref()
        .and_then(|_| {
            compaction_cut(
                &roles,
                |index| history[index].role == AgentRole::Summary,
                keep_turns,
            )
        })
        .map(|cut| {
            let turns = history[..cut.split]
                .iter()
                .filter(|message| message.role == AgentRole::User)
                .count()
                .max(1);
            u32::try_from(turns).unwrap_or(u32::MAX)
        });
    let can_compact = compact_turns.is_some();

    let mut split = Split::default();
    for part in history.into_iter().flat_map(|message| message.parts) {
        match part {
            HubPart::Image { .. } | HubPart::Document { .. } | HubPart::Audio { .. } => {
                split.attachments.push(part);
            }
            HubPart::ToolResult { .. } => split.tool_results.push(part),
            _ => split.messages.push(part),
        }
    }
    let (instructions, skills) = (resolved.instructions_tokens, resolved.skills_tokens);
    let system = estimate_context_tokens(Some(&resolved.system), &[], &[])
        .saturating_sub(instructions + skills);
    let mut breakdown = ContextBreakdown {
        system,
        instructions,
        skills,
        tool_definitions: estimate_context_tokens(None, &[], &resolved.specs),
        messages: tokens(split.messages),
        tool_results: tokens(split.tool_results),
        attachments: tokens(split.attachments),
        total: 0,
        limit: model.context_limit,
        can_compact,
        compact_turns,
    };
    breakdown.total = breakdown.system
        + breakdown.instructions
        + breakdown.skills
        + breakdown.tool_definitions
        + breakdown.messages
        + breakdown.tool_results
        + breakdown.attachments;
    Ok(breakdown)
}
