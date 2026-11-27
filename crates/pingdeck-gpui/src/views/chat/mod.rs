//! AI Chat & Coding Agent module (`NavModule::Chat`).

pub mod actions;
pub mod actions_extra;
pub mod actions_items;
pub mod composer;
pub mod composer_popovers;
pub mod gates;
pub mod gates_extra;
pub mod inspector;
pub mod inspector_tabs;
pub mod metrics;
pub mod modals;
pub mod reasoning;
pub mod run_actions;
pub mod run_graph;
pub mod run_graph_cards;
pub mod run_monitor;
pub mod run_monitor_detail;
pub mod run_node_inspector;
pub mod run_status;
pub mod run_steps;
pub mod run_topology;
pub mod run_topology_subagent;
pub mod session_sync;
pub mod sidebar;
pub mod sidebar_board;
pub mod thread;
pub mod thread_header;
pub mod trace;
pub mod types;
pub mod welcome;

use std::cell::Cell;
use std::collections::{HashMap, HashSet};
use std::time::Instant;

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, ScrollHandle,
    Styled as _, Subscription, Task, Window, div, px,
};
use pingdeck_desktop::core::agent::dto::{AgentBoard, AgentRunTopology};
use pingdeck_desktop::core::agent::model::{
    AgentRunStatus, AgentSession, AgentSessionSummary, AgentSkill, AgentTarget,
    AgentToolCallRecord, FileChange,
};
use pingdeck_desktop::core::agent::tools::process::BackgroundProcess;
use pingdeck_desktop::core::app::{EnvironmentDto, WorkspaceInfo};
use pingdeck_desktop::core::model::Workspace;
use pingdeck_desktop::persistence::ChatDraftsStore;
use pingdeck_desktop::state::{ChatThreadState, RunViewMode};

use crate::bridge::runtime;
use crate::ui::{
    InputEvent, InputState, TextareaState, Tone, h_resizable, palette, resizable_panel, to_hsla,
    v_col,
};

pub use types::{
    ChatHandoff, ChatModal, ChatSidebarMode, DetailsTab, ModelOption, MonitorFilter,
    PendingAttachment, ProviderChatMeta,
};

