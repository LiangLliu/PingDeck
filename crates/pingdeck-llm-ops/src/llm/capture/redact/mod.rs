//! Redaction of a raw exchange before it is stored, on the log-writer thread.
//!
//! [`pingdeck_base::secrets::redact_secrets`] (text patterns R4-R10 and the exact-value needles,
//! floor 8 characters) is composed with the structural rules only a capture has: header names
//! (R1), URL userinfo, query values (R2) and path segments (R10 via
//! [`pingdeck_base::secrets::redact_url_path`]) and JSON object keys (R3). Order per text: redact
//! the full text, then elide base64, then cut to the caps. A secret split across stream frames is
//! removed from every frame it spans (`split_frames`).
//!
//! Masking: header, URL-query and `Authorization: Bearer` values keep their last four characters
//! from 12 characters on (`…3f9a`) and a recognisable prefix too from 24 on (`sk-…3f9a`), else
//! become the redaction marker. Body and frame text always gets the whole marker, so the number of
//! removed values stays derivable from the stored text ([`super::replay_eligibility`]).

mod json_keys;
mod split_frames;
mod url;

use std::borrow::Cow;

use serde_json::Value;

use super::bound::{
    BoundedBody, MAX_BODY_BYTES, MAX_FRAME_BYTES, MAX_FRAMES, cut_text, elide_text,
};
use super::model::{
    CapturedBody, CapturedExchange, CapturedFrames, RawExchange, RedactedHeader, TranslationNote,
};
use json_keys::mask_json_keys;
use pingdeck_base::secrets::{
    ELIDED, REDACT_QUERY_KEYS, REDACTED, is_secret_header_name, mask_secret_hint, redact_json,
    redact_secrets,
};
use split_frames::redact_split_frames;

pub(in crate::llm::capture) use url::{redact_url, userinfo_at};

/// R1 headers whose value is never partially shown.
const FULLY_HIDDEN_HEADERS: [&str; 2] = ["cookie", "set-cookie"];

/// The masked form of a secret value, in the tiers of [`mask_secret_hint`]: the redaction marker
/// below 12 characters, the last four (`…3f9a`) below 24, the first three plus the last four
/// (`sk-…3f9a`) from 24 on, so a stored capture never shows a large share of a short secret.
#[must_use]
pub(in crate::llm::capture) fn mask_value(value: &str) -> String {
    if value.trim().chars().count() < 12 {
        return REDACTED.to_owned();
    }
    mask_secret_hint(value)
}

/// Redacts free text: JSON keys (R3), then `redact_secrets` (patterns R4-R8 and R10, needles
/// R9). Returns the text and how many values were removed or elided.
#[must_use]
pub fn redact_text(text: &str, needles: &[String]) -> (String, u32) {
    let mut removed = 0;
    let keyed = mask_json_keys(text, false, &mut removed);
    let text = keyed.as_deref().unwrap_or(text);
    let masked = redact_secrets(text, needles);
    let added = count_markers(&masked).saturating_sub(count_markers(text))
        + masked
            .matches(ELIDED)
            .count()
            .saturating_sub(text.matches(ELIDED).count());
    (masked, removed + saturating_u32(added))
}

/// Redacts the free text of a log row, whether or not it has a capture: the error text and every
/// string of the attempts list (upstream error messages, URLs). Returns how many values were
/// removed.
pub fn redact_row_texts(
    error: Option<&mut String>,
    attempts: &mut Value,
    needles: &[String],
) -> u32 {
    let mut removed = 0;
    if let Some(error) = error {
        let (text, count) = redact_text(error, needles);
        *error = text;
        removed += count;
    }
    let before = json_marker_count(attempts);
    *attempts = redact_json(attempts, needles);
    removed + saturating_u32(json_marker_count(attempts).saturating_sub(before))
}

fn saturating_u32(count: usize) -> u32 {
    u32::try_from(count).unwrap_or(u32::MAX)
}

/// How many redaction markers `text` holds.
pub(super) fn count_markers(text: &str) -> usize {
    text.matches(REDACTED).count()
}

/// The redaction and elision markers in every key and string of `value`: the same count as over
/// `value.to_string()`, because the markers are non-ASCII (never escaped) and JSON escapes only
/// insert ASCII, so no marker is created, split or joined across two strings.
fn json_marker_count(value: &Value) -> usize {
    let marks = |text: &str| count_markers(text) + text.matches(ELIDED).count();
    match value {
        Value::String(text) => marks(text),
        Value::Array(items) => items.iter().map(json_marker_count).sum(),
        Value::Object(members) => members
            .iter()
            .map(|(key, member)| marks(key) + json_marker_count(member))
            .sum(),
        Value::Null | Value::Bool(_) | Value::Number(_) => 0,
    }
}

