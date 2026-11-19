//! Native response-body formatting (JSON, XML, HTML, YAML) and JSONPath filtering.
//!
//! Single responsibility: turn a captured response body into the exact text the
//! workbench shows for one view format and evaluate JSONPath filters over it.
//!
//! JSON is handled by a strict RFC 8259 parser of our own ([`Json`]) instead of
//! `serde_json::Value`: it keeps object key order and duplicate keys, and keeps
//! number and string lexemes exactly as the server wrote them, so pretty-printing
//! and YAML conversion never rewrite `1.10`, `1e2`, big integers or escape
//! sequences. Nesting is capped at [`MAX_JSON_DEPTH`], which also bounds every
//! recursive walk over the tree.

mod bytes_view;
mod json_ast;
mod json_path;
mod markup;

#[cfg(test)]
mod tests;

use std::borrow::Cow;

use serde::{Deserialize, Serialize};

use json_ast::{Json, parse_document, write_block, write_pretty, write_yaml};
use json_path::{parse_json_path, select_json_path};
use markup::pretty_markup;

/// Deepest container nesting the JSON parser accepts.
pub(super) const MAX_JSON_DEPTH: usize = 128;
/// One indentation level for HTML/XML output.
pub(super) const INDENT: &str = "  ";
/// Mode label shown when a body is passed through because it is too large.
pub(super) const TOO_LARGE_MODE: &str = "Exact body · too large to format";
/// Error for any `[?…]` that is not `[?(@.path OP literal)]`, `[?(@ OP literal)]` or `[?(@.path)]`.
pub(super) const UNSUPPORTED_FILTER: &str =
    "Unsupported filter expression · use [?(@.path OP literal)] or [?(@.path)]";
/// Error for a quoted key or filter literal with an escape other than
/// `\\ \' \" \/ \b \f \n \r \t \uXXXX`.
pub(super) const INVALID_ESCAPE: &str = "Invalid escape in quoted string";
/// Most nodes any JSONPath working set may hold before the query is rejected.
pub(super) const MAX_JSON_PATH_NODES: usize = 100_000;

/// Response body view formats, mirroring the workbench format menu.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponseBodyFormat {
    /// Pretty-print JSON.
    Json,
    /// Indent XML.
    Xml,
    /// Indent HTML.
    Html,
    /// Convert JSON to YAML.
    Yaml,
    /// Hex dump of the first 64 KiB of the bytes.
    Hex,
    /// Base64 of the first 2 MiB of the bytes.
    Base64,
}

/// Result of formatting a response body for one view format.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FormattedBodyDto {
    /// Text to render and copy.
    pub text: String,
    /// Short status label shown next to the line count.
    pub mode: String,
    /// Whether the body parsed as JSON in this pass (JSON/YAML views only).
    pub is_valid_json: bool,
    /// Byte limit a Hex/Base64 view was cut at, so Copy can say it copied a prefix.
    #[serde(default)]
    pub truncated_at_bytes: Option<usize>,
    /// Whether the exact body is shown because it is over the format cap (`mode` is a label, not
    /// a contract).
    #[serde(default)]
    pub too_large: bool,
}

/// Outcome of one JSONPath filter over a response body.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "lowercase")]
pub enum JsonPathFilterOutcomeDto {
    /// Matches rendered as pretty JSON.
    Filtered {
        /// Pretty-printed match text.
        text: String,
        /// Short status label (e.g. `"3 JSONPath matches"`).
        mode: String,
    },
    /// The expression or the body was rejected.
    Error {
        /// Readable reason.
        message: String,
    },
}

/// Whether `text` is one valid JSON document (the strict parser the formatters use). The caller
/// passes a body the executor already captured, so its size is bounded by the body cap.
#[must_use]
pub fn is_valid_json(text: &str) -> bool {
    parse_document(text).is_some()
}

/// Formats raw body bytes for `format`. The text views decode the bytes as lossy UTF-8 and come
/// back unchanged above `max_bytes`; Hex and Base64 show the bytes themselves.
#[must_use]
pub fn format_response_bytes(
    body: &[u8],
    format: ResponseBodyFormat,
    max_bytes: usize,
) -> FormattedBodyDto {
    match format {
        ResponseBodyFormat::Hex => bytes_view::hex_view(body),
        ResponseBodyFormat::Base64 => bytes_view::base64_view(body),
        _ => format_response_body(&String::from_utf8_lossy(body), format, max_bytes),
    }
}

/// Formats `body_text` for `format`; text views of bodies over `max_bytes` (the response body
/// cap) come back unchanged. Hex and Base64 show the UTF-8 bytes of the text.
#[must_use]
pub fn format_response_body(
    body_text: &str,
    format: ResponseBodyFormat,
    max_bytes: usize,
) -> FormattedBodyDto {
    match format {
        ResponseBodyFormat::Hex => return bytes_view::hex_view(body_text.as_bytes()),
        ResponseBodyFormat::Base64 => return bytes_view::base64_view(body_text.as_bytes()),
        _ => {}
    }
    if body_text.len() > max_bytes {
        return too_large_body(body_text);
    }
    match format {
        ResponseBodyFormat::Json => format_json(body_text),
        ResponseBodyFormat::Xml => format_markup(body_text, false),
        ResponseBodyFormat::Html => format_markup(body_text, true),
        ResponseBodyFormat::Yaml => format_yaml(body_text),
        ResponseBodyFormat::Hex | ResponseBodyFormat::Base64 => exact_body(body_text, "Exact body"),
    }
}

