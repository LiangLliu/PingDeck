//! Decoding: upstream replies into hub responses, and client requests (`input` items, tools,
//! options) into hub requests.

use serde_json::Value;

use super::decode_items::{
    failure_message, finish_of, function_call_part, parse_usage, reasoning_part, unsupported_item,
    url_citation, web_search_part,
};
use super::options::{decode_format, decode_reasoning, decode_tool_choice, decode_tools};
use crate::llm::adapters::{InboundHint, as_u32, parse_base64_data_url, str_of, utf16_len};
use crate::llm::hub::{
    HubDocument, HubDocumentSource, HubImage, HubMessage, HubPart, HubRequest, HubResponse,
    HubRole, text_of,
};
use spanbay_base::error::AppError;

/// The hub response for an upstream reply body.
///
/// # Errors
///
/// A network error for a failed reply, a reply without `output`, or the first output item of a
/// type the hub cannot carry (the parts read before it are dropped).
pub(super) fn response(body: &Value) -> Result<HubResponse, AppError> {
    if let Some(message) = failure_message(body) {
        return Err(AppError::network(message));
    }
    let output = body
        .get("output")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::network("Responses reply has no `output` array"))?;
    let mut content = Vec::new();
    for item in output {
        match str_of(item, "type") {
            Some("message") => content.extend(message_part(item)),
            Some("reasoning") => content.push(reasoning_part(item)),
            Some("function_call") => content.push(function_call_part(item)),
            Some("web_search_call") => content.push(web_search_part(item)),
            other => return Err(unsupported_item(other)),
        }
    }
    let has_calls = content
        .iter()
        .any(|part| matches!(part, HubPart::ToolCall { .. }));
    Ok(HubResponse {
        id: str_of(body, "id").unwrap_or_default().to_owned(),
        model: str_of(body, "model").unwrap_or_default().to_owned(),
        content,
        finish: finish_of(str_of(body, "status"), body, has_calls),
        usage: body.get("usage").map(parse_usage).unwrap_or_default(),
    })
}

/// The text of a `message` output item: its `output_text` and `refusal` parts joined, each part's
/// citations shifted to message offsets. `None` when there is no text.
fn message_part(item: &Value) -> Option<HubPart> {
    let Some(parts) = item.get("content").and_then(Value::as_array) else {
        let text = item
            .get("content")
            .and_then(Value::as_str)
            .unwrap_or_default();
        return (!text.is_empty()).then(|| HubPart::text(text));
    };
    let mut text = String::new();
    // The UTF-16 length of `text`, kept as a running sum instead of recounting the text.
    let mut text_len: u32 = 0;
    let mut citations = Vec::new();
    for part in parts {
        let more = match str_of(part, "type") {
            Some("output_text") => str_of(part, "text"),
            Some("refusal") => str_of(part, "refusal"),
            _ => None,
        };
        let Some(more) = more else { continue };
        let shift = text_len;
        for mut citation in part
            .get("annotations")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
            .filter_map(url_citation)
        {
            citation.start_index = citation
                .start_index
                .map(|start| start.saturating_add(shift));
            citation.end_index = citation.end_index.map(|end| end.saturating_add(shift));
            citations.push(citation);
        }
        text.push_str(more);
        text_len = text_len.saturating_add(utf16_len(more));
    }
    (!text.is_empty()).then_some(HubPart::Text {
        text,
        signature: None,
        origin: None,
        citations,
    })
}

/// The hub request for a client request body.
///
/// # Errors
///
/// A validation or unsupported error, in field order: the body, `model`, each `input` item,
/// `tools`, `tool_choice`, then `text.format`.
pub(super) fn request(body: &Value, hint: &InboundHint) -> Result<HubRequest, AppError> {
    let object = body
        .as_object()
        .ok_or_else(|| AppError::validation("request body must be a JSON object"))?;
    let model = object
        .get("model")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .map(str::to_owned)
        .or_else(|| hint.model.clone())
        .ok_or_else(|| AppError::validation("`model` is required"))?;
    let mut system: Vec<String> = Vec::new();
    if let Some(instructions) = object.get("instructions").and_then(Value::as_str) {
        system.push(instructions.to_owned());
    }
    let messages = decode_input(object.get("input"), &mut system)?;
    let system: Vec<String> = system.into_iter().filter(|t| !t.is_empty()).collect();
    let (tools, native_tools) = decode_tools(object)?;
    // `tool_choice` is decoded before `text.format`: struct fields evaluate in order.
    Ok(HubRequest {
        model,
        system: (!system.is_empty()).then(|| system.join("\n\n")),
        messages,
        tools,
        native_tools,
        tool_choice: object
            .get("tool_choice")
            .map(decode_tool_choice)
            .transpose()?,
        parallel_tool_calls: object.get("parallel_tool_calls").and_then(Value::as_bool),
        max_tokens: object.get("max_output_tokens").and_then(as_u32),
        temperature: object.get("temperature").and_then(Value::as_f64),
        top_p: object.get("top_p").and_then(Value::as_f64),
        stream: object
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false)
            || hint.stream,
        response_format: decode_format(object.get("text"))?,
        reasoning: decode_reasoning(object.get("reasoning")),
        user: object
            .get("user")
            .and_then(Value::as_str)
            .map(str::to_owned),
        cache: crate::llm::adapters::shared::prompt_cache_key(body),
        ..HubRequest::default()
    })
}

