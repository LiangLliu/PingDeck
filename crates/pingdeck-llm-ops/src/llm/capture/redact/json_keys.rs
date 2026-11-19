//! R3 on free text: the value of every credential JSON key is replaced, found by a byte scan that
//! does not parse the JSON (bodies may be cut, elided or not JSON at all).

use pingdeck_base::secrets::{REDACT_JSON_KEYS, REDACTED};

fn skip_ws(bytes: &[u8], mut at: usize) -> usize {
    while bytes.get(at).is_some_and(u8::is_ascii_whitespace) {
        at += 1;
    }
    at
}

/// Whether the quote token (`"`, or `\"` inside an escaped JSON string) starts at `at`.
fn quote_at(bytes: &[u8], at: usize, escaped: bool) -> bool {
    if escaped {
        bytes.get(at) == Some(&b'\\') && bytes.get(at + 1) == Some(&b'"')
    } else {
        bytes.get(at) == Some(&b'"')
    }
}

/// Index of the quote token that closes a string whose content starts at `from`. A backslash
/// escapes the byte after it, so `\"` closes an escaped string but never a plain one.
fn string_end(bytes: &[u8], from: usize, escaped: bool) -> Option<usize> {
    let mut i = from;
    loop {
        let rest = bytes.get(i..)?;
        let at = if escaped {
            i + memchr::memchr(b'\\', rest)?
        } else {
            i + memchr::memchr2(b'"', b'\\', rest)?
        };
        if quote_at(bytes, at, escaped) {
            return Some(at);
        }
        i = at + 2;
    }
}

/// Index of the next quote token at or after `from`.
fn next_quote(bytes: &[u8], from: usize, escaped: bool) -> Option<usize> {
    let rest = bytes.get(from..)?;
    let found = if escaped {
        memchr::memmem::find(rest, b"\\\"")
    } else {
        memchr::memchr(b'"', rest)
    };
    found.map(|at| from + at)
}

/// Index just past a bare JSON value (a number) that starts at `from`.
fn number_end(bytes: &[u8], from: usize) -> usize {
    bytes[from..]
        .iter()
        .position(|&b| matches!(b, b',' | b'}' | b']') || b.is_ascii_whitespace())
        .map_or(bytes.len(), |at| from + at)
}

/// R3: replaces the value of every R3 key and adds one to `removed` per value. `escaped` scans
/// the content of a JSON string that itself holds JSON (`"arguments":"{\"password\":\"x\"}"`).
/// `None` when nothing changed.
pub(super) fn mask_json_keys(text: &str, escaped: bool, removed: &mut u32) -> Option<String> {
    let bytes = text.as_bytes();
    let quote = if escaped { "\\\"" } else { "\"" };
    let mut out = String::new();
    let mut copied = 0;
    let mut changed = false;
    let mut i = 0;
    while let Some(open) = next_quote(bytes, i, escaped) {
        let start = open + quote.len();
        let Some(end) = string_end(bytes, start, escaped) else {
            break;
        };
        let after = end + quote.len();
        let colon = skip_ws(bytes, after);
        let is_key = bytes.get(colon) == Some(&b':');
        if !is_key {
            if !escaped
                && text[start..end].contains("\\\"")
                && let Some(inner) = mask_json_keys(&text[start..end], true, removed)
            {
                out.push_str(&text[copied..start]);
                out.push_str(&inner);
                copied = end;
                changed = true;
            }
            i = after;
            continue;
        }
        i = colon + 1;
        if !REDACT_JSON_KEYS.contains(&&text[start..end]) {
            continue;
        }
        let value = skip_ws(bytes, i);
        if quote_at(bytes, value, escaped) {
            let value_start = value + quote.len();
            let Some(value_end) = string_end(bytes, value_start, escaped) else {
                break;
            };
            let inner = &text[value_start..value_end];
            if !inner.is_empty() && inner != REDACTED {
                out.push_str(&text[copied..value_start]);
                out.push_str(REDACTED);
                copied = value_end;
                changed = true;
                *removed += 1;
            }
            i = value_end + quote.len();
        } else if bytes
            .get(value)
            .is_some_and(|b| b.is_ascii_digit() || *b == b'-')
        {
            // A number becomes a string marker so the JSON stays valid.
            let end = number_end(bytes, value);
            out.push_str(&text[copied..value]);
            out.push_str(quote);
            out.push_str(REDACTED);
            out.push_str(quote);
            copied = end;
            changed = true;
            *removed += 1;
            i = end;
        } else {
            i = value;
        }
    }
    if !changed {
        return None;
    }
    out.push_str(&text[copied..]);
    Some(out)
}
