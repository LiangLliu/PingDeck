//! The result of a run: status words, the shaped message, timing, usage, cost and the tabs' data.
//!
//! Single responsibility: pure assembly from what happened (nothing sent, a send that failed, an
//! answer read to its ending) into an [`AiResultDto`], following the result vocabulary:
//! `Rejected 401: <provider message>`, `Cannot reach <host>: <reason>`, `Timed out`,
//! `Stopped at 1.0 s`, `The model returned no text`.

use std::time::Instant;

use super::merge::{Merged, from_body, from_stream};
use super::read::{Ending, Reading};
use super::send::{SendFailure, host_of, unreachable_reason};
use super::types::{
    AiHeader, AiResultDto, AiRunSpec, AiRunStatus, AiSentRequest, AiUsageDto, CostNoteCode,
};
use super::wire::protocol_label;
use crate::llm::capture::{MAX_FRAMES, PhaseValue, bound_body};
use crate::llm::capture_record::{CapturedBody, CapturedFrames, TimingJson};
use crate::llm::failure::{FailureKind, classify};
use crate::llm::upstream::{ConnFacts, ceil_ms, provider_message};
use crate::llm::usage::{CostUsage, PriceResolution, cost, estimate_tokens};
use spanbay_base::secrets::mask_secrets;

/// Output tokens from which a tokens-per-second figure is shown (D-44).
const TPS_MIN_TOKENS: u64 = 300;
/// Frames the result carries (the drawer's cap).
const RESULT_FRAMES: usize = MAX_FRAMES;

/// The response head of an answer.
pub(super) struct Head {
    pub status: u16,
    pub headers: Vec<(String, String)>,
    pub conn: ConnFacts,
    /// Milliseconds from the send to the head.
    pub headers_ms: u64,
}

/// What happened to the call.
pub(super) enum Stage {
    /// Nothing was sent.
    Refused {
        status: AiRunStatus,
        message: String,
    },
    /// The send failed before a head arrived.
    NotAnswered(SendFailure),
    /// A head arrived and the body was read to `reading.ending`.
    Answered { head: Head, reading: Box<Reading> },
}

/// The assembled result and what the log row needs besides it.
pub(super) struct Assembled {
    pub dto: AiResultDto,
    pub raw_body: String,
    pub failure: Option<FailureKind>,
    pub usage: Option<CostUsage>,
    pub resp_bytes: u64,
}

/// The moments of the call.
pub(super) struct Clock {
    /// The call began.
    pub t0: Instant,
    /// The request left (`None`: it never did).
    pub sent_at: Option<Instant>,
    /// The call ended.
    pub ended: Instant,
}

/// Response headers worth reading first: waits, rate limits and request ids.
fn is_priority(name: &str) -> bool {
    let name = name.to_ascii_lowercase();
    name.starts_with("retry-after")
        || name.starts_with("x-ratelimit-")
        || name.starts_with("ratelimit-")
        || name.ends_with("request-id")
        || name.ends_with("requestid")
}

fn ordered_headers(headers: &[(String, String)]) -> Vec<AiHeader> {
    let (first, rest): (Vec<_>, Vec<_>) = headers.iter().partition(|(name, _)| is_priority(name));
    first
        .into_iter()
        .chain(rest)
        .map(|(name, value)| AiHeader {
            name: name.clone(),
            value: value.clone(),
        })
        .collect()
}

fn text_of(raw: &[u8]) -> String {
    String::from_utf8_lossy(raw).into_owned()
}

/// How the answer ended, as status, message, HTTP status and failure category.
struct Verdict {
    status: AiRunStatus,
    http: Option<u16>,
    message: Option<String>,
    kind: Option<FailureKind>,
}

impl Verdict {
    fn of(status: AiRunStatus, http: Option<u16>, message: String, kind: FailureKind) -> Self {
        Self {
            status,
            http,
            message: Some(message),
            kind: Some(kind),
        }
    }
}

