//! Lightweight workflow handoff, plan-gate toggle, board badge refresh, and single-run stop
//! controller actions for [`ChatView`].

use crate::bridge::{spawn_async_into, spawn_blocking_into};
use crate::ui::{
    Tone, btn_ghost, h_row, palette, status_indicator, status_label, status_visual, to_hsla,
};
use crate::views::runs::WorkflowTab;
use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, Styled as _, div, px,
};
use spanbay_desktop::core::agent::model::{
    AgentExportFormat, AgentRole, AgentRunStatus, AgentStopReason, ApprovalDecision, ApprovalScope,
};
use spanbay_desktop::core::app::{
    agent_board_impl, agent_message_edit_impl, agent_message_retry_impl, agent_run_stop_impl,
    agent_session_delete_impl, agent_session_export_redacted_impl, environment_list_impl,
};
use spanbay_desktop::services::{FileFilter, HostDialog, RfdDialog, SaveFileSpec};
use spanbay_desktop::state::{ApprovalResolution, GraphNodeStatus};

use super::ChatView;
use super::metrics::{format_cost_micro, format_duration_ms, stop_reason_label};
use super::reasoning::message_has_reasoning;
use super::types::ChatHandoff;
impl ChatView {
    pub fn retry_message(&mut self, message_id: String, cx: &mut Context<Self>) {
        self.retry_message_with_undo(message_id, false, cx);
    }

