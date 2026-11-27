//! Failover decisions shared by the gateway pipeline, the in-process client and AI runs.
//!
//! Single responsibility: the failover rules as pure code. A failed attempt is described by a
//! [`FailureObservation`]; the [`Failure::verdict`] of its [`assess`] says whether the next *key*
//! of the same provider, the next *target*, or nothing (return the answer to the caller) is the
//! right move; [`AttemptPlan`] walks the resolved groups with the attempt budget, the one
//! `Retry-After` wait ([`RetryAfterPolicy`]) and the skipped circuits. No I/O, no clock: callers
//! sleep and send.
//!
//! The model-not-found and context-length patterns are `[INFERENCE]` from vendor documentation and
//! recorded error bodies (the fixture table in the tests pins them).

use std::time::{Duration, SystemTime};

use serde_json::Value;

use crate::llm::hub::HubRequest;
use crate::llm::upstream::{AttemptError, TRUNCATED_STREAM};
use crate::llm::wire::Protocol;

pub use crate::llm::failure::{
    Failure, FailureObservation, FailureScope, KeyRest, Transport, Verdict, assess,
    names_context_limit, names_missing_model,
};

/// Longest `Retry-After` that is waited out.
pub const MAX_RETRY_AFTER: Duration = Duration::from_secs(10);

/// Smallest first-byte timeout of a request that enables reasoning.
pub const REASONING_FIRST_BYTE: Duration = Duration::from_secs(120);

/// The observation of a failed attempt. `None` for failures that are not about the upstream
/// (cancellation, a request that could not be built): those end the request unchanged.
#[must_use]
pub fn observe(error: &AttemptError) -> Option<FailureObservation> {
    let transport = |transport, body: String| FailureObservation {
        transport: Some(transport),
        body,
        ..FailureObservation::default()
    };
    match error {
        AttemptError::Status {
            status,
            body,
            retry_after_ms,
        } => Some(FailureObservation {
            status: Some(*status),
            transport: None,
            body: body.clone(),
            retry_after_ms: *retry_after_ms,
        }),
        AttemptError::Network { message, cause } => {
            use spanbay_base::error::NetworkCause as C;
            // This machine's network is told by the cause, never by the message text.
            let local = matches!(
                cause,
                Some(C::Dns | C::ProxyAuth | C::ProxyRefused | C::ProxyUnreachable)
            );
            let kind = if local {
                Transport::Local
            } else {
                Transport::Connection
            };
            Some(transport(kind, message.clone()))
        }
        AttemptError::Timeout => Some(transport(Transport::Timeout, String::new())),
        AttemptError::NotSse { preview, .. } => {
            Some(transport(Transport::UnexpectedBody, preview.clone()))
        }
        AttemptError::BadBody(message) => {
            Some(transport(Transport::UnexpectedBody, message.clone()))
        }
        AttemptError::Stream {
            kind: Some(kind), ..
        } if kind == TRUNCATED_STREAM => Some(transport(Transport::Truncated, String::new())),
        AttemptError::Stream { message, .. } => {
            Some(transport(Transport::StreamError, message.clone()))
        }
        AttemptError::Cancelled | AttemptError::Invalid(_) => None,
    }
}

/// How an attempt appears in the log's attempt list: `rejected` for a refused credential,
/// `failure` otherwise, with the reason text (`rejected 401`, `429 retry-after 2`, the upstream
/// message...).
#[must_use]
pub fn describe(error: &AttemptError) -> (&'static str, String) {
    if let AttemptError::Status {
        status,
        retry_after_ms,
        ..
    } = error
    {
        if (401..=403).contains(status) {
            return ("rejected", format!("rejected {status}"));
        }
        if let Some(ms) = retry_after_ms.filter(|_| matches!(status, 429 | 503)) {
            return (
                "failure",
                format!("{status} retry-after {}", ms.div_ceil(1000)),
            );
        }
    }
    ("failure", error.message())
}

