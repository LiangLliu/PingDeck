//! DAG topology manipulation, edge insertion (`+`), swimlane management, layout controls,
//! history playback stepping, and hybrid drag-to-reposition actions for `FlowsView`.

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::app::factory_service::{flow_step_new, new_entity_id};
use pingdeck_desktop::core::flows::{check_flow_dag_cycles, has_dag_topology};
use pingdeck_desktop::core::model::items::{
    FlowBranch, FlowLaneDef, FlowLayoutDirectionPref, FlowStepKind,
};
use pingdeck_desktop::core::model::{HttpMethod, Request};
use pingdeck_desktop::state::FlowViewMode;

use crate::ui::Tone;

use super::state::{FlowStepTab, FlowsView};

impl FlowsView {
    /// Converts a purely linear flow into explicit `depends_on` edges before applying a DAG edit
    /// (adding a branch, parallel sibling, join, or explicit dependency) so existing step ordering
    /// stays intact.
    fn materialize_linear_edges_if_needed(&mut self) {
        let Some(flow) = self.draft.as_mut() else {
            return;
        };
        if has_dag_topology(flow) {
            return;
        }
        flow.steps.sort_by_key(|s| s.position);
        for i in 1..flow.steps.len() {
            let prev_id = flow.steps[i - 1].id.clone();
            flow.steps[i].depends_on.push(prev_id);
        }
    }

    fn reindex_draft_steps(&mut self) {
        if let Some(flow) = self.draft.as_mut() {
            for (idx, step) in flow.steps.iter_mut().enumerate() {
                step.position = idx as i64;
            }
        }
    }

    pub fn set_view_mode(&mut self, mode: FlowViewMode, cx: &mut Context<Self>) {
        self.view_mode = mode;
        cx.notify();
    }

