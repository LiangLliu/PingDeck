//! MCP server configuration, tool listing and skill management use-cases.
//!
//! MCP credentials never reach the database or a DTO: saving moves every environment/header value
//! into the secret store (`agent::mcp` credentials), listing returns the stored placeholders and
//! connecting resolves them. The secret store is never called while `state.store()` is held. The
//! status, restart, log and subscribe use-cases expose what `agent::mcp::McpManager` knows.

use std::path::Path;
use std::sync::Arc;
use std::time::Instant;

use crate::agent::mcp::{
    AgentMcpServerView, CONNECT_TIMEOUT, McpServerStatus, McpServerStatusDto, McpStreamEvent,
    McpTool, McpTransportConfig, McpTrustState, TEST_ID_PREFIX, config_fingerprint,
    delete_server_secrets, effective_hints, externalize_transport, mcp_name_key, mcp_policy_key,
    mcp_tool_name, prune_server_secrets, resolve_transport, tool_fingerprint,
};
use crate::agent::model::{AgentMcpServer, AgentMcpTestResult, AgentSkill, AgentToolInfo};
use crate::agent::skills::{MANAGED_ORIGIN, SkillInfo, discover, discover_skills, import_skill};
use crate::error::AppError;
use crate::events::{EventSink, Sequenced};
use crate::secrets::SecretStore;
use crate::settings::Settings;

use super::tools::{builtin_available, builtin_specs, list_mcp_tools};
use crate::app::factory_service::new_entity_id;
use crate::app::{AppState, command_error, current_time_ms};

fn validate_transport(transport: &McpTransportConfig) -> Result<(), AppError> {
    match transport {
        McpTransportConfig::Stdio { command, .. } if command.trim().is_empty() => {
            Err(AppError::validation("an MCP stdio server needs a command"))
        }
        McpTransportConfig::Stdio { cwd: Some(cwd), .. }
            if !cwd.trim().is_empty() && !Path::new(cwd.trim()).is_absolute() =>
        {
            Err(AppError::validation(
                "an MCP stdio server's working directory must be an absolute path",
            ))
        }
        McpTransportConfig::Http { url, .. } => match url::Url::parse(url) {
            Ok(parsed) if matches!(parsed.scheme(), "http" | "https") && parsed.has_host() => {
                Ok(())
            }
            _ => Err(AppError::validation(
                "an MCP http server needs an http:// or https:// URL",
            )),
        },
        McpTransportConfig::Stdio { .. } => Ok(()),
    }
}

/// The app's network proxy URL when one is set: stdio MCP servers get the proxy variables only
/// then.
pub(crate) fn app_proxy(state: &AppState) -> Option<String> {
    let proxy = state.settings().network.proxy_url.trim().to_owned();
    (!proxy.is_empty() && !proxy.eq_ignore_ascii_case(pingdeck_base::proxy_url::DIRECT))
        .then_some(proxy)
}

/// Whether the stored configuration of `server` is one the user has not reviewed: it arrived
/// through sync or import, or differs from what was last saved. The baseline is stored, so the
/// answer survives a restart; a server with none yet (saved before this existed) is taken as
/// reviewed and its current configuration becomes the baseline. Such a server is never started.
pub(crate) fn server_needs_review(state: &AppState, server: &AgentMcpServer) -> bool {
    let fingerprint = config_fingerprint(&server.transport);
    let store = state.store();
    match store.mcp_review_baseline(&server.id) {
        Ok(Some(baseline)) => baseline != fingerprint,
        Ok(None) => {
            let _ = store.set_mcp_review_baseline(&server.id, &fingerprint);
            false
        }
        Err(_) => true,
    }
}

