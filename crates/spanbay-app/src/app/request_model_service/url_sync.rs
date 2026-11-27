//! URL text ⇄ query-parameter rows ⇄ path variables (see the module docs for the rules).

use percent_encoding::{AsciiSet, NON_ALPHANUMERIC, percent_decode_str, utf8_percent_encode};
use serde::{Deserialize, Serialize};

use crate::workspace::KeyValue;

/// Which side the user edited in [`sync_request_url`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum UrlSyncEdit {
    /// The URL text changed: rows and path variables are re-derived from it.
    Url,
    /// The Params table changed: the URL query is rebuilt from the rows.
    Query,
}

/// The URL, Params table and path variables as they are after an edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlSyncInput {
    pub url: String,
    pub query_params: Vec<KeyValue>,
    pub path_variables: Vec<KeyValue>,
    pub edited: UrlSyncEdit,
}

/// The three synchronised pieces after an edit.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UrlSyncOutput {
    pub url: String,
    pub query_params: Vec<KeyValue>,
    pub path_variables: Vec<KeyValue>,
}

/// Byte length of the `{{name}}` token that starts at `bytes[at]`, if one does.
fn token_len(bytes: &[u8], at: usize) -> Option<usize> {
    if !bytes[at..].starts_with(b"{{") {
        return None;
    }
    let mut end = at + 2;
    while end < bytes.len() && bytes[end] != b'{' && bytes[end] != b'}' {
        end += 1;
    }
    (end > at + 2 && bytes[end..].starts_with(b"}}")).then_some(end + 2 - at)
}

/// Copies `input`, running `convert` on every stretch of text outside `{{…}}` tokens and
/// leaving the tokens verbatim.
pub(super) fn map_outside_tokens(
    input: &str,
    mut convert: impl FnMut(&str, &mut String),
) -> String {
    let bytes = input.as_bytes();
    let mut out = String::with_capacity(input.len());
    let (mut plain_start, mut index) = (0, 0);
    while index < bytes.len() {
        if let Some(len) = token_len(bytes, index) {
            convert(&input[plain_start..index], &mut out);
            out.push_str(&input[index..index + len]);
            index += len;
            plain_start = index;
        } else {
            index += 1;
        }
    }
    convert(&input[plain_start..], &mut out);
    out
}

/// Position of the first `separator` byte that is not inside a `{{…}}` token.
fn find_outside_tokens(input: &str, separator: u8) -> Option<usize> {
    let bytes = input.as_bytes();
    let mut index = 0;
    while index < bytes.len() {
        if let Some(len) = token_len(bytes, index) {
            index += len;
        } else if bytes[index] == separator {
            return Some(index);
        } else {
            index += 1;
        }
    }
    None
}

/// The characters `encodeURIComponent` leaves literal.
pub(crate) const URI_COMPONENT: &AsciiSet = &NON_ALPHANUMERIC
    .remove(b'-')
    .remove(b'_')
    .remove(b'.')
    .remove(b'!')
    .remove(b'~')
    .remove(b'*')
    .remove(b'\'')
    .remove(b'(')
    .remove(b')');

/// `encodeURIComponent` for the text outside tokens.
fn encode_component(input: &str) -> String {
    map_outside_tokens(input, |plain, out| {
        out.extend(utf8_percent_encode(plain, URI_COMPONENT));
    })
}

/// Form decoding (`+` is a space, `%XX` a byte, invalid UTF-8 becomes U+FFFD, malformed `%`
/// stays) for the text outside tokens.
fn decode_component(input: &str) -> String {
    map_outside_tokens(input, |plain, out| {
        out.push_str(&percent_decode_str(&plain.replace('+', " ")).decode_utf8_lossy());
    })
}

/// Whether a Params row is part of the URL text: enabled with a non-blank key.
fn is_url_row(row: &KeyValue) -> bool {
    row.enabled && !row.key.trim().is_empty()
}

/// Rebuilds the URL query from the enabled, non-blank-key rows, keeping the fragment.
#[must_use]
pub fn url_with_query_params(url: &str, params: &[KeyValue]) -> String {
    let (before_fragment, fragment) =
        find_outside_tokens(url, b'#').map_or((url, ""), |at| url.split_at(at));
    let base = find_outside_tokens(before_fragment, b'?')
        .map_or(before_fragment, |at| &before_fragment[..at]);
    let mut out = String::with_capacity(url.len());
    out.push_str(base);
    let mut separator = '?';
    for row in params.iter().filter(|row| is_url_row(row)) {
        out.push(separator);
        separator = '&';
        out.push_str(&encode_component(&row.key));
        if !row.value.is_empty() {
            out.push('=');
            out.push_str(&encode_component(&row.value));
        }
    }
    out.push_str(fragment);
    out
}