/// Whether a request body enables reasoning, per protocol: Messages `thinking` enabled,
/// Chat and Responses `reasoning_effort` / `reasoning`, Gemini `thinkingConfig`.
#[must_use]
pub fn enables_reasoning(protocol: Protocol, body: &Value) -> bool {
    let effort_on = |value: &Value| {
        value
            .as_str()
            .is_some_and(|effort| !effort.is_empty() && effort != "none" && effort != "off")
    };
    match protocol {
        Protocol::AnthropicMessages => body.get("thinking").is_some_and(|thinking| {
            thinking
                .get("type")
                .and_then(Value::as_str)
                .is_some_and(|kind| kind == "enabled" || kind == "adaptive")
        }),
        Protocol::OpenaiChat | Protocol::OpenaiResponses => {
            body.get("reasoning_effort").is_some_and(effort_on)
                || body
                    .get("reasoning")
                    .is_some_and(|reasoning| match reasoning {
                        Value::Object(map) => {
                            map.get("effort").is_none_or(effort_on)
                                && map.get("enabled").and_then(Value::as_bool) != Some(false)
                                && !map.is_empty()
                        }
                        other => effort_on(other),
                    })
        }
        Protocol::Gemini => {
            let config = body
                .pointer("/generationConfig/thinkingConfig")
                .or_else(|| body.pointer("/generation_config/thinking_config"))
                .or_else(|| body.get("thinkingConfig"));
            config.is_some_and(|config| {
                let budget = config
                    .get("thinkingBudget")
                    .or_else(|| config.get("thinking_budget"))
                    .and_then(Value::as_i64);
                let level = config
                    .get("thinkingLevel")
                    .or_else(|| config.get("thinking_level"))
                    .is_some();
                budget != Some(0)
                    && (budget.is_some()
                        || level
                        || config.get("includeThoughts").and_then(Value::as_bool) == Some(true))
            })
        }
        _ => false,
    }
}

/// The first-byte timeout for a raw request body: `setting`, or at least 120 s when the request
/// enables reasoning.
#[must_use]
pub fn first_byte_timeout_raw(protocol: Protocol, body: &Value, setting: Duration) -> Duration {
    if enables_reasoning(protocol, body) {
        setting.max(REASONING_FIRST_BYTE)
    } else {
        setting
    }
}

/// The first-byte timeout for a hub request: `setting`, or at least 120 s when it asks for
/// reasoning (an explicit "off" or a zero budget does not).
#[must_use]
pub fn first_byte_timeout(setting: Duration, request: &HubRequest) -> Duration {
    let reasoning = request.reasoning.as_ref().is_some_and(|reasoning| {
        reasoning.enabled != Some(false)
            && (reasoning.enabled == Some(true)
                || reasoning.effort.is_some()
                || reasoning.budget_tokens.is_some_and(|budget| budget > 0))
    });
    if reasoning {
        setting.max(REASONING_FIRST_BYTE)
    } else {
        setting
    }
}

/// The attempt budget of one request: the first attempt plus `max_retries`. Circuit-skipped
/// targets do not use it up; a `Retry-After` wait retry does.
#[must_use]
pub fn attempt_budget(max_retries: u8) -> usize {
    1 + usize::from(max_retries)
}

/// The status a request ends with after it ran out of attempts: the last upstream status when it
/// is an error, else 502.
#[must_use]
pub fn exhausted_status(last_status: Option<u16>) -> u16 {
    last_status.filter(|status| *status >= 400).unwrap_or(502)
}

/// Parses a `Retry-After` header value (delay seconds, or an HTTP date) into milliseconds from
/// `now`. A date in the past is `0`; anything unparseable is `None`.
#[must_use]
pub fn parse_retry_after(value: &str, now: SystemTime) -> Option<u64> {
    let value = value.trim();
    if let Ok(seconds) = value.parse::<f64>() {
        return (seconds.is_finite() && seconds >= 0.0)
            .then(|| (seconds * 1000.0).min(3.0e14) as u64);
    }
    let date = httpdate::parse_http_date(value).ok()?;
    let wait = date.duration_since(now).unwrap_or(Duration::ZERO);
    Some(u64::try_from(wait.as_millis()).unwrap_or(u64::MAX))
}

