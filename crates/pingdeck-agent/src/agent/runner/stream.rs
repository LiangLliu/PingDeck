//! One streamed model call: events to the UI, recovery points, stall detection, retries.
//!
//! Single responsibility: turn a [`ChatBackend`] stream into a [`TurnOutcome`] while publishing
//! deltas, saving the answer so far as a partial row every 2 s and at each tool-call boundary, and
//! noticing a stream that stopped talking: once the first event arrived, 90 s without a byte ends
//! the attempt (before it, only the wake rule applies; the client's first-byte timeout and failover
//! own that wait). Before any answer content the call is made again: once after a stall, and up to
//! [`TRANSIENT_RETRIES`] times after a transient failure the client did not already retry, waiting
//! 2 s then 6 s (±25 %) or the upstream's `Retry-After`, at most 30 s. A retry after
//! content would repeat it.

use std::collections::HashMap;
use std::time::{Duration, Instant};

use futures_util::StreamExt;

use pingdeck_base::cancel::CancelToken;
use pingdeck_base::error::AppError;
use pingdeck_llm::llm::client::{ChatError, ChatErrorKind};
use pingdeck_llm::llm::hub::{HubAttempt, HubEvent, HubPart, HubRequest, collect_events};

use super::super::limits::TRANSIENT_RETRIES;
use super::super::model::AgentEvent;
use super::{
    CallIds, ChatBackend, PARTIAL_SAVE_INTERVAL, RunHost, RunInput, STALL_TIMEOUT, ServedModel,
    TurnFailure, TurnOutcome, WAKE_STALL_TIMEOUT,
};

/// The stall timer before the first event: long enough that only the wake rule ends it.
const BEFORE_FIRST_EVENT: Duration = Duration::from_secs(24 * 60 * 60);
/// The longest wait before a retry; a longer `Retry-After` is the user's call.
const RETRY_WAIT_CAP: Duration = Duration::from_secs(30);

/// How one attempt ended.
enum Attempt {
    Done(TurnOutcome),
    Failed(TurnFailure),
    /// No byte for [`STALL_TIMEOUT`].
    Stalled(TurnFailure),
}

/// The text and reasoning an event list has produced so far.
fn partial_parts(events: &[HubEvent]) -> Vec<HubPart> {
    let usable: Vec<HubEvent> = events
        .iter()
        .filter(|event| !matches!(event, HubEvent::Error { .. }))
        .cloned()
        .collect();
    collect_events(&usable)
        .map(|response| {
            response
                .content
                .into_iter()
                .filter(|part| matches!(part, HubPart::Text { .. } | HubPart::Reasoning { .. }))
                .collect()
        })
        .unwrap_or_default()
}

fn failure(error: AppError, events: &[HubEvent], content_started: bool) -> TurnFailure {
    TurnFailure {
        error,
        partial: partial_parts(events),
        content_started,
        chat_kind: None,
        provider_failure: None,
        attempts: Vec::new(),
        status: None,
        retry_after_ms: None,
        retried: false,
        transient: false,
    }
}

/// [`failure`] for an error the client typed.
fn typed_failure(error: ChatError, events: &[HubEvent], content_started: bool) -> TurnFailure {
    let transient = error.all_transient();
    let ChatError {
        kind,
        error,
        failure: provider_failure,
        attempts,
        status,
        retry_after_ms,
        retried,
        ..
    } = error;
    TurnFailure {
        chat_kind: Some(kind),
        provider_failure,
        attempts,
        status,
        retry_after_ms,
        retried,
        transient,
        ..failure(error, events, content_started)
    }
}

/// Whether the loop may make a failed call again: every attempt of the client's walk failed
/// transiently, the client did not already retry in place and nothing was streamed.
fn retryable(failed: &TurnFailure) -> bool {
    failed.chat_kind == Some(ChatErrorKind::Failed)
        && failed.transient
        && !failed.retried
        && !failed.content_started
}

/// The wait before retry `retry` (1-based): the upstream's `Retry-After` when it sent one, else
/// 2 s then 6 s spread by `spread_permille` (0 to 500 maps to -25 % to +25 %); at most 30 s.
pub(super) fn retry_delay(
    retry: u32,
    retry_after_ms: Option<u64>,
    spread_permille: u64,
) -> Duration {
    let wait = match retry_after_ms {
        Some(ms) => Duration::from_millis(ms),
        None => {
            let base: u64 = if retry <= 1 { 2_000 } else { 6_000 };
            Duration::from_millis(base * (750 + spread_permille.min(500)) / 1_000)
        }
    };
    wait.min(RETRY_WAIT_CAP)
}

