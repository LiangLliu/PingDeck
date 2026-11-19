//! AI Chat & Coding Agent module (`NavModule::Chat`).
//!
//! 3-pane native GPUI workspace (`h_resizable`): categorized thread sidebar, centered
//! editorial conversation stream with live auto-expanded reasoning & structured tool trace cards,
//! floating top-accented approval gate & rounded composer dock with native anchored popovers,
//! and a collapsible right-hand Details inspector (`Progress` / `Changes` / `Context` / `Options`).

pub mod actions;
pub mod composer;
pub mod gates;
pub mod inspector;
pub mod metrics;
pub mod reasoning;
pub mod reasoning_diag;
pub mod sidebar;
pub mod thread;
pub mod trace;
pub mod types;
pub mod welcome;

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, ScrollHandle,
    Styled as _, Subscription, Task, Window, px,
};
use pingdeck_desktop::core::agent::dto::AgentSubscribeTarget;
use pingdeck_desktop::core::agent::model::{
    AgentEvent, AgentRunStatus, AgentSession, AgentSessionFilter, AgentSessionSummary, AgentTarget,
    AgentToolCallRecord,
};
use pingdeck_desktop::core::app::{
    AgentPageRequest, agent_run_subscribe_impl, agent_run_unsubscribe_impl, agent_session_get_impl,
    agent_session_list_impl, settings_get_impl,
};
use pingdeck_desktop::core::logging::{LogLevel, log_line};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::persistence::ChatDraftsStore;
use pingdeck_desktop::state::ChatThreadState;
use pingdeck_desktop::streams::coalesced_agent_sink;

use crate::bridge::{runtime, spawn_blocking_into, subscribe_stream_into};
use crate::ui::{
    InputEvent, InputState, TextareaState, Tone, h_resizable, palette, resizable_panel, to_hsla,
    v_col,
};

use actions::load_model_options;
pub use types::{DetailsTab, ModelOption, ProviderChatMeta};

