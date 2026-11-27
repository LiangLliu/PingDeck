//! What a run is built from, resolved at its start and again at every step boundary where it can
//! change.
//!
//! Single responsibility: turn a chat row, the settings and the machine into the run's tool list,
//! system prompt and folder ([`Resolved`]), and say cheaply whether anything that feeds them
//! changed since ([`Fingerprint`]). A Folder chosen, cleared or trusted mid-run, Plan switched on
//! or off, a tool or skill toggled in the Details panel: the next model call carries the new
//! prompt and tools. In Plan (read-only) mode only Read, SpanBay-read, `read_skill`,
//! `read_attachment` and `todo_write` are offered. A folder's `AGENTS.md` /
//! `CLAUDE.md` reach the prompt only for a trusted folder whose instructions are unchanged since
//! it was trusted; a sub-agent's folder is its parent's, so it gets the parent's answer.

use std::path::{Path, PathBuf};
use std::sync::Arc;

use crate::agent::context::estimate_context_tokens;
use crate::agent::git_head::read_branch;
use crate::agent::model::AgentSession;
use crate::agent::prompt::{PromptInput, build_system_prompt};
use crate::agent::runner::ModelInfo;
use crate::agent::skills::{SkillInfo, render_skill_blocks, skill_bodies, skills_prompt_section};
use crate::agent::tools::{ToolClass, ToolRegistry, ToolSpec};
use crate::llm::usage::local_day;
use crate::settings::{RoleModels, ToolApproval};

use super::config::enabled_skills;
use super::host::blocking;
use super::roles::roles_prompt_section;
use super::tools::{Role, register_session_tools};
use crate::app::{AppState, current_time_ms};

/// Tools Plan mode offers besides the Read and SpanBay-read classes. `task` is allowed for
/// read-only `kind: "explore"` delegation (non-explore sub-agents are rejected during Plan mode).
const PLAN_TOOLS: [&str; 7] = [
    "read_skill",
    "read_attachment",
    "read_output",
    "todo_write",
    crate::agent_tools::subagent::TASK,
    crate::agent::tools::submit_plan::SUBMIT_PLAN,
    crate::agent::tools::ask_user::ASK_USER,
];

/// What the next model call is built from: when two fingerprints are equal, nothing needs
/// resolving again.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub(super) struct Fingerprint {
    workspace_dir: Option<String>,
    mode: ToolApproval,
    plan: bool,
    /// The chat is bound to a SpanBay workspace that still exists: the workspace tools are in
    /// the tool table. A deleted workspace flips it at the next step boundary.
    pub(super) bound: bool,
    web_enabled: bool,
    disabled_tools: Vec<String>,
    disabled_skills: Vec<String>,
    /// The chat's own instructions changed (they are part of the system prompt).
    instructions: Option<String>,
    /// The folder's trust or trust digest changed (the folder is trusted, digest of its
    /// instructions and skills while they are the trusted ones).
    trust: (bool, Option<String>),
    /// The sub-agent role models changed (they shape `task` and the prompt).
    roles: RoleModels,
    /// The user's global instructions changed (`ai.agent.global_instructions`, in the prompt).
    global_instructions: String,
}

impl Fingerprint {
    /// Whether the run's folder is trusted (false without a folder).
    pub(super) fn folder_trusted(&self) -> bool {
        self.trust.0
    }
}

/// A run's tools, prompt and folder.
#[derive(Default)]
pub(super) struct Resolved {
    pub fingerprint: Fingerprint,
    /// The tools the model may call (and the host may execute).
    pub registry: ToolRegistry,
    pub specs: Vec<ToolSpec>,
    pub system: String,
    pub workspace: Option<PathBuf>,
    /// The provider searches the web itself.
    pub native_web_search: bool,
    /// Names of MCP servers still connecting.
    pub connecting: Vec<String>,
    /// Estimated tokens of the skills catalogue inside `system`.
    pub skills_tokens: u64,
    /// The enabled skills, for the texts of skills attached to messages.
    pub skills: Vec<SkillInfo>,
    /// Estimated tokens of the project instructions inside `system`.
    pub instructions_tokens: u64,
    /// The trusted folder's git branch, for the read-before-write guard (`branch_state`).
    pub branch: Option<String>,
}

/// Whether Plan mode offers `spec`.
pub(super) fn plan_allows(spec: &ToolSpec) -> bool {
    matches!(spec.class, ToolClass::Read | ToolClass::SpanBayRead)
        || PLAN_TOOLS.contains(&spec.name.as_str())
}

/// `UTC+08:00` for an offset in minutes east.
fn zone_label(offset_minutes: i16) -> String {
    let sign = if offset_minutes < 0 { '-' } else { '+' };
    let minutes = offset_minutes.unsigned_abs();
    format!("UTC{sign}{:02}:{:02}", minutes / 60, minutes % 60)
}