/// The messages of `input` (a string or an array of items); system and developer messages go to
/// `system` instead.
fn decode_input(
    input: Option<&Value>,
    system: &mut Vec<String>,
) -> Result<Vec<HubMessage>, AppError> {
    let mut messages = Vec::new();
    match input {
        Some(Value::String(text)) => messages.push(HubMessage::user(text.clone())),
        Some(Value::Array(items)) => {
            for item in items {
                decode_item(item, &mut messages, system)?;
            }
        }
        _ => {
            return Err(AppError::validation(
                "`input` must be a string or an array of items",
            ));
        }
    }
    Ok(messages)
}

/// An image URL: a base64 `data:` URL becomes inline bytes, anything else stays a URL.
fn parse_image(url: &str) -> HubImage {
    if let Some((media_type, data)) = parse_base64_data_url(url) {
        return HubImage::Base64 {
            media_type: media_type.to_owned(),
            data: data.to_owned(),
        };
    }
    HubImage::Url {
        url: url.to_owned(),
    }
}

/// The media type for a file name or URL extension (query and fragment ignored). The short list is
/// deliberate: a general table (`mime_guess`) maps far more extensions and would change which
/// documents providers receive as what.
fn media_type_of(name: &str) -> &'static str {
    let path = name.split(['?', '#']).next().unwrap_or(name);
    match path
        .rsplit_once('.')
        .map(|(_, ext)| ext.to_ascii_lowercase())
        .as_deref()
    {
        Some("pdf") => "application/pdf",
        Some("txt") => "text/plain",
        Some("md") => "text/markdown",
        Some("csv") => "text/csv",
        Some("json") => "application/json",
        Some("html" | "htm") => "text/html",
        _ => "application/octet-stream",
    }
}

/// An `input_file` part as a document: inline `file_data` or a `file_url`; uploaded file ids
/// cannot be resolved.
fn decode_input_file(part: &Value) -> Result<HubPart, AppError> {
    let name = str_of(part, "filename").map(str::to_owned);
    let document = if let Some(file_data) = str_of(part, "file_data") {
        let (media_type, data) = parse_base64_data_url(file_data).ok_or_else(|| {
            AppError::unsupported("`input_file.file_data` must be a base64 `data:` URL")
        })?;
        HubDocument {
            media_type: media_type.to_owned(),
            name,
            source: HubDocumentSource::Base64 {
                data: data.to_owned(),
            },
        }
    } else if let Some(url) = str_of(part, "file_url") {
        HubDocument {
            media_type: media_type_of(name.as_deref().unwrap_or(url)).to_owned(),
            name,
            source: HubDocumentSource::Url {
                url: url.to_owned(),
            },
        }
    } else {
        return Err(AppError::unsupported(
            "`input_file` with a `file_id` is not supported, send `file_data` or `file_url`",
        ));
    };
    Ok(HubPart::Document { document })
}

