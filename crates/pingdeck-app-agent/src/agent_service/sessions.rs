//! Session use-cases: create, read (paged along the active path), list, update, tombstone delete
//! and restore, search, export, and the message-tree edits (set head, delete message, branch,
//! prepare edit, prepare retry).

use std::collections::HashSet;
use std::fmt::Write as _;
use std::path::Path;

use serde::{Deserialize, Serialize};

use crate::agent::model::{
    AgentExportFormat, AgentExportPreview, AgentMessage, AgentMessageMeta, AgentPathWindow,
    AgentRole, AgentRunSummary, AgentSession, AgentSessionDetail, AgentSessionFilter,
    AgentSessionInput, AgentSessionPage, AgentSessionPatch, AgentThreadHit, AgentTodoItem,
    AgentTodoStatus, LiveRunSnapshot, MessageDeletion, ReasoningEffort, SessionSearchHit,
};
use crate::error::AppError;
use crate::llm::hub::{HubCitation, HubDocument, HubDocumentSource, HubImage, HubPart, HubUsage};
use crate::platform::normalize_display_path;
use crate::settings::{ReasoningEffortOrAuto, ToolApproval};
use crate::workspace::storage::PingDeckBinding;
use crate::workspace::{EnvironmentId, WorkspaceId, WorkspaceStore};

use crate::app::factory_service::new_entity_id;
use crate::app::{AppState, command_error, current_time_ms, ensure_workspace_exists};

/// Title of a session before its first message names it.
pub(super) const DEFAULT_TITLE: &str = "New chat";

/// Longest title, in characters.
const MAX_TITLE_CHARS: usize = 120;

/// Turns per page when the caller names no limit.
const DEFAULT_PAGE_TURNS: u32 = 50;
/// Most turns per page.
const MAX_PAGE_TURNS: u32 = 500;
/// Most runs a detail or an export lists.
const MAX_RUNS: u32 = 500;
/// Hits a search returns when the caller names no limit: the command palette shows 20.
const DEFAULT_SEARCH_HITS: u32 = 20;

/// Most tasks in a session's task list.
const MAX_TODOS: usize = 100;
/// Longest task text, in characters.
const MAX_TODO_CHARS: usize = 500;
/// Most tool or skill names a session can switch off.
const MAX_SWITCHES: usize = 256;
/// Most attachments one message carries.
const MAX_ATTACHMENTS: usize = 10;

fn clean_title(title: &str) -> String {
    let title: String = title.split_whitespace().collect::<Vec<_>>().join(" ");
    let title: String = title.chars().take(MAX_TITLE_CHARS).collect();
    if title.is_empty() {
        DEFAULT_TITLE.to_owned()
    } else {
        title
    }
}

fn validate_names(field: &str, names: &[String]) -> Result<(), AppError> {
    if names.len() > MAX_SWITCHES || names.iter().any(|name| name.trim().is_empty()) {
        return Err(AppError::validation(format!(
            "{field} must hold at most {MAX_SWITCHES} non-blank names"
        )));
    }
    Ok(())
}

fn validate_todos(todos: &[AgentTodoItem]) -> Result<(), AppError> {
    let mut ids = HashSet::new();
    if todos.len() > MAX_TODOS
        || todos.iter().any(|todo| {
            todo.id.trim().is_empty()
                || todo.content.trim().is_empty()
                || todo.content.chars().count() > MAX_TODO_CHARS
                || !ids.insert(todo.id.as_str())
        })
    {
        return Err(AppError::validation(format!(
            "todos must hold at most {MAX_TODOS} tasks with unique ids and \
             1 to {MAX_TODO_CHARS} characters of text"
        )));
    }
    crate::agent::model::validate_task_graph_dependencies(todos).map_err(AppError::validation)?;
    Ok(())
}

/// What a user message may hold: text, and attachments as references (a message never
/// stores base64), at most [`MAX_ATTACHMENTS`] of them, and something to say.
pub(super) fn validate_user_parts(parts: &[HubPart]) -> Result<(), AppError> {
    let mut attachments = 0;
    let mut has_text = false;
    for part in parts {
        match part {
            HubPart::Text { text, .. } => has_text |= !text.trim().is_empty(),
            HubPart::Image {
                image: HubImage::AttachmentRef { .. },
            }
            | HubPart::Document {
                document:
                    HubDocument {
                        source: HubDocumentSource::AttachmentRef { .. },
                        ..
                    },
            } => attachments += 1,
            _ => {
                return Err(AppError::validation(
                    "a message holds text and attachment references only",
                ));
            }
        }
    }
    if attachments > MAX_ATTACHMENTS {
        return Err(AppError::validation(format!(
            "a message holds at most {MAX_ATTACHMENTS} attachments"
        )));
    }
    if !has_text && attachments == 0 {
        return Err(AppError::validation("the message cannot be blank"));
    }
    Ok(())
}