    pub fn retry_message_with_undo(
        &mut self,
        message_id: String,
        undo_first: bool,
        cx: &mut Context<Self>,
    ) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() || self.is_run_active() {
            return;
        }
        self.context_breakdown = None;
        let sid_check = sid.clone();
        let task = spawn_async_into(
            cx,
            move |state| async move {
                agent_message_retry_impl(&state, &sid, &message_id, undo_first).await
            },
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                if let Err(e) = res {
                    this.status_msg = Some((Tone::Danger, e.message().to_owned()));
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn confirm_edit_message(&mut self, message_id: String, cx: &mut Context<Self>) {
        self.confirm_edit_message_with_undo(message_id, false, cx);
    }

    pub fn confirm_edit_message_with_undo(
        &mut self,
        message_id: String,
        undo_first: bool,
        cx: &mut Context<Self>,
    ) {
        let sid = self.thread.session_id.clone();
        let text = self.modal_textarea.read(cx).value().trim().to_owned();
        if sid.is_empty() || text.is_empty() {
            return;
        }
        self.active_modal = None;
        self.context_breakdown = None;
        let sid_check = sid.clone();
        let task = spawn_async_into(
            cx,
            move |state| async move {
                agent_message_edit_impl(&state, &sid, &message_id, text, Vec::new(), undo_first)
                    .await
            },
            move |this, res, cx| {
                if this.thread.session_id != sid_check {
                    return;
                }
                if let Err(e) = res {
                    this.status_msg = Some((Tone::Danger, e.message().to_owned()));
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

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

    pub fn toggle_trace(&mut self, key: String, cx: &mut Context<Self>) {
        if !self.expanded_traces.remove(&key) {
            self.expanded_traces.insert(key);
        }
        cx.notify();
    }

    /// Toggles the collapsed/expanded state of a turn's activity spine (`turn-fold-{turn_id}`).
    pub fn toggle_turn_fold(&mut self, turn_id: String, cx: &mut Context<Self>) {
        let key = format!("turn-fold-{turn_id}");
        if !self.expanded_traces.remove(&key) {
            self.expanded_traces.insert(key);
        }
        cx.notify();
    }

    /// Returns true if the turn associated with `message_id` (user or assistant) has active
    /// (non-undone) file changes that can be reverted before retrying or editing.
    #[must_use]
    pub fn turn_has_active_changes(&self, message_id: &str) -> bool {
        use spanbay_desktop::core::agent::model::FileChangeStatus;
        self.cached_turn_entries().iter().any(|entry| match entry {
            spanbay_desktop::state::ThreadTurnEntry::AssistantTurn(turn) => {
                let matches_turn = turn.user_message_id.as_deref() == Some(message_id)
                    || turn.first_assistant_id.as_deref() == Some(message_id)
                    || turn.last_assistant_id.as_deref() == Some(message_id)
                    || turn.message_indices.iter().any(|&ix| {
                        self.thread
                            .messages
                            .get(ix)
                            .is_some_and(|m| m.id == message_id)
                    });
                matches_turn
                    && turn
                        .turn_changes
                        .iter()
                        .any(|c| c.status != FileChangeStatus::Undone)
            }
            _ => false,
        })
    }

    pub fn toggle_reasoning(&mut self, key: String, cx: &mut Context<Self>) {
        if key == "live-reasoning" {
            self.collapsed_live_reasoning = !self.collapsed_live_reasoning;
        } else if !self.expanded_reasoning.remove(&key) {
            self.expanded_reasoning.insert(key);
        }
        cx.notify();
    }

    pub fn toggle_all_reasoning(&mut self, cx: &mut Context<Self>) {
        let keys: Vec<String> = self
            .thread
            .messages
            .iter()
            .filter(|m| message_has_reasoning(m))
            .map(|m| format!("reason-{}", m.id))
            .collect();
        if keys.is_empty() {
            return;
        }
        let all_open = keys.iter().all(|k| self.expanded_reasoning.contains(k));
        if all_open {
            for k in keys {
                self.expanded_reasoning.remove(&k);
            }
        } else {
            for k in keys {
                self.expanded_reasoning.insert(k);
            }
        }
        cx.notify();
    }
    /// Ensures `self.environments` matches the currently bound (or active fallback) workspace.
    pub(super) fn ensure_bound_workspace_envs(&mut self, cx: &mut Context<Self>) {
        let bound_ws_id = match &self.session {
            Some(s) => s.spanbay_workspace_id.clone(),
            None => self.active_workspace_id.clone(),
        };
        let Some(wid) = bound_ws_id else {
            if !self.environments.is_empty() {
                self.environments.clear();
                cx.notify();
            }
            return;
        };
        if !self.environments.is_empty() && self.environments.iter().all(|e| e.workspace_id == wid)
        {
            return;
        }
        let sid_check = self.thread.session_id.clone();
        let wid_check = wid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| environment_list_impl(state, wid).unwrap_or_default(),
            move |this, envs, cx| {
                let current_ws = match &this.session {
                    Some(s) => s.spanbay_workspace_id.as_deref(),
                    None => this.active_workspace_id.as_deref(),
                };
                if this.thread.session_id == sid_check && current_ws == Some(wid_check.as_str()) {
                    this.environments = envs;
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }

    pub fn export_chat_redacted(&mut self, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            return;
        }
        let default_name = format!("{}.md", self.thread.title.replace('/', "_"));
        let task = spawn_blocking_into(
            cx,
            move |state| {
                let spec = SaveFileSpec {
                    title: None,
                    starting_dir: None,
                    file_name: Some(default_name),
                    filters: vec![FileFilter {
                        name: "Markdown / JSON".into(),
                        extensions: vec!["md".into(), "json".into()],
                    }],
                };
                let Some(dest) = RfdDialog.save_file(&spec) else {
                    return Ok(None);
                };
                let format = if dest
                    .extension()
                    .and_then(|e| e.to_str())
                    .is_some_and(|e| e.eq_ignore_ascii_case("json"))
                {
                    AgentExportFormat::Json
                } else {
                    AgentExportFormat::Markdown
                };
                let text = agent_session_export_redacted_impl(state, &sid, format, Some(true))?;
                std::fs::write(&dest, text).map_err(|e| {
                    spanbay_desktop::core::error::AppError::io(format!("export write failed: {e}"))
                })?;
                Ok::<Option<String>, spanbay_desktop::core::error::AppError>(Some(
                    dest.display().to_string(),
                ))
            },
            |this, res, cx| {
                match res {
                    Ok(Some(path)) => {
                        this.status_msg =
                            Some((Tone::Success, crate::ui::tr(cx).chat().act_exported(path)));
                    }
                    Ok(None) => {}
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn delete_session_by_id(&mut self, sid: String, cx: &mut Context<Self>) {
        let sid_check = sid.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| agent_session_delete_impl(state, &sid),
            move |this, res, cx| {
                if res.is_ok() {
                    if this.thread.session_id == sid_check {
                        this.reset_session_view(String::new(), None, cx);
                    }
                    this.reload_sessions(cx);
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }
}

/// Neutral wording of how an approval was answered (a timeout or denial already shown as the row's
/// status returns `None`).
pub(super) fn resolution_label(
    r: ApprovalResolution,
    status: GraphNodeStatus,
    cx: &gpui_kit::App,
) -> Option<&'static str> {
    if r.timed_out || (r.decision == ApprovalDecision::Deny && status == GraphNodeStatus::Denied) {
        return None;
    }
    let ch = crate::ui::tr(cx).chat();
    Some(match (r.decision, r.scope) {
        (ApprovalDecision::Deny, _) => ch.trace_resolution_denied(),
        (ApprovalDecision::Allow, ApprovalScope::Once) => ch.trace_resolution_once(),
        (ApprovalDecision::Allow, ApprovalScope::Session) => ch.trace_resolution_session(),
        (ApprovalDecision::Allow, ApprovalScope::WorkspacePattern) => ch.trace_resolution_folder(),
        (ApprovalDecision::Allow, ApprovalScope::Tool) => ch.trace_resolution_global(),
    })
}

/// End-of-turn marker for a run that ended without its answer: cancelled, interrupted, or
/// stopped early for a reason the last turn's footer does not already show.
pub(super) fn stop_marker(view: &ChatView) -> Option<(GraphNodeStatus, Option<AgentStopReason>)> {
    let run = view.thread.active_run.as_ref()?;
    match run.status {
        AgentRunStatus::Cancelled => Some((GraphNodeStatus::Cancelled, None)),
        AgentRunStatus::Interrupted => Some((GraphNodeStatus::Interrupted, None)),
        AgentRunStatus::Completed => {
            let stop = view
                .thread
                .last_stop_reason
                .filter(|r| *r != AgentStopReason::Completed)?;
            let footer_has_it = view
                .thread
                .messages
                .iter()
                .rev()
                .find(|m| m.role == AgentRole::Assistant)
                .is_some_and(|m| m.meta.stop_reason == Some(stop));
            (!footer_has_it).then_some((GraphNodeStatus::Interrupted, Some(stop)))
        }
        _ => None,
    }
}

pub(super) fn render_stop_marker(view: &ChatView, cx: &mut Context<ChatView>) -> AnyElement {
    let (Some((status, reason)), Some(run)) = (stop_marker(view), view.thread.active_run.as_ref())
    else {
        return div().into_any_element();
    };
    let ch = crate::ui::tr(cx).chat();
    let ran = run
        .finished_at_ms
        .map(|end| format_duration_ms(u64::try_from(end - run.started_at_ms).unwrap_or(0)));
    let mut files: Vec<&str> = view
        .persisted_changes
        .iter()
        .filter(|c| c.run_id == run.run_id)
        .map(|c| c.path.as_str())
        .collect();
    files.sort_unstable();
    files.dedup();
    let cost_suffix = format_cost_micro(run.cost_micro_usd, run.cost_currency.as_deref())
        .map_or_else(String::new, |c| format!(" · {c}"));
    let status_word = status_label(status, None, cx);
    let title = match (reason, ran) {
        (Some(AgentStopReason::MaxIterations), _) => ch.run_ended_max_steps(run.iterations),
        (Some(r), _) => stop_reason_label(r, cx).unwrap_or(status_word).to_owned(),
        (None, Some(time)) if status == GraphNodeStatus::Cancelled => {
            ch.run_ended_cancelled(time, run.iterations, files.len(), cost_suffix)
        }
        (None, Some(time)) => ch.run_ended_interrupted(time),
        (None, None) => status_word.to_owned(),
    };
    let meta = (reason.is_none() && run.iterations == 0 && files.is_empty())
        .then(|| SharedString::from(ch.run_ended_nothing()));
    let review = (reason.is_none() && !files.is_empty()).then(|| {
        btn_ghost("chat-stop-review", ch.run_ended_review())
            .on_click(cx.listener(|this, _, _, cx| {
                this.open_review(None, cx);
            }))
            .into_any_element()
    });
    let p = palette(cx);
    let (shape, _) = status_visual(status);
    let (ink, faint) = (to_hsla(p.ink), to_hsla(p.faint));
    h_row()
        .id("chat-stop-marker")
        .w_full()
        .gap(px(6.0))
        .px(px(10.0))
        .py(px(6.0))
        .rounded(px(6.0))
        .bg(to_hsla(p.pane))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(status_indicator(shape, cx))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .text_xs()
                .text_color(ink)
                .child(SharedString::from(title)),
        )
        .children(meta.map(|m| div().flex_shrink_0().text_xs().text_color(faint).child(m)))
        .children(review.map(|r| div().flex_shrink_0().child(r).into_any_element()))
        .into_any_element()
}