    pub fn set_graph_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        self.graph_zoom = zoom.clamp(0.75, 1.15);
        cx.notify();
    }

    pub fn set_layout_direction(&mut self, dir: FlowLayoutDirectionPref, cx: &mut Context<Self>) {
        if let Some(flow) = self.draft.as_mut()
            && flow.options.layout.direction != dir
        {
            flow.options.layout.direction = dir;
            if dir == FlowLayoutDirectionPref::Serpentine {
                flow.options.layout.swimlanes = false;
            }
            flow.options.pinned_positions.clear();
            self.dirty = true;
        }
        self.drag_state = None;
        self.alignment_guides.clear();
        cx.notify();
    }

    pub fn toggle_center_align(&mut self, cx: &mut Context<Self>) {
        if let Some(flow) = self.draft.as_mut() {
            flow.options.layout.center_align = !flow.options.layout.center_align;
            self.dirty = true;
        }
        cx.notify();
    }

    pub fn toggle_swimlanes(&mut self, cx: &mut Context<Self>) {
        if let Some(flow) = self.draft.as_mut() {
            let enabling = !flow.options.layout.swimlanes;
            flow.options.layout.swimlanes = enabling;
            if enabling {
                if flow.options.lanes.is_empty() {
                    let fl = crate::ui::Tr(self.lang).flows();
                    flow.options.lanes = vec![
                        FlowLaneDef {
                            id: new_entity_id("lane"),
                            title: fl.lane_default_auth().to_owned(),
                            accent_index: 0,
                        },
                        FlowLaneDef {
                            id: new_entity_id("lane"),
                            title: fl.lane_default_core().to_owned(),
                            accent_index: 1,
                        },
                        FlowLaneDef {
                            id: new_entity_id("lane"),
                            title: fl.lane_default_verify().to_owned(),
                            accent_index: 2,
                        },
                    ];
                }
                if flow.options.layout.direction == FlowLayoutDirectionPref::Serpentine {
                    flow.options.layout.direction = FlowLayoutDirectionPref::LeftToRight;
                }
            }
            flow.options.pinned_positions.clear();
            self.dirty = true;
        }
        self.drag_state = None;
        self.alignment_guides.clear();
        cx.notify();
    }

    pub fn toggle_data_flow(&mut self, cx: &mut Context<Self>) {
        if let Some(flow) = self.draft.as_mut() {
            flow.options.layout.show_data_flow = !flow.options.layout.show_data_flow;
            self.dirty = true;
        }
        cx.notify();
    }

    pub fn reset_pinned_layout(&mut self, cx: &mut Context<Self>) {
        if let Some(flow) = self.draft.as_mut() {
            flow.options.pinned_positions.clear();
            self.dirty = true;
        }
        self.drag_state = None;
        self.alignment_guides.clear();
        cx.notify();
    }

    pub fn unpin_step(&mut self, step_id: &str, cx: &mut Context<Self>) {
        if let Some(flow) = self.draft.as_mut()
            && flow.options.pinned_positions.remove(step_id).is_some()
        {
            self.dirty = true;
            cx.notify();
        }
    }

    pub fn add_lane(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let title = self.lane_name_input.read(cx).value().trim().to_owned();
        if title.is_empty() {
            return;
        }
        if let Some(flow) = self.draft.as_mut() {
            let accent = flow.options.lanes.len();
            flow.options.lanes.push(FlowLaneDef {
                id: new_entity_id("lane"),
                title,
                accent_index: accent,
            });
            flow.options.layout.swimlanes = true;
            self.dirty = true;
        }
        self.lane_name_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    pub fn remove_lane(&mut self, lane_id: &str, cx: &mut Context<Self>) {
        if let Some(flow) = self.draft.as_mut() {
            flow.options.lanes.retain(|l| l.id != lane_id);
            for step in &mut flow.steps {
                if step.lane.as_deref() == Some(lane_id) {
                    step.lane = None;
                }
            }
            if flow.options.lanes.is_empty() {
                flow.options.layout.swimlanes = false;
            }
            self.dirty = true;
            cx.notify();
        }
    }

    pub fn add_control_step(&mut self, kind: FlowStepKind, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        self.materialize_linear_edges_if_needed();
        let Some(flow) = self.draft.as_mut() else {
            return;
        };
        let prev_id = self
            .selected_step_id
            .clone()
            .or_else(|| flow.steps.last().map(|s| s.id.clone()));
        let mut step = flow_step_new(flow);
        step.step_kind = kind;
        let fl = crate::ui::Tr(self.lang).flows();
        match kind {
            FlowStepKind::Condition => {
                step.name = fl.kind_condition_default_name().to_owned();
                step.branches = vec![
                    FlowBranch {
                        id: new_entity_id("br"),
                        label: fl.branch_default_true().to_owned(),
                        expression: "{{status}} == 200".to_owned(),
                        target_step_ids: Vec::new(),
                    },
                    FlowBranch {
                        id: new_entity_id("br"),
                        label: fl.branch_default_else().to_owned(),
                        expression: String::new(),
                        target_step_ids: Vec::new(),
                    },
                ];
            }
            FlowStepKind::Join => {
                step.name = fl.kind_join_default_name().to_owned();
            }
            FlowStepKind::Request => {
                step.method_override = Some(HttpMethod::Get);
                step.url_override = Some("https://httpbin.org/get".to_owned());
            }
        }
        if let Some(pid) = prev_id {
            step.depends_on.push(pid);
        }
        let id = step.id.clone();
        flow.steps.push(step);
        self.reindex_draft_steps();
        self.selected_step_id = Some(id);
        self.inspector_open = true;
        self.step_tab = if kind == FlowStepKind::Request {
            FlowStepTab::Request
        } else {
            FlowStepTab::Topology
        };
        self.dirty = true;
        self.pending_sync = true;
        cx.notify();
    }

    /// Creates a parallel sibling step sharing the same upstream dependencies as `anchor_step_id`.
    pub fn add_parallel_sibling(&mut self, anchor_step_id: &str, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        self.materialize_linear_edges_if_needed();
        let Some(flow) = self.draft.as_mut() else {
            return;
        };
        let (deps, lane) = flow
            .steps
            .iter()
            .find(|s| s.id == anchor_step_id)
            .map(|s| (s.depends_on.clone(), s.lane.clone()))
            .unwrap_or_default();
        let mut step = flow_step_new(flow);
        step.method_override = Some(HttpMethod::Get);
        step.url_override = Some("https://httpbin.org/get".to_owned());
        step.depends_on = deps;
        step.lane = lane;
        let id = step.id.clone();
        flow.steps.push(step);
        self.reindex_draft_steps();
        self.selected_step_id = Some(id);
        self.inspector_open = true;
        self.step_tab = FlowStepTab::Request;
        self.dirty = true;
        self.pending_sync = true;
        cx.notify();
    }

    /// Inserts a new step directly on the control edge `from_id -> to_id`, rewiring linear or DAG
    /// connections seamlessly.
    pub fn insert_step_between(
        &mut self,
        from_id: &str,
        to_id: &str,
        kind: FlowStepKind,
        saved_req: Option<&Request>,
        cx: &mut Context<Self>,
    ) {
        self.snapshot_inputs(cx);
        let is_dag_before = self.draft.as_ref().is_some_and(has_dag_topology);
        if kind != FlowStepKind::Request && !is_dag_before {
            self.materialize_linear_edges_if_needed();
        }
        let Some(flow) = self.draft.as_mut() else {
            return;
        };
        let is_dag = has_dag_topology(flow);
        let mut step = flow_step_new(flow);
        step.step_kind = kind;
        let fl = crate::ui::Tr(self.lang).flows();
        if let Some(req) = saved_req {
            step.name = req.name.clone();
            step.request_id = req.id.as_str().to_owned();
        } else {
            match kind {
                FlowStepKind::Request => {
                    step.method_override = Some(HttpMethod::Get);
                    step.url_override = Some("https://httpbin.org/get".to_owned());
                }
                FlowStepKind::Condition => {
                    step.name = fl.kind_condition_default_name().to_owned();
                    step.branches = vec![
                        FlowBranch {
                            id: new_entity_id("br"),
                            label: fl.branch_default_true().to_owned(),
                            expression: "{{status}} == 200".to_owned(),
                            target_step_ids: vec![to_id.to_owned()],
                        },
                        FlowBranch {
                            id: new_entity_id("br"),
                            label: fl.branch_default_else().to_owned(),
                            expression: String::new(),
                            target_step_ids: Vec::new(),
                        },
                    ];
                }
                FlowStepKind::Join => {
                    step.name = fl.kind_join_default_name().to_owned();
                }
            }
        }
        let new_id = step.id.clone();

        if is_dag {
            step.depends_on.push(from_id.to_owned());
            // Rewire any branch on `from_id` that pointed to `to_id`.
            if let Some(from_step) = flow.steps.iter_mut().find(|s| s.id == from_id) {
                for br in &mut from_step.branches {
                    for target in &mut br.target_step_ids {
                        if target == to_id {
                            *target = new_id.clone();
                        }
                    }
                }
            }
            if let Some(to_step) = flow.steps.iter_mut().find(|s| s.id == to_id) {
                to_step.depends_on.retain(|d| d != from_id);
                if kind != FlowStepKind::Condition && !to_step.depends_on.contains(&new_id) {
                    to_step.depends_on.push(new_id.clone());
                }
            }
            let insert_idx = flow
                .steps
                .iter()
                .position(|s| s.id == to_id)
                .unwrap_or(flow.steps.len());
            flow.steps.insert(insert_idx, step);
        } else {
            let insert_idx = flow
                .steps
                .iter()
                .position(|s| s.id == to_id)
                .unwrap_or(flow.steps.len());
            flow.steps.insert(insert_idx, step);
        }

        self.reindex_draft_steps();
        self.selected_step_id = Some(new_id);
        self.inspector_open = true;
        self.step_tab = if kind == FlowStepKind::Request {
            FlowStepTab::Request
        } else {
            FlowStepTab::Topology
        };
        self.active_modal = None;
        self.dirty = true;
        self.pending_sync = true;
        cx.notify();
    }

    pub fn set_selected_step_kind(&mut self, kind: FlowStepKind, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        if kind != FlowStepKind::Request {
            self.materialize_linear_edges_if_needed();
        }
        let fl = crate::ui::Tr(self.lang).flows();
        if let Some(step) = self.selected_step_mut() {
            step.step_kind = kind;
            if kind == FlowStepKind::Condition && step.branches.is_empty() {
                step.branches = vec![
                    FlowBranch {
                        id: new_entity_id("br"),
                        label: fl.branch_default_true().to_owned(),
                        expression: "{{status}} == 200".to_owned(),
                        target_step_ids: Vec::new(),
                    },
                    FlowBranch {
                        id: new_entity_id("br"),
                        label: fl.branch_default_else().to_owned(),
                        expression: String::new(),
                        target_step_ids: Vec::new(),
                    },
                ];
            }
            self.dirty = true;
            cx.notify();
        }
    }

    pub fn set_selected_step_lane(&mut self, lane: Option<String>, cx: &mut Context<Self>) {
        if let Some(step) = self.selected_step_mut() {
            step.lane = lane;
            self.dirty = true;
            cx.notify();
        }
    }

    pub fn toggle_selected_step_dependency(&mut self, dep_id: String, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        self.materialize_linear_edges_if_needed();
        let Some(sel_id) = self.selected_step_id.clone() else {
            return;
        };
        if sel_id == dep_id {
            return;
        }
        let Some(flow) = self.draft.as_mut() else {
            return;
        };
        let mut candidate = flow.clone();
        if let Some(st) = candidate.steps.iter_mut().find(|s| s.id == sel_id) {
            if let Some(pos) = st.depends_on.iter().position(|d| d == &dep_id) {
                st.depends_on.remove(pos);
            } else {
                st.depends_on.push(dep_id);
            }
        }
        if let Err(cyclic) = check_flow_dag_cycles(&candidate) {
            self.status_msg = Some((
                Tone::Warning,
                crate::ui::Tr(self.lang)
                    .flows()
                    .topo_cycle_blocked(cyclic.join(" → ")),
            ));
            cx.notify();
            return;
        }
        *flow = candidate;
        self.dirty = true;
        cx.notify();
    }

    pub fn add_branch_to_selected(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        let label = self.branch_label_input.read(cx).value().trim().to_owned();
        let expr = self.branch_expr_input.read(cx).value().trim().to_owned();
        if label.is_empty() {
            return;
        }
        if let Some(step) = self.selected_step_mut() {
            step.branches.push(FlowBranch {
                id: new_entity_id("br"),
                label,
                expression: expr,
                target_step_ids: Vec::new(),
            });
            self.dirty = true;
        }
        self.branch_label_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.branch_expr_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        cx.notify();
    }

    pub fn remove_branch_from_selected(&mut self, branch_id: &str, cx: &mut Context<Self>) {
        if let Some(step) = self.selected_step_mut() {
            step.branches.retain(|b| b.id != branch_id);
            self.dirty = true;
            cx.notify();
        }
    }

    pub fn toggle_branch_target(
        &mut self,
        branch_id: &str,
        target_id: String,
        cx: &mut Context<Self>,
    ) {
        let Some(sel_id) = self.selected_step_id.clone() else {
            return;
        };
        if sel_id == target_id {
            return;
        }
        let Some(flow) = self.draft.as_mut() else {
            return;
        };
        let mut candidate = flow.clone();
        if let Some(st) = candidate.steps.iter_mut().find(|s| s.id == sel_id)
            && let Some(br) = st.branches.iter_mut().find(|b| b.id == branch_id)
        {
            if let Some(pos) = br.target_step_ids.iter().position(|t| t == &target_id) {
                br.target_step_ids.remove(pos);
            } else {
                br.target_step_ids.push(target_id);
            }
        }
        if let Err(cyclic) = check_flow_dag_cycles(&candidate) {
            self.status_msg = Some((
                Tone::Warning,
                crate::ui::Tr(self.lang)
                    .flows()
                    .topo_cycle_blocked(cyclic.join(" → ")),
            ));
            cx.notify();
            return;
        }
        *flow = candidate;
        self.dirty = true;
        cx.notify();
    }

    pub fn select_history_run(&mut self, run_id: Option<String>, cx: &mut Context<Self>) {
        self.active_history_run_id = run_id;
        // If replaying a failed past run, auto-focus the failed step on Output tab.
        if let Some(failed_id) = self
            .active_past_summary()
            .and_then(|s| s.failed_step_id.clone())
        {
            self.selected_step_id = Some(failed_id);
            self.inspector_open = true;
            self.step_tab = FlowStepTab::Output;
            self.pending_sync = true;
        }
        cx.notify();
    }

    pub fn step_history(&mut self, delta: isize, cx: &mut Context<Self>) {
        if self.past_runs.is_empty() {
            return;
        }
        let cur_idx: isize = match self.active_history_run_id.as_deref() {
            None => 0,
            Some(id) => self
                .past_runs
                .iter()
                .position(|r| r.run_id == id)
                .unwrap_or(0) as isize,
        };
        let next = cur_idx + delta;
        if next >= 0
            && (next as usize) < self.past_runs.len()
            && let Some(run) = self.past_runs.get(next as usize)
        {
            let rid = run.run_id.clone();
            self.select_history_run(Some(rid), cx);
        }
    }
}
