//! What Settings › AI › Tools, Skills and Permissions show on top of the plain lists.
//!
//! Single responsibility: enrich the lists of [`super::config`] and [`super::policies`] with
//! derived facts so the UI does not repeat the tables behind them: a tool's permission class, whether
//! "Always allow" may be offered for it and whether an MCP tool appeared since the page was last
//! read; why a skill cannot be used; whether a saved rule can still match; the state of the skill
//! folders and of the shell sandbox. Nothing here stores anything.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agent::model::{AgentSkill, AgentToolInfo};
use crate::agent::permissions::{
    PermissionRule, SafetyContext, blocked_by_safety_rules, glob_match,
};
use crate::agent::sandbox::SandboxKind;
use crate::agent::tools::ToolClass;
use crate::agent::tools::shell_meta::SHELL;
use crate::error::AppError;

use super::config::{agent_skill_list_impl, agent_tool_list_impl};
use super::policies::agent_permission_rule_list_impl;
use super::tools::builtin_specs;
use crate::app::AppState;
use crate::app::app_info_service::{SandboxInfo, SandboxSelftest, sandbox_info};

/// Tools whose standing "Allow" is never offered: they act outside the folder or on the user's
/// own requests, so each call is approved on its own.
const NEVER_ALLOWED: [&str; 4] = ["shell", "send_request", "create_request", "run_probe"];

/// The `source` of a built-in tool in [`AgentToolInfo`].
const BUILTIN: &str = "builtin";

/// One tool of the Tools page: the plain facts plus what the page derives from them.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentToolEntry {
    /// The plain tool facts.
    #[serde(flatten)]
    pub info: AgentToolInfo,
    /// The capability-matrix row of the tool.
    pub class: ToolClass,
    /// Whether the approval card may offer a standing "Allow" for this tool (D-223).
    pub allow_offered: bool,
    /// An MCP tool that was not in the list the last time it was read.
    pub is_new: bool,
}

/// A skill with the reason it cannot be used.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSkillEntry {
    /// The plain skill facts.
    #[serde(flatten)]
    pub skill: AgentSkill,
    /// Why the skill cannot be used (`Missing description`); `None` for a usable one.
    pub invalid: Option<String>,
}

/// Why a saved rule can never fire.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleInactive {
    /// A safety rule refuses every command the rule would allow.
    SafetyRule,
    /// A shell rule that names several commands in one line: rules only match one command.
    ChainedCommands,
}

/// A saved rule with what the Permissions list shows about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PermissionRuleView {
    /// The rule as stored.
    #[serde(flatten)]
    pub rule: PermissionRule,
    /// Whether the rule's workspace folder still exists (always true for a rule of every folder).
    pub folder_exists: bool,
    /// Why the rule can never fire, if so.
    pub inactive: Option<RuleInactive>,
}

/// The result of testing the shell sandbox.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SandboxTestDto {
    /// The tier whose tool is installed.
    pub tier: SandboxKind,
    /// Whether that tier works.
    pub selftest: SandboxSelftest,
    /// One sentence for the page.
    pub detail: String,
}

/// The class of one listed tool: a built-in's declared class, or the class the MCP adapter
/// registers (read for a read-only tool, network otherwise).
fn class_of(info: &AgentToolInfo, builtin: &HashMap<String, ToolClass>) -> ToolClass {
    if info.source == BUILTIN {
        // A built-in the registry does not know cannot be listed; the strictest class if it were.
        builtin.get(&info.name).copied().unwrap_or(ToolClass::Exec)
    } else if info.read_only {
        ToolClass::Read
    } else {
        ToolClass::Network
    }
}

/// Whether a standing "Allow" may be offered. `read_only` of an MCP tool is already the effective
/// one: only a trusted server's hint counts.
fn allow_offered(info: &AgentToolInfo, class: ToolClass) -> bool {
    !NEVER_ALLOWED.contains(&info.name.as_str())
        && class != ToolClass::Internal
        && (info.source == BUILTIN || info.read_only)
}

