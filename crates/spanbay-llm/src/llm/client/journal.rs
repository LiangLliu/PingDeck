//! The request-log row of one in-process chat call (source `agent`).
//!
//! Single responsibility: collect what happened during one [`super::LlmClient`] call (every
//! attempt with its reason and timing, the serving target, tokens, time to first content) and
//! queue one `agent` row on the runtime's log writer when the call ends, however it ends: a
//! dropped future or stream logs a `canceled` row. Chat logs with the gateway stopped, because the
//! writer belongs to the runtime. An agent call's row carries its run key (`run_id`, `message_id`,
//! `purpose`), how the answer ended and an `rq_` request id.

use std::sync::Arc;
use std::time::Instant;

use serde_json::{Value, json};

use crate::llm::capture::PhaseValue;
use crate::llm::capture_record::{AttemptTiming, TimingJson};
use crate::llm::failure::FailureKind;
use crate::llm::gateway::logwriter::LogJob;
use crate::llm::hub::{HubFinish, HubUsage};
use crate::llm::router::{Plan, Target};
use crate::llm::runtime::{LlmRuntime, now_ms};
use crate::llm::store::RequestLogEntry;
use crate::llm::upstream::{AttemptError, UpstreamMeta, ceil_ms};
use crate::llm::usage::estimate_tokens;
use crate::llm::wire::Protocol;

/// How a call ended, for its row.
pub(super) enum Ended {
    /// The answer was delivered in full.
    Success,
    /// The call failed with `status` (the upstream's, or the HTTP status of an answer that broke
    /// off), the error text and the failure category.
    Failed {
        status: u16,
        error: String,
        kind: FailureKind,
    },
    /// The caller went away (cancel token or dropped future).
    Canceled,
}

/// The target and connection of the attempt that served (or last ran).
struct Attempted {
    provider_id: String,
    provider_name: String,
    key_id: String,
    model: String,
    protocol: Option<Protocol>,
    meta: UpstreamMeta,
    /// A stream delivered its first content event at this instant.
    first_content: Option<Instant>,
    /// The attempt delivered an answer (it is the one that serves); a failed or canceled one is in
    /// `attempts` (or finalized when the call drops mid-attempt).
    serving: bool,
    /// True from [`Journal::attempting`] until `failed`, `canceled` or `serving` records the
    /// attempt; a `Drop` while true records a `canceled` attempt row so attribution is kept.
    in_flight: bool,
}

/// Single-provider fallback attribution when every candidate was skipped before any attempt ran.
pub(crate) enum SkippedAttribution {
    None,
    One {
        provider_id: String,
        provider_name: String,
        key_id: Option<String>,
        model: Option<String>,
    },
    Ambiguous,
}

impl SkippedAttribution {
    pub(crate) fn note(
        &mut self,
        provider_id: &str,
        provider_name: &str,
        key_id: Option<&str>,
        model: &str,
    ) {
        match self {
            Self::None => {
                *self = Self::One {
                    provider_id: provider_id.to_owned(),
                    provider_name: provider_name.to_owned(),
                    key_id: key_id.map(str::to_owned),
                    model: Some(model.to_owned()),
                };
            }
            Self::One {
                provider_id: known_provider,
                key_id: known_key,
                model: known_model,
                ..
            } => {
                if known_provider != provider_id {
                    *self = Self::Ambiguous;
                    return;
                }
                if known_key.as_deref() != key_id {
                    *known_key = None;
                }
                if known_model.as_deref() != Some(model) {
                    *known_model = None;
                }
            }
            Self::Ambiguous => {}
        }
    }
}

/// The row of one call, queued when the call ends.
pub(super) struct Journal {
    runtime: Arc<LlmRuntime>,
    tag: super::CallTag,
    requested: String,
    stream: bool,
    started: Instant,
    ts_ms: i64,
    attempts: Vec<Value>,
    timings: Vec<AttemptTiming>,
    last: Option<Attempted>,
    skipped: SkippedAttribution,
    usage: HubUsage,
    /// What the serving stream delivered (text, reasoning, arguments), bounded: the estimate of
    /// the output tokens of a call cut short before the upstream reported usage.
    generated: String,
    /// How the serving answer ended, when the stream or answer said.
    finish: Option<HubFinish>,
    logged: bool,
}

/// One entry of the row's attempt list (the shape the gateway writes; no secrets).
struct Row<'a> {
    provider: &'a str,
    key_id: Option<&'a str>,
    model: &'a str,
    protocol: Option<Protocol>,
    status: Option<u16>,
    outcome: &'a str,
    latency_ms: u64,
    error: Option<String>,
}