/// Parses the URL query into enabled rows; empty when the URL has no query.
#[must_use]
pub fn query_params_from_url(url: &str) -> Vec<KeyValue> {
    let before_fragment = find_outside_tokens(url, b'#').map_or(url, |at| &url[..at]);
    let Some(query_start) = find_outside_tokens(before_fragment, b'?') else {
        return Vec::new();
    };
    let mut rest = &before_fragment[query_start + 1..];
    let mut rows = Vec::new();
    while !rest.is_empty() {
        let (pair, tail) = match find_outside_tokens(rest, b'&') {
            Some(at) => (&rest[..at], &rest[at + 1..]),
            None => (rest, ""),
        };
        rest = tail;
        if pair.is_empty() {
            continue;
        }
        let (key, value) = match find_outside_tokens(pair, b'=') {
            Some(at) => (&pair[..at], &pair[at + 1..]),
            None => (pair, ""),
        };
        rows.push(KeyValue {
            key: decode_component(key),
            value: decode_component(value),
            enabled: true,
            description: None,
        });
    }
    rows
}

fn is_name_start(byte: u8) -> bool {
    byte.is_ascii_alphabetic() || byte == b'_'
}

/// Length of the `[A-Za-z_][A-Za-z0-9_]*` identifier that starts `bytes`, 0 if none.
fn identifier_len(bytes: &[u8]) -> usize {
    match bytes.first() {
        Some(&first) if is_name_start(first) => {
            1 + bytes[1..]
                .iter()
                .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'_')
                .count()
        }
        _ => 0,
    }
}

/// Length of the `[A-Za-z0-9_]+` run that starts `bytes`, 0 if none: the characters of a `:name`
/// path variable, exactly what the executor substitutes (a name may start with a digit).
fn colon_name_len(bytes: &[u8]) -> usize {
    bytes
        .iter()
        .take_while(|byte| byte.is_ascii_alphanumeric() || **byte == b'_')
        .count()
}

/// Lists the `:name` (`[A-Za-z0-9_]+`, right after a `/` or at the start) and `{name}` path
/// variables of the URL path, in URL order and without duplicates. The query, fragment and
/// `{{tokens}}` are ignored.
#[must_use]
pub fn path_variable_names(url: &str) -> Vec<String> {
    // The path ends at the first `?` or `#` that is not inside a `{{token}}`.
    let end = [
        find_outside_tokens(url, b'?'),
        find_outside_tokens(url, b'#'),
    ]
    .into_iter()
    .flatten()
    .min()
    .unwrap_or(url.len());
    let path = &url[..end];
    let bytes = path.as_bytes();
    let mut names: Vec<String> = Vec::new();
    let mut add = |name: &str| {
        if !names.iter().any(|existing| existing == name) {
            names.push(name.to_owned());
        }
    };
    let (mut index, mut after_slash) = (0, true);
    while index < bytes.len() {
        if let Some(len) = token_len(bytes, index) {
            index += len;
            after_slash = false;
            continue;
        }
        let byte = bytes[index];
        match byte {
            b':' if after_slash => {
                let len = colon_name_len(&bytes[index + 1..]);
                if len > 0 {
                    add(&path[index + 1..index + 1 + len]);
                }
            }
            b'{' => {
                let len = identifier_len(&bytes[index + 1..]);
                if len > 0 && bytes.get(index + 1 + len) == Some(&b'}') {
                    add(&path[index + 1..index + 1 + len]);
                }
            }
            _ => {}
        }
        after_slash = byte == b'/';
        index += 1;
    }
    names
}

/// One row per path variable of `url`, reusing the existing row (value, enabled flag) with the
/// same key and starting new names empty and enabled.
#[must_use]
pub fn merge_path_variables(url: &str, existing: &[KeyValue]) -> Vec<KeyValue> {
    path_variable_names(url)
        .into_iter()
        .map(|name| {
            existing
                .iter()
                .find(|row| row.key == name)
                .cloned()
                .unwrap_or(KeyValue {
                    key: name,
                    value: String::new(),
                    enabled: true,
                    description: None,
                })
        })
        .collect()
}

/// Reconciles the URL, Params table and path variables after one of them was edited.
///
/// `Url`: the text is left as typed; the rows become the URL's query pairs followed by the rows
/// the URL cannot hold (disabled, or blank key); the path variables follow the URL's names. The
/// n-th pair of a key takes the description of the n-th old row of that key. A blank URL keeps
/// every row and switches the URL rows off.
/// `Query`: the URL query is rebuilt from the rows; the path variables are returned as given.
#[must_use]
pub fn sync_request_url(input: UrlSyncInput) -> UrlSyncOutput {
    let UrlSyncInput {
        url,
        query_params,
        path_variables,
        edited,
    } = input;
    match edited {
        UrlSyncEdit::Url if url.trim().is_empty() => UrlSyncOutput {
            path_variables: merge_path_variables(&url, &path_variables),
            query_params: query_params
                .into_iter()
                .map(|mut row| {
                    row.enabled &= !is_url_row(&row);
                    row
                })
                .collect(),
            url,
        },
        UrlSyncEdit::Url => {
            let (mut notes, kept): (Vec<KeyValue>, Vec<KeyValue>) =
                query_params.into_iter().partition(is_url_row);
            let mut rows = query_params_from_url(&url);
            for row in &mut rows {
                if let Some(at) = notes.iter().position(|old| old.key == row.key) {
                    row.description = notes.remove(at).description;
                }
            }
            rows.extend(kept);
            UrlSyncOutput {
                path_variables: merge_path_variables(&url, &path_variables),
                query_params: rows,
                url,
            }
        }
        UrlSyncEdit::Query => UrlSyncOutput {
            url: url_with_query_params(&url, &query_params),
            query_params,
            path_variables,
        },
    }
}