/// The content of a message or tool result: a string, or an array of content parts.
fn decode_content(content: Option<&Value>) -> Result<Vec<HubPart>, AppError> {
    let parts = match content {
        None | Some(Value::Null) => return Ok(Vec::new()),
        Some(Value::String(text)) => return Ok(vec![HubPart::text(text.clone())]),
        Some(Value::Array(parts)) => parts,
        Some(_) => {
            return Err(AppError::validation(
                "message `content` must be a string or an array",
            ));
        }
    };
    let mut out = Vec::new();
    for part in parts {
        match str_of(part, "type") {
            Some("input_text" | "output_text" | "text") => {
                out.push(HubPart::text(str_of(part, "text").unwrap_or_default()));
            }
            Some("refusal") => out.push(HubPart::text(str_of(part, "refusal").unwrap_or_default())),
            Some("input_image") => {
                let url = match part.get("image_url") {
                    Some(Value::String(url)) => Some(url.as_str()),
                    Some(object) => str_of(object, "url"),
                    None => None,
                };
                let url = url.ok_or_else(|| {
                    AppError::unsupported(
                        "`input_image` without `image_url` (file ids) is not supported",
                    )
                })?;
                out.push(HubPart::Image {
                    image: parse_image(url),
                });
            }
            Some("input_file") => out.push(decode_input_file(part)?),
            Some("input_audio") => {
                return Err(AppError::unsupported(
                    "`input_audio` content parts are not supported",
                ));
            }
            other => {
                return Err(AppError::unsupported(format!(
                    "Responses content part type `{}` is not supported",
                    other.unwrap_or("<missing>")
                )));
            }
        }
    }
    Ok(out)
}

/// Appends `part` to the last message when it has `role`, else starts a new message.
fn push_part(messages: &mut Vec<HubMessage>, role: HubRole, part: HubPart) {
    match messages.last_mut() {
        Some(last) if last.role == role => last.content.push(part),
        _ => messages.push(HubMessage {
            role,
            content: vec![part],
        }),
    }
}

/// The name of the earlier tool call with id `call_id`.
fn call_name(messages: &[HubMessage], call_id: &str) -> Option<String> {
    messages
        .iter()
        .flat_map(|message| &message.content)
        .find_map(|part| match part {
            HubPart::ToolCall { id, name, .. } if id == call_id => Some(name.clone()),
            _ => None,
        })
}

/// Decodes one `input` item into `messages` (or `system`).
fn decode_item(
    item: &Value,
    messages: &mut Vec<HubMessage>,
    system: &mut Vec<String>,
) -> Result<(), AppError> {
    // An item with a role but no type is the short form of a message; an explicit empty `type`
    // reads as missing.
    let kind = str_of(item, "type").unwrap_or(if item.get("role").is_some() {
        "message"
    } else {
        ""
    });
    match kind {
        "message" => decode_message_item(item, messages, system)?,
        "function_call" => push_part(messages, HubRole::Assistant, function_call_part(item)),
        "function_call_output" => decode_function_call_output(item, messages)?,
        "web_search_call" => push_part(messages, HubRole::Assistant, web_search_part(item)),
        "reasoning" => push_part(messages, HubRole::Assistant, reasoning_part(item)),
        other => {
            return Err(AppError::unsupported(format!(
                "Responses input item type `{}` is not supported",
                if other.is_empty() { "<missing>" } else { other }
            )));
        }
    }
    Ok(())
}

/// A message item; an empty user message is skipped.
fn decode_message_item(
    item: &Value,
    messages: &mut Vec<HubMessage>,
    system: &mut Vec<String>,
) -> Result<(), AppError> {
    let parts = decode_content(item.get("content"))?;
    match str_of(item, "role") {
        Some("system" | "developer") => system.push(text_of(&parts)),
        Some("user") if !parts.is_empty() => {
            messages.push(HubMessage {
                role: HubRole::User,
                content: parts,
            });
        }
        Some("assistant") => {
            for part in parts {
                push_part(messages, HubRole::Assistant, part);
            }
        }
        Some("user") => {}
        other => {
            return Err(AppError::validation(format!(
                "unknown message role `{}`",
                other.unwrap_or("<missing>")
            )));
        }
    }
    Ok(())
}

/// A tool result; its name comes from the call with the same id, when one came earlier.
fn decode_function_call_output(
    item: &Value,
    messages: &mut Vec<HubMessage>,
) -> Result<(), AppError> {
    let call_id = str_of(item, "call_id").unwrap_or_default().to_owned();
    let content = match item.get("output") {
        Some(Value::Array(_)) => decode_content(item.get("output"))?,
        Some(Value::String(text)) => vec![HubPart::text(text.clone())],
        Some(Value::Null) | None => Vec::new(),
        Some(other) => vec![HubPart::text(other.to_string())],
    };
    let name = call_name(messages, &call_id);
    push_part(
        messages,
        HubRole::Tool,
        HubPart::ToolResult {
            call_id,
            name,
            content,
            is_error: false,
        },
    );
    Ok(())
}