impl Row<'_> {
    fn json(self) -> Value {
        json!({
            "provider": self.provider,
            "key_id": self.key_id,
            "model": self.model,
            "protocol": self.protocol.map(Protocol::as_str),
            "status": self.status,
            "outcome": self.outcome,
            "latency_ms": self.latency_ms,
            "error": self.error,
        })
    }
}

/// Milliseconds since `started`, rounded up like the headers time, so the parts of an attempt
/// never add up to more than its end.
fn millis(started: Instant) -> u64 {
    ceil_ms(started.elapsed())
}

impl Journal {
    /// Starts the row of a call for `requested` (the name or `provider/model` the caller used).
    pub(super) fn new(
        runtime: &Arc<LlmRuntime>,
        tag: &super::CallTag,
        requested: &str,
        stream: bool,
    ) -> Self {
        Self {
            runtime: Arc::clone(runtime),
            tag: tag.clone(),
            requested: requested.to_owned(),
            stream,
            started: Instant::now(),
            ts_ms: now_ms(),
            attempts: Vec::new(),
            timings: Vec::new(),
            last: None,
            skipped: SkippedAttribution::None,
            usage: HubUsage::default(),
            generated: String::new(),
            finish: None,
            logged: false,
        }
    }

    /// Notes how the serving answer ended.
    pub(super) fn finished(&mut self, finish: &HubFinish) {
        self.finish = Some(finish.clone());
    }

    /// Notes the targets the plan left out (`skipped: disabled`...).
    pub(super) fn planned(&mut self, plan: &Plan) {
        self.skipped_targets(&plan.skipped);
    }

    /// Lists the targets a plan left out, also when the plan failed.
    pub(super) fn skipped_targets(&mut self, skipped: &[crate::llm::router::SkippedTarget]) {
        for skipped in skipped {
            self.skipped.note(
                &skipped.provider_id,
                &skipped.provider_name,
                skipped.key_id.as_deref(),
                &skipped.model,
            );
            self.attempts.push(
                Row {
                    provider: &skipped.provider_name,
                    key_id: skipped.key_id.as_deref(),
                    model: &skipped.model,
                    protocol: None,
                    status: None,
                    outcome: "skipped",
                    latency_ms: 0,
                    error: Some(skipped.reason.clone()),
                }
                .json(),
            );
        }
    }

    /// A target refused at attempt time (its circuit opened or a probe is in flight).
    pub(super) fn skipped(&mut self, target: &Target, reason: &str) {
        self.skipped.note(
            &target.provider_id,
            &target.provider_name,
            Some(&target.key_id),
            &target.model,
        );
        self.attempts.push(
            Row {
                provider: &target.provider_name,
                key_id: Some(&target.key_id),
                model: &target.model,
                protocol: None,
                status: None,
                outcome: "skipped",
                latency_ms: 0,
                error: Some(reason.to_owned()),
            }
            .json(),
        );
    }

    fn remember(
        &mut self,
        target: &Target,
        protocol: Option<Protocol>,
        meta: UpstreamMeta,
        serving: bool,
        in_flight: bool,
    ) {
        self.last = Some(Attempted {
            provider_id: target.provider_id.clone(),
            provider_name: target.provider_name.clone(),
            key_id: target.key_id.clone(),
            model: target.model.clone(),
            protocol,
            meta,
            first_content: None,
            serving,
            in_flight,
        });
    }

    /// An attempt against `target` is starting (a drop before response headers still keeps its
    /// provider, key and model on the row).
    pub(super) fn attempting(&mut self, target: &Target) {
        self.remember(target, None, UpstreamMeta::unsent(), false, true);
    }

    /// An attempt failed before it served anything.
    pub(super) fn failed(
        &mut self,
        target: &Target,
        protocol: Option<Protocol>,
        meta: UpstreamMeta,
        error: &AttemptError,
    ) {
        let (outcome, reason) = crate::llm::failover::describe(error);
        let latency = millis(meta.started());
        self.attempts.push(
            Row {
                provider: &target.provider_name,
                key_id: Some(&target.key_id),
                model: &target.model,
                protocol,
                status: error.status().or(meta.status),
                outcome,
                latency_ms: latency,
                error: Some(reason),
            }
            .json(),
        );
        self.timings
            .push(meta.timing(PhaseValue::NotNeeded, latency));
        self.remember(target, protocol, meta, false, false);
    }