/// A random spread for [`retry_delay`]; the middle when the OS has no randomness to give.
fn spread_permille() -> u64 {
    let mut bytes = [0_u8; 2];
    if getrandom::fill(&mut bytes).is_ok() {
        u64::from(u16::from_le_bytes(bytes)) % 501
    } else {
        250
    }
}

/// A short reason for a retry notice: the status, else the failure category.
fn retry_reason(failed: &TurnFailure) -> String {
    failed.status.map_or_else(
        || {
            failed
                .provider_failure
                .map_or("network", |kind| kind.as_str())
                .to_owned()
        },
        |status| format!("HTTP {status}"),
    )
}

/// Whole milliseconds of `duration`.
fn millis(duration: std::time::Duration) -> u64 {
    u64::try_from(duration.as_millis()).unwrap_or(u64::MAX)
}

/// Calls the model, again after a stall or a transient failure that came before any content (see
/// the module doc), and returns the finished answer.
///
/// # Errors
/// A [`TurnFailure`] holding the error (`Cancelled` when `cancel` fired) and the content received.
#[allow(clippy::result_large_err)] // a cold failure path: boxing would allocate and change the crate-internal signature
pub(super) async fn stream_turn<B: ChatBackend, H: RunHost>(
    backend: &B,
    host: &H,
    input: &RunInput,
    request: HubRequest,
    message_id: &str,
    call_ids: &CallIds,
    cancel: &CancelToken,
) -> Result<TurnOutcome, TurnFailure> {
    let mut retries: u32 = 0;
    loop {
        match attempt(backend, host, input, &request, message_id, call_ids, cancel).await {
            Attempt::Done(outcome) => return Ok(outcome),
            Attempt::Stalled(stalled) => {
                // A stall already waited 90 s: one more try, right away.
                if retries > 0 || stalled.content_started {
                    return Err(stalled);
                }
                retries += 1;
                host.emit(AgentEvent::coded_notice(
                    "stall_retry",
                    "The model stopped responding; trying once more.",
                    &[],
                ));
            }
            Attempt::Failed(failed) => {
                if retries >= TRANSIENT_RETRIES || !retryable(&failed) || cancel.is_cancelled() {
                    return Err(failed);
                }
                retries += 1;
                let delay = retry_delay(retries, failed.retry_after_ms, spread_permille());
                let delay_s = delay.as_millis().div_ceil(1_000).to_string();
                let reason = retry_reason(&failed);
                host.emit(AgentEvent::coded_notice(
                    "retrying",
                    format!(
                        "The model call failed ({reason}); retry {retries} of {TRANSIENT_RETRIES} in {delay_s} s."
                    ),
                    &[
                        ("attempt", retries.to_string()),
                        ("of", TRANSIENT_RETRIES.to_string()),
                        ("delay_s", delay_s),
                        ("reason", reason),
                    ],
                ));
                tokio::select! {
                    () = host.clock().sleep(delay) => {}
                    () = cancel.cancelled() => {
                        return Err(failure(AppError::cancelled(), &[], false));
                    }
                }
            }
        }
    }
}

/// The note that a fallback answered: the first attempt that ran on another model, `None` when
/// the call's own target served (an in-place retry is not a fallback).
fn fallback_notice(served: &ServedModel, attempts: &[HubAttempt]) -> Option<AgentEvent> {
    let failed = attempts.iter().find(|attempt| {
        attempt.action != "skipped"
            && (attempt.provider_id != served.provider_id || attempt.model != served.model)
    })?;
    let reason = failed.status.map_or_else(
        || failed.kind.clone().unwrap_or_else(|| "error".to_owned()),
        |status| format!("HTTP {status}"),
    );
    Some(AgentEvent::coded_notice(
        "fallback",
        format!(
            "{} failed ({reason}); {} answered instead.",
            failed.model, served.model
        ),
        &[
            ("from", failed.model.clone()),
            ("to", served.model.clone()),
            ("reason", reason),
        ],
    ))
}