/// Whether `name` equals an entry of `list`, ignoring ASCII case. Every list it is used with is
/// lower-case ASCII.
pub(super) fn in_list_ignore_case(list: &[&str], name: &str) -> bool {
    list.iter().any(|item| item.eq_ignore_ascii_case(name))
}

/// Whether a query pair carries a secret: an R2 key with a non-empty value.
pub(super) fn is_secret_query_pair(key: &str, value: &str) -> bool {
    !value.is_empty() && in_list_ignore_case(&REDACT_QUERY_KEYS, key)
}

/// R1 for one header (a credential name, or one of `hidden`: the provider's extra header names,
/// whose values are secrets of any length); every other header value still goes through the text
/// rules.
fn redact_header(name: &str, value: &str, needles: &[String], hidden: &[String]) -> RedactedHeader {
    let secret = is_secret_header_name(name) || hidden.iter().any(|h| h.eq_ignore_ascii_case(name));
    let masked = if value.trim().is_empty() || !secret {
        let (text, removed) = redact_text(value, needles);
        (removed > 0).then_some(text)
    } else if in_list_ignore_case(&FULLY_HIDDEN_HEADERS, name) {
        Some(REDACTED.to_owned())
    } else {
        Some(match value.trim().split_once(char::is_whitespace) {
            Some((scheme, rest)) if scheme.eq_ignore_ascii_case("bearer") => {
                format!("{scheme} {}", mask_value(rest))
            }
            Some(_) => REDACTED.to_owned(),
            None => mask_value(value),
        })
    };
    RedactedHeader {
        name: name.to_owned(),
        redacted: masked.is_some(),
        value: masked.unwrap_or_else(|| value.to_owned()),
    }
}

fn redact_headers(
    headers: Vec<(String, String)>,
    needles: &[String],
    hidden: &[String],
) -> Vec<RedactedHeader> {
    headers
        .into_iter()
        .map(|(name, value)| redact_header(&name, &value, needles, hidden))
        .collect()
}

/// Drops the trailing token characters of a body that was cut. A body copied up to its copy cap
/// may end inside a secret the redaction cannot see whole, so the whole token run goes, however
/// long it is (fail closed).
fn trim_cut_token(text: &mut String) {
    let keep = text
        .trim_end_matches(|c: char| c.is_ascii_alphanumeric() || "_-.+/=".contains(c))
        .len();
    text.truncate(keep);
}

/// Redacts the full copied text, elides base64, then cuts to the cap.
fn redact_body(body: BoundedBody, needles: &[String], removed: &mut u32) -> CapturedBody {
    let (redacted, count) = redact_text(&body.text, needles);
    *removed += count;
    let elided = match elide_text(&redacted) {
        Cow::Owned(elided) => Some(elided),
        Cow::Borrowed(_) => None,
    };
    let (mut text, writer_elided) = match elided {
        Some(elided) => (elided, true),
        None => (redacted, false),
    };
    let cut_len = cut_text(&text, MAX_BODY_BYTES).len();
    let cut_here = cut_len < text.len();
    text.truncate(cut_len);
    if body.truncated && !cut_here {
        // The text still ends where the request path stopped copying.
        trim_cut_token(&mut text);
    }
    CapturedBody {
        text: Some(text),
        truncated: body.truncated || cut_here,
        elided: body.elided || writer_elided,
        original_bytes: body.original_bytes,
    }
}

/// Redacts the secrets split across frames first (on the raw text, so that a secret whose first
/// part is already recognisable on its own does not leave its tail behind), then every frame,
/// then cuts to the frame caps.
fn redact_frames(
    mut frames: CapturedFrames,
    needles: &[String],
    removed: &mut u32,
) -> CapturedFrames {
    redact_split_frames(&mut frames.frames, needles, removed);
    for frame in &mut frames.frames {
        let (data, count) = redact_text(&frame.data, needles);
        frame.data = data;
        *removed += count;
    }
    let (mut bytes, mut keep) = (0, 0);
    for frame in &frames.frames {
        let size = frame.event.len() + frame.data.len();
        if keep >= MAX_FRAMES || bytes + size > MAX_FRAME_BYTES {
            break;
        }
        bytes += size;
        keep += 1;
    }
    frames.frames.truncate(keep);
    frames.truncated = frames.total_frames > keep as u64;
    frames
}

fn redact_hub_request(value: &Value, needles: &[String], removed: &mut u32) -> Value {
    let masked = redact_json(value, needles);
    *removed += saturating_u32(json_marker_count(&masked).saturating_sub(json_marker_count(value)));
    masked
}

