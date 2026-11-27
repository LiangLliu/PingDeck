//! Answering a tool approval or an `ask_user` question: the use-cases behind those cards.
//!
//! Single responsibility: turn the card's choice into a saved permission rule (the answer's scope)
//! and deliver the answer to the run that waits. A rule is written before the run is released, so a
//! scope that cannot be saved (one the card did not offer) is reported to the card and the call
//! keeps waiting. Edited arguments are only syntax-checked here; the run validates
//! them with the tool before running anything.

use crate::agent::approvals::{ApprovalAnswer, RuleContext};
use crate::agent::gate::{GateAnswer, GatePayload};
use crate::agent::model::{AgentQuestionAnswer, AgentToolPolicy, ApprovalDecision, ApprovalScope};
use crate::agent::permissions::{PermissionAction, PermissionRule};
use crate::agent::tools::ask_user;
use crate::error::AppError;

use crate::app::{AppState, command_error};

/// What an answer leaves behind.
enum Remembered {
    /// Nothing ("once", or any denial: a denial is the user's answer to this call only).
    Nothing,
    /// A narrow stored rule for this chat or this folder.
    Rule(PermissionRule),
    /// "Always allow this tool", offered only for read-only tools: a per-tool Settings value.
    ToolPolicy(AgentToolPolicy),
}

/// What an answer of `scope` leaves behind, if anything. A scope the card did not offer is
/// refused: the menu offers exactly the scopes of the scope table.
fn remembered(
    context: &RuleContext,
    decision: ApprovalDecision,
    scope: ApprovalScope,
) -> Result<Remembered, AppError> {
    if decision == ApprovalDecision::Deny || scope == ApprovalScope::Once {
        return Ok(Remembered::Nothing);
    }
    if !context.offered.contains(&scope) {
        return Err(AppError::validation(
            "this approval does not offer that scope; allow it once",
        ));
    }
    let rule = |session_id: Option<String>, workspace_dir: Option<String>| PermissionRule {
        id: String::new(),
        session_id,
        workspace_dir,
        tool_pattern: context.tool.clone(),
        arg_pattern: context.pattern.clone(),
        action: PermissionAction::Allow,
        created_at_ms: 0,
    };
    match scope {
        ApprovalScope::Once => Ok(Remembered::Nothing),
        ApprovalScope::Session => Ok(Remembered::Rule(rule(
            Some(context.session_id.clone()),
            None,
        ))),
        ApprovalScope::WorkspacePattern => {
            let dir = context.workspace_dir.clone().ok_or_else(|| {
                AppError::validation("this chat has no folder to remember the choice for")
            })?;
            Ok(Remembered::Rule(rule(None, Some(dir))))
        }
        ApprovalScope::Tool => Ok(Remembered::ToolPolicy(AgentToolPolicy {
            tool_key: context.tool.clone(),
            policy: PermissionAction::Allow,
        })),
    }
}

/// Answers the approval of `call_id` in run `run_id`. `answer.scope` decides what is remembered,
/// always something narrower than the tool: `Once` nothing, `Session` the card's rule (an argv
/// prefix, a folder, a host, a method and host, a provider) in this chat, `WorkspacePattern` the
/// same rule in this chat's folder, `Tool` (read-only tools only) a per-tool Settings value; a
/// denial is never remembered. Only the scopes the card offered are accepted. `edited_arguments` replace the call's arguments (re-validated before the tool runs);
/// `allow_network_once` lets this one shell call use the network. Returns `false` when nothing
/// waits for the call (already answered, timed out, or the run ended).
///
/// # Errors
/// [`AppError::Validation`] for unparseable edited arguments or a scope that cannot be saved; the
/// call then keeps waiting.
pub fn agent_approval_answer_impl(
    state: &AppState,
    run_id: &str,
    call_id: &str,
    answer: ApprovalAnswer,
) -> Result<bool, AppError> {
    // The claim keeps a second answer from saving a rule for the same call.
    let Some(context) = state.agent.approvals.begin_answer(run_id, call_id) else {
        return Ok(false);
    };
    let saved = match save_remembered(state, &context, &answer) {
        Ok(saved) => saved,
        Err(error) => {
            state.agent.approvals.abort_answer(run_id, call_id);
            return Err(error);
        }
    };
    let delivered = state.agent.approvals.deliver(run_id, call_id, answer);
    if !delivered {
        // The call ended while the rule was being saved: no standing allow for a call that
        // never ran. Best effort: a failing delete leaves what the user chose to remember.
        saved.roll_back(state);
    }
    state.agent.publish_attention();
    Ok(delivered)
}

