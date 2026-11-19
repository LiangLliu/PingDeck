//! Maps a `tools/call` or `resources/read` result to model content parts.
//!
//! Single responsibility: text blocks and embedded text resources become text parts (capped at
//! 100 KiB in total), `image` content becomes an image part (capped at 5 MiB each), everything
//! else a one-line text marker.

use serde_json::Value;

use super::McpCallResult;
use pingdeck_llm::llm::hub::{HubImage, HubPart};

/// Longest tool result text kept (100 KiB).
pub(super) const MAX_RESULT_BYTES: usize = 100 * 1024;
/// Largest decoded image kept (5 MiB).
pub(super) const MAX_IMAGE_BYTES: usize = 5 * 1024 * 1024;
/// Appended where the text cap cut a result.
const TRUNCATED: &str = "… [truncated]";

/// One block of a result before text blocks are merged.
enum Item {
    Text(String),
    Image(HubImage),
}

/// Renders a `tools/call` result: consecutive text blocks joined by newlines, images in place.
pub(super) fn parse_call_result(result: &Value) -> McpCallResult {
    let is_error = result
        .get("isError")
        .and_then(Value::as_bool)
        .unwrap_or(false);
    let content: &[Value] = result
        .get("content")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);
    let mut items: Vec<Item> = content.iter().map(item_of).collect();
    let has_text = content
        .iter()
        .any(|part| part.get("type").and_then(Value::as_str) == Some("text"));
    if !has_text
        && let Some(structured) = result
            .get("structuredContent")
            .filter(|value| !value.is_null())
    {
        items.push(Item::Text(structured.to_string()));
    }
    McpCallResult {
        parts: merge(items),
        is_error,
    }
}

/// Renders a `resources/read` result: each text content as text (after a line naming its URI
/// when there are several), an image blob as an image, any other blob as a marker.
pub(super) fn parse_resource_contents(result: &Value) -> McpCallResult {
    let contents: &[Value] = result
        .get("contents")
        .and_then(Value::as_array)
        .map_or(&[], Vec::as_slice);
    let several = contents.len() > 1;
    let items = contents
        .iter()
        .map(|content| {
            let field = |key: &str| content.get(key).and_then(Value::as_str);
            let uri = field("uri").unwrap_or("unknown");
            let media_type = field("mimeType");
            match (field("text"), field("blob")) {
                (Some(text), _) if several => Item::Text(format!("[{uri}]\n{text}")),
                (Some(text), _) => Item::Text(text.to_owned()),
                (None, Some(blob)) if media_type.is_some_and(|m| m.starts_with("image/")) => {
                    image_of(Some(blob), media_type)
                }
                (None, Some(blob)) => Item::Text(format!(
                    "[resource {uri} ({}), {} bytes of binary data]",
                    media_type.unwrap_or("unknown type"),
                    blob.len() / 4 * 3
                )),
                (None, None) => Item::Text(format!("[resource {uri}]")),
            }
        })
        .collect();
    McpCallResult {
        parts: merge(items),
        is_error: false,
    }
}

fn item_of(part: &Value) -> Item {
    let field = |key: &str| part.get(key).and_then(Value::as_str);
    match field("type").unwrap_or_default() {
        "text" => Item::Text(field("text").unwrap_or_default().to_owned()),
        "image" => image_of(field("data"), field("mimeType")),
        "audio" => Item::Text(format!(
            "[audio {}]",
            field("mimeType").unwrap_or("unknown")
        )),
        "resource" => {
            let resource = part.get("resource").unwrap_or(&Value::Null);
            let field = |key: &str| resource.get(key).and_then(Value::as_str);
            Item::Text(match field("text") {
                Some(text) => text.to_owned(),
                None => format!("[resource {}]", field("uri").unwrap_or("unknown")),
            })
        }
        "resource_link" => Item::Text(format!("[resource {}]", field("uri").unwrap_or("unknown"))),
        other => Item::Text(format!("[{other}]")),
    }
}

/// An image part, or a marker when the data is missing, is not an `image/*` type or is too big.
fn image_of(data: Option<&str>, media_type: Option<&str>) -> Item {
    let media_type = media_type.unwrap_or("unknown");
    let Some(data) = data.filter(|data| !data.is_empty()) else {
        return Item::Text(format!("[image {media_type}]"));
    };
    if !media_type.starts_with("image/") {
        return Item::Text(format!("[image {media_type}]"));
    }
    // Base64 expands 3 bytes to 4 characters.
    if data.len() / 4 * 3 > MAX_IMAGE_BYTES {
        return Item::Text(format!("[image {media_type} omitted: over 5 MiB]"));
    }
    Item::Image(HubImage::Base64 {
        media_type: media_type.to_owned(),
        data: data.to_owned(),
    })
}

/// Joins runs of text blocks with newlines and spends the shared 100 KiB text budget.
fn merge(items: Vec<Item>) -> Vec<HubPart> {
    let mut parts = Vec::new();
    let mut run: Vec<String> = Vec::new();
    let mut budget = MAX_RESULT_BYTES;
    let mut flush = |run: &mut Vec<String>, parts: &mut Vec<HubPart>| {
        if run.is_empty() {
            return;
        }
        let text = cap(run.join("\n"), &mut budget);
        run.clear();
        parts.push(HubPart::text(text));
    };
    for item in items {
        match item {
            Item::Text(text) => run.push(text),
            Item::Image(image) => {
                flush(&mut run, &mut parts);
                parts.push(HubPart::Image { image });
            }
        }
    }
    flush(&mut run, &mut parts);
    parts
}