/// The canonical form of a session's folder: absolute, existing and a directory. It is what is
/// stored, so every later comparison with the folder a tool resolves is a string comparison.
pub(super) fn canonical_workspace_dir(dir: &str) -> Result<String, AppError> {
    let path = Path::new(dir);
    if !path.is_absolute() {
        return Err(AppError::validation(format!(
            "workspace_dir must be an absolute path, got {dir}"
        )));
    }
    let canonical = std::fs::canonicalize(path).map_err(|error| {
        AppError::validation(format!("workspace_dir {dir} cannot be used: {error}"))
    })?;
    if !canonical.is_dir() {
        return Err(AppError::validation(format!(
            "workspace_dir {dir} is not a folder"
        )));
    }
    let home = std::env::var_os("HOME").map(std::path::PathBuf::from);
    crate::agent::tools::fs::check_workspace(&canonical, home.as_deref(), None)?;
    Ok(normalize_display_path(&canonical))
}

/// `None` for an absent or blank folder, else its canonical form.
fn optional_workspace_dir(dir: Option<&str>) -> Result<Option<String>, AppError> {
    dir.filter(|dir| !dir.trim().is_empty())
        .map(canonical_workspace_dir)
        .transpose()
}

/// The chat's own instructions: blank is `None` (the core's prompt alone).
fn clean_instructions(text: Option<String>) -> Option<String> {
    text.filter(|text| !text.trim().is_empty())
}

fn effort_from_setting(effort: ReasoningEffortOrAuto) -> Option<ReasoningEffort> {
    match effort {
        ReasoningEffortOrAuto::Auto => None,
        ReasoningEffortOrAuto::Minimal => Some(ReasoningEffort::Minimal),
        ReasoningEffortOrAuto::Low => Some(ReasoningEffort::Low),
        ReasoningEffortOrAuto::Medium => Some(ReasoningEffort::Medium),
        ReasoningEffortOrAuto::High => Some(ReasoningEffort::High),
        ReasoningEffortOrAuto::Max => Some(ReasoningEffort::Max),
    }
}

/// The error of a chat that needs a model when none is set up.
pub(super) fn no_model() -> AppError {
    AppError::validation("Choose a model for this chat first: no model is set up yet")
        .coded(crate::error::ErrorCode::NoModel, None)
}

/// The model of the newest chat that has one.
fn newest_target(state: &AppState) -> Result<Option<crate::agent::model::AgentTarget>, AppError> {
    let page = state
        .store()
        .list_agent_sessions(&Default::default(), Some(20), None)
        .map_err(command_error)?;
    Ok(page.sessions.into_iter().find_map(|session| session.target))
}

/// Whether `target` names a provider that a sync import brought in and the user has not reviewed
/// (C-S1): such a provider is not taken as the default target. Unreadable routing facts count as
/// reviewed; the run itself reports a broken target.
fn awaits_review(state: &AppState, target: &crate::agent::model::AgentTarget) -> bool {
    use crate::agent::model::AgentTarget;
    let Ok((snapshot, _)) = crate::app::settings_service::load_routing_facts(state) else {
        return false;
    };
    match target {
        AgentTarget::Named { name } => snapshot.awaiting_review(name).is_some(),
        AgentTarget::Pinned { provider_id, .. } => snapshot
            .providers
            .get(provider_id)
            .is_some_and(|provider| provider.review_pending_since_ms.is_some()),
    }
}

/// Creates a session; unset fields come from `ai.agent` (target, folder, reasoning effort,
/// permission mode). An explicit folder must exist; the settings' default folder is used only
/// while it still does. A PingDeck workspace named in `input` must exist: the chat acts there
/// from now on, whichever workspace is open later.
pub fn agent_session_create_impl(
    state: &AppState,
    input: AgentSessionInput,
) -> Result<AgentSession, AppError> {
    let settings = state.settings();
    let defaults = &settings.ai.agent;
    let workspace_dir = match input.workspace_dir.as_deref() {
        Some(dir) => optional_workspace_dir(Some(dir))?,
        None => optional_workspace_dir(defaults.default_workspace.as_deref())
            .ok()
            .flatten(),
    };
    let disabled_tools = input.disabled_tools.unwrap_or_default();
    validate_names("disabled_tools", &disabled_tools)?;
    let disabled_skills = input.disabled_skills.unwrap_or_default();
    validate_names("disabled_skills", &disabled_skills)?;
    if let Some(workspace_id) = &input.pingdeck_workspace_id {
        ensure_workspace_exists(&state.store(), workspace_id)?;
    }
    let target = match input.target.or_else(|| {
        defaults
            .default_target
            .clone()
            .filter(|target| !awaits_review(state, target))
    }) {
        Some(target) => Some(target),
        None => newest_target(state)?,
    };
    if target.is_none() && input.require_model == Some(true) {
        return Err(no_model());
    }
    let now = current_time_ms();
    let session = AgentSession {
        id: new_entity_id("as"),
        title: clean_title(input.title.as_deref().unwrap_or_default()),
        target,
        workspace_dir,
        // Full access is never a default: a stored `auto_all` reads as Auto-approve reads.
        permission_mode: input
            .permission_mode
            .unwrap_or(match defaults.default_permission_mode {
                ToolApproval::AutoAll => ToolApproval::AutoReadOnly,
                mode => mode,
            }),
        reasoning_effort: input
            .reasoning_effort
            .or_else(|| effort_from_setting(defaults.default_effort)),
        disabled_tools,
        disabled_skills,
        web_enabled: input.web_enabled.unwrap_or(false),
        title_locked: input.title.as_deref().is_some_and(|t| !t.trim().is_empty()),
        head_message_id: None,
        pinned_at_ms: None,
        archived_at_ms: None,
        deleted_at_ms: None,
        forked_from: None,
        forked_from_message_id: None,
        parent_session_id: None,
        parent_run_id: None,
        todos: Vec::new(),
        total_usage: HubUsage::default(),
        total_cost_micro_usd: 0,
        last_context_tokens: 0,
        pingdeck_workspace_id: input.pingdeck_workspace_id,
        pingdeck_environment_id: input.pingdeck_environment_id,
        pingdeck_workspace_name: None,
        notice: None,
        instructions: clean_instructions(input.instructions),
        created_at_ms: now,
        updated_at_ms: now,
    };
    state
        .store()
        .save_agent_session(&session)
        .map_err(command_error)?;
    // Read back: the store fills the workspace name.
    live_session(&state.store(), &session.id)
}

