//! Suggests how a shell should display a response body by default.
//!
//! Single responsibility: the one implementation of "what is this body": the `Content-Type`
//! first, then a content sniff (including real JSON validity), so no UI re-implements detection.

use serde::{Deserialize, Serialize};

use super::types::ExecutionHeader;
use pingdeck_response::response_viewer::is_valid_json;

/// The view a shell should open for a response unless the user picks another.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SuggestedView {
    /// Pretty-printed JSON.
    Json,
    /// Indented XML.
    Xml,
    /// Indented HTML (a shell may offer a sandboxed preview too).
    Html,
    /// YAML text.
    Yaml,
    /// JavaScript source.
    Javascript,
    /// Markdown text.
    Markdown,
    /// An image: show it from the body bytes.
    Image,
    /// Plain text, shown exactly.
    #[default]
    Raw,
}

/// Picks the default view from the response headers and the full body text.
///
/// `Content-Type` decides when it names a known type (`json`, `xml`, `html`, `yaml`/`yml`,
/// `javascript`/`ecmascript`, `markdown`, `image/*`); otherwise a body that opens with `{` or `[`
/// is JSON only if it really parses, one that opens with `<` is XML, and anything else is raw.
#[must_use]
pub fn suggest_view(headers: &[ExecutionHeader], body_text: &str) -> SuggestedView {
    let content_type = headers
        .iter()
        .find(|header| header.key.eq_ignore_ascii_case("content-type"))
        .map(|header| header.value.to_ascii_lowercase());
    if let Some(content_type) = content_type.as_deref() {
        let named = [
            ("json", SuggestedView::Json),
            ("xml", SuggestedView::Xml),
            ("html", SuggestedView::Html),
            ("yaml", SuggestedView::Yaml),
            ("yml", SuggestedView::Yaml),
            ("javascript", SuggestedView::Javascript),
            ("ecmascript", SuggestedView::Javascript),
            ("markdown", SuggestedView::Markdown),
        ];
        if content_type.trim_start().starts_with("image/") {
            return SuggestedView::Image;
        }
        if let Some((_, view)) = named
            .iter()
            .find(|(needle, _)| content_type.contains(needle))
        {
            return *view;
        }
    }
    let trimmed = body_text.trim();
    if trimmed.starts_with('{') || trimmed.starts_with('[') {
        return if is_valid_json(trimmed) {
            SuggestedView::Json
        } else {
            SuggestedView::Raw
        };
    }
    if trimmed.starts_with('<') {
        return SuggestedView::Xml;
    }
    SuggestedView::Raw
}

#[cfg(test)]
mod tests {
    use super::*;

    fn headers(content_type: &str) -> Vec<ExecutionHeader> {
        vec![ExecutionHeader {
            key: "Content-Type".into(),
            value: content_type.into(),
        }]
    }

    #[test]
    fn content_type_wins_over_the_body() {
        assert_eq!(
            suggest_view(&headers("application/json; charset=utf-8"), "nope"),
            SuggestedView::Json
        );
        assert_eq!(
            suggest_view(&headers("text/html"), "{}"),
            SuggestedView::Html
        );
        assert_eq!(
            suggest_view(&headers("image/png"), ""),
            SuggestedView::Image
        );
        assert_eq!(
            suggest_view(&headers("application/x-yaml"), ""),
            SuggestedView::Yaml
        );
        assert_eq!(
            suggest_view(&headers("application/ecmascript"), ""),
            SuggestedView::Javascript
        );
        assert_eq!(
            suggest_view(&headers("text/markdown"), ""),
            SuggestedView::Markdown
        );
        assert_eq!(
            suggest_view(&headers("application/atom+xml"), ""),
            SuggestedView::Xml
        );
    }

    #[test]
    fn without_a_known_type_the_body_is_sniffed_and_json_must_parse() {
        let none: Vec<ExecutionHeader> = Vec::new();
        assert_eq!(suggest_view(&none, "  {\"a\":1}  "), SuggestedView::Json);
        assert_eq!(suggest_view(&none, "[1,2"), SuggestedView::Raw);
        assert_eq!(suggest_view(&none, "<a/>"), SuggestedView::Xml);
        assert_eq!(
            suggest_view(&headers("text/plain"), "hello"),
            SuggestedView::Raw
        );
        assert_eq!(
            suggest_view(&headers("text/plain"), "{\"a\":1}"),
            SuggestedView::Json
        );
    }
}
