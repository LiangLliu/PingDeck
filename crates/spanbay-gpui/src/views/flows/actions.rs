//! Core CRUD, step manipulation, live streaming execution, cancellation, and run history actions for `FlowsView`.

use std::collections::BTreeMap;

use gpui_kit::{Context, Window};
use spanbay_desktop::core::app::factory_service::{
    flow_duplicate, flow_new, flow_step_duplicate, flow_step_new, flow_step_remove,
    flow_steps_reorder, new_entity_id,
};
use spanbay_desktop::core::app::{
    environment_list_impl, execution_cancel_impl, flow_delete_impl, flow_list_impl, flow_run_impl,
    flow_run_list_impl, flow_save_impl, request_list_impl, settings_get_impl,
};
use spanbay_desktop::core::error::AppError;
use spanbay_desktop::core::flows::FlowEvent;
use spanbay_desktop::core::model::{HttpMethod, Request};
use spanbay_desktop::streams::channel_sink;

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into, subscribe_stream_into};
use crate::ui::Tone;

use super::state::{FlowModal, FlowStepTab, FlowsView};

impl FlowsView {
    pub fn reload(&mut self, ws_id: String, cx: &mut Context<Self>) {
        self.lang = settings_get_impl(runtime(cx).state()).general.language;
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let entries = flow_list_impl(state, ws_id.clone())?;
                let requests = request_list_impl(state, ws_id.clone()).unwrap_or_default();
                let envs = environment_list_impl(state, ws_id).unwrap_or_default();
                Ok::<_, AppError>((entries, requests, envs))
            },
            |this, res, cx| {
                match res {
                    Ok((entries, requests, envs)) => {
                        this.entries = entries;
                        this.requests = requests;
                        this.environments = envs;
                        let sel_ok = this
                            .selected_flow_id
                            .as_deref()
                            .is_some_and(|id| this.entries.iter().any(|e| e.flow.id == id));
                        if !sel_ok {
                            this.selected_flow_id = this.entries.first().map(|e| e.flow.id.clone());
                            this.draft = this.entries.first().map(|e| e.flow.clone());
                            this.dirty = false;
                            this.pending_sync = true;
                        } else if !this.dirty
                            && let Some(id) = this.selected_flow_id.as_deref()
                            && let Some(e) = this.entries.iter().find(|e| e.flow.id == id)
                        {
                            this.draft = Some(e.flow.clone());
                            this.pending_sync = true;
                        }
                        if let Some(fid) = this.selected_flow_id.clone() {
                            this.load_past_runs(fid, cx);
                        }
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn select_flow(&mut self, flow_id: String, cx: &mut Context<Self>) {
        if self.selected_flow_id.as_deref() == Some(flow_id.as_str()) {
            return;
        }
        self.selected_flow_id = Some(flow_id.clone());
        self.draft = self
            .entries
            .iter()
            .find(|e| e.flow.id == flow_id)
            .map(|e| e.flow.clone());
        self.dirty = false;
        self.selected_step_id = None;
        self.inspector_open = false;
        self.live_steps.clear();
        self.last_result = None;
        self.past_runs.clear();
        self.active_history_run_id = None;
        self.drag_state = None;
        self.alignment_guides.clear();
        self.pending_sync = true;
        self.load_past_runs(flow_id, cx);
        cx.notify();
    }

    pub fn load_past_runs(&mut self, flow_id: String, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let fid_match = flow_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| flow_run_list_impl(state, ws_id, flow_id),
            move |this, res, cx| {
                if this.selected_flow_id.as_deref() != Some(fid_match.as_str()) {
                    return;
                }
                if let Ok(runs) = res {
                    this.past_runs = runs;
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn open_create_flow_modal(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.new_flow_name_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.active_modal = Some(FlowModal::CreateFlow);
        cx.notify();
    }

    pub fn create_flow(&mut self, cx: &mut Context<Self>) {
        self.create_flow_named(None, cx);
    }

    pub fn create_flow_named(&mut self, custom_name: Option<String>, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let ws_reload = ws_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let mut flow = flow_new(state, &ws_id)?;
                if let Some(nm) = custom_name {
                    flow.name = nm;
                }
                let mut first = flow_step_new(&flow);
                first.method_override = Some(HttpMethod::Get);
                first.url_override = Some("https://httpbin.org/get".to_owned());
                flow.steps.push(first);
                flow_save_impl(state, flow)
            },
            move |this, res, cx| {
                match res {
                    Ok(saved) => {
                        this.selected_flow_id = Some(saved.id.clone());
                        this.draft = Some(saved);
                        this.dirty = false;
                        this.pending_sync = true;
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::Tr(this.lang)
                                .flows()
                                .actions_created_flow()
                                .to_owned(),
                        ));
                        this.reload(ws_reload, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn save_flow(&mut self, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let Some(flow) = self.draft.clone() else {
            return;
        };
        let ws_reload = self.workspace_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| flow_save_impl(state, flow),
            move |this, res, cx| {
                match res {
                    Ok(saved) => {
                        this.draft = Some(saved.clone());
                        this.dirty = false;
                        this.pending_sync = true;
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::Tr(this.lang)
                                .flows()
                                .actions_saved_flow()
                                .to_owned(),
                        ));
                        if let Some(ws) = ws_reload {
                            this.reload(ws, cx);
                        }
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn duplicate_selected(&mut self, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let Some(flow) = self.draft.clone() else {
            return;
        };
        let ws_reload = self.workspace_id.clone();
        let copy = flow_duplicate(flow);
        let task = spawn_blocking_into(
            cx,
            move |state| flow_save_impl(state, copy),
            move |this, res, cx| {
                match res {
                    Ok(saved) => {
                        this.selected_flow_id = Some(saved.id.clone());
                        this.draft = Some(saved);
                        this.dirty = false;
                        this.pending_sync = true;
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::Tr(this.lang)
                                .flows()
                                .actions_duplicated_flow()
                                .to_owned(),
                        ));
                        if let Some(ws) = ws_reload {
                            this.reload(ws, cx);
                        }
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn delete_flow(&mut self, flow_id: String, cx: &mut Context<Self>) {
        let Some(ws_id) = self.workspace_id.clone() else {
            return;
        };
        let ws_reload = ws_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| flow_delete_impl(state, ws_id, flow_id),
            move |this, res, cx| {
                this.active_modal = None;
                match res {
                    Ok(()) => {
                        this.selected_flow_id = None;
                        this.draft = None;
                        this.dirty = false;
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::Tr(this.lang)
                                .flows()
                                .actions_deleted_flow()
                                .to_owned(),
                        ));
                        this.reload(ws_reload, cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn add_inline_step(&mut self, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let Some(flow) = self.draft.as_mut() else {
            return;
        };
        let is_dag = spanbay_desktop::core::flows::has_dag_topology(flow);
        let prev_id = self
            .selected_step_id
            .clone()
            .or_else(|| flow.steps.last().map(|s| s.id.clone()));
        let mut step = flow_step_new(flow);
        step.method_override = Some(HttpMethod::Get);
        step.url_override = Some("https://httpbin.org/get".to_owned());
        if is_dag && let Some(pid) = prev_id {
            step.depends_on.push(pid);
        }
        let id = step.id.clone();
        flow.steps.push(step);
        self.selected_step_id = Some(id);
        self.inspector_open = true;
        self.step_tab = FlowStepTab::Request;
        self.dirty = true;
        self.pending_sync = true;
        cx.notify();
    }

    pub fn relink_selected_step_request(&mut self, req: &Request, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        if let Some(step) = self.selected_step_mut() {
            step.request_id = req.id.as_str().to_owned();
            if step.name.trim().is_empty() || step.name.starts_with("Step ") {
                step.name = req.name.clone();
            }
            self.dirty = true;
            self.pending_sync = true;
        }
        self.active_modal = None;
        cx.notify();
    }

    pub fn add_saved_request_step(&mut self, req: &Request, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let Some(flow) = self.draft.as_mut() else {
            return;
        };
        let is_dag = spanbay_desktop::core::flows::has_dag_topology(flow);
        let prev_id = self
            .selected_step_id
            .clone()
            .or_else(|| flow.steps.last().map(|s| s.id.clone()));
        let mut step = flow_step_new(flow);
        step.name = req.name.clone();
        step.request_id = req.id.as_str().to_owned();
        if is_dag && let Some(pid) = prev_id {
            step.depends_on.push(pid);
        }
        let id = step.id.clone();
        flow.steps.push(step);
        self.selected_step_id = Some(id);
        self.inspector_open = true;
        self.step_tab = FlowStepTab::Request;
        self.active_modal = None;
        self.dirty = true;
        self.pending_sync = true;
        cx.notify();
    }

    pub fn move_step(&mut self, from: usize, to: usize, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let Some(flow) = self.draft.as_mut() else {
            return;
        };
        if let Ok(steps) = flow_steps_reorder(flow.steps.clone(), from, to) {
            flow.steps = steps;
            self.dirty = true;
            cx.notify();
        }
    }

    pub fn duplicate_step(&mut self, step_id: String, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let Some(flow) = self.draft.as_mut() else {
            return;
        };
        if let Ok(steps) = flow_step_duplicate(flow.steps.clone(), &step_id) {
            let next_id = steps
                .iter()
                .position(|s| s.id == step_id)
                .and_then(|idx| steps.get(idx + 1))
                .map(|s| s.id.clone());
            flow.steps = steps;
            if let Some(id) = next_id {
                self.selected_step_id = Some(id);
            }
            self.dirty = true;
            self.pending_sync = true;
            cx.notify();
        }
    }

    pub fn remove_step(&mut self, step_id: String, cx: &mut Context<Self>) {
        self.snapshot_inputs(cx);
        let Some(flow) = self.draft.as_mut() else {
            return;
        };
        if let Ok(steps) = flow_step_remove(flow.steps.clone(), &step_id) {
            flow.steps = steps;
            flow.options.pinned_positions.remove(&step_id);
            self.dirty = true;
            self.pending_sync = true;
            cx.notify();
        }
    }

    pub fn run_flow(
        &mut self,
        max_steps: Option<usize>,
        only_step: Option<usize>,
        confirmed: Option<bool>,
        cx: &mut Context<Self>,
    ) {
        self.snapshot_inputs(cx);
        let (Some(ws_id), Some(flow)) = (self.workspace_id.clone(), self.draft.clone()) else {
            return;
        };
        let flow_id = flow.id.clone();
        let env_id = self.selected_env_id.clone();
        let dirty_save = self.dirty.then_some(flow.clone());
        let token = new_entity_id("flow_cancel");

        // When running a single step, seed from the previous run's captured variables if any.
        let seed_vars: Option<BTreeMap<String, String>> = if only_step.is_some() {
            let mut merged = flow.options.initial_variables.clone();
            if let Some(prev) = &self.last_result {
                for kv in &prev.variables {
                    merged.insert(kv.key.clone(), kv.value.clone());
                }
            }
            Some(merged)
        } else {
            Some(flow.options.initial_variables.clone())
        };
        let cookie_session = Some(flow.options.cookie_session);

        self.running = true;
        self.cancel_token = Some(token.clone());
        self.live_steps.clear();
        self.live_total = flow.steps.len();
        self.active_history_run_id = None;
        self.active_modal = None;
        cx.notify();

        let (sink, rx) = channel_sink::<FlowEvent>();
        let stream_task = subscribe_stream_into(cx, rx, 16, |this, events, cx| {
            for ev in events {
                match ev {
                    FlowEvent::StepsFinished { total, steps, .. } => {
                        this.live_total = total;
                        for st in steps {
                            this.live_steps.insert(st.step_id.clone(), st);
                        }
                    }
                    FlowEvent::RunFinished { .. } => {}
                }
            }
            cx.notify();
        });
        self._tasks.push(stream_task);

        let fid_reload = flow_id.clone();
        let ws_reload = ws_id.clone();
        let task = spawn_async_into(
            cx,
            move |state| async move {
                if let Some(f) = dirty_save {
                    let _ = flow_save_impl(&state, f)?;
                }
                flow_run_impl(
                    &state,
                    ws_id,
                    flow_id,
                    env_id,
                    seed_vars,
                    max_steps,
                    only_step,
                    cookie_session,
                    Some(token),
                    confirmed,
                    sink.as_ref(),
                )
                .await
            },
            move |this, res, cx| {
                this.running = false;
                this.cancel_token = None;
                match res {
                    Ok(run) => {
                        this.dirty = false;
                        let summary_msg = if run.cancelled {
                            crate::ui::Tr(this.lang)
                                .flows()
                                .actions_flow_run_cancelled()
                                .to_owned()
                        } else if run.passed {
                            crate::ui::Tr(this.lang)
                                .flows()
                                .actions_flow_passed_steps_ms(run.steps.len(), run.elapsed_ms)
                        } else {
                            crate::ui::Tr(this.lang)
                                .flows()
                                .run_failed_ms(run.elapsed_ms)
                        };
                        let tone = if run.cancelled {
                            Tone::Warning
                        } else if run.passed {
                            Tone::Success
                        } else {
                            Tone::Danger
                        };
                        let failed_step_id = (!run.passed && !run.cancelled)
                            .then(|| {
                                run.steps
                                    .iter()
                                    .find(|s| !s.passed)
                                    .map(|s| s.step_id.clone())
                            })
                            .flatten();
                        this.last_result = Some(run);
                        this.active_history_run_id = None;
                        if let Some(fid) = failed_step_id {
                            this.selected_step_id = Some(fid);
                            this.inspector_open = true;
                            this.step_tab = FlowStepTab::Output;
                            this.pending_sync = true;
                        }
                        this.status_msg = Some((tone, summary_msg));
                        this.load_past_runs(fid_reload, cx);
                        this.reload(ws_reload, cx);
                    }
                    Err(AppError::ConfirmationRequired { message, .. }) => {
                        this.active_modal = Some(FlowModal::ConfirmRun {
                            reason: message,
                            max_steps,
                            only_step,
                        });
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn cancel_run(&mut self, cx: &mut Context<Self>) {
        if let Some(tok) = self.cancel_token.clone() {
            let _ = execution_cancel_impl(runtime(cx).state(), tok);
            self.status_msg = Some((
                Tone::Warning,
                crate::ui::Tr(self.lang)
                    .flows()
                    .actions_cancelling_flow_run()
                    .to_owned(),
            ));
            cx.notify();
        }
    }
}
