//! Session hydration, live stream subscription, and catalog synchronization for `ChatView`.

use std::time::{Duration, Instant};

use gpui_kit::Context;
use spanbay_desktop::core::agent::attention::AgentAttention;
use spanbay_desktop::core::agent::dto::AgentSubscribeTarget;
use spanbay_desktop::core::agent::model::{AgentEvent, AgentSessionFilter};
use spanbay_desktop::core::app::{
    AgentPageRequest, agent_attention_subscribe_impl, agent_board_impl, agent_changes_list_impl,
    agent_process_list_impl, agent_queue_list_impl, agent_run_subscribe_impl,
    agent_run_unsubscribe_impl, agent_session_generate_title_impl, agent_session_get_impl,
    agent_session_list_impl, agent_skill_list_impl, agent_workspace_info_impl,
    environment_list_impl, workspace_list_impl,
};
use spanbay_desktop::core::logging::{LogLevel, log_line};
use spanbay_desktop::core::model::events::Sequenced;
use spanbay_desktop::state::ChatThreadState;
use spanbay_desktop::streams::{channel_sink, coalesced_agent_sink};

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into, subscribe_stream_into};

use super::actions::load_model_options;
use super::{ChatView, reasoning};

impl ChatView {
    /// Subscribes to the global [`AgentAttention`] feed so sidebar status pills stay synchronized
    /// in real time across all concurrent chats.
    pub(super) fn subscribe_attention(&mut self, cx: &mut Context<Self>) {
        if self.attention_task.is_some() {
            return;
        }
        let rt = runtime(cx);
        let (sink, rx) = channel_sink::<Sequenced<AgentAttention>>();
        if let Ok(sub_id) = agent_attention_subscribe_impl(rt.state(), None, sink) {
            self.attention_sub_id = Some(sub_id);
            self.attention_task = Some(subscribe_stream_into(cx, rx, 16, |this, batch, cx| {
                if batch.is_empty() {
                    return;
                }
                this.load_board(cx);
            }));
        }
    }

