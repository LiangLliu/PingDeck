//! Global 90% viewport Workflow Monitor modal (`WorkflowMonitorModal`) owned by `AppShell` and
//! shared identically across `ChatView` (header Workflow icon / historical turn pills / changes
//! pill) and `RunsView` (clicking any run in the global Run Monitor list).

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use gpui_kit::{
    AnyElement, App, AppContext as _, ClickEvent, Context, Entity, InteractiveElement as _,
    IntoElement, ParentElement as _, Render, Styled as _, Task, Window, div, px,
};
use pingdeck_desktop::core::agent::dto::{AgentRunTopology, SessionRunHistoryItem};
use pingdeck_desktop::core::agent::model::FileChange;
use pingdeck_desktop::core::agent::tools::process::{BackgroundProcess, ProcessState};
use pingdeck_desktop::state::{
    AlignmentGuide, CrossAlignment, LayoutDirection, LayoutPolicy, RunViewMode, is_graph_available,
    select_default_view_mode,
};

use crate::ui::{
    InputState, PayloadViewer, ScrollableElement as _, Tone, btn_secondary, empty_inline,
    h_resizable, modal_dialog, palette, resizable_panel, to_hsla, v_col, viewport_modal,
};

use super::changes::render_changes_tab;
use super::graph::{build_modal_graph_layout, render_execution_graph};
use super::modal_chrome::{
    render_modal_footer, render_modal_telemetry, render_modal_view_bar, render_processes_tray,
};
use super::modal_header::render_modal_header;
use super::node_inspector::render_node_inspector;
use super::steps::render_steps_view;
use super::timeline::render_timeline_view;
use super::types::{GraphDragState, HistoryPickerFilter, WorkflowModalHandoff, WorkflowTab};

/// Reusable 90% viewport overlay displaying one session/run's Execution Graph, Steps, Timeline,
/// File Changes Diff workbench, background processes, and deep 7-archetype Node Inspector.
pub struct WorkflowMonitorModal {
    pub open: bool,
    pub session_id: String,
    pub run_id: Option<String>,
    pub topology: Option<AgentRunTopology>,
    pub session_history: Vec<SessionRunHistoryItem>,
    pub history_filter: HistoryPickerFilter,
    pub changes: Vec<FileChange>,
    pub selected_change_id: Option<String>,
    pub selected_change_diff: Option<(String, String, bool)>,
    pub processes: Vec<BackgroundProcess>,
    pub show_processes: bool,
    pub tab_overrides: HashMap<String, WorkflowTab>,
    pub pending_initial_tab: Option<WorkflowTab>,
    pub selected_node_id: Option<String>,
    pub user_cleared_selection: bool,
    pub expanded_groups: HashSet<String>,
    pub expanded_rounds: HashSet<String>,
    pub expanded_sections: HashSet<String>,
    pub graph_zoom: f32,
    pub layout_policy: LayoutPolicy,
    pub pinned_positions: HashMap<String, (f32, f32)>,
    pub drag_state: Option<GraphDragState>,
    pub alignment_guides: Vec<AlignmentGuide>,
    pub drag_suppress_click: bool,
    pub node_recovery_input: Entity<InputState>,
    pub full_output_dialog: Option<(String, String)>,
    pub full_output_viewer: Entity<PayloadViewer>,
    pub full_output_sig: Option<(String, usize)>,
    pub status_msg: Option<(Tone, String)>,
    pub pending_handoff: Cell<Option<WorkflowModalHandoff>>,
    pub(super) tick_task: Option<Task<()>>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl WorkflowMonitorModal {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ch = crate::ui::tr(cx).chat();
        let node_recovery_input = cx.new(|cx| {
            InputState::new(window, cx).placeholder(ch.graph_node_recovery_guidance_ph())
        });
        let full_output_viewer = cx.new(|cx| PayloadViewer::new("wf-full-out", true, window, cx));
        Self {
            open: false,
            session_id: String::new(),
            run_id: None,
            topology: None,
            session_history: Vec::new(),
            history_filter: HistoryPickerFilter::All,
            changes: Vec::new(),
            selected_change_id: None,
            selected_change_diff: None,
            processes: Vec::new(),
            show_processes: false,
            tab_overrides: HashMap::new(),
            pending_initial_tab: None,
            selected_node_id: None,
            user_cleared_selection: false,
            expanded_groups: HashSet::new(),
            expanded_rounds: HashSet::new(),
            expanded_sections: HashSet::new(),
            graph_zoom: 1.0,
            layout_policy: LayoutPolicy::default(),
            pinned_positions: HashMap::new(),
            drag_state: None,
            alignment_guides: Vec::new(),
            drag_suppress_click: false,
            node_recovery_input,
            full_output_dialog: None,
            full_output_viewer,
            full_output_sig: None,
            status_msg: None,
            pending_handoff: Cell::new(None),
            tick_task: None,
            _tasks: Vec::new(),
        }
    }

