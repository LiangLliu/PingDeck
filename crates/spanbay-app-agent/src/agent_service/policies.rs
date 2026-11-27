//! The permission screens' data: per-tool settings, saved permission rules, the safety rules
//! (pinned defaults plus the user's add-only extras) and the per-chat "Run without a
//! sandbox" acknowledgement.
//!
//! Single responsibility: store-backed reads and edits for Settings › AI › Tools. The decision
//! itself is `agent::permissions`; the run reads these tables per call.

use serde::{Deserialize, Serialize};

use crate::agent::model::AgentToolPolicy;
use crate::agent::permissions::{PINNED_RULES, PermissionAction, PermissionRule};
use crate::agent::sandbox::{SandboxKind, SandboxState, sandbox_status};
use crate::error::AppError;

use crate::app::{AppState, command_error};

/// One safety rule as the settings page lists it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BlockedRule {
    /// Stable id of the rule (`pinned:<id>` or `command:<pattern>`).
    pub id: String,
    /// The rule in words: the command pattern that is refused.
    pub summary: String,
    /// A pinned default (read-only in Settings, never removable) or one of the user's extras.
    pub pinned: bool,
}

/// Tools whose per-tool "Allow" is never offered: they run commands, send data out or create
/// requests, so each call keeps asking unless a narrow rule covers it.
const NO_TOOL_ALLOW: [&str; 4] = ["shell", "run_probe", "send_request", "create_request"];

/// The global per-tool policies, by tool key.
///
/// # Errors
/// A storage failure.
pub fn agent_tool_policy_list_impl(state: &AppState) -> Result<Vec<AgentToolPolicy>, AppError> {
    state.store().list_tool_policy().map_err(command_error)
}

/// Sets the policy of `tool_key`, or removes it (`None`: back to the permission mode's default,
/// which stores nothing); returns what is stored now.
///
/// # Errors
/// [`AppError::Validation`] for a tool key outside `1..=200` characters of letters, digits and
/// `_ . : -` (one tool, never a pattern), and for "Allow" on `shell`, `run_probe`,
/// `send_request` or `create_request`.
pub fn agent_tool_policy_set_impl(
    state: &AppState,
    tool_key: &str,
    policy: Option<PermissionAction>,
) -> Result<Option<AgentToolPolicy>, AppError> {
    if policy == Some(PermissionAction::Allow)
        && NO_TOOL_ALLOW
            .iter()
            .any(|tool| crate::agent::permissions::glob_match(tool_key, tool))
    {
        return Err(AppError::validation(format!(
            "`{tool_key}` can only be allowed once or with a narrow rule, never as a whole tool"
        )));
    }
    let store = state.store();
    match policy {
        Some(policy) => {
            let stored = AgentToolPolicy {
                tool_key: tool_key.to_owned(),
                policy,
            };
            store.set_tool_policy(&stored).map_err(command_error)?;
            Ok(Some(stored))
        }
        None => {
            store.delete_tool_policy(tool_key).map_err(command_error)?;
            Ok(None)
        }
    }
}

/// The saved rules that can apply to a chat: the folder rules of `workspace_dir` (all folder
/// rules when it is `None`) and, when given, the chat rules of `session_id`; oldest first.
///
/// # Errors
/// A storage failure.
pub fn agent_permission_rule_list_impl(
    state: &AppState,
    session_id: Option<&str>,
    workspace_dir: Option<&str>,
) -> Result<Vec<PermissionRule>, AppError> {
    let mut rules = state
        .store()
        .list_permission_rules(session_id)
        .map_err(command_error)?;
    if let Some(dir) = workspace_dir {
        rules.retain(|rule| rule.workspace_dir.as_deref().is_none_or(|own| own == dir));
    }
    Ok(rules)
}