/// The tools of `server` whose definition no longer matches its pin, by server-side name. Only a
/// trusted server has pins: a tool of it without one is pinned now (the user saw the tool list
/// when trusting the server); an untrusted server has none drifted. A store failure counts
/// every tool as drifted, so nothing is believed that cannot be checked.
pub(crate) fn drifted_tools(
    state: &AppState,
    server: &AgentMcpServer,
    tools: &[McpTool],
) -> Vec<String> {
    if !server.trusted {
        return Vec::new();
    }
    let store = state.store();
    let Ok(pins) = store.mcp_tool_pins(&server.id) else {
        return tools.iter().map(|tool| tool.name.clone()).collect();
    };
    let mut fresh = Vec::new();
    let mut drifted = Vec::new();
    for tool in tools {
        let fingerprint = tool_fingerprint(tool);
        match pins.get(&tool.name) {
            None => fresh.push((tool.name.clone(), fingerprint)),
            Some(pinned) if *pinned != fingerprint => drifted.push(tool.name.clone()),
            Some(_) => {}
        }
    }
    if !fresh.is_empty() && store.pin_mcp_tools(&server.id, &fresh).is_err() {
        drifted.extend(fresh.into_iter().map(|(name, _)| name));
    }
    drifted
}

/// The trust state of `server` and its drifted tools (from the cached tool list).
fn trust_of(state: &AppState, server: &AgentMcpServer) -> (McpTrustState, Vec<String>) {
    if !server.trusted {
        return (McpTrustState::Untrusted, Vec::new());
    }
    let tools = state.agent.mcp.cached_tools(&server.id).unwrap_or_default();
    let drifted = drifted_tools(state, server, &tools);
    let trust = if drifted.is_empty() {
        McpTrustState::Trusted
    } else {
        McpTrustState::NeedsReview
    };
    (trust, drifted)
}

/// The per-tool policy key of the tool the model calls `function`: `mcp:<server id>:<tool>` for
/// a cached MCP tool, else the function name itself.
pub(crate) fn policy_key_of(state: &AppState, function: &str) -> String {
    if !function.starts_with("mcp__") {
        return function.to_owned();
    }
    let servers = state.store().list_agent_mcp_servers().unwrap_or_default();
    servers
        .iter()
        .find_map(|server| {
            let tools = state.agent.mcp.cached_tools(&server.id)?;
            tools
                .iter()
                .find(|tool| mcp_tool_name(&server.name, &tool.name) == function)
                .map(|tool| mcp_policy_key(&server.id, &tool.name))
        })
        .unwrap_or_else(|| function.to_owned())
}

/// What the UI shows for `server`: switched off, changed since the user last saved it, missing a
/// stored token, or what the manager knows about its connection.
fn server_status(state: &AppState, server: &AgentMcpServer) -> McpServerStatus {
    if !server.enabled {
        return McpServerStatus::Disabled;
    }
    if server_needs_review(state, server) {
        return McpServerStatus::NeedsReview;
    }
    // A placeholder without a stored secret is the only validation error resolving can raise.
    match resolve_transport(state.secrets().as_ref(), &server.transport) {
        Ok(_) => state.agent.mcp.status(&server.id),
        Err(AppError::Validation { .. }) => McpServerStatus::NeedsAuth,
        Err(error) => McpServerStatus::Error {
            message: error.message().to_owned(),
        },
    }
}

/// The configured MCP servers with their live status and cached tool count. Credentials appear
/// only as `{{secret:…}}` placeholders.
pub fn agent_mcp_server_list_impl(state: &AppState) -> Result<Vec<AgentMcpServerView>, AppError> {
    let servers = state
        .store()
        .list_agent_mcp_servers()
        .map_err(command_error)?;
    Ok(servers
        .into_iter()
        .map(|server| view_of(state, server))
        .collect())
}

fn view_of(state: &AppState, server: AgentMcpServer) -> AgentMcpServerView {
    let counts = state.agent.mcp.tool_counts(&server);
    let (trust, drifted) = trust_of(state, &server);
    AgentMcpServerView {
        status: server_status(state, &server),
        tool_count: counts.0,
        read_only_count: counts.1,
        trust_state: Some(trust),
        drifted_tools: (!drifted.is_empty()).then_some(drifted),
        server,
    }
}