/// A live session; a tombstoned one is as good as gone for every use-case but restore.
pub(super) fn load_session(state: &AppState, id: &str) -> Result<AgentSession, AppError> {
    live_session(&state.store(), id)
}

fn live_session(store: &WorkspaceStore, id: &str) -> Result<AgentSession, AppError> {
    store
        .get_agent_session(id)
        .map_err(command_error)?
        .filter(|session| session.deleted_at_ms.is_none())
        .ok_or_else(|| AppError::not_found(format!("chat session {id} not found")))
}

/// Links each message of `messages` to its attachments.
fn fill_attachments(
    state: &AppState,
    store: &WorkspaceStore,
    messages: &mut [AgentMessage],
) -> Result<(), AppError> {
    let ids: Vec<String> = messages.iter().map(|message| message.id.clone()).collect();
    let mut by_message = store
        .list_message_attachments(&ids)
        .map_err(command_error)?;
    // The blob of a cleared attachment is gone while its row stays.
    let blobs = super::attachments::store_for(state).ok();
    for message in messages {
        if let Some(mut attachments) = by_message.remove(&message.id) {
            for attachment in &mut attachments {
                attachment.removed = blobs
                    .as_ref()
                    .is_some_and(|store| !store.exists(&attachment.sha256));
            }
            message.attachments = attachments;
        }
    }
    Ok(())
}

/// How much of the active path one call returns: the newest turns, the turns before a
/// position, the turns after one, or the page around a given message.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentPageRequest {
    /// Turns before the message with this `seq` (scrolling up).
    pub before_seq: Option<i64>,
    /// Turns after the message with this `seq` (scrolling down from a page opened around a hit).
    pub after_seq: Option<i64>,
    /// The page around the turn that holds this message (opening a search hit); when the message
    /// is on another branch the chat switches to that branch first, as clicking the branch does.
    pub around_message_id: Option<String>,
    /// Turns per page; default 50, at most 500.
    pub limit: Option<u32>,
}

fn detail(
    state: &AppState,
    store: &WorkspaceStore,
    session: AgentSession,
    window: AgentPathWindow,
    live: Option<LiveRunSnapshot>,
) -> Result<AgentSessionDetail, AppError> {
    let mut messages = window.messages;
    fill_attachments(state, store, &mut messages)?;
    let on_page: HashSet<&str> = messages.iter().map(|message| message.id.as_str()).collect();
    let tool_calls = store
        .list_tool_calls(&session.id)
        .map_err(command_error)?
        .into_iter()
        .filter(|call| {
            call.message_id
                .as_deref()
                .is_some_and(|id| on_page.contains(id))
        })
        .collect();
    let runs = store
        .list_agent_runs(Some(&session.id), true, MAX_RUNS)
        .map_err(command_error)?;
    let queue_paused = state.agent.runs.queue_paused(&session.id).then_some(true);
    let plan_mode = state.agent.plan_active(&session.id).then_some(true);
    let cost_by_currency = store
        .agent_session_cost_by_currency(&session.id)
        .map_err(command_error)?;
    Ok(AgentSessionDetail {
        session,
        messages,
        has_more: window.has_before,
        has_more_after: window.has_after,
        tool_calls,
        runs,
        live,
        queue_paused,
        cost_by_currency: (!cost_by_currency.is_empty()).then_some(cost_by_currency),
        plan_mode,
    })
}

