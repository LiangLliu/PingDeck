//! AI Chat & Coding Agent module (`NavModule::Chat`).

pub mod actions;
pub mod actions_extra;
pub mod actions_items;
pub mod approval_card;
pub mod composer;
pub mod composer_popovers;
pub mod context_meter;
pub mod gates;
pub mod gates_extra;
pub mod live_turn;
pub mod media;
pub mod metrics;
pub mod modals;
pub mod reasoning;
pub mod review_actions;
pub mod review_panel;
pub mod run_actions;
pub mod run_topology_subagent;
pub mod session_sync;
pub mod sidebar;
pub mod sidebar_board;
pub mod stream_sync;
pub mod thread;
pub mod thread_header;
pub mod thread_list;
pub mod todos;
pub mod trace;
pub mod trace_detail;
pub mod types;
pub mod welcome;

use std::cell::{Cell, RefCell};
use std::collections::{HashMap, HashSet};
use std::sync::Arc;
use std::time::Instant;

use crate::bridge::runtime;
use crate::ui::{
    InputEvent, InputState, TextareaState, Tone, h_resizable, palette, resizable_panel, to_hsla,
    v_col,
};
use gpui_kit::{
    AppContext as _, Context, Entity, FocusHandle, Focusable as _, FollowMode,
    InteractiveElement as _, IntoElement, KeyDownEvent, ListAlignment, ListState,
    ParentElement as _, Render, Styled as _, Subscription, Task, Window, div, px,
};
use spanbay_desktop::core::agent::dto::AgentBoard;
use spanbay_desktop::core::agent::model::{
    AgentAttachment, AgentSession, AgentSessionSummary, AgentSkill, AgentToolCallRecord,
    CurrencyCost, FileChange, HumanGateBody, SessionSearchHit,
};
use spanbay_desktop::core::agent::tools::process::BackgroundProcess;
use spanbay_desktop::core::app::{ContextBreakdown, EnvironmentDto, WorkspaceInfo};
use spanbay_desktop::core::model::Workspace;
use spanbay_desktop::persistence::ChatDraftsStore;
use spanbay_desktop::state::ChatThreadState;
use spanbay_desktop::streams::AgentStreamSink;

