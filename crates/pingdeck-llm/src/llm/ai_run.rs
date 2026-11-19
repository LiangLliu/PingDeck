//! One AI API call, end to end: plan, send, read, assemble, log.
//!
//! Single responsibility: run ONE call of an AI API request and say what happened. The use case
//! (`app::ai_run_service`) resolves variables and scripts and hands over an [`AiRunSpec`]; this
//! module fixes URL and credential of a provider target (the real wire: no protocol translation),
//! sends through the runtime's client or, for a custom URL, the guarded streaming transport,
//! reads a stream frame by frame (events to the sink at most 30 per second, a stop keeps the
//! partial reply), shapes errors, measures TTFT, tokens per second, timing and cost, and writes
//! the `source = api` row of the request log.
//!
//! The caller emits the closing [`AiRunEvent::Done`] itself, after its scripts and assertions
//! have filled the rest of the result; this module flushes pending frames before it returns, so
//! `Done` is always the last event.

mod assemble;
mod attachments;
mod journal;
mod merge;
mod read;
mod send;
mod types;
mod wire;

#[cfg(test)]
mod tests;

use std::sync::Arc;
use std::time::Instant;

use assemble::{Clock, Head, Stage, assemble};
use journal::Wirelog;
use read::{read_body, read_stream};
pub use types::{
    AiHeader, AiResultDto, AiRunEvent, AiRunOutcome, AiRunSpec, AiRunStatus, AiSentRequest,
    AiToolCallDto, AiUsageDto, AiVectorDto, CostNoteCode, Destination,
};
pub use wire::{missing_endpoint, protocol_label};

use crate::llm::adapters::codec;
use crate::llm::capture::bound_body;
use crate::llm::runtime::{LlmRuntime, now_ms};
use pingdeck_base::cancel::CancelToken;
use pingdeck_model::events::EventSink;
use pingdeck_model::models::bounded_body_preview;

/// Longest error body kept to explain a rejection.
const ERROR_BODY_BYTES: usize = 64 * 1024;
/// The sent body text the Raw tab shows is cut here.
const SENT_BODY_PREVIEW_BYTES: usize = 256 * 1024;
/// Header names the upstream's own request id may use, best first.
const REQUEST_ID_HEADERS: [&str; 5] = [
    "x-request-id",
    "request-id",
    "openai-request-id",
    "x-amzn-requestid",
    "x-amzn-request-id",
];

