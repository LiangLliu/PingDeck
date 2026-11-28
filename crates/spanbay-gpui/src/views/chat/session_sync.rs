//! Session catalog, titles, open/refresh of the active chat for `ChatView` (the live stream
//! itself is in `stream_sync.rs`).

use gpui_kit::{Context, FollowMode};
use spanbay_desktop::core::agent::model::{
    AgentSession, AgentSessionFilter, AgentSessionInput, AgentSessionPatch, AgentSkill,
    AgentToolCallRecord, FileChange, QueuedTurn, ReasoningEffort,
};
use spanbay_desktop::core::agent::tools::process::BackgroundProcess;
use spanbay_desktop::core::app::{
    AgentPageRequest, AppState, EnvironmentDto, WorkspaceInfo, agent_board_impl,
    agent_changes_list_impl, agent_process_list_impl, agent_queue_list_impl,
    agent_session_create_impl, agent_session_generate_title_impl, agent_session_get_impl,
    agent_session_list_impl, agent_session_search_impl, agent_session_update_impl,
    agent_skill_list_impl, agent_workspace_info_impl, environment_list_impl, workspace_list_impl,
};
use spanbay_desktop::core::model::settings::ToolApproval;
use spanbay_desktop::state::ChatThreadState;

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::ChatView;
use super::actions::{load_model_options, resolve_fallback_target};
use super::actions_extra::resolve_active_binding;
use super::types::ChatModal;

