//! The Gemini native codec: `generateContent` and `streamGenerateContent?alt=sse`, both sides.
//!
//! `encode` and `decode` hold the request and response bodies, `config` the
//! `generationConfig`/`tools`/`toolConfig` mappings, `schema` the OpenAPI schema conversion,
//! `function_response` the tool results, `grounding` the search metadata and `stream` both
//! stream directions. This module keeps the primitives they share: snake_case-tolerant field
//! lookup, part builders, signatures, and the finish and usage mappings.

mod config;
mod decode;
mod encode;
mod function_response;
mod grounding;
mod schema;
mod stream;
#[cfg(test)]
mod tests;

use serde_json::{Map, Value, json};

use crate::llm::adapters::{
    ChatCodec, EncodeCtx, InboundHint, StreamDecoder, StreamEncoder, insert_some, object,
    parse_args_object,
};
use crate::llm::hub::{
    HubFinish, HubRequest, HubResponse, HubUsage, unwrap_signature, wrap_signature,
};
use crate::llm::wire::Protocol;
use pingdeck_base::error::AppError;

/// Placeholder signature for the first `functionCall` of a model turn.
///
/// Gemini requires every model turn's first function call to carry a `thoughtSignature`; this
/// documented sentinel makes it skip the check for history it never signed (unsigned calls or
/// calls from another protocol).
const DUMMY_THOUGHT_SIGNATURE: &str = "context_engineering_is_the_way_to_go";

/// Longest snake_case key [`field`] spells on the stack; longer keys fall back to a `String`.
const SNAKE_KEY_BUFFER: usize = 64;

/// The Gemini `generateContent` protocol, toward upstreams and toward Gemini clients.
pub(crate) struct GeminiGenerate;

impl ChatCodec for GeminiGenerate {
    fn protocol(&self) -> Protocol {
        Protocol::Gemini
    }

    fn encode_request(&self, req: &HubRequest, ctx: &EncodeCtx<'_>) -> Result<Value, AppError> {
        encode::request(req, ctx)
    }

    fn decode_response(&self, body: &Value) -> Result<HubResponse, AppError> {
        decode::response(body)
    }

    fn stream_decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(stream::GeminiStreamDecoder::default())
    }

    fn decode_request(&self, body: &Value, hint: &InboundHint) -> Result<HubRequest, AppError> {
        decode::request(body, hint)
    }

    fn encode_response(&self, response: &HubResponse) -> Value {
        encode::response(response)
    }

    fn stream_encoder(&self) -> Box<dyn StreamEncoder> {
        Box::new(stream::GeminiStreamEncoder::default())
    }

    fn encode_error(&self, status: u16, message: &str, _kind: &str) -> Value {
        let status_text = match status {
            400 => "INVALID_ARGUMENT",
            401 => "UNAUTHENTICATED",
            403 => "PERMISSION_DENIED",
            404 => "NOT_FOUND",
            429 => "RESOURCE_EXHAUSTED",
            503 => "UNAVAILABLE",
            500.. => "INTERNAL",
            _ => "UNKNOWN",
        };
        json!({"error": {"code": status, "message": message, "status": status_text}})
    }
}

/// `value[camel]`, or else its snake_case spelling (`usageMetadata` / `usage_metadata`).
///
/// Gemini accepts the proto-JSON snake_case aliases of every field, so both spellings are read;
/// the camelCase one wins when both are present.
fn field<'a>(value: &'a Value, camel: &str) -> Option<&'a Value> {
    let object = value.as_object()?;
    if let Some(found) = object.get(camel) {
        return Some(found);
    }
    if !camel.bytes().any(|byte| byte.is_ascii_uppercase()) {
        // The snake_case spelling is `camel` itself, already looked up.
        return None;
    }
    let mut buffer = [0; SNAKE_KEY_BUFFER];
    match write_snake_case(camel, &mut buffer) {
        Some(snake) => object.get(snake),
        None => object.get(snake_case(camel).as_str()),
    }
}