/// Cuts `text` to the remaining `budget` on a character boundary, marks a cut and spends the budget.
fn cap(mut text: String, budget: &mut usize) -> String {
    if text.len() <= *budget {
        *budget -= text.len();
        return text;
    }
    let mut end = *budget;
    while !text.is_char_boundary(end) {
        end -= 1;
    }
    text.truncate(end);
    text.push_str(TRUNCATED);
    *budget = 0;
    text
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    fn text_of(result: &McpCallResult) -> String {
        result.text()
    }

    #[test]
    fn text_markers_and_embedded_resources() {
        let result = parse_call_result(&json!({
            "content": [
                {"type": "text", "text": "a"},
                {"type": "audio", "data": "", "mimeType": "audio/wav"},
                {"type": "resource", "resource": {"uri": "file:///x", "text": "inline"}},
                {"type": "resource", "resource": {"uri": "file:///y", "blob": "AA"}},
                {"type": "resource_link", "uri": "file:///z"},
                {"type": "weird"},
            ],
            "structuredContent": {"ignored": true},
            "isError": true,
        }));
        assert_eq!(
            result.parts,
            [HubPart::text(
                "a\n[audio audio/wav]\ninline\n[resource file:///y]\n[resource file:///z]\n[weird]"
            )]
        );
        assert!(result.is_error);
        assert!(parse_call_result(&json!({})).parts.is_empty());
    }

    #[test]
    fn structured_content_stands_in_only_when_there_is_no_text() {
        let result = parse_call_result(&json!({"content": [], "structuredContent": {"a": 1}}));
        assert_eq!(text_of(&result), "{\"a\":1}");
    }

    #[test]
    fn images_become_image_parts_in_place_and_split_text_runs() {
        let result = parse_call_result(&json!({
            "content": [
                {"type": "text", "text": "before"},
                {"type": "text", "text": "more"},
                {"type": "image", "data": "AAAA", "mimeType": "image/png"},
                {"type": "text", "text": "after"},
            ],
        }));
        assert_eq!(
            result.parts,
            [
                HubPart::text("before\nmore"),
                HubPart::Image {
                    image: HubImage::Base64 {
                        media_type: "image/png".to_owned(),
                        data: "AAAA".to_owned(),
                    }
                },
                HubPart::text("after"),
            ]
        );
        assert!(!result.is_error);
    }

    #[test]
    fn unusable_and_oversized_images_become_markers() {
        let big = "A".repeat((MAX_IMAGE_BYTES / 3 + 1) * 4);
        let edge = "A".repeat(MAX_IMAGE_BYTES / 3 * 4);
        let result = parse_call_result(&json!({
            "content": [
                {"type": "image", "mimeType": "image/png"},
                {"type": "image", "data": "AAAA", "mimeType": "text/html"},
                {"type": "image", "data": big, "mimeType": "image/jpeg"},
                {"type": "image", "data": edge, "mimeType": "image/webp"},
            ],
        }));
        assert_eq!(
            result.parts[0],
            HubPart::text(
                "[image image/png]\n[image text/html]\n[image image/jpeg omitted: over 5 MiB]"
            )
        );
        assert!(matches!(result.parts[1], HubPart::Image { .. }));
        assert_eq!(result.parts.len(), 2);
    }

    #[test]
    fn text_is_capped_at_100_kib_in_total_but_images_are_not_counted() {
        let half = "x".repeat(MAX_RESULT_BYTES - 10);
        let result = parse_call_result(&json!({
            "content": [
                {"type": "text", "text": half},
                {"type": "image", "data": "AAAA", "mimeType": "image/png"},
                {"type": "text", "text": "y".repeat(100)},
                {"type": "text", "text": "z"},
            ],
        }));
        let [first, image, last] = result.parts.as_slice() else {
            panic!("{:?}", result.parts);
        };
        assert_eq!(*first, HubPart::text("x".repeat(MAX_RESULT_BYTES - 10)));
        assert!(matches!(image, HubPart::Image { .. }));
        assert_eq!(
            *last,
            HubPart::text(format!("{}{TRUNCATED}", "y".repeat(10)))
        );
    }

    #[test]
    fn a_single_big_block_is_cut_exactly_at_the_cap() {
        let result = parse_call_result(&json!({
            "content": [{"type": "text", "text": "x".repeat(200_000)}],
        }));
        let text = text_of(&result);
        assert_eq!(text.len(), MAX_RESULT_BYTES + TRUNCATED.len());
        assert!(text.ends_with(TRUNCATED));
    }

    #[test]
    fn truncation_respects_char_boundaries() {
        let mut budget = MAX_RESULT_BYTES;
        let cut = cap("é".repeat(MAX_RESULT_BYTES), &mut budget);
        assert!(cut.ends_with(TRUNCATED));
        assert_eq!(cut.len(), MAX_RESULT_BYTES + TRUNCATED.len());
        assert_eq!(budget, 0);
        let mut budget = 10;
        assert_eq!(cap("short".to_owned(), &mut budget), "short");
        assert_eq!(budget, 5);
    }
}