/// The newest page of the active path.
fn newest_page(
    state: &AppState,
    store: &WorkspaceStore,
    session: AgentSession,
) -> Result<AgentSessionDetail, AppError> {
    let (messages, has_before) = store
        .list_active_path_page(&session.id, None, DEFAULT_PAGE_TURNS)
        .map_err(command_error)?;
    detail(
        state,
        store,
        session,
        AgentPathWindow {
            messages,
            has_before,
            has_after: false,
        },
        // Callers refuse a chat that is answering.
        None,
    )
}

/// The live view of the chat's running top-level run (a sub-agent has a chat of its own).
fn live_run(state: &AppState, session_id: &str) -> Option<LiveRunSnapshot> {
    let run = state.agent.runs.active_for_session(session_id)?;
    run.parent_run_id.is_none().then(|| run.live_snapshot())
}

/// A session with one page of its active path, positional: by default the newest
/// `limit` turns (default 50, at most 500), oldest first, each message with its attachments and
/// branch siblings; `before_seq` pages back, `after_seq` forward, and `around_message_id` opens
/// the page around the turn of a search hit in one call. `has_more` tells whether earlier turns
/// exist and `has_more_after` whether later ones do. `runs` lists the session's active runs.
pub fn agent_session_get_impl(
    state: &AppState,
    session_id: &str,
    request: AgentPageRequest,
) -> Result<AgentSessionDetail, AppError> {
    // Before any store read: a message stored after this point is announced by a later event.
    let live = live_run(state, session_id);
    let store = state.store();
    let mut session = live_session(&store, session_id)?;
    let limit = request
        .limit
        .unwrap_or(DEFAULT_PAGE_TURNS)
        .clamp(1, MAX_PAGE_TURNS);
    if let Some(message_id) = &request.around_message_id {
        let mut window = store
            .list_active_path_around(session_id, message_id, limit)
            .map_err(command_error)?;
        if window.is_none() {
            // A hit on another branch: switch to it, like the branch switcher does.
            ensure_idle(state, &store, session_id)?;
            store
                .set_head(session_id, message_id)
                .map_err(command_error)?
                .ok_or_else(|| AppError::not_found(format!("message {message_id} not found")))?;
            session = live_session(&store, session_id)?;
            window = store
                .list_active_path_around(session_id, message_id, limit)
                .map_err(command_error)?;
        }
        let window =
            window.ok_or_else(|| AppError::not_found(format!("message {message_id} not found")))?;
        return detail(state, &store, session, window, live);
    }
    if let Some(after_seq) = request.after_seq {
        let window = store
            .list_active_path_after(session_id, after_seq, limit)
            .map_err(command_error)?;
        return detail(state, &store, session, window, live);
    }
    let (messages, has_before) = store
        .list_active_path_page(session_id, request.before_seq, limit)
        .map_err(command_error)?;
    detail(
        state,
        &store,
        session,
        AgentPathWindow {
            messages,
            has_before,
            has_after: false,
        },
        live,
    )
}

/// One page of session summaries, pinned first and then newest first; see
/// [`AgentSessionFilter`] for what narrows the list (none: every top-level session except
/// archived and deleted ones). Pass a page's `next_cursor` as `cursor` for the next.
pub fn agent_session_list_impl(
    state: &AppState,
    filter: Option<AgentSessionFilter>,
    limit: Option<u32>,
    cursor: Option<String>,
) -> Result<AgentSessionPage, AppError> {
    state
        .store()
        .list_agent_sessions(&filter.unwrap_or_default(), limit, cursor.as_deref())
        .map_err(command_error)
}

