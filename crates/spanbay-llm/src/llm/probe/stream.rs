//! The stream check: time to first token and throughput in one streamed call.
//!
//! Single responsibility: [`test_stream`], the only check that reads an event stream.

use std::time::{Duration, Instant};

use tokio::time::timeout_at;

use serde_json::json;

use super::{
    Ctx, Outcome, elapsed_ms, fail, now_ms, pass, preview, set_tokens, user_request,
    wait_before_retry,
};
use crate::llm::client::encode_for;
use crate::llm::failure::{FailureKind, classify};
use crate::llm::hub::{HubEvent, HubRequest, HubUsage};
use crate::llm::store::ProbeResult;
use crate::llm::upstream::{AttemptError, Call, UpstreamStream, send_measured};

/// The stream check's prompt: it asks for a long answer so most models generate at least
/// [`MIN_TPS_TOKENS`] tokens.
pub(super) const STREAM_PROMPT: &str = "Write a detailed explanation of how a lighthouse works, \
covering the lamp, the lens, the rotation mechanism, the light characteristic that identifies \
each lighthouse, the keeper's duties and the history of automation. Use plain paragraphs, no \
lists, and write at least 350 words.";

/// Output token cap of the stream check (the chat check keeps 256).
pub(super) const STREAM_MAX_TOKENS: u32 = 512;

/// Fewest provider-reported output tokens for which a speed is shown.
pub(crate) const MIN_TPS_TOKENS: u64 = 300;

/// Output tokens per whole second between the first content event and the end of the stream;
/// `None` below [`MIN_TPS_TOKENS`] or when the provider reported no usage.
pub(crate) fn tokens_per_second(output_tokens: Option<u64>, window: Duration) -> Option<f64> {
    let tokens = output_tokens.filter(|tokens| *tokens >= MIN_TPS_TOKENS)?;
    if window.is_zero() {
        return None;
    }
    #[allow(clippy::cast_precision_loss)]
    Some((tokens as f64 / window.as_secs_f64()).round())
}

/// The Stream check's request: the long-answer prompt, 512 output tokens.
pub(super) fn stream_request(model: &str) -> HubRequest {
    user_request(model, STREAM_PROMPT, STREAM_MAX_TOKENS)
}

pub(super) async fn test_stream(ctx: &Ctx<'_>, result: &mut ProbeResult) -> Outcome {
    let protocol = ctx.chat_proto()?;
    result.endpoint = protocol.as_str().to_owned();
    let request = stream_request(&ctx.target.model);
    let body = encode_for(ctx.target, protocol, &request, true).map_err(AttemptError::Invalid)?;
    let timeouts = ctx.timeouts;
    let mut retried = false;
    let (response, started_ms, started, clock) = loop {
        let started_ms = now_ms();
        let started = Instant::now();
        let clock = tokio::time::Instant::now();
        let sent = send_measured(&Call {
            runtime: ctx.runtime,
            target: ctx.target,
            protocol,
            stream: true,
            url: None,
            body: &body,
            timeout: timeouts.stream_first,
            cancel: ctx.cancel,
        })
        .await
        .0;
        match sent {
            Ok(response) => break (response, started_ms, started, clock),
            Err(error) => {
                ctx.log.record(
                    ctx.target,
                    protocol.as_str(),
                    started_ms,
                    started,
                    None,
                    None,
                    Some(&error),
                    true,
                );
                if retried || !wait_before_retry(ctx, &error, timeouts.stream_first).await {
                    return Err(error);
                }
                retried = true;
            }
        }
    };
    let mut stream = UpstreamStream::new(
        response,
        protocol,
        timeouts.stream_idle,
        ctx.cancel.cloned(),
    )?;

    let mut first_content: Option<Duration> = None;
    let mut text = String::new();
    let mut reasoning_chars = 0usize;
    let mut usage = HubUsage::default();
    let mut stream_error: Option<String> = None;
    let mut transport_error: Option<AttemptError> = None;
    'read: loop {
        // First content must arrive within the setting; afterwards only the idle limit (inside
        // the stream) and the ceiling apply.
        let deadline = if first_content.is_none() {
            clock + timeouts.stream_first
        } else {
            clock + timeouts.stream_ceiling
        };
        let chunk = match timeout_at(deadline, stream.next_chunk()).await {
            Ok(Some(Ok(chunk))) => chunk,
            Ok(Some(Err(error))) => {
                transport_error = Some(error);
                break;
            }
            Ok(None) => break,
            Err(_) => {
                transport_error = Some(AttemptError::Timeout);
                break;
            }
        };
        for event in chunk.events {
            match event {
                HubEvent::TextDelta { text: delta } => {
                    first_content.get_or_insert_with(|| started.elapsed());
                    text.push_str(&delta);
                }
                HubEvent::ReasoningDelta { text: delta } => {
                    first_content.get_or_insert_with(|| started.elapsed());
                    reasoning_chars += delta.chars().count();
                }
                HubEvent::ToolCallStart { .. } => {
                    first_content.get_or_insert_with(|| started.elapsed());
                }
                HubEvent::Usage { usage: reported } => usage.merge(&reported),
                HubEvent::Error { message, .. } => {
                    stream_error = Some(message);
                    break 'read;
                }
                _ => {}
            }
        }
    }
    let total = started.elapsed();
    result.latency_ms = Some(elapsed_ms(started));
    result.ttft_ms = first_content.map(|ttft| u64::try_from(ttft.as_millis()).unwrap_or(u64::MAX));
    set_tokens(result, &usage);
    result.tps = first_content
        .and_then(|ttft| tokens_per_second(usage.output_tokens, total.saturating_sub(ttft)));
    result.detail = json!({
        "preview": preview(&text, 200),
        "reasoning_chars": reasoning_chars,
    });
    ctx.log.record_stream(
        ctx.target,
        protocol.as_str(),
        (started_ms, started),
        result.ttft_ms,
        &usage,
        transport_error.as_ref(),
    );
    if let Some(error) = transport_error {
        return Err(error);
    }
    if let Some(message) = stream_error {
        fail(result, classify(None, &message), message);
    } else if first_content.is_none() {
        fail(
            result,
            FailureKind::EmptyOutput,
            "the stream ended without content",
        );
    } else {
        let ttft = result.ttft_ms.unwrap_or(0);
        pass(
            result,
            format!(
                "first token after {ttft} ms, {} ms in total",
                elapsed_ms(started)
            ),
        );
    }
    Ok(())
}
