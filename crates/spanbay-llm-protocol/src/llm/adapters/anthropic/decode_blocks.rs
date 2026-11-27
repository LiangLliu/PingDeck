//! Content block readers shared by the Anthropic response, request and stream decoders.

use serde_json::{Value, json};

use super::OWN;
use crate::llm::adapters::{WEB_SEARCH_TOOL_NAME, str_of, str_or_empty};
use crate::llm::hub::{HubCitation, HubDocument, HubDocumentSource, HubImage, HubPart};
use spanbay_base::error::AppError;

/// Decodes the blocks that may also appear inside a `tool_result`: text (`None` when empty),
/// image and document. Any other type is unsupported.
pub(super) fn decode_simple_part(block: &Value) -> Result<Option<HubPart>, AppError> {
    match str_of(block, "type") {
        Some("text") => {
            Ok(str_of(block, "text")
                .filter(|t| !t.is_empty())
                .map(|text| HubPart::Text {
                    text: text.to_owned(),
                    signature: None,
                    origin: None,
                    citations: block
                        .get("citations")
                        .and_then(Value::as_array)
                        .map(|items| items.iter().filter_map(citation).collect())
                        .unwrap_or_default(),
                }))
        }
        Some("image") => decode_image(block).map(Some),
        Some("document") => decode_document(block).map(Some),
        other => Err(AppError::unsupported(format!(
            "anthropic: content block type `{}` is not supported",
            other.unwrap_or("missing")
        ))),
    }
}

fn decode_image(block: &Value) -> Result<HubPart, AppError> {
    let source = block.get("source").unwrap_or(&Value::Null);
    let image = match str_of(source, "type") {
        Some("base64") => HubImage::Base64 {
            media_type: str_of(source, "media_type")
                .unwrap_or("image/png")
                .to_owned(),
            data: str_or_empty(source, "data"),
        },
        Some("url") => HubImage::Url {
            url: str_or_empty(source, "url"),
        },
        other => {
            return Err(AppError::validation(format!(
                "unsupported image source `{}`",
                other.unwrap_or("missing")
            )));
        }
    };
    Ok(HubPart::Image { image })
}

fn decode_document(block: &Value) -> Result<HubPart, AppError> {
    let source = block.get("source").unwrap_or(&Value::Null);
    let (media_type, source) = match str_of(source, "type") {
        Some("base64") => (
            str_of(source, "media_type").unwrap_or("application/pdf"),
            HubDocumentSource::Base64 {
                data: str_or_empty(source, "data"),
            },
        ),
        Some("text") => (
            str_of(source, "media_type").unwrap_or("text/plain"),
            HubDocumentSource::Text {
                text: str_or_empty(source, "data"),
            },
        ),
        // Anthropic's URL documents are PDFs, and the source carries no media type.
        Some("url") => (
            "application/pdf",
            HubDocumentSource::Url {
                url: str_or_empty(source, "url"),
            },
        ),
        other => {
            return Err(AppError::unsupported(format!(
                "anthropic: document source `{}` is not supported",
                other.unwrap_or("missing")
            )));
        }
    };
    Ok(HubPart::Document {
        document: HubDocument {
            media_type: media_type.to_owned(),
            name: str_of(block, "title")
                .filter(|title| !title.is_empty())
                .map(str::to_owned),
            source,
        },
    })
}

pub(super) fn server_tool_use(block: &Value) -> HubPart {
    HubPart::ServerToolUse {
        id: str_or_empty(block, "id"),
        name: str_or_empty(block, "name"),
        input: block.get("input").cloned().unwrap_or_else(|| json!({})),
        origin: Some(OWN),
    }
}

pub(super) fn server_tool_result(block: &Value) -> HubPart {
    let payload = block.get("content").cloned().unwrap_or_else(|| json!([]));
    let citations = search_hits(&payload);
    HubPart::ServerToolResult {
        call_id: str_or_empty(block, "tool_use_id"),
        name: WEB_SEARCH_TOOL_NAME.to_owned(),
        payload,
        citations,
        origin: Some(OWN),
    }
}

/// The `web_search_result` items of a search payload as citations.
pub(super) fn search_hits(payload: &Value) -> Vec<HubCitation> {
    payload
        .as_array()
        .into_iter()
        .flatten()
        .filter(|item| str_of(item, "type") == Some("web_search_result"))
        .map(|item| HubCitation {
            url: str_or_empty(item, "url"),
            title: str_or_empty(item, "title"),
            snippet: None,
            start_index: None,
            end_index: None,
        })
        .collect()
}

/// A text citation; `None` for a location type the hub does not model.
pub(super) fn citation(value: &Value) -> Option<HubCitation> {
    let (url, title) = match str_of(value, "type")? {
        "web_search_result_location" => (str_of(value, "url"), str_of(value, "title")),
        "search_result_location" => (str_of(value, "source"), str_of(value, "title")),
        "char_location" | "page_location" | "content_block_location" => {
            (None, str_of(value, "document_title"))
        }
        _ => return None,
    };
    Some(HubCitation {
        url: url.unwrap_or_default().to_owned(),
        title: title.unwrap_or_default().to_owned(),
        snippet: str_of(value, "cited_text")
            .filter(|t| !t.is_empty())
            .map(str::to_owned),
        start_index: None,
        end_index: None,
    })
}
