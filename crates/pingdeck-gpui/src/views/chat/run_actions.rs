//! Run Monitor & Execution Topology controller actions for [`ChatView`].

use gpui_kit::{Context, Window};
use pingdeck_desktop::core::agent::dto::AgentRunTopology;
use pingdeck_desktop::core::agent::model::NodeRecoveryAction;
use pingdeck_desktop::core::app::{
    agent_board_impl, agent_run_stop_all_impl, agent_run_stop_impl, agent_run_topology_impl,
    agent_session_latest_topology_impl, agent_task_node_recover_impl,
};
use pingdeck_desktop::state::{RunViewMode, is_graph_available, select_default_view_mode};

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::Tone;

use super::ChatView;
use super::run_monitor::all_board_rows;
use super::types::{ChatModal, ChatSidebarMode};

impl ChatView {
    /// Opens the full-canvas Run Monitor, optionally focusing a specific run id.
    pub fn open_run_monitor(&mut self, focus_run_id: Option<String>, cx: &mut Context<Self>) {
        self.sidebar_mode = ChatSidebarMode::Board;
        if let Some(rid) = focus_run_id {
            self.selected_monitor_run_id = Some(rid);
        }
        self.load_board(cx);
        self.start_elapsed_ticker(cx);
        cx.notify();
    }

    /// Manually refreshes both the global board snapshot and the selected run's topology.
    pub fn refresh_monitor(&mut self, cx: &mut Context<Self>) {
        self.load_board(cx);
        self.load_selected_topology(cx);
    }

    /// Selects a run in the Run Monitor feed and loads its execution topology.
    pub fn select_monitor_run(&mut self, run_id: String, cx: &mut Context<Self>) {
        if self.selected_monitor_run_id.as_deref() == Some(run_id.as_str())
            && self
                .selected_topology
                .as_ref()
                .is_some_and(|t| t.run.run_id == run_id)
        {
            return;
        }
        self.selected_monitor_run_id = Some(run_id);
        self.selected_graph_node_id = None;
        self.load_selected_topology(cx);
        cx.notify();
    }

    /// Resolves the active single-run presentation mode (`Steps` / `Graph` / `Timeline`) along
    /// with whether it was selected automatically by the adaptive router.
    #[must_use]
    pub fn effective_monitor_view_mode(&self, topo: &AgentRunTopology) -> (RunViewMode, bool) {
        if let Some(&manual) = self.monitor_view_overrides.get(&topo.run.run_id) {
            if manual == RunViewMode::Graph && !is_graph_available(topo) {
                return (RunViewMode::Steps, true);
            }
            return (manual, false);
        }
        (select_default_view_mode(topo), true)
    }

    /// Persists the user's manual view mode override for `run_id`.
    pub fn set_monitor_view_mode(
        &mut self,
        run_id: String,
        mode: RunViewMode,
        cx: &mut Context<Self>,
    ) {
        self.monitor_view_overrides.insert(run_id, mode);
        cx.notify();
    }

    /// Focuses or clears a selected node/member across Execution Graph, Steps, and Node Inspector.
    pub fn select_graph_node(&mut self, node_id: Option<String>, cx: &mut Context<Self>) {
        self.selected_graph_node_id = node_id;
        cx.notify();
    }

    /// Toggles expand/collapse of a single Parallel Group card in the Execution Graph.
    pub fn toggle_graph_group(&mut self, group_id: String, cx: &mut Context<Self>) {
        if !self.expanded_graph_groups.remove(&group_id) {
            self.expanded_graph_groups.insert(group_id);
        }
        cx.notify();
    }

    /// Expands all collapsible Parallel Group cards if any are collapsed, else collapses them all.
    pub fn toggle_all_graph_groups(&mut self, group_ids: &[String], cx: &mut Context<Self>) {
        let all_open = group_ids
            .iter()
            .all(|id| self.expanded_graph_groups.contains(id));
        if all_open {
            for id in group_ids {
                self.expanded_graph_groups.remove(id);
            }
        } else {
            for id in group_ids {
                self.expanded_graph_groups.insert(id.clone());
            }
        }
        cx.notify();
    }

    /// Sets the Execution Graph canvas zoom scale (`0.75..=1.15`).
    pub fn set_graph_zoom(&mut self, zoom: f32, cx: &mut Context<Self>) {
        self.graph_zoom = zoom.clamp(0.75, 1.15);
        cx.notify();
    }

    /// Toggles between Task Graph preview and Markdown view on a Plan approval gate card.
    pub fn set_plan_gate_markdown(
        &mut self,
        gate_id: String,
        show_md: bool,
        cx: &mut Context<Self>,
    ) {
        if show_md {
            self.plan_gate_show_markdown.insert(gate_id);
        } else {
            self.plan_gate_show_markdown.remove(&gate_id);
        }
        cx.notify();
    }

