//! Standalone global Run Monitor module (`NavModule::Runs`) and shared 90% viewport
//! Workflow Monitor modal (`WorkflowMonitorModal`).
//!
//! Decouples workspace-wide multi-chat concurrency monitoring and cross-chat run history from
//! `ChatView`: clicking any run row opens the shared 90% viewport Workflow Monitor overlay on top
//! of the entire application window, identical to opening the active chat's workflow from its
//! top-right header icon.

pub mod changes;
pub mod feed;
pub mod feed_cards;
pub mod graph;
pub mod graph_cards;
pub mod modal;
pub mod modal_chrome;
pub mod modal_header;
pub mod modal_io;
pub mod node_inspector;
pub mod node_payloads;
pub mod node_payloads_tools;
pub mod status;
pub mod steps;
pub mod timeline;
pub mod timeline_rows;
pub mod types;

use std::cell::Cell;
use std::time::Duration;

use gpui_kit::{
    AppContext as _, Context, Entity, IntoElement, ParentElement as _, Render, Styled as _,
    Subscription, Task, WeakEntity, Window, div, px,
};
use pingdeck_desktop::core::agent::dto::{AgentBoard, AgentBoardRow, SessionRunHistoryItem};
use pingdeck_desktop::core::agent::tools::process::{BackgroundProcess, ProcessState};
use pingdeck_desktop::core::app::{
    agent_all_runs_history_impl, agent_board_impl, agent_process_kill_impl,
    agent_process_list_impl, agent_run_stop_all_impl, agent_run_stop_impl,
};

use crate::bridge::spawn_blocking_into;
use crate::ui::{
    InputEvent, InputState, Tone, btn_danger, btn_secondary, h_row, modal_dialog, palette, to_hsla,
    v_col,
};

pub use modal::WorkflowMonitorModal;
pub use types::{
    HistoryPickerFilter, RunsFilter, RunsHandoff, RunsScope, WorkflowModalHandoff, WorkflowTab,
};

/// Full-width global Run Monitor & cross-chat run history page (`NavModule::Runs`).
pub struct RunsView {
    pub board: Option<AgentBoard>,
    pub all_history: Vec<SessionRunHistoryItem>,
    pub processes: Vec<BackgroundProcess>,
    pub scope: RunsScope,
    pub filter: RunsFilter,
    pub search_input: Entity<InputState>,
    pub show_processes: bool,
    pub stop_all_confirm: Option<Vec<(String, String)>>,
    pub status_msg: Option<(Tone, String)>,
    pub pending_handoff: Cell<Option<RunsHandoff>>,
    tick_task: Option<Task<()>>,
    _subscriptions: Vec<Subscription>,
    _tasks: Vec<Task<()>>,
}

