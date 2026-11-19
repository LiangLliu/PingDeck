//! The tools a chat offers its run, and the helpers the settings screens use to list them.
//!
//! Single responsibility: build the [`ToolRegistry`] of one run from the chat's switches
//! ([`register_session_tools`] is the one place every tool family is added; sub-agent and task-list
//! tools register there too), and describe the built-in and MCP tools for the tool picker. The
//! tools themselves live in `crate::agent::tools` and `crate::agent::mcp`.

use std::collections::HashSet;
use std::sync::Arc;

use crate::agent::mcp::{
    CONNECT_TIMEOUT, McpResourceTool, McpTool, McpToolAdapter, resolve_transport,
};
use crate::agent::model::{
    AgentMcpServer, AgentSession, SubagentRole, ToolErrorCode, coded_error_text,
};
use crate::agent::prompt::PromptKind;
use crate::agent::runner::ModelInfo;
use crate::agent::skills::SkillInfo;
use crate::agent::tools::{ToolClass, ToolRegistry, ToolSource, ToolSpec, fs};
use crate::agent_tools::skill::READ_SKILL;
use crate::agent_tools::subagent::SubagentKind;
use crate::agent_tools::web::{FetchUrlTool, WEB_SEARCH, WebSearchTool};
use crate::agent_tools::{pingdeck, shell, skill, subagent, todo};
use crate::llm::capabilities::Capability;

use crate::app::AppState;

/// A run's tools and what building them found out.
pub(super) struct SessionTools {
    /// The tools the model may call.
    pub registry: ToolRegistry,
    /// The provider searches the web itself: `web_search` is not registered and the request
    /// carries the native tool.
    pub native_web_search: bool,
    /// Names of MCP servers with no cached tool list; they connect in the background and their
    /// tools appear from the next turn on.
    pub connecting: Vec<String>,
}

/// The enabled MCP servers with their transport placeholders resolved (blocking: the secret store
/// is the database). A server whose secret is missing is left out; its status says why.
async fn resolved_servers(
    state: &AppState,
) -> Vec<(AgentMcpServer, crate::agent::mcp::McpTransportConfig)> {
    let secrets = Arc::clone(state.secrets());
    let state = state.clone();
    tokio::task::spawn_blocking(move || {
        let servers = state.store().list_agent_mcp_servers().unwrap_or_default();
        servers
            .into_iter()
            .filter(|server| server.enabled)
            .filter(|server| !super::config::server_needs_review(&state, server))
            .filter_map(|server| {
                let transport = resolve_transport(secrets.as_ref(), &server.transport).ok()?;
                Some((server, transport))
            })
            .collect()
    })
    .await
    .unwrap_or_default()
}

/// Whose tools are built: the chat's own agent or one of its `task` sub-agents.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Role {
    /// The chat's own agent.
    Main,
    /// A sub-agent of this kind. Depth is 1: it never gets `task`.
    Subagent(SubagentKind),
}

impl Role {
    /// The system-prompt variant of the role.
    pub(super) const fn prompt_kind(self) -> PromptKind {
        match self {
            Self::Main => PromptKind::Main,
            Self::Subagent(SubagentKind::Explore) => PromptKind::Explore,
            Self::Subagent(SubagentKind::General) => PromptKind::General,
        }
    }
}

/// What an Explore sub-agent may use: file reads and searches, `web_search`
/// (no `fetch_url`: a URL can carry data out) and MCP tools of trusted servers that declare
/// themselves read-only.
fn explore_allows(spec: &ToolSpec) -> bool {
    match spec.source {
        ToolSource::Mcp { .. } => spec.read_only && !spec.destructive,
        ToolSource::Builtin => {
            matches!(spec.class, ToolClass::Read | ToolClass::PingDeckRead)
                || spec.name == WEB_SEARCH
        }
    }
}