/// Deletes a saved rule; `false` when it did not exist.
///
/// # Errors
/// A storage failure.
pub fn agent_permission_rule_delete_impl(
    state: &AppState,
    rule_id: &str,
) -> Result<bool, AppError> {
    state
        .store()
        .delete_permission_rule(rule_id)
        .map_err(command_error)
}

/// The safety rules the shell tool refuses in every permission mode: the pinned defaults first,
/// then the user's extra patterns (`ai.agent.blocked_commands`).
#[must_use]
pub fn agent_blocked_rules_impl(state: &AppState) -> Vec<BlockedRule> {
    let pinned = PINNED_RULES.iter().map(|rule| BlockedRule {
        id: format!("pinned:{}", rule.id),
        summary: rule.summary.to_owned(),
        pinned: true,
    });
    let settings = state.settings();
    let extra = settings
        .ai
        .agent
        .blocked_commands
        .iter()
        .map(|pattern| BlockedRule {
            id: format!("command:{pattern}"),
            summary: pattern.clone(),
            pinned: false,
        });
    pinned.chain(extra).collect()
}

/// The newest calls a Block or a hard block refused (at most 50, newest first), as rows of the
/// same list: `id` is the audit's `decided_by` (`hard_block:sudo`, `block:rule:<id>`, …) and
/// `summary` the tool with the start of its arguments.
///
/// # Errors
/// A storage failure.
pub fn agent_recent_blocks_impl(state: &AppState) -> Result<Vec<BlockedRule>, AppError> {
    const RECENT: usize = 50;
    const ARGS: usize = 120;
    let calls = state
        .store()
        .recent_blocked_calls(RECENT)
        .map_err(command_error)?;
    Ok(calls
        .into_iter()
        .map(|call| {
            let args: String = call.args_json.chars().take(ARGS).collect();
            BlockedRule {
                id: call.decided_by.unwrap_or_default(),
                summary: format!("{}: {args}", call.name),
                pinned: false,
            }
        })
        .collect())
}

/// Records or withdraws the chat's "Run without a sandbox" acknowledgement. It lasts for the app
/// process and is never inherited by a new chat or turned on by Full access.
pub fn agent_sandbox_acknowledge_impl(state: &AppState, session_id: &str, on: bool) {
    state.agent.approvals.acknowledge_sandbox(session_id, on);
    state.agent.publish_attention();
}

/// What Details › Permissions shows about the shell sandbox for one chat.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxStatusDto {
    /// Whether the sandbox applies on this machine (the cached startup self-test).
    pub state: SandboxState,
    /// The tier of the tool found.
    pub tier: SandboxKind,
    /// Whether the chat chose "Run without a sandbox" (this app run only).
    pub acknowledged: bool,
}

/// The sandbox self-test result and whether `session_id` runs without it. Blocking: the first
/// call of the process runs the probe.
#[must_use]
pub fn agent_sandbox_status_impl(state: &AppState, session_id: &str) -> SandboxStatusDto {
    let status = sandbox_status();
    SandboxStatusDto {
        state: status.state,
        tier: status.tier,
        acknowledged: state.agent.approvals.sandbox_acknowledged(session_id),
    }
}

#[cfg(test)]
#[allow(clippy::unwrap_used)]
mod tests {
    use super::*;

    #[test]
    fn the_sandbox_acknowledgement_is_per_chat_and_reported_with_the_status() {
        let state = AppState::open_in_memory().unwrap();
        assert!(!agent_sandbox_status_impl(&state, "a").acknowledged);
        agent_sandbox_acknowledge_impl(&state, "a", true);
        assert!(agent_sandbox_status_impl(&state, "a").acknowledged);
        assert!(!agent_sandbox_status_impl(&state, "b").acknowledged);
        agent_sandbox_acknowledge_impl(&state, "a", false);
        assert!(!agent_sandbox_status_impl(&state, "a").acknowledged);
    }

