//! Encoding: hub requests toward an upstream (messages to `input` items, tools, options) and hub
//! responses toward a client.

use std::borrow::Cow;

use serde_json::{Map, Value, json};

use super::OWN;
use super::encode_items::{
    ResponseHead, annotation_json, client_reasoning_item, function_call_item, is_own_search,
    message_item, reasoning_item, response_object, status_of, web_search_call_item,
};
use super::options::{
    insert_reasoning, native_search_limit, text_format_json, tool_choice_json, tools_json,
};
use crate::llm::adapters::{
    EncodeCtx, WEB_SEARCH_TOOL_NAME, image_data_url, insert_some, utf16_len,
};
use crate::llm::hub::{
    HubDocument, HubDocumentSource, HubMessage, HubPart, HubRequest, HubResponse, HubRole,
    signature_usable,
};
use crate::llm::wire::Protocol;
use spanbay_base::error::AppError;

/// The upstream request body for `req`.
///
/// Errors come in a fixed order: the assistant prefill check, then message content, then native
/// tools.
pub(super) fn request(req: &HubRequest, ctx: &EncodeCtx<'_>) -> Result<Value, AppError> {
    if req.ends_with_assistant() {
        return Err(AppError::validation(
            "the Responses API does not take an assistant prefill; end the conversation with \
             a user or tool message",
        ));
    }
    let mut body = Map::new();
    body.insert("model".into(), json!(req.model));
    if let Some(system) = req.system.as_deref().filter(|s| !s.is_empty()) {
        body.insert("instructions".into(), json!(system));
    }
    body.insert("input".into(), Value::Array(input_items(&req.messages)?));
    // The gateway keeps no upstream state: reasoning is replayed through `encrypted_content`.
    body.insert("store".into(), json!(false));
    if req.stream {
        body.insert("stream".into(), json!(true));
    }
    insert_some(&mut body, "max_output_tokens", req.max_tokens);
    if !ctx.quirks.drops_sampling() {
        insert_some(&mut body, "temperature", req.temperature);
        insert_some(&mut body, "top_p", req.top_p);
    }
    if let Some(tools) = tools_json(req)? {
        body.insert("tools".into(), Value::Array(tools));
    }
    insert_some(
        &mut body,
        "max_tool_calls",
        native_search_limit(&req.native_tools),
    );
    if let Some(choice) = &req.tool_choice {
        body.insert("tool_choice".into(), tool_choice_json(choice));
    }
    insert_some(&mut body, "parallel_tool_calls", req.parallel_tool_calls);
    insert_some(
        &mut body,
        "text",
        text_format_json(req.response_format.as_ref()),
    );
    insert_reasoning(req, ctx, &mut body);
    insert_some(&mut body, "user", req.user.as_deref());
    insert_some(
        &mut body,
        "prompt_cache_key",
        req.cache.as_ref().and_then(|cache| cache.key.as_deref()),
    );
    Ok(Value::Object(body))
}

/// The client response body for `response`.
pub(super) fn response(response: &HubResponse) -> Value {
    let output = response_output(&response.content);
    let (status, reason) = status_of(&response.finish);
    response_object(
        &ResponseHead::new(&response.id, &response.model),
        status,
        output,
        Some(&response.usage),
        reason,
    )
}

/// The `output` items of a response. Consecutive text parts join into one message, their
/// citations shifted to message offsets; any other part ends the message, even one that is
/// dropped (a foreign web search, say).
fn response_output(content: &[HubPart]) -> Vec<Value> {
    let mut output: Vec<Value> = Vec::new();
    let mut text = String::new();
    let mut annotations: Vec<Value> = Vec::new();
    // The UTF-16 length of `text`, kept as a running sum instead of recounting the text.
    let mut text_len: u32 = 0;
    for part in content {
        if let HubPart::Text {
            text: more,
            citations,
            ..
        } = part
        {
            let len = utf16_len(more);
            annotations.extend(
                citations
                    .iter()
                    .map(|citation| annotation_json(citation, text_len, len)),
            );
            text.push_str(more);
            text_len = text_len.saturating_add(len);
            continue;
        }
        flush_text(&mut output, &mut text, &mut annotations);
        text_len = 0;
        push_part_item(&mut output, part);
    }
    flush_text(&mut output, &mut text, &mut annotations);
    output
}