/// "Trust" and "Review" on a server row: trusts `server_id` (an untrusted server starts with no
/// pins) and pins the current definition of `tool_names` (all cached tools when empty), so those
/// tools follow the permission rules again. Returns the server's row.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown server; a storage failure.
pub fn agent_mcp_tool_review_impl(
    state: &AppState,
    server_id: &str,
    tool_names: &[String],
) -> Result<AgentMcpServerView, AppError> {
    let mut server = state
        .store()
        .list_agent_mcp_servers()
        .map_err(command_error)?
        .into_iter()
        .find(|server| server.id == server_id)
        .ok_or_else(|| AppError::not_found(format!("MCP server {server_id} not found")))?;
    if !server.trusted {
        server.trusted = true;
        server.updated_at_ms = current_time_ms();
        let store = state.store();
        store
            .clear_mcp_tool_pins(server_id)
            .map_err(command_error)?;
        store
            .save_agent_mcp_server(&server)
            .map_err(command_error)?;
        drop(store);
        state.sync.notify_changed();
    }
    let pins: Vec<(String, String)> = state
        .agent
        .mcp
        .cached_tools(server_id)
        .unwrap_or_default()
        .iter()
        .filter(|tool| tool_names.is_empty() || tool_names.contains(&tool.name))
        .map(|tool| (tool.name.clone(), tool_fingerprint(tool)))
        .collect();
    state
        .store()
        .pin_mcp_tools(server_id, &pins)
        .map_err(command_error)?;
    Ok(view_of(state, server))
}

/// The confirmation a stdio server needs before it starts with a command the user has not
/// approved: a new one, one switched on, or one whose stored configuration changed since the user
/// last saved it (sync or import). `None` when no confirmation is needed.
fn start_confirmation(
    state: &AppState,
    server: &AgentMcpServer,
    previous: Option<&AgentMcpServer>,
) -> Option<String> {
    let McpTransportConfig::Stdio { command, args, .. } = &server.transport else {
        return None;
    };
    let unapproved = match previous {
        None => true,
        Some(previous) => !previous.enabled || server_needs_review(state, previous),
    };
    (server.enabled && unapproved).then(|| {
        format!(
            "confirm the command before {} starts; it runs outside the sandbox: {command} {}",
            server.name,
            args.join(" ")
        )
    })
}

/// Creates or updates an MCP server (a blank id creates): its environment/header values move into
/// the secret store and the stored (and returned) config holds placeholders; its cached connection
/// and tool list are dropped so the next use reconnects with the new configuration. A stdio server
/// about to start a command the user has not approved needs `confirmed`; an untrusted
/// server keeps no tool pins.
///
/// # Errors
/// [`AppError::ConfirmationRequired`] without `confirmed` for such a server (nothing is saved);
/// validation, conflict and storage errors.
pub fn agent_mcp_server_save_impl(
    state: &AppState,
    mut server: AgentMcpServer,
    confirmed: bool,
) -> Result<AgentMcpServer, AppError> {
    server.name = server.name.trim().to_owned();
    if server.name.is_empty() {
        return Err(AppError::validation("an MCP server needs a name"));
    }
    validate_transport(&server.transport)?;
    if server.id.trim().is_empty() {
        server.id = new_entity_id("mcp");
    }
    server.updated_at_ms = current_time_ms();
    let previous = {
        let store = state.store();
        let servers = store.list_agent_mcp_servers().map_err(command_error)?;
        // Compared by the name the model sees (`my.srv` and `my_srv` are one prefix): otherwise
        // one server's tools would shadow the other's, trust flags included.
        let key = mcp_name_key(&server.name);
        if servers
            .iter()
            .any(|other| mcp_name_key(&other.name) == key && other.id != server.id)
        {
            return Err(AppError::conflict(format!(
                "an MCP server named {} already exists (names are compared with every character \
                 other than letters, digits and `-` read as `_`)",
                server.name
            )));
        }
        servers.into_iter().find(|other| other.id == server.id)
    };
    if !confirmed && let Some(message) = start_confirmation(state, &server, previous.as_ref()) {
        return Err(AppError::confirmation_required(message));
    }
    let secrets = state.secrets();
    externalize_transport(secrets.as_ref(), &server.id, &mut server.transport)?;
    state
        .store()
        .save_agent_mcp_server(&server)
        .map_err(command_error)?;
    if let Some(previous) = previous {
        prune_server_secrets(
            secrets.as_ref(),
            &server.id,
            &previous.transport,
            Some(&server.transport),
        )?;
    }
    // Saving is the user's review of exactly this configuration.
    state
        .store()
        .set_mcp_review_baseline(&server.id, &config_fingerprint(&server.transport))
        .map_err(command_error)?;
    if !server.trusted {
        state
            .store()
            .clear_mcp_tool_pins(&server.id)
            .map_err(command_error)?;
    }
    if server.enabled {
        state.agent.mcp.restart(&server.id);
    } else {
        state.agent.mcp.mark_disabled(&server.id);
    }
    state.sync.notify_changed();
    Ok(server)
}