/// What the `Retry-After` rule needs to know about the request so far.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct WaitContext {
    /// Another enabled key or target is still left for this request.
    pub another_choice: bool,
    /// The request already waited once.
    pub waited: bool,
    /// A streaming client already received content.
    pub content_delivered: bool,
    /// The attempt budget allows one more attempt.
    pub budget_left: bool,
}

/// The `Retry-After` rule: wait out a short hint only when nothing else can serve the request.
pub struct RetryAfterPolicy;

impl RetryAfterPolicy {
    /// How long to wait before retrying the same key and model once, or `None`: only for 429 and
    /// 503 with a `Retry-After` of at most [`MAX_RETRY_AFTER`], only when no other key or target
    /// remains, only once per request, never after content reached a streaming client, never past
    /// the attempt budget.
    #[must_use]
    pub fn wait(observation: &FailureObservation, context: WaitContext) -> Option<Duration> {
        if context.another_choice
            || context.waited
            || context.content_delivered
            || !context.budget_left
            || !matches!(observation.status, Some(429 | 503))
        {
            return None;
        }
        let wait = Duration::from_millis(observation.retry_after_ms?);
        (wait <= MAX_RETRY_AFTER).then_some(wait)
    }
}

/// Why an [`AttemptPlan`] stopped.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StopReason {
    /// The last failure is the caller's answer (a client error, content already delivered).
    Returned,
    /// Every key and target was tried or skipped.
    Exhausted,
    /// The attempt budget is spent.
    BudgetSpent,
}

/// What an [`AttemptPlan`] asks for next.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Step {
    /// Attempt key `key` of group `group`; wait `wait` first when it is the `Retry-After` retry.
    Attempt {
        /// Index of the target group.
        group: usize,
        /// Index of the key inside the group.
        key: usize,
        /// The wait before the attempt, only for the one `Retry-After` retry.
        wait: Option<Duration>,
    },
    /// Nothing more to try.
    Stop(StopReason),
}

/// The default wait before an in-place retry of the last candidate (drivers pass a jittered one).
const SAME_TARGET_WAIT: Duration = Duration::from_millis(500);

/// Whether a failure of the last candidate is worth one in-place retry: a transient
/// provider error or a connection failure of the target. A 504 or a first-byte timeout already
/// waited in full.
fn retries_in_place(failure: &Failure) -> bool {
    match failure.kind {
        crate::llm::failure::FailureKind::ProviderError => {
            matches!(failure.status, Some(500 | 502 | 503 | 529))
        }
        crate::llm::failure::FailureKind::Network => failure.scope == FailureScope::Target,
        _ => false,
    }
}

/// Walks the resolved target groups for one request.
///
/// The caller loops: [`AttemptPlan::step`] names the key to attempt; a refused circuit is reported
/// with [`AttemptPlan::skipped`] (it uses no attempt), a failure with [`AttemptPlan::failed`]; a
/// success ends the request. The plan owns the order, the budget and the single in-place retry
/// (the `Retry-After` wait, or one jittered retry of the last candidate).
///
/// The budget has two layers: rotating to another key of the same group needs room for
/// one attempt at every later non-empty group, so each candidate of the chain is tried at least
/// once and key rotation spends what is left.
#[derive(Debug, Clone)]
pub struct AttemptPlan {
    keys: Vec<usize>,
    budget: usize,
    made: usize,
    group: usize,
    key: usize,
    waited: bool,
    pending_wait: Option<Duration>,
    /// The next step repeats the failed attempt in place.
    repeat: bool,
    /// The step just named repeats the previous attempt in place.
    retrying: bool,
    same_target_wait: Duration,
    stopped: Option<StopReason>,
}

