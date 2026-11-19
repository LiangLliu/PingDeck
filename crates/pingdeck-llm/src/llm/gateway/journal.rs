//! The request-log row of one gateway call, and its capture.
//!
//! Single responsibility: collect what happened during one request (every attempt with its reason
//! and timing, the serving target, tokens, time to first content, the bounded bytes of the
//! exchange) and queue one [`LogJob`] when the call ends, however it ends: a dropped handler
//! future (the client hung up) or a dropped stream body logs a `canceled` row. Nothing here
//! blocks, redacts or touches the store; the writer thread does.
//!
//! The row's timing: `queue_ms` is the call's total minus the ends of its attempts
//! (decode, planning, circuit checks, the `Retry-After` wait), so queue plus the attempts' ends
//! equals the total. A capture keeps the client's side once (request and response); the upstream
//! side only where it differs (a re-encoded request, a translated reply, an upstream error).

use std::time::Instant;

use serde_json::{Value, json};

use crate::llm::capture::{BoundedBody, PhaseValue, RawExchange, bound_body};
use crate::llm::capture_record::{AttemptTiming, CapturedFrames, TimingJson};
use crate::llm::client::SkippedAttribution;
use crate::llm::failover::describe;
use crate::llm::failure::FailureKind;
use crate::llm::hub::HubUsage;
use crate::llm::router::{Plan, Target};
use crate::llm::runtime::now_ms;
use crate::llm::store::RequestLogEntry;
use crate::llm::upstream::{AttemptError, UpstreamMeta, ceil_ms};
use crate::llm::usage::{estimate_tokens, estimate_tokens_bytes};
use crate::llm::wire::Protocol;

use super::chat::Inbound;
use super::exchange::Sent;
use super::logwriter::LogJob;
use super::reply::DROPPED_HEADER;
use super::server::Context;

/// How a call ended, for its row.
pub(super) enum Ended {
    /// The answer was delivered in full.
    Success,
    /// The call failed with `status` (the status the client saw), the error text and category.
    Failed {
        status: u16,
        error: String,
        kind: FailureKind,
    },
    /// The client went away (dropped future or stream) or the replay was stopped.
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
    /// The first content event reached the client at this instant.
    first_content: Option<Instant>,
    /// The attempt delivered an answer (it serves); a failed or canceled one is in `attempts`
    /// (or finalized when the call drops mid-attempt).
    serving: bool,
    /// True from [`Journal::attempting`] until `failed`, `canceled` or `serving` records the
    /// attempt; a `Drop` while true records a `canceled` attempt row so attribution is kept.
    in_flight: bool,
    sent: Option<Sent>,
    /// The upstream's error body of a failed attempt, bounded.
    error_body: Option<BoundedBody>,
}

/// One entry of the row's attempt list (no secrets).
struct Row<'a> {
    provider: &'a str,
    key_id: Option<&'a str>,
    model: &'a str,
    protocol: Option<Protocol>,
    status: Option<u16>,
    outcome: &'a str,
    latency_ms: u64,
    error: Option<String>,
    /// The `Retry-After` wait the upstream named, in whole seconds rounded up (429 and 503).
    retry_after_s: Option<u64>,
}

impl Row<'_> {
    fn json(self) -> Value {
        let mut row = json!({
            "provider": self.provider,
            "key_id": self.key_id,
            "model": self.model,
            "protocol": self.protocol.map(Protocol::as_str),
            "status": self.status,
            "outcome": self.outcome,
            "latency_ms": self.latency_ms,
            "error": self.error,
        });
        if let Some(wait) = self.retry_after_s {
            row["retry_after_s"] = json!(wait);
        }
        row
    }
}

/// Milliseconds since `started`, rounded up like the headers time, so the parts of an attempt
/// never add up to more than its end.
fn millis(started: Instant) -> u64 {
    ceil_ms(started.elapsed())
}

/// A request whose body is at most this large also stores its hub decode in the capture.
const HUB_CAPTURE_MAX_BYTES: u64 = 256 * 1024;