/// Deletes an MCP server, its secrets, its connection, its tool pins, its tools' per-tool
/// policies and its call timeout (`ai.agent.mcp_call_timeout_s`); returns how many policies went
/// with it.
pub fn agent_mcp_server_delete_impl(state: &AppState, server_id: &str) -> Result<usize, AppError> {
    state.agent.mcp.forget(server_id);
    let not_found = || AppError::not_found(format!("MCP server {server_id} not found"));
    let server = state
        .store()
        .list_agent_mcp_servers()
        .map_err(command_error)?
        .into_iter()
        .find(|server| server.id == server_id)
        .ok_or_else(not_found)?;
    let _ = state.store().clear_mcp_tool_pins(server_id);
    if !state
        .store()
        .delete_agent_mcp_server(server_id)
        .map_err(command_error)?
    {
        return Err(not_found());
    }
    let policies = state
        .store()
        .delete_mcp_tool_policies(server_id)
        .map_err(command_error)?;
    delete_server_secrets(state.secrets().as_ref(), server_id, &server.transport)?;
    if state
        .settings()
        .ai
        .agent
        .mcp_call_timeout_s
        .contains_key(server_id)
    {
        // The server is gone either way; a refused patch only leaves an unused entry behind.
        let patch = serde_json::json!({"agent": {"mcp_call_timeout_s": {server_id: null}}});
        let _ =
            crate::app::settings_update_impl(state, crate::settings::SettingsSection::Ai, &patch);
    }
    state.sync.notify_changed();
    Ok(policies)
}

/// The status of every configured server, with its cached tool count.
pub fn agent_mcp_status_list_impl(state: &AppState) -> Result<Vec<McpServerStatusDto>, AppError> {
    Ok(agent_mcp_server_list_impl(state)?
        .into_iter()
        .map(|view| McpServerStatusDto {
            server_id: view.server.id,
            status: view.status,
            tool_count: view.tool_count,
            read_only_count: view.read_only_count,
        })
        .collect())
}