/// Every tool the UI can offer, with its class, whether Allow may be offered and whether it is new.
///
/// "New" is relative to the previous call in this process; the first call after launch only
/// records the baseline, so nothing is new then (shortcut: the baseline is not stored, upgrade to
/// a persisted marker if tools that appear while the app is closed must be flagged).
///
/// # Errors
/// As [`agent_tool_list_impl`].
pub async fn agent_tool_entries_impl(
    state: &AppState,
    session_id: Option<&str>,
) -> Result<Vec<AgentToolEntry>, AppError> {
    let tools = agent_tool_list_impl(state, session_id).await?;
    let new = state.agent.seen_tools.observe(
        tools
            .iter()
            .filter(|info| info.source != BUILTIN)
            .map(|info| info.name.as_str()),
    );
    Ok(tool_entries(tools, &new))
}

/// What [`agent_tool_entries_impl`] derives from the plain tool facts, without touching the
/// "seen" baseline: readers that only want the class and the Allow rule (the AI snapshot) pass an
/// empty `new` set.
pub(crate) fn tool_entries(
    tools: Vec<AgentToolInfo>,
    new: &HashSet<String>,
) -> Vec<AgentToolEntry> {
    let builtin: HashMap<String, ToolClass> = builtin_specs()
        .into_iter()
        .map(|spec| (spec.name, spec.class))
        .collect();
    tools
        .into_iter()
        .map(|info| {
            let class = class_of(&info, &builtin);
            AgentToolEntry {
                allow_offered: allow_offered(&info, class),
                is_new: new.contains(&info.name),
                class,
                info,
            }
        })
        .collect()
}

/// The skills the agent can see, each with why it cannot be used (a skill without a description
/// is listed but never offered to the model).
///
/// # Errors
/// As [`agent_skill_list_impl`].
pub fn agent_skill_entries_impl(
    state: &AppState,
    workspace_dir: Option<&str>,
) -> Result<Vec<AgentSkillEntry>, AppError> {
    Ok(agent_skill_list_impl(state, workspace_dir)?
        .into_iter()
        .map(|skill| AgentSkillEntry {
            invalid: skill
                .description
                .trim()
                .is_empty()
                .then(|| "Missing description".to_owned()),
            skill,
        })
        .collect())
}

/// Characters that make a shell rule name several commands.
fn is_chained(pattern: &str) -> bool {
    pattern.contains(['&', ';', '|', '\n', '`']) || pattern.contains("$(")
}

/// Why `rule` can never fire, given the safety rules `blocked`.
fn inactive_reason(rule: &PermissionRule, blocked: &[String]) -> Option<RuleInactive> {
    let command = rule.arg_pattern.as_deref()?;
    if !glob_match(&rule.tool_pattern, SHELL) {
        return None;
    }
    if is_chained(command) {
        return Some(RuleInactive::ChainedCommands);
    }
    // A trailing `*` stands for "any further words": the bare prefix is what the rule allows.
    let prefix = command.trim_end_matches('*').trim_end();
    let context = SafetyContext {
        folder: None,
        data_dir: None,
        home: None,
    };
    blocked_by_safety_rules(blocked, prefix, &context)
        .is_some()
        .then_some(RuleInactive::SafetyRule)
}

/// The saved rules that can apply to a chat, with `folder_exists` and `inactive`. Without
/// `session_id` only the global rules (those of no chat) are listed.
///
/// # Errors
/// As [`agent_permission_rule_list_impl`].
pub fn agent_permission_rule_views_impl(
    state: &AppState,
    session_id: Option<&str>,
    workspace_dir: Option<&str>,
) -> Result<Vec<PermissionRuleView>, AppError> {
    let blocked = state.settings().ai.agent.blocked_commands.clone();
    Ok(
        agent_permission_rule_list_impl(state, session_id, workspace_dir)?
            .into_iter()
            .map(|rule| PermissionRuleView {
                folder_exists: rule
                    .workspace_dir
                    .as_deref()
                    .is_none_or(|dir| Path::new(dir).is_dir()),
                inactive: inactive_reason(&rule, &blocked),
                rule,
            })
            .collect(),
    )
}