/// Evaluates `expression` against the JSON in `body_text`, refusing bodies over `max_bytes`.
#[must_use]
pub fn filter_response_jsonpath(
    body_text: &str,
    expression: &str,
    max_bytes: usize,
) -> JsonPathFilterOutcomeDto {
    if body_text.len() > max_bytes {
        return JsonPathFilterOutcomeDto::Error {
            message: format!("Response body is too large to filter (limit {max_bytes} bytes)."),
        };
    }
    // The cheap expression check runs first so a typo never pays for the body parse.
    let segments = match parse_json_path(expression) {
        Ok(segments) => segments,
        Err(message) => return JsonPathFilterOutcomeDto::Error { message },
    };
    let Some(root) = parse_document(body_text) else {
        return JsonPathFilterOutcomeDto::Error {
            message: "Response body is not valid JSON.".to_owned(),
        };
    };

    match select_json_path(&root, &segments) {
        Ok(matches) => {
            let count = matches.len();
            JsonPathFilterOutcomeDto::Filtered {
                text: json_path_result_text(&matches),
                mode: match count {
                    0 => "JSONPath · no matches".to_owned(),
                    1 => "1 JSONPath match".to_owned(),
                    _ => format!("{count} JSONPath matches"),
                },
            }
        }
        Err(message) => JsonPathFilterOutcomeDto::Error { message },
    }
}

/// Resolves a saved path (assertion, Flow extraction, Mock rule) in the filter's dialect. The
/// legacy dot form is accepted too: no leading `$` (`user.name`, `[0].id`), an all-digit
/// segment as an array index (`items.0.id`) and an empty path for the root. Like the filter,
/// one match is the value itself and several are an array; no match is `Ok(None)`. Syntax the
/// dialect does not support is an error, never the whole body.
pub fn json_path_value<'a>(
    root: &'a serde_json::Value,
    path: &str,
) -> Result<Option<Cow<'a, serde_json::Value>>, String> {
    let path = path.trim();
    let normalized: Cow<'_, str> = if path.starts_with('$') {
        Cow::Borrowed(path)
    } else if path.is_empty() {
        Cow::Borrowed("$")
    } else if path.starts_with('[') {
        Cow::Owned(format!("${path}"))
    } else {
        Cow::Owned(format!("$.{path}"))
    };
    let segments = parse_json_path(&normalized)?;
    let mut matches = select_json_path(root, &segments)?;
    Ok(match matches.len() {
        0 => None,
        1 => matches.pop().map(Cow::Borrowed),
        _ => Some(Cow::Owned(serde_json::Value::Array(
            matches.into_iter().cloned().collect(),
        ))),
    })
}

/// Renders matches like Postman: one match unwrapped, otherwise the array.
fn json_path_result_text(matches: &[&Json<'_>]) -> String {
    let mut out = String::new();
    match matches {
        [single] => write_pretty(single, 0, &mut out),
        _ => write_block(&mut out, 0, ('[', ']'), matches, |item, out| {
            write_pretty(item, 1, out);
        }),
    }
    out
}

fn exact_body(body_text: &str, mode: &str) -> FormattedBodyDto {
    FormattedBodyDto {
        text: body_text.to_owned(),
        mode: mode.to_owned(),
        is_valid_json: false,
        truncated_at_bytes: None,
        too_large: false,
    }
}

/// The exact body, marked as too large to format (the only place `too_large` is `true`).
fn too_large_body(body_text: &str) -> FormattedBodyDto {
    FormattedBodyDto {
        too_large: true,
        ..exact_body(body_text, TOO_LARGE_MODE)
    }
}

fn format_json(body_text: &str) -> FormattedBodyDto {
    let Some(value) = parse_document(body_text) else {
        return exact_body(body_text, "Exact body");
    };
    let mut text = String::with_capacity(body_text.len() * 2);
    write_pretty(&value, 0, &mut text);
    FormattedBodyDto {
        text,
        mode: "Pretty printed".to_owned(),
        is_valid_json: true,
        truncated_at_bytes: None,
        too_large: false,
    }
}

fn format_markup(body_text: &str, html: bool) -> FormattedBodyDto {
    let formatted = pretty_markup(body_text, html);
    let mode = if formatted == body_text {
        "Exact body"
    } else {
        "Formatted"
    };
    FormattedBodyDto {
        text: formatted,
        mode: mode.to_owned(),
        is_valid_json: false,
        truncated_at_bytes: None,
        too_large: false,
    }
}

fn format_yaml(body_text: &str) -> FormattedBodyDto {
    let Some(value) = parse_document(body_text) else {
        return exact_body(body_text, "Exact body");
    };
    let mut text = String::with_capacity(body_text.len());
    write_yaml(&value, 0, &mut text, false);
    FormattedBodyDto {
        text,
        mode: "Converted from JSON".to_owned(),
        is_valid_json: true,
        truncated_at_bytes: None,
        too_large: false,
    }
}

/* ------------------------------------------------------------------ *
 * Lexical JSON tree: strict RFC 8259, document order, raw lexemes.
 * ------------------------------------------------------------------ */
