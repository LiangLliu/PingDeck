//! Pending tool approvals: the approval facade over [`HumanGate`](super::gate::HumanGate) (the
//! registry, the wait and the timeout rule of `docs/architecture.md` §11.2, `docs/decisions.md`
//! §5 D-119), plus the per-chat "Run without a sandbox" acknowledgement (process lifetime only).
//!
//! Single responsibility: hand one [`ApprovalAnswer`] from `agent_approval_answer` to the run
//! waiting for it. A watched run (a live subscriber and a focused window) never times out; an
//! unwatched run, or a sub-agent whose parent chat is unwatched, times out after
//! `ai.agent.approval_timeout_s`. Writing permission rules for the answer's scope is the
//! use-case's job (`app::agent_service::run`); this module stores only what it needs
//! ([`RuleContext`]).

pub use pingdeck_model::agent::approvals::RuleContext;

pub use super::gate::GateWatch as ApprovalGate;

use std::collections::HashSet;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use pingdeck_base::cancel::CancelToken;

use super::attention::PendingApprovalSummary;
use super::gate::{
    GateAnswer, GateKind, GateOutcome, GatePayload, GateRequest, GateTicket, HumanGate,
};
use super::model::{ApprovalDecision, ApprovalScope};

/// The user's answer to one approval request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApprovalAnswer {
    /// Allow or deny.
    pub decision: ApprovalDecision,
    /// How far the answer reaches.
    pub scope: ApprovalScope,
    /// A note for the model ("use the staging URL instead"), returned with the tool result.
    #[serde(default)]
    pub message: Option<String>,
    /// Arguments the user edited on the card; the run re-validates them before running.
    #[serde(default)]
    pub edited_arguments: Option<String>,
    /// Run this one shell call with network access ("Allow once with network").
    #[serde(default)]
    pub allow_network_once: bool,
}

/// How a wait for an approval ended.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ApprovalOutcome {
    /// The user answered.
    Answered(ApprovalAnswer),
    /// Nobody answered in time (only an unwatched run can time out).
    TimedOut,
    /// A steer message arrived: the call never runs.
    Superseded,
    /// The run was cancelled, or the request was dropped.
    Cancelled,
}

/// Every approval waiting for an answer, by (run id, call id), and the chats that acknowledged
/// "Run without a sandbox".
#[derive(Default)]
pub struct Approvals {
    gates: Arc<HumanGate>,
    /// Chats that chose to run commands without the sandbox. Never persisted: the choice lasts
    /// for the app process and is never inherited by a new chat.
    sandbox_off: Mutex<HashSet<String>>,
}

/// A registered request; dropping it unanswered forgets the request.
pub struct ApprovalTicket(GateTicket);

impl Approvals {
    /// Approvals kept in `gates`, beside the other kinds of wait.
    #[must_use]
    pub fn over(gates: Arc<HumanGate>) -> Self {
        Self {
            gates,
            sandbox_off: Mutex::default(),
        }
    }

    /// Registers a request for `summary.run_id` / `summary.call_id`, before it is announced so an
    /// instant answer is never lost. `None` when that call already has a pending request.
    #[must_use]
    pub fn register(
        &self,
        summary: PendingApprovalSummary,
        context: RuleContext,
    ) -> Option<ApprovalTicket> {
        let request = GateRequest {
            gate_id: summary.call_id.clone(),
            run_id: summary.run_id.clone(),
            session_id: summary.session_id.clone(),
            kind: GateKind::Approval,
            opened_ms: summary.requested_ms,
            payload: GatePayload::Approval {
                summary: Box::new(summary),
                context,
            },
        };
        self.gates.open(request).map(ApprovalTicket)
    }

    /// Claims the call for one answer and returns what a rule for its scope is made from. `None`
    /// when nothing waits for the call or another answer is already being saved, so two answers
    /// never both persist a rule. Follow with [`Approvals::deliver`], or give the claim back with
    /// [`Approvals::abort_answer`].
    #[must_use]
    pub fn begin_answer(&self, run_id: &str, call_id: &str) -> Option<RuleContext> {
        match self.gates.begin_answer(run_id, call_id)? {
            GatePayload::Approval { context, .. } => Some(context),
            _ => {
                self.gates.abort_answer(run_id, call_id);
                None
            }
        }
    }

    /// Gives back the claim of [`Approvals::begin_answer`]: the answer could not be saved and the
    /// call keeps waiting for another.
    pub fn abort_answer(&self, run_id: &str, call_id: &str) {
        self.gates.abort_answer(run_id, call_id);
    }

    /// Delivers `answer`; `false` when nothing waits for the call (answered, timed out, gone).
    pub fn deliver(&self, run_id: &str, call_id: &str, answer: ApprovalAnswer) -> bool {
        self.gates
            .deliver(run_id, call_id, GateAnswer::Approval(answer))
    }

    /// Whether `session_id` acknowledged "Run without a sandbox".
    #[must_use]
    pub fn sandbox_acknowledged(&self, session_id: &str) -> bool {
        self.sandbox_off
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .contains(session_id)
    }

    /// Records (`on`) or withdraws the chat's "Run without a sandbox" acknowledgement.
    pub fn acknowledge_sandbox(&self, session_id: &str, on: bool) {
        let mut set = self
            .sandbox_off
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if on {
            set.insert(session_id.to_owned());
        } else {
            set.remove(session_id);
        }
    }

    /// Every pending request, oldest first.
    #[must_use]
    pub fn list(&self) -> Vec<PendingApprovalSummary> {
        self.gates
            .list(GateKind::Approval)
            .into_iter()
            .filter_map(|request| match request.payload {
                GatePayload::Approval { summary, .. } => Some(*summary),
                _ => None,
            })
            .collect()
    }

    /// Whether `run_id` has a pending request.
    #[must_use]
    pub fn has_pending(&self, run_id: &str) -> bool {
        self.gates.has_open(run_id, GateKind::Approval)
    }
}

impl ApprovalTicket {
    /// Waits for the answer under the gate's timeout rule ([`super::gate::watched`]).
    pub async fn wait(
        self,
        gate: &dyn ApprovalGate,
        cancel: &CancelToken,
        timeout: Duration,
    ) -> ApprovalOutcome {
        match self.0.wait(gate, cancel, timeout).await {
            GateOutcome::Answered(GateAnswer::Approval(answer)) => {
                ApprovalOutcome::Answered(answer)
            }
            GateOutcome::Answered(_) | GateOutcome::Cancelled => ApprovalOutcome::Cancelled,
            GateOutcome::TimedOut => ApprovalOutcome::TimedOut,
            GateOutcome::Superseded => ApprovalOutcome::Superseded,
        }
    }
}

#[cfg(test)]
mod tests;
