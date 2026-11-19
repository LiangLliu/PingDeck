//! The global attention feed: what needs the user across every chat.
//!
//! Single responsibility: the [`AgentAttention`] value pushed through `AgentRuntime::attention_hub`
//! whenever a run's status, an approval queue or the set of unopened finished runs changes. It
//! carries the three counters of the feed (approvals waiting, runs active, runs finished and not
//! yet opened), the waiting approvals oldest first, the active chats and the latest ended runs;
//! [`AgentAttention::signals_since`] derives the away-from-app signals (an approval is needed, a
//! run of 30 s or more ended) the shell turns into a dock bounce and a system notification.
//! Computing the snapshot is `AgentRuntime`'s job (it owns the run registry and the approvals);
//! this module defines the shape and the pure assembly.

use serde::{Deserialize, Serialize};

use super::model::AgentRunStatus;
use super::permissions::ApprovalCard;
use super::tools::ToolApprovalPreview;

/// A run at least this long (awake time) that ends is worth an away-from-app signal.
pub const LONG_RUN_MS: u64 = 30_000;

/// One approval waiting for the user.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PendingApprovalSummary {
    /// The waiting run.
    pub run_id: String,
    /// Its chat.
    pub session_id: String,
    /// The call.
    pub call_id: String,
    /// The tool.
    pub tool: String,
    /// Why approval is needed.
    pub reason: String,
    /// What the card shows.
    pub preview: Option<ToolApprovalPreview>,
    /// When the request was made, Unix milliseconds.
    pub requested_ms: i64,
    /// `low`, `medium` or `high`.
    pub risk: String,
    /// One plain line of what the call does (a board card shows it).
    pub summary: String,
    /// The whole command, URL or argument text, for a Low-risk call only: the board offers an
    /// inline Allow exactly when this is present.
    pub full_text: Option<String>,
    /// The whole card (risk reason, tags, operators, scopes), so the board needs no chat.
    pub card: Option<ApprovalCard>,
}

/// One chat's run state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionRunStateDto {
    /// The chat.
    pub session_id: String,
    /// Its top-level run.
    pub run_id: String,
    /// The run's status.
    pub status: AgentRunStatus,
    /// What the run was doing last.
    pub last_step: Option<String>,
    /// The chat's queue waits for a resume.
    pub queue_paused: bool,
    /// What a waiting run waits for (`approval`, `plan`, `cost limit`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_detail: Option<String>,
}

/// A top-level run that ended.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FinishedRun {
    /// The run.
    pub run_id: String,
    /// Its chat.
    pub session_id: String,
    /// How it ended.
    pub status: AgentRunStatus,
    /// How long it ran, without machine sleep, in milliseconds.
    pub duration_ms: u64,
    /// When it ended, Unix milliseconds.
    pub finished_at_ms: i64,
}

/// Why the shell should draw the user's attention while the window is not focused. Never carries
/// command text.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttentionSignal {
    /// A new approval is waiting.
    ApprovalNeeded,
    /// A run that lasted [`LONG_RUN_MS`] or more completed or failed.
    RunFinished,
}

/// What needs attention now.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct AgentAttention {
    /// Approvals waiting (also `approvals.len()`; the dock badge).
    pub waiting_approval: u32,
    /// Runs that have not ended: queued, running or waiting, sub-agents included.
    pub active_runs: u32,
    /// Top-level runs that ended and that the user has not opened.
    pub unseen_finished: u32,
    /// The waiting approvals, oldest first.
    pub approvals: Vec<PendingApprovalSummary>,
    /// Every active top-level run, by chat.
    pub sessions: Vec<SessionRunStateDto>,
    /// The latest ended top-level runs, newest first.
    pub finished: Vec<FinishedRun>,
    /// Questions waiting (`ask_user`).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub waiting_question: u32,
    /// Plans waiting for approval.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub waiting_plan: u32,
    /// Runs paused at a cost or token cap.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub cost_paused: u32,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_zero(value: &u32) -> bool {
    *value == 0
}

fn count(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// A plan, a question or a cost pause that a chat began waiting on, for the away-from-app signals.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GateSignal {
    /// A plan waits for approval.
    Plan,
    /// The agent asks the user a question.
    Question,
    /// The run paused at a cost cap.
    Cost,
}

impl AgentAttention {
    /// Assembles a snapshot; `approvals` are sorted oldest first and `sessions` by chat id so equal
    /// states compare equal.
    #[must_use]
    pub fn assemble(
        mut approvals: Vec<PendingApprovalSummary>,
        mut sessions: Vec<SessionRunStateDto>,
        active_runs: usize,
        unseen_finished: usize,
        finished: Vec<FinishedRun>,
    ) -> Self {
        approvals.sort_by(|a, b| (a.requested_ms, &a.call_id).cmp(&(b.requested_ms, &b.call_id)));
        sessions.sort_by(|a, b| a.session_id.cmp(&b.session_id));
        Self {
            waiting_approval: count(approvals.len()),
            active_runs: count(active_runs),
            unseen_finished: count(unseen_finished),
            approvals,
            sessions,
            finished,
            ..Self::default()
        }
    }

    /// The same snapshot with the plans, questions and cost pauses waiting.
    #[must_use]
    pub fn with_gates(mut self, plans: usize, questions: usize, cost_pauses: usize) -> Self {
        self.waiting_plan = count(plans);
        self.waiting_question = count(questions);
        self.cost_paused = count(cost_pauses);
        self
    }

