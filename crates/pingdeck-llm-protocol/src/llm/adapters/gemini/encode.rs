//! Hub to Gemini JSON: the `generateContent` request toward an upstream and the response body
//! toward a Gemini client.

use serde_json::{Map, Value, json};

use super::{
    DUMMY_THOUGHT_SIGNATURE, candidate, candidates_body, client_signature, config, encode_finish,
    encode_usage, function_call_part, function_response, grounding, inline_data_part, text_part,
    thought_part,
};
use crate::llm::adapters::{EncodeCtx, WEB_SEARCH_TOOL_NAME, object};
use crate::llm::hub::{
    HubCitation, HubDocument, HubDocumentSource, HubImage, HubMessage, HubPart, HubRequest,
    HubResponse, HubRole, signature_usable,
};
use crate::llm::wire::{Protocol, ReasoningStyle};
use pingdeck_base::error::AppError;

/// The `generateContent` request body of a hub request.
pub(super) fn request(req: &HubRequest, ctx: &EncodeCtx<'_>) -> Result<Value, AppError> {
    if req.ends_with_assistant() {
        return Err(AppError::validation(
            "Gemini does not accept a request that ends with a model turn (prefill); end the \
             conversation with a user or tool message",
        ));
    }
    let mut body = Map::new();
    if let Some(system) = req.system.as_deref().filter(|s| !s.is_empty()) {
        body.insert(
            "systemInstruction".into(),
            json!({"parts": [{"text": system}]}),
        );
    }
    body.insert("contents".into(), Value::Array(contents(&req.messages)?));
    let mut generation = config::generation_config(req, ctx.quirks.drops_sampling());
    if ctx.quirks.reasoning_request == Some(ReasoningStyle::None) {
        generation.remove("thinkingConfig");
    }
    if !generation.is_empty() {
        body.insert("generationConfig".into(), Value::Object(generation));
    }
    let tools = config::encode_tools(&req.tools, &req.native_tools)?;
    if !tools.is_empty() {
        body.insert("tools".into(), Value::Array(tools));
    }
    if let Some(choice) = &req.tool_choice {
        body.insert("toolConfig".into(), config::tool_config(choice));
    }
    Ok(Value::Object(body))
}

/// The response body toward a Gemini client.
pub(super) fn response(response: &HubResponse) -> Value {
    let (parts, grounding) = response_parts(&response.content);
    candidates_body(
        candidate(parts, Some(encode_finish(&response.finish)), grounding),
        encode_usage(&response.usage),
        &response.model,
        &response.id,
    )
}

/// `contents`: consecutive messages of the same role merge into one turn (tool results are
/// `user` turns), and each model turn's first call is signed (see [`sign_first_call`]).
fn contents(messages: &[HubMessage]) -> Result<Vec<Value>, AppError> {
    let mut turns: Vec<(&str, Vec<Value>)> = Vec::new();
    let mut calls: Vec<(&str, &str)> = Vec::new();
    for message in messages {
        let role = if message.role == HubRole::Assistant {
            "model"
        } else {
            "user"
        };
        let mut parts = Vec::new();
        for part in &message.content {
            if let HubPart::ToolCall { id, name, .. } = part {
                calls.push((id, name));
            }
            parts.extend(encode_part(part, &calls)?);
        }
        if parts.is_empty() {
            continue;
        }
        match turns.last_mut() {
            Some((last, existing)) if *last == role => existing.extend(parts),
            _ => turns.push((role, parts)),
        }
    }
    Ok(turns
        .into_iter()
        .map(|(role, mut parts)| {
            if role == "model" {
                sign_first_call(&mut parts);
            }
            object([("role", role.into()), ("parts", Value::Array(parts))])
        })
        .collect())
}

/// Gives the turn's first `functionCall` the [`DUMMY_THOUGHT_SIGNATURE`] unless it is signed.
fn sign_first_call(parts: &mut [Value]) {
    if let Some(part) = parts
        .iter_mut()
        .find(|part| part.get("functionCall").is_some())
        .and_then(Value::as_object_mut)
    {
        part.entry("thoughtSignature")
            .or_insert_with(|| DUMMY_THOUGHT_SIGNATURE.into());
    }
}

/// A signature Gemini can verify: only one Gemini made (or one of unknown origin).
fn request_signature(signature: Option<&str>, origin: Option<Protocol>) -> Option<&str> {
    signature.filter(|_| signature_usable(origin, Protocol::Gemini))
}

