//! The one redaction step in front of the model.
//!
//! Single responsibility: every SpanBay tool result passes through [`redact_for_model`], a pure
//! function. Values of secret variables (four characters or longer, also URL-encoded, base64 and
//! JSON-escaped, and base64 embedded in a longer base64 text) become `{{secret:<name>}}`;
//! credential header values become `<redacted>`; everything else the shared corpus of
//! [`crate::secrets::redact_secrets`] catches (query parameters named `key` or `token`, `sk-…`
//! and gateway client tokens, `Bearer …`, PEM keys, JWTs) is masked; finally the text is cut at
//! the caller's cap with the standard truncation marker.

use std::collections::HashSet;
use std::sync::LazyLock;

use base64::Engine as _;
use base64::engine::general_purpose::{STANDARD, STANDARD_NO_PAD, URL_SAFE, URL_SAFE_NO_PAD};
use regex::{Regex, RegexBuilder};

use crate::secrets::{REDACTED, redact_secrets};

/// Shortest secret value the step masks.
const MIN_SECRET_CHARS: usize = 4;
/// Shortest base64 fragment used as a needle, so a short secret does not mask arbitrary text.
const MIN_FRAGMENT_CHARS: usize = 4;
/// What a credential header value becomes.
pub(super) const REDACTED_HEADER: &str = "<redacted>";

/// A secret variable the model must never see: its name (shown as the placeholder) and value.
#[derive(Clone, PartialEq, Eq)]
pub struct NamedSecret {
    /// The variable's name (`token`).
    pub name: String,
    /// The secret value; never logged, so `Debug` hides it.
    pub value: String,
}

impl std::fmt::Debug for NamedSecret {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter
            .debug_struct("NamedSecret")
            .field("name", &self.name)
            .field("value", &"…")
            .finish()
    }
}

/// Whether `name` is a header whose value is a credential or a session identifier.
#[must_use]
pub fn is_credential_header(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "authorization"
            | "proxy-authorization"
            | "cookie"
            | "set-cookie"
            | "x-api-key"
            | "api-key"
            | "x-goog-api-key"
            | "x-auth-token"
    )
}

/// The value of header `name` as the model may see it: `<redacted>` for a credential header.
#[must_use]
pub fn header_value_for_model(name: &str, value: &str) -> String {
    if is_credential_header(name) {
        REDACTED_HEADER.to_owned()
    } else {
        value.to_owned()
    }
}

static TEMPLATE_ONLY: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r"^(?:[A-Za-z][A-Za-z0-9-]*\s+)?\{\{[^{}]+\}\}$").ok());

/// Whether `value` is empty or only a `{{variable}}`, optionally behind a scheme word (`Bearer`):
/// a reference shows no secret, so it may be shown as written.
pub(super) fn template_only(value: &str) -> bool {
    let value = value.trim();
    value.is_empty()
        || TEMPLATE_ONLY
            .as_ref()
            .is_some_and(|pattern| pattern.is_match(value))
}

/// Query parameter names that carry a credential.
pub(super) fn is_credential_param(name: &str) -> bool {
    matches!(
        name.trim().to_ascii_lowercase().as_str(),
        "key"
            | "token"
            | "api_key"
            | "apikey"
            | "access_token"
            | "password"
            | "secret"
            | "client_secret"
            | "signature"
            | "sig"
    )
}

/// Whether the value of row `name` is a credential a person or the model must not read as a
/// literal: a credential header (also the `x-…-token` style names) or query parameter.
pub(super) fn is_credential_row(name: &str, header: bool) -> bool {
    if header {
        is_credential_header(name) || crate::secrets::is_secret_header_name(name)
    } else {
        is_credential_param(name)
    }
}

/// A row value for a diff or a definition: a `{{variable}}` reference as written, any other
/// value of a credential row as `<redacted>`.
pub(super) fn row_value_shown(name: &str, value: &str, header: bool) -> String {
    if is_credential_row(name, header) && !template_only(value) {
        REDACTED_HEADER.to_owned()
    } else {
        value.to_owned()
    }
}

/// `text` made safe for the model and cut to at most `cap_bytes` (plus the truncation marker).
///
/// Idempotent apart from the cap, and pure: nothing is read from the environment.
#[must_use]
pub fn redact_for_model(text: &str, secrets: &[NamedSecret], cap_bytes: usize) -> String {
    let masked = mask_secret_values(text, secrets);
    let masked = redact_secrets(&masked, &[]);
    let masked = match CREDENTIAL_HEADER_MASK.as_ref() {
        Some(pattern) => pattern
            .replace_all(&masked, format!("${{1}}{REDACTED_HEADER}"))
            .into_owned(),
        None => masked,
    };
    cap_text(masked, cap_bytes)
}