/// Stops a server, reconnects it and returns its status: the way out of an `Error` or
/// `NeedsAuth` after fixing the cause. A disabled server or one that needs review is refused.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown id, [`AppError::Validation`] for a disabled server or one
/// that needs review.
pub async fn agent_mcp_server_restart_impl(
    state: &AppState,
    server_id: &str,
) -> Result<McpServerStatusDto, AppError> {
    let server = state
        .store()
        .list_agent_mcp_servers()
        .map_err(command_error)?
        .into_iter()
        .find(|server| server.id == server_id)
        .ok_or_else(|| AppError::not_found(format!("MCP server {server_id} not found")))?;
    let proxy = app_proxy(state);
    match server_status(state, &server) {
        McpServerStatus::Disabled => {
            return Err(AppError::validation("the MCP server is switched off"));
        }
        McpServerStatus::NeedsReview => {
            return Err(AppError::validation(
                "the MCP server's configuration changed: review and save it first",
            ));
        }
        _ => {}
    }
    state.agent.mcp.restart(server_id);
    let outcome = async {
        let transport =
            resolve_blocking(Arc::clone(state.secrets()), server.transport.clone()).await?;
        state
            .agent
            .mcp
            .ensure_tools(server_id, &transport, proxy.as_deref(), CONNECT_TIMEOUT)
            .await
    }
    .await;
    // A failed reconnect is a status, not an error of this use-case.
    drop(outcome);
    let counts = state.agent.mcp.tool_counts(&server);
    Ok(McpServerStatusDto {
        server_id: server.id.clone(),
        status: server_status(state, &server),
        tool_count: counts.0,
        read_only_count: counts.1,
    })
}

/// The last 200 stderr lines of a stdio server (secrets redacted), oldest first.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown id.
pub fn agent_mcp_server_log_impl(
    state: &AppState,
    server_id: &str,
) -> Result<Vec<String>, AppError> {
    let known = state
        .store()
        .list_agent_mcp_servers()
        .map_err(command_error)?
        .iter()
        .any(|server| server.id == server_id);
    if !known {
        return Err(AppError::not_found(format!(
            "MCP server {server_id} not found"
        )));
    }
    Ok(state.agent.mcp.log(server_id))
}

/// Watches server status changes: replays the remembered ones after `from_seq`, then delivers new
/// ones to `sink`; a `from_seq` whose events were evicted is announced with
/// [`McpStreamEvent::Gap`] first. Returns the subscription id for [`agent_mcp_unsubscribe_impl`].
///
/// # Errors
/// [`AppError::Conflict`] when `from_seq` is `None` and the replay ring no longer starts at 1, or
/// there are too many watchers.
pub fn agent_mcp_subscribe_impl(
    state: &AppState,
    from_seq: Option<u64>,
    sink: Arc<dyn EventSink<Sequenced<McpStreamEvent>>>,
) -> Result<u64, AppError> {
    state.agent.mcp.subscribe(from_seq, sink)
}

/// The position of the MCP status stream: subscribe from it and no event is lost.
#[must_use]
pub fn agent_mcp_last_seq(state: &AppState) -> u64 {
    state.agent.mcp.last_seq()
}

/// Ends a status subscription; returns whether it was still active.
pub fn agent_mcp_unsubscribe_impl(state: &AppState, subscription_id: u64) -> bool {
    state.agent.mcp.unsubscribe(subscription_id)
}

/// Resolves `transport`'s placeholders on a blocking thread (the secret store is the database).
async fn resolve_blocking(
    secrets: Arc<dyn SecretStore>,
    transport: McpTransportConfig,
) -> Result<McpTransportConfig, AppError> {
    tokio::task::spawn_blocking(move || resolve_transport(secrets.as_ref(), &transport))
        .await
        .map_err(|error| AppError::internal(format!("secret lookup failed: {error}")))?
}