    #[must_use]
    pub fn is_open(&self) -> bool {
        self.open
    }

    #[must_use]
    pub fn has_pending_signal(&self) -> bool {
        let val = self.pending_handoff.take();
        let has = val.is_some();
        self.pending_handoff.set(val);
        has
    }

    pub fn take_pending_handoff(&self) -> Option<WorkflowModalHandoff> {
        self.pending_handoff.take()
    }

    /// Opens the 90% Workflow Monitor modal scoped to `session_id` (optionally focusing a specific
    /// historical `focus_run_id` and/or opening directly on `initial_tab`, e.g. `Changes`).
    pub fn open_for_session(
        &mut self,
        session_id: String,
        focus_run_id: Option<String>,
        initial_tab: Option<WorkflowTab>,
        cx: &mut Context<Self>,
    ) {
        let session_changed = self.session_id != session_id;
        let run_changed = focus_run_id.is_some() && self.run_id != focus_run_id;
        self.open = true;
        self.session_id = session_id;
        self.full_output_dialog = None;
        self.status_msg = None;
        if session_changed || run_changed {
            self.run_id = focus_run_id;
            self.topology = None;
            self.selected_node_id = None;
            self.user_cleared_selection = false;
            self.expanded_sections.clear();
            self.pinned_positions.clear();
            self.drag_state = None;
            self.alignment_guides.clear();
        }
        if session_changed {
            self.session_history.clear();
            self.changes.clear();
            self.selected_change_id = None;
            self.selected_change_diff = None;
            self.processes.clear();
            self.show_processes = false;
        }
        self.pending_initial_tab = initial_tab;
        if let (Some(tab), Some(rid)) = (initial_tab, self.run_id.clone()) {
            self.tab_overrides.insert(rid, tab);
        }
        self.reload_all(cx);
        cx.notify();
    }

    /// Closes the 90% modal and halts its background ticker cleanly.
    pub fn close(&mut self, cx: &mut Context<Self>) {
        self.open = false;
        self.full_output_dialog = None;
        self.tick_task = None;
        cx.notify();
    }

    /// Layered `Esc` dismissal: closes an inner full-output sub-dialog first, otherwise closes
    /// the 90% modal itself. Returns `true` if something was dismissed.
    pub fn dismiss_inner_or_close(&mut self, cx: &mut Context<Self>) -> bool {
        if !self.open {
            return false;
        }
        if self.full_output_dialog.take().is_some() {
            cx.notify();
            return true;
        }
        self.close(cx);
        true
    }

    pub fn reload_all(&mut self, cx: &mut Context<Self>) {
        if self.session_id.is_empty() {
            return;
        }
        self.load_topology(cx);
        self.load_session_history(cx);
        self.load_changes(cx);
        self.load_processes(cx);
    }

    pub fn select_history_run(&mut self, run_id: String, cx: &mut Context<Self>) {
        if self.run_id.as_deref() == Some(run_id.as_str())
            && self
                .topology
                .as_ref()
                .is_some_and(|t| t.run.run_id == run_id)
        {
            return;
        }
        self.run_id = Some(run_id);
        self.selected_node_id = None;
        self.user_cleared_selection = false;
        self.expanded_sections.clear();
        self.pinned_positions.clear();
        self.drag_state = None;
        self.alignment_guides.clear();
        self.load_topology(cx);
        cx.notify();
    }

    /// Steps backward (`delta = +1`, older) or forward (`delta = -1`, newer) in session history.
    pub fn step_history(&mut self, delta: isize, cx: &mut Context<Self>) {
        let Some(cur_id) = self
            .run_id
            .as_deref()
            .or_else(|| self.topology.as_ref().map(|t| t.run.run_id.as_str()))
        else {
            return;
        };
        let Some(idx) = self
            .session_history
            .iter()
            .position(|r| r.run.run_id == cur_id)
        else {
            return;
        };
        let next_idx = idx as isize + delta;
        if next_idx >= 0
            && (next_idx as usize) < self.session_history.len()
            && let Some(target) = self.session_history.get(next_idx as usize)
        {
            let next_id = target.run.run_id.clone();
            self.select_history_run(next_id, cx);
        }
    }

