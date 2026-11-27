//! Gemini JSON to hub: an upstream's response body and a client's `generateContent` request.

use serde_json::Value;

use super::grounding::{self, GROUNDING_CALL_ID, Grounding};
use super::{
    blocked, candidate_parts, config, decode_finish, decode_usage, error_message, field,
    first_candidate, function_response, read_signature, str_field,
};
use crate::llm::adapters::{InboundHint, WEB_SEARCH_TOOL_NAME};
use crate::llm::hub::{
    HubDocument, HubDocumentSource, HubFinish, HubImage, HubMessage, HubPart, HubRequest,
    HubResponse, HubRole,
};
use crate::llm::wire::Protocol;
use spanbay_base::error::AppError;

/// Part keys that only describe a part; a part with nothing else is skipped, not rejected.
const PART_METADATA: [&str; 5] = [
    "thought",
    "thoughtSignature",
    "thought_signature",
    "mediaResolution",
    "media_resolution",
];

/// Decodes an upstream's response body.
pub(super) fn response(body: &Value) -> Result<HubResponse, AppError> {
    if !body.is_object() {
        return Err(AppError::network("Gemini response is not a JSON object"));
    }
    if let Some(error) = body.get("error").filter(|e| e.is_object()) {
        return Err(AppError::network(error_message(error)));
    }
    let candidate = first_candidate(body);
    let (content, calls) = response_content(candidate)?;
    let finish = match candidate {
        Some(candidate) => decode_finish(
            field(candidate, "finishReason").and_then(Value::as_str),
            calls > 0,
        ),
        None if blocked(body) => HubFinish::ContentFilter,
        None => return Err(AppError::network("Gemini response has no candidates")),
    };
    Ok(HubResponse {
        id: str_field(body, "responseId"),
        model: str_field(body, "modelVersion"),
        content,
        finish,
        usage: field(body, "usageMetadata")
            .map(decode_usage)
            .unwrap_or_default(),
    })
}

/// Decodes a client's request body; the model comes from the URL path in `hint`.
pub(super) fn request(body: &Value, hint: &InboundHint) -> Result<HubRequest, AppError> {
    let model = hint
        .model
        .as_deref()
        .filter(|m| !m.is_empty())
        .ok_or_else(|| AppError::validation("Gemini request needs a model in the URL path"))?;
    if !body.is_object() {
        return Err(AppError::validation(
            "Gemini request body must be a JSON object",
        ));
    }
    let contents = field(body, "contents")
        .and_then(Value::as_array)
        .ok_or_else(|| AppError::validation("Gemini request needs a `contents` array"))?;
    let mut req = HubRequest {
        model: model.to_owned(),
        stream: hint.stream,
        messages: decode_contents(contents)?,
        system: field(body, "systemInstruction")
            .map(content_text)
            .filter(|s| !s.is_empty()),
        ..HubRequest::default()
    };
    if let Some(generation) = field(body, "generationConfig") {
        config::apply_generation_config(&mut req, generation);
    }
    (req.tools, req.native_tools) = config::decode_tools(body)?;
    req.tool_choice = config::decode_tool_choice(body);
    Ok(req)
}

/// The kinds of inline media Gemini carries.
#[derive(Clone, Copy)]
pub(super) enum Media {
    Image,
    Audio,
    Document,
}

impl Media {
    pub(super) fn name(self) -> &'static str {
        match self {
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Document => "document",
        }
    }
}

fn classify(mime: &str) -> Result<Media, String> {
    let base = mime
        .split(';')
        .next()
        .unwrap_or(mime)
        .trim()
        .to_ascii_lowercase();
    if base.starts_with("image/") {
        Ok(Media::Image)
    } else if base.starts_with("audio/") {
        Ok(Media::Audio)
    } else if base == "application/pdf" || base.starts_with("text/") {
        Ok(Media::Document)
    } else {
        Err(format!("media type `{mime}` is not supported"))
    }
}

/// The hub part of an `inlineData` or `fileData` part; `None` for any other part.
pub(super) fn media_part(part: &Value) -> Result<Option<HubPart>, String> {
    if let Some(inline) = field(part, "inlineData") {
        return inline_media(inline).map(Some);
    }
    field(part, "fileData").map(file_media).transpose()
}