/// Appends the output item of a part that is not text, its id numbered by output position. Parts
/// without an item (a foreign web search, reasoning `client_reasoning_item` drops, any other part)
/// add nothing.
fn push_part_item(output: &mut Vec<Value>, part: &HubPart) {
    match part {
        HubPart::Reasoning {
            text,
            encrypted,
            id,
            origin,
            ..
        } => {
            let synthesized = format!("rs_{}", output.len());
            if let Some(item) = client_reasoning_item(
                text,
                encrypted.as_deref(),
                id.as_deref(),
                *origin,
                &synthesized,
            ) {
                output.push(item);
            }
        }
        HubPart::ToolCall {
            id,
            name,
            arguments,
            ..
        } => {
            let item_id = format!("fc_{}", output.len());
            output.push(function_call_item(&item_id, id, name, arguments));
        }
        HubPart::ServerToolUse {
            id,
            name,
            input,
            origin,
        } if is_own_search(*origin, name) => {
            output.push(web_search_call_item(id, "completed", input));
        }
        _ => {}
    }
}

/// Ends the open message, if any, as a `msg_{n}` item.
fn flush_text(output: &mut Vec<Value>, text: &mut String, annotations: &mut Vec<Value>) {
    let annotations = std::mem::take(annotations);
    if !text.is_empty() {
        let id = format!("msg_{}", output.len());
        output.push(message_item(&id, &std::mem::take(text), annotations));
    }
}

/// The error for an attachment that was not replaced by its content before encoding.
fn unresolved(what: &str) -> AppError {
    AppError::validation(format!(
        "{what} attachment must be resolved before the request is encoded"
    ))
}

/// The file name sent for an unnamed inline document.
fn default_filename(media_type: &str) -> &'static str {
    match media_type {
        "application/pdf" => "document.pdf",
        "text/plain" => "document.txt",
        "text/markdown" => "document.md",
        "text/csv" => "document.csv",
        _ => "document",
    }
}

/// A user document: inline bytes and URLs as `input_file`, text inside a `<document>` block. The
/// block escapes only `"` in the name and nothing in the type, a known gap kept as it is.
fn document_item(document: &HubDocument) -> Result<Value, AppError> {
    match &document.source {
        HubDocumentSource::Base64 { data } => Ok(json!({
            "type": "input_file",
            "filename": document
                .name
                .as_deref()
                .unwrap_or_else(|| default_filename(&document.media_type)),
            "file_data": format!("data:{};base64,{data}", document.media_type),
        })),
        HubDocumentSource::Url { url } => Ok(json!({"type": "input_file", "file_url": url})),
        HubDocumentSource::Text { text } => {
            let name = document
                .name
                .as_deref()
                .map(|n| format!(" name=\"{}\"", n.replace('"', "&quot;")))
                .unwrap_or_default();
            Ok(json!({
                "type": "input_text",
                "text": format!(
                    "<document{name} type=\"{}\">\n{text}\n</document>",
                    document.media_type
                ),
            }))
        }
        HubDocumentSource::AttachmentRef { .. } => Err(unresolved("document")),
    }
}

/// The `input` items for the conversation. The content parts of a message are grouped into one
/// message item; a tool call, tool result or reasoning item ends that group and follows it as its
/// own item.
fn input_items(messages: &[HubMessage]) -> Result<Vec<Value>, AppError> {
    let mut items = Vec::new();
    for message in messages {
        let assistant = message.role == HubRole::Assistant;
        let mut buffer: Vec<Value> = Vec::new();
        for part in &message.content {
            match part {
                HubPart::Text { text, .. } if !text.is_empty() => buffer.push(json!({
                    "type": if assistant { "output_text" } else { "input_text" },
                    "text": text,
                })),
                HubPart::Text { .. } => {}
                HubPart::Image { image } if !assistant => {
                    buffer.push(json!({"type": "input_image",
                        "image_url": image_data_url(image).ok_or_else(|| unresolved("image"))?
                    }));
                }
                HubPart::Document { document } if !assistant => {
                    buffer.push(document_item(document)?);
                }
                HubPart::Image { .. } | HubPart::Document { .. } => {
                    return Err(AppError::unsupported(
                        "OpenAI Responses: images and documents in assistant messages are not \
                         supported",
                    ));
                }
                HubPart::Audio { .. } => {
                    return Err(AppError::unsupported(
                        "OpenAI Responses does not accept audio input here",
                    ));
                }
                _ => {
                    if let Some(item) = special_item(part)? {
                        flush_message(&mut items, &mut buffer, assistant);
                        items.push(item);
                    }
                }
            }
        }
        flush_message(&mut items, &mut buffer, assistant);
    }
    Ok(items)
}