    #[must_use]
    pub fn effective_tab(&self, topo: &AgentRunTopology) -> (WorkflowTab, bool) {
        if let Some(&manual) = self.tab_overrides.get(&topo.run.run_id) {
            if manual == WorkflowTab::Graph && !is_graph_available(topo) {
                return (WorkflowTab::Steps, true);
            }
            return (manual, false);
        }
        let tab = match select_default_view_mode(topo) {
            RunViewMode::Steps => WorkflowTab::Steps,
            RunViewMode::Graph => WorkflowTab::Graph,
            RunViewMode::Timeline => WorkflowTab::Timeline,
        };
        (tab, true)
    }

    pub fn set_tab(&mut self, run_id: String, tab: WorkflowTab, cx: &mut Context<Self>) {
        self.tab_overrides.insert(run_id, tab);
        if tab == WorkflowTab::Changes
            && self.selected_change_id.is_none()
            && let Some(first) = self.changes.first().map(|c| c.id.clone())
        {
            self.select_change(first, cx);
        }
        cx.notify();
    }

    pub fn select_node(&mut self, node_id: Option<String>, cx: &mut Context<Self>) {
        self.user_cleared_selection = node_id.is_none();
        self.selected_node_id = node_id;
        cx.notify();
    }

    pub fn toggle_group(&mut self, group_id: String, cx: &mut Context<Self>) {
        if !self.expanded_groups.remove(&group_id) {
            self.expanded_groups.insert(group_id);
        }
        cx.notify();
    }

    pub fn toggle_all_groups(&mut self, ids: &[String], cx: &mut Context<Self>) {
        let all_open = ids.iter().all(|id| self.expanded_groups.contains(id));
        if all_open {
            for id in ids {
                self.expanded_groups.remove(id);
            }
        } else {
            for id in ids {
                self.expanded_groups.insert(id.clone());
            }
        }
        cx.notify();
    }

    pub fn toggle_round(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.expanded_rounds.remove(&key) {
            self.expanded_rounds.insert(key);
        }
        cx.notify();
    }

    pub fn toggle_section(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.expanded_sections.remove(&key) {
            self.expanded_sections.insert(key);
        }
        cx.notify();
    }

    pub fn set_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        self.graph_zoom = zoom.clamp(0.75, 1.15);
        cx.notify();
    }

    pub fn set_layout_direction(&mut self, direction: LayoutDirection, cx: &mut Context<Self>) {
        if self.layout_policy.direction != direction {
            self.layout_policy.direction = direction;
            if direction.is_serpentine() {
                self.layout_policy.swimlanes = false;
            }
            self.pinned_positions.clear();
            self.drag_state = None;
            self.alignment_guides.clear();
            cx.notify();
        }
    }

    pub fn toggle_alignment(&mut self, cx: &mut Context<Self>) {
        self.layout_policy.alignment = match self.layout_policy.alignment {
            CrossAlignment::Start => CrossAlignment::Center,
            CrossAlignment::Center | CrossAlignment::Barycenter => CrossAlignment::Start,
        };
        cx.notify();
    }

    pub fn toggle_swimlanes(&mut self, cx: &mut Context<Self>) {
        self.layout_policy.swimlanes = !self.layout_policy.swimlanes;
        if self.layout_policy.swimlanes && self.layout_policy.direction.is_serpentine() {
            self.layout_policy.direction = LayoutDirection::LeftToRight;
        }
        self.pinned_positions.clear();
        self.drag_state = None;
        self.alignment_guides.clear();
        cx.notify();
    }

    pub fn reset_pinned_layout(&mut self, cx: &mut Context<Self>) {
        self.pinned_positions.clear();
        self.drag_state = None;
        self.alignment_guides.clear();
        cx.notify();
    }

    pub fn unpin_node(&mut self, node_id: &str, cx: &mut Context<Self>) {
        if self.pinned_positions.remove(node_id).is_some() {
            cx.notify();
        }
    }

    pub fn open_chat_and_close(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.close(cx);
        self.pending_handoff
            .set(Some(WorkflowModalHandoff::OpenChatSession(session_id)));
        cx.notify();
    }

    pub fn open_usage_and_close(&mut self, run_id: String, cx: &mut Context<Self>) {
        self.close(cx);
        self.pending_handoff
            .set(Some(WorkflowModalHandoff::OpenUsageRun(run_id)));
        cx.notify();
    }

    #[must_use]
    pub fn active_process_count(&self) -> usize {
        self.processes
            .iter()
            .filter(|p| p.state == ProcessState::Running)
            .count()
    }
}

