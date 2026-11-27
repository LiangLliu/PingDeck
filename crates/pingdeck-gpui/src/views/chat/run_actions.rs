//! Lightweight workflow handoff, plan-gate toggle, board badge refresh, and single-run stop
//! controller actions for [`ChatView`].

use gpui_kit::Context;
use pingdeck_desktop::core::app::{agent_board_impl, agent_run_stop_impl};

use crate::bridge::spawn_blocking_into;
use crate::views::runs::WorkflowTab;

use super::ChatView;
use super::types::ChatHandoff;

impl ChatView {
    /// Emits a shell handoff opening the shared 90% viewport Workflow Monitor modal scoped to
    /// the current chat session.
    pub fn open_workflow_modal(
        &mut self,
        focus_run_id: Option<String>,
        initial_tab: Option<WorkflowTab>,
        cx: &mut Context<Self>,
    ) {
        if self.thread.session_id.is_empty() {
            return;
        }
        let run_id = focus_run_id
            .or_else(|| self.thread.active_run.as_ref().map(|r| r.run_id.clone()))
            .or_else(|| self.latest_session_run_id.clone());
        self.pending_handoff
            .set(Some(ChatHandoff::OpenWorkflowModal {
                session_id: self.thread.session_id.clone(),
                run_id,
                initial_tab,
            }));
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

    /// Stops any single active run — top-level or individual sub-agent — by its `run_id`.
    pub fn stop_run_by_id(&mut self, run_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| agent_run_stop_impl(state, &run_id),
            |this, _, cx| this.load_board(cx),
        );
        self._tasks.push(task);
    }

    /// Refreshes the lightweight board snapshot used for sidebar `Needs you` badges and today's
    /// token/cost footer.
    pub fn load_board(&mut self, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(cx, agent_board_impl, |this, res, cx| {
            if let Ok(board) = res {
                this.board = Some(board);
                cx.notify();
            }
        });
        self._tasks.push(task);
    }
}