/// Ends the buffered content parts, if any, as a message item.
fn flush_message(items: &mut Vec<Value>, buffer: &mut Vec<Value>, assistant: bool) {
    if !buffer.is_empty() {
        let role = if assistant { "assistant" } else { "user" };
        items.push(json!({"type": "message", "role": role, "content": std::mem::take(buffer)}));
    }
}

/// One part of a tool result as Responses takes it.
enum ToolOutputPart<'a> {
    Text(Cow<'a, str>),
    /// An image URL.
    Image(String),
}

fn tool_output_part(part: &HubPart) -> Result<ToolOutputPart<'_>, AppError> {
    match part {
        HubPart::Text { text, .. } => Ok(ToolOutputPart::Text(Cow::Borrowed(text))),
        HubPart::Image { image } => image_data_url(image)
            .map(ToolOutputPart::Image)
            .ok_or_else(|| unresolved("image")),
        HubPart::Document { document } => {
            if matches!(document.source, HubDocumentSource::AttachmentRef { .. }) {
                return Err(unresolved("document"));
            }
            let name = document.name.as_deref().unwrap_or(&document.media_type);
            Ok(ToolOutputPart::Text(Cow::Owned(format!(
                "[document: {name}]"
            ))))
        }
        _ => Err(AppError::unsupported(
            "OpenAI Responses: only text, image and document parts can be a tool result",
        )),
    }
}

/// The `output` of a `function_call_output`: the text lines joined, or a list of input items when
/// the result carries an image, which Responses takes as it is. A document becomes a placeholder
/// line naming it.
fn tool_output(content: &[HubPart]) -> Result<Value, AppError> {
    let parts = content
        .iter()
        .map(tool_output_part)
        .collect::<Result<Vec<_>, _>>()?;
    if !parts
        .iter()
        .any(|part| matches!(part, ToolOutputPart::Image(_)))
    {
        let lines: Vec<&str> = parts
            .iter()
            .filter_map(|part| match part {
                ToolOutputPart::Text(text) => Some(text.as_ref()),
                ToolOutputPart::Image(_) => None,
            })
            .collect();
        return Ok(Value::String(lines.join("\n")));
    }
    let items = parts
        .into_iter()
        .map(|part| match part {
            ToolOutputPart::Text(text) => json!({"type": "input_text", "text": text}),
            ToolOutputPart::Image(url) => json!({"type": "input_image", "image_url": url}),
        })
        .collect();
    Ok(Value::Array(items))
}

/// The item for a part that is not message content; `None` for parts that are dropped (reasoning
/// without an id or from another protocol, another protocol's server tool calls).
fn special_item(part: &HubPart) -> Result<Option<Value>, AppError> {
    Ok(match part {
        HubPart::ToolCall {
            id,
            name,
            arguments,
            ..
        } => Some(json!({
            "type": "function_call",
            "call_id": id,
            "name": name,
            "arguments": arguments,
        })),
        HubPart::ToolResult {
            call_id, content, ..
        } => Some(
            json!({"type": "function_call_output", "call_id": call_id, "output": tool_output(content)?}),
        ),
        HubPart::Reasoning {
            text,
            encrypted,
            id,
            origin,
            ..
        } => id
            .as_ref()
            .filter(|_| signature_usable(*origin, OWN))
            .map(|id| reasoning_item(id, text, encrypted.as_deref())),
        HubPart::ServerToolUse {
            id,
            name,
            input,
            origin,
        } => replayed_server_tool(id, name, input, *origin)?,
        _ => None,
    })
}

/// The replayed item of a server tool call: `None` for another protocol's call, an error for an
/// own call other than web search.
fn replayed_server_tool(
    id: &str,
    name: &str,
    input: &Value,
    origin: Option<Protocol>,
) -> Result<Option<Value>, AppError> {
    if !signature_usable(origin, OWN) {
        return Ok(None);
    }
    if name != WEB_SEARCH_TOOL_NAME {
        return Err(AppError::unsupported(format!(
            "OpenAI Responses: server tool `{name}` cannot be replayed"
        )));
    }
    Ok(Some(web_search_call_item(id, "completed", input)))
}
