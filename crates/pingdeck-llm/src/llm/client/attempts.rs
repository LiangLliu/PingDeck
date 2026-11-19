//! The failover driver of one in-process call.
//!
//! Single responsibility: walk the plan of a call with the shared failover rules (`failover`): ask
//! the circuit for each key, wait out the one `Retry-After`, record every attempt in the call's
//! [`Journal`] and in the health book, and turn the end of the walk into the typed error the
//! caller sees. The attempt itself (JSON or stream) is the caller's.

use std::collections::HashSet;
use std::sync::Arc;
use std::time::Duration;

use crate::llm::events::acquire_announcing;
use crate::llm::failover::{
    AttemptPlan, Failure, FailureScope, Step, Verdict, exhausted_status, observe,
};
use crate::llm::failure::FailureKind;
use crate::llm::health::{CircuitConfig, Outcome, Permit};
use crate::llm::hub::{HubAttempt, HubRequest};
use crate::llm::key_book::error_line;
use crate::llm::router::{Plan, RequestNeeds, RoutingSnapshot, Target};
use crate::llm::runtime::LlmRuntime;
use crate::llm::upstream::{AttemptError, UpstreamMeta};
use crate::llm::wire::Protocol;
use pingdeck_base::cancel::{CancelToken, until_cancelled};
use pingdeck_base::error::AppError;

use super::journal::{Ended, Journal};
use super::{AttemptSummary, ChatError, ChatErrorKind, ChatTarget, HealthGuard};

/// A permitted attempt: the target and the guard that records its health outcome.
pub(super) struct Attempt {
    pub(super) target: Target,
    pub(super) guard: HealthGuard,
    /// The attempt repeats the previous one in place (`Retry-After` wait or same-target retry).
    pub(super) retry: bool,
}

/// The wait before the one same-target retry: 250 to 750 ms, so parallel calls do not retry in
/// step.
pub(crate) fn retry_wait() -> Duration {
    let mut bytes = [0_u8; 8];
    let spread = if getrandom::fill(&mut bytes).is_ok() {
        u64::from_le_bytes(bytes) % 501
    } else {
        250
    };
    Duration::from_millis(250 + spread)
}

/// What a failed attempt led to, for the attempt lists.
pub(crate) fn next_action(plan: &AttemptPlan, verdict: Verdict) -> &'static str {
    let mut next = plan.clone();
    match next.step() {
        Step::Attempt { .. } if next.retrying() => "same_target_retry",
        _ if verdict == Verdict::NextKey => "next_key",
        _ => "next_target",
    }
}

/// What an attempt sent and learned, whether it worked or not.
pub(super) struct Spent {
    pub(super) protocol: Option<Protocol>,
    pub(super) meta: UpstreamMeta,
}

impl Spent {
    /// An attempt that failed before anything was sent.
    pub(super) fn unsent(protocol: Option<Protocol>) -> Self {
        Self {
            protocol,
            meta: UpstreamMeta::unsent(),
        }
    }
}

/// One call's walk over its plan.
pub(super) struct Run {
    runtime: Arc<LlmRuntime>,
    /// The routing facts the plan was made from.
    snapshot: Arc<RoutingSnapshot>,
    plan: Plan,
    walk: AttemptPlan,
    circuit: CircuitConfig,
    cancel: Option<CancelToken>,
    pub(super) journal: Journal,
    /// The last failure that moved on (or ended the walk), with what the rules judged.
    last: Option<(AttemptError, Failure)>,
    /// Keys that failed on the key itself in this request: later groups skip them.
    rejected: HashSet<String>,
    /// The attempts that did not serve, for [`crate::llm::hub::HubEvent::Served`].
    pub(super) not_served: Vec<HubAttempt>,
    /// Every attempt that failed or was skipped so far, for the error that ends the walk.
    attempts: Vec<AttemptSummary>,
    /// Settings read once when the call started.
    pub(super) first_byte: Duration,
    pub(super) idle: Duration,
}

impl ChatError {
    pub(super) fn cancelled() -> Self {
        Self::new(ChatErrorKind::Cancelled, AppError::cancelled())
    }
}