pub use types::{
    ChatHandoff, ChatModal, ChatSidebarMode, DiffCommentChip, ImageViewerSource,
    LastApprovalNotice, ModelOption, ProviderChatMeta, ReviewDiffMode, ReviewScope,
    ReviewToolOutput, TranscriptDensity,
};
pub struct ChatView {
    pub sidebar_mode: ChatSidebarMode,
    pub density: TranscriptDensity,
    pub review_open: bool,
    pub sidebar_collapsed: bool,
    pub review_panel_width: f32,
    pub review_scope: ReviewScope,
    pub review_diff_mode: ReviewDiffMode,
    pub review_selected_path: Option<String>,
    pub review_selected_change_id: Option<String>,
    pub review_diff_cache: Option<review_actions::CachedReviewDiff>,
    pub review_diff_error: Option<(String, String)>,
    pub review_tool_output: Option<ReviewToolOutput>,
    pub review_undo_precheck: bool,
    pub review_comment_target: Option<(String, usize)>,
    pub review_comment_input: Entity<InputState>,
    pub diff_comments: Vec<DiffCommentChip>,
    pub editing_queue_id: Option<String>,
    pub sessions: Vec<AgentSessionSummary>,
    pub sessions_next_cursor: Option<String>,
    pub sessions_loading_more: bool,
    pub trash_sessions: Vec<AgentSessionSummary>,
    pub trash_next_cursor: Option<String>,
    pub trash_loading_more: bool,
    pub has_earlier_messages: bool,
    pub has_later_messages: bool,
    pub loaded_earlier_once: bool,
    pub loading_earlier_messages: bool,
    pub loading_later_messages: bool,
    pub(super) load_later_failed: bool,
    pub(super) catalog_filter_epoch: u64,
    pub turn_inputs_version: u64,
    pub(super) tool_output_loading_more: bool,
    pub(super) last_expanded_reasoning: HashSet<String>,
    pub(super) pending_placeholder_reset: bool,
    pub board: Option<AgentBoard>,
    pub latest_session_run_id: Option<String>,
    pub plan_gate_show_markdown: HashSet<String>,
    pub search_input: Entity<InputState>,
    pub model_search_input: Entity<InputState>,
    pub question_other_input: Entity<InputState>,
    pub question_other_inputs: HashMap<String, Entity<InputState>>,
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
    pub active_workspace_id: Option<String>,
    pub active_environment_id: Option<String>,
    pub available_skills: Vec<AgentSkill>,
    pub pending_attachments: Vec<AgentAttachment>,
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
    /// Virtualized thread rows; follows the tail until the user scrolls up (gpui built-in).
    pub list_state: ListState,
    pub(super) list_keys: Vec<thread_list::ThreadItemKey>,
    /// Full-text hits for the sidebar query (message bodies, not only titles).
    pub search_hits: Vec<SessionSearchHit>,
    pub(super) context_breakdown: Option<ContextBreakdown>,
    /// Approval card whose deny-reason field is open.
    pub(super) deny_reason_call: Option<String>,
    pub(super) deny_reason_input: Entity<InputState>,
    /// Focus of the first waiting approval card (Esc there denies, design §7).
    pub approval_focus: FocusHandle,
    /// Focus of the first waiting plan/question gate card (Esc / digits / Enter, design §7).
    pub gate_focus: FocusHandle,
    /// True after "Keep planning" was clicked until the plan gate closes or a revision is sent.
    pub plan_revising: bool,
    /// Result banner left behind in the dock after answering an approval card.
    pub last_approval_notice: Option<LastApprovalNotice>,
    /// Gate/approval id that last auto-focused so re-renders never steal focus back.
    pub(super) last_focused_gate_id: Option<String>,
    /// Message id to scroll to once an `open_session_at_message` snapshot lands.
    pub pending_scroll_message_id: Option<String>,
    pub active_modal: Option<ChatModal>,
    pub status_msg: Option<(Tone, String)>,
    pub pending_handoff: Cell<Option<ChatHandoff>>,
    pub(super) title_generated_sessions: HashSet<String>,
    pub(super) pending_draft_sync: Option<String>,
    pub(super) pending_instructions_sync: Option<String>,
    pub(super) active_sub_id: Option<u64>,
    pub(super) active_stream_sink: Option<Arc<AgentStreamSink>>,
    pub(super) subscribed_session_id: Option<String>,
    pub(super) stream_task: Option<Task<()>>,
    /// Pending resubscribe after a lost or failed subscription (1 s / 3 s / 9 s, then 60 s).
    pub(super) resubscribe_task: Option<Task<()>>,
    /// Bumped on every (re)subscribe so late answers for an older attempt are ignored.
    pub(super) stream_epoch: u64,
    pub(super) subscribe_attempts: usize,
    /// The open chat's cost per currency (never summed across currencies).
    pub(super) session_costs: Vec<CurrencyCost>,
    /// Decoded images by key (attachments, tool-result images); `None` while fetching or when
    /// the bytes do not decode. Filled while rendering, hence the cell.
    pub(super) media_cache: RefCell<HashMap<String, Option<media::CachedMedia>>>,
    /// The todo panel above the StatusLine is expanded.
    pub todos_open: bool,
    pub(super) error_details_open: bool,
    pub(super) attention_sub_id: Option<u64>,
    pub(super) attention_task: Option<Task<()>>,
    pub(super) tick_task: Option<Task<()>>,
    pub(super) _subscriptions: Vec<Subscription>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl ChatView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let rt = runtime(cx);
        let drafts = rt.load_chat_drafts();
        let ch = crate::ui::tr(cx).chat();
        let search_input = cx.new(|cx| InputState::new(window, cx).placeholder(ch.ph_search()));
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
                .rows(5)
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
        let model_search_sub = cx.subscribe(&model_search_input, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        let question_other_sub = cx.subscribe_in(
            &question_other_input,
            window,
            |this, _, ev: &InputEvent, window, cx| match ev {
                InputEvent::PressEnter { .. } => {
                    let first_q = this
                        .thread
                        .open_gates
                        .iter()
                        .find(|(_, g)| matches!(g.body, HumanGateBody::Question { .. }))
                        .map(|(rid, g)| (rid.clone(), g.call_id.clone()));
                    if let Some((run_id, call_id)) = first_q {
                        this.answer_question(run_id, call_id, false, window, cx);
                    }
                }
                InputEvent::Change => {
                    let non_empty = !this.question_other_input.read(cx).value().trim().is_empty();
                    if non_empty
                        && let Some((_, gate)) = this
                            .thread
                            .open_gates
                            .iter()
                            .find(|(_, g)| matches!(g.body, HumanGateBody::Question { .. }))
                        && matches!(gate.body, HumanGateBody::Question { multi: false, .. })
                    {
                        this.question_draft_choices.remove(&gate.call_id);
                        cx.notify();
                    }
                }
                _ => {}
            },
        );
        let deny_reason_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(ch.approval_deny_reason_ph()));
        let deny_sub = cx.subscribe(&deny_reason_input, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                this.submit_deny(cx);
            }
        });
        let review_comment_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(ch.rv_comment_ph()));
        let rv_comment_sub = cx.subscribe(&review_comment_input, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::PressEnter { .. }) {
                this.submit_review_comment(cx);
            }
        });
        let initial_rv_w = rt
            .load_ui_state()
            .pane_widths
            .get("chat.review_panel")
            .copied()
            .unwrap_or(460.0)
            .clamp(340.0, 760.0);
        let list_state = ListState::new(0, ListAlignment::Bottom, px(800.0));
        list_state.set_follow_mode(FollowMode::Tail);

        let mut view = Self {
            sidebar_mode: ChatSidebarMode::Time,
            density: TranscriptDensity::Normal,
            review_open: false,
            sidebar_collapsed: false,
            review_panel_width: initial_rv_w,
            review_scope: ReviewScope::Turn,
            review_diff_mode: ReviewDiffMode::Net,
            review_selected_path: None,
            review_selected_change_id: None,
            review_diff_cache: None,
            review_diff_error: None,
            review_tool_output: None,
            review_undo_precheck: false,
            review_comment_target: None,
            review_comment_input,
            diff_comments: Vec::new(),
            editing_queue_id: None,
            sessions: Vec::new(),
            sessions_next_cursor: None,
            sessions_loading_more: false,
            trash_sessions: Vec::new(),
            trash_next_cursor: None,
            trash_loading_more: false,
            has_earlier_messages: false,
            has_later_messages: false,
            loaded_earlier_once: false,
            loading_earlier_messages: false,
            loading_later_messages: false,
            load_later_failed: false,
            catalog_filter_epoch: 0,
            turn_inputs_version: 0,
            tool_output_loading_more: false,
            last_expanded_reasoning: HashSet::new(),
            pending_placeholder_reset: false,
            board: None,
            latest_session_run_id: None,
            plan_gate_show_markdown: HashSet::new(),
            search_input,
            model_search_input,
            question_other_input,
            question_other_inputs: HashMap::new(),
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
            active_workspace_id: None,
            active_environment_id: None,
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
            list_state,
            list_keys: Vec::new(),
            search_hits: Vec::new(),
            context_breakdown: None,
            deny_reason_call: None,
            deny_reason_input,
            approval_focus: cx.focus_handle(),
            gate_focus: cx.focus_handle(),
            plan_revising: false,
            last_approval_notice: None,
            last_focused_gate_id: None,
            pending_scroll_message_id: None,
            active_modal: None,
            status_msg: None,
            pending_handoff: Cell::new(None),
            title_generated_sessions: HashSet::new(),
            pending_draft_sync: None,
            pending_instructions_sync: None,
            active_sub_id: None,
            active_stream_sink: None,
            subscribed_session_id: None,
            stream_task: None,
            resubscribe_task: None,
            stream_epoch: 0,
            subscribe_attempts: 0,
            session_costs: Vec::new(),
            media_cache: RefCell::new(HashMap::new()),
            todos_open: false,
            error_details_open: false,
            attention_sub_id: None,
            attention_task: None,
            tick_task: None,
            _subscriptions: vec![
                enter_sub,
                search_sub,
                model_search_sub,
                question_other_sub,
                deny_sub,
                rv_comment_sub,
            ],
            _tasks: Vec::new(),
        };
        view.subscribe_attention(cx);
        view.reload_sessions(cx);
        view
    }

    pub fn is_run_active(&self) -> bool {
        self.submitting
            || self
                .thread
                .active_run
                .as_ref()
                .is_some_and(|r| r.status.is_active())
            || !self.thread.streaming_text.is_empty()
            || !self.thread.streaming_reasoning.is_empty()
            || !self.thread.pending_tool_calls.is_empty()
            || !self.thread.pending_approvals.is_empty()
            || !self.thread.open_gates.is_empty()
            || self.thread.cost_pause.is_some()
    }

    #[must_use]
    pub fn take_pending_handoff(&self) -> Option<ChatHandoff> {
        self.pending_handoff.take()
    }

    /// Persists the currently open chat session's composer draft before application exit.
    pub fn flush_active_draft(&mut self, cx: &gpui_kit::App) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let text = self
            .pending_draft_sync
            .clone()
            .unwrap_or_else(|| self.composer.read(cx).value().to_string());
        self.drafts.set(&sid, text);
        runtime(cx).save_chat_drafts(&self.drafts);
    }

    pub fn dismiss_modal_if_open(&mut self, cx: &mut Context<Self>) -> bool {
        if self.active_modal.take().is_some() {
            cx.notify();
            true
        } else {
            false
        }
    }
}

