//! Stream and part fixtures shared by the codec test suites.

use serde_json::Value;

use crate::llm::adapters::{ChatCodec, EncodeCtx};
use crate::llm::hub::{HubEvent, HubPart, HubRequest};
use crate::llm::sse::SseParser;
use crate::llm::wire::Quirks;
use spanbay_base::error::AppError;

/// Encodes `req` toward an upstream with `quirks`.
pub(crate) fn try_encode(
    codec: &dyn ChatCodec,
    req: &HubRequest,
    quirks: &Quirks,
) -> Result<Value, AppError> {
    codec.encode_request(req, &EncodeCtx { quirks })
}

/// [`try_encode`] for requests that must encode.
pub(crate) fn encode_with(codec: &dyn ChatCodec, req: &HubRequest, quirks: &Quirks) -> Value {
    try_encode(codec, req, quirks).unwrap()
}

/// Parses raw SSE text and runs it through `codec`'s stream decoder, stopping at the first error.
pub(crate) fn try_decode_sse_text(
    codec: &dyn ChatCodec,
    raw: &str,
) -> Result<Vec<HubEvent>, AppError> {
    let mut parser = SseParser::new();
    let mut decoder = codec.stream_decoder();
    let mut events = Vec::new();
    for sse in parser
        .feed(raw.as_bytes())
        .into_iter()
        .chain(parser.finish())
    {
        events.extend(decoder.push(&sse)?);
    }
    events.extend(decoder.finish()?);
    Ok(events)
}

/// [`try_decode_sse_text`] for streams that must decode.
pub(crate) fn decode_sse_text(codec: &dyn ChatCodec, raw: &str) -> Vec<HubEvent> {
    try_decode_sse_text(codec, raw).unwrap()
}

/// Sorts object keys recursively so golden JSON comparisons stay independent of
/// `serde_json/preserve_order` feature unification across the workspace.
pub(crate) fn sort_json_keys(value: Value) -> Value {
    match value {
        Value::Object(map) => {
            let mut entries: Vec<(String, Value)> = map
                .into_iter()
                .map(|(k, v)| (k, sort_json_keys(v)))
                .collect();
            entries.sort_by(|a, b| a.0.cmp(&b.0));
            Value::Object(entries.into_iter().collect())
        }
        Value::Array(items) => Value::Array(items.into_iter().map(sort_json_keys).collect()),
        other => other,
    }
}

/// Canonicalizes a JSON string with sorted keys for order-independent golden comparison.
pub(crate) fn canonical_json_str(raw: &str) -> String {
    serde_json::from_str::<Value>(raw)
        .map(|v| sort_json_keys(v).to_string())
        .unwrap_or_else(|_| raw.to_owned())
}

/// Canonicalizes every `data: <json>` line of an SSE stream for order-independent golden checks.
pub(crate) fn canonicalize_sse(raw: &str) -> String {
    let mut out = String::with_capacity(raw.len());
    for chunk in raw.split_inclusive('\n') {
        if let Some(rest) = chunk.strip_prefix("data: ") {
            let (payload, nl) = rest.strip_suffix('\n').map_or((rest, ""), |p| (p, "\n"));
            if payload.starts_with('{') || payload.starts_with('[') {
                out.push_str("data: ");
                out.push_str(&canonical_json_str(payload));
                out.push_str(nl);
                continue;
            }
        }
        out.push_str(chunk);
    }
    out
}

/// Runs `events` through `codec`'s stream encoder and returns the SSE text; a second `finish`
/// must add nothing.
pub(crate) fn encode_stream(codec: &dyn ChatCodec, events: &[HubEvent]) -> String {
    let mut encoder = codec.stream_encoder();
    let mut bytes = Vec::new();
    for event in events {
        bytes.extend(encoder.push(event));
    }
    bytes.extend(encoder.finish());
    assert!(encoder.finish().is_empty());
    canonicalize_sse(&String::from_utf8(bytes).unwrap())
}

/// Encodes `events` as a stream and decodes the text again: `(text, decoded events)`.
pub(crate) fn reencode(codec: &dyn ChatCodec, events: &[HubEvent]) -> (String, Vec<HubEvent>) {
    let text = encode_stream(codec, events);
    let decoded = decode_sse_text(codec, &text);
    (text, decoded)
}

/// An unsigned tool call part.
pub(crate) fn tool_call(id: &str, name: &str, arguments: &str) -> HubPart {
    HubPart::ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: arguments.into(),
        signature: None,
        origin: None,
    }
}
