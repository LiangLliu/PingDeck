//! Content block and tool definition builders of the Anthropic Messages encoder.

use serde_json::{Map, Value, json};

use super::{OWN, WEB_SEARCH_TOOL, text_block};
use crate::llm::adapters::{WEB_SEARCH_TOOL_NAME, insert_some, parse_args_object};
use crate::llm::hub::{
    HubDocument, HubDocumentSource, HubImage, HubNativeTool, HubPart, HubTool, signature_usable,
    wrap_signature,
};
use crate::llm::wire::Protocol;
use pingdeck_base::error::AppError;

/// The error for an attachment that should have been inlined before encoding.
pub(super) fn unresolved(what: &str, id: &str) -> AppError {
    AppError::validation(format!(
        "anthropic: {what} attachment {id} must be resolved before the request is encoded"
    ))
}

/// The error for audio, which Anthropic Messages cannot carry.
pub(super) fn no_audio() -> AppError {
    AppError::unsupported("anthropic: Anthropic Messages does not accept audio input")
}

/// A short name of a part's kind for error messages.
pub(super) fn part_kind(part: &HubPart) -> &'static str {
    match part {
        HubPart::Text { .. } => "text",
        HubPart::Image { .. } => "image",
        HubPart::Document { .. } => "document",
        HubPart::Audio { .. } => "audio",
        HubPart::ToolCall { .. } => "tool call",
        HubPart::ToolResult { .. } => "tool result",
        HubPart::Reasoning { .. } => "reasoning",
        HubPart::ServerToolUse { .. } => "server tool call",
        HubPart::ServerToolResult { .. } => "server tool result",
    }
}

pub(super) fn tool_use_block(id: &str, name: &str, arguments: &str) -> Value {
    json!({"type": "tool_use", "id": id, "name": name, "input": parse_args_object(arguments)})
}

pub(super) fn server_tool_use_block(id: &str, name: &str, input: &Value) -> Value {
    json!({"type": "server_tool_use", "id": id, "name": name, "input": input})
}

/// A server tool result block; its type is `<tool name>_tool_result`.
pub(super) fn server_tool_result_block(call_id: &str, name: &str, payload: &Value) -> Value {
    json!({"type": format!("{name}_tool_result"), "tool_use_id": call_id, "content": payload})
}

pub(super) fn image_block(image: &HubImage) -> Result<Value, AppError> {
    let source = match image {
        HubImage::Base64 { media_type, data } => {
            json!({"type": "base64", "media_type": media_type, "data": data})
        }
        HubImage::AttachmentRef { attachment_id, .. } => {
            return Err(unresolved("image", attachment_id));
        }
        HubImage::Url { url } => json!({"type": "url", "url": url}),
    };
    Ok(json!({"type": "image", "source": source}))
}

/// A document block; a non-empty name becomes its `title`.
pub(super) fn document_block(document: &HubDocument) -> Result<Value, AppError> {
    let source = match &document.source {
        HubDocumentSource::Base64 { data } => {
            json!({"type": "base64", "media_type": document.media_type, "data": data})
        }
        HubDocumentSource::Text { text } => {
            json!({"type": "text", "media_type": "text/plain", "data": text})
        }
        HubDocumentSource::Url { url } => json!({"type": "url", "url": url}),
        HubDocumentSource::AttachmentRef { attachment_id } => {
            return Err(unresolved("document", attachment_id));
        }
    };
    let mut block = json!({"type": "document", "source": source});
    if let Some(name) = document.name.as_deref().filter(|n| !n.is_empty()) {
        block["title"] = json!(name);
    }
    Ok(block)
}

/// A reasoning part replayed **upstream**.
///
/// Anthropic verifies thinking signatures, so reasoning another protocol produced, or text
/// without a signature, is dropped. Encrypted data with an item `id` came from OpenAI Responses
/// and cannot be replayed as `redacted_thinking`.
pub(super) fn reasoning_block(
    text: &str,
    signature: Option<&str>,
    encrypted: Option<&str>,
    id: Option<&str>,
    origin: Option<Protocol>,
) -> Option<Value> {
    if !signature_usable(origin, OWN) {
        return None;
    }
    if let Some(signature) = signature.filter(|s| !s.is_empty()) {
        return Some(json!({"type": "thinking", "thinking": text, "signature": signature}));
    }
    match (encrypted.filter(|d| !d.is_empty()), id) {
        (Some(data), None) => Some(json!({"type": "redacted_thinking", "data": data})),
        _ => None,
    }
}

