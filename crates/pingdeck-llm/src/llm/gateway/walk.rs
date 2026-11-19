//! The failover driver of one gateway call.
//!
//! Single responsibility: walk the plan of a call with the shared failover rules
//! (`llm::failover`): skip targets that cannot carry the request, wait out the one `Retry-After`,
//! ask the circuit for each key, record every attempt in the call's [`Journal`] and in the health
//! book, and say when the walk is over and how. The attempt itself (JSON or stream) is the
//! caller's. The attempt budget, the circuit thresholds and the other limits are the values the
//! gateway started with (a change of the Failover settings applies after a restart).

use std::collections::HashSet;

use crate::llm::client::HealthGuard;
use crate::llm::client::retry_wait;
use crate::llm::events::acquire_announcing;
use crate::llm::failover::{AttemptPlan, FailureScope, Step, Verdict, observe};
use crate::llm::failure::FailureKind;
use crate::llm::health::{CircuitConfig, Outcome, Permit};
use crate::llm::key_book::error_line;
use crate::llm::router::{Plan, PlanError, Target};
use crate::llm::upstream::{AttemptError, UpstreamMeta};
use crate::llm::wire::Protocol;
use pingdeck_base::cancel::{CancelToken, until_cancelled};

use super::exchange::Sent;
use super::journal::Journal;
use super::server::Context;

/// A permitted attempt: the target (of group `group` of the plan) and the guard that records its
/// health outcome.
pub(super) struct Permitted {
    pub(super) group: usize,
    pub(super) target: Target,
    pub(super) guard: HealthGuard,
    /// The attempt repeats the previous one in place (`Retry-After` wait or same-target retry).
    pub(super) retry: bool,
}

/// What an attempt sent and learned, whether it worked or not.
pub(super) struct Spent {
    pub(super) protocol: Option<Protocol>,
    pub(super) meta: UpstreamMeta,
    pub(super) sent: Option<Sent>,
}

/// Why a target cannot take the request: the attempt-list reason and the 400 text for the client
/// when no other target remains.
pub(super) struct Refusal {
    pub(super) reason: String,
    pub(super) message: String,
}

/// Says before each attempt whether a target can take the request at all (a feature it cannot
/// translate); the answer is per provider model, so the gate may cache it per group.
pub(super) trait Gate: Send {
    /// `Some` when the target of group `group` must be skipped.
    fn refuse(&mut self, group: usize, target: &Target) -> Option<Refusal>;
}

/// How a failed attempt ended the walk.
pub(super) enum Stop {
    /// The upstream's error is the client's answer (a client error). `upstream` is the protocol
    /// it was spoken in (`None` when unknown): another dialect than the client's is re-encoded.
    Answer {
        error: AttemptError,
        upstream: Option<Protocol>,
    },
    /// The caller cancelled.
    Canceled,
    /// A fault of this side, which says nothing about the upstream.
    Fault(AttemptError),
}

/// One call's walk over its plan.
pub(super) struct Walk {
    plan: Plan,
    walk: AttemptPlan,
    circuit: CircuitConfig,
    /// The last failure that moved on.
    last: Option<AttemptError>,
    /// Keys that failed on the key itself in this call: later groups skip them.
    rejected: HashSet<String>,
    refused: HashSet<usize>,
    first_refusal: Option<String>,
}

impl Walk {
    /// A walk over `plan` with `max_retries` retries and the circuit settings of the start.
    pub(super) fn new(plan: Plan, max_retries: u8, circuit: CircuitConfig) -> Self {
        Self {
            walk: AttemptPlan::new(plan.key_counts(), max_retries).with_retry_wait(retry_wait()),
            plan,
            circuit,
            last: None,
            rejected: HashSet::new(),
            refused: HashSet::new(),
            first_refusal: None,
        }
    }