impl Run {
    /// Plans `target` for `request` and starts the call's row. A `reshaping` call takes images out
    /// for a model known not to read them, so such a target stays in the plan, after the others.
    ///
    /// # Errors
    /// The planning failure (unknown name, nothing usable, every circuit open, a needed capability
    /// missing everywhere), as [`ChatErrorKind::Failed`].
    pub(super) async fn begin(
        runtime: &Arc<LlmRuntime>,
        tag: &super::CallTag,
        target: &ChatTarget,
        request: &HubRequest,
        stream: bool,
        reshaping: bool,
        cancel: Option<CancelToken>,
    ) -> Result<Self, ChatError> {
        let snapshot = runtime
            .snapshot()
            .await
            .map_err(|error| ChatError::new(ChatErrorKind::Failed, error))?;
        let settings = runtime.settings();
        let gateway = &settings.ai.gateway;
        let mut needs = RequestNeeds::from_hub(request);
        let images_droppable = reshaping && needs.vision;
        needs.vision &= !images_droppable;
        let planned = match target {
            ChatTarget::Named { name, fallbacks } => snapshot.plan_with_fallbacks_explained(
                name,
                fallbacks,
                &needs,
                &runtime.health,
                &runtime.keys,
            ),
            ChatTarget::Pinned {
                provider_id,
                key_id,
                model,
            } => snapshot.plan_pinned_explained(
                provider_id,
                key_id.as_deref(),
                model,
                &needs,
                &runtime.health,
                &runtime.keys,
            ),
        };
        let mut plan = match planned {
            Ok(plan) => plan,
            Err(failure) => {
                let error = AppError::from(failure.error);
                // A call that never reached a target still gets its row.
                let requested = match target {
                    ChatTarget::Named { name, .. } => name.clone(),
                    ChatTarget::Pinned {
                        provider_id, model, ..
                    } => format!("{provider_id}/{model}"),
                };
                let mut journal = Journal::new(runtime, tag, &requested, stream);
                journal.skipped_targets(&failure.skipped);
                journal.end(&Ended::Failed {
                    status: 503,
                    error: error.message().to_owned(),
                    kind: FailureKind::ModelUnavailable,
                });
                return Err(ChatError::new(ChatErrorKind::Failed, error));
            }
        };
        if images_droppable {
            // Stable: the plan's order holds within each side.
            plan.groups.sort_by_key(|group| {
                super::reshape::reads_no_images(&snapshot, &group.provider_id, &group.model)
            });
        }
        let mut journal = Journal::new(runtime, tag, &plan.requested, stream);
        journal.planned(&plan);
        Ok(Self {
            runtime: Arc::clone(runtime),
            snapshot,
            walk: AttemptPlan::new(plan.key_counts(), gateway.max_retries)
                .with_retry_wait(retry_wait()),
            plan,
            circuit: crate::llm::runtime::circuit_config(gateway),
            cancel,
            journal,
            last: None,
            rejected: HashSet::new(),
            not_served: Vec::new(),
            attempts: Vec::new(),
            first_byte: Duration::from_secs(u64::from(gateway.first_byte_timeout_s)),
            idle: Duration::from_secs(u64::from(gateway.idle_timeout_s)),
        })
    }

    /// What the routing facts say about the model `target` calls.
    pub(super) fn facts(&self, target: &Target) -> super::TargetFacts {
        super::TargetFacts::of(&self.snapshot, target)
    }

    /// The next target to call, after any `Retry-After` wait and the circuit check; `None` when
    /// the walk is over.
    ///
    /// # Errors
    /// [`ChatErrorKind::Cancelled`] when the token fires during the wait.
    pub(super) async fn next(&mut self) -> Result<Option<Attempt>, ChatError> {
        loop {
            let (group, key, wait) = match self.walk.step() {
                Step::Stop(_) => return Ok(None),
                Step::Attempt { group, key, wait } => (group, key, wait),
            };
            let target = self.plan.groups[group].targets[key].clone();
            let retry = self.walk.retrying();
            if !retry && self.rejected.contains(&target.key_id) {
                self.skip(&target, "key rejected earlier in this request");
                continue;
            }
            if let Some(wait) = wait
                && until_cancelled(self.cancel.as_ref(), tokio::time::sleep(wait))
                    .await
                    .is_none()
            {
                self.journal.end(&Ended::Canceled);
                return Err(ChatError::cancelled());
            }
            if acquire_announcing(&self.runtime, &target.key_id, &target.model) == Permit::Blocked {
                self.skip(&target, "circuit open");
                continue;
            }
            let guard = HealthGuard::with_config(&self.runtime, &target, self.circuit);
            self.journal.attempting(&target);
            return Ok(Some(Attempt {
                target,
                guard,
                retry,
            }));
        }
    }