/// What the store says about `workspace`: whether the folder is trusted and, while its
/// instructions and skills are the trusted ones, its project instructions (empty without a file)
/// with the folder's trust digest (`instructions::trust_digest`); `None` when untrusted or
/// changed since trusted.
fn trusted_instructions(
    state: &AppState,
    workspace: Option<&Path>,
) -> (bool, Option<(String, String)>) {
    let Some(dir) = workspace.and_then(|dir| dir.canonicalize().ok()) else {
        return (false, None);
    };
    let trust = super::trust::folder_trust(state, &dir);
    let instructions = trust.pinned.then(|| {
        let text = trust.instructions.map(|selected| selected.text());
        (text.unwrap_or_default(), trust.digest)
    });
    (trust.trusted, instructions)
}

/// What a run reads from the folder, off the async threads.
#[derive(Default)]
struct FolderFacts {
    /// The enabled skills (the chat's own switches applied).
    skills: Vec<SkillInfo>,
    trusted: bool,
    /// The trusted folder's instructions (empty without a file) and its trust digest.
    instructions: Option<(String, String)>,
    /// The trusted folder's git branch (an untrusted `HEAD` never reaches the prompt).
    branch: Option<String>,
}

/// The chat's SpanBay workspace name and its bound environment with whether it is guarded, for
/// the environment block; nothing for an unbound chat or a deleted workspace. Blocking.
fn spanbay_binding(state: &AppState, session_id: &str) -> (Option<String>, Option<(String, bool)>) {
    use crate::workspace::{EnvironmentId, WorkspaceId};
    let store = state.store();
    let Some(binding) = store.session_spanbay_binding(session_id).ok().flatten() else {
        return (None, None);
    };
    let Ok(workspace_id) = WorkspaceId::new(&binding.workspace_id) else {
        return (None, None);
    };
    let Some(workspace) = store.get_workspace(&workspace_id).ok().flatten() else {
        return (None, None);
    };
    let environment = binding
        .environment_id
        .and_then(|id| EnvironmentId::new(id).ok())
        .and_then(|id| store.get_environment(&workspace_id, &id).ok().flatten())
        .map(|environment| (environment.name, environment.confirm_before_send));
    (Some(workspace.name), environment)
}

/// The enabled skills, the trusted folder's instructions and its branch.
async fn folder_facts(state: &AppState, session: &AgentSession) -> FolderFacts {
    let (owner, settings) = (state.clone(), state.settings());
    let (workspace, disabled) = (
        session.workspace_dir.clone().map(PathBuf::from),
        session.disabled_skills.clone(),
    );
    blocking(state, move |_| {
        let (trusted, instructions) = trusted_instructions(&owner, workspace.as_deref());
        let skills = chat_skills(&owner, &settings, workspace.as_deref(), &disabled);
        let branch = workspace
            .as_deref()
            .filter(|_| trusted)
            .and_then(read_branch);
        FolderFacts {
            skills,
            trusted,
            instructions,
            branch,
        }
    })
    .await
    .unwrap_or_default()
}

/// The enabled skills of a chat in `workspace`, its own switches applied. Blocking.
fn chat_skills(
    state: &AppState,
    settings: &crate::settings::Settings,
    workspace: Option<&Path>,
    disabled: &[String],
) -> Vec<SkillInfo> {
    enabled_skills(state, settings, workspace)
        .unwrap_or_default()
        .into_iter()
        .filter(|skill| !disabled.contains(&skill.name))
        .collect()
}

/// The enabled skills of `session`, for a run that resolves nothing else (manual compaction).
pub(super) async fn session_skills(state: &AppState, session: &AgentSession) -> Vec<SkillInfo> {
    let (owner, session) = (state.clone(), session.clone());
    blocking(state, move |_| {
        let workspace = session.workspace_dir.as_deref().map(Path::new);
        chat_skills(
            &owner,
            &owner.settings(),
            workspace,
            &session.disabled_skills,
        )
    })
    .await
    .unwrap_or_default()
}

/// The skills attached as `names` that the model of `session` would not get in full. Blocking;
/// no folder scan when nothing is attached.
pub(super) fn undelivered_skills(
    state: &AppState,
    session: &AgentSession,
    names: &[String],
) -> Vec<crate::agent::model::SkillUndelivered> {
    if names.is_empty() {
        return Vec::new();
    }
    let workspace = session.workspace_dir.as_deref().map(Path::new);
    let skills = chat_skills(
        state,
        &state.settings(),
        workspace,
        &session.disabled_skills,
    );
    render_skill_blocks(&skill_bodies(&skills, names)).1
}