/// The corpus masks a credential header value with [`REDACTED`]; the model sees the word
/// `<redacted>` instead.
static CREDENTIAL_HEADER_MASK: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(&format!(
        r#"(?i)(\b(?:proxy-authorization|authorization|set-cookie|cookie|x-api-key)\b["']?\s*[:=]\s*["']?){REDACTED}"#
    ))
    .ok()
});

/// Cuts `text` at a character boundary at or before `cap` bytes and appends the marker.
fn cap_text(mut text: String, cap: usize) -> String {
    if text.len() <= cap {
        return text;
    }
    let mut cut = cap;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    let omitted = text.len() - cut;
    text.truncate(cut);
    text.push_str(&format!("\n[truncated: {omitted} bytes omitted]"));
    text
}

/// Replaces every form of every secret value with its placeholder in one pass, so a placeholder
/// is never scanned again.
fn mask_secret_values(text: &str, secrets: &[NamedSecret]) -> String {
    let mut forms: Vec<(String, String)> = Vec::new();
    let mut seen = HashSet::new();
    for secret in secrets
        .iter()
        .filter(|secret| secret.value.chars().count() >= MIN_SECRET_CHARS)
    {
        let placeholder = format!("{{{{secret:{}}}}}", secret.name);
        for form in value_forms(&secret.value) {
            if seen.insert(form.clone()) {
                forms.push((form, placeholder.clone()));
            }
        }
    }
    if forms.is_empty() {
        return text.to_owned();
    }
    // Longest first: at one position the regex alternation tries the longest form first.
    forms.sort_by_key(|(form, _)| std::cmp::Reverse(form.len()));
    let pattern = forms
        .iter()
        .map(|(form, _)| regex::escape(form))
        .collect::<Vec<_>>()
        .join("|");
    match RegexBuilder::new(&pattern).size_limit(64 << 20).build() {
        Ok(regex) => regex
            .replace_all(text, |found: &regex::Captures<'_>| {
                forms
                    .iter()
                    .find(|(form, _)| form == &found[0])
                    .map_or_else(String::new, |(_, placeholder)| placeholder.clone())
            })
            .into_owned(),
        // Never fail open: without the combined pattern, replace form by form.
        Err(_) => forms
            .iter()
            .fold(text.to_owned(), |text, (form, placeholder)| {
                text.replace(form.as_str(), placeholder)
            }),
    }
}

/// Every spelling of `value` a server, a URL or a JSON document may echo back.
fn value_forms(value: &str) -> Vec<String> {
    let mut forms = vec![value.to_owned()];
    let form_encoded: String = url::form_urlencoded::byte_serialize(value.as_bytes()).collect();
    let percent_encoded = form_encoded.replace('+', "%20");
    forms.push(lower_hex_escapes(&percent_encoded));
    forms.push(lower_hex_escapes(&form_encoded));
    forms.push(percent_encoded);
    forms.push(form_encoded);
    if let Ok(json) = serde_json::to_string(value) {
        forms.push(json.trim_matches('"').to_owned());
    }
    let bytes = value.as_bytes();
    forms.push(STANDARD.encode(bytes));
    forms.push(STANDARD_NO_PAD.encode(bytes));
    forms.push(URL_SAFE.encode(bytes));
    forms.push(URL_SAFE_NO_PAD.encode(bytes));
    // The value inside a longer base64 text starts at any of three alignments.
    for offset in 0..3_usize {
        let mut shifted = vec![0_u8; offset];
        shifted.extend_from_slice(bytes);
        let skip = [0, 2, 3][offset];
        let end = shifted.len() * 8 / 6;
        for encoded in [STANDARD.encode(&shifted), URL_SAFE.encode(&shifted)] {
            if let Some(fragment) = encoded.get(skip..end)
                && fragment.len() >= MIN_FRAGMENT_CHARS
            {
                forms.push(fragment.to_owned());
            }
        }
    }
    forms.retain(|form| form.chars().count() >= MIN_SECRET_CHARS);
    forms
}

/// `%2F` spelled `%2f`: some encoders emit lower-case hex.
fn lower_hex_escapes(encoded: &str) -> String {
    let mut out = String::with_capacity(encoded.len());
    let mut chars = encoded.chars();
    while let Some(c) = chars.next() {
        out.push(c);
        if c == '%' {
            for _ in 0..2 {
                if let Some(digit) = chars.next() {
                    out.push(digit.to_ascii_lowercase());
                }
            }
        }
    }
    out
}