/// Registers every tool family the chat may use: the file and shell tools with a bound folder,
/// web tools when the chat has web on, `read_skill` for the enabled `skills`, `todo_write`, `task`
/// for a chat's own agent that has a folder or web to hand a job about, and the MCP tools of the
/// enabled servers: with `connect`, a server without a cached list gets up to [`MCP_START_WAIT`]
/// to list its tools and an expired list refreshes in the background; without it only
/// cached lists count and nothing starts (the context panel). Tools in the
/// chat's `disabled_tools` are left out; an Explore sub-agent keeps only [`explore_allows`] tools.
/// `task` offers the roles `ai.agent.role_models` sets and is there with a vision role even
/// without a folder or web (images are a job to hand over). A sub-agent whose model is known to
/// lack tool calls (a role's reasoning-only model) gets no tools: it answers once. A chat without
/// a bound PingDeck workspace (`bound` is false) is not offered the
/// [`pingdeck::WORKSPACE_SCOPED`] tools; the settings picker still lists them.
pub(super) async fn register_session_tools(
    state: &AppState,
    session: &AgentSession,
    model: &ModelInfo,
    skills: &[SkillInfo],
    role: Role,
    connect: bool,
    bound: bool,
) -> SessionTools {
    let mut registry = ToolRegistry::new();
    if matches!(role, Role::Subagent(_)) && model.capabilities.known_absent(Capability::Tools) {
        return SessionTools {
            registry,
            native_web_search: false,
            connecting: Vec::new(),
        };
    }
    if session.workspace_dir.is_some() {
        fs::register(&mut registry);
        shell::register(&mut registry);
    }
    let native_web_search = session.web_enabled && model.capabilities.web_search.supported;
    if session.web_enabled {
        registry.register(Arc::new(FetchUrlTool::new()));
        if !native_web_search {
            registry.register(Arc::new(WebSearchTool::new()));
        }
    }
    skill::register(&mut registry, skills.to_vec());
    crate::agent_tools::attachment::register(&mut registry);
    crate::agent_tools::read_output::register(&mut registry);
    todo::register(&mut registry);
    pingdeck::register(&mut registry);
    let roles: Vec<SubagentRole> = state
        .settings()
        .ai
        .agent
        .role_models
        .configured()
        .into_iter()
        .map(|(role, _)| role)
        .collect();
    if role == Role::Main
        && (session.workspace_dir.is_some()
            || session.web_enabled
            || roles.contains(&SubagentRole::Vision))
    {
        subagent::register(&mut registry, &roles);
    }
    // Only the chat's own agent asks the user; a sub-agent writes its question in its report.
    if role == Role::Main {
        crate::agent::tools::ask_user::register(&mut registry);
    }

    let mut connecting = Vec::new();
    let mut pending = Vec::new();
    for (server, transport) in resolved_servers(state).await {
        let cached = state.agent.mcp.cached_tools(&server.id);
        if connect && (cached.is_none() || state.agent.mcp.tools_stale(&server.id)) {
            let (owner, proxy) = (state.clone(), super::config::app_proxy(state));
            let (id, config) = (server.id.clone(), transport.clone());
            // A failure is recorded by the manager and shows in the server's status.
            let task = tokio::spawn(async move {
                let mcp = &owner.agent.mcp;
                let _ = mcp
                    .ensure_tools(&id, &config, proxy.as_deref(), CONNECT_TIMEOUT)
                    .await;
            });
            if cached.is_none() {
                pending.push((server, transport, task));
                continue;
            }
        }
        match cached {
            Some(tools) => register_mcp(state, &mut registry, &server, &transport, tools),
            None => connecting.push(server.name.clone()),
        }
    }
    // A cold server gets a bounded wait; one still starting after it is announced instead.
    let deadline = tokio::time::Instant::now() + MCP_START_WAIT;
    for (server, transport, mut task) in pending {
        let finished = tokio::time::timeout_at(deadline, &mut task).await.is_ok();
        match state.agent.mcp.cached_tools(&server.id) {
            Some(tools) => register_mcp(state, &mut registry, &server, &transport, tools),
            None if !finished => connecting.push(server.name.clone()),
            None => {}
        }
    }

    let disabled: HashSet<&str> = session.disabled_tools.iter().map(String::as_str).collect();
    let explore = role == Role::Subagent(SubagentKind::Explore);
    let mut enabled = ToolRegistry::new();
    for spec in registry.specs() {
        let unbound = !bound && pingdeck::WORKSPACE_SCOPED.contains(&spec.name.as_str());
        if disabled.contains(spec.name.as_str()) || unbound || (explore && !explore_allows(&spec)) {
            continue;
        }
        if let Some(tool) = registry.get(&spec.name) {
            enabled.register(Arc::clone(tool));
        }
    }
    SessionTools {
        registry: enabled,
        native_web_search,
        connecting,
    }
}

/// How long a run's start waits for an MCP server without a cached tool list.
pub(super) const MCP_START_WAIT: std::time::Duration = std::time::Duration::from_secs(3);

/// Registers `tools` of `server`; a tool whose definition drifted from its pin on a trusted
/// server is registered as untrusted (it asks every time until reviewed).
fn register_mcp(
    state: &AppState,
    registry: &mut ToolRegistry,
    server: &AgentMcpServer,
    transport: &crate::agent::mcp::McpTransportConfig,
    tools: Vec<McpTool>,
) {
    let drifted = super::config::drifted_tools(state, server, &tools);
    for tool in tools {
        let changed = drifted.contains(&tool.name);
        // A name another tool already holds (two servers whose names clean to the same prefix)
        // never shadows it: the first registered tool keeps the name.
        registry.register_unique(McpToolAdapter::pinned(
            server,
            transport.clone(),
            tool,
            changed,
        ));
    }
    // A server that offers resources gets one read tool for them.
    if state.agent.mcp.offers_resources(&server.id) {
        registry.register_unique(McpResourceTool::new(server, transport.clone()));
    }
}