impl AttemptPlan {
    /// A plan over groups with `keys[i]` keys each, for `1 + max_retries` attempts.
    #[must_use]
    pub fn new(keys: Vec<usize>, max_retries: u8) -> Self {
        Self {
            keys,
            budget: attempt_budget(max_retries),
            made: 0,
            group: 0,
            key: 0,
            waited: false,
            pending_wait: None,
            repeat: false,
            retrying: false,
            same_target_wait: SAME_TARGET_WAIT,
            stopped: None,
        }
    }

    /// The plan with `wait` before an in-place retry of the last candidate (the driver draws it
    /// between 250 and 750 ms; the plan touches no entropy).
    #[must_use]
    pub fn with_retry_wait(mut self, wait: Duration) -> Self {
        self.same_target_wait = wait;
        self
    }

    /// Attempts made so far (a skipped circuit is not one).
    #[must_use]
    pub fn attempts(&self) -> usize {
        self.made
    }

    /// Whether the attempt just named repeats the previous one in place (the `Retry-After` wait or
    /// the same-target retry).
    #[must_use]
    pub fn retrying(&self) -> bool {
        self.retrying
    }

    fn settle_position(&mut self) {
        while self.group < self.keys.len() && self.key >= self.keys[self.group] {
            self.group += 1;
            self.key = 0;
        }
    }

    /// The next attempt, or why there is none.
    pub fn step(&mut self) -> Step {
        if let Some(reason) = self.stopped {
            return Step::Stop(reason);
        }
        if self.made >= self.budget {
            self.stopped = Some(StopReason::BudgetSpent);
        } else {
            self.settle_position();
            if self.group >= self.keys.len() {
                self.stopped = Some(StopReason::Exhausted);
            }
        }
        match self.stopped {
            Some(reason) => {
                self.retrying = false;
                Step::Stop(reason)
            }
            None => {
                self.retrying = std::mem::take(&mut self.repeat);
                Step::Attempt {
                    group: self.group,
                    key: self.key,
                    wait: self.pending_wait.take(),
                }
            }
        }
    }

    /// The attempt just named was refused before it was sent (open circuit, a key that rests):
    /// move to the next key of the group without using an attempt.
    pub fn skipped(&mut self) {
        self.pending_wait = None;
        self.repeat = false;
        self.retrying = false;
        self.key += 1;
    }

    /// The attempt just made failed with `observation`. `content_delivered` says a streaming
    /// client already received content, which ends failover. Returns the verdict that drove the
    /// plan.
    pub fn failed(&mut self, observation: &FailureObservation, content_delivered: bool) -> Verdict {
        self.made += 1;
        self.retrying = false;
        let failure = assess(observation);
        let verdict = failure.verdict();
        if content_delivered || verdict == Verdict::Return {
            self.stopped = Some(StopReason::Returned);
            return Verdict::Return;
        }
        self.settle_position();
        let keys_left = self.keys[self.group] - self.key - 1;
        let groups_left = self.keys[self.group + 1..]
            .iter()
            .filter(|keys| **keys > 0)
            .count();
        let another_choice = groups_left > 0 || (verdict == Verdict::NextKey && keys_left > 0);
        let budget_left = self.made < self.budget;
        let wait = RetryAfterPolicy::wait(
            observation,
            WaitContext {
                another_choice,
                waited: self.waited,
                content_delivered,
                budget_left,
            },
        )
        .or_else(|| {
            (!another_choice && !self.waited && budget_left && retries_in_place(&failure))
                .then_some(self.same_target_wait)
        });
        if let Some(wait) = wait {
            self.waited = true;
            self.repeat = true;
            self.pending_wait = Some(wait);
            return verdict;
        }
        if verdict == Verdict::NextKey
            && keys_left > 0
            && self.made + 1 + groups_left <= self.budget
        {
            self.key += 1;
        } else {
            self.group += 1;
            self.key = 0;
        }
        verdict
    }
}

#[cfg(test)]
#[path = "failover_tests.rs"]
mod tests;