impl RunsView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let ch = crate::ui::tr(cx).chat();
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(ch.monitor_search_ph()));
        let search_sub = cx.subscribe(&search_input, |_, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change) {
                cx.notify();
            }
        });
        let mut view = Self {
            board: None,
            all_history: Vec::new(),
            processes: Vec::new(),
            scope: RunsScope::Recent24h,
            filter: RunsFilter::All,
            search_input,
            show_processes: false,
            stop_all_confirm: None,
            status_msg: None,
            pending_handoff: Cell::new(None),
            tick_task: None,
            _subscriptions: vec![search_sub],
            _tasks: Vec::new(),
        };
        view.reload_all(cx);
        view
    }

    pub fn take_pending_handoff(&self) -> Option<RunsHandoff> {
        self.pending_handoff.take()
    }

    pub fn dismiss_modal_if_open(&mut self, cx: &mut Context<Self>) -> bool {
        if self.stop_all_confirm.take().is_some() {
            cx.notify();
            return true;
        }
        false
    }

    pub fn reload_all(&mut self, cx: &mut Context<Self>) {
        self.load_board(cx);
        self.load_processes(cx);
        if self.scope == RunsScope::AllHistory {
            self.load_history(cx);
        }
    }

    pub fn set_scope(&mut self, scope: RunsScope, cx: &mut Context<Self>) {
        self.scope = scope;
        if scope == RunsScope::AllHistory && self.all_history.is_empty() {
            self.load_history(cx);
        }
        cx.notify();
    }

    pub fn set_filter(&mut self, filter: RunsFilter, cx: &mut Context<Self>) {
        self.filter = filter;
        cx.notify();
    }

    pub fn toggle_processes(&mut self, cx: &mut Context<Self>) {
        self.show_processes = !self.show_processes;
        if self.show_processes {
            self.load_processes(cx);
        }
        cx.notify();
    }

    pub fn load_board(&mut self, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(cx, agent_board_impl, |this, res, cx| {
            if let Ok(board) = res {
                let any_active = !board.running.is_empty()
                    || board.needs_you.iter().any(|r| r.run.status.is_active());
                this.board = Some(board);
                if any_active {
                    this.start_refresh_ticker(cx);
                } else {
                    this.tick_task = None;
                }
                cx.notify();
            }
        });
        self._tasks.push(task);
    }

    pub fn load_history(&mut self, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            |state| agent_all_runs_history_impl(state, Some(80)),
            |this, res, cx| {
                if let Ok(items) = res {
                    this.all_history = items;
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn load_processes(&mut self, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            |state| agent_process_list_impl(state, None),
            |this, res, cx| {
                if let Ok(list) = res {
                    this.processes = list;
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn stop_run_by_id(&mut self, run_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| agent_run_stop_impl(state, &run_id),
            |this, _, cx| this.reload_all(cx),
        );
        self._tasks.push(task);
    }

    pub fn request_stop_all_runs(&mut self, cx: &mut Context<Self>) {
        let Some(board) = &self.board else {
            return;
        };
        let mut active: Vec<(String, String)> = Vec::new();
        for row in board.running.iter().chain(board.needs_you.iter()) {
            if row.run.status.is_active() {
                let title = if row.title.trim().is_empty() {
                    crate::ui::tr(cx).chat().header_new_chat().to_owned()
                } else {
                    row.title.clone()
                };
                active.push((row.run.run_id.clone(), title));
            }
        }
        if active.is_empty() {
            return;
        }
        self.stop_all_confirm = Some(active);
        cx.notify();
    }

    pub fn confirm_stop_all_runs(&mut self, cx: &mut Context<Self>) {
        self.stop_all_confirm = None;
        let task = spawn_blocking_into(cx, agent_run_stop_all_impl, |this, _, cx| {
            this.reload_all(cx);
        });
        self._tasks.push(task);
        cx.notify();
    }

    pub fn kill_process(&mut self, proc_id: String, cx: &mut Context<Self>) {
        let task = spawn_blocking_into(
            cx,
            move |state| agent_process_kill_impl(state, &proc_id),
            |this, _, cx| {
                this.load_processes(cx);
                this.load_board(cx);
            },
        );
        self._tasks.push(task);
    }

    pub fn open_workflow_modal(
        &mut self,
        session_id: String,
        run_id: Option<String>,
        initial_tab: Option<WorkflowTab>,
        cx: &mut Context<Self>,
    ) {
        self.pending_handoff
            .set(Some(RunsHandoff::OpenWorkflowModal {
                session_id,
                run_id,
                initial_tab,
            }));
        cx.notify();
    }

    pub fn open_chat_session(&mut self, session_id: String, cx: &mut Context<Self>) {
        self.pending_handoff
            .set(Some(RunsHandoff::OpenChatSession(session_id)));
        cx.notify();
    }

    pub fn open_usage_run(&mut self, run_id: String, cx: &mut Context<Self>) {
        self.pending_handoff
            .set(Some(RunsHandoff::OpenUsageRun(run_id)));
        cx.notify();
    }

    #[must_use]
    pub fn active_process_count(&self) -> usize {
        self.processes
            .iter()
            .filter(|p| p.state == ProcessState::Running)
            .count()
    }

    fn start_refresh_ticker(&mut self, cx: &mut Context<Self>) {
        if self.tick_task.is_some() {
            return;
        }
        self.tick_task = Some(cx.spawn(async move |this: WeakEntity<Self>, cx| {
            loop {
                cx.background_executor().timer(Duration::from_secs(2)).await;
                let keep_going = this
                    .update(cx, |view, cx| {
                        view.load_board(cx);
                        view.board.as_ref().is_some_and(|b| {
                            !b.running.is_empty()
                                || b.needs_you.iter().any(|r| r.run.status.is_active())
                        })
                    })
                    .unwrap_or(false);
                if !keep_going {
                    break;
                }
            }
            let _ = this.update(cx, |view, _| view.tick_task = None);
        }));
    }
}

/// Iterates every board row across `needs_you`, `running`, and `done`.
pub fn all_board_rows(board: &AgentBoard) -> impl Iterator<Item = &AgentBoardRow> {
    board
        .needs_you
        .iter()
        .chain(board.running.iter())
        .chain(board.done.iter())
}

impl Render for RunsView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let mut root = div()
            .relative()
            .size_full()
            .child(feed::render_runs_page(self, cx));

        if let Some(active_runs) = self.stop_all_confirm.clone() {
            let p = palette(cx);
            let ch = crate::ui::tr(cx).chat();
            let count = active_runs.len();
            let mut list = v_col().gap(px(4.0));
            for (_, title) in active_runs.iter().take(8) {
                list = list.child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .child(format!("• {title}")),
                );
            }
            let body = v_col()
                .gap(px(8.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.ink))
                        .child(ch.monitor_stop_all_body(count)),
                )
                .child(list)
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("runs-stopall-cancel", crate::ui::tr(cx).common().cancel())
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.stop_all_confirm = None;
                            cx.notify();
                        })),
                )
                .child(
                    btn_danger("runs-stopall-ok", ch.monitor_stop_all_confirm(count)).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.confirm_stop_all_runs(cx);
                        }),
                    ),
                )
                .into_any_element();
            let ent = cx.entity().clone();
            root = root.child(modal_dialog(
                "runs-stopall-modal",
                ch.monitor_stop_all_title(),
                440.0,
                body,
                footer,
                window,
                cx,
                move |_, _, cx| {
                    ent.update(cx, |this, cx| {
                        this.stop_all_confirm = None;
                        cx.notify();
                    });
                },
            ));
        }

        root
    }
}