/// Connects to `server` (saved or not), lists its tools and reports what happened; a connection
/// failure or a placeholder without a stored secret is a result with `ok: false`, not an error.
/// Placeholders resolve from the secret store; nothing is persisted.
pub async fn agent_mcp_server_test_impl(
    state: &AppState,
    server: AgentMcpServer,
) -> Result<AgentMcpTestResult, AppError> {
    validate_transport(&server.transport)?;
    let proxy = app_proxy(state);
    let started = Instant::now();
    // A test always connects fresh, so a stale cached connection cannot hide a broken server.
    let test_id = format!("{TEST_ID_PREFIX}{}", server.id);
    state.agent.mcp.disconnect(&test_id);
    let outcome = async {
        let transport =
            resolve_blocking(Arc::clone(state.secrets()), server.transport.clone()).await?;
        let connection = state
            .agent
            .mcp
            .connection(&test_id, &transport, proxy.as_deref(), CONNECT_TIMEOUT)
            .await?;
        let tools = connection.list_tools(CONNECT_TIMEOUT).await?;
        let info = connection.server_info();
        Ok::<_, AppError>((format!("{} {}", info.name, info.version), tools))
    }
    .await;
    state.agent.mcp.disconnect(&test_id);
    let elapsed_ms = u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX);
    Ok(match outcome {
        Ok((server, tools)) => AgentMcpTestResult {
            ok: true,
            server: Some(server),
            tool_names: tools.into_iter().map(|tool| tool.name).collect(),
            error: None,
            elapsed_ms,
        },
        Err(error) => AgentMcpTestResult {
            ok: false,
            server: None,
            tool_names: Vec::new(),
            error: Some(error.message().to_owned()),
            elapsed_ms,
        },
    })
}

fn discover_for(
    state: &AppState,
    settings: &Settings,
    workspace_dir: Option<&Path>,
) -> Vec<SkillInfo> {
    // Folder skills are loaded only for a folder the user trusted, while its
    // instructions and skills are the ones trusted (trust digest v2).
    let trusted = workspace_dir
        .and_then(|dir| std::fs::canonicalize(dir).ok())
        .is_some_and(|dir| super::trust::folder_pinned(state, &dir));
    discover(
        settings,
        state.agent.managed_skills_dir.as_deref(),
        workspace_dir,
        trusted,
    )
}

fn with_flags(state: &AppState, skills: Vec<SkillInfo>) -> Result<Vec<AgentSkill>, AppError> {
    let flags = state
        .store()
        .list_agent_skill_flags()
        .map_err(command_error)?;
    Ok(skills
        .into_iter()
        .map(|skill| AgentSkill {
            enabled: flags.get(&skill.name).copied().unwrap_or(true),
            name: skill.name,
            description: skill.description,
            path: skill.path,
            origin: skill.origin,
        })
        .collect())
}

/// The skills the agent can see, with their enabled flag: the configured folders, the managed
/// folder and, with a `workspace_dir`, that workspace's `.claude/skills` and `.agents/skills`.
pub fn agent_skill_list_impl(
    state: &AppState,
    workspace_dir: Option<&str>,
) -> Result<Vec<AgentSkill>, AppError> {
    with_flags(
        state,
        discover_for(state, &state.settings(), workspace_dir.map(Path::new)),
    )
}

/// The enabled skills only (the same folders as [`agent_skill_list_impl`]).
pub(super) fn enabled_skills(
    state: &AppState,
    settings: &Settings,
    workspace_dir: Option<&Path>,
) -> Result<Vec<SkillInfo>, AppError> {
    let flags = state
        .store()
        .list_agent_skill_flags()
        .map_err(command_error)?;
    Ok(discover_for(state, settings, workspace_dir)
        .into_iter()
        .filter(|skill| flags.get(&skill.name).copied().unwrap_or(true))
        .collect())
}

/// Copies a folder containing `SKILL.md` into the managed skill folder.
pub fn agent_skill_import_impl(state: &AppState, source_dir: &str) -> Result<AgentSkill, AppError> {
    let managed = state.agent.managed_skills_dir.as_deref().ok_or_else(|| {
        AppError::unsupported("this PingDeck session has no managed skill folder")
    })?;
    let info = import_skill(Path::new(source_dir), managed)?;
    state
        .store()
        .set_agent_skill_enabled(&info.name, true)
        .map_err(command_error)?;
    state.sync.notify_changed();
    Ok(AgentSkill {
        name: info.name,
        description: info.description,
        path: info.path,
        origin: MANAGED_ORIGIN.to_owned(),
        enabled: true,
    })
}

