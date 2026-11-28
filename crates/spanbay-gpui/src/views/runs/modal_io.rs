//! Async loaders, mutations, and live 1s refresh ticker for [`WorkflowMonitorModal`].

use std::time::Duration;

use gpui_kit::{Context, WeakEntity, Window};
use spanbay_desktop::core::agent::model::{FileChangeStatus, NodeRecoveryAction};
use spanbay_desktop::core::app::{
    agent_change_diff_impl, agent_change_keep_impl, agent_change_net_diff_impl,
    agent_change_undo_impl, agent_changes_list_impl, agent_process_kill_impl,
    agent_process_list_impl, agent_run_stop_impl, agent_run_topology_impl,
    agent_session_latest_topology_impl, agent_session_run_history_impl,
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
                    let cleaned =
                        spanbay_desktop::state::clean_tool_output_for_modal(&name, &page.text);
                    this.full_output_dialog = Some((name.clone(), cleaned));
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
        self.selected_change_diff_err = None;
        let cid_check = change_id.clone();
        // Several edits of one file in the same session: also fetch the file's net change (product.md §7).
        let same_file: Vec<String> = self
            .changes
            .iter()
            .find(|c| c.id == change_id)
            .filter(|sel| sel.status != FileChangeStatus::Undone)
            .map(|sel| {
                self.changes
                    .iter()
                    .filter(|c| {
                        c.session_id == sel.session_id
                            && c.path == sel.path
                            && c.folder == sel.folder
                            && c.status != FileChangeStatus::Undone
                    })
                    .map(|c| c.id.clone())
                    .collect()
            })
            .unwrap_or_default();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let net = (same_file.len() > 1)
                    .then(|| agent_change_net_diff_impl(state, &same_file).ok())
                    .flatten();
                (agent_change_diff_impl(state, &change_id), net)
            },
            move |this, (res, net), cx| {
                if this.selected_change_id.as_deref() != Some(cid_check.as_str()) {
                    return;
                }
                match res {
                    Ok(dto) => {
                        this.selected_change_diff = Some((cid_check, dto.unified_diff, dto.binary));
                        this.selected_change_diff_err = None;
                        this.selected_net_diff = net.map(|d| (d.unified_diff, d.binary));
                    }
                    Err(err) => {
                        this.selected_change_diff = None;
                        this.selected_change_diff_err = Some((cid_check, err.message().to_owned()));
                        this.selected_net_diff = None;
                    }
                }
                cx.notify();
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

    pub fn undo_change(&mut self, change_id: String, force: bool, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| agent_change_undo_impl(state, &[change_id], force),
            |this, res, cx| match res {
                Ok(touched) => {
                    if touched
                        .iter()
                        .any(|c| c.status == FileChangeStatus::Conflict)
                    {
                        this.status_msg = Some((
                            Tone::Warning,
                            crate::ui::tr(cx).common().change_conflict_hint().to_owned(),
                        ));
                    }
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
                match res {
                    Ok(Some(topo)) => {
                        let actual_rid = topo.run.run_id.clone();
                        let is_active = topo.run.status.is_active();
                        if this
                            .topology
                            .as_ref()
                            .is_some_and(|prev| prev.run.run_id != actual_rid)
                        {
                            this.selected_node_id = None;
                            this.user_cleared_selection = false;
                            this.expanded_sections.clear();
                            this.pinned_positions.clear();
                            this.drag_state = None;
                            this.alignment_guides.clear();
                        }
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
                        this.topology_loaded = true;
                        this.topology_error = None;
                        if is_active && this.open {
                            this.start_live_ticker(cx);
                        } else {
                            this.tick_task = None;
                        }
                    }
                    Ok(None) => {
                        this.topology = None;
                        this.topology_loaded = true;
                        this.topology_error = None;
                        this.tick_task = None;
                    }
                    Err(err) => {
                        let msg = err.message().to_owned();
                        this.topology = None;
                        this.topology_loaded = true;
                        this.topology_error = Some(msg.clone());
                        this.status_msg = Some((Tone::Danger, msg));
                        this.tick_task = None;
                    }
                }
                cx.notify();
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
                    if let Some(sel) = this.selected_change_id.clone() {
                        if this.changes.iter().any(|c| c.id == sel) {
                            this.select_change(sel, cx);
                        } else {
                            this.selected_change_id = None;
                            this.selected_change_diff = None;
                            this.selected_change_diff_err = None;
                            this.selected_net_diff = None;
                            if let Some(fid) = first_id {
                                this.select_change(fid, cx);
                            }
                        }
                    } else if let Some(fid) = first_id {
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
