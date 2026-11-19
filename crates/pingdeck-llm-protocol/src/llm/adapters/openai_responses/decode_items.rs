//! Reading upstream output items, usage and status, shared by the whole-response decoder, the
//! client request decoder and the stream decoder.

use serde_json::{Value, json};

use super::OWN;
use crate::llm::adapters::{WEB_SEARCH_TOOL_NAME, arguments_text, as_u32, str_of, str_or_empty};
use crate::llm::hub::{HubCitation, HubFinish, HubPart, HubUsage};
use crate::llm::wire::Protocol;
use pingdeck_base::error::AppError;

/// The error message of a failed reply (an `error` value or `status: failed`); `None` otherwise.
pub(super) fn failure_message(response: &Value) -> Option<String> {
    let error = response.get("error").filter(|e| !e.is_null());
    let failed = str_of(response, "status") == Some("failed");
    if error.is_none() && !failed {
        return None;
    }
    let message = match error {
        Some(Value::String(message)) => Some(message.as_str()),
        Some(error) => str_of(error, "message"),
        None => None,
    };
    Some(message.unwrap_or("the response failed").to_owned())
}

/// The hub finish for a reply status; a missing status counts as completed.
pub(super) fn finish_of(status: Option<&str>, response: &Value, has_calls: bool) -> HubFinish {
    match status {
        Some("incomplete") => {
            let reason = response
                .get("incomplete_details")
                .and_then(|details| str_of(details, "reason"));
            match reason {
                Some("max_output_tokens") => HubFinish::Length,
                Some("content_filter") => HubFinish::ContentFilter,
                Some(other) => HubFinish::Other(other.to_owned()),
                None => HubFinish::Other("incomplete".into()),
            }
        }
        Some("completed") | None => {
            if has_calls {
                HubFinish::ToolCalls
            } else {
                HubFinish::Stop
            }
        }
        Some(other) => HubFinish::Other(other.to_owned()),
    }
}

/// Token usage, cached and reasoning tokens included.
pub(super) fn parse_usage(usage: &Value) -> HubUsage {
    let number = |value: Option<&Value>| value.and_then(Value::as_u64);
    HubUsage {
        input_tokens: number(usage.get("input_tokens")),
        output_tokens: number(usage.get("output_tokens")),
        cached_input_tokens: number(usage.pointer("/input_tokens_details/cached_tokens")),
        cache_write_tokens: number(usage.pointer("/input_tokens_details/cache_write_tokens")),
        cache_write_long_tokens: None,
        reasoning_tokens: number(usage.pointer("/output_tokens_details/reasoning_tokens")),
    }
}

/// The text of a reasoning item: its summary parts, or its content parts when there is no summary,
/// joined by blank lines.
pub(super) fn reasoning_text(item: &Value) -> String {
    let texts = |key: &str| -> Vec<&str> {
        item.get(key)
            .and_then(Value::as_array)
            .map(|parts| parts.iter().filter_map(|p| str_of(p, "text")).collect())
            .unwrap_or_default()
    };
    let summary = texts("summary");
    if summary.is_empty() {
        texts("content").join("\n\n")
    } else {
        summary.join("\n\n")
    }
}

/// A `url_citation` annotation; `None` for other annotation types. An index that is not a `u32`
/// reads as missing.
pub(super) fn url_citation(annotation: &Value) -> Option<HubCitation> {
    if str_of(annotation, "type") != Some("url_citation") {
        return None;
    }
    Some(HubCitation {
        url: str_or_empty(annotation, "url"),
        title: str_or_empty(annotation, "title"),
        snippet: None,
        start_index: annotation.get("start_index").and_then(as_u32),
        end_index: annotation.get("end_index").and_then(as_u32),
    })
}

/// The `action` of a web search call as the tool input; `{}` when missing.
pub(super) fn search_input(item: &Value) -> Value {
    item.get("action")
        .filter(|a| !a.is_null())
        .cloned()
        .unwrap_or_else(|| json!({}))
}

/// The error for an output item type the hub cannot carry.
pub(super) fn unsupported_item(kind: Option<&str>) -> AppError {
    AppError::network(format!(
        "Responses output item type `{}` is not supported",
        kind.unwrap_or("<missing>")
    ))
}

/// Reasoning carrying an id or an encrypted payload came from this protocol and can go back to it.
pub(super) fn reasoning_origin(id: Option<&str>, encrypted: Option<&str>) -> Option<Protocol> {
    (id.is_some() || encrypted.is_some()).then_some(OWN)
}

/// The call id of a function call item, or its item id when it has none.
pub(super) fn call_id_of(item: &Value) -> &str {
    str_of(item, "call_id")
        .or_else(|| str_of(item, "id"))
        .unwrap_or_default()
}

/// A `function_call` item as a tool call.
pub(super) fn function_call_part(item: &Value) -> HubPart {
    HubPart::ToolCall {
        id: call_id_of(item).to_owned(),
        name: str_or_empty(item, "name"),
        arguments: arguments_text(item.get("arguments")),
        signature: None,
        origin: None,
    }
}

/// A `web_search_call` item as a server tool use of this protocol.
pub(super) fn web_search_part(item: &Value) -> HubPart {
    HubPart::ServerToolUse {
        id: str_or_empty(item, "id"),
        name: WEB_SEARCH_TOOL_NAME.into(),
        input: search_input(item),
        origin: Some(OWN),
    }
}

/// A `reasoning` item as a reasoning part.
pub(super) fn reasoning_part(item: &Value) -> HubPart {
    let encrypted = str_of(item, "encrypted_content").map(str::to_owned);
    let id = str_of(item, "id").map(str::to_owned);
    HubPart::Reasoning {
        text: reasoning_text(item),
        signature: None,
        origin: reasoning_origin(id.as_deref(), encrypted.as_deref()),
        encrypted,
        id,
    }
}