/// The built-in tools, whether or not they can run now (the tool picker lists all of them).
pub(super) fn builtin_specs() -> Vec<ToolSpec> {
    let mut registry = ToolRegistry::new();
    fs::register(&mut registry);
    shell::register(&mut registry);
    registry.register(Arc::new(FetchUrlTool::new()));
    registry.register(Arc::new(WebSearchTool::new()));
    // `read_skill` registers only with a skill to read; a placeholder yields its spec.
    skill::register(
        &mut registry,
        vec![SkillInfo {
            name: "placeholder".to_owned(),
            description: String::new(),
            path: String::new(),
            origin: String::new(),
        }],
    );
    todo::register(&mut registry);
    pingdeck::register(&mut registry);
    subagent::register(&mut registry, &[]);
    registry.specs()
}

/// Which built-in tools can run with the current configuration: all of them, `read_skill` only
/// while a skill is enabled (`web_search` always: without a configured backend it falls back to
/// keyless DuckDuckGo).
pub(super) fn builtin_available(
    _state: &AppState,
    _settings: &crate::settings::Settings,
    skills: &[SkillInfo],
) -> std::collections::HashMap<String, bool> {
    builtin_specs()
        .into_iter()
        .map(|spec| {
            let available = spec.name != READ_SKILL || !skills.is_empty();
            (spec.name, available)
        })
        .collect()
}

/// The cached tool lists of `servers`: reading settings never starts a server; a server
/// lists its tools once a run or "Refresh tools" connects it.
pub(super) fn list_mcp_tools(
    state: &AppState,
    servers: Vec<AgentMcpServer>,
) -> Vec<(AgentMcpServer, Vec<McpTool>)> {
    servers
        .into_iter()
        .filter_map(|server| {
            let tools = state.agent.mcp.cached_tools(&server.id)?;
            Some((server, tools))
        })
        .collect()
}

/// What the model reads after calling a tool the run does not have: that it needs a bound
/// PingDeck workspace the chat lacks (`not_bound`), or that the chat switched it off, else that it
/// does not exist (both `tool_unavailable`), plus up to three registered names closest to `name`.
pub(super) fn unknown_tool_text(
    name: &str,
    registry: &ToolRegistry,
    disabled: &[String],
    bound: bool,
) -> String {
    let (code, lead) = if !bound && pingdeck::WORKSPACE_SCOPED.contains(&name) {
        (
            ToolErrorCode::NotBound,
            format!(
                "The tool {name} needs a PingDeck workspace and this chat is not bound to one."
            ),
        )
    } else if disabled.iter().any(|tool| tool == name) {
        (
            ToolErrorCode::ToolUnavailable,
            format!("The tool {name} is switched off in this chat."),
        )
    } else {
        (
            ToolErrorCode::ToolUnavailable,
            format!("There is no tool named {name}."),
        )
    };
    let mut names: Vec<(usize, String)> = registry
        .specs()
        .into_iter()
        .map(|spec| (edit_distance(name, &spec.name), spec.name))
        .collect();
    names.sort();
    let near: Vec<String> = names.into_iter().take(3).map(|(_, name)| name).collect();
    if near.is_empty() {
        return coded_error_text(code, &lead);
    }
    coded_error_text(code, &format!("{lead} Closest tools: {}.", near.join(", ")))
}

/// Levenshtein distance over characters (tool names are short; two rows suffice).
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (row[j + 1] + 1)
                .min(row[j] + 1)
                .min(diagonal + usize::from(ca != *cb));
            diagonal = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

#[cfg(test)]
mod lint {
    use super::*;

    /// Every built-in tool spec is one a model API accepts and the permission engine
    /// classifies consistently; an MCP tool name never takes a built-in's.
    #[test]
    fn builtin_tool_specs_pass_the_contract_lint() {
        let name_ok = regex::Regex::new("^[a-z][a-z0-9_]{0,63}$").unwrap();
        let mut faults = Vec::new();
        let specs = builtin_specs();
        for spec in &specs {
            let name = &spec.name;
            if !name_ok.is_match(name) {
                faults.push(format!("{name}: name"));
            }
            if spec.description.chars().count() > 1024 {
                faults.push(format!("{name}: description over 1,024 characters"));
            }
            let properties = spec.input_schema["properties"].as_object();
            for (arg, schema) in properties.into_iter().flatten() {
                if schema.get("description").and_then(|d| d.as_str()).is_none() {
                    faults.push(format!("{name}.{arg}: no description"));
                }
            }
            for required in spec.input_schema["required"]
                .as_array()
                .into_iter()
                .flatten()
            {
                let known = required
                    .as_str()
                    .is_some_and(|arg| properties.is_some_and(|p| p.contains_key(arg)));
                if !known {
                    faults.push(format!("{name}: required {required} is not a property"));
                }
            }
            let writes = matches!(
                spec.class,
                ToolClass::Edit | ToolClass::Exec | ToolClass::PingDeckWrite
            );
            if (spec.read_only && (spec.destructive || writes))
                || (matches!(spec.class, ToolClass::Read) && !spec.read_only)
            {
                faults.push(format!("{name}: class and read-only/destructive disagree"));
            }
            if name.starts_with("mcp__") {
                faults.push(format!("{name}: takes the MCP prefix"));
            }
        }
        assert!(faults.is_empty(), "{faults:#?}");
    }
}