/// Applies `patch` to a session: title (which locks automatic titling unless the patch says
/// otherwise), model, folder (an empty string unbinds), permission mode, reasoning effort, tool
/// and skill switches, web, own instructions (an empty string clears them), pin, archive and task
/// list. Everything is validated before anything is stored.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown chat, [`AppError::Validation`] for an invalid field,
/// [`AppError::Conflict`] for a different folder while the chat has an active run.
pub fn agent_session_update_impl(
    state: &AppState,
    session_id: &str,
    patch: AgentSessionPatch,
) -> Result<AgentSession, AppError> {
    let mut session = load_session(state, session_id)?;
    if let Some(todos) = &patch.todos {
        validate_todos(todos)?;
    }
    if let Some(names) = &patch.disabled_tools {
        validate_names("disabled_tools", names)?;
    }
    if let Some(names) = &patch.disabled_skills {
        validate_names("disabled_skills", names)?;
    }
    let workspace_dir = patch
        .workspace_dir
        .as_deref()
        .map(|dir| optional_workspace_dir(Some(dir)))
        .transpose()?;
    // A run resolves its paths against the folder it started with.
    if workspace_dir
        .as_ref()
        .is_some_and(|dir| *dir != session.workspace_dir)
        && state.agent.runs.active_for_session(session_id).is_some()
    {
        return Err(AppError::conflict(
            "Stop this chat's run before changing its folder",
        ));
    }

    let now = current_time_ms();
    if let Some(title) = patch.title {
        session.title = clean_title(&title);
        session.title_locked = true;
    }
    if let Some(locked) = patch.title_locked {
        session.title_locked = locked;
    }
    if let Some(target) = patch.target {
        session.target = Some(target);
    }
    if let Some(workspace_dir) = workspace_dir {
        session.workspace_dir = workspace_dir;
    }
    if let Some(mode) = patch.permission_mode {
        session.permission_mode = mode;
    }
    if let Some(effort) = patch.reasoning_effort {
        session.reasoning_effort = effort;
    }
    if let Some(names) = patch.disabled_tools {
        session.disabled_tools = names;
    }
    if let Some(names) = patch.disabled_skills {
        session.disabled_skills = names;
    }
    if let Some(web) = patch.web_enabled {
        session.web_enabled = web;
    }
    if let Some(instructions) = patch.instructions {
        session.instructions = clean_instructions(Some(instructions));
    }
    if let Some(pinned) = patch.pinned {
        session.pinned_at_ms = pinned.then(|| session.pinned_at_ms.unwrap_or(now));
    }
    if let Some(archived) = patch.archived {
        session.archived_at_ms = archived.then(|| session.archived_at_ms.unwrap_or(now));
    }
    session.updated_at_ms = now;
    let store = state.store();
    store.save_agent_session(&session).map_err(command_error)?;
    if let Some(todos) = patch.todos {
        store
            .save_session_todos(session_id, &todos)
            .map_err(command_error)?;
        session.todos = todos;
    }
    Ok(session)
}

/// Deletes a session: a tombstone (`deleted_at_ms`) that hides it, its messages and its
/// sub-agent sessions from every use-case until [`agent_session_restore_impl`] or the purge.
///
/// The chat's active run is cancelled first (its sub-agents with it) and its background shell
/// processes are killed; the rows themselves go at the startup purge, together with what lives
/// outside the database.
pub fn agent_session_delete_impl(state: &AppState, session_id: &str) -> Result<(), AppError> {
    if state
        .agent
        .cancel_and_soft_delete(&state.store(), session_id, current_time_ms())
        .map_err(command_error)?
    {
        Ok(())
    } else {
        Err(AppError::not_found(format!(
            "chat session {session_id} not found"
        )))
    }
}

/// Undoes [`agent_session_delete_impl`]. `NotFound` when the session was never deleted, is
/// already purged or does not exist.
pub fn agent_session_restore_impl(
    state: &AppState,
    session_id: &str,
) -> Result<AgentSession, AppError> {
    if !state
        .store()
        .restore_session(session_id)
        .map_err(command_error)?
    {
        return Err(AppError::not_found(format!(
            "no deleted chat session {session_id} to restore"
        )));
    }
    load_session(state, session_id)
}

/// Finds `query` in the messages of every live chat (archived ones too), at most 20 hits, one per
/// chat, newest first; see [`WorkspaceStore::search_agent_messages`]. A chat whose only matches
/// are inside tool results has `in_tool_output` set on its hit.
pub fn agent_session_search_impl(
    state: &AppState,
    query: &str,
    limit: Option<u32>,
) -> Result<Vec<SessionSearchHit>, AppError> {
    state
        .store()
        .search_agent_messages(query, limit.unwrap_or(DEFAULT_SEARCH_HITS))
        .map_err(command_error)
}

/// Finds `query` in the active path of one chat, in path order, at most 500 hits (the in-chat
/// find): each hit carries its message, turn index and whether it is inside a tool
/// result, so an unloaded turn opens with `agent_session_get` around that message.
pub fn agent_session_find_impl(
    state: &AppState,
    session_id: &str,
    query: &str,
) -> Result<Vec<AgentThreadHit>, AppError> {
    let store = state.store();
    live_session(&store, session_id)?;
    store
        .find_in_session(session_id, query)
        .map_err(command_error)
}

/// Dismisses the one-time inline note of a chat; returns whether it had one.
pub fn agent_session_dismiss_notice_impl(
    state: &AppState,
    session_id: &str,
) -> Result<bool, AppError> {
    let store = state.store();
    live_session(&store, session_id)?;
    store
        .clear_session_notice(session_id)
        .map_err(command_error)
}