/// The row of one call, queued when the call ends.
pub(super) struct Journal {
    context: Context,
    protocol_in: Protocol,
    requested: String,
    stream: bool,
    started: Instant,
    ts_ms: i64,
    /// The client's body, for its size and the token estimate when the upstream reports none.
    request: bytes::Bytes,
    attempts: Vec<Value>,
    timings: Vec<AttemptTiming>,
    last: Option<Attempted>,
    skipped: SkippedAttribution,
    usage: HubUsage,
    /// What the model generated (text, reasoning, arguments), for the estimate.
    generated: String,
    resp_bytes: u64,
    capture: Option<Box<RawExchange>>,
    logged: bool,
}

impl Journal {
    /// Starts the row of a call for `requested` (the name the client used). `capture` says the
    /// capture policy may want this exchange: the client's side is copied now.
    pub(super) fn new(
        context: &Context,
        inbound: &Inbound,
        requested: &str,
        stream: bool,
        capture: bool,
    ) -> Self {
        let ts_ms = now_ms();
        let capture = capture.then(|| {
            Box::new(RawExchange {
                request_log_id: 0,
                ts_ms,
                client_protocol: inbound.protocol,
                client_req_headers: inbound.headers.clone(),
                client_req_body: Some(bound_body(&inbound.bytes)),
                hub_request: None,
                upstream_url: String::new(),
                upstream_req_headers: Vec::new(),
                upstream_req_body: None,
                upstream_status: None,
                upstream_resp_headers: Vec::new(),
                upstream_resp_body: None,
                client_resp_headers: Vec::new(),
                client_resp_body: None,
                upstream_frames: None,
                client_frames: None,
                translation_notes: Vec::new(),
                timing: TimingJson::default(),
            })
        });
        Self {
            context: context.clone(),
            protocol_in: inbound.protocol,
            requested: requested.to_owned(),
            stream,
            started: Instant::now(),
            ts_ms,
            request: inbound.bytes.clone(),
            attempts: Vec::new(),
            timings: Vec::new(),
            last: None,
            skipped: SkippedAttribution::None,
            usage: HubUsage::default(),
            generated: String::new(),
            resp_bytes: 0,
            capture,
            logged: false,
        }
    }

    /// Whether the exchange is being copied for a possible capture.
    pub(super) fn capturing(&self) -> bool {
        self.capture.is_some()
    }