/// Answers the `ask_user` question of `call_id` in run `run_id` with options, the user's own
/// text or a skip. The answer only informs the model: it saves no rule and approves no call.
/// Returns `false` when nothing waits for the question (answered, timed out, or the run ended).
///
/// # Errors
/// [`AppError::Validation`] for an answer the question does not take (an option it lacks, two
/// picks of a single-choice question, nothing chosen); the question keeps waiting.
pub fn agent_question_answer_impl(
    state: &AppState,
    run_id: &str,
    call_id: &str,
    answer: AgentQuestionAnswer,
) -> Result<bool, AppError> {
    let gates = &state.agent.gates;
    let Some(payload) = gates.begin_answer(run_id, call_id) else {
        return Ok(false);
    };
    let checked = match &payload {
        GatePayload::Question(card) => ask_user::check_answer(card, &answer),
        _ => Err(AppError::validation("this gate is not a question")),
    };
    if let Err(error) = checked {
        gates.abort_answer(run_id, call_id);
        return Err(error);
    }
    let delivered = gates.deliver(run_id, call_id, GateAnswer::Question(answer));
    state.agent.publish_attention();
    Ok(delivered)
}

/// What [`save_remembered`] wrote, so that it can be taken back.
enum Saved {
    Nothing,
    Rule(String),
    /// The policy of the tool before the answer (`None`: it had none).
    ToolPolicy {
        tool_key: String,
        before: Option<AgentToolPolicy>,
    },
}

impl Saved {
    fn roll_back(self, state: &AppState) {
        let store = state.store();
        let _ = match self {
            Self::Nothing => Ok(()),
            Self::Rule(id) => store.delete_permission_rule(&id).map(drop),
            Self::ToolPolicy {
                tool_key,
                before: None,
            } => store.delete_tool_policy(&tool_key).map(drop),
            Self::ToolPolicy {
                before: Some(policy),
                ..
            } => store.set_tool_policy(&policy),
        };
    }
}

/// Checks the edited arguments and stores the rule or policy the answer's scope asks for.
fn save_remembered(
    state: &AppState,
    context: &RuleContext,
    answer: &ApprovalAnswer,
) -> Result<Saved, AppError> {
    if let Some(edited) = &answer.edited_arguments {
        serde_json::from_str::<serde_json::Value>(edited).map_err(|error| {
            AppError::validation(format!("the edited arguments are not valid JSON: {error}"))
        })?;
    }
    Ok(match remembered(context, answer.decision, answer.scope)? {
        Remembered::Nothing => Saved::Nothing,
        Remembered::Rule(rule) => Saved::Rule(
            state
                .store()
                .save_permission_rule(&rule)
                .map_err(command_error)?
                .id,
        ),
        Remembered::ToolPolicy(mut policy) => {
            // An MCP tool's policy is keyed by server id, so it survives a rename.
            policy.tool_key = super::config::policy_key_of(state, &policy.tool_key);
            let store = state.store();
            let before = store
                .list_tool_policy()
                .map_err(command_error)?
                .into_iter()
                .find(|existing| existing.tool_key == policy.tool_key);
            store.set_tool_policy(&policy).map_err(command_error)?;
            Saved::ToolPolicy {
                tool_key: policy.tool_key,
                before,
            }
        }
    })
}