/// Writes the snake_case spelling of `camel` into `buffer`; `None` when it does not fit.
fn write_snake_case<'b>(camel: &str, buffer: &'b mut [u8]) -> Option<&'b str> {
    let mut len = 0;
    for byte in camel.bytes() {
        if byte.is_ascii_uppercase() {
            *buffer.get_mut(len)? = b'_';
            len += 1;
        }
        *buffer.get_mut(len)? = byte.to_ascii_lowercase();
        len += 1;
    }
    // Only ASCII bytes were changed, so the result is valid UTF-8.
    std::str::from_utf8(buffer.get(..len)?).ok()
}

/// The snake_case spelling of `camel`: `_` before each ASCII capital, which is lowercased.
fn snake_case(camel: &str) -> String {
    let mut snake = String::with_capacity(camel.len() + 4);
    for c in camel.chars() {
        if c.is_ascii_uppercase() {
            snake.push('_');
        }
        snake.push(c.to_ascii_lowercase());
    }
    snake
}

/// The string at [`field`]`(value, camel)` as an owned value; empty when missing.
fn str_field(value: &Value, camel: &str) -> String {
    field(value, camel)
        .and_then(Value::as_str)
        .unwrap_or_default()
        .to_owned()
}

/// The `message` of a Gemini error object, or the whole object as JSON text.
fn error_message(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .map_or_else(|| error.to_string(), str::to_owned)
}

/// The first candidate of a response or stream chunk; the hub carries one choice.
fn first_candidate(body: &Value) -> Option<&Value> {
    body.get("candidates")
        .and_then(|candidates| candidates.get(0))
}

/// The `content.parts` of a candidate; empty when any level is missing.
fn candidate_parts(candidate: Option<&Value>) -> &[Value] {
    candidate
        .and_then(|candidate| field(candidate, "content"))
        .and_then(|content| content.get("parts"))
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice)
}

/// Whether a response without candidates was blocked by `promptFeedback.blockReason`.
fn blocked(body: &Value) -> bool {
    field(body, "promptFeedback")
        .and_then(|feedback| field(feedback, "blockReason"))
        .is_some_and(Value::is_string)
}

/// A `text` part, signed when `signature` is given.
fn text_part(text: &str, signature: Option<&str>) -> Value {
    let mut part = Map::new();
    part.insert("text".into(), text.into());
    insert_some(&mut part, "thoughtSignature", signature);
    Value::Object(part)
}

/// A thought (`"thought": true`) text part, signed when `signature` is given.
fn thought_part(text: &str, signature: Option<&str>) -> Value {
    let mut part = Map::new();
    part.insert("text".into(), text.into());
    part.insert("thought".into(), true.into());
    insert_some(&mut part, "thoughtSignature", signature);
    Value::Object(part)
}

/// A `functionCall` part; an empty `id` is left out and the signature sits beside the call.
fn function_call_part(id: &str, name: &str, arguments: &str, signature: Option<&str>) -> Value {
    let mut call = Map::new();
    call.insert("name".into(), name.into());
    call.insert("args".into(), parse_args_object(arguments));
    if !id.is_empty() {
        call.insert("id".into(), id.into());
    }
    let mut part = Map::new();
    part.insert("functionCall".into(), Value::Object(call));
    insert_some(&mut part, "thoughtSignature", signature);
    Value::Object(part)
}

/// An `inlineData` part of base64 bytes.
fn inline_data_part(media_type: &str, data: &str) -> Value {
    json!({"inlineData": {"mimeType": media_type, "data": data}})
}

/// The Gemini `finishReason` of a hub finish.
fn encode_finish(finish: &HubFinish) -> &str {
    match finish {
        HubFinish::Stop | HubFinish::ToolCalls => "STOP",
        HubFinish::Length | HubFinish::Pause => "MAX_TOKENS",
        HubFinish::ContentFilter => "SAFETY",
        HubFinish::Other(reason) => reason,
    }
}

/// The hub finish of a Gemini `finishReason`; Gemini says `STOP` after function calls too, so
/// `has_calls` turns it into [`HubFinish::ToolCalls`].
fn decode_finish(reason: Option<&str>, has_calls: bool) -> HubFinish {
    match reason {
        None | Some("STOP") if has_calls => HubFinish::ToolCalls,
        None | Some("STOP") => HubFinish::Stop,
        Some("MAX_TOKENS") => HubFinish::Length,
        Some("CONTINUATION") => HubFinish::Pause,
        Some(
            "SAFETY"
            | "RECITATION"
            | "BLOCKLIST"
            | "PROHIBITED_CONTENT"
            | "SPII"
            | "IMAGE_SAFETY"
            | "IMAGE_PROHIBITED_CONTENT"
            | "IMAGE_RECITATION",
        ) => HubFinish::ContentFilter,
        Some(other) => HubFinish::Other(other.to_owned()),
    }
}