/// Runs (the first time) or recalls the shell sandbox self-test. Blocking: the first call starts
/// a trivial command under the sandbox.
#[must_use]
pub fn agent_sandbox_test_impl() -> SandboxTestDto {
    let SandboxInfo { tier, selftest } = sandbox_info();
    let detail = match (tier, selftest) {
        (SandboxKind::None, _) | (_, SandboxSelftest::NotRun) => {
            "No operating-system sandbox is available here: commands rely on folder limits and approvals."
        }
        (SandboxKind::Seatbelt, SandboxSelftest::Passed) => {
            "Seatbelt ran a test command inside its profile."
        }
        (SandboxKind::Bubblewrap, SandboxSelftest::Passed) => {
            "Bubblewrap ran a test command inside its profile."
        }
        (_, SandboxSelftest::Failed) => {
            "The sandbox tool is installed but cannot apply its profile here (often because PingDeck itself runs inside a sandbox)."
        }
    };
    SandboxTestDto {
        tier,
        selftest,
        detail: detail.to_owned(),
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::permissions::PermissionAction;

    fn tool(name: &str, source: &str, read_only: bool) -> AgentToolInfo {
        AgentToolInfo {
            policy_key: None,
            name: name.to_owned(),
            description: String::new(),
            read_only,
            destructive: false,
            source: source.to_owned(),
            available: true,
            needs_workspace: false,
        }
    }

    fn rule(tool_pattern: &str, arg_pattern: Option<&str>, dir: Option<&str>) -> PermissionRule {
        PermissionRule {
            id: String::new(),
            session_id: None,
            workspace_dir: dir.map(str::to_owned),
            tool_pattern: tool_pattern.to_owned(),
            arg_pattern: arg_pattern.map(str::to_owned),
            action: PermissionAction::Allow,
            created_at_ms: 0,
        }
    }

    #[test]
    fn allow_is_offered_for_built_in_tools_except_the_four_and_for_read_only_mcp_tools() {
        let builtin = builtin_specs()
            .into_iter()
            .map(|spec| (spec.name, spec.class))
            .collect();
        let offered = |info: &AgentToolInfo| allow_offered(info, class_of(info, &builtin));
        assert!(offered(&tool("read_file", BUILTIN, true)));
        assert!(offered(&tool("write_file", BUILTIN, false)));
        for name in NEVER_ALLOWED {
            assert!(!offered(&tool(name, BUILTIN, false)), "{name}");
        }
        assert!(!offered(&tool("todo_write", BUILTIN, false)), "internal");
        assert!(offered(&tool("mcp__git__log", "git", true)));
        assert!(!offered(&tool("mcp__git__push", "git", false)));
        assert_eq!(
            class_of(&tool("mcp__git__log", "git", true), &builtin),
            ToolClass::Read
        );
        assert_eq!(
            class_of(&tool("mcp__git__push", "git", false), &builtin),
            ToolClass::Network
        );
        assert_eq!(
            class_of(&tool("shell", BUILTIN, false), &builtin),
            ToolClass::Exec
        );
    }

    #[test]
    fn a_rule_is_inactive_when_a_safety_rule_refuses_it_or_it_chains_commands() {
        let blocked = vec!["sudo *".to_owned(), "rm -rf /".to_owned()];
        let why = |tool: &str, arg: Option<&str>| inactive_reason(&rule(tool, arg, None), &blocked);
        assert_eq!(
            why("shell", Some("sudo apt *")),
            Some(RuleInactive::SafetyRule)
        );
        assert_eq!(why("shell", Some("cargo test *")), None);
        assert_eq!(
            why("shell", Some("make && make install")),
            Some(RuleInactive::ChainedCommands)
        );
        assert_eq!(why("shell", None), None);
        assert_eq!(why("read_file", Some("sudo *")), None, "not a shell rule");
        assert_eq!(why("*", Some("sudo *")), Some(RuleInactive::SafetyRule));
    }

    #[test]
    fn rule_views_leave_out_chat_rules_without_a_chat_and_check_the_folder() {
        let state = AppState::open_in_memory().unwrap();
        let session = crate::agent_service::agent_session_create_impl(
            &state,
            crate::agent::model::AgentSessionInput::default(),
        )
        .unwrap();
        let temp = std::env::temp_dir();
        let existing = temp.to_string_lossy().into_owned();
        let save = |rule: PermissionRule| state.store().save_permission_rule(&rule).unwrap();
        save(rule("shell", Some("sudo *"), Some(&existing)));
        let ask = |dir: &str| PermissionRule {
            action: PermissionAction::Ask,
            ..rule("edit_file", None, Some(dir))
        };
        save(ask(&existing));
        save(ask("/no/such/folder/pd"));
        save(PermissionRule {
            session_id: Some(session.id.clone()),
            workspace_dir: None,
            action: PermissionAction::Ask,
            ..rule("fetch_url", None, None)
        });

        let global = agent_permission_rule_views_impl(&state, None, None).unwrap();
        assert_eq!(global.len(), 3, "the chat's own rule needs its chat");
        let by_dir = |dir: &str| {
            global
                .iter()
                .find(|view| view.rule.workspace_dir.as_deref() == Some(dir))
                .unwrap()
        };
        assert!(by_dir(&existing).folder_exists);
        assert!(!by_dir("/no/such/folder/pd").folder_exists);
        let sudo = global
            .iter()
            .find(|view| view.rule.arg_pattern.is_some())
            .unwrap();
        assert_eq!(sudo.inactive, Some(RuleInactive::SafetyRule));
        assert!(sudo.folder_exists);

        let with_chat = agent_permission_rule_views_impl(&state, Some(&session.id), None).unwrap();
        assert_eq!(with_chat.len(), 4);
    }

    #[test]
    fn skills_without_a_description_are_listed_with_the_reason() {
        let root = std::env::temp_dir().join(format!("pd-skills-{}", std::process::id()));
        let _ = std::fs::remove_dir_all(&root);
        for (dir, text) in [
            ("good", "---\nname: good\ndescription: does things\n---\n"),
            ("bare", "---\nname: bare\n---\n"),
        ] {
            std::fs::create_dir_all(root.join(dir)).unwrap();
            std::fs::write(root.join(dir).join("SKILL.md"), text).unwrap();
        }
        let state = AppState::open_in_memory().unwrap();
        crate::app::settings_update_impl(
            &state,
            crate::settings::SettingsSection::Ai,
            &serde_json::json!({ "agent": { "skill_dirs": [root.to_string_lossy()] } }),
        )
        .unwrap();

        let skills = agent_skill_entries_impl(&state, None).unwrap();
        let invalid = |name: &str| {
            skills
                .iter()
                .find(|entry| entry.skill.name == name)
                .unwrap()
                .invalid
                .clone()
        };
        assert_eq!(invalid("good"), None);
        assert_eq!(invalid("bare").as_deref(), Some("Missing description"));
        let _ = std::fs::remove_dir_all(&root);
    }

    #[test]
    fn the_sandbox_test_says_what_it_found() {
        let result = agent_sandbox_test_impl();
        assert_eq!(
            result.selftest == SandboxSelftest::NotRun,
            result.tier == SandboxKind::None
        );
        assert!(!result.detail.is_empty());
    }

    #[test]
    fn only_tools_that_appear_after_the_first_read_are_new() {
        let seen = crate::agent::runtime::SeenTools::default();
        assert!(
            seen.observe(["a", "b"]).is_empty(),
            "the first read is the baseline"
        );
        assert_eq!(
            seen.observe(["a", "b", "c"]),
            HashSet::from(["c".to_owned()])
        );
        assert!(
            seen.observe(["a", "c"]).is_empty(),
            "a read clears the flag"
        );
        assert!(
            seen.observe(["b"]).is_empty(),
            "a tool that came back is not new"
        );
    }

    #[tokio::test]
    async fn the_tool_list_carries_classes_and_never_marks_built_ins_new() {
        let state = AppState::open_in_memory().unwrap();
        for _ in 0..2 {
            let tools = agent_tool_entries_impl(&state, None).await.unwrap();
            assert!(tools.iter().all(|entry| !entry.is_new));
            assert!(tools.iter().any(|entry| entry.class == ToolClass::Exec));
        }
    }

    #[tokio::test]
    async fn a_chats_tool_list_needs_a_chat_that_exists() {
        let state = AppState::open_in_memory().unwrap();
        let error = agent_tool_list_impl(&state, Some("missing"))
            .await
            .unwrap_err();
        assert!(matches!(error, AppError::NotFound { .. }), "{error:?}");
        let chat =
            crate::agent_service::agent_session_create_impl(&state, Default::default()).unwrap();
        let tools = agent_tool_list_impl(&state, Some(&chat.id)).await.unwrap();
        assert!(tools.iter().any(|tool| tool.name == "shell"));
    }
}