fn redact_notes(
    notes: Vec<TranslationNote>,
    needles: &[String],
    removed: &mut u32,
) -> Vec<TranslationNote> {
    notes
        .into_iter()
        .map(|note| {
            let (field, a) = redact_text(&note.field, needles);
            let (reason, b) = redact_text(&note.reason, needles);
            *removed += a + b;
            TranslationNote {
                field,
                reason,
                ..note
            }
        })
        .collect()
}

/// Bytes the stored exchange occupies: header names and values, the URL, body texts, frame event
/// names and data, and the compact JSON of the hub request.
fn stored_bytes(exchange: &CapturedExchange) -> u64 {
    let headers = |list: &[RedactedHeader]| -> usize {
        list.iter().map(|h| h.name.len() + h.value.len()).sum()
    };
    let body =
        |body: Option<&CapturedBody>| body.and_then(|b| b.text.as_deref()).map_or(0, str::len);
    let frames = |frames: Option<&CapturedFrames>| -> usize {
        frames.map_or(0, |f| {
            f.frames.iter().map(|f| f.event.len() + f.data.len()).sum()
        })
    };
    let total = headers(&exchange.client_req_headers)
        + headers(&exchange.upstream_req_headers)
        + headers(&exchange.upstream_resp_headers)
        + headers(&exchange.client_resp_headers)
        + exchange.upstream_url.len()
        + body(exchange.client_req_body.as_ref())
        + body(exchange.upstream_req_body.as_ref())
        + body(exchange.upstream_resp_body.as_ref())
        + body(exchange.client_resp_body.as_ref())
        + frames(exchange.upstream_frames.as_ref())
        + frames(exchange.client_frames.as_ref())
        + exchange.hub_request.as_ref().map_or(0, compact_json_len);
    total as u64
}

/// Counts the bytes written to it.
struct ByteCount(usize);

impl std::io::Write for ByteCount {
    fn write(&mut self, buf: &[u8]) -> std::io::Result<usize> {
        self.0 += buf.len();
        Ok(buf.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// The length of `value.to_string()` without building the string. Serialising a `Value` into a
/// writer that never fails cannot fail, so the `0` fallback is never taken.
fn compact_json_len(value: &Value) -> usize {
    let mut counter = ByteCount(0);
    serde_json::to_writer(&mut counter, value).map_or(0, |()| counter.0)
}

/// Redacts `raw` for storage: headers (R1), URL (R2), bodies, frames and the hub request (R3-R9
/// and the exact-value `needles`). Returns the exchange (whose `redacted_count` is also the second
/// element) so the writer can log the count.
#[must_use]
pub fn redact_exchange(raw: RawExchange, needles: &[String]) -> (CapturedExchange, u32) {
    redact_exchange_hiding(raw, needles, &[])
}

/// [`redact_exchange`] that also masks the value of every header named in `hidden` (the provider's
/// extra header names), whatever its length: a value under the needle floor cannot be a needle.
#[must_use]
pub fn redact_exchange_hiding(
    raw: RawExchange,
    needles: &[String],
    hidden: &[String],
) -> (CapturedExchange, u32) {
    let mut removed = 0;
    let (upstream_url, url_removed) = redact_url(&raw.upstream_url, needles);
    removed += url_removed;
    let mut body = |body: Option<BoundedBody>| body.map(|b| redact_body(b, needles, &mut removed));
    let client_req_body = body(raw.client_req_body);
    let upstream_req_body = body(raw.upstream_req_body);
    let upstream_resp_body = body(raw.upstream_resp_body);
    let client_resp_body = body(raw.client_resp_body);
    let upstream_frames = raw
        .upstream_frames
        .map(|f| redact_frames(f, needles, &mut removed));
    let client_frames = raw
        .client_frames
        .map(|f| redact_frames(f, needles, &mut removed));
    let hub_request = raw
        .hub_request
        .map(|value| redact_hub_request(&value, needles, &mut removed));
    let translation_notes = redact_notes(raw.translation_notes, needles, &mut removed);
    let mut exchange = CapturedExchange {
        request_log_id: raw.request_log_id,
        ts_ms: raw.ts_ms,
        client_protocol: raw.client_protocol,
        client_req_headers: redact_headers(raw.client_req_headers, needles, hidden),
        client_req_body,
        hub_request,
        upstream_url,
        upstream_req_headers: redact_headers(raw.upstream_req_headers, needles, hidden),
        upstream_req_body,
        upstream_status: raw.upstream_status,
        upstream_resp_headers: redact_headers(raw.upstream_resp_headers, needles, hidden),
        upstream_resp_body,
        client_resp_headers: redact_headers(raw.client_resp_headers, needles, hidden),
        client_resp_body,
        upstream_frames,
        client_frames,
        translation_notes,
        timing: raw.timing,
        bytes_total: 0,
        redacted_count: removed,
    };
    exchange.bytes_total = stored_bytes(&exchange);
    (exchange, removed)
}
