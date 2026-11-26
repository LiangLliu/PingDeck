//! Run Monitor & Execution Topology controller actions for [`ChatView`].

use gpui_kit::Context;
use pingdeck_desktop::core::app::{
    agent_board_impl, agent_run_stop_all_impl, agent_run_stop_impl, agent_run_topology_impl,
    agent_session_latest_topology_impl,
};

use crate::bridge::spawn_blocking_into;

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
        self.load_selected_topology(cx);
        cx.notify();
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