    pub(super) fn start_elapsed_ticker(&mut self, cx: &mut Context<Self>) {
        if self.tick_task.is_some() {
            return;
        }
        self.tick_task = Some(cx.spawn(async move |weak, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let keep_going = weak
                    .update(cx, |this, cx| {
                        let active = this.is_run_active();
                        if !active {
                            this.tick_task = None;
                        }
                        cx.notify();
                        active
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
        }));
    }

    pub fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        let include_archived = self.show_archived;
        let query_raw = self.search_input.read(cx).value().trim().to_owned();
        let query = (!query_raw.is_empty()).then_some(query_raw);
        let dir = self.session.as_ref().and_then(|s| s.workspace_dir.clone());
        let task = spawn_blocking_into(
            cx,
            move |state| {
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
                (page, catalog, board, workspaces, skills)
            },
            |this, (res, (opts, meta), board, workspaces, skills), cx| {
                this.available_models = opts;
                this.provider_meta = meta;
                this.board = board;
                this.workspaces = workspaces;
                this.available_skills = skills;
                if let Ok(page) = res {
                    this.sessions = page.sessions;
                    this.sessions_next_cursor = page.next_cursor;
                    this.sessions_loading_more = false;
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn reload_sessions(&mut self, cx: &mut Context<Self>) {
        let include_archived = self.show_archived;
        let query_raw = self.search_input.read(cx).value().trim().to_owned();
        let query = (!query_raw.is_empty()).then_some(query_raw);
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let filter = AgentSessionFilter {
                    include_archived,
                    query,
                    ..AgentSessionFilter::default()
                };
                let page = agent_session_list_impl(state, Some(filter), Some(50), None);
                let catalog = load_model_options(state);
                let board = agent_board_impl(state).ok();
                let workspaces = workspace_list_impl(state).unwrap_or_default();
                let skills = agent_skill_list_impl(state, None).unwrap_or_default();
                (page, catalog, board, workspaces, skills)
            },
            |this, (res, (opts, meta), board, workspaces, skills), cx| {
                this.available_models = opts;
                this.provider_meta = meta;
                this.board = board;
                this.workspaces = workspaces;
                this.available_skills = skills;
                if let Ok(page) = res {
                    let first_id = page.sessions.first().map(|s| s.id.clone());
                    this.sessions = page.sessions;
                    this.sessions_next_cursor = page.next_cursor;
                    this.sessions_loading_more = false;
                    if this.thread.session_id.is_empty()
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

    pub(super) fn ensure_subscribed(&mut self, session_id: &str, cx: &mut Context<Self>) {
        if self.subscribed_session_id.as_deref() == Some(session_id) && self.stream_task.is_some() {
            return;
        }
        let rt = runtime(cx);
        if let Some(old_id) = self.active_sub_id.take() {
            agent_run_unsubscribe_impl(rt.state(), old_id);
        }
        self.stream_task = None;
        self.subscribed_session_id = None;

        let (sink, rx) = coalesced_agent_sink(rt.tokio_handle());
        let target = AgentSubscribeTarget::Session {
            session_id: session_id.to_owned(),
        };
        match agent_run_subscribe_impl(rt.state(), &target, None, sink) {
            Ok(sub_id) => {
                self.active_sub_id = Some(sub_id);
                self.subscribed_session_id = Some(session_id.to_owned());
                self.stream_task = Some(subscribe_stream_into(cx, rx, 32, |this, batch, cx| {
                    let mut got_user_msg = false;
                    let mut got_run_started: Option<String> = None;
                    let mut got_title: Option<(String, String)> = None;
                    let mut got_finished = false;
                    let mut got_scroll_worthy = false;
                    let mut auto_expand_reasoning_ids: Vec<String> = Vec::new();

                    for env in &batch {
                        match &env.event {
                            AgentEvent::RunStarted { .. } => {
                                got_run_started = Some(env.run_id.clone());
                                got_scroll_worthy = true;
                                this.thinking_started_at = None;
                                this.thinking_ended_at = None;
                            }
                            AgentEvent::UserMessage { .. } => {
                                got_user_msg = true;
                                got_scroll_worthy = true;
                            }
                            AgentEvent::ReasoningDelta { .. } => {
                                got_scroll_worthy = true;
                                this.thinking_started_at.get_or_insert_with(Instant::now);
                            }
                            AgentEvent::TextDelta { text } => {
                                got_scroll_worthy = true;
                                if text.contains("<think>") || text.contains("<thinking>") {
                                    this.thinking_started_at.get_or_insert_with(Instant::now);
                                } else if this.thinking_started_at.is_some()
                                    && this.thinking_ended_at.is_none()
                                {
                                    this.thinking_ended_at = Some(Instant::now());
                                }
                            }
                            AgentEvent::ToolCallStarted { .. } => {
                                got_scroll_worthy = true;
                                if this.thinking_started_at.is_some()
                                    && this.thinking_ended_at.is_none()
                                {
                                    this.thinking_ended_at = Some(Instant::now());
                                }
                            }
                            AgentEvent::MessageSaved { message } => {
                                got_scroll_worthy = true;
                                if let Some(started) = this.thinking_started_at.take() {
                                    let ended =
                                        this.thinking_ended_at.take().unwrap_or_else(Instant::now);
                                    let dur_ms =
                                        ended.saturating_duration_since(started).as_millis() as u64;
                                    if dur_ms > 0 {
                                        this.thinking_durations.insert(message.id.clone(), dur_ms);
                                    }
                                }
                                let (rinfo, _) =
                                    reasoning::extract_message_reasoning(message, None);
                                if rinfo.kind == reasoning::ReasoningKind::Plaintext {
                                    auto_expand_reasoning_ids
                                        .push(format!("reason-{}", message.id));
                                }
                            }
                            AgentEvent::ToolOutputDelta { .. }
                            | AgentEvent::ToolApprovalRequired { .. }
                            | AgentEvent::GateOpened { .. } => {
                                got_scroll_worthy = true;
                            }
                            AgentEvent::SubagentStarted { .. }
                            | AgentEvent::SubagentFinished { .. }
                            | AgentEvent::ToolResult { .. } => {
                                got_scroll_worthy = true;
                            }
                            AgentEvent::SessionTitleUpdated { session_id, title } => {
                                got_title = Some((session_id.clone(), title.clone()));
                            }
                            AgentEvent::Done { .. } | AgentEvent::Error { .. } => {
                                got_finished = true;
                                got_scroll_worthy = true;
                                this.thinking_started_at = None;
                                this.thinking_ended_at = None;
                            }
                            _ => {}
                        }
                    }

                    let changed = this.thread.apply_batch(batch);
                    if !this.collapsed_live_reasoning {
                        for rkey in auto_expand_reasoning_ids {
                            this.expanded_reasoning.insert(rkey);
                        }
                    }
                    if got_user_msg {
                        this.pending_user_text = None;
                    }
                    if let Some(started_rid) = got_run_started {
                        this.submitting = false;
                        this.latest_session_run_id = Some(started_rid);
                        this.run_started_at.get_or_insert_with(Instant::now);
                        this.start_elapsed_ticker(cx);
                    }
                    if let Some((title_sid, updated_title)) = got_title {
                        this.apply_session_title_locally(&title_sid, &updated_title, false);
                        if this.title_generated_sessions.insert(title_sid.clone()) {
                            this.spawn_auto_title_upgrade(title_sid, Some(updated_title), cx);
                        }
                        if !got_finished {
                            this.refresh_catalog(cx);
                        }
                    }
                    if got_finished {
                        this.submitting = false;
                        this.run_started_at = None;
                        this.tick_task = None;
                        this.pending_user_text = None;
                        this.refresh_after_turn(cx);
                    }
                    if changed {
                        if this.thread.needs_reload {
                            let sid = this.thread.session_id.clone();
                            this.open_session(sid, cx);
                        } else if got_scroll_worthy {
                            this.auto_scroll_if_following();
                        }
                        cx.notify();
                    }
                }));
            }
            Err(err) => {
                log_line(
                    LogLevel::Warn,
                    "chat",
                    &format!(
                        "failed to subscribe to session {session_id}: {}",
                        err.message()
                    ),
                );
            }
        }
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
                this.processes = procs;
                this.thread.queued_turns = queue;
                if let Ok(detail) = res {
                    if let Some(run) = detail.runs.first() {
                        this.latest_session_run_id = Some(run.run_id.clone());
                    }
                    for tc in detail.tool_calls {
                        if !this
                            .audit_tool_calls
                            .iter()
                            .any(|existing| existing.call_id == tc.call_id)
                        {
                            this.audit_tool_calls.push(tc);
                        }
                    }
                    this.plan_mode = detail.plan_mode.unwrap_or(false);
                    this.thread.queue_paused = detail.queue_paused.unwrap_or(false);
                    this.thread.todos = detail.session.todos.clone();
                    this.thread.context_used_tokens = detail.session.last_context_tokens;
                    this.thread.cost_micro_usd = detail.session.total_cost_micro_usd;
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
                        this.thread
                            .merge_latest_snapshot(session.title.clone(), detail.messages);
                        if prev_earliest_seq.is_none() || prev_earliest_seq == first_latest_seq {
                            this.has_earlier_messages = detail.has_more;
                        }
                    }
                    if session.title_locked {
                        this.title_generated_sessions.insert(session.id.clone());
                    }
                    this.session = Some(session);
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn open_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        let prev_sid = self.thread.session_id.clone();
        if !prev_sid.is_empty() && prev_sid != session_id {
            let current_text = self.composer.read(cx).value().to_string();
            self.drafts.set(&prev_sid, current_text);
            runtime(cx).save_chat_drafts(&self.drafts);
        }

        self.thread = ChatThreadState::new(session_id.clone(), "");
        self.session = None;
        self.latest_session_run_id = None;
        self.plan_mode = false;
        self.has_earlier_messages = false;
        self.loading_earlier_messages = false;
        self.audit_tool_calls.clear();
        self.persisted_changes.clear();
        self.processes.clear();
        self.workspace_info = None;
        self.status_msg = None;
        self.submitting = false;
        self.pending_user_text = None;
        self.thinking_started_at = None;
        self.thinking_ended_at = None;
        self.collapsed_live_reasoning = false;
        self.stick_to_bottom = true;
        self.pending_draft_sync = Some(self.drafts.get(&session_id).unwrap_or_default().to_owned());

        self.ensure_subscribed(&session_id, cx);

        let sid_load = session_id.clone();
        let sid_check = session_id;
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let detail = agent_session_get_impl(state, &sid_load, AgentPageRequest::default());
                let changes = agent_changes_list_impl(state, &sid_load).unwrap_or_default();
                let procs = agent_process_list_impl(state, Some(&sid_load)).unwrap_or_default();
                let queue = agent_queue_list_impl(state, &sid_load).unwrap_or_default();
                let (ws_info, envs, skills) = match &detail {
                    Ok(d) => {
                        let wi = d
                            .session
                            .workspace_dir
                            .as_deref()
                            .and_then(|p| agent_workspace_info_impl(state, p).ok());
                        let es = d
                            .session
                            .spanbay_workspace_id
                            .as_deref()
                            .and_then(|wid| environment_list_impl(state, wid.to_owned()).ok())
                            .unwrap_or_default();
                        let sk = agent_skill_list_impl(state, d.session.workspace_dir.as_deref())
                            .unwrap_or_default();
                        (wi, es, sk)
                    }
                    Err(_) => (None, Vec::new(), Vec::new()),
                };
                (detail, changes, procs, queue, ws_info, envs, skills)
            },
            move |this, (res, changes, procs, queue, ws_info, envs, skills), cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                this.persisted_changes = changes;
                this.processes = procs;
                this.workspace_info = ws_info;
                this.environments = envs;
                this.available_skills = skills;
                this.thread.queued_turns = queue;
                if let Ok(detail) = res {
                    this.latest_session_run_id = detail.runs.first().map(|r| r.run_id.clone());
                    this.plan_mode = detail.plan_mode.unwrap_or(false);
                    this.thread.queue_paused = detail.queue_paused.unwrap_or(false);
                    this.has_earlier_messages = detail.has_more;
                    this.loading_earlier_messages = false;
                    if !this.submitting && !this.is_run_active() {
                        this.thread
                            .replace_snapshot(detail.session.title.clone(), detail.messages);
                        if let Some(run) = detail.runs.first() {
                            this.thread.active_run = Some(run.clone());
                        }
                    } else if this.thread.title.is_empty() {
                        this.thread.title = detail.session.title.clone();
                    }
                    if let Some(live) = &detail.live {
                        this.thread.hydrate_live(live);
                        this.run_started_at.get_or_insert_with(Instant::now);
                        this.start_elapsed_ticker(cx);
                    }
                    this.thread.todos = detail.session.todos.clone();
                    this.thread.context_used_tokens = detail.session.last_context_tokens;
                    this.thread.cost_micro_usd = detail.session.total_cost_micro_usd;
                    this.pending_instructions_sync =
                        Some(detail.session.instructions.clone().unwrap_or_default());
                    this.audit_tool_calls = detail.tool_calls;
                    if detail.session.title_locked {
                        this.title_generated_sessions
                            .insert(detail.session.id.clone());
                    }
                    this.session = Some(detail.session);
                    this.scroll_handle.scroll_to_bottom();
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
        cx.notify();
    }
}