/// A reasoning part sent to a **gateway client**.
///
/// A foreign signature or encrypted blob is wrapped in a `pdk1.<origin>.` envelope so it can be
/// routed back to its producer; unsigned text keeps an empty signature. Encrypted data with an
/// item `id` (OpenAI Responses) is dropped, as upstream.
pub(super) fn client_reasoning_block(
    text: &str,
    signature: Option<&str>,
    encrypted: Option<&str>,
    id: Option<&str>,
    origin: Option<Protocol>,
) -> Option<Value> {
    if let Some(signature) = signature.filter(|s| !s.is_empty()) {
        let signature = wrap_signature(origin, signature, OWN);
        return Some(json!({"type": "thinking", "thinking": text, "signature": signature}));
    }
    if text.is_empty() {
        let data = encrypted.filter(|d| !d.is_empty())?;
        if id.is_some() {
            return None;
        }
        let data = wrap_signature(origin, data, OWN);
        return Some(json!({"type": "redacted_thinking", "data": data}));
    }
    Some(json!({"type": "thinking", "thinking": text, "signature": ""}))
}

/// A `tool_result` block; only text, images and documents may appear inside it.
pub(super) fn tool_result_block(
    call_id: &str,
    content: &[HubPart],
    is_error: bool,
) -> Result<Value, AppError> {
    let mut inner: Vec<Value> = Vec::new();
    for part in content {
        match part {
            HubPart::Text { text, .. } if text.is_empty() => {}
            HubPart::Text { text, .. } => inner.push(text_block(text)),
            HubPart::Image { image } => inner.push(image_block(image)?),
            HubPart::Document { document } => inner.push(document_block(document)?),
            HubPart::Audio { .. } => return Err(no_audio()),
            other => {
                return Err(AppError::unsupported(format!(
                    "anthropic: a {} inside a tool result is not supported",
                    part_kind(other)
                )));
            }
        }
    }
    let mut block = Map::new();
    block.insert("type".into(), json!("tool_result"));
    block.insert("tool_use_id".into(), json!(call_id));
    if !inner.is_empty() {
        block.insert("content".into(), Value::Array(inner));
    }
    if is_error {
        block.insert("is_error".into(), json!(true));
    }
    Ok(Value::Object(block))
}

/// A function tool definition; a schema that is not an object becomes `{"type": "object"}`.
pub(super) fn encode_tool(tool: &HubTool) -> Value {
    let mut out = Map::new();
    out.insert("name".into(), json!(tool.name));
    insert_some(&mut out, "description", tool.description.as_deref());
    let schema = if tool.parameters.is_object() {
        tool.parameters.clone()
    } else {
        json!({"type": "object"})
    };
    out.insert("input_schema".into(), schema);
    insert_some(&mut out, "strict", tool.strict);
    Value::Object(out)
}

/// A native tool definition (web search).
pub(super) fn encode_native_tool(tool: &HubNativeTool) -> Result<Value, AppError> {
    match tool {
        HubNativeTool::WebSearch {
            max_uses,
            allowed_domains,
            blocked_domains,
        } => {
            if !allowed_domains.is_empty() && !blocked_domains.is_empty() {
                return Err(AppError::unsupported(
                    "anthropic: web search accepts `allowed_domains` or `blocked_domains`, not both",
                ));
            }
            let mut out = json!({"type": WEB_SEARCH_TOOL, "name": WEB_SEARCH_TOOL_NAME});
            if let Some(max_uses) = max_uses {
                out["max_uses"] = json!(max_uses);
            }
            if !allowed_domains.is_empty() {
                out["allowed_domains"] = json!(allowed_domains);
            }
            if !blocked_domains.is_empty() {
                out["blocked_domains"] = json!(blocked_domains);
            }
            Ok(out)
        }
    }
}
