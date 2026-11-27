//! Async loaders, mutations, and live 1s refresh ticker for [`WorkflowMonitorModal`].

use std::time::Duration;

use gpui_kit::{Context, WeakEntity, Window};
use pingdeck_desktop::core::agent::model::NodeRecoveryAction;
use pingdeck_desktop::core::app::{
    agent_change_diff_impl, agent_change_keep_impl, agent_change_undo_impl,
    agent_changes_list_impl, agent_process_kill_impl, agent_process_list_impl, agent_run_stop_impl,
    agent_run_topology_impl, agent_session_latest_topology_impl, agent_session_run_history_impl,
    agent_task_node_recover_impl, agent_tool_output_read_impl,
};

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::graph::build_modal_graph_layout;
use super::modal::WorkflowMonitorModal;

impl WorkflowMonitorModal {
    pub fn open_full_tool_output(&mut self, call_id: String, name: String, cx: &mut Context<Self>) {
        let sid = self.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let task = spawn_blocking_into(
            cx,
            move |state| agent_tool_output_read_impl(state, &sid, &call_id, 0, 32_768),
            move |this, res, cx| match res {
                Ok(page) => {
                    this.full_output_dialog = Some((name.clone(), page.text));
                    cx.notify();
                }
                Err(err) => {
                    this.status_msg = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn open_text_dialog(&mut self, title: String, content: String, cx: &mut Context<Self>) {
        self.full_output_dialog = Some((title, content));
        cx.notify();
    }

    pub fn stop_run_by_id(&mut self, run_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| agent_run_stop_impl(state, &run_id),
            |this, _, cx| this.reload_all(cx),
        );
        self._tasks.push(task);
    }

    pub fn recover_task_node(
        &mut self,
        node_id: String,
        action: NodeRecoveryAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let sid = self.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let guidance = self.node_recovery_input.read(cx).value().trim().to_owned();
        let guidance_opt = (!guidance.is_empty()).then_some(guidance);
        self.node_recovery_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        let task = spawn_async_into(
            cx,
            move |state| async move {
                agent_task_node_recover_impl(&state, &sid, &node_id, action, guidance_opt).await
            },
            |this, res, cx| match res {
                Ok(started) => {
                    this.run_id = Some(started.run_id);
                    this.selected_node_id = None;
                    this.user_cleared_selection = false;
                    this.reload_all(cx);
                }
                Err(err) => {
                    this.status_msg = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn select_change(&mut self, change_id: String, cx: &mut Context<Self>) {
        self.selected_change_id = Some(change_id.clone());
        let cid_check = change_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_change_diff_impl(state, &change_id),
            move |this, res, cx| {
                if this.selected_change_id.as_deref() != Some(cid_check.as_str()) {
                    return;
                }
                if let Ok(dto) = res {
                    this.selected_change_diff = Some((cid_check, dto.unified_diff, dto.binary));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
        cx.notify();
    }

    pub fn keep_changes(&mut self, ids: Vec<String>, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| agent_change_keep_impl(state, &ids),
            |this, res, cx| match res {
                Ok(_) => this.reload_all(cx),
                Err(err) => {
                    this.status_msg = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn undo_change(&mut self, change_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| agent_change_undo_impl(state, &[change_id], false),
            |this, res, cx| match res {
                Ok(_) => this.reload_all(cx),
                Err(err) => {
                    this.status_msg = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn kill_process(&mut self, proc_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| agent_process_kill_impl(state, &proc_id),
            |this, _, cx| this.load_processes(cx),
        );
        self._tasks.push(task);
    }

    pub fn load_topology(&mut self, cx: &mut Context<Self>) {
        let sid = self.session_id.clone();
        let rid = self.run_id.clone();
        let sid_check = sid.clone();
        let rid_check = rid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| match rid {
                Some(r) => agent_run_topology_impl(state, &r).map(Some),
                None => agent_session_latest_topology_impl(state, &sid),
            },
            move |this, res, cx| {
                if this.session_id != sid_check || this.run_id != rid_check {
                    return;
                }
                if let Ok(Some(topo)) = res {
                    let actual_rid = topo.run.run_id.clone();
                    let is_active = topo.run.status.is_active();
                    this.run_id = Some(actual_rid.clone());
                    if let Some(init_tab) = this.pending_initial_tab.take() {
                        this.tab_overrides.insert(actual_rid, init_tab);
                    }
                    // Smart default focus: auto-select first waiting/failed node if user hasn't
                    // manually picked or cleared selection.
                    if this.selected_node_id.is_none() && !this.user_cleared_selection {
                        let layout = build_modal_graph_layout(this, &topo, cx);
                        this.selected_node_id = layout.default_focus_node_id();
                    }
                    this.topology = Some(topo);
                    if is_active && this.open {
                        this.start_live_ticker(cx);
                    } else {
                        this.tick_task = None;
                    }
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn load_session_history(&mut self, cx: &mut Context<Self>) {
        let sid = self.session_id.clone();
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_session_run_history_impl(state, &sid, Some(30)),
            move |this, res, cx| {
                if this.session_id != sid_check {
                    return;
                }
                if let Ok(items) = res {
                    this.session_history = items;
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn load_changes(&mut self, cx: &mut Context<Self>) {
        let sid = self.session_id.clone();
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_changes_list_impl(state, &sid),
            move |this, res, cx| {
                if this.session_id != sid_check {
                    return;
                }
                if let Ok(list) = res {
                    let first_id = list.first().map(|c| c.id.clone());
                    this.changes = list;
                    if let Some(sel) = this.selected_change_id.clone()
                        && !this.changes.iter().any(|c| c.id == sel)
                    {
                        this.selected_change_id = None;
                        this.selected_change_diff = None;
                    }
                    if this.selected_change_id.is_none()
                        && let Some(fid) = first_id
                    {
                        this.select_change(fid, cx);
                    }
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub(super) fn load_processes(&mut self, cx: &mut Context<Self>) {
        let sid = self.session_id.clone();
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_process_list_impl(state, Some(&sid)),
            move |this, res, cx| {
                if this.session_id != sid_check {
                    return;
                }
                if let Ok(list) = res {
                    this.processes = list;
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    fn start_live_ticker(&mut self, cx: &mut Context<Self>) {
        if self.tick_task.is_some() {
            return;
        }
        self.tick_task = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(1)).await;
                let keep = this
                    .update(cx, |m, cx| {
                        if !m.open {
                            return false;
                        }
                        m.load_topology(cx);
                        m.load_changes(cx);
                        m.topology
                            .as_ref()
                            .is_some_and(|t| t.run.status.is_active())
                    })
                    .unwrap_or(false);
                if !keep {
                    break;
                }
            }
            let _ = this.update(cx, |m, _| m.tick_task = None);
        }));
    }
}
