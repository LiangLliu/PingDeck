//! Reading, editing and naming the users' JSON (JSONC) and TOML config files.

use std::path::Path;

use jsonc_parser::cst::{CstInputValue, CstObject, CstRootNode};
use serde_json::{Map, Value};

use pingdeck_base::{error::AppError, platform};

use super::fsio::read_optional;

/// `path` as the user sees it (normalized separators).
pub(super) fn display(path: &Path) -> String {
    platform::normalize_display_path(path)
}

/// `path` as `~/rel` when it lies under `home` (the form the unparsable-file message uses), else
/// absolute.
pub(super) fn display_in_home(home: &Path, path: &Path) -> String {
    match path.strip_prefix(home) {
        Ok(rel) => format!(
            "~/{}",
            rel.components()
                .map(|c| c.as_os_str().to_string_lossy())
                .collect::<Vec<_>>()
                .join("/")
        ),
        Err(_) => display(path),
    }
}

/// The JSON dialect the tools' JSON files may use: comments and trailing commas (JSONC, as
/// Gemini CLI and OpenCode accept), nothing looser.
fn jsonc_options() -> jsonc_parser::ParseOptions {
    jsonc_parser::ParseOptions {
        allow_comments: true,
        allow_loose_object_property_names: false,
        allow_trailing_commas: true,
        allow_missing_commas: false,
        allow_single_quoted_strings: false,
        allow_hexadecimal_numbers: false,
        allow_unary_plus_numbers: false,
        allow_bare_decimal_point_numbers: false,
        allow_non_finite_numbers: false,
        allow_extended_string_escapes: false,
    }
}

/// The message for a file that cannot be parsed (`kind` is `JSON` or `TOML`).
fn unparsable(home: &Path, path: &Path, kind: &str, line: usize) -> AppError {
    AppError::validation(format!(
        "`{}` is not valid {kind} at line {line}. Fix the file, then try again. PingDeck never overwrites a file it cannot read.",
        display_in_home(home, path),
    ))
}

/// Parses `text` as a JSON object (blank means `{}`); comments and trailing commas are allowed.
pub(super) fn parse_json_object(
    home: &Path,
    path: &Path,
    text: &str,
) -> Result<Map<String, Value>, AppError> {
    if text.trim().is_empty() {
        return Ok(Map::new());
    }
    match jsonc_parser::parse_to_serde_value::<Value>(text, &jsonc_options()) {
        Ok(Value::Object(map)) => Ok(map),
        Ok(_) => Err(AppError::validation(format!(
            "{} must contain a JSON object",
            display(path)
        ))),
        Err(e) => Err(unparsable(home, path, "JSON", e.line_display())),
    }
}

/// Reads `path` and parses it as a JSON object; `None` when the file does not exist.
///
/// # Errors
/// As [`read_optional`] and [`parse_json_object`].
pub(super) fn read_json_object(
    home: &Path,
    path: &Path,
) -> Result<Option<Map<String, Value>>, AppError> {
    read_optional(path)?
        .map(|text| parse_json_object(home, path, &text))
        .transpose()
}

/// Parses `text` as TOML, keeping comments and order for the edit.
pub(super) fn parse_toml(
    home: &Path,
    path: &Path,
    text: &str,
) -> Result<toml_edit::DocumentMut, AppError> {
    text.parse().map_err(|e: toml_edit::TomlError| {
        // Counted in bytes: a span off a char boundary cannot panic, and `\n` never occurs
        // inside a multi-byte sequence, so the count matches a char-based one.
        let line = e.span().map_or(1, |span| {
            let end = span.start.min(text.len());
            memchr::memchr_iter(b'\n', &text.as_bytes()[..end]).count() + 1
        });
        unparsable(home, path, "TOML", line)
    })
}

/// Parses `before` as a JSON object (blank or missing means `{}`), lets `edit` modify it and
/// writes the result back. An existing file is edited in place: only the values that changed are
/// replaced, so the user's key order, comments, indentation and trailing commas stay; a new (or
/// blank) file is written pretty-printed with a trailing newline. When the edit leaves the parsed
/// content as it was, `before` is returned untouched.
pub(super) fn edit_json(
    home: &Path,
    path: &Path,
    before: Option<&str>,
    edit: impl FnOnce(&mut Map<String, Value>) -> Result<(), AppError>,
) -> Result<String, AppError> {
    let mut map = parse_json_object(home, path, before.unwrap_or(""))?;
    let original = map.clone();
    edit(&mut map)?;
    let Some(before) = before.filter(|before| !before.trim().is_empty()) else {
        let mut text = serde_json::to_string_pretty(&Value::Object(map))
            .map_err(|e| AppError::internal(format!("failed to serialise JSON: {e}")))?;
        text.push('\n');
        return Ok(text);
    };
    if map == original {
        return Ok(before.to_owned());
    }
    let root = CstRootNode::parse(before, &jsonc_options())
        .map_err(|e| unparsable(home, path, "JSON", e.line_display()))?;
    let object = root.object_value().ok_or_else(|| {
        AppError::validation(format!("{} must contain a JSON object", display(path)))
    })?;
    patch_object(&object, &original, &map);
    Ok(root.to_string())
}

/// Turns `from` into `to` inside `object`: removed keys go, changed values are replaced (an object
/// that stays an object is patched key by key, keeping its layout), new keys are appended.
fn patch_object(object: &CstObject, from: &Map<String, Value>, to: &Map<String, Value>) {
    for key in from.keys().filter(|key| !to.contains_key(*key)) {
        if let Some(prop) = object.get(key) {
            prop.remove();
        }
    }
    for (key, value) in to {
        match (from.get(key), object.get(key)) {
            (Some(old), _) if old == value => {}
            (Some(Value::Object(old)), Some(prop)) => match (value, prop.object_value()) {
                (Value::Object(new), Some(child)) => patch_object(&child, old, new),
                _ => prop.set_value(cst_value(value)),
            },
            (_, Some(prop)) => prop.set_value(cst_value(value)),
            (_, None) => {
                object.append(key, cst_value(value));
            }
        }
    }
}

/// A JSON value as the CST editor inserts it.
fn cst_value(value: &Value) -> CstInputValue {
    match value {
        Value::Null => CstInputValue::Null,
        Value::Bool(flag) => CstInputValue::Bool(*flag),
        Value::Number(number) => CstInputValue::Number(number.to_string()),
        Value::String(text) => CstInputValue::String(text.clone()),
        Value::Array(items) => CstInputValue::Array(items.iter().map(cst_value).collect()),
        Value::Object(map) => CstInputValue::Object(
            map.iter()
                .map(|(key, value)| (key.clone(), cst_value(value)))
                .collect(),
        ),
    }
}

/// Returns `map[key]` as an object, creating it when absent.
pub(super) fn child_object<'a>(
    map: &'a mut Map<String, Value>,
    key: &str,
    path: &Path,
) -> Result<&'a mut Map<String, Value>, AppError> {
    map.entry(key)
        .or_insert_with(|| Value::Object(Map::new()))
        .as_object_mut()
        .ok_or_else(|| {
            AppError::validation(format!(
                "`{key}` in {} must be a JSON object",
                display(path)
            ))
        })
}