fn rejected(head: &Head, body: &[u8]) -> Verdict {
    let status = head.status;
    let text = text_of(body);
    let message = if (300..400).contains(&status) {
        let location = head
            .headers
            .iter()
            .find(|(name, _)| name == "location")
            .map(|(_, value)| value.as_str());
        match location {
            Some(location) => {
                format!("Rejected {status}: redirect to {location} was not followed")
            }
            None => format!("Rejected {status}: a redirect was not followed"),
        }
    } else {
        let said = provider_message(&text);
        let said = if said.is_empty() {
            http::StatusCode::from_u16(status)
                .ok()
                .and_then(|code| code.canonical_reason())
                .unwrap_or("error")
                .to_owned()
        } else {
            said
        };
        format!("Rejected {status}: {said}")
    };
    Verdict::of(
        AiRunStatus::Rejected,
        Some(status),
        message,
        classify(Some(status), &text),
    )
}

/// The one-line outcome of the answer read as `reading`.
fn verdict(
    spec: &AiRunSpec,
    url_host: &str,
    head: &Head,
    reading: &Reading,
    merged: &Result<Merged, String>,
    elapsed_s: f64,
) -> Verdict {
    let http = Some(head.status);
    if head.status >= 300 {
        return rejected(head, &reading.raw);
    }
    match &reading.ending {
        Ending::Stopped => {
            return Verdict::of(
                AiRunStatus::Stopped,
                http,
                format!("Stopped at {elapsed_s:.1} s"),
                FailureKind::Cancelled,
            );
        }
        Ending::TimedOut => {
            let seconds = spec.timeout.as_secs_f64();
            let message = if reading.streamed {
                format!("Timed out: no data for {seconds} s")
            } else {
                format!("Timed out after {seconds} s")
            };
            return Verdict::of(AiRunStatus::TimedOut, http, message, FailureKind::Timeout);
        }
        Ending::Broke(error) => {
            return Verdict::of(
                AiRunStatus::Unreachable,
                http,
                format!(
                    "Cannot reach {url_host}: {} while reading the answer",
                    unreachable_reason(error)
                ),
                FailureKind::Network,
            );
        }
        Ending::BadStream(message) => {
            return Verdict::of(
                AiRunStatus::InvalidResponse,
                http,
                format!(
                    "The event stream is not valid {}: {message}",
                    protocol_label(spec.protocol)
                ),
                FailureKind::ProtocolMismatch,
            );
        }
        Ending::Complete => {}
    }
    match merged {
        Err(message) => Verdict::of(
            AiRunStatus::InvalidResponse,
            http,
            format!(
                "The answer is not a valid {} response: {message}",
                protocol_label(spec.protocol)
            ),
            FailureKind::ProtocolMismatch,
        ),
        Ok(merged) => match &merged.stream_error {
            Some(message) => Verdict::of(
                AiRunStatus::Rejected,
                http,
                format!("Stream error: {message}"),
                classify(None, message),
            ),
            None => Verdict {
                status: AiRunStatus::Done,
                http,
                message: None,
                kind: None,
            },
        },
    }
}

fn usage_of(merged: &Merged, spec: &AiRunSpec, counted: bool) -> (AiUsageDto, bool) {
    if !counted {
        return (AiUsageDto::default(), false);
    }
    let reported = &merged.usage;
    let output_reported = reported.output_tokens.is_some();
    let produced = format!(
        "{}{}{}",
        merged.reasoning,
        merged.reply,
        merged
            .tool_calls
            .iter()
            .map(|call| call.arguments.as_str())
            .collect::<String>()
    );
    let input = reported
        .input_tokens
        .unwrap_or_else(|| estimate_tokens(&spec.body));
    let output = reported
        .output_tokens
        .unwrap_or_else(|| estimate_tokens(&produced));
    let estimated = reported.input_tokens.is_none() || !output_reported;
    (
        AiUsageDto {
            input_tokens: Some(input),
            output_tokens: Some(output),
            cached_tokens: reported.cached_input_tokens,
            cache_write_tokens: reported.cache_write_tokens,
            reasoning_tokens: reported.reasoning_tokens,
            total_tokens: Some(input.saturating_add(output)),
            estimated,
        },
        output_reported,
    )
}

