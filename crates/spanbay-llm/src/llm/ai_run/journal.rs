//! The request-log row of one AI run (`source = api`).
//!
//! Single responsibility: turn a finished run into one [`RequestLogEntry`] (and, when the user
//! keeps the exchange, the bounded raw capture), queue it on the runtime's log writer and learn
//! the id of the stored row. The writer thread redacts, prices and rolls up; nothing here stores
//! a secret.

use std::sync::Arc;

use serde_json::{Value, json};

use super::assemble::Assembled;
use super::types::{AiRunSpec, AiRunStatus, Destination};
use crate::llm::capture::{BoundedBody, RawExchange, bound_body};
use crate::llm::capture_record::CapturedFrames;
use crate::llm::gateway::logwriter::LogJob;
use crate::llm::runtime::LlmRuntime;
use crate::llm::store::{self, RequestLogEntry};

/// The wire side of a kept exchange.
pub(super) struct Wirelog {
    /// The URL that was requested.
    pub url: String,
    /// Headers as sent (credentials included: the writer redacts them).
    pub sent_headers: Vec<(String, String)>,
    /// Status of the answer.
    pub status: Option<u16>,
    /// Response headers as received.
    pub response_headers: Vec<(String, String)>,
    /// A plain body (non-streaming answer or an error).
    pub body: Option<BoundedBody>,
    /// The frames of a stream.
    pub frames: Option<CapturedFrames>,
}

/// The status column of a run that had no answer, like the gateway's: 504 for a timeout, 499 for
/// a stop, 502 for anything else.
fn status_column(assembled: &Assembled) -> u16 {
    let dto = &assembled.dto;
    dto.http_status.unwrap_or(match dto.status {
        AiRunStatus::TimedOut => 504,
        AiRunStatus::Stopped => 499,
        AiRunStatus::Unsupported | AiRunStatus::Skipped => 400,
        _ => 502,
    })
}

/// Queues the row of the run and returns its id and `request_id` once stored (`None` when no
/// writer or database exists, as for an in-memory app).
pub(super) async fn record(
    runtime: &Arc<LlmRuntime>,
    spec: &AiRunSpec,
    assembled: &Assembled,
    ts_ms: i64,
    wire: Wirelog,
    upstream_request_id: Option<String>,
) -> Option<(i64, String)> {
    let dto = &assembled.dto;
    let (provider_id, provider_name, key_id) = match &spec.destination {
        Destination::Provider(target) => (
            Some(target.provider_id.clone()),
            Some(target.provider_name.clone()),
            Some(target.key_id.clone()),
        ),
        Destination::Custom { .. } => (None, None, None),
    };
    let outcome = match dto.status {
        AiRunStatus::Done => "success",
        AiRunStatus::Stopped => "canceled",
        _ => "failure",
    };
    let failed = outcome == "failure";
    let error = failed.then(|| dto.message.clone()).flatten();
    let status = status_column(assembled);
    let latency = dto.total_ms;
    let attempts: Value = json!([{
        "provider": provider_name.clone().unwrap_or_else(|| super::send::host_of(&wire.url)),
        "key_id": key_id,
        "model": spec.model,
        "protocol": spec.protocol.as_str(),
        "status": dto.http_status,
        "outcome": outcome,
        "latency_ms": latency,
        "error": error,
    }]);
    let rq = store::new_request_id();
    let entry = RequestLogEntry {
        ts_ms,
        protocol_in: spec.protocol.as_str().to_owned(),
        requested_model: spec.model.clone(),
        provider_id,
        provider_name,
        key_id,
        upstream_model: Some(spec.model.clone()),
        protocol_out: Some(spec.protocol.as_str().to_owned()),
        status,
        outcome: outcome.to_owned(),
        stream: spec.stream,
        latency_ms: latency,
        ttft_ms: dto.ttft_ms,
        input_tokens: dto.usage.input_tokens,
        output_tokens: dto.usage.output_tokens,
        cached_tokens: dto.usage.cached_tokens,
        cache_write_tokens: dto.usage.cache_write_tokens,
        reasoning_tokens: dto.usage.reasoning_tokens,
        tokens_estimated: dto.usage.estimated,
        attempts,
        error,
        source: "api".to_owned(),
        source_ref: Some(spec.run_id.clone()),
        request_id: Some(rq.clone()),
        failure_kind: assembled.failure.filter(|_| failed),
        upstream_request_id,
        req_bytes: Some(spec.body.len() as u64),
        resp_bytes: Some(assembled.resp_bytes),
        timing: Some(dto.timing.clone()),
        ..RequestLogEntry::default()
    };
    let mut job = LogJob::from_entry(entry);
    job.usage = assembled.usage;
    job.any_attempt_failed = failed;
    job.keep_exchange = spec.keep_exchange;
    if spec.keep_exchange {
        job.raw = Some(RawExchange {
            request_log_id: 0,
            ts_ms,
            client_protocol: spec.protocol,
            client_req_headers: spec.headers.clone(),
            client_req_body: Some(bound_body(spec.body.as_bytes())),
            hub_request: None,
            upstream_url: wire.url,
            upstream_req_headers: wire.sent_headers,
            upstream_req_body: None,
            upstream_status: wire.status,
            upstream_resp_headers: wire.response_headers,
            upstream_resp_body: wire.body,
            client_resp_headers: Vec::new(),
            client_resp_body: None,
            upstream_frames: wire.frames,
            client_frames: None,
            translation_notes: Vec::new(),
            timing: dto.timing.clone(),
        });
    }
    let queue = runtime.log().ok()?;
    queue.submit(job);
    let runtime = Arc::clone(runtime);
    tokio::task::spawn_blocking(move || {
        queue.flush();
        let connection = runtime.db().open().ok()?;
        store::log_id_for_request_id(&connection, &rq)
            .ok()
            .flatten()
            .map(|id| (id, rq))
    })
    .await
    .ok()
    .flatten()
}
