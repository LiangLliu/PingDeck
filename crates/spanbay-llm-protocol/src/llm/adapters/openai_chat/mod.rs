//! OpenAI Chat Completions codec: hub format <-> `/chat/completions` JSON and SSE, both sides.
//!
//! `encode` and `decode` hold the request and response bodies, `reasoning` the provider
//! reasoning dialects and `stream` both stream directions. This module keeps what they share:
//! error, reasoning, tool call, finish, usage and citation mappings.
//!
//! Hub citation indices are UTF-16 offsets into their own text part; Chat annotations are
//! offsets into the whole message content, so encoders add the UTF-16 length of the text before
//! the part (see `adapters::utf16_len`).

use serde_json::{Map, Value, json};

use crate::llm::adapters::{
    ChatCodec, EncodeCtx, InboundHint, StreamDecoder, StreamEncoder, arguments_text, insert_some,
    str_or_empty, to_u32, utf16_len,
};
use crate::llm::hub::{HubCitation, HubFinish, HubPart, HubRequest, HubResponse, HubUsage};
use crate::llm::wire::Protocol;
use spanbay_base::error::AppError;

mod decode;
mod encode;
mod reasoning;
mod stream;
#[cfg(test)]
mod tests;

use stream::{ChatStreamDecoder, ChatStreamEncoder};

/// The fields providers put reasoning text in, in lookup order: the first field holding a
/// string wins, even an empty one.
const REASONING_FIELDS: [&str; 3] = ["reasoning_content", "reasoning", "thinking"];
/// The response id sent when the hub has none.
const DEFAULT_ID: &str = "chatcmpl-spanbay";

/// The OpenAI Chat Completions protocol, toward upstreams and toward Chat clients.
pub(crate) struct OpenaiChat;

impl ChatCodec for OpenaiChat {
    fn protocol(&self) -> Protocol {
        Protocol::OpenaiChat
    }

    fn encode_request(&self, req: &HubRequest, ctx: &EncodeCtx<'_>) -> Result<Value, AppError> {
        encode::request(req, ctx)
    }

    fn decode_response(&self, body: &Value) -> Result<HubResponse, AppError> {
        decode::response(body)
    }

    fn stream_decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(ChatStreamDecoder::default())
    }

    fn decode_request(&self, body: &Value, hint: &InboundHint) -> Result<HubRequest, AppError> {
        decode::request(body, hint)
    }

    fn encode_response(&self, response: &HubResponse) -> Value {
        encode::response(response)
    }

    fn stream_encoder(&self) -> Box<dyn StreamEncoder> {
        Box::new(ChatStreamEncoder::new())
    }

    fn encode_error(&self, _status: u16, message: &str, kind: &str) -> Value {
        json!({"error": {"message": message, "type": kind, "param": null, "code": null}})
    }
}

/// The message of an error object (or of a bare error string), else the whole value as JSON.
fn error_message(error: &Value) -> String {
    error
        .get("message")
        .and_then(Value::as_str)
        .or_else(|| error.as_str())
        .map_or_else(|| error.to_string(), str::to_owned)
}

/// The error kind: a non-empty `type`, else `code` (a string or a number).
fn error_kind(error: &Value) -> Option<String> {
    ["type", "code"]
        .iter()
        .find_map(|key| match error.get(*key) {
            Some(Value::String(kind)) if !kind.is_empty() => Some(kind.clone()),
            Some(Value::Number(number)) => Some(number.to_string()),
            _ => None,
        })
}

/// The reasoning text of a message (see [`REASONING_FIELDS`]); `None` when empty.
fn reasoning_text(message: &Value) -> Option<&str> {
    REASONING_FIELDS
        .iter()
        .find_map(|field| message.get(*field).and_then(Value::as_str))
        .filter(|text| !text.is_empty())
}

/// An unsigned reasoning part: Chat Completions reasoning carries no signature.
fn reasoning_part(text: &str) -> HubPart {
    HubPart::Reasoning {
        text: text.to_owned(),
        signature: None,
        encrypted: None,
        id: None,
        origin: None,
    }
}

/// The text of message content: a string, or the `text` of every part concatenated.
fn content_text(content: &Value) -> String {
    match content {
        Value::String(text) => text.clone(),
        Value::Array(parts) => parts
            .iter()
            .filter_map(|part| part.get("text").and_then(Value::as_str))
            .collect(),
        _ => String::new(),
    }
}

/// The tool calls of a message: `tool_calls`, or the legacy single `function_call`. A call
/// without an id is named `call_<position>`.
fn tool_call_parts(message: &Value) -> Vec<HubPart> {
    let call = |position: usize, id: Option<&str>, function: &Value| HubPart::ToolCall {
        id: id
            .filter(|id| !id.is_empty())
            .map_or_else(|| format!("call_{position}"), str::to_owned),
        name: str_or_empty(function, "name"),
        arguments: arguments_text(function.get("arguments")),
        signature: None,
        origin: None,
    };
    if let Some(calls) = message.get("tool_calls").and_then(Value::as_array) {
        return calls
            .iter()
            .enumerate()
            .map(|(pos, entry)| call(pos, entry["id"].as_str(), &entry["function"]))
            .collect();
    }
    match message.get("function_call") {
        Some(function) if function.is_object() => vec![call(0, None, function)],
        _ => Vec::new(),
    }
}