/// The fingerprint of `session` now. Blocking (it asks the store about trust).
pub(super) fn fingerprint(state: &AppState, session: &AgentSession, plan: bool) -> Fingerprint {
    let workspace = session.workspace_dir.as_deref().map(Path::new);
    let (trusted, instructions) = trusted_instructions(state, workspace);
    let settings = state.settings();
    let agent = &settings.ai.agent;
    let bound = spanbay_binding(state, &session.id).0.is_some();
    Fingerprint {
        workspace_dir: session.workspace_dir.clone(),
        mode: session.permission_mode,
        plan,
        bound,
        web_enabled: session.web_enabled,
        disabled_tools: session.disabled_tools.clone(),
        disabled_skills: session.disabled_skills.clone(),
        instructions: session.instructions.clone(),
        trust: (trusted, instructions.map(|(_, hash)| hash)),
        roles: agent.role_models.clone(),
        global_instructions: agent.global_instructions.clone(),
    }
}

/// Resolves the tools, prompt and folder of `session` for `role`.
pub(super) async fn resolve(
    state: &AppState,
    session: &AgentSession,
    model: &ModelInfo,
    role: Role,
    connect: bool,
) -> Resolved {
    let plan = role == Role::Main && state.agent.plan_active(&session.id);
    let FolderFacts {
        skills,
        trusted,
        instructions,
        branch,
    } = folder_facts(state, session).await;
    // Read before the tools register: a chat without a SpanBay workspace is not offered the
    // tools that act on one.
    let session_id = session.id.clone();
    let (spanbay_workspace, environment) =
        blocking(state, move |state| spanbay_binding(state, &session_id))
            .await
            .unwrap_or_default();
    let bound = spanbay_workspace.is_some();
    let tools = register_session_tools(state, session, model, &skills, role, connect, bound).await;
    let (registry, specs) = if plan {
        let mut kept = ToolRegistry::new();
        for spec in tools.registry.specs().into_iter().filter(plan_allows) {
            if let Some(tool) = tools.registry.get(&spec.name) {
                kept.register(Arc::clone(tool));
            }
        }
        // Only a main run in Plan mode hands its plan to the user.
        crate::agent::tools::submit_plan::register(&mut kept);
        let specs = kept.specs();
        (kept, specs)
    } else {
        let specs = tools.registry.specs();
        (tools.registry, specs)
    };
    let tool_names: Vec<String> = specs.iter().map(|spec| spec.name.clone()).collect();
    let workspace = session.workspace_dir.clone().map(PathBuf::from);
    let tz = state.llm().tz_offset_minutes();
    let today = local_day(current_time_ms(), tz);
    let time_zone = zone_label(tz);
    let shell = std::env::var("SHELL").ok();
    let skills_section = if tool_names.iter().any(|name| name == "read_skill") {
        skills_prompt_section(&skills)
    } else {
        String::new()
    };
    let settings = state.settings();
    let roles = settings.ai.agent.role_models.clone();
    let roles_section = if role == Role::Main && tool_names.iter().any(|name| name == "task") {
        roles_prompt_section(state, &roles).await
    } else {
        String::new()
    };
    let fingerprint = Fingerprint {
        workspace_dir: session.workspace_dir.clone(),
        mode: session.permission_mode,
        plan,
        bound,
        web_enabled: session.web_enabled,
        disabled_tools: session.disabled_tools.clone(),
        disabled_skills: session.disabled_skills.clone(),
        instructions: session.instructions.clone(),
        trust: (trusted, instructions.as_ref().map(|(_, hash)| hash.clone())),
        roles,
        global_instructions: settings.ai.agent.global_instructions.clone(),
    };
    let mut system = build_system_prompt(&PromptInput {
        kind: role.prompt_kind(),
        os: std::env::consts::OS,
        os_version: None,
        shell: shell.as_deref(),
        workspace_dir: workspace.as_deref(),
        git_branch: branch.as_deref(),
        tool_names: &tool_names,
        native_search: tools.native_web_search,
        mode: session.permission_mode,
        plan_mode: plan,
        skills_section: &skills_section,
        roles_section: &roles_section,
        global_instructions: Some(&settings.ai.agent.global_instructions),
        chat_instructions: session.instructions.as_deref(),
        folder_trusted: trusted,
        project_instructions: instructions.as_ref().map(|(text, _)| text.as_str()),
        spanbay_workspace: spanbay_workspace.as_deref(),
        environment: environment
            .as_ref()
            .map(|(name, guarded)| (name.as_str(), *guarded)),
        today: &today,
        time_zone: Some(&time_zone),
        sandbox: crate::agent::sandbox::sandbox_status().usable(),
    });
    // Changes from run to run, so it goes last.
    for name in &tools.connecting {
        system.push_str(&format!(
            "\nMCP server {name} is still connecting; its tools are not available in this step."
        ));
    }
    Resolved {
        fingerprint,
        registry,
        specs,
        system,
        workspace,
        native_web_search: tools.native_web_search,
        connecting: tools.connecting,
        skills_tokens: estimate_context_tokens(Some(&skills_section), &[], &[]),
        skills,
        instructions_tokens: instructions
            .as_ref()
            .filter(|(text, _)| !text.is_empty())
            .map_or(0, |(text, _)| estimate_context_tokens(Some(text), &[], &[])),
        branch,
    }
}
