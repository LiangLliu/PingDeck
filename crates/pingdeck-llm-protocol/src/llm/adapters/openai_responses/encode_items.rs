//! Output items and the response envelope toward a client, shared by the whole-response encoder
//! and the stream encoder.

use serde_json::{Map, Value, json};

use super::OWN;
use crate::llm::adapters::{WEB_SEARCH_TOOL_NAME, insert_some, object, unix_now};
use crate::llm::hub::{HubCitation, HubFinish, HubUsage, signature_usable};
use crate::llm::wire::Protocol;

/// A completed assistant message with one `output_text` part.
pub(super) fn message_item(id: &str, text: &str, annotations: Vec<Value>) -> Value {
    let part = object([
        ("type", "output_text".into()),
        ("text", text.into()),
        ("annotations", Value::Array(annotations)),
    ]);
    object([
        ("id", id.into()),
        ("type", "message".into()),
        ("role", "assistant".into()),
        ("status", "completed".into()),
        ("content", Value::Array(vec![part])),
    ])
}

/// A `url_citation` annotation whose range is shifted by `shift`, the UTF-16 length of the message
/// text before the cited part. A citation without a range covers the whole part (`len` units).
pub(super) fn annotation_json(citation: &HubCitation, shift: u32, len: u32) -> Value {
    json!({
        "type": "url_citation",
        "url": citation.url,
        "title": citation.title,
        "start_index": citation.start_index.map_or(shift, |start| start.saturating_add(shift)),
        "end_index": citation
            .end_index
            .map_or(shift.saturating_add(len), |end| end.saturating_add(shift)),
    })
}

/// A `web_search_call` output item.
pub(super) fn web_search_call_item(id: &str, status: &str, action: &Value) -> Value {
    json!({"id": id, "type": "web_search_call", "status": status, "action": action})
}

/// Whether a server tool call is a web search this protocol ran, the only one it can replay.
pub(super) fn is_own_search(origin: Option<Protocol>, name: &str) -> bool {
    name == WEB_SEARCH_TOOL_NAME && signature_usable(origin, OWN)
}

/// A reasoning item toward a client. Only reasoning from this protocol (or of unknown origin) keeps
/// its upstream id and encrypted payload; other reasoning keeps its text under `synthesized_id`
/// and drops the payload, which only its own provider can read, and is dropped when it has no
/// text.
pub(super) fn client_reasoning_item(
    text: &str,
    encrypted: Option<&str>,
    id: Option<&str>,
    origin: Option<Protocol>,
    synthesized_id: &str,
) -> Option<Value> {
    match id {
        Some(id) if signature_usable(origin, OWN) => Some(reasoning_item(id, text, encrypted)),
        _ if !text.is_empty() => Some(reasoning_item(synthesized_id, text, None)),
        _ => None,
    }
}

/// A `reasoning` item with an optional summary text and encrypted payload.
pub(super) fn reasoning_item(id: &str, text: &str, encrypted: Option<&str>) -> Value {
    let summary = if text.is_empty() {
        json!([])
    } else {
        json!([{"type": "summary_text", "text": text}])
    };
    let mut item = Map::new();
    item.insert("id".into(), id.into());
    item.insert("type".into(), "reasoning".into());
    item.insert("summary".into(), summary);
    insert_some(&mut item, "encrypted_content", encrypted);
    Value::Object(item)
}

/// A completed `function_call` output item.
pub(super) fn function_call_item(id: &str, call_id: &str, name: &str, arguments: &str) -> Value {
    json!({
        "id": id,
        "type": "function_call",
        "call_id": call_id,
        "name": name,
        "arguments": arguments,
        "status": "completed",
    })
}

/// The response status and incomplete reason for a finish. Responses has no closer words for
/// `Pause` (reported as `max_output_tokens`) or a provider-specific reason (reported as completed).
pub(super) fn status_of(finish: &HubFinish) -> (&'static str, Option<&'static str>) {
    match finish {
        HubFinish::Length | HubFinish::Pause => ("incomplete", Some("max_output_tokens")),
        HubFinish::ContentFilter => ("incomplete", Some("content_filter")),
        HubFinish::Stop | HubFinish::ToolCalls | HubFinish::Other(_) => ("completed", None),
    }
}

/// The identity of a response, the same in every event of a stream.
pub(super) struct ResponseHead<'a> {
    pub(super) id: &'a str,
    pub(super) model: &'a str,
    pub(super) created_at: u64,
}

impl<'a> ResponseHead<'a> {
    pub(super) fn new(id: &'a str, model: &'a str) -> Self {
        Self {
            id,
            model,
            created_at: unix_now(),
        }
    }
}

/// The response envelope shared by whole responses and stream events; `usage` is `None` while a
/// stream is in progress.
pub(super) fn response_object(
    head: &ResponseHead<'_>,
    status: &str,
    output: Vec<Value>,
    usage: Option<&HubUsage>,
    incomplete_reason: Option<&str>,
) -> Value {
    let usage = usage.map(|usage| {
        let input = usage.input_tokens.unwrap_or(0);
        let output = usage.output_tokens.unwrap_or(0);
        json!({
            "input_tokens": input,
            "output_tokens": output,
            "total_tokens": input.saturating_add(output),
            "input_tokens_details": {"cached_tokens": usage.cached_input_tokens.unwrap_or(0)},
            "output_tokens_details": {"reasoning_tokens": usage.reasoning_tokens.unwrap_or(0)},
        })
    });
    json!({
        "id": head.id,
        "object": "response",
        "created_at": head.created_at,
        "status": status,
        "model": head.model,
        "output": output,
        "error": null,
        "incomplete_details": incomplete_reason.map(|reason| json!({"reason": reason})),
        "usage": usage,
    })
}