async fn attempt<B: ChatBackend, H: RunHost>(
    backend: &B,
    host: &H,
    input: &RunInput,
    request: &HubRequest,
    message_id: &str,
    call_ids: &CallIds,
    cancel: &CancelToken,
) -> Attempt {
    let mut ids = call_ids.clone();
    let meta = super::call_meta(host, Some(message_id), false);
    let mut stream = backend.stream(
        &meta,
        input.target.clone(),
        request.clone(),
        Some(cancel.clone()),
    );
    let mut events: Vec<HubEvent> = Vec::new();
    let mut call_ids: HashMap<u32, String> = HashMap::new();
    let mut content_started = false;
    // `started` is when the request went out, `first_content` when the first answer content came.
    let started = host.clock().now();
    let mut first_content: Option<Instant> = None;
    let mut served: Option<ServedModel> = None;
    let mut last_save: Instant = started;
    let mut first_event = false;
    loop {
        let stall = if first_event {
            STALL_TIMEOUT
        } else {
            BEFORE_FIRST_EVENT
        };
        let item = tokio::select! {
            item = stream.next() => item,
            () = cancel.cancelled() => {
                return Attempt::Failed(failure(AppError::cancelled(), &events, content_started));
            }
            () = host.clock().stall_wait(stall, WAKE_STALL_TIMEOUT) => {
                let error = AppError::network("The model stopped responding (no data for 90 s)");
                let mut stalled = failure(error, &events, content_started);
                stalled.chat_kind = Some(ChatErrorKind::IdleStall);
                return Attempt::Stalled(stalled);
            }
        };
        let Some(item) = item else { break };
        first_event = true;
        let mut event = match item {
            Ok(event) => event,
            Err(error) if error.kind == ChatErrorKind::IdleStall => {
                return Attempt::Stalled(typed_failure(error, &events, content_started));
            }
            Err(error) => {
                return Attempt::Failed(typed_failure(error, &events, content_started));
            }
        };
        // From here on the call is known by its chat-unique id, in events, parts and results.
        if let HubEvent::ToolCallStart { id, .. } = &mut event {
            *id = ids.assign(id);
        }
        let mut boundary = false;
        match &event {
            HubEvent::TextDelta { text } => {
                content_started = true;
                first_content.get_or_insert_with(|| host.clock().now());
                host.emit(AgentEvent::TextDelta { text: text.clone() });
            }
            HubEvent::ReasoningDelta { text } => {
                content_started = true;
                first_content.get_or_insert_with(|| host.clock().now());
                host.emit(AgentEvent::ReasoningDelta { text: text.clone() });
            }
            HubEvent::Citation { citation } => {
                content_started = true;
                host.emit(AgentEvent::CitationAdded {
                    citation: citation.clone(),
                });
            }
            HubEvent::ToolCallStart {
                index, id, name, ..
            } => {
                content_started = true;
                first_content.get_or_insert_with(|| host.clock().now());
                boundary = true;
                call_ids.insert(*index, id.clone());
                host.emit(AgentEvent::ToolCallStarted {
                    call_id: id.clone(),
                    name: name.clone(),
                    source: host.tool_source(name),
                });
            }
            HubEvent::ToolCallDelta { index, arguments } => {
                if let Some(call_id) = call_ids.get(index) {
                    host.emit(AgentEvent::ToolCallArgsDelta {
                        call_id: call_id.clone(),
                        arguments: arguments.clone(),
                    });
                }
            }
            HubEvent::Error { message, .. } => {
                let error = AppError::network(message.clone());
                events.push(event);
                return Attempt::Failed(failure(error, &events, content_started));
            }
            HubEvent::Served {
                provider_id,
                model,
                attempts,
            } => {
                let answered = ServedModel {
                    provider_id: provider_id.clone(),
                    model: model.clone(),
                };
                if let Some(notice) = fallback_notice(&answered, attempts) {
                    host.emit(notice);
                }
                served = Some(answered);
                continue;
            }
            _ => {}
        }
        events.push(event);
        let now = host.clock().now();
        if boundary || now.saturating_duration_since(last_save) >= PARTIAL_SAVE_INTERVAL {
            last_save = now;
            let parts = partial_parts(&events);
            if !parts.is_empty() {
                host.save_partial(message_id, parts).await;
            }
        }
    }
    drop(stream);
    let ended = host.clock().now();
    match collect_events(&events) {
        Ok(response) => Attempt::Done(TurnOutcome {
            content: response.content,
            usage: response.usage,
            finish: response.finish,
            ttft_ms: first_content.map(|first| millis(first.saturating_duration_since(started))),
            stream_ms: first_content.map(|first| millis(ended.saturating_duration_since(first))),
            served,
        }),
        Err(message) => Attempt::Failed(failure(
            AppError::network(message),
            &events,
            content_started,
        )),
    }
}
