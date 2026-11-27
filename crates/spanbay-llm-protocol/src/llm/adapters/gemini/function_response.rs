//! `functionResponse` parts, both directions.
//!
//! A function response is a JSON object: the tool's text goes under `result` (`error` for a
//! failed call) and each inline medium is a sibling `parts[]` entry with a `displayName`
//! (`image_1`, `document_1`, `audio_1`, numbered per kind) that the object refers to as
//! `{"$ref": "<displayName>"}`. Decoding drops those references again and reads the media from
//! `parts`.

use serde_json::{Map, Value, json};

use super::decode::{Media, media_part};
use super::encode::{document_text, unresolved};
use super::field;
use crate::llm::adapters::object;
use crate::llm::hub::{HubDocumentSource, HubImage, HubPart};
use spanbay_base::error::AppError;

fn inline_only(kind: &str, url: &str) -> AppError {
    AppError::validation(format!(
        "Gemini function responses only accept inline {kind} data; the tool result \
         {kind} {url} must be fetched and sent as base64"
    ))
}

/// One inline media of a tool result: its kind (each kind numbered on its own), media type and
/// base64 data.
type InlineMedia<'a> = (Media, &'a str, &'a str);

/// The `functionResponse` part of a hub tool result.
pub(super) fn encode(
    call_id: &str,
    name: &str,
    content: &[HubPart],
    is_error: bool,
) -> Result<Value, AppError> {
    let (text, media) = collect_response_content(content)?;
    Ok(function_response_json(
        call_id, name, text, &media, is_error,
    ))
}

/// The text (documents as blocks, joined by newlines) and inline media of a tool result.
fn collect_response_content(
    content: &[HubPart],
) -> Result<(String, Vec<InlineMedia<'_>>), AppError> {
    let mut text = String::new();
    let mut media = Vec::new();
    for part in content {
        match part {
            HubPart::Text { text: more, .. } => text.push_str(more),
            HubPart::Image {
                image: HubImage::Base64 { media_type, data },
            } => media.push((Media::Image, media_type.as_str(), data.as_str())),
            HubPart::Image {
                image: HubImage::Url { url },
            } => return Err(inline_only("image", url)),
            HubPart::Image {
                image: HubImage::AttachmentRef { attachment_id, .. },
            } => return Err(unresolved("image", attachment_id)),
            HubPart::Document { document } => match &document.source {
                HubDocumentSource::Base64 { data } => {
                    media.push((Media::Document, &document.media_type, data));
                }
                HubDocumentSource::Text { text: body } => {
                    if !text.is_empty() {
                        text.push('\n');
                    }
                    text.push_str(&document_text(document, body));
                }
                HubDocumentSource::Url { url } => return Err(inline_only("document", url)),
                HubDocumentSource::AttachmentRef { attachment_id } => {
                    return Err(unresolved("document", attachment_id));
                }
            },
            HubPart::Audio { media_type, data } => {
                media.push((Media::Audio, media_type, data));
            }
            HubPart::ServerToolUse { .. } | HubPart::ServerToolResult { .. } => {}
            HubPart::ToolCall { .. } | HubPart::ToolResult { .. } | HubPart::Reasoning { .. } => {
                return Err(AppError::unsupported(
                    "Gemini: a function response can only carry text, images, documents and audio",
                ));
            }
        }
    }
    Ok((text, media))
}

fn function_response_json(
    call_id: &str,
    name: &str,
    text: String,
    media: &[InlineMedia<'_>],
    is_error: bool,
) -> Value {
    let mut response = Map::new();
    if !text.is_empty() || media.is_empty() || is_error {
        let key = if is_error { "error" } else { "result" };
        response.insert(key.into(), Value::String(text));
    }
    let (mut images, mut documents, mut audios) = (0_usize, 0_usize, 0_usize);
    let mut parts = Vec::with_capacity(media.len());
    for &(kind, mime, data) in media {
        let count = match kind {
            Media::Image => &mut images,
            Media::Document => &mut documents,
            Media::Audio => &mut audios,
        };
        *count += 1;
        let display_name = format!("{}_{count}", kind.name());
        parts.push(
            json!({"inlineData": {"mimeType": mime, "data": data, "displayName": display_name}}),
        );
        response.insert(
            display_name.clone(),
            object([("$ref", Value::String(display_name))]),
        );
    }
    let mut out = Map::new();
    if !call_id.is_empty() {
        out.insert("id".into(), call_id.into());
    }
    out.insert("name".into(), name.into());
    out.insert("response".into(), Value::Object(response));
    if !parts.is_empty() {
        out.insert("parts".into(), Value::Array(parts));
    }
    object([("functionResponse", Value::Object(out))])
}

/// Whether a response value is a `{"$ref": ...}` pointer to a sibling media part.
fn is_part_ref(value: &Value) -> bool {
    value
        .as_object()
        .is_some_and(|map| map.len() == 1 && map.contains_key("$ref"))
}

/// The text of a `functionResponse.response` and whether it reports an error.
///
/// Media references are dropped; a lone `result` or `error` is unwrapped, anything else is
/// sent as JSON text.
fn tool_result_text(response: Option<&Value>) -> (String, bool) {
    let render = |value: &Value| {
        value
            .as_str()
            .map_or_else(|| value.to_string(), str::to_owned)
    };
    let Some(object) = response.and_then(Value::as_object) else {
        return (response.map(render).unwrap_or_default(), false);
    };
    let kept = || object.iter().filter(|(_, value)| !is_part_ref(value));
    let mut entries = kept();
    match (entries.next(), entries.next()) {
        (None, _) => (if object.is_empty() { "{}" } else { "" }.to_owned(), false),
        (Some((key, value)), None) if key == "result" => (render(value), false),
        (Some((key, value)), None) if key == "error" => (render(value), true),
        _ => {
            let kept: Map<String, Value> = kept().map(|(k, v)| (k.clone(), v.clone())).collect();
            (Value::Object(kept).to_string(), false)
        }
    }
}

/// The content of a client's `functionResponse` and whether it reports an error.
///
/// # Errors
///
/// The reason a media part cannot be represented.
pub(super) fn decode(response: &Value) -> Result<(Vec<HubPart>, bool), String> {
    let (text, is_error) = tool_result_text(response.get("response"));
    let mut media = Vec::new();
    for part in field(response, "parts")
        .and_then(Value::as_array)
        .into_iter()
        .flatten()
    {
        media.push(
            media_part(part)?.ok_or("a functionResponse part must be inlineData or fileData")?,
        );
    }
    let mut content = Vec::new();
    if !text.is_empty() || media.is_empty() {
        content.push(HubPart::text(text));
    }
    content.extend(media);
    Ok((content, is_error))
}