    /// Triggers selective node-level recovery (`Retry`, `UndoAndRetry`, or `Skip`) on a Task Graph
    /// node, preserving completed upstream & sibling nodes.
    pub fn recover_task_node(
        &mut self,
        session_id: String,
        node_id: String,
        action: NodeRecoveryAction,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        let guidance = self.node_recovery_input.read(cx).value().trim().to_owned();
        let guidance_opt = (!guidance.is_empty()).then_some(guidance);
        self.node_recovery_input
            .update(cx, |input, cx| input.set_value("", window, cx));
        let task = spawn_async_into(
            cx,
            move |state| async move {
                agent_task_node_recover_impl(&state, &session_id, &node_id, action, guidance_opt)
                    .await
            },
            |this, res, cx| match res {
                Ok(started) => {
                    this.selected_monitor_run_id = Some(started.run_id);
                    this.selected_graph_node_id = None;
                    this.refresh_monitor(cx);
                }
                Err(err) => {
                    this.status_msg = Some((Tone::Danger, err.message().to_owned()));
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    /// Jumps from the Run Monitor straight into a chat thread and returns the center stage to chat.
    pub fn open_session_from_monitor(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.sidebar_mode = ChatSidebarMode::Time;
        self.open_session(session_id, cx);
    }

    /// Reloads the execution topology for the currently selected run in the Run Monitor.
    pub fn load_selected_topology(&mut self, cx: &mut Context<Self>) {
        let Some(run_id) = self.selected_monitor_run_id.clone() else {
            self.selected_topology = None;
            return;
        };
        let rid_check = run_id.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_run_topology_impl(state, &run_id),
            move |this, res, cx| {
                if this.selected_monitor_run_id.as_deref() != Some(rid_check.as_str()) {
                    return;
                }
                if let Ok(topo) = res {
                    this.selected_topology = Some(topo);
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    /// Reloads the execution topology for the active chat session (used in the right Execution tab).
    pub fn load_session_topology(&mut self, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            self.session_topology = None;
            return;
        }
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_session_latest_topology_impl(state, &sid),
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                if let Ok(topo) = res {
                    this.session_topology = topo;
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    /// Stops any single active run — top-level or individual sub-agent — by its `run_id`.
    pub fn stop_run_by_id(&mut self, run_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| agent_run_stop_impl(state, &run_id),
            |this, _, cx| {
                this.load_board(cx);
                this.load_selected_topology(cx);
                if this.details_open {
                    this.load_session_topology(cx);
                }
            },
        );
        self._tasks.push(task);
    }

    /// Opens the confirmation modal listing every active run before cancelling all runs.
    pub fn request_stop_all_runs(&mut self, cx: &mut Context<Self>) {
        let active_runs: Vec<(String, String)> = self
            .board
            .as_ref()
            .map(|b| {
                b.running
                    .iter()
                    .chain(b.needs_you.iter().filter(|r| r.run.status.is_active()))
                    .map(|r| {
                        let title = if r.title.trim().is_empty() {
                            crate::ui::tr(cx).chat().header_new_chat().to_owned()
                        } else {
                            r.title.clone()
                        };
                        (r.run.run_id.clone(), title)
                    })
                    .collect()
            })
            .unwrap_or_default();
        if active_runs.is_empty() {
            return;
        }
        self.active_modal = Some(ChatModal::StopAllConfirm { active_runs });
        cx.notify();
    }

    /// Cancels every active top-level run (and all its sub-agents) after user confirmation.
    pub fn confirm_stop_all_runs(&mut self, cx: &mut Context<Self>) {
        self.active_modal = None;
        let task = spawn_blocking_into(cx, agent_run_stop_all_impl, |this, _, cx| {
            this.load_board(cx);
            this.load_selected_topology(cx);
            if this.details_open {
                this.load_session_topology(cx);
            }
        });
        self._tasks.push(task);
        cx.notify();
    }

    /// Toggles expand/collapse of a sub-agent lane or parallel tool wave inside the topology view.
    pub fn toggle_topology_node(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.expanded_topology_nodes.remove(&key) {
            self.expanded_topology_nodes.insert(key);
        }
        cx.notify();
    }

    /// Fetches the latest global [`AgentBoard`] and ensures the selected monitor run stays synced.
    pub fn refresh_board_and_selection(&mut self, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(cx, agent_board_impl, |this, res, cx| {
            if let Ok(board) = res {
                let keep_sel = this
                    .selected_monitor_run_id
                    .as_deref()
                    .is_some_and(|id| all_board_rows(&board).any(|r| r.run.run_id == id));
                if !keep_sel {
                    this.selected_monitor_run_id =
                        all_board_rows(&board).next().map(|r| r.run.run_id.clone());
                }
                this.board = Some(board);
                if this.sidebar_mode == ChatSidebarMode::Board {
                    if this.has_any_active_board_run() {
                        this.start_elapsed_ticker(cx);
                    }
                    this.load_selected_topology(cx);
                }
            }
            cx.notify();
        });
        self._tasks.push(task);
    }
}