pub struct ChatView {
    pub sidebar_mode: ChatSidebarMode,
    pub sessions: Vec<AgentSessionSummary>,
    pub sessions_next_cursor: Option<String>,
    pub sessions_loading_more: bool,
    pub trash_sessions: Vec<AgentSessionSummary>,
    pub trash_next_cursor: Option<String>,
    pub trash_loading_more: bool,
    pub has_earlier_messages: bool,
    pub loading_earlier_messages: bool,
    pub board: Option<AgentBoard>,
    pub monitor_search_input: Entity<InputState>,
    pub monitor_filter: MonitorFilter,
    pub selected_monitor_run_id: Option<String>,
    pub selected_topology: Option<AgentRunTopology>,
    pub session_topology: Option<AgentRunTopology>,
    pub expanded_topology_nodes: HashSet<String>,
    pub monitor_view_overrides: HashMap<String, RunViewMode>,
    pub selected_graph_node_id: Option<String>,
    pub expanded_graph_groups: HashSet<String>,
    pub graph_zoom: f32,
    pub plan_gate_show_markdown: HashSet<String>,
    pub node_recovery_input: Entity<InputState>,
    pub search_input: Entity<InputState>,
    pub model_search_input: Entity<InputState>,
    pub question_other_input: Entity<InputState>,
    pub modal_input: Entity<InputState>,
    pub modal_textarea: Entity<TextareaState>,
    pub show_archived: bool,
    pub session: Option<AgentSession>,
    pub plan_mode: bool,
    pub audit_tool_calls: Vec<AgentToolCallRecord>,
    pub persisted_changes: Vec<FileChange>,
    pub processes: Vec<BackgroundProcess>,
    pub workspace_info: Option<WorkspaceInfo>,
    pub workspaces: Vec<Workspace>,
    pub environments: Vec<EnvironmentDto>,
    pub available_skills: Vec<AgentSkill>,
    pub pending_attachments: Vec<PendingAttachment>,
    pub selected_skills: Vec<String>,
    pub question_draft_choices: HashMap<String, Vec<u8>>,
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
    /// True while the thread viewport is pinned to the latest output; disengaged immediately when
    /// the user scrolls upward so reading earlier content never flickers or jumps back down.
    pub stick_to_bottom: bool,
    pub details_open: bool,
    pub details_tab: DetailsTab,
    pub active_modal: Option<ChatModal>,
    pub status_msg: Option<(Tone, String)>,
    pub pending_handoff: Cell<Option<ChatHandoff>>,
    pub(super) title_generated_sessions: HashSet<String>,
    pub(super) pending_draft_sync: Option<String>,
    pub(super) pending_instructions_sync: Option<String>,
    pub(super) active_sub_id: Option<u64>,
    pub(super) subscribed_session_id: Option<String>,
    pub(super) stream_task: Option<Task<()>>,
    pub(super) attention_sub_id: Option<u64>,
    pub(super) attention_task: Option<Task<()>>,
    pub(super) tick_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl ChatView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let rt = runtime(cx);
        let drafts = rt.load_chat_drafts();
        let ch = crate::ui::tr(cx).chat();
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder(ch.ph_search()));
        let monitor_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(ch.monitor_search_ph()));
        let node_recovery_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(ch.graph_node_recovery_guidance_ph())
        });
        let model_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(ch.ph_filter_models()));
        let question_other_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(ch.ph_custom_answer()));
        let modal_input = cx.new(|cx| InputState::new(window, cx).placeholder(ch.ph_title()));
        let modal_textarea = cx.new(|cx| TextareaState::new(window, cx).rows(5));
        let composer = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(ch.ph_composer())
                .rows(3)
                .submit_on_enter(true)
        });
        let instructions_input = cx.new(|cx| {
            TextareaState::new(window, cx)
                .placeholder(ch.ph_instructions())
                .rows(4)
        });

        let enter_sub = cx.subscribe_in(&composer, window, |this, _, ev, window, cx| match ev {
            InputEvent::PressEnter {
                secondary: true, ..
            } => {
                if this.is_run_active() {
                    this.steer_active_run(window, cx);
                } else {
                    this.send_message(window, cx);
                }
            }
            InputEvent::PressEnter { shift: false, .. } => {
                this.send_message(window, cx);
            }
            _ => {}
        });
        let search_sub = cx.subscribe(&search_input, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change | InputEvent::PressEnter { .. }) {
                this.refresh_catalog(cx);
            }
        });
        let monitor_search_sub =
            cx.subscribe(&monitor_search_input, |_, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify();
                }
            });
        let model_search_sub = cx.subscribe(&model_search_input, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });

        let mut view = Self {
            sidebar_mode: ChatSidebarMode::Time,
            sessions: Vec::new(),
            sessions_next_cursor: None,
            sessions_loading_more: false,
            trash_sessions: Vec::new(),
            trash_next_cursor: None,
            trash_loading_more: false,
            has_earlier_messages: false,
            loading_earlier_messages: false,
            board: None,
            monitor_search_input,
            monitor_filter: MonitorFilter::All,
            selected_monitor_run_id: None,
            selected_topology: None,
            session_topology: None,
            expanded_topology_nodes: HashSet::new(),
            monitor_view_overrides: HashMap::new(),
            selected_graph_node_id: None,
            expanded_graph_groups: HashSet::new(),
            graph_zoom: 1.0,
            plan_gate_show_markdown: HashSet::new(),
            node_recovery_input,
            search_input,
            model_search_input,
            question_other_input,
            modal_input,
            modal_textarea,
            show_archived: false,
            session: None,
            plan_mode: false,
            audit_tool_calls: Vec::new(),
            persisted_changes: Vec::new(),
            processes: Vec::new(),
            workspace_info: None,
            workspaces: Vec::new(),
            environments: Vec::new(),
            available_skills: Vec::new(),
            pending_attachments: Vec::new(),
            selected_skills: Vec::new(),
            question_draft_choices: HashMap::new(),
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
            stick_to_bottom: true,
            details_open: false,
            details_tab: DetailsTab::Progress,
            active_modal: None,
            status_msg: None,
            pending_handoff: Cell::new(None),
            title_generated_sessions: HashSet::new(),
            pending_draft_sync: None,
            pending_instructions_sync: None,
            active_sub_id: None,
            subscribed_session_id: None,
            stream_task: None,
            attention_sub_id: None,
            attention_task: None,
            tick_task: None,
            _subscriptions: vec![enter_sub, search_sub, monitor_search_sub, model_search_sub],
            _tasks: Vec::new(),
        };
        view.subscribe_attention(cx);
        view.reload_sessions(cx);
        view
    }

    /// Distance in pixels between the current scroll offset and the bottom of the thread.
    #[must_use]
    pub fn distance_from_bottom(&self) -> f32 {
        let max_y = self.scroll_handle.max_offset().y.as_f32().max(0.0);
        if max_y <= 0.5 {
            return 0.0;
        }
        let cur_y = self.scroll_handle.offset().y.as_f32();
        (max_y + cur_y).max(0.0)
    }

    /// Scrolls to bottom only when the viewport is actively following live output.
    /// If the user dragged the scrollbar thumb away from the bottom, respects that position.
    pub(super) fn auto_scroll_if_following(&mut self) {
        if !self.stick_to_bottom {
            return;
        }
        if self.distance_from_bottom() > 64.0 {
            self.stick_to_bottom = false;
            return;
        }
        self.scroll_handle.scroll_to_bottom();
    }

    pub fn has_any_active_board_run(&self) -> bool {
        self.board.as_ref().is_some_and(|b| {
            !b.running.is_empty() || b.needs_you.iter().any(|r| r.run.status.is_active())
        })
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

    #[must_use]
    pub fn take_pending_handoff(&self) -> Option<ChatHandoff> {
        self.pending_handoff.take()
    }

    pub fn dismiss_modal_if_open(&mut self, cx: &mut Context<Self>) -> bool {
        if self.active_modal.take().is_some() {
            cx.notify();
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn pending_attention_count(&self) -> usize {
        self.board.as_ref().map_or_else(
            || {
                self.thread.pending_approvals.len()
                    + self.thread.open_gates.len()
                    + usize::from(self.thread.cost_pause.is_some())
            },
            |b| b.needs_you.len(),
        )
    }

    #[must_use]
    pub fn format_provider_model(&self, provider_id: &str, model_id: &str) -> String {
        let pname = self
            .provider_meta
            .get(provider_id)
            .map(|m| m.name.as_str())
            .unwrap_or(provider_id);
        format!("{pname} / {model_id}")
    }

    #[must_use]
    pub fn format_target_label(&self, target: &AgentTarget) -> String {
        match target {
            AgentTarget::Pinned {
                provider_id, model, ..
            } => self.format_provider_model(provider_id, model),
            AgentTarget::Named { name } => self.format_raw_model_label(name),
        }
    }

    #[must_use]
    pub fn format_raw_model_label(&self, raw: &str) -> String {
        if let Some((pid, mid)) = raw.split_once('/') {
            self.format_provider_model(pid, mid)
        } else {
            raw.to_owned()
        }
    }

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

    #[must_use]
    pub fn active_provider_missing_reasoning_quirk(&self) -> bool {
        self.provider_meta_for_raw(None)
            .is_some_and(ProviderChatMeta::missing_reasoning_quirk)
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

        let monitor_open = self.sidebar_mode == ChatSidebarMode::Board;
        let center: gpui_kit::AnyElement = if monitor_open {
            run_monitor::render_run_monitor(self, cx)
        } else {
            v_col()
                .size_full()
                .min_w_0()
                .bg(to_hsla(p.surface))
                .child(thread_header::render_header(self, cx))
                .child(thread::render_messages(self, cx))
                .child(composer::render_dock(self, cx))
                .into_any_element()
        };

        let mut split = h_resizable("chat-split-layout")
            .child(
                resizable_panel()
                    .size(px(260.0))
                    .size_range(px(210.0)..px(360.0))
                    .child(sidebar::render_sidebar(self, cx)),
            )
            .child(resizable_panel().child(center));

        if self.details_open && !monitor_open {
            split = split.child(
                resizable_panel()
                    .size(px(316.0))
                    .size_range(px(250.0)..px(440.0))
                    .child(inspector::render_inspector(self, cx)),
            );
        }

        let mut root = div().relative().size_full().child(split);
        if let Some(modal) = self.active_modal.clone() {
            root = root.child(modals::render_active_modal(self, modal, window, cx));
        }
        root
    }
}