    /// An attempt was cancelled before it served anything.
    pub(super) fn canceled(
        &mut self,
        target: &Target,
        protocol: Option<Protocol>,
        meta: UpstreamMeta,
    ) {
        let latency = millis(meta.started());
        self.attempts.push(
            Row {
                provider: &target.provider_name,
                key_id: Some(&target.key_id),
                model: &target.model,
                protocol,
                status: meta.status,
                outcome: "canceled",
                latency_ms: latency,
                error: None,
            }
            .json(),
        );
        self.timings
            .push(meta.timing(PhaseValue::NotNeeded, latency));
        self.remember(target, protocol, meta, false, false);
    }

    /// An attempt answered and now serves the call.
    pub(super) fn serving(
        &mut self,
        target: &Target,
        protocol: Option<Protocol>,
        meta: UpstreamMeta,
    ) {
        self.remember(target, protocol, meta, true, false);
    }

    /// The serving stream delivered its first content event (the first one counts).
    pub(super) fn first_content(&mut self) {
        if let Some(last) = self
            .last
            .as_mut()
            .filter(|last| last.first_content.is_none())
        {
            last.first_content = Some(Instant::now());
        }
    }

    /// Notes what the serving stream delivered, for the estimate of a call cut short.
    pub(super) fn generated(&mut self, text: &str) {
        if self.generated.len() < crate::llm::hub::MAX_TEXT_BYTES {
            self.generated.push_str(text);
        }
    }

    /// Takes the token counts a stream or answer reported.
    pub(super) fn add_usage(&mut self, usage: &HubUsage) {
        self.usage.merge(usage);
    }