    /// Records the failure of `attempt`. `Some` is the final error (a client error that is the
    /// caller's answer, a cancellation, a local fault); `None` means the walk goes on.
    pub(super) fn failed(
        &mut self,
        mut attempt: Attempt,
        spent: Spent,
        error: AttemptError,
    ) -> Option<ChatError> {
        if matches!(error, AttemptError::Cancelled) {
            attempt.guard.finish(Outcome::Canceled);
            self.journal
                .canceled(&attempt.target, spent.protocol, spent.meta);
            self.journal.end(&Ended::Canceled);
            return Some(ChatError::cancelled());
        }
        self.note(&attempt.target, error.message().to_owned());
        let Some(observation) = observe(&error) else {
            // A fault of this side says nothing about the upstream's health.
            if let Some(last) = self.attempts.last_mut() {
                last.transient = Some(false);
            }
            attempt.guard.fault();
            self.journal
                .failed(&attempt.target, spent.protocol, spent.meta, &error);
            return Some(self.finish_failed(error, ChatErrorKind::Failed));
        };
        let verdict = self.walk.failed(&observation, false);
        let mut failure = observation.assess();
        failure.retried = attempt.retry;
        if let Some(last) = self.attempts.last_mut() {
            last.transient = Some(super::is_transient(&failure));
        }
        if failure.scope == FailureScope::Key {
            self.rejected.insert(attempt.target.key_id.clone());
        }
        let line = error_line(&error, &attempt.target.secret);
        self.not_served.push(HubAttempt {
            provider_id: attempt.target.provider_id.clone(),
            model: attempt.target.model.clone(),
            key_id: Some(attempt.target.key_id.clone()),
            kind: Some(failure.kind.as_str().to_owned()),
            status: failure.status,
            action: next_action(&self.walk, verdict).to_owned(),
            why: line.clone(),
        });
        attempt.guard.fail(&observation, line, attempt.retry);
        self.journal
            .failed(&attempt.target, spent.protocol, spent.meta, &error);
        if verdict == Verdict::Return {
            return Some(
                self.finish_failed(error, ChatErrorKind::Failed)
                    .with_judgement(&failure),
            );
        }
        self.last = Some((error, failure));
        None
    }

    fn skip(&mut self, target: &Target, reason: &str) {
        self.journal.skipped(target, reason);
        self.note(target, reason.to_owned());
        self.not_served.push(HubAttempt {
            provider_id: target.provider_id.clone(),
            model: target.model.clone(),
            key_id: Some(target.key_id.clone()),
            kind: None,
            status: None,
            action: "skipped".to_owned(),
            why: reason.to_owned(),
        });
        self.walk.skipped();
    }

    fn note(&mut self, target: &Target, reason: String) {
        self.attempts.push(AttemptSummary {
            target: format!("{}/{}", target.provider_name, target.model),
            reason,
            transient: None,
        });
    }

    /// The error of an exhausted walk: every attempt failed or nothing could be attempted.
    pub(super) fn exhausted(&mut self) -> ChatError {
        let Some((error, failure)) = self.last.take() else {
            let unavailable = AppError::from(self.plan.unavailable(&self.runtime.health));
            self.journal.end(&Ended::Failed {
                status: 503,
                error: unavailable.message().to_owned(),
                kind: FailureKind::ProviderError,
            });
            return ChatError::new(ChatErrorKind::Failed, unavailable)
                .with_failure(FailureKind::ProviderError)
                .with_attempts(std::mem::take(&mut self.attempts));
        };
        let kind = if matches!(failure.status, Some(401 | 403)) {
            ChatErrorKind::KeyRejected
        } else if error.is_empty_stream() {
            ChatErrorKind::Refusal
        } else {
            ChatErrorKind::Failed
        };
        let message = format!("every upstream attempt failed; last: {}", error.message());
        self.finish_failed_with(&error, AppError::network(message), kind)
            .with_judgement(&failure)
    }

    /// Ends the call with `error` as it is (the caller's own answer).
    fn finish_failed(&mut self, error: AttemptError, kind: ChatErrorKind) -> ChatError {
        let app = error.clone().into_app_error();
        self.finish_failed_with(&error, app, kind)
    }

    fn finish_failed_with(
        &mut self,
        error: &AttemptError,
        app: AppError,
        kind: ChatErrorKind,
    ) -> ChatError {
        self.journal.end(&Ended::Failed {
            status: exhausted_status(error.status()),
            error: app.message().to_owned(),
            kind: error.kind(),
        });
        ChatError::new(kind, app)
            .with_failure(error.kind())
            .with_attempts(std::mem::take(&mut self.attempts))
    }
}