    /// The next target to call, after any `Retry-After` wait and the circuit check; `None` when
    /// the walk is over. `gate` says whether a target cannot take the request;
    /// such a group is skipped (once in the attempt list) without using an attempt.
    ///
    /// # Errors
    /// `()` when `cancel` fires during the wait.
    pub(super) async fn next(
        &mut self,
        context: &Context,
        journal: &mut Journal,
        cancel: Option<&CancelToken>,
        gate: &mut dyn Gate,
    ) -> Result<Option<Permitted>, ()> {
        loop {
            let (group, key, wait) = match self.walk.step() {
                Step::Stop(_) => return Ok(None),
                Step::Attempt { group, key, wait } => (group, key, wait),
            };
            let target = self.plan.groups[group].targets[key].clone();
            let retry = self.walk.retrying();
            if !retry && self.rejected.contains(&target.key_id) {
                journal.skipped(&target, false, "key rejected earlier in this request");
                self.walk.skipped();
                continue;
            }
            if let Some(refusal) = gate.refuse(group, &target) {
                if self.refused.insert(group) {
                    journal.skipped(&target, false, &refusal.reason);
                    self.first_refusal.get_or_insert(refusal.message);
                }
                self.walk.skipped();
                continue;
            }
            if let Some(wait) = wait
                && until_cancelled(cancel, tokio::time::sleep(wait))
                    .await
                    .is_none()
            {
                return Err(());
            }
            if acquire_announcing(&context.llm, &target.key_id, &target.model) == Permit::Blocked {
                journal.skipped(&target, true, "circuit open");
                self.walk.skipped();
                continue;
            }
            let guard = HealthGuard::with_config(&context.llm, &target, self.circuit);
            return Ok(Some(Permitted {
                group,
                target,
                guard,
                retry,
            }));
        }
    }

    /// Records the failure of `permitted`. `Some` ends the walk (the error is the answer, or the
    /// call was cancelled); `None` means the walk goes on.
    pub(super) fn failed(
        &mut self,
        journal: &mut Journal,
        permitted: Permitted,
        spent: Spent,
        error: AttemptError,
    ) -> Option<Stop> {
        let Permitted {
            target,
            mut guard,
            retry,
            ..
        } = permitted;
        if matches!(error, AttemptError::Cancelled) {
            guard.finish(Outcome::Canceled);
            journal.canceled(&target, spent.protocol, spent.meta, spent.sent);
            return Some(Stop::Canceled);
        }
        let Some(observation) = observe(&error) else {
            // A fault of this side says nothing about the upstream's health.
            guard.fault();
            journal.failed(&target, spent.protocol, spent.meta, &error, spent.sent);
            return Some(Stop::Fault(error));
        };
        let verdict = self.walk.failed(&observation, false);
        let judged = observation.assess();
        // A rate limit is about this model on the key; the key may still serve another model.
        if judged.scope == FailureScope::Key && judged.kind != FailureKind::RateLimit {
            self.rejected.insert(target.key_id.clone());
        }
        guard.fail(&observation, error_line(&error, &target.secret), retry);
        let upstream = spent.protocol;
        journal.failed(&target, upstream, spent.meta, &error, spent.sent);
        if verdict == Verdict::Return {
            return Some(Stop::Answer { error, upstream });
        }
        self.last = Some(error);
        None
    }

    /// The failure an exhausted walk ends with, when an attempt was made.
    pub(super) fn take_last(&mut self) -> Option<AttemptError> {
        self.last.take()
    }

    /// Seconds until the first key of the plan stops resting for its model, when every one
    /// rests: the `Retry-After` of an exhausted walk that ended on rate limits.
    pub(super) fn soonest_rest_s(&self, context: &Context) -> Option<u64> {
        let now = crate::llm::runtime::now_ms();
        let mut soonest: Option<i64> = None;
        for target in self.plan.groups.iter().flat_map(|group| &group.targets) {
            let until = context
                .llm
                .keys
                .cooling_until(&target.key_id, &target.model)?;
            soonest = Some(soonest.map_or(until, |known| known.min(until)));
        }
        soonest.map(|until| {
            u64::try_from((until - now).max(0))
                .unwrap_or(0)
                .div_ceil(1000)
                .max(1)
        })
    }

    /// The 400 text of the first target that could not carry the request, if any did.
    pub(super) fn refusal(&self) -> Option<&str> {
        self.first_refusal.as_deref()
    }

    /// The answer when no attempt was made: every circuit closed its door after the plan.
    pub(super) fn unavailable(&self, context: &Context) -> PlanError {
        self.plan.unavailable(&context.llm.health)
    }
}