fn inline_media(inline: &Value) -> Result<HubPart, String> {
    let mime = field(inline, "mimeType")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .ok_or("an inlineData part has no mimeType")?;
    let data = inline
        .get("data")
        .and_then(Value::as_str)
        .ok_or("an inlineData part has no data")?
        .to_owned();
    let media_type = mime.to_owned();
    Ok(match classify(mime)? {
        Media::Image => HubPart::Image {
            image: HubImage::Base64 { media_type, data },
        },
        Media::Audio => HubPart::Audio { media_type, data },
        Media::Document => HubPart::Document {
            document: HubDocument {
                media_type,
                name: None,
                source: HubDocumentSource::Base64 { data },
            },
        },
    })
}

fn file_media(file: &Value) -> Result<HubPart, String> {
    let uri = field(file, "fileUri")
        .and_then(Value::as_str)
        .filter(|u| !u.is_empty())
        .ok_or("a fileData part has no fileUri")?
        .to_owned();
    let mime = field(file, "mimeType")
        .and_then(Value::as_str)
        .filter(|m| !m.is_empty())
        .ok_or("a fileData part has no mimeType")?;
    Ok(match classify(mime)? {
        Media::Image => HubPart::Image {
            image: HubImage::Url { url: uri },
        },
        Media::Document => HubPart::Document {
            document: HubDocument {
                media_type: mime.to_owned(),
                name: None,
                source: HubDocumentSource::Url { url: uri },
            },
        },
        Media::Audio => return Err("audio fileData (a remote audio file) is not supported".into()),
    })
}

/// The hub part of a model or user part (not a `functionResponse`); `None` for an empty part.
///
/// `calls` counts the function calls so far: a call without an id is named `call_<count>`.
/// `from_client` says whether signatures may be another protocol's wrapped ones.
///
/// # Errors
///
/// The reason a part cannot be represented; callers add the context.
pub(super) fn model_part(
    part: &Value,
    calls: &mut u32,
    from_client: bool,
) -> Result<Option<HubPart>, String> {
    let (signature, origin) = read_signature(part, from_client);
    if let Some(call) = field(part, "functionCall") {
        let index = *calls;
        *calls += 1;
        let id = call
            .get("id")
            .and_then(Value::as_str)
            .filter(|id| !id.is_empty())
            .map_or_else(|| format!("call_{index}"), str::to_owned);
        return Ok(Some(HubPart::ToolCall {
            id,
            name: str_field(call, "name"),
            arguments: call
                .get("args")
                .map_or_else(|| "{}".to_owned(), Value::to_string),
            signature,
            origin,
        }));
    }
    if let Some(text) = part.get("text") {
        let text = text.as_str().ok_or("a text part is not a string")?;
        if text.is_empty() && signature.is_none() {
            return Ok(None);
        }
        if part.get("thought").and_then(Value::as_bool) == Some(true) {
            return Ok(Some(HubPart::Reasoning {
                text: text.to_owned(),
                signature,
                encrypted: None,
                id: None,
                origin,
            }));
        }
        return Ok(Some(HubPart::Text {
            text: text.to_owned(),
            signature,
            origin,
            citations: Vec::new(),
        }));
    }
    if let Some(media) = media_part(part)? {
        return Ok(Some(media));
    }
    match part.as_object().and_then(|map| {
        map.keys()
            .find(|key| !PART_METADATA.contains(&key.as_str()))
    }) {
        Some(kind) => Err(format!("part kind `{kind}` is not supported")),
        None => Ok(None),
    }
}

/// The parts of a response candidate with its grounding attached, and the number of calls.
fn response_content(candidate: Option<&Value>) -> Result<(Vec<HubPart>, u32), AppError> {
    let mut content = Vec::new();
    let mut calls = 0;
    for part in candidate_parts(candidate) {
        let part = model_part(part, &mut calls, false)
            .map_err(|message| AppError::network(format!("Gemini response: {message}")))?;
        content.extend(part);
    }
    if let Some(candidate) = candidate {
        attach_grounding(&mut content, grounding::decode(candidate));
    }
    Ok((content, calls))
}