/// Builds the result of the call.
pub(super) fn assemble(
    spec: &AiRunSpec,
    url: &str,
    sent: AiSentRequest,
    hidden: &[String],
    stage: Stage,
    clock: &Clock,
) -> Assembled {
    let host = host_of(url);
    let queue_ms = ceil_ms(
        clock
            .sent_at
            .unwrap_or(clock.ended)
            .saturating_duration_since(clock.t0),
    );
    let elapsed_s = clock
        .ended
        .saturating_duration_since(clock.t0)
        .as_secs_f64();
    let mut dto = AiResultDto {
        run_id: spec.run_id.clone(),
        status: AiRunStatus::Done,
        http_status: None,
        message: None,
        notice: None,
        stream_error: None,
        model: spec.model.clone(),
        response_model: None,
        protocol: spec.protocol,
        streamed: false,
        reply: String::new(),
        reasoning: String::new(),
        tool_calls: Vec::new(),
        vectors: Vec::new(),
        finish_reason: None,
        usage: AiUsageDto::default(),
        ttft_ms: None,
        total_ms: queue_ms,
        stream_ms: None,
        tokens_per_second: None,
        timing: TimingJson {
            queue_ms,
            attempts: Vec::new(),
        },
        cost: None,
        price: None,
        cost_note: None,
        cost_note_code: None,
        dropped_headers: spec.dropped_headers.clone(),
        response_headers: Vec::new(),
        request: sent,
        frames: CapturedFrames::default(),
        response_body: None,
        event_count: 0,
        kept_in_log: false,
        log_row_id: None,
        log_request_id: None,
        assertion_results: Vec::new(),
        script_tests: Vec::new(),
        console: Vec::new(),
        script_error: None,
        history_id: None,
        history_saved: false,
    };
    let mut raw_body = String::new();
    let failure;
    let mut resp_bytes = 0;
    let mut counted_usage = None;
    match stage {
        Stage::Refused { status, message } => {
            dto.status = status;
            dto.message = Some(message);
            failure = Some(FailureKind::ProtocolMismatch);
        }
        Stage::NotAnswered(failed) => {
            let end_ms = ceil_ms(
                clock
                    .ended
                    .saturating_duration_since(clock.sent_at.unwrap_or(clock.ended)),
            );
            dto.status = failed.status;
            dto.message = Some(failed.message);
            failure = Some(failed.kind);
            dto.total_ms = queue_ms + end_ms;
            dto.timing.attempts = vec![failed.conn.timing(None, PhaseValue::NotNeeded, end_ms)];
        }
        Stage::Answered { head, reading } => {
            let reading = *reading;
            dto.streamed = reading.streamed;
            resp_bytes = reading.raw.len() as u64;
            raw_body = text_of(&reading.raw);
            dto.response_headers = ordered_headers(&head.headers);
            dto.event_count = reading.frames.len() as u64;
            let ok = (200..300).contains(&head.status);
            let merged = if !ok {
                Err(String::new())
            } else if reading.streamed {
                Ok(from_stream(&reading))
            } else if reading.ending == Ending::Complete {
                from_body(spec.protocol, &reading.raw)
            } else {
                Ok(Merged::default())
            };
            let said = verdict(spec, &host, &head, &reading, &merged, elapsed_s);
            dto.status = said.status;
            dto.http_status = said.http;
            dto.message = said.message;
            failure = said.kind;
            if ok && let Ok(merged) = &merged {
                dto.reply.clone_from(&merged.reply);
                dto.reasoning.clone_from(&merged.reasoning);
                dto.tool_calls.clone_from(&merged.tool_calls);
                dto.vectors.clone_from(&merged.vectors);
                dto.finish_reason.clone_from(&merged.finish);
                dto.response_model.clone_from(&merged.model);
                dto.stream_error.clone_from(&merged.stream_error);
                let has_content = !merged.reply.is_empty()
                    || !merged.reasoning.is_empty()
                    || !merged.tool_calls.is_empty()
                    || !merged.vectors.is_empty();
                if dto.status == AiRunStatus::Done && !has_content {
                    let why = merged.finish.as_deref().map_or(String::new(), |reason| {
                        format!(" (finish reason: {reason})")
                    });
                    dto.notice = Some(format!("The model returned no text{why}"));
                }
                let counted = matches!(
                    dto.status,
                    AiRunStatus::Done | AiRunStatus::Stopped | AiRunStatus::TimedOut
                ) && (has_content || merged.usage.output_tokens.is_some());
                let (usage, output_reported) = usage_of(merged, spec, counted);
                dto.usage = usage;
                counted_usage = counted.then_some(output_reported);
            }
            // Timing of the single attempt.
            let sent_at = clock.sent_at.unwrap_or(clock.t0);
            let first = reading
                .first_content
                .map(|at| ceil_ms(at.saturating_duration_since(sent_at)));
            let first_token = if reading.streamed {
                first.map_or(PhaseValue::NotMeasured, |at| {
                    PhaseValue::Ms(at.saturating_sub(head.headers_ms))
                })
            } else {
                PhaseValue::NotNeeded
            };
            let attempt_end = ceil_ms(clock.ended.saturating_duration_since(sent_at));
            let end_ms = attempt_end.max(head.headers_ms + first_token.ms().unwrap_or(0));
            dto.timing.attempts =
                vec![head.conn.timing(Some(head.headers_ms), first_token, end_ms)];
            dto.total_ms = queue_ms + end_ms;
            if reading.streamed {
                dto.ttft_ms = reading
                    .first_content
                    .map(|at| ceil_ms(at.saturating_duration_since(clock.t0)));
                dto.stream_ms = reading
                    .first_content
                    .map(|at| ceil_ms(reading.last_byte.saturating_duration_since(at)));
            }
            if let (Some(true), Some(output), Some(stream_ms)) =
                (counted_usage, dto.usage.output_tokens, dto.stream_ms)
                && output >= TPS_MIN_TOKENS
                && stream_ms > 0
            {
                dto.tokens_per_second = Some(output as f64 / (stream_ms as f64 / 1000.0));
            }
            if reading.streamed {
                let total = reading.frames.len();
                let mut frames = reading.frames;
                frames.truncate(RESULT_FRAMES);
                dto.frames = CapturedFrames {
                    truncated: total > frames.len(),
                    total_frames: total as u64,
                    frames,
                };
            } else {
                let body = bound_body(&reading.raw);
                dto.response_body = Some(CapturedBody::from(body));
            }
        }
    }
    // Cost: the effective price over the counted usage.
    let usage = counted_usage.map(|_| CostUsage {
        input_tokens: dto.usage.input_tokens.unwrap_or(0),
        output_tokens: dto.usage.output_tokens.unwrap_or(0),
        cached_tokens: dto.usage.cached_tokens.unwrap_or(0),
        cache_write_tokens: dto.usage.cache_write_tokens.unwrap_or(0),
        reasoning_tokens: dto.usage.reasoning_tokens.unwrap_or(0),
        tokens_estimated: dto.usage.estimated,
        ..CostUsage::default()
    });
    match (&usage, &spec.price) {
        (Some(usage), PriceResolution::Known(price)) => {
            let booked = cost(usage, price);
            if booked.estimated {
                dto.cost_note_code = Some(CostNoteCode::CatalogEstimate);
            }
            dto.cost = Some(booked);
            dto.price = Some(price.clone());
        }
        (Some(_), PriceResolution::Free) => dto.cost_note_code = Some(CostNoteCode::LocalFree),
        (Some(_), PriceResolution::Unknown(_)) => {
            dto.cost_note = Some("No price for this model".to_owned());
            dto.cost_note_code = Some(CostNoteCode::NoPrice);
        }
        (None, PriceResolution::Known(_)) if failure.is_none() => {
            dto.cost_note_code = Some(CostNoteCode::NoUsage);
        }
        (None, _) => {}
    }
    hide(&mut dto, &mut raw_body, hidden);
    Assembled {
        dto,
        raw_body,
        failure,
        usage,
        resp_bytes,
    }
}

/// Replaces the provider's own credential texts wherever the result shows what the upstream said
/// (reply, messages, frames, bodies, headers): the user's own response is not redacted, but the
/// credential the core injected never appears in it, even when the upstream echoes it.
fn hide(dto: &mut AiResultDto, raw_body: &mut String, hidden: &[String]) {
    if hidden.is_empty() {
        return;
    }
    let mask = |text: &mut String| {
        if !text.is_empty() {
            *text = mask_secrets(text, hidden);
        }
    };
    for text in [&mut dto.reply, &mut dto.reasoning, raw_body] {
        mask(text);
    }
    for text in [&mut dto.message, &mut dto.notice, &mut dto.stream_error]
        .into_iter()
        .flatten()
    {
        mask(text);
    }
    for call in &mut dto.tool_calls {
        mask(&mut call.arguments);
    }
    for header in &mut dto.response_headers {
        mask(&mut header.value);
    }
    for frame in &mut dto.frames.frames {
        mask(&mut frame.data);
    }
    if let Some(text) = dto
        .response_body
        .as_mut()
        .and_then(|body| body.text.as_mut())
    {
        mask(text);
    }
}
