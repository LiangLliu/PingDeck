//! The Anthropic Messages codec: hub format <-> `/v1/messages`, on both sides of the gateway.
//!
//! `encode`/`decode` handle whole requests and responses (block builders and readers live in
//! `encode_blocks`/`decode_blocks`, the generation-dependent thinking shape in `thinking`);
//! `stream_decode`/`stream_encode` handle the SSE streams.

mod decode;
mod decode_blocks;
mod encode;
mod encode_blocks;
mod stream_decode;
mod stream_encode;
#[cfg(test)]
mod tests;
mod thinking;

use serde_json::{Value, json};

use crate::llm::adapters::{ChatCodec, EncodeCtx, InboundHint, StreamDecoder, StreamEncoder};
use crate::llm::hub::{HubFinish, HubRequest, HubResponse, HubUsage};
use crate::llm::wire::Protocol;
use spanbay_base::error::AppError;

/// `max_tokens` when the request sets none; Anthropic requires the field.
const DEFAULT_MAX_TOKENS: u32 = 8192;
/// Appended to the system prompt for a JSON-object response format, which Anthropic lacks.
const JSON_OBJECT_NOTE: &str = "Respond with a single valid JSON object only.";
/// The protocol this codec speaks; signatures it produced need no `pdk1.` envelope.
const OWN: Protocol = Protocol::AnthropicMessages;
/// The web search tool type; the suffix is Anthropic's dated tool version.
const WEB_SEARCH_TOOL: &str = "web_search_20250305";

/// The Anthropic Messages codec (`/v1/messages`).
pub(crate) struct AnthropicMessages;

impl ChatCodec for AnthropicMessages {
    fn protocol(&self) -> Protocol {
        OWN
    }

    fn encode_request(&self, req: &HubRequest, ctx: &EncodeCtx<'_>) -> Result<Value, AppError> {
        encode::request(req, ctx)
    }

    fn decode_response(&self, body: &Value) -> Result<HubResponse, AppError> {
        decode::response(body)
    }

    fn stream_decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(stream_decode::Decoder::default())
    }

    fn decode_request(&self, body: &Value, hint: &InboundHint) -> Result<HubRequest, AppError> {
        decode::request(body, hint)
    }

    fn encode_response(&self, response: &HubResponse) -> Value {
        encode::response(response)
    }

    fn stream_encoder(&self) -> Box<dyn StreamEncoder> {
        Box::new(stream_encode::Encoder::default())
    }

    /// Anthropic error bodies carry no status, so `_status` is ignored.
    fn encode_error(&self, _status: u16, message: &str, kind: &str) -> Value {
        json!({"type": "error", "error": {"type": kind, "message": message}})
    }
}

/// The message of an Anthropic error body, or the whole body when it has none.
fn error_text(body: &Value) -> String {
    body.pointer("/error/message")
        .and_then(Value::as_str)
        .map_or_else(|| body.to_string(), str::to_owned)
}

fn decode_stop_reason(reason: Option<&str>) -> HubFinish {
    match reason {
        None | Some("end_turn" | "stop_sequence") => HubFinish::Stop,
        Some("max_tokens" | "model_context_window_exceeded") => HubFinish::Length,
        Some("pause_turn") => HubFinish::Pause,
        Some("tool_use") => HubFinish::ToolCalls,
        Some("refusal") => HubFinish::ContentFilter,
        Some(other) => HubFinish::Other(other.to_owned()),
    }
}

fn encode_stop_reason(finish: &HubFinish) -> &str {
    match finish {
        HubFinish::Stop => "end_turn",
        HubFinish::Length => "max_tokens",
        HubFinish::Pause => "pause_turn",
        HubFinish::ToolCalls => "tool_use",
        HubFinish::ContentFilter => "refusal",
        HubFinish::Other(other) => other,
    }
}

/// Reads an Anthropic `usage` object into hub usage.
///
/// Anthropic's `input_tokens` excludes cache reads and writes, while the hub's is the total, so
/// the three are added. In a stream, `base` holds what earlier events reported (`message_start`):
/// a `message_delta` that carries only cache fields then rebuilds the raw input from `base`, and
/// every field it omits keeps the `base` value. `ephemeral_1h_input_tokens == 0` counts as absent.
fn read_usage(usage: &Value, base: &HubUsage) -> HubUsage {
    let get = |key: &str| usage.get(key).and_then(Value::as_u64);
    let (input, read, write) = (
        get("input_tokens"),
        get("cache_read_input_tokens"),
        get("cache_creation_input_tokens"),
    );
    let input_tokens = if input.is_some() || read.is_some() || write.is_some() {
        let read_total = read.or(base.cached_input_tokens);
        let write_total = write.or(base.cache_write_tokens);
        Some(
            input
                .unwrap_or_else(|| uncached_input_tokens(base))
                .saturating_add(read_total.unwrap_or(0))
                .saturating_add(write_total.unwrap_or(0)),
        )
    } else {
        None
    };
    HubUsage {
        input_tokens,
        output_tokens: get("output_tokens"),
        cached_input_tokens: read.or(base.cached_input_tokens),
        cache_write_tokens: write.or(base.cache_write_tokens),
        cache_write_long_tokens: usage
            .pointer("/cache_creation/ephemeral_1h_input_tokens")
            .and_then(Value::as_u64)
            .filter(|tokens| *tokens > 0)
            .or(base.cache_write_long_tokens),
        reasoning_tokens: get("thinking_tokens").or(base.reasoning_tokens),
    }
}

/// The hub's total input minus cache reads and writes: Anthropic's `input_tokens`.
fn uncached_input_tokens(usage: &HubUsage) -> u64 {
    usage
        .input_tokens
        .unwrap_or(0)
        .saturating_sub(usage.cached_input_tokens.unwrap_or(0))
        .saturating_sub(usage.cache_write_tokens.unwrap_or(0))
}

/// The `usage` object of a response body; the inverse of [`read_usage`].
fn usage_json(usage: &HubUsage) -> Value {
    json!({
        "input_tokens": uncached_input_tokens(usage),
        "output_tokens": usage.output_tokens.unwrap_or(0),
        "cache_read_input_tokens": usage.cached_input_tokens.unwrap_or(0),
        "cache_creation_input_tokens": usage.cache_write_tokens.unwrap_or(0),
    })
}

fn text_block(text: &str) -> Value {
    json!({"type": "text", "text": text})
}