/// Binds a chat to a PingDeck workspace and environment: its PingDeck tools act there from now on,
/// whichever workspace the user opens later, after a restart too. An idle chat
/// may move to another workspace or environment; a running chat is never redirected. Binding the
/// same pair again is a no-op.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown chat or workspace, [`AppError::Conflict`] while the chat
/// is answering.
pub fn agent_session_bind_pingdeck_impl(
    state: &AppState,
    session_id: &str,
    workspace_id: &str,
    environment_id: Option<String>,
) -> Result<AgentSession, AppError> {
    let store = state.store();
    let session = live_session(&store, session_id)?;
    ensure_workspace_exists(&store, workspace_id)?;
    if let Some(environment) = &environment_id {
        let found = store
            .get_environment(
                &WorkspaceId::new(workspace_id).map_err(command_error)?,
                &EnvironmentId::new(environment).map_err(command_error)?,
            )
            .map_err(command_error)?;
        if found.is_none() {
            return Err(AppError::not_found(format!(
                "environment {environment} not found"
            )));
        }
    }
    if session.pingdeck_workspace_id.as_deref() == Some(workspace_id)
        && session.pingdeck_environment_id == environment_id
    {
        return Ok(session);
    }
    ensure_idle(state, &store, session_id)?;
    store
        .set_session_pingdeck_binding(
            session_id,
            Some(&PingDeckBinding {
                workspace_id: workspace_id.to_owned(),
                environment_id,
            }),
        )
        .map_err(command_error)?;
    live_session(&store, session_id)
}

/// Refuses while the chat answers. A run registers in memory before its row exists, so both
/// views are consulted: the registry first (it covers the whole run), then the store.
fn ensure_idle(state: &AppState, store: &WorkspaceStore, session_id: &str) -> Result<(), AppError> {
    if state.agent.runs.active_for_session(session_id).is_some()
        || store
            .has_active_agent_run(session_id)
            .map_err(command_error)?
    {
        Err(AppError::conflict("this chat is already answering"))
    } else {
        Ok(())
    }
}

/// Switches the active branch to the one containing `message_id` (the head becomes the last
/// message of that branch) and returns the session's newest page. Refused while a run is
/// active; `NotFound` for a message of another chat.
pub fn agent_session_set_head_impl(
    state: &AppState,
    session_id: &str,
    message_id: &str,
) -> Result<AgentSessionDetail, AppError> {
    let store = state.store();
    live_session(&store, session_id)?;
    ensure_idle(state, &store, session_id)?;
    store
        .set_head(session_id, message_id)
        .map_err(command_error)?
        .ok_or_else(|| AppError::not_found(format!("message {message_id} not found")))?;
    let session = live_session(&store, session_id)?;
    newest_page(state, &store, session)
}

/// What deleting `message_id` would remove, for the confirmation text.
pub fn agent_session_delete_message_preview_impl(
    state: &AppState,
    session_id: &str,
    message_id: &str,
) -> Result<MessageDeletion, AppError> {
    let store = state.store();
    live_session(&store, session_id)?;
    store
        .preview_message_deletion(session_id, message_id)
        .map_err(command_error)?
        .ok_or_else(|| AppError::not_found(format!("message {message_id} not found")))
}

/// Deletes a message and everything below it (see [`WorkspaceStore::delete_message`]) and
/// returns the session's newest page. Refused while a run is active.
pub fn agent_session_delete_message_impl(
    state: &AppState,
    session_id: &str,
    message_id: &str,
) -> Result<AgentSessionDetail, AppError> {
    let store = state.store();
    live_session(&store, session_id)?;
    ensure_idle(state, &store, session_id)?;
    store
        .delete_message(session_id, message_id)
        .map_err(command_error)?
        .ok_or_else(|| AppError::not_found(format!("message {message_id} not found")))?;
    let session = live_session(&store, session_id)?;
    newest_page(state, &store, session)
}

/// Branch into a new chat: copies the active path from the root through `message_id` into a
/// new session titled `title`, or the source's title (without the " (branch)" suffixes older
/// branches were given: the list marks a branch by `forked_from`, not by its title). The copy keeps the model, folder, tool,
/// skill and web switches, reasoning effort, plan and PingDeck workspace and records where it
/// came from; it starts unpinned and unarchived with no usage, runs or queue, and in
/// `AutoReadOnly` when the original ran with full access. A steer or a message the
/// app wrote is not a branch point.
pub fn agent_session_branch_impl(
    state: &AppState,
    session_id: &str,
    message_id: &str,
    title: Option<String>,
) -> Result<AgentSession, AppError> {
    let store = state.store();
    let source = live_session(&store, session_id)?;
    if let Some(target) = store
        .get_agent_message(session_id, message_id)
        .map_err(command_error)?
        && target.role == AgentRole::User
        && !target.is_turn_boundary()
    {
        return Err(AppError::validation(
            "a steer or a system message is not a turn: branch from the message that opened it",
        ));
    }
    let now = current_time_ms();
    let branch = AgentSession {
        id: new_entity_id("as"),
        title: clean_title(&title.unwrap_or_else(|| branch_title(&source.title).to_owned())),
        title_locked: true,
        permission_mode: match source.permission_mode {
            ToolApproval::AutoAll => ToolApproval::AutoReadOnly,
            other => other,
        },
        pinned_at_ms: None,
        archived_at_ms: None,
        created_at_ms: now,
        updated_at_ms: now,
        ..source
    };
    store
        .branch_session(session_id, message_id, &branch)
        .map_err(command_error)?
        .ok_or_else(|| AppError::not_found(format!("message {message_id} not found")))
}