/// Switches a skill on or off. The flag is keyed by name; `workspace_dir` lets a skill that only
/// the workspace provides be found.
pub fn agent_skill_set_enabled_impl(
    state: &AppState,
    name: &str,
    enabled: bool,
    workspace_dir: Option<&str>,
) -> Result<(), AppError> {
    if !discover_for(state, &state.settings(), workspace_dir.map(Path::new))
        .iter()
        .any(|skill| skill.name == name)
    {
        return Err(AppError::not_found(format!("skill {name} not found")));
    }
    state
        .store()
        .set_agent_skill_enabled(name, enabled)
        .map_err(command_error)?;
    state.sync.notify_changed();
    Ok(())
}

/// Deletes an imported skill's folder; skills from configured folders are never touched.
pub fn agent_skill_remove_impl(state: &AppState, name: &str) -> Result<(), AppError> {
    let managed = state
        .agent
        .managed_skills_dir
        .as_deref()
        .ok_or_else(|| AppError::unsupported("no managed skill folder"))?;
    let skill = discover_skills(&[managed.to_path_buf()], &[MANAGED_ORIGIN.to_owned()])
        .into_iter()
        .find(|skill| skill.name == name)
        .ok_or_else(|| AppError::not_found(format!("no imported skill named {name}")))?;
    std::fs::remove_dir_all(&skill.path).map_err(|error| AppError::io(error.to_string()))?;
    state.sync.notify_changed();
    Ok(())
}

/// Every tool the UI can offer: built-ins (with whether they work now) and the tools of the
/// enabled MCP servers that connect. With `session_id` the answer is that chat's: `read_skill`
/// is available only while a skill is enabled for it (the Folder's own skills included, the
/// chat's skill switches applied).
///
/// # Errors
/// [`AppError::NotFound`] for an unknown chat; a storage failure.
pub async fn agent_tool_list_impl(
    state: &AppState,
    session_id: Option<&str>,
) -> Result<Vec<AgentToolInfo>, AppError> {
    let settings = state.settings();
    let session = session_id
        .map(|id| super::sessions::load_session(state, id))
        .transpose()?;
    let workspace = session
        .as_ref()
        .and_then(|session| session.workspace_dir.as_deref().map(Path::new));
    let mut skills = enabled_skills(state, &settings, workspace)?;
    if let Some(session) = &session {
        skills.retain(|skill| !session.disabled_skills.contains(&skill.name));
    }
    let available = builtin_available(state, &settings, &skills);
    let mut tools: Vec<AgentToolInfo> = builtin_specs()
        .into_iter()
        .map(|spec| AgentToolInfo {
            policy_key: None,
            available: available[&spec.name],
            needs_workspace: crate::agent_tools::pingdeck::WORKSPACE_SCOPED
                .contains(&spec.name.as_str()),
            name: spec.name,
            description: spec.description,
            read_only: spec.read_only,
            destructive: spec.destructive,
            source: "builtin".to_owned(),
        })
        .collect();
    let servers: Vec<AgentMcpServer> = state
        .store()
        .list_agent_mcp_servers()
        .map_err(command_error)?
        .into_iter()
        .filter(|server| server.enabled)
        .collect();
    for (server, listed) in list_mcp_tools(state, servers) {
        let drifted = drifted_tools(state, &server, &listed);
        for tool in listed {
            let (read_only, destructive) = if drifted.contains(&tool.name) {
                (false, true)
            } else {
                effective_hints(&server, &tool)
            };
            tools.push(AgentToolInfo {
                policy_key: Some(mcp_policy_key(&server.id, &tool.name)),
                name: mcp_tool_name(&server.name, &tool.name),
                description: tool.description,
                read_only,
                destructive,
                source: server.name.clone(),
                available: true,
                needs_workspace: false,
            });
        }
    }
    Ok(tools)
}
