//! `HumanGate`: every wait for the user (an approval, a cost pause, a plan, a question) in
//! one registry with one wait and one timeout rule (`docs/architecture.md` §11.2).
//!
//! Single responsibility: hold the open gates by (run id, gate id), hand one [`GateAnswer`] to the
//! run waiting on it, and decide how long a run waits. A gate is watched when its chat has a live
//! subscriber **and** the main window has focus; a watched gate never times out, an unwatched one
//! times out after `ai.agent.approval_timeout_s`. The clock runs while nobody watches and starts
//! from zero each time watching stops ([`watched`]). Freeing the run's slot, the run status and
//! the events are the host's job; [`Approvals`](super::approvals::Approvals) is the approval
//! facade over this registry.

use std::collections::HashMap;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;
use tokio::sync::{oneshot, watch};

use spanbay_base::cancel::CancelToken;
use spanbay_model::agent::approvals::RuleContext;
use spanbay_model::settings::ToolApproval;

use super::approvals::ApprovalAnswer;
use super::attention::PendingApprovalSummary;
use super::model::{AgentQuestionAnswer, HumanGateDto};
use super::registry::CostDecision;

/// What a gate waits for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum GateKind {
    /// A tool call needs the user's approval.
    Approval,
    /// The model asks the user a question (`ask_user`).
    Question,
    /// The model submitted a plan (`submit_plan`).
    Plan,
    /// The run tree reached a cost or token cap.
    Cost,
}

impl GateKind {
    /// `AgentRunSummary::status_detail` of a run waiting on this kind.
    #[must_use]
    pub const fn status_detail(self) -> &'static str {
        match self {
            Self::Approval => "approval",
            Self::Question => "question",
            Self::Plan => "plan",
            Self::Cost => "cost limit",
        }
    }

    /// Whether a steer message ends the wait: the user typed instead of answering. A cost pause
    /// keeps waiting (the steer goes with the next call).
    const fn steerable(self) -> bool {
        !matches!(self, Self::Cost)
    }
}

/// What a gate shows, kept while it is open.
#[derive(Debug, Clone)]
pub enum GatePayload {
    /// The approval card and what a rule for its scope is made from.
    Approval {
        /// The card as the attention feed lists it.
        summary: Box<PendingApprovalSummary>,
        /// What a rule for the answer's scope is made from.
        context: RuleContext,
    },
    /// The plan card.
    Plan(HumanGateDto),
    /// The question card.
    Question(HumanGateDto),
    /// A cost pause (its figures went out with `CostLimitReached`).
    Cost,
}

/// One wait for the user.
#[derive(Debug, Clone)]
pub struct GateRequest {
    /// Approvals, plans and questions: the call id; cost: `cost:<n>`.
    pub gate_id: String,
    /// The waiting run.
    pub run_id: String,
    /// Its chat.
    pub session_id: String,
    /// What it waits for.
    pub kind: GateKind,
    /// What it shows.
    pub payload: GatePayload,
    /// When it opened, Unix milliseconds.
    pub opened_ms: i64,
}

/// The user's choice on a plan.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanDecision {
    /// Carry the plan out in `mode`.
    Approve,
    /// Keep planning with the user's additions.
    Revise,
}

/// An answer to a gate. Internal to the core; the IPC keeps one command per kind.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateAnswer {
    /// `agent_approval_answer`.
    Approval(ApprovalAnswer),
    /// `agent_plan_approve`, or the user's additions.
    Plan {
        /// Approve or revise.
        decision: PlanDecision,
        /// The permission mode the approved plan runs in.
        mode: Option<ToolApproval>,
        /// The localised first message of the next run, or the additions.
        text: String,
    },
    /// `agent_run_continue`.
    Cost(CostDecision),
    /// `agent_question_answer_save`.
    Question(AgentQuestionAnswer),
    /// The user dismissed the gate.
    Skipped,
}

/// How a wait ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum GateOutcome {
    /// The user answered.
    Answered(GateAnswer),
    /// Nobody watched for the whole timeout.
    TimedOut,
    /// A steer message arrived instead of an answer.
    Superseded,
    /// The run was cancelled, or the gate was dropped.
    Cancelled,
}

/// What a wait needs to know about the world outside the gate.
pub trait GateWatch: Send + Sync {
    /// Live subscribers of the run's chat.
    fn watchers(&self) -> usize;
    /// Whether the main window has focus.
    fn focused(&self) -> bool {
        true
    }
    /// Changes whenever the subscribers or the focus may have changed.
    fn changes(&self) -> watch::Receiver<u64>;
    /// A timer for `duration`.
    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()>;
    /// Resolves when the user steered the run.
    fn steered(&self) -> BoxFuture<'_, ()>;
}

/// The timeout rule: someone is looking when the chat has a subscriber and the window has focus.
/// Only an unwatched gate runs a clock.
#[must_use]
pub const fn watched(watchers: usize, focused: bool) -> bool {
    watchers > 0 && focused
}

struct Entry {
    request: GateRequest,
    sender: oneshot::Sender<GateAnswer>,
    /// An answer is being saved; no second one may start.
    answering: bool,
}

type Key = (String, String);
type Open = Arc<Mutex<HashMap<Key, Entry>>>;

fn lock(open: &Mutex<HashMap<Key, Entry>>) -> MutexGuard<'_, HashMap<Key, Entry>> {
    open.lock().unwrap_or_else(PoisonError::into_inner)
}

/// Every open gate of every run.
#[derive(Default)]
pub struct HumanGate {
    open: Open,
}

/// A registered gate; dropping it unanswered closes the gate.
pub struct GateTicket {
    receiver: oneshot::Receiver<GateAnswer>,
    key: Key,
    kind: GateKind,
    open: Open,
}