pub struct ChatView {
    pub sessions: Vec<AgentSessionSummary>,
    pub search_input: Entity<InputState>,
    pub show_archived: bool,
    pub session: Option<AgentSession>,
    pub audit_tool_calls: Vec<AgentToolCallRecord>,
    pub thread: ChatThreadState,
    pub drafts: ChatDraftsStore,
    pub composer: Entity<TextareaState>,
    pub instructions_input: Entity<TextareaState>,
    pub available_models: Vec<ModelOption>,
    pub provider_meta: HashMap<String, ProviderChatMeta>,
    pub expanded_traces: HashSet<String>,
    pub expanded_reasoning: HashSet<String>,
    pub collapsed_live_reasoning: bool,
    pub submitting: bool,
    pub run_started_at: Option<Instant>,
    pub thinking_started_at: Option<Instant>,
    pub thinking_ended_at: Option<Instant>,
    pub thinking_durations: HashMap<String, u64>,
    pub pending_user_text: Option<String>,
    pub scroll_handle: ScrollHandle,
    pub details_open: bool,
    pub details_tab: DetailsTab,
    pub status_msg: Option<(Tone, String)>,
    pub(super) pending_draft_sync: Option<String>,
    pub(super) pending_instructions_sync: Option<String>,
    pub(super) active_sub_id: Option<u64>,
    pub(super) subscribed_session_id: Option<String>,
    pub(super) stream_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl ChatView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let rt = runtime(cx);
        let drafts = rt.load_chat_drafts();
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索对话 / Search chats…"));
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("输入消息或编码任务… (Enter 发送 · Shift+Enter 换行)")
                .rows(3)
                .submit_on_enter(true)
        });
        let instructions_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder("为本对话添加额外指令（系统提示词补充）…")
                .rows(4)
        });

        let enter_sub = cx.subscribe_in(&composer, window, |this, _, ev, window, cx| {
            if matches!(
                ev,
                InputEvent::PressEnter { shift: false, .. }
                    | InputEvent::PressEnter {
                        secondary: true,
                        ..
                    }
            ) {
                this.send_message(window, cx);
            }
        });

        let mut view = Self {
            sessions: Vec::new(),
            search_input,
            show_archived: false,
            session: None,
            audit_tool_calls: Vec::new(),
            thread: ChatThreadState::default(),
            drafts,
            composer,
            instructions_input,
            available_models: Vec::new(),
            provider_meta: HashMap::new(),
            expanded_traces: HashSet::new(),
            expanded_reasoning: HashSet::new(),
            collapsed_live_reasoning: false,
            submitting: false,
            run_started_at: None,
            thinking_started_at: None,
            thinking_ended_at: None,
            thinking_durations: HashMap::new(),
            pending_user_text: None,
            scroll_handle: ScrollHandle::new(),
            details_open: false,
            details_tab: DetailsTab::Progress,
            status_msg: None,
            pending_draft_sync: None,
            pending_instructions_sync: None,
            active_sub_id: None,
            subscribed_session_id: None,
            stream_task: None,
            _subscriptions: vec![enter_sub],
            _tasks: Vec::new(),
        };
        view.reload_sessions(cx);
        view
    }

    pub fn is_run_active(&self) -> bool {
        self.submitting
            || self.thread.active_run.as_ref().is_some_and(|r| {
                matches!(
                    r.status,
                    AgentRunStatus::Queued
                        | AgentRunStatus::Running
                        | AgentRunStatus::WaitingApproval
                )
            })
            || !self.thread.streaming_text.is_empty()
            || !self.thread.streaming_reasoning.is_empty()
    }

    /// Formats `(provider_id, model_id)` using the user's custom provider name (`provider / model`).
    #[must_use]
    pub fn format_provider_model(&self, provider_id: &str, model_id: &str) -> String {
        let pname = self
            .provider_meta
            .get(provider_id)
            .map(|m| m.name.as_str())
            .unwrap_or(provider_id);
        format!("{pname} / {model_id}")
    }

    /// Formats an [`AgentTarget`] as `provider / model` (resolving custom provider names).
    #[must_use]
    pub fn format_target_label(&self, target: &AgentTarget) -> String {
        match target {
            AgentTarget::Pinned {
                provider_id, model, ..
            } => self.format_provider_model(provider_id, model),
            AgentTarget::Named { name } => self.format_raw_model_label(name),
        }
    }

    /// Resolves a stored `provider_id/model` label into `provider_name / model`.
    #[must_use]
    pub fn format_raw_model_label(&self, raw: &str) -> String {
        if let Some((pid, mid)) = raw.split_once('/') {
            self.format_provider_model(pid, mid)
        } else {
            raw.to_owned()
        }
    }

    /// Looks up provider metadata for a raw `provider_id/model` label (or falls back to the
    /// session's current target provider).
    #[must_use]
    pub fn provider_meta_for_raw(&self, raw: Option<&str>) -> Option<&ProviderChatMeta> {
        if let Some((pid, _)) = raw.and_then(|r| r.split_once('/'))
            && let Some(m) = self.provider_meta.get(pid)
        {
            return Some(m);
        }
        match self.session.as_ref().and_then(|s| s.target.as_ref()) {
            Some(AgentTarget::Pinned { provider_id, .. }) => self.provider_meta.get(provider_id),
            Some(AgentTarget::Named { name }) => name
                .split_once('/')
                .and_then(|(pid, _)| self.provider_meta.get(pid)),
            None => self
                .available_models
                .first()
                .and_then(|m| self.provider_meta.get(&m.provider_id)),
        }
    }

    /// Returns true if the currently selected provider is OpenAI-compat without a reasoning quirk.
    #[must_use]
    pub fn active_provider_missing_reasoning_quirk(&self) -> bool {
        self.provider_meta_for_raw(None)
            .is_some_and(ProviderChatMeta::missing_reasoning_quirk)
    }

    /// Lightweight refresh of sessions list + available models (safe to call mid-run).
    pub fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        let include_archived = self.show_archived;
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let filter = AgentSessionFilter {
                    include_archived,
                    ..AgentSessionFilter::default()
                };
                let page = agent_session_list_impl(state, Some(filter), Some(150), None);
                let catalog = load_model_options(state);
                (page, catalog)
            },
            |this, (res, (opts, meta)), cx| {
                this.available_models = opts;
                this.provider_meta = meta;
                if let Ok(page) = res {
                    this.sessions = page.sessions;
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn reload_sessions(&mut self, cx: &mut Context<Self>) {
        let include_archived = self.show_archived;
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let filter = AgentSessionFilter {
                    include_archived,
                    ..AgentSessionFilter::default()
                };
                let page = agent_session_list_impl(state, Some(filter), Some(150), None);
                let catalog = load_model_options(state);
                (page, catalog)
            },
            |this, (res, (opts, meta)), cx| {
                this.available_models = opts;
                this.provider_meta = meta;
                if let Ok(page) = res {
                    let first_id = page.sessions.first().map(|s| s.id.clone());
                    this.sessions = page.sessions;
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
                    let mut got_run_started = false;
                    let mut got_title = false;
                    let mut got_finished = false;
                    let mut got_scroll_worthy = false;
                    let mut auto_expand_reasoning_ids: Vec<String> = Vec::new();

                    for env in &batch {
                        match &env.event {
                            AgentEvent::RunStarted { .. } => {
                                got_run_started = true;
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
                            | AgentEvent::ToolApprovalRequired { .. } => {
                                got_scroll_worthy = true;
                            }
                            AgentEvent::SessionTitleUpdated { .. } => {
                                got_title = true;
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
                    if got_run_started {
                        this.submitting = false;
                        this.run_started_at.get_or_insert_with(Instant::now);
                    }
                    if got_finished {
                        this.submitting = false;
                        this.run_started_at = None;
                        this.pending_user_text = None;
                        this.refresh_after_turn(cx);
                    } else if got_title {
                        this.refresh_catalog(cx);
                    }
                    if changed {
                        if this.thread.needs_reload {
                            let sid = this.thread.session_id.clone();
                            this.open_session(sid, cx);
                        } else if got_scroll_worthy {
                            this.scroll_handle.scroll_to_bottom();
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

    fn refresh_after_turn(&mut self, cx: &mut Context<Self>) {
        self.refresh_catalog(cx);
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_session_get_impl(state, &sid, AgentPageRequest::default()),
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                if let Ok(detail) = res {
                    this.audit_tool_calls = detail.tool_calls;
                    this.thread.todos = detail.session.todos.clone();
                    this.thread.context_used_tokens = detail.session.last_context_tokens;
                    this.thread.cost_micro_usd = detail.session.total_cost_micro_usd;
                    if !this.is_run_active() && detail.messages.len() >= this.thread.messages.len()
                    {
                        this.thread
                            .replace_snapshot(detail.session.title.clone(), detail.messages);
                    }
                    this.session = Some(detail.session);
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
        self.audit_tool_calls.clear();
        self.status_msg = None;
        self.submitting = false;
        self.pending_user_text = None;
        self.thinking_started_at = None;
        self.thinking_ended_at = None;
        self.collapsed_live_reasoning = false;
        self.pending_draft_sync = Some(self.drafts.get(&session_id).unwrap_or_default().to_owned());

        self.ensure_subscribed(&session_id, cx);

        let sid_load = session_id.clone();
        let sid_check = session_id;
        let task = spawn_blocking_into(
            cx,
            move |state| agent_session_get_impl(state, &sid_load, AgentPageRequest::default()),
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                if let Ok(detail) = res {
                    if !this.submitting && !this.is_run_active() {
                        this.thread
                            .replace_snapshot(detail.session.title.clone(), detail.messages);
                        if let Some(run) = detail.runs.first() {
                            this.thread.active_run = Some(run.clone());
                        }
                    } else if this.thread.title.is_empty() {
                        this.thread.title = detail.session.title.clone();
                    }
                    this.thread.todos = detail.session.todos.clone();
                    this.thread.context_used_tokens = detail.session.last_context_tokens;
                    this.thread.cost_micro_usd = detail.session.total_cost_micro_usd;
                    this.pending_instructions_sync =
                        Some(detail.session.instructions.clone().unwrap_or_default());
                    this.audit_tool_calls = detail.tool_calls;
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

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(draft) = self.pending_draft_sync.take() {
            self.composer
                .update(cx, |c, cx| c.set_value(draft, window, cx));
        }
        if let Some(inst) = self.pending_instructions_sync.take() {
            self.instructions_input
                .update(cx, |c, cx| c.set_value(inst, window, cx));
        }
        let p = palette(cx);
        let zh = settings_get_impl(runtime(cx).state()).general.language == Language::ZhCn;

        let center = v_col()
            .size_full()
            .min_w_0()
            .bg(to_hsla(p.surface))
            .child(thread::render_header(self, zh, cx))
            .child(thread::render_messages(self, zh, cx))
            .child(composer::render_dock(self, zh, cx));

        let mut split = h_resizable("chat-split-layout")
            .child(
                resizable_panel()
                    .size(px(254.0))
                    .size_range(px(200.0)..px(360.0))
                    .child(sidebar::render_sidebar(self, zh, cx)),
            )
            .child(resizable_panel().child(center));

        if self.details_open {
            split = split.child(
                resizable_panel()
                    .size(px(304.0))
                    .size_range(px(240.0)..px(420.0))
                    .child(inspector::render_inspector(self, zh, cx)),
            );
        }

        split
    }
}