    #[test]
    fn a_tool_policy_is_set_listed_replaced_and_removed() {
        let state = AppState::open_in_memory().unwrap();
        assert!(agent_tool_policy_list_impl(&state).unwrap().is_empty());
        let set = |key, policy| agent_tool_policy_set_impl(&state, key, policy).unwrap();
        assert_eq!(
            set("shell", Some(PermissionAction::Block)).map(|p| p.policy),
            Some(PermissionAction::Block)
        );
        set("shell", Some(PermissionAction::Ask));
        set("mcp:mcp-1:get_issue", Some(PermissionAction::Allow));
        let listed = agent_tool_policy_list_impl(&state).unwrap();
        assert_eq!(listed.len(), 2);
        assert_eq!(listed[1].tool_key, "shell");
        assert_eq!(listed[1].policy, PermissionAction::Ask);
        assert_eq!(set("shell", None), None);
        assert_eq!(agent_tool_policy_list_impl(&state).unwrap().len(), 1);
        for bad in ["bad key!", "mcp:mcp-1:*"] {
            assert!(matches!(
                agent_tool_policy_set_impl(&state, bad, Some(PermissionAction::Allow)),
                Err(AppError::Validation { .. })
            ));
        }
    }

    #[test]
    fn allow_is_not_offered_for_shell_and_the_outbound_tools() {
        let state = AppState::open_in_memory().unwrap();
        for tool in ["shell", "run_probe", "send_request", "create_request"] {
            assert!(
                matches!(
                    agent_tool_policy_set_impl(&state, tool, Some(PermissionAction::Allow)),
                    Err(AppError::Validation { .. })
                ),
                "{tool}"
            );
            // Always ask and Block stay available.
            for action in [PermissionAction::Ask, PermissionAction::Block] {
                assert!(agent_tool_policy_set_impl(&state, tool, Some(action)).is_ok());
            }
        }
        assert!(
            agent_tool_policy_set_impl(&state, "fetch_url", Some(PermissionAction::Allow)).is_ok()
        );
        assert!(
            agent_tool_policy_list_impl(&state)
                .unwrap()
                .iter()
                .all(|p| p.tool_key != "shell" || p.policy != PermissionAction::Allow)
        );
    }

    #[test]
    fn rules_are_listed_by_workspace_and_deleted_once() {
        let state = AppState::open_in_memory().unwrap();
        let save = |dir: Option<&str>| {
            state
                .store()
                .save_permission_rule(&PermissionRule {
                    id: String::new(),
                    session_id: None,
                    workspace_dir: dir.map(str::to_owned),
                    tool_pattern: "shell".to_owned(),
                    arg_pattern: Some("cargo test *".to_owned()),
                    action: PermissionAction::Allow,
                    created_at_ms: 0,
                })
                .unwrap()
        };
        // Every rule has a folder or a chat: there are no global rules.
        let first = save(Some("/work/a"));
        save(Some("/work/a"));
        save(Some("/work/b"));
        let list = |dir| agent_permission_rule_list_impl(&state, None, dir).unwrap();
        assert_eq!(list(None).len(), 3);
        let for_a = list(Some("/work/a"));
        assert_eq!(for_a.len(), 2);
        assert!(
            for_a
                .iter()
                .all(|rule| rule.workspace_dir.as_deref() != Some("/work/b"))
        );
        assert!(agent_permission_rule_delete_impl(&state, &first.id).unwrap());
        assert!(!agent_permission_rule_delete_impl(&state, &first.id).unwrap());
    }

    #[test]
    fn blocked_rules_list_the_pinned_defaults_then_the_extras() {
        let state = AppState::open_in_memory().unwrap();
        let rules = agent_blocked_rules_impl(&state);
        assert!(
            rules
                .iter()
                .take(PINNED_RULES.len())
                .all(|rule| rule.pinned)
        );
        assert!(rules.iter().any(|rule| rule.id == "pinned:privilege"));
        assert_eq!(
            rules.iter().filter(|rule| !rule.pinned).count(),
            state.settings().ai.agent.blocked_commands.len()
        );
    }
}