/// Fraction of the chat pane width used by thread content, banners, welcome and the dock.
const CHAT_COLUMN_FRACTION: f32 = 0.92;

/// Centered percentage-width column shared by thread rows, banners, welcome and dock.
pub(crate) fn chat_column(content: gpui_kit::Div) -> gpui_kit::Div {
    div().w_full().child(
        content
            .w(gpui_kit::relative(CHAT_COLUMN_FRACTION))
            .mx_auto(),
    )
}

impl Render for ChatView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if self.pending_placeholder_reset {
            self.pending_placeholder_reset = false;
            self.reset_composer_placeholder(window, cx);
        }
        if let Some(draft) = self.pending_draft_sync.take() {
            self.composer
                .update(cx, |c, cx| c.set_value(draft, window, cx));
        }
        if let Some(inst) = self.pending_instructions_sync.take() {
            self.instructions_input
                .update(cx, |c, cx| c.set_value(inst, window, cx));
        }
        // Auto-focus a newly arrived gate/approval card when the composer is empty or nothing
        // else holds focus (design §7), unless a modal dialog is open.
        let first_gate = if self.active_modal.is_some() {
            None
        } else {
            self.thread
                .open_gates
                .first()
                .map(|(_, g)| (g.id.clone(), self.gate_focus.clone()))
                .or_else(|| {
                    self.thread
                        .pending_approvals
                        .first()
                        .map(|a| (a.call_id.clone(), self.approval_focus.clone()))
                })
        };
        match first_gate {
            Some((id, handle)) if self.last_focused_gate_id.as_deref() != Some(id.as_str()) => {
                let composer_empty = self.composer.read(cx).value().trim().is_empty();
                let composer_focused = self.composer.read(cx).focus_handle(cx).is_focused(window);
                if window.focused(cx).is_none() || (composer_focused && composer_empty) {
                    window.focus(&handle, cx);
                }
                self.last_focused_gate_id = Some(id);
            }
            None if self.active_modal.is_none() => self.last_focused_gate_id = None,
            _ => {}
        }
        self.sync_question_inputs(window, cx);
        if self._tasks.len() > 64 {
            let excess = self._tasks.len() - 64;
            self._tasks.drain(..excess);
        }
        let p = palette(cx);
        // Cap the stacked gates/queue/todos so the composer below always stays fully visible.
        let dock_stack_max_h = window.viewport_size().height * 0.45;
        let center = v_col()
            .size_full()
            .min_w_0()
            .bg(to_hsla(p.surface))
            .child(thread_header::render_header(self, cx))
            .child(thread_list::render_messages(self, cx))
            .child(composer::render_dock(self, dock_stack_max_h, cx));

        let win_w = f32::from(window.viewport_size().width).max(960.0);
        let max_rv_w = (win_w - 560.0).clamp(360.0, 760.0);
        let rv_w = if self.review_panel_width > 0.0 {
            self.review_panel_width.clamp(340.0, max_rv_w)
        } else {
            (win_w * 0.40).clamp(360.0, max_rv_w)
        };
        let mut split = h_resizable("chat-split-layout");
        if !self.sidebar_collapsed {
            split = split.child(
                resizable_panel()
                    .size(px(248.0))
                    .size_range(px(200.0)..px(340.0))
                    .child(sidebar::render_sidebar(self, cx)),
            );
        }
        split = split.child(
            resizable_panel()
                .size_range(px(560.0)..px(4000.0))
                .child(center),
        );
        if self.review_open {
            split = split.child(
                resizable_panel()
                    .size(px(rv_w))
                    .size_range(px(340.0)..px(max_rv_w))
                    .child(review_panel::render_review_panel(self, cx)),
            );
        }

        let mut root = div()
            .relative()
            .size_full()
            .on_key_down(cx.listener(|this, ev: &KeyDownEvent, _, cx| {
                let ks = &ev.keystroke;
                if ks.modifiers.control && !ks.modifiers.shift && ks.key.eq_ignore_ascii_case("o") {
                    this.cycle_density(cx);
                } else if (ks.modifiers.platform || ks.modifiers.control)
                    && ks.modifiers.shift
                    && ks.key.eq_ignore_ascii_case("d")
                {
                    this.toggle_review(cx);
                }
            }))
            .child(split);
        if let Some(modal) = self.active_modal.clone() {
            root = root.child(modals::render_active_modal(self, modal, window, cx));
        }
        root
    }
}