impl Render for WorkflowMonitorModal {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if !self.open {
            return div().into_any_element();
        }
        let p = palette(cx);
        let ch = crate::ui::tr(cx).chat();
        let ent_close = cx.entity().clone();
        let on_close = move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
            ent_close.update(cx, |this, cx| this.close(cx));
        };

        let header = render_modal_header(self, cx);
        let footer = render_modal_footer(self, cx);

        let body: AnyElement = if let Some(topo) = &self.topology {
            let (active_tab, is_auto) = self.effective_tab(topo);
            let graph_ok = is_graph_available(topo);
            let layout = (active_tab == WorkflowTab::Graph || self.selected_node_id.is_some())
                .then(|| build_modal_graph_layout(self, topo, cx));

            let telemetry = render_modal_telemetry(self, topo, layout.as_ref(), cx);
            let proc_tray = self.show_processes.then(|| render_processes_tray(self, cx));
            let view_bar = render_modal_view_bar(
                self,
                topo,
                active_tab,
                is_auto,
                graph_ok,
                layout.as_ref(),
                cx,
            );

            let stage: AnyElement = if active_tab == WorkflowTab::Changes {
                render_changes_tab(self, cx)
            } else {
                let main_pane: AnyElement = match active_tab {
                    WorkflowTab::Graph => match &layout {
                        Some(l) => render_execution_graph(self, l, cx),
                        None => render_steps_view(self, topo, cx),
                    },
                    WorkflowTab::Steps => div()
                        .id("wf-modal-steps-scroll")
                        .size_full()
                        .overflow_y_scrollbar()
                        .child(
                            v_col()
                                .w_full()
                                .gap(px(12.0))
                                .px(px(18.0))
                                .py(px(14.0))
                                .child(render_steps_view(self, topo, cx)),
                        )
                        .into_any_element(),
                    WorkflowTab::Timeline => div()
                        .id("wf-modal-timeline-scroll")
                        .size_full()
                        .overflow_y_scrollbar()
                        .child(
                            v_col()
                                .w_full()
                                .gap(px(12.0))
                                .px(px(18.0))
                                .py(px(14.0))
                                .child(render_timeline_view(self, topo, cx)),
                        )
                        .into_any_element(),
                    WorkflowTab::Changes => div().into_any_element(),
                };
                let inspector = layout
                    .as_ref()
                    .and_then(|l| render_node_inspector(self, topo, l, cx));

                if let Some(insp_el) = inspector {
                    h_resizable("wf-modal-split")
                        .child(resizable_panel().child(main_pane))
                        .child(
                            resizable_panel()
                                .size(px(416.0))
                                .size_range(px(336.0)..px(560.0))
                                .child(insp_el),
                        )
                        .into_any_element()
                } else {
                    div().size_full().child(main_pane).into_any_element()
                }
            };

            v_col()
                .size_full()
                .min_h_0()
                .child(telemetry)
                .children(proc_tray)
                .child(view_bar)
                .child(div().flex_1().min_h_0().w_full().child(stage))
                .into_any_element()
        } else {
            div()
                .size_full()
                .bg(to_hsla(p.surface))
                .flex()
                .items_center()
                .justify_center()
                .child(empty_inline(ch.monitor_loading_topology(), None, None, cx))
                .into_any_element()
        };

        let mut root = div().child(viewport_modal(
            "workflow-monitor-modal",
            0.90,
            0.90,
            header,
            body,
            footer,
            window,
            cx,
            on_close,
        ));

        if let Some((title, content)) = self.full_output_dialog.clone() {
            let sig = (title.clone(), content.len());
            if self.full_output_sig.as_ref() != Some(&sig) {
                self.full_output_sig = Some(sig);
                self.full_output_viewer.update(cx, |v, cx| {
                    v.set_text(content, None, window, cx);
                });
            }
            let ent = cx.entity().clone();
            let sub_body = div()
                .w_full()
                .h(px(480.0))
                .child(self.full_output_viewer.clone())
                .into_any_element();
            let sub_foot = btn_secondary("wf-full-out-close", ch.monitor_close())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.full_output_dialog = None;
                    this.full_output_sig = None;
                    cx.notify();
                }))
                .into_any_element();
            root = root.child(modal_dialog(
                "wf-full-out-modal",
                title,
                820.0,
                sub_body,
                sub_foot,
                window,
                cx,
                move |_, _, cx| {
                    ent.update(cx, |this, cx| {
                        this.full_output_dialog = None;
                        this.full_output_sig = None;
                        cx.notify();
                    });
                },
            ));
        }

        root.into_any_element()
    }
}