/// Hub usage of a `usageMetadata` object.
///
/// Gemini's `candidatesTokenCount` excludes `thoughtsTokenCount`, while hub output includes
/// reasoning, so output is their sum.
fn decode_usage(meta: &Value) -> HubUsage {
    let count = |key: &str| field(meta, key).and_then(Value::as_u64);
    let (candidates, thoughts) = (count("candidatesTokenCount"), count("thoughtsTokenCount"));
    HubUsage {
        input_tokens: count("promptTokenCount"),
        output_tokens: (candidates.is_some() || thoughts.is_some()).then(|| {
            candidates
                .unwrap_or(0)
                .saturating_add(thoughts.unwrap_or(0))
        }),
        cached_input_tokens: count("cachedContentTokenCount"),
        cache_write_tokens: None,
        cache_write_long_tokens: None,
        reasoning_tokens: thoughts,
    }
}

/// The `usageMetadata` of hub usage (the inverse of [`decode_usage`]); `None` when empty.
fn encode_usage(usage: &HubUsage) -> Option<Value> {
    let mut meta = Map::new();
    insert_some(&mut meta, "promptTokenCount", usage.input_tokens);
    insert_some(
        &mut meta,
        "candidatesTokenCount",
        usage
            .output_tokens
            .map(|output| output.saturating_sub(usage.reasoning_tokens.unwrap_or(0))),
    );
    insert_some(&mut meta, "thoughtsTokenCount", usage.reasoning_tokens);
    insert_some(
        &mut meta,
        "cachedContentTokenCount",
        usage.cached_input_tokens,
    );
    insert_some(&mut meta, "totalTokenCount", usage.total_tokens());
    (!meta.is_empty()).then_some(Value::Object(meta))
}

/// A part's `thoughtSignature` and the protocol that made it.
///
/// An upstream's signature is Gemini's own; a client's may be another protocol's wrapped
/// signature (see [`unwrap_signature`]).
fn read_signature(part: &Value, from_client: bool) -> (Option<String>, Option<Protocol>) {
    match field(part, "thoughtSignature").and_then(Value::as_str) {
        None => (None, None),
        Some(raw) if from_client => {
            let (origin, signature) = unwrap_signature(raw, Protocol::Gemini);
            (Some(signature), Some(origin))
        }
        Some(raw) => (Some(raw.to_owned()), Some(Protocol::Gemini)),
    }
}

/// A signature as sent to a Gemini client: wrapped with its origin unless it is Gemini's own.
fn client_signature(signature: Option<&str>, origin: Option<Protocol>) -> Option<String> {
    signature.map(|signature| wrap_signature(origin, signature, Protocol::Gemini))
}

/// `{"candidates": [candidate], ...}` with usage, model and id when known; shared by the
/// response and the stream chunks.
fn candidates_body(candidate: Value, usage: Option<Value>, model: &str, id: &str) -> Value {
    let mut body = Map::new();
    body.insert("candidates".into(), Value::Array(vec![candidate]));
    insert_some(&mut body, "usageMetadata", usage);
    if !model.is_empty() {
        body.insert("modelVersion".into(), model.into());
    }
    if !id.is_empty() {
        body.insert("responseId".into(), id.into());
    }
    Value::Object(body)
}

/// A model candidate at index 0: `content` of `parts`, then the finish and grounding when set.
fn candidate(parts: Vec<Value>, finish: Option<&str>, grounding: Option<Value>) -> Value {
    let mut candidate = Map::new();
    candidate.insert(
        "content".into(),
        object([("role", "model".into()), ("parts", Value::Array(parts))]),
    );
    insert_some(&mut candidate, "finishReason", finish);
    candidate.insert("index".into(), 0.into());
    insert_some(&mut candidate, "groundingMetadata", grounding);
    Value::Object(candidate)
}