/// The reasoning text of hub parts, concatenated in order.
fn reasoning_text_of(parts: &[HubPart]) -> String {
    parts
        .iter()
        .filter_map(|part| match part {
            HubPart::Reasoning { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

/// The `tool_calls` entries of hub parts.
fn tool_calls_json(parts: &[HubPart]) -> Vec<Value> {
    parts
        .iter()
        .filter_map(|part| match part {
            HubPart::ToolCall {
                id,
                name,
                arguments,
                ..
            } => Some(call_json(id, name, arguments)),
            _ => None,
        })
        .collect()
}

/// One `tool_calls` entry; blank arguments are sent as `"{}"`, which every server parses.
fn call_json(id: &str, name: &str, arguments: &str) -> Value {
    let arguments = if arguments.trim().is_empty() {
        "{}"
    } else {
        arguments
    };
    json!({"id": id, "type": "function", "function": {"name": name, "arguments": arguments}})
}

/// The hub finish of a `finish_reason`; a missing one is a normal stop.
fn decode_finish(reason: Option<&str>) -> HubFinish {
    match reason {
        None | Some("stop") => HubFinish::Stop,
        Some("length") => HubFinish::Length,
        Some("tool_calls" | "function_call") => HubFinish::ToolCalls,
        Some("content_filter") => HubFinish::ContentFilter,
        Some(other) => HubFinish::Other(other.to_owned()),
    }
}

/// The `finish_reason` of a hub finish.
fn encode_finish(finish: &HubFinish) -> &str {
    match finish {
        HubFinish::Stop => "stop",
        // A paused answer has no Chat Completions word; `length` tells the client to go on.
        HubFinish::Length | HubFinish::Pause => "length",
        HubFinish::ToolCalls => "tool_calls",
        HubFinish::ContentFilter => "content_filter",
        HubFinish::Other(other) => other,
    }
}

/// Hub usage of a `usage` object; DeepSeek reports cache hits as `prompt_cache_hit_tokens`.
fn decode_usage(usage: &Value) -> HubUsage {
    HubUsage {
        input_tokens: usage["prompt_tokens"].as_u64(),
        output_tokens: usage["completion_tokens"].as_u64(),
        cached_input_tokens: usage["prompt_tokens_details"]["cached_tokens"]
            .as_u64()
            .or_else(|| usage["prompt_cache_hit_tokens"].as_u64()),
        cache_write_tokens: usage["prompt_tokens_details"]["cache_write_tokens"].as_u64(),
        cache_write_long_tokens: None,
        reasoning_tokens: usage["completion_tokens_details"]["reasoning_tokens"].as_u64(),
    }
}

/// The `usage` object of hub usage; zeros when nothing is known, since clients expect one.
fn encode_usage(usage: &HubUsage) -> Value {
    if *usage == HubUsage::default() {
        return json!({"prompt_tokens": 0, "completion_tokens": 0, "total_tokens": 0});
    }
    let mut out = Map::new();
    insert_some(&mut out, "prompt_tokens", usage.input_tokens);
    insert_some(&mut out, "completion_tokens", usage.output_tokens);
    insert_some(&mut out, "total_tokens", usage.total_tokens());
    if let Some(cached) = usage.cached_input_tokens {
        out.insert(
            "prompt_tokens_details".into(),
            json!({"cached_tokens": cached}),
        );
    }
    if let Some(reasoning) = usage.reasoning_tokens {
        out.insert(
            "completion_tokens_details".into(),
            json!({"reasoning_tokens": reasoning}),
        );
    }
    Value::Object(out)
}

fn non_empty_or<'a>(value: &'a str, default: &'a str) -> &'a str {
    if value.is_empty() { default } else { value }
}

/// The `url_citation` annotations as hub citations; other annotation types are skipped.
fn url_citations(annotations: &Value) -> Vec<HubCitation> {
    annotations
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(url_citation)
        .collect()
}

fn url_citation(annotation: &Value) -> Option<HubCitation> {
    if annotation["type"].as_str() != Some("url_citation") {
        return None;
    }
    let inner = &annotation["url_citation"];
    Some(HubCitation {
        url: inner["url"].as_str()?.to_owned(),
        title: str_or_empty(inner, "title"),
        snippet: None,
        start_index: inner["start_index"].as_u64().map(to_u32),
        end_index: inner["end_index"].as_u64().map(to_u32),
    })
}

/// A `url_citation` annotation whose indices are shifted by `offset` (the UTF-16 length of the
/// content before the citation's part); a missing index counts as 0.
fn annotation_json(citation: &HubCitation, offset: u32) -> Value {
    let at = |index: Option<u32>| index.unwrap_or(0).saturating_add(offset);
    json!({
        "type": "url_citation",
        "url_citation": {
            "start_index": at(citation.start_index),
            "end_index": at(citation.end_index),
            "url": citation.url,
            "title": citation.title,
        },
    })
}

/// The annotations of a response: each text part's citations shifted to message offsets.
fn response_annotations(content: &[HubPart]) -> Vec<Value> {
    let mut offset = 0;
    let mut out = Vec::new();
    for part in content {
        if let HubPart::Text {
            text, citations, ..
        } = part
        {
            out.extend(citations.iter().map(|c| annotation_json(c, offset)));
            offset = offset.saturating_add(utf16_len(text));
        }
    }
    out
}