/// The Gemini part of one hub part; `None` for parts Gemini has no request form for.
fn encode_part(part: &HubPart, calls: &[(&str, &str)]) -> Result<Option<Value>, AppError> {
    Ok(match part {
        HubPart::Text {
            text,
            signature,
            origin,
            ..
        } => {
            let signature = request_signature(signature.as_deref(), *origin);
            (!text.is_empty() || signature.is_some()).then(|| text_part(text, signature))
        }
        HubPart::Image {
            image: HubImage::Base64 { media_type, data },
        } => Some(inline_data_part(media_type, data)),
        HubPart::Image {
            image: HubImage::Url { url },
        } => Some(json!({"fileData": {"mimeType": guess_mime(url), "fileUri": url}})),
        HubPart::Image {
            image: HubImage::AttachmentRef { attachment_id, .. },
        } => return Err(unresolved("image", attachment_id)),
        HubPart::Document { document } => Some(document_part(document)?),
        HubPart::Audio { media_type, data } => Some(inline_data_part(media_type, data)),
        HubPart::ServerToolUse { .. } | HubPart::ServerToolResult { .. } => None,
        HubPart::ToolCall {
            id,
            name,
            arguments,
            signature,
            origin,
        } => Some(function_call_part(
            id,
            name,
            arguments,
            request_signature(signature.as_deref(), *origin),
        )),
        HubPart::ToolResult {
            call_id,
            name,
            content,
            is_error,
        } => {
            // A result without a name takes the name of the latest call with its id.
            let name = name.as_deref().unwrap_or_else(|| {
                calls
                    .iter()
                    .rev()
                    .find(|(id, _)| id == call_id)
                    .map_or("", |(_, name)| name)
            });
            Some(function_response::encode(
                call_id, name, content, *is_error,
            )?)
        }
        HubPart::Reasoning {
            text,
            signature: Some(signature),
            origin,
            ..
        } if !text.is_empty() && signature_usable(*origin, Protocol::Gemini) => {
            Some(thought_part(text, Some(signature)))
        }
        HubPart::Reasoning { .. } => None,
    })
}

/// A document: inline bytes, a file URI, or text wrapped in a `<document>` block.
fn document_part(document: &HubDocument) -> Result<Value, AppError> {
    Ok(match &document.source {
        HubDocumentSource::Base64 { data } => inline_data_part(&document.media_type, data),
        HubDocumentSource::Url { url } => {
            json!({"fileData": {"mimeType": document.media_type, "fileUri": url}})
        }
        HubDocumentSource::Text { text } => text_part(&document_text(document, text), None),
        HubDocumentSource::AttachmentRef { attachment_id } => {
            return Err(unresolved("document", attachment_id));
        }
    })
}

/// The media type of an image URL by its extension.
///
/// JPEG is the fallback: it is the most common photo format and the URL usually names none.
fn guess_mime(url: &str) -> &'static str {
    let path = url.split(['?', '#']).next().unwrap_or(url);
    match path
        .rsplit('.')
        .next()
        .unwrap_or_default()
        .to_ascii_lowercase()
        .as_str()
    {
        "png" => "image/png",
        "webp" => "image/webp",
        "gif" => "image/gif",
        "heic" => "image/heic",
        "heif" => "image/heif",
        _ => "image/jpeg",
    }
}

/// Escapes text for a double-quoted attribute of the `<document>` block.
fn attribute(text: &str) -> String {
    text.replace('&', "&amp;").replace('"', "&quot;")
}

/// A text document as a `<document name=".." type="..">` block; the name is left out when
/// unknown.
pub(super) fn document_text(document: &HubDocument, text: &str) -> String {
    let name = document
        .name
        .as_deref()
        .map(|name| format!(" name=\"{}\"", attribute(name)))
        .unwrap_or_default();
    format!(
        "<document{name} type=\"{}\">\n{text}\n</document>",
        attribute(&document.media_type)
    )
}

pub(super) fn unresolved(kind: &str, attachment_id: &str) -> AppError {
    AppError::validation(format!(
        "{kind} attachment {attachment_id} must be resolved before the request is encoded"
    ))
}

/// The response part toward a client; `None` for parts with no Gemini response form.
fn response_part(part: &HubPart) -> Option<Value> {
    match part {
        HubPart::Text {
            text,
            signature,
            origin,
            ..
        } => Some(text_part(
            text,
            client_signature(signature.as_deref(), *origin).as_deref(),
        )),
        HubPart::Reasoning {
            text,
            signature,
            origin,
            ..
        } => (!text.is_empty() || signature.is_some()).then(|| {
            thought_part(
                text,
                client_signature(signature.as_deref(), *origin).as_deref(),
            )
        }),
        HubPart::ToolCall {
            id,
            name,
            arguments,
            signature,
            origin,
        } => Some(function_call_part(
            id,
            name,
            arguments,
            client_signature(signature.as_deref(), *origin).as_deref(),
        )),
        HubPart::Image {
            image: HubImage::Base64 { media_type, data },
        }
        | HubPart::Audio { media_type, data }
        | HubPart::Document {
            document:
                HubDocument {
                    media_type,
                    source: HubDocumentSource::Base64 { data },
                    ..
                },
        } => Some(inline_data_part(media_type, data)),
        _ => None,
    }
}

/// The response parts and their `groundingMetadata`.
///
/// A text part's citations become supports pointing at that part's index; search results add
/// sources without a part, and web search calls add their queries.
fn response_parts(content: &[HubPart]) -> (Vec<Value>, Option<Value>) {
    let mut parts = Vec::new();
    let mut sources: Vec<(Option<usize>, &HubCitation)> = Vec::new();
    let mut queries = Vec::new();
    for part in content {
        match part {
            HubPart::Text { citations, .. } => {
                let index = parts.len();
                sources.extend(citations.iter().map(|citation| (Some(index), citation)));
            }
            HubPart::ServerToolResult { citations, .. } => {
                sources.extend(citations.iter().map(|citation| (None, citation)));
            }
            HubPart::ServerToolUse { name, input, .. } if name == WEB_SEARCH_TOOL_NAME => {
                queries.extend(grounding::search_queries(input));
            }
            _ => {}
        }
        parts.extend(response_part(part));
    }
    (parts, grounding::encode(sources, &queries))
}