    /// The chats whose run began waiting on a plan, a question or a cost pause since `previous`.
    pub fn gates_since<'a>(
        &'a self,
        previous: &'a Self,
    ) -> impl Iterator<Item = (&'a str, GateSignal)> {
        self.sessions.iter().filter_map(|session| {
            let signal = match session.status_detail.as_deref() {
                Some("plan") => GateSignal::Plan,
                Some("question") => GateSignal::Question,
                Some("cost limit") => GateSignal::Cost,
                _ => return None,
            };
            let known = previous.sessions.iter().any(|before| {
                before.run_id == session.run_id && before.status_detail == session.status_detail
            });
            (!known && session.status == AgentRunStatus::WaitingApproval)
                .then_some((session.session_id.as_str(), signal))
        })
    }

    /// The approvals of this snapshot that `previous` did not have, oldest first.
    pub fn approvals_since<'a>(
        &'a self,
        previous: &'a Self,
    ) -> impl Iterator<Item = &'a PendingApprovalSummary> {
        self.approvals.iter().filter(|approval| {
            !previous
                .approvals
                .iter()
                .any(|known| known.run_id == approval.run_id && known.call_id == approval.call_id)
        })
    }

    /// The runs of [`LONG_RUN_MS`] or more that completed or failed since `previous`.
    pub fn long_runs_ended_since<'a>(
        &'a self,
        previous: &'a Self,
    ) -> impl Iterator<Item = &'a FinishedRun> {
        self.finished.iter().filter(|run| {
            run.duration_ms >= LONG_RUN_MS
                && matches!(
                    run.status,
                    AgentRunStatus::Completed | AgentRunStatus::Failed
                )
                && !previous
                    .finished
                    .iter()
                    .any(|known| known.run_id == run.run_id)
        })
    }

    /// The signals this snapshot adds to `previous`: one [`AttentionSignal::ApprovalNeeded`] when
    /// an approval appeared, one [`AttentionSignal::RunFinished`] when a run of
    /// [`LONG_RUN_MS`] or more completed or failed.
    #[must_use]
    pub fn signals_since(&self, previous: &Self) -> Vec<AttentionSignal> {
        let mut signals = Vec::new();
        if self.approvals_since(previous).next().is_some() {
            signals.push(AttentionSignal::ApprovalNeeded);
        }
        if self.long_runs_ended_since(previous).next().is_some() {
            signals.push(AttentionSignal::RunFinished);
        }
        signals
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn approval(call: &str, at: i64) -> PendingApprovalSummary {
        PendingApprovalSummary {
            run_id: "r".to_owned(),
            session_id: "s".to_owned(),
            call_id: call.to_owned(),
            tool: "shell".to_owned(),
            reason: String::new(),
            preview: None,
            requested_ms: at,
            risk: "low".to_owned(),
            summary: "Run: git status".to_owned(),
            full_text: Some("git status".to_owned()),
            card: None,
        }
    }

    fn finished(run: &str, status: AgentRunStatus, duration_ms: u64) -> FinishedRun {
        FinishedRun {
            run_id: run.to_owned(),
            session_id: "s".to_owned(),
            status,
            duration_ms,
            finished_at_ms: 1,
        }
    }

    #[test]
    fn approvals_are_listed_oldest_first_and_counted_beside_the_other_counters() {
        let snapshot = AgentAttention::assemble(
            vec![approval("b", 20), approval("a", 10)],
            Vec::new(),
            3,
            1,
            Vec::new(),
        );
        assert_eq!(
            snapshot
                .approvals
                .iter()
                .map(|a| a.call_id.as_str())
                .collect::<Vec<_>>(),
            ["a", "b"]
        );
        assert_eq!(
            (
                snapshot.waiting_approval,
                snapshot.active_runs,
                snapshot.unseen_finished
            ),
            (2, 3, 1)
        );
    }

    #[test]
    fn equal_states_compare_equal_whatever_the_input_order() {
        let sessions = |order: [&str; 2]| {
            order
                .into_iter()
                .map(|id| SessionRunStateDto {
                    session_id: id.to_owned(),
                    run_id: format!("run-{id}"),
                    status: AgentRunStatus::Running,
                    last_step: None,
                    queue_paused: false,
                    status_detail: None,
                })
                .collect::<Vec<_>>()
        };
        assert_eq!(
            AgentAttention::assemble(Vec::new(), sessions(["a", "b"]), 2, 0, Vec::new()),
            AgentAttention::assemble(Vec::new(), sessions(["b", "a"]), 2, 0, Vec::new())
        );
    }

    #[test]
    fn a_new_approval_and_a_long_run_ending_each_signal_once() {
        let before = AgentAttention::default();
        let after = AgentAttention::assemble(
            vec![approval("a", 1), approval("b", 2)],
            Vec::new(),
            0,
            1,
            vec![
                finished("long", AgentRunStatus::Completed, 45_000),
                finished("failed", AgentRunStatus::Failed, 31_000),
            ],
        );
        assert_eq!(
            after.signals_since(&before),
            [
                AttentionSignal::ApprovalNeeded,
                AttentionSignal::RunFinished
            ]
        );
        // The same snapshot again adds nothing.
        assert!(after.signals_since(&after).is_empty());
    }

    #[test]
    fn a_short_a_cancelled_or_an_already_known_run_makes_no_signal() {
        let known = finished("old", AgentRunStatus::Completed, 90_000);
        let before = AgentAttention::assemble(Vec::new(), Vec::new(), 0, 0, vec![known.clone()]);
        let after = AgentAttention::assemble(
            Vec::new(),
            Vec::new(),
            0,
            3,
            vec![
                finished("short", AgentRunStatus::Completed, 10_000),
                finished("stopped", AgentRunStatus::Cancelled, 60_000),
                finished("crash", AgentRunStatus::Interrupted, 60_000),
                known,
            ],
        );
        assert!(after.signals_since(&before).is_empty());
    }
}