/// The default title of a branch of a chat titled `source`: the same title, with the
/// " (branch)" suffixes that branches made before this rule carry taken off.
pub(super) fn branch_title(source: &str) -> &str {
    let mut title = source.trim_end();
    while let Some(rest) = title.strip_suffix(" (branch)") {
        title = rest.trim_end();
    }
    if title.is_empty() { source } else { title }
}

/// Edit: stores `parts` as a new user message beside `message_id` (same parent, next branch
/// index, skills carried over), makes it the head and returns it; the caller then starts the
/// run that answers it. Refused while a run is active.
pub fn agent_message_prepare_edit_impl(
    state: &AppState,
    session_id: &str,
    message_id: &str,
    parts: Vec<HubPart>,
) -> Result<AgentMessage, AppError> {
    validate_user_parts(&parts)?;
    let store = state.store();
    live_session(&store, session_id)?;
    ensure_idle(state, &store, session_id)?;
    let old = store
        .get_agent_message(session_id, message_id)
        .map_err(command_error)?
        .ok_or_else(|| AppError::not_found(format!("message {message_id} not found")))?;
    if !old.is_turn_boundary() {
        return Err(AppError::validation(
            "only a message you wrote that opens a turn can be edited",
        ));
    }
    let meta = AgentMessageMeta {
        skills: old.meta.skills,
        ..AgentMessageMeta::default()
    };
    let mut message = store
        .create_sibling_user_message(session_id, message_id, parts, meta)
        .map_err(command_error)?
        .ok_or_else(|| AppError::not_found(format!("message {message_id} not found")))?;
    fill_attachments(state, &store, std::slice::from_mut(&mut message))?;
    Ok(message)
}

/// Retry: makes the user message that opened the turn of `message_id` the head and returns it;
/// the caller then starts the run, whose first assistant message becomes a sibling of the
/// retried answer. Refused while a run is active.
pub fn agent_message_prepare_retry_impl(
    state: &AppState,
    session_id: &str,
    message_id: &str,
) -> Result<AgentMessage, AppError> {
    let store = state.store();
    live_session(&store, session_id)?;
    ensure_idle(state, &store, session_id)?;
    let mut message = store
        .prepare_retry(session_id, message_id)
        .map_err(command_error)?
        .ok_or_else(|| AppError::not_found(format!("message {message_id} not found")))?;
    fill_attachments(state, &store, std::slice::from_mut(&mut message))?;
    Ok(message)
}

fn push_citations(out: &mut String, citations: &[HubCitation]) {
    if citations.is_empty() {
        return;
    }
    out.push_str("Sources:\n\n");
    for citation in citations {
        let _ = writeln!(out, "- [{}]({})", citation.title, citation.url);
    }
    out.push('\n');
}

fn push_todos(out: &mut String, todos: &[AgentTodoItem]) {
    if todos.is_empty() {
        return;
    }
    out.push_str("\n## Todos\n\n");
    for todo in todos {
        let (mark, note) = match todo.status {
            AgentTodoStatus::Completed => ("x", ""),
            AgentTodoStatus::Pending => (" ", ""),
            AgentTodoStatus::InProgress => (" ", " (in progress)"),
            AgentTodoStatus::Cancelled => (" ", " (cancelled)"),
            AgentTodoStatus::Failed => ("!", " (failed)"),
            AgentTodoStatus::Blocked => (" ", " (blocked)"),
        };
        let _ = writeln!(out, "- [{mark}] {}{note}", todo.content);
    }
}