    /// Ends the call and queues its row (once).
    pub(super) fn end(&mut self, ended: &Ended) {
        if self.logged {
            return;
        }
        self.logged = true;
        let (status, outcome, error, kind) = match ended {
            Ended::Success => (200, "success", None, None),
            Ended::Failed {
                status,
                error,
                kind,
            } => (*status, "failure", Some(error.clone()), Some(*kind)),
            Ended::Canceled => (
                if self.last.as_ref().is_some_and(|last| last.serving) {
                    200
                } else {
                    499
                },
                "canceled",
                None,
                Some(FailureKind::Cancelled),
            ),
        };
        let mut attempts = std::mem::take(&mut self.attempts);
        let mut timings = std::mem::take(&mut self.timings);
        let mut ttft_ms = None;
        // Only a serving attempt is taken here; a failed or canceled one stays for the row's
        // columns. An attempt still in flight when the call drops gets its row recorded here.
        let serving_last = if self.last.as_ref().is_some_and(|last| last.serving) {
            self.last.take()
        } else {
            None
        };
        if let Some(last) = serving_last {
            let first = last.first_content.map(|at| {
                u64::try_from(
                    at.saturating_duration_since(last.meta.started())
                        .as_millis(),
                )
                .unwrap_or(u64::MAX)
            });
            ttft_ms = last.first_content.map(|at| {
                u64::try_from(at.saturating_duration_since(self.started).as_millis())
                    .unwrap_or(u64::MAX)
            });
            let first_token = if !self.stream {
                PhaseValue::NotNeeded
            } else {
                first.map_or(PhaseValue::NotMeasured, |at| {
                    PhaseValue::Ms(at.saturating_sub(last.meta.headers_ms.unwrap_or(0)))
                })
            };
            // The parts of the attempt (time to headers, then to the first token) fit in its end.
            let end_ms = millis(last.meta.started())
                .max(last.meta.headers_ms.unwrap_or(0) + first_token.ms().unwrap_or(0));
            attempts.push(
                Row {
                    provider: &last.provider_name,
                    key_id: Some(&last.key_id),
                    model: &last.model,
                    protocol: last.protocol,
                    status: last.meta.status,
                    outcome,
                    latency_ms: end_ms,
                    error: error.clone(),
                }
                .json(),
            );
            timings.push(last.meta.timing(first_token, end_ms));
            self.last = Some(last);
        } else if let Some(last) = self.last.as_ref().filter(|last| last.in_flight) {
            let latency = millis(last.meta.started());
            attempts.push(
                Row {
                    provider: &last.provider_name,
                    key_id: Some(&last.key_id),
                    model: &last.model,
                    protocol: last.protocol,
                    status: last.meta.status,
                    outcome,
                    latency_ms: latency,
                    error: error.clone(),
                }
                .json(),
            );
            timings.push(last.meta.timing(PhaseValue::NotNeeded, latency));
        }
        let ended_attempt_ms: u64 = timings.iter().filter_map(|t| t.end_ms.ms()).sum();
        // The call took at least as long as its attempts (rounding up can make them add to more).
        let total_ms = millis(self.started).max(ended_attempt_ms);
        let facts = self.last.take();
        let failed_over = attempts.len() > 1;
        let mut usage = std::mem::take(&mut self.usage);
        // A call cut short after it served (cancelled, or broken mid-stream) is billed for
        // what it received; output the upstream never reported is estimated from the text.
        let serving = facts.as_ref().is_some_and(|last| last.serving);
        let mut tokens_estimated = false;
        if serving
            && outcome != "success"
            && usage.output_tokens.is_none()
            && !self.generated.is_empty()
        {
            usage.output_tokens = Some(estimate_tokens(&self.generated));
            usage.input_tokens.get_or_insert(0);
            tokens_estimated = true;
        }
        let protocol_out = facts.as_ref().and_then(|last| last.protocol);
        let (provider_id, provider_name, key_id, upstream_model) = match &facts {
            Some(last) => (
                Some(last.provider_id.clone()),
                Some(last.provider_name.clone()),
                Some(last.key_id.clone()),
                Some(last.model.clone()),
            ),
            None => match std::mem::replace(&mut self.skipped, SkippedAttribution::None) {
                SkippedAttribution::One {
                    provider_id,
                    provider_name,
                    key_id,
                    model,
                } => (Some(provider_id), Some(provider_name), key_id, model),
                SkippedAttribution::None | SkippedAttribution::Ambiguous => {
                    (None, None, None, None)
                }
            },
        };
        let mut entry = RequestLogEntry {
            ts_ms: self.ts_ms,
            protocol_in: protocol_out
                .unwrap_or(Protocol::OpenaiChat)
                .as_str()
                .to_owned(),
            requested_model: self.requested.clone(),
            provider_id,
            provider_name,
            key_id,
            upstream_model,
            protocol_out: protocol_out.map(|protocol| protocol.as_str().to_owned()),
            status,
            outcome: outcome.to_owned(),
            stream: self.stream,
            latency_ms: total_ms,
            ttft_ms,
            input_tokens: usage.input_tokens,
            output_tokens: usage.output_tokens,
            cached_tokens: usage.cached_input_tokens,
            cache_write_tokens: usage.cache_write_tokens,
            reasoning_tokens: usage.reasoning_tokens,
            tokens_estimated,
            attempts: Value::Array(attempts),
            error,
            source: "agent".to_owned(),
            source_ref: self.tag.session.clone(),
            session_id: self.tag.session.clone(),
            request_id: Some(crate::llm::store::new_request_id()),
            run_id: self.tag.meta.as_ref().map(|meta| meta.run_id.clone()),
            message_id: self
                .tag
                .meta
                .as_ref()
                .and_then(|meta| meta.message_id.clone()),
            purpose: self
                .tag
                .meta
                .as_ref()
                .map(|meta| meta.purpose.as_str().to_owned()),
            finish_reason: self.finish.as_ref().map(finish_reason),
            // Retired with aliases; the column keeps old rows only.
            alias: None,
            failure_kind: kind,
            upstream_request_id: facts
                .as_ref()
                .and_then(|last| last.meta.upstream_request_id.clone()),
            ..RequestLogEntry::default()
        };
        entry.timing = (!timings.is_empty()).then(|| TimingJson {
            queue_ms: total_ms.saturating_sub(ended_attempt_ms),
            attempts: timings,
        });
        let mut job = LogJob::from_entry(entry);
        job.any_attempt_failed = failed_over || outcome == "failure";
        // A writer that cannot start loses the row; the call itself is unaffected.
        if let Ok(log) = self.runtime.log() {
            log.submit(job);
        }
    }
}

/// The stored word of how an answer ended (`finish_reason`).
fn finish_reason(finish: &HubFinish) -> String {
    match finish {
        HubFinish::Stop => "stop".to_owned(),
        HubFinish::Length => "length".to_owned(),
        HubFinish::ToolCalls => "tool_calls".to_owned(),
        HubFinish::ContentFilter => "content_filter".to_owned(),
        HubFinish::Pause => "needs_continuation".to_owned(),
        HubFinish::Other(word) => word.chars().take(64).collect(),
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        self.end(&Ended::Canceled);
    }
}