    /// Whether the client's request body is small enough to store its hub decode too.
    pub(super) fn wants_hub(&self) -> bool {
        self.capture.is_some() && self.request.len() as u64 <= HUB_CAPTURE_MAX_BYTES
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
            self.skipped_row(
                &skipped.provider_name,
                skipped.key_id.as_deref(),
                &skipped.model,
                &skipped.reason,
            );
        }
    }

    fn skipped_row(&mut self, provider: &str, key_id: Option<&str>, model: &str, reason: &str) {
        self.attempts.push(
            Row {
                provider,
                key_id,
                model,
                protocol: None,
                status: None,
                outcome: "skipped",
                latency_ms: 0,
                error: Some(reason.to_owned()),
                retry_after_s: None,
            }
            .json(),
        );
    }

    /// A target refused at attempt time (its circuit opened, or it cannot carry a feature).
    pub(super) fn skipped(&mut self, target: &Target, key: bool, reason: &str) {
        let key_id = key.then_some(target.key_id.as_str());
        self.skipped.note(
            &target.provider_id,
            &target.provider_name,
            key_id,
            &target.model,
        );
        self.skipped_row(&target.provider_name, key_id, &target.model, reason);
    }

    fn remember(
        &mut self,
        target: &Target,
        protocol: Option<Protocol>,
        meta: UpstreamMeta,
        serving: bool,
        sent: Option<Sent>,
    ) {
        self.context
            .note_attempt(&target.provider_id, &target.model);
        self.last = Some(Attempted {
            provider_id: target.provider_id.clone(),
            provider_name: target.provider_name.clone(),
            key_id: target.key_id.clone(),
            model: target.model.clone(),
            protocol,
            meta,
            first_content: None,
            serving,
            in_flight: false,
            sent,
            error_body: None,
        });
    }

    /// An attempt against `target` is starting (a drop before response headers still keeps its
    /// provider, key and model on the row; `context.note_attempt` waits until the attempt ends so
    /// the attempt counter is never double-counted).
    pub(super) fn attempting(&mut self, target: &Target, protocol: Option<Protocol>) {
        self.last = Some(Attempted {
            provider_id: target.provider_id.clone(),
            provider_name: target.provider_name.clone(),
            key_id: target.key_id.clone(),
            model: target.model.clone(),
            protocol,
            meta: UpstreamMeta::unsent(),
            first_content: None,
            serving: false,
            in_flight: true,
            sent: None,
            error_body: None,
        });
    }

    /// An attempt was cancelled before it served anything.
    pub(super) fn canceled(
        &mut self,
        target: &Target,
        protocol: Option<Protocol>,
        meta: UpstreamMeta,
        sent: Option<Sent>,
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
                retry_after_s: None,
            }
            .json(),
        );
        self.timings
            .push(meta.timing(PhaseValue::NotNeeded, latency));
        self.remember(target, protocol, meta, false, sent);
    }

    /// An attempt failed before it served anything.
    pub(super) fn failed(
        &mut self,
        target: &Target,
        protocol: Option<Protocol>,
        meta: UpstreamMeta,
        error: &AttemptError,
        sent: Option<Sent>,
    ) {
        let (outcome, reason) = describe(error);
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
                retry_after_s: error
                    .retry_after()
                    .filter(|_| matches!(error.status(), Some(429 | 503)))
                    .and_then(|wait| u64::try_from(wait.as_millis().div_ceil(1000)).ok()),
            }
            .json(),
        );
        self.timings
            .push(meta.timing(PhaseValue::NotNeeded, latency));
        let error_body = self
            .capture
            .is_some()
            .then(|| match error {
                AttemptError::Status { body, .. } => Some(bound_body(body.as_bytes())),
                AttemptError::NotSse { preview, .. } => Some(bound_body(preview.as_bytes())),
                _ => None,
            })
            .flatten();
        self.remember(target, protocol, meta, false, sent);
        if let Some(last) = &mut self.last {
            last.error_body = error_body;
        }
    }

    /// An attempt answered and now serves the call.
    pub(super) fn serving(
        &mut self,
        target: &Target,
        protocol: Option<Protocol>,
        meta: UpstreamMeta,
        sent: Option<Sent>,
    ) {
        self.remember(target, protocol, meta, true, sent);
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

    /// Takes the token counts a stream or answer reported.
    pub(super) fn add_usage(&mut self, usage: &HubUsage) {
        self.usage.merge(usage);
    }

    /// Adds model output to the text the token estimate falls back on.
    pub(super) fn generated(&mut self, text: &str) {
        if self.generated.len() < crate::llm::hub::MAX_TEXT_BYTES {
            self.generated.push_str(text);
        }
    }

    /// Counts bytes delivered to the client.
    pub(super) fn sent_to_client(&mut self, bytes: usize) {
        self.resp_bytes += bytes as u64;
    }

    /// The client's answer as a whole body (non-streaming calls and errors).
    pub(super) fn client_answer(&mut self, content_type: &str, dropped: Option<&str>, body: &[u8]) {
        self.resp_bytes = body.len() as u64;
        if self.capture.is_some() {
            let bounded = bound_body(body);
            self.client_headers(content_type, dropped);
            if let Some(capture) = &mut self.capture {
                capture.client_resp_body = Some(bounded);
            }
        }
    }

    /// The client's answer as a stream: the headers now, the frames at the end.
    pub(super) fn client_stream(&mut self, content_type: &str, dropped: Option<&str>) {
        self.client_headers(content_type, dropped);
    }

    fn client_headers(&mut self, content_type: &str, dropped: Option<&str>) {
        let request_id = self.context.request_id.clone();
        if let Some(capture) = &mut self.capture {
            capture.client_resp_headers = vec![
                ("content-type".to_owned(), content_type.to_owned()),
                ("x-pingdeck-request-id".to_owned(), request_id),
            ];
            if let Some(names) = dropped {
                capture
                    .client_resp_headers
                    .push((DROPPED_HEADER.to_owned(), names.to_owned()));
            }
        }
    }

    /// The upstream's whole answer, stored when it differs from what the client got.
    pub(super) fn upstream_answer(&mut self, body: &[u8]) {
        if let Some(capture) = &mut self.capture {
            capture.upstream_resp_body = Some(bound_body(body));
        }
    }

    /// The frames of the client's stream and, for a translated one, the upstream's.
    pub(super) fn frames(&mut self, client: CapturedFrames, upstream: Option<CapturedFrames>) {
        if let Some(capture) = &mut self.capture {
            capture.client_frames = Some(client);
            capture.upstream_frames = upstream;
        }
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
        // Only a serving attempt is taken here; a failed one stays for the row's columns.
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
                    retry_after_s: None,
                }
                .json(),
            );
            timings.push(last.meta.timing(first_token, end_ms));
            self.last = Some(last);
        } else if let Some(last) = self.last.as_ref().filter(|last| last.in_flight) {
            self.context.note_attempt(&last.provider_id, &last.model);
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
                    retry_after_s: None,
                }
                .json(),
            );
            timings.push(last.meta.timing(PhaseValue::NotNeeded, latency));
        }
        let ended_attempt_ms: u64 = timings.iter().filter_map(|t| t.end_ms.ms()).sum();
        // The call took at least as long as its attempts (rounding up can make them add to more).
        let total_ms = millis(self.started).max(ended_attempt_ms);
        let facts = self.last.take();
        let any_attempt_failed = attempts.iter().any(|row| {
            !matches!(
                row.get("outcome").and_then(Value::as_str),
                Some("success" | "skipped")
            )
        });
        let serving = facts.as_ref().is_some_and(|last| last.serving);
        let mut usage = std::mem::take(&mut self.usage);
        let mut estimated = false;
        if outcome == "success"
            && serving
            && usage.input_tokens.is_none()
            && usage.output_tokens.is_none()
        {
            usage.input_tokens = Some(estimate_tokens_bytes(&self.request));
            usage.output_tokens = Some(estimate_tokens(&self.generated));
            estimated = true;
        } else if outcome != "success"
            && serving
            && usage.output_tokens.is_none()
            && !self.generated.is_empty()
        {
            // Cut short after it served; the reported input stays, the rest is estimated.
            usage
                .input_tokens
                .get_or_insert_with(|| estimate_tokens_bytes(&self.request));
            usage.output_tokens = Some(estimate_tokens(&self.generated));
            estimated = true;
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
            protocol_in: self.protocol_in.as_str().to_owned(),
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
            tokens_estimated: estimated,
            attempts: Value::Array(attempts),
            error,
            // Retired with aliases; the column keeps old rows only.
            alias: None,
            translated: facts
                .as_ref()
                .and_then(|last| last.sent.as_ref())
                .is_some_and(|sent| sent.reencoded),
            failure_kind: kind,
            upstream_request_id: facts
                .as_ref()
                .and_then(|last| last.meta.upstream_request_id.clone()),
            req_bytes: Some(self.request.len() as u64),
            resp_bytes: Some(self.resp_bytes),
            ..RequestLogEntry::default()
        };
        let queue_ms = total_ms.saturating_sub(ended_attempt_ms);
        let timing = (!timings.is_empty()).then_some(TimingJson {
            queue_ms,
            attempts: timings,
        });
        entry.timing.clone_from(&timing);
        let mut job = LogJob::from_entry(entry);
        job.any_attempt_failed = any_attempt_failed;
        job.raw = self.capture.take().map(|capture| {
            let mut raw = *capture;
            if let Some(last) = facts {
                raw.upstream_status = last.meta.status;
                raw.upstream_resp_headers = last.meta.response_headers;
                if let Some(sent) = last.sent {
                    raw.upstream_url = sent.url;
                    raw.upstream_req_headers = sent.headers;
                    raw.upstream_req_body = sent.body;
                    raw.translation_notes = sent.notes;
                    raw.hub_request = sent.hub;
                }
                if last.error_body.is_some() {
                    raw.upstream_resp_body = last.error_body;
                }
            }
            raw.timing = timing.unwrap_or_default();
            raw
        });
        self.context.submit(job);
    }
}

impl Drop for Journal {
    fn drop(&mut self) {
        self.end(&Ended::Canceled);
    }
}