/// Places grounding in the content: citations join the last text part (or a new empty one),
/// and the search becomes a web search call before the first text or tool call.
fn attach_grounding(content: &mut Vec<HubPart>, grounding: Grounding) {
    let input = grounding.search_input();
    if !grounding.citations.is_empty() {
        let last_text = content.iter_mut().rev().find_map(|part| match part {
            HubPart::Text { citations, .. } => Some(citations),
            _ => None,
        });
        match last_text {
            Some(citations) => citations.extend(grounding.citations),
            None => content.push(HubPart::Text {
                text: String::new(),
                signature: None,
                origin: None,
                citations: grounding.citations,
            }),
        }
    }
    if let Some(input) = input {
        let at = content
            .iter()
            .position(|part| matches!(part, HubPart::Text { .. } | HubPart::ToolCall { .. }))
            .unwrap_or(content.len());
        content.insert(
            at,
            HubPart::ServerToolUse {
                id: GROUNDING_CALL_ID.to_owned(),
                name: WEB_SEARCH_TOOL_NAME.to_owned(),
                input,
                origin: Some(Protocol::Gemini),
            },
        );
    }
}

/// The texts of a content's parts, joined by blank lines (the system instruction).
fn content_text(content: &Value) -> String {
    let texts: Vec<&str> = content
        .get("parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
        .filter_map(|part| part.get("text").and_then(Value::as_str))
        .collect();
    texts.join("\n\n")
}

fn unsupported(message: String) -> AppError {
    AppError::unsupported(format!("Gemini: {message}"))
}

/// A client's `contents` as hub messages.
///
/// The function responses of a turn become tool messages placed before the turn's other parts.
fn decode_contents(contents: &[Value]) -> Result<Vec<HubMessage>, AppError> {
    let mut messages = Vec::new();
    let mut calls = 0;
    // Calls not yet answered, as (id, name), oldest first.
    let mut pending: Vec<(String, String)> = Vec::new();
    for content in contents {
        let role = match content.get("role").and_then(Value::as_str) {
            Some("model") => HubRole::Assistant,
            _ => HubRole::User,
        };
        let mut regular = Vec::new();
        let mut results = Vec::new();
        for part in content
            .get("parts")
            .and_then(Value::as_array)
            .into_iter()
            .flatten()
        {
            if let Some(response) = field(part, "functionResponse") {
                results.push(function_response_message(
                    response,
                    &mut pending,
                    &mut calls,
                )?);
            } else if let Some(decoded) = model_part(part, &mut calls, true).map_err(unsupported)? {
                if let HubPart::ToolCall { id, name, .. } = &decoded {
                    pending.push((id.clone(), name.clone()));
                }
                regular.push(decoded);
            }
        }
        messages.extend(results);
        if !regular.is_empty() {
            messages.push(HubMessage {
                role,
                content: regular,
            });
        }
    }
    Ok(messages)
}

/// The tool message of one `functionResponse`.
fn function_response_message(
    response: &Value,
    pending: &mut Vec<(String, String)>,
    calls: &mut u32,
) -> Result<HubMessage, AppError> {
    let name = str_field(response, "name");
    let call_id = resolve_response_call_id(str_field(response, "id"), &name, pending, calls);
    let (content, is_error) = function_response::decode(response).map_err(unsupported)?;
    Ok(HubMessage {
        role: HubRole::Tool,
        content: vec![HubPart::ToolResult {
            call_id,
            name: Some(name),
            content,
            is_error,
        }],
    })
}

/// Pairs a function response with its call.
///
/// An explicit `id` wins and closes that call. Otherwise the response answers the most recent
/// open call of the same name, and with none open it gets the next synthetic `call_<n>` id.
fn resolve_response_call_id(
    id: String,
    name: &str,
    pending: &mut Vec<(String, String)>,
    calls: &mut u32,
) -> String {
    if !id.is_empty() {
        pending.retain(|(open, _)| *open != id);
        return id;
    }
    if let Some(position) = pending.iter().rposition(|(_, open)| open == name) {
        return pending.remove(position).0;
    }
    *calls += 1;
    format!("call_{}", *calls - 1)
}