fn markdown(session: &AgentSession, messages: &[AgentMessage]) -> String {
    let mut out = format!("# {}\n\n", session.title);
    if let Some(target) = &session.target {
        let _ = writeln!(out, "- Model: {}", target.label());
    }
    if let Some(dir) = &session.workspace_dir {
        let _ = writeln!(out, "- Folder: {dir}");
    }
    for message in messages {
        let heading = match message.role {
            AgentRole::Summary => "Summary of earlier messages",
            AgentRole::User => "User",
            AgentRole::Assistant => "Assistant",
            AgentRole::Tool => "Tool results",
        };
        let _ = write!(out, "\n## {heading}\n\n");
        for attachment in &message.attachments {
            let _ = writeln!(
                out,
                "**Attachment** {} ({}, {} bytes)\n",
                attachment.name, attachment.media_type, attachment.size_bytes
            );
        }
        for part in &message.parts {
            match part {
                HubPart::Text {
                    text, citations, ..
                } => {
                    let _ = write!(out, "{}\n\n", text.trim());
                    push_citations(&mut out, citations);
                }
                HubPart::Reasoning { text, .. } if !text.trim().is_empty() => {
                    let _ = write!(
                        out,
                        "<details><summary>Reasoning</summary>\n\n{}\n\n</details>\n\n",
                        text.trim()
                    );
                }
                HubPart::ToolCall {
                    name, arguments, ..
                } => {
                    let _ = write!(
                        out,
                        "**Tool call** `{name}`\n\n```json\n{arguments}\n```\n\n"
                    );
                }
                HubPart::ToolResult {
                    name,
                    content,
                    is_error,
                    ..
                } => {
                    let text: String = content
                        .iter()
                        .filter_map(|part| match part {
                            HubPart::Text { text, .. } => Some(text.as_str()),
                            _ => None,
                        })
                        .collect::<Vec<_>>()
                        .join("\n");
                    let _ = write!(
                        out,
                        "**{}** `{}`\n\n```\n{}\n```\n\n",
                        if *is_error {
                            "Tool error"
                        } else {
                            "Tool result"
                        },
                        name.as_deref().unwrap_or("tool"),
                        text
                    );
                }
                HubPart::Image { .. } => out.push_str("[image]\n\n"),
                HubPart::Document { document } => {
                    let name = document.name.as_deref().unwrap_or(&document.media_type);
                    let _ = write!(out, "[document: {name}]\n\n");
                }
                HubPart::Audio { .. } => out.push_str("[audio]\n\n"),
                HubPart::ServerToolResult { citations, .. } => push_citations(&mut out, citations),
                HubPart::Reasoning { .. } | HubPart::ServerToolUse { .. } => {}
            }
        }
        if message.partial {
            out.push_str("> Interrupted: this answer is incomplete.\n");
        }
        if let Some(error) = &message.error {
            let _ = writeln!(out, "> Stopped: {error}");
        }
    }
    push_todos(&mut out, &session.todos);
    out
}

/// The JSON export: every message of every branch (`parent_id` and `branch_index` give the
/// tree) and `active_path`, the ids from the root to the head.
#[derive(Serialize)]
struct SessionExport<'a> {
    session: &'a AgentSession,
    messages: &'a [AgentMessage],
    active_path: Vec<&'a str>,
    runs: &'a [AgentRunSummary],
}

/// [`agent_session_export_redacted_impl`] with the default: secrets redacted.
pub fn agent_session_export_impl(
    state: &AppState,
    session_id: &str,
    format: AgentExportFormat,
) -> Result<String, AppError> {
    agent_session_export_redacted_impl(state, session_id, format, None)
}

/// The export with known secret values, credential fields and token shapes replaced,
/// and how many it replaced. The secret store is read after the store lock is released.
fn redacted_export(
    state: &AppState,
    session_id: &str,
    format: AgentExportFormat,
) -> Result<(String, u32), AppError> {
    let raw = raw_export(state, session_id, format)?;
    let needles =
        crate::agent::secret_guard::expand_needles(state.secrets().active_redaction_needles());
    Ok(crate::llm::capture::redact_text(&raw, &needles))
}

/// The session as a Markdown transcript of the active path (messages, attachments, documents,
/// citations and task list), or as JSON holding the whole message tree with the active path
/// marked, the session and its runs. Redacted unless `redact` is `Some(false)`.
pub fn agent_session_export_redacted_impl(
    state: &AppState,
    session_id: &str,
    format: AgentExportFormat,
    redact: Option<bool>,
) -> Result<String, AppError> {
    if redact == Some(false) {
        return raw_export(state, session_id, format);
    }
    redacted_export(state, session_id, format).map(|(text, _)| text)
}

/// How many values a redacted export of the session would replace; the export applies the same
/// pass, so the two numbers agree.
pub fn agent_session_export_preview_impl(
    state: &AppState,
    session_id: &str,
    format: AgentExportFormat,
) -> Result<AgentExportPreview, AppError> {
    redacted_export(state, session_id, format)
        .map(|(_, redactions)| AgentExportPreview { redactions })
}

fn raw_export(
    state: &AppState,
    session_id: &str,
    format: AgentExportFormat,
) -> Result<String, AppError> {
    let store = state.store();
    let session = live_session(&store, session_id)?;
    match format {
        AgentExportFormat::Markdown => {
            let mut path = store.list_active_path(session_id).map_err(command_error)?;
            fill_attachments(state, &store, &mut path)?;
            drop(store);
            Ok(markdown(&session, &path))
        }
        AgentExportFormat::Json => {
            let mut messages = store
                .list_agent_messages(session_id)
                .map_err(command_error)?;
            fill_attachments(state, &store, &mut messages)?;
            let active: Vec<String> = store
                .list_active_path(session_id)
                .map_err(command_error)?
                .into_iter()
                .map(|message| message.id)
                .collect();
            let runs = store
                .list_agent_runs(Some(session_id), false, MAX_RUNS)
                .map_err(command_error)?;
            drop(store);
            serde_json::to_string_pretty(&SessionExport {
                session: &session,
                messages: &messages,
                active_path: active.iter().map(String::as_str).collect(),
                runs: &runs,
            })
            .map_err(|error| AppError::internal(error.to_string()))
        }
    }
}