/// Runs one call: `Started` and the coalesced `Frames` events go to `sink`; the result comes
/// back (and its row is in the request log, `log_row_id`). Never fails: every way a call can end
/// is a status of the result.
pub async fn run_ai_request(
    runtime: &Arc<LlmRuntime>,
    spec: AiRunSpec,
    sink: &dyn EventSink<AiRunEvent>,
    cancel: &CancelToken,
) -> AiRunOutcome {
    let t0 = Instant::now();
    let ts_ms = now_ms();
    sink.emit(AiRunEvent::Started {
        run_id: spec.run_id.clone(),
        model: spec.model.clone(),
    });
    let planned = wire::plan(&spec);
    let url = planned
        .as_ref()
        .map_or_else(|_| spec.shown_url.clone(), |planned| planned.url.clone());
    let hidden = planned
        .as_ref()
        .map(|planned| planned.hidden.clone())
        .unwrap_or_default();
    let mut sent = AiSentRequest {
        method: "POST".to_owned(),
        url: spec.shown_url.clone(),
        headers: match &planned {
            Ok(planned) => wire::masked_view(planned),
            Err(_) => wire::masked_view(&wire::Planned {
                url: String::new(),
                headers: spec.headers.clone(),
                injected: std::collections::HashSet::new(),
                hidden: Vec::new(),
                secrets: spec.secrets.clone(),
            }),
        },
        body: String::new(),
        body_bytes: spec.body.len(),
        body_truncated: false,
    };
    let masked_body = pingdeck_base::secrets::mask_secrets(&spec.body, &spec.secrets);
    (sent.body, sent.body_truncated) = bounded_body_preview(&masked_body, SENT_BODY_PREVIEW_BYTES);

    let mut sent_at = None;
    let mut wirelog = Wirelog {
        url: url.clone(),
        sent_headers: Vec::new(),
        status: None,
        response_headers: Vec::new(),
        body: None,
        frames: None,
    };
    let mut upstream_request_id = None;
    let stage = match planned {
        Err(refusal) => Stage::Refused {
            status: refusal.status,
            message: refusal.message,
        },
        Ok(planned) => {
            wirelog.sent_headers.clone_from(&planned.headers);
            let body = if attachments::has_placeholder(&spec.body) {
                let text = spec.body.clone();
                let store = spec.attachments_dir.clone();
                tokio::task::spawn_blocking(move || attachments::resolve(&text, store.as_deref()))
                    .await
                    .unwrap_or_else(|_| {
                        Err(attachments::AttachmentError(
                            "The attachment reader failed".to_owned(),
                        ))
                    })
            } else {
                Ok(spec.body.clone().into_bytes())
            };
            match body {
                Err(error) => Stage::Refused {
                    status: AiRunStatus::Unsupported,
                    message: error.to_string(),
                },
                Ok(bytes) => {
                    sent.body_bytes = bytes.len();
                    let mut at = Instant::now();
                    let opened = send::open(runtime, &spec, &planned, bytes, cancel, &mut at).await;
                    sent_at = Some(at);
                    match opened {
                        Err(failed) => Stage::NotAnswered(failed),
                        Ok(opened) => {
                            let head = Head {
                                status: opened.stream.status,
                                headers: opened.stream.headers.clone(),
                                conn: opened.conn,
                                headers_ms: opened.headers_ms,
                            };
                            wirelog.status = Some(head.status);
                            wirelog.response_headers.clone_from(&head.headers);
                            upstream_request_id = REQUEST_ID_HEADERS.iter().find_map(|name| {
                                head.headers
                                    .iter()
                                    .find(|(own, _)| own == name)
                                    .map(|(_, value)| value.chars().take(128).collect::<String>())
                            });
                            let ok = (200..300).contains(&head.status);
                            let event_stream =
                                opened.stream.header("content-type").is_some_and(|value| {
                                    value.to_ascii_lowercase().contains("text/event-stream")
                                });
                            let chat = codec(spec.protocol);
                            let mut reading = match chat {
                                Some(chat) if ok && event_stream => {
                                    read_stream(
                                        opened.stream,
                                        chat,
                                        spec.max_response_bytes,
                                        spec.keep_exchange,
                                        at,
                                        sink,
                                    )
                                    .await
                                }
                                _ => {
                                    let cap = if ok {
                                        spec.max_response_bytes
                                    } else {
                                        ERROR_BODY_BYTES
                                    };
                                    read_body(opened.stream, cap).await
                                }
                            };
                            if spec.keep_exchange {
                                wirelog.frames = reading.recorded.take();
                                if !reading.streamed {
                                    wirelog.body = Some(bound_body(&reading.raw));
                                }
                            }
                            Stage::Answered {
                                head,
                                reading: Box::new(reading),
                            }
                        }
                    }
                }
            }
        }
    };
    let clock = Clock {
        t0,
        sent_at,
        ended: Instant::now(),
    };
    let mut assembled = assemble(&spec, &url, sent, &hidden, stage, &clock);
    let row = journal::record(
        runtime,
        &spec,
        &assembled,
        ts_ms,
        wirelog,
        upstream_request_id,
    )
    .await;
    assembled.dto.kept_in_log = spec.keep_exchange && row.is_some();
    (assembled.dto.log_row_id, assembled.dto.log_request_id) = row.unzip();
    AiRunOutcome {
        result: assembled.dto,
        raw_body: assembled.raw_body,
    }
}