impl ChatView {
    pub fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        self.fetch_catalog(true, false, cx);
    }

    pub fn reload_sessions(&mut self, cx: &mut Context<Self>) {
        self.fetch_catalog(false, true, cx);
    }

    fn fetch_catalog(
        &mut self,
        with_search_hits: bool,
        auto_open_first: bool,
        cx: &mut Context<Self>,
    ) {
        let include_archived = self.show_archived;
        let query_raw = self.search_input.read(cx).value().trim().to_owned();
        let query_check = query_raw.clone();
        let query = (!query_raw.is_empty()).then_some(query_raw);
        let dir = self.session.as_ref().and_then(|s| s.workspace_dir.clone());
        let sid_check = self.thread.session_id.clone();
        self.catalog_filter_epoch = self.catalog_filter_epoch.wrapping_add(1);
        let filter_epoch = self.catalog_filter_epoch;
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let hits = if with_search_hits {
                    query
                        .as_deref()
                        .map(|q| agent_session_search_impl(state, q, None).unwrap_or_default())
                        .unwrap_or_default()
                } else {
                    Vec::new()
                };
                let filter = AgentSessionFilter {
                    include_archived,
                    query,
                    ..AgentSessionFilter::default()
                };
                let page = agent_session_list_impl(state, Some(filter), Some(50), None);
                let catalog = load_model_options(state);
                let board = agent_board_impl(state).ok();
                let workspaces = workspace_list_impl(state).unwrap_or_default();
                let skills = agent_skill_list_impl(state, dir.as_deref()).unwrap_or_default();
                (page, hits, catalog, board, workspaces, skills)
            },
            move |this, (res, hits, (opts, meta), board, workspaces, skills), cx| {
                this.available_models = opts;
                this.provider_meta = meta;
                this.board = board;
                this.workspaces = workspaces;
                if this.thread.session_id == sid_check {
                    this.available_skills = skills;
                }
                // A slower answer for an older query or archived toggle must not replace current results.
                if this.catalog_filter_epoch != filter_epoch
                    || this.search_input.read(cx).value().trim() != query_check
                {
                    cx.notify();
                    return;
                }
                if with_search_hits {
                    this.search_hits = hits;
                }
                if let Ok(page) = res {
                    let first_id = page.sessions.first().map(|s| s.id.clone());
                    this.sessions = page.sessions;
                    this.sessions_next_cursor = page.next_cursor;
                    this.sessions_loading_more = false;
                    if auto_open_first
                        && this.thread.session_id.is_empty()
                        && !this.submitting
                        && let Some(id) = first_id
                    {
                        this.open_session(id, cx);
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    /// Reflects a session title update immediately in the active header and sidebar list.
    pub(super) fn apply_session_title_locally(
        &mut self,
        session_id: &str,
        title: &str,
        lock: bool,
    ) {
        if self.thread.session_id == session_id {
            self.thread.title = title.to_owned();
            if let Some(session) = &mut self.session {
                session.title = title.to_owned();
                if lock {
                    session.title_locked = true;
                }
            }
        }
        for row in &mut self.sessions {
            if row.id == session_id {
                row.title = title.to_owned();
            }
        }
    }

    /// Asynchronously asks the session's model to summarize the user's first question into a
    /// concise title. Succeeding updates and locks the title; failing keeps the provisional title
    /// untouched and locks it so it is never retried or modified again.
    pub fn spawn_auto_title_upgrade(
        &mut self,
        session_id: String,
        expected_current_title: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let sid_for_cb = session_id.clone();
        let automatic = expected_current_title.is_some();
        let task = spawn_async_into(
            cx,
            move |state| async move {
                agent_session_generate_title_impl(
                    &state,
                    &session_id,
                    expected_current_title.as_deref(),
                )
                .await
            },
            move |this, res, cx| {
                if let Ok(Some(new_title)) = res {
                    this.apply_session_title_locally(&sid_for_cb, &new_title, true);
                    cx.notify();
                } else if automatic
                    && this.thread.session_id == sid_for_cb
                    && let Some(session) = &mut this.session
                {
                    session.title_locked = true;
                }
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn refresh_after_turn(&mut self, cx: &mut Context<Self>) {
        self.refresh_catalog(cx);
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let detail = agent_session_get_impl(state, &sid, AgentPageRequest::default());
                let changes = agent_changes_list_impl(state, &sid).unwrap_or_default();
                let procs = agent_process_list_impl(state, Some(&sid)).unwrap_or_default();
                let queue = agent_queue_list_impl(state, &sid).unwrap_or_default();
                (detail, changes, procs, queue)
            },
            move |this, (res, changes, procs, queue), cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                this.persisted_changes = changes;
                this.turn_inputs_version = this.turn_inputs_version.wrapping_add(1);
                this.review_diff_cache = None;
                if this.review_open {
                    this.ensure_review_selection_and_load(cx);
                }
                this.processes = procs;
                this.thread.queued_turns = queue;
                if let Ok(detail) = res {
                    if let Some(run) = detail.runs.first() {
                        this.latest_session_run_id = Some(run.run_id.clone());
                        if !this.is_run_active() {
                            this.thread.active_run = Some(run.clone());
                        }
                    }
                    this.merge_audit_tool_calls(detail.tool_calls);
                    this.plan_mode = detail.plan_mode.unwrap_or(false);
                    this.thread.queue_paused = detail.queue_paused.unwrap_or(false);
                    this.thread.todos = detail.session.todos.clone();
                    this.thread.context_used_tokens = detail.session.last_context_tokens;
                    this.session_costs = detail.cost_by_currency.unwrap_or_default();
                    let keep_newer_locked_title = this.session.as_ref().is_some_and(|cur| {
                        cur.id == detail.session.id
                            && cur.title_locked
                            && !detail.session.title_locked
                    });
                    let mut session = detail.session;
                    if keep_newer_locked_title {
                        session.title = this.thread.title.clone();
                        session.title_locked = true;
                    }
                    if !this.is_run_active() {
                        let first_latest_seq = detail.messages.first().map(|m| m.seq);
                        let prev_earliest_seq = this.thread.messages.first().map(|m| m.seq);
                        if this.has_later_messages {
                            this.thread
                                .replace_snapshot(session.title.clone(), detail.messages);
                            this.has_earlier_messages = detail.has_more;
                            this.has_later_messages = false;
                            this.loaded_earlier_once = false;
                        } else {
                            this.thread
                                .merge_latest_snapshot(session.title.clone(), detail.messages);
                            if prev_earliest_seq.is_none() || prev_earliest_seq == first_latest_seq
                            {
                                this.has_earlier_messages = detail.has_more;
                            }
                        }
                    }
                    if session.title_locked {
                        this.title_generated_sessions.insert(session.id.clone());
                    }
                    this.session = Some(session);
                    this.turn_inputs_version = this.turn_inputs_version.wrapping_add(1);
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn open_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.open_session_at(session_id, None, cx);
    }

    /// Opens a session and centers its first page around `message_id` (used by sidebar FTS hits).
    pub fn open_session_at_message(
        &mut self,
        session_id: String,
        message_id: String,
        cx: &mut Context<Self>,
    ) {
        self.open_session_at(session_id, Some(message_id), cx);
    }

    pub(super) fn reset_session_view(
        &mut self,
        session_id: String,
        around_message_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        self.drop_subscription(cx);
        self.thread = if session_id.is_empty() {
            ChatThreadState::default()
        } else {
            ChatThreadState::new(session_id, "")
        };
        self.session = None;
        self.latest_session_run_id = None;
        self.plan_mode = false;
        self.plan_revising = false;
        self.pending_placeholder_reset = true;
        self.last_approval_notice = None;
        self.last_focused_gate_id = None;
        self.has_earlier_messages = false;
        self.has_later_messages = false;
        self.loaded_earlier_once = false;
        self.loading_earlier_messages = false;
        self.loading_later_messages = false;
        self.load_later_failed = false;
        self.audit_tool_calls.clear();
        self.persisted_changes.clear();
        self.turn_inputs_version = self.turn_inputs_version.wrapping_add(1);
        self.processes.clear();
        self.workspace_info = None;
        self.environments.clear();
        self.session_costs.clear();
        self.pending_attachments.clear();
        self.selected_skills.clear();
        self.question_draft_choices.clear();
        self.question_other_inputs.clear();
        self.plan_gate_show_markdown.clear();
        self.active_modal = None;
        self.tool_output_loading_more = false;
        self.review_diff_cache = None;
        self.review_diff_error = None;
        self.review_tool_output = None;
        self.review_selected_path = None;
        self.review_selected_change_id = None;
        self.review_undo_precheck = false;
        self.review_comment_target = None;
        self.diff_comments.clear();
        self.editing_queue_id = None;
        self.status_msg = None;
        self.submitting = false;
        self.run_started_at = None;
        self.tick_task = None;
        self.pending_user_text = None;
        self.thinking_started_at = None;
        self.thinking_ended_at = None;
        self.collapsed_live_reasoning = false;
        // Row keys are message seqs, unique only within one chat: start the list over.
        self.list_state.reset(0);
        self.list_keys.clear();
        self.list_state.set_follow_mode(FollowMode::Tail);
        self.context_breakdown = None;
        self.deny_reason_call = None;
        self.error_details_open = false;
        self.pending_scroll_message_id = around_message_id;
        // Decoded images belong to the chat being left; a new chat starts a fresh retry budget.
        self.media_cache.borrow_mut().clear();
        self.subscribe_attempts = 0;
    }

    fn open_session_at(
        &mut self,
        session_id: String,
        around_message_id: Option<String>,
        cx: &mut Context<Self>,
    ) {
        let prev_sid = self.thread.session_id.clone();
        if !prev_sid.is_empty() && prev_sid != session_id {
            // If a prior switch hadn't rendered yet, `pending_draft_sync` holds `prev_sid`'s
            // true draft while `composer` still holds the session before it (A -> B -> C).
            let current_text = self
                .pending_draft_sync
                .take()
                .unwrap_or_else(|| self.composer.read(cx).value().to_string());
            self.drafts.set(&prev_sid, current_text);
            runtime(cx).save_chat_drafts(&self.drafts);
        }

        self.reset_session_view(session_id.clone(), around_message_id.clone(), cx);
        self.pending_draft_sync = Some(self.drafts.get(&session_id).unwrap_or_default().to_owned());

        self.snapshot_and_subscribe_page(
            AgentPageRequest {
                around_message_id,
                ..AgentPageRequest::default()
            },
            cx,
        );
        cx.notify();
    }

    /// Upserts audit tool-call rows by `call_id` so terminal rows replace earlier mid-run ones
    /// while preserving rows loaded for earlier pages.
    pub(super) fn merge_audit_tool_calls(&mut self, incoming: Vec<AgentToolCallRecord>) {
        for tc in incoming {
            if let Some(existing) = self
                .audit_tool_calls
                .iter_mut()
                .find(|e| e.call_id == tc.call_id)
            {
                *existing = tc;
            } else {
                self.audit_tool_calls.push(tc);
            }
        }
        self.turn_inputs_version = self.turn_inputs_version.wrapping_add(1);
    }

    /// Applies the side panels read with a chat's first snapshot.
    pub(super) fn apply_session_extras(&mut self, extras: SessionExtras) {
        self.persisted_changes = extras.changes;
        self.turn_inputs_version = self.turn_inputs_version.wrapping_add(1);
        self.processes = extras.processes;
        self.thread.queued_turns = extras.queue;
        self.workspace_info = extras.workspace_info;
        self.environments = extras.environments;
        self.available_skills = extras.skills;
    }

    pub fn patch_session(&mut self, patch: AgentSessionPatch, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if !sid.is_empty() {
            self.patch_session_by_id(sid, patch, cx);
            return;
        }
        let show_saved_instructions = patch.instructions.is_some();
        let active_ws = self.active_workspace_id.clone();
        let active_env = self.active_environment_id.clone();
        let epoch = self.stream_epoch;
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let target = patch
                    .target
                    .clone()
                    .or_else(|| resolve_fallback_target(state));
                let (spanbay_workspace_id, spanbay_environment_id) =
                    resolve_active_binding(state, active_ws, active_env);
                let session = agent_session_create_impl(
                    state,
                    AgentSessionInput {
                        target,
                        workspace_dir: patch.workspace_dir.clone(),
                        spanbay_workspace_id,
                        spanbay_environment_id,
                        ..AgentSessionInput::default()
                    },
                )?;
                agent_session_update_impl(state, &session.id, patch)
            },
            move |this, res, cx| {
                if this.stream_epoch != epoch || !this.thread.session_id.is_empty() {
                    return;
                }
                match res {
                    Ok(session) => {
                        let id = session.id.clone();
                        this.refresh_catalog(cx);
                        this.open_session(id, cx);
                        if show_saved_instructions {
                            this.status_msg = Some((
                                Tone::Success,
                                crate::ui::tr(cx).chat().actions_saved_instructions().into(),
                            ));
                        }
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn patch_session_by_id(
        &mut self,
        sid: String,
        patch: AgentSessionPatch,
        cx: &mut Context<Self>,
    ) {
        let refresh_folder_info = patch.workspace_dir.is_some();
        let invalidates_context = patch.target.is_some() || patch.instructions.is_some();
        let show_saved_instructions = patch.instructions.is_some();
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let updated = agent_session_update_impl(state, &sid, patch)?;
                let ws_info = refresh_folder_info.then(|| {
                    updated
                        .workspace_dir
                        .as_deref()
                        .and_then(|p| agent_workspace_info_impl(state, p).ok())
                });
                Ok::<_, spanbay_desktop::core::error::AppError>((updated, ws_info))
            },
            move |this, res, cx| {
                match res {
                    Ok((updated, ws_info)) => {
                        if this.thread.session_id == sid_check {
                            this.thread.title = updated.title.clone();
                            this.session = Some(updated);
                            this.turn_inputs_version = this.turn_inputs_version.wrapping_add(1);
                            if let Some(info) = ws_info {
                                this.workspace_info = info;
                            }
                            if invalidates_context {
                                this.context_breakdown = None;
                            }
                            if show_saved_instructions {
                                this.status_msg = Some((
                                    Tone::Success,
                                    crate::ui::tr(cx).chat().actions_saved_instructions().into(),
                                ));
                            }
                        }
                        this.refresh_catalog(cx);
                    }
                    Err(err) => this.status_msg = Some((Tone::Danger, err.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn request_permission_mode(&mut self, mode: ToolApproval, cx: &mut Context<Self>) {
        if mode == ToolApproval::AutoAll {
            self.active_modal = Some(ChatModal::FullAccessConfirm {
                acknowledged: false,
            });
            cx.notify();
        } else {
            self.set_permission_mode(mode, cx);
        }
    }

    pub fn set_permission_mode(&mut self, mode: ToolApproval, cx: &mut Context<Self>) {
        self.patch_session(
            AgentSessionPatch {
                permission_mode: Some(mode),
                ..Default::default()
            },
            cx,
        );
    }

    pub fn set_reasoning_effort(
        &mut self,
        effort: Option<ReasoningEffort>,
        cx: &mut Context<Self>,
    ) {
        self.patch_session(
            AgentSessionPatch {
                reasoning_effort: Some(effort),
                ..Default::default()
            },
            cx,
        );
    }

    pub fn toggle_web_enabled(&mut self, cx: &mut Context<Self>) {
        let next = !self.session.as_ref().is_some_and(|s| s.web_enabled);
        self.patch_session(
            AgentSessionPatch {
                web_enabled: Some(next),
                ..Default::default()
            },
            cx,
        );
    }

    pub fn toggle_pinned(&mut self, cx: &mut Context<Self>) {
        let next = !self
            .session
            .as_ref()
            .is_some_and(|s| s.pinned_at_ms.is_some());
        self.patch_session(
            AgentSessionPatch {
                pinned: Some(next),
                ..Default::default()
            },
            cx,
        );
    }

    pub fn save_instructions(&mut self, cx: &mut Context<Self>) {
        let text = self.instructions_input.read(cx).value().trim().to_owned();
        self.patch_session(
            AgentSessionPatch {
                instructions: Some(text),
                ..AgentSessionPatch::default()
            },
            cx,
        );
    }
}

pub(super) struct SessionExtras {
    changes: Vec<FileChange>,
    processes: Vec<BackgroundProcess>,
    queue: Vec<QueuedTurn>,
    workspace_info: Option<WorkspaceInfo>,
    environments: Vec<EnvironmentDto>,
    skills: Vec<AgentSkill>,
}

pub(super) fn load_session_extras(state: &AppState, session: &AgentSession) -> SessionExtras {
    let sid = session.id.as_str();
    let dir = session.workspace_dir.as_deref();
    SessionExtras {
        changes: agent_changes_list_impl(state, sid).unwrap_or_default(),
        processes: agent_process_list_impl(state, Some(sid)).unwrap_or_default(),
        queue: agent_queue_list_impl(state, sid).unwrap_or_default(),
        workspace_info: dir.and_then(|p| agent_workspace_info_impl(state, p).ok()),
        environments: session
            .spanbay_workspace_id
            .clone()
            .and_then(|wid| environment_list_impl(state, wid).ok())
            .unwrap_or_default(),
        skills: agent_skill_list_impl(state, dir).unwrap_or_default(),
    }
}