impl Drop for GateTicket {
    fn drop(&mut self) {
        lock(&self.open).remove(&self.key);
    }
}

impl HumanGate {
    /// Opens `request`'s gate, before it is announced so an instant answer is never lost. `None`
    /// when that gate is already open.
    #[must_use]
    pub fn open(&self, request: GateRequest) -> Option<GateTicket> {
        let key = (request.run_id.clone(), request.gate_id.clone());
        let kind = request.kind;
        let mut open = lock(&self.open);
        if open.contains_key(&key) {
            return None;
        }
        let (sender, receiver) = oneshot::channel();
        open.insert(
            key.clone(),
            Entry {
                request,
                sender,
                answering: false,
            },
        );
        Some(GateTicket {
            receiver,
            key,
            kind,
            open: Arc::clone(&self.open),
        })
    }

    /// Claims the gate for one answer and returns what it shows. `None` when nothing waits on it
    /// or another answer is being saved. Follow with [`HumanGate::deliver`] or
    /// [`HumanGate::abort_answer`].
    #[must_use]
    pub fn begin_answer(&self, run_id: &str, gate_id: &str) -> Option<GatePayload> {
        let mut open = lock(&self.open);
        let entry = open.get_mut(&(run_id.to_owned(), gate_id.to_owned()))?;
        if entry.answering {
            return None;
        }
        entry.answering = true;
        Some(entry.request.payload.clone())
    }

    /// Gives back the claim of [`HumanGate::begin_answer`].
    pub fn abort_answer(&self, run_id: &str, gate_id: &str) {
        if let Some(entry) = lock(&self.open).get_mut(&(run_id.to_owned(), gate_id.to_owned())) {
            entry.answering = false;
        }
    }

    /// Delivers `answer`; `false` when nothing waits on the gate (answered, timed out, gone).
    pub fn deliver(&self, run_id: &str, gate_id: &str, answer: GateAnswer) -> bool {
        lock(&self.open)
            .remove(&(run_id.to_owned(), gate_id.to_owned()))
            .is_some_and(|entry| entry.sender.send(answer).is_ok())
    }

    /// The id of `run_id`'s open gate of `kind`, the oldest when there are several.
    #[must_use]
    pub fn open_gate(&self, run_id: &str, kind: GateKind) -> Option<String> {
        lock(&self.open)
            .values()
            .filter(|entry| entry.request.run_id == run_id && entry.request.kind == kind)
            .min_by_key(|entry| entry.request.opened_ms)
            .map(|entry| entry.request.gate_id.clone())
    }

    /// Reads a clone of the payload stored on an open gate without claiming or removing it.
    #[must_use]
    pub fn open_payload(&self, run_id: &str, gate_id: &str) -> Option<GatePayload> {
        lock(&self.open)
            .get(&(run_id.to_owned(), gate_id.to_owned()))
            .map(|entry| entry.request.payload.clone())
    }

    /// The open gates of `kind`, oldest first.
    #[must_use]
    pub fn list(&self, kind: GateKind) -> Vec<GateRequest> {
        let mut all: Vec<GateRequest> = lock(&self.open)
            .values()
            .filter(|entry| entry.request.kind == kind)
            .map(|entry| entry.request.clone())
            .collect();
        all.sort_by(|a, b| (a.opened_ms, &a.gate_id).cmp(&(b.opened_ms, &b.gate_id)));
        all
    }

    /// Open gates of `kind`.
    #[must_use]
    pub fn count(&self, kind: GateKind) -> usize {
        lock(&self.open)
            .values()
            .filter(|entry| entry.request.kind == kind)
            .count()
    }

    /// Whether `run_id` has an open gate of `kind`.
    #[must_use]
    pub fn has_open(&self, run_id: &str, kind: GateKind) -> bool {
        lock(&self.open)
            .values()
            .any(|entry| entry.request.run_id == run_id && entry.request.kind == kind)
    }
}

impl GateTicket {
    /// Waits for the answer under the timeout rule: while [`watched`] is false a timer of
    /// `timeout` runs; watching again cancels it, and the next unwatched spell starts from zero.
    pub async fn wait(
        mut self,
        watch: &dyn GateWatch,
        cancel: &CancelToken,
        timeout: Duration,
    ) -> GateOutcome {
        let mut changes = watch.changes();
        let mut timer: Option<BoxFuture<'static, ()>> = None;
        let mut changes_open = true;
        let steerable = self.kind.steerable();
        loop {
            // Mark the current version seen before reading the state, so a change after the read
            // wakes the `changed()` below.
            changes.borrow_and_update();
            if watched(watch.watchers(), watch.focused()) {
                timer = None;
            } else if timer.is_none() {
                timer = Some(watch.sleep(timeout));
            }
            tokio::select! {
                answer = &mut self.receiver => {
                    return answer.map_or(GateOutcome::Cancelled, GateOutcome::Answered);
                }
                () = cancel.cancelled() => return GateOutcome::Cancelled,
                () = async {
                    if steerable {
                        watch.steered().await;
                    } else {
                        std::future::pending::<()>().await;
                    }
                } => return GateOutcome::Superseded,
                () = async {
                    match timer.as_mut() {
                        Some(timer) => timer.await,
                        None => std::future::pending().await,
                    }
                } => return GateOutcome::TimedOut,
                changed = async {
                    if changes_open {
                        changes.changed().await.is_ok()
                    } else {
                        std::future::pending().await
                    }
                } => {
                    changes_open = changed;
                }
            }
        }
    }
}

#[cfg(test)]
mod tests;
