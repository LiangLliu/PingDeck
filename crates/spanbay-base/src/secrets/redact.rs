//! Text and JSON credential redaction patterns.

use std::{borrow::Cow, sync::LazyLock};

use regex::Regex;

pub const REDACTED: &str = "••••••";
pub const MIN_REDACTION_CHARS: usize = 8;

/// How a pattern match is masked.
#[derive(Clone, Copy)]
enum Mask {
    /// The whole match.
    Whole,
    /// Everything after capture group 1 (the key and its separator).
    AfterKey,
    /// Everything after capture group 1, written as a quoted JSON string.
    QuotedAfterKey,
    /// Everything after capture group 1; a value wrapped in `"` or `'` (groups 2 and 3 hold the
    /// opening quote) keeps its quotes, so a masked JSON member stays valid JSON.
    KeepQuotes,
}

/// Identifier words that make a key a credential (`DB_PASSWORD`, `id_token`, `secretKey`), shared
/// by the form (`key=value`), JSON (`"key": "value"`) and header-name rules; the patterns are
/// case-insensitive.
const KEY_CORES: &str = r"password|passwd|secret|token|api[_-]?key|private[_-]?key";

/// What a credential key may end with after its core word (`secret_access_key`, `token_id`);
/// anything else (`max_tokens`, `token_type`, `passwords`) is not a credential.
const KEY_ENDINGS: [&str; 4] = ["accesskey", "key", "id", "value"];

/// The text-pattern form of a credential key: identifier characters around a core word.
fn compound_key_pattern() -> String {
    format!(r"[\w.-]*(?:{KEY_CORES})(?:[_.-]?(?:access[_.-]?key|key|id|value))?")
}

/// R1: header names whose value is always removed, compared case-insensitively. The
/// one list every sink uses (the text patterns below and the header-aware capture redaction);
/// [`is_secret_header_name`] adds the custom `x-…-token` style names.
pub const REDACT_HEADERS: [&str; 9] = [
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "x-goog-api-key",
    "api-key",
    "x-auth-token",
    "x-amz-security-token",
    "cookie",
    "set-cookie",
];

/// Whether the value of header `name` must never be stored or shown: one of [`REDACT_HEADERS`],
/// or a name that carries a credential word (`x-relay-token`, `X-Client-Secret`,
/// `x-api-key-v2`). The single check for header-aware redaction, whatever the value's length.
#[must_use]
pub fn is_secret_header_name(name: &str) -> bool {
    let lower = name.trim().to_ascii_lowercase();
    REDACT_HEADERS.contains(&lower.as_str()) || key_is_credential(&lower)
}

/// R2: URL query keys whose value is removed, compared case-insensitively.
pub const REDACT_QUERY_KEYS: [&str; 7] = [
    "key",
    "api_key",
    "apikey",
    "token",
    "access_token",
    "signature",
    "sig",
];

/// R3: JSON object keys (exact, case-sensitive; `max_tokens` is not one) whose value is removed
/// at any depth.
pub const REDACT_JSON_KEYS: [&str; 9] = [
    "api_key",
    "apiKey",
    "authorization",
    "access_token",
    "refresh_token",
    "client_secret",
    "password",
    "secret",
    "token",
];

/// What every minted gateway client token starts with (`pd_<tag>_<32 base62>`). The
/// minting code and R5 both read this constant, so a changed format cannot leave tokens
/// unredacted.
pub const CLIENT_TOKEN_PREFIX: &str = "pd_";

/// The legacy gateway token prefix (`pdk_…`) that older client configs still carry.
const LEGACY_CLIENT_TOKEN_PREFIX: &str = "pdk_";

/// R4 to R7 token shapes, each needing a word boundary before it so
/// `risk-assessment-…` is not an `sk-` key. R5 is generated from [`CLIENT_TOKEN_PREFIX`]: the
/// prefix followed by 16 or more token characters (a full `pd_<tag>_<body>` token included).
pub(super) fn token_shapes() -> String {
    let client = regex::escape(CLIENT_TOKEN_PREFIX);
    let legacy = regex::escape(LEGACY_CLIENT_TOKEN_PREFIX);
    format!(
        r"\b(?:sk-[A-Za-z0-9_-]{{16,}}|AIza[0-9A-Za-z_-]{{35}}|gh[pousr]_[A-Za-z0-9]{{36,}}|github_pat_[A-Za-z0-9_]{{50,}}|xox[abprs]-[A-Za-z0-9-]{{10,}}|AKIA[0-9A-Z]{{16}}|gsk_[A-Za-z0-9]{{20,}}|xai-[A-Za-z0-9]{{20,}}|ya29\.[A-Za-z0-9._-]{{20,}}|glpat-[A-Za-z0-9_-]{{20,}}|npm_[A-Za-z0-9]{{36}}|[sr]k_(?:live|test)_[A-Za-z0-9]{{16,}}|{client}[A-Za-z0-9_]{{16,}}|{legacy}[A-Za-z0-9_]{{16,}}|eyJ[A-Za-z0-9_-]{{8,}}\.[A-Za-z0-9_-]{{8,}}\.[A-Za-z0-9_-]{{8,}})"
    )
}

/// R4 to R7 and `Bearer …` as one pattern, for [`is_token_shaped`].
static TOKEN_SHAPE: LazyLock<Option<Regex>> = LazyLock::new(|| {
    Regex::new(&format!(
        r"{}|(?i)\bbearer\s+[A-Za-z0-9._~+/-]{{8,}}",
        token_shapes()
    ))
    .ok()
});

/// Whether `value` holds a well-known credential shape: a provider or platform key (`sk-…`,
/// `ghp_…`, `AKIA…`, `pd_…`), a JWT or a `Bearer` credential. For deciding that a value written
/// at run time (a script's `pm.environment.set`) must be stored as a secret.
#[must_use]
pub fn is_token_shaped(value: &str) -> bool {
    TOKEN_SHAPE
        .as_ref()
        .is_some_and(|pattern| pattern.is_match(value))
}

/// A PEM private-key block; a block cut off before its `END` line is masked to the end of text.
const PEM_PRIVATE_KEY: &str = r"(?s)-----BEGIN [A-Z ]*PRIVATE KEY-----.*?-----END [A-Z ]*PRIVATE KEY-----|-----BEGIN [A-Z ]*PRIVATE KEY-----.*";

/// Number of patterns [`compile_patterns`] must produce; a test pins it so a pattern that fails
/// to compile cannot silently switch redaction off.
#[cfg(test)]
pub(super) const PATTERN_COUNT: usize = 8;

/// The corpus beyond the needles, applied in order: PEM private keys (R8), credential
/// headers (R1, also as JSON or query fields; the value runs to the end of the line, or is one
/// quoted string), URL query parameters (R2), form keys, JSON keys (R3), `Bearer` values (R7),
/// token shapes (R4 to R7). Applying it twice changes nothing.
pub(super) static PATTERNS: LazyLock<Vec<(Regex, String)>> = LazyLock::new(compile_patterns);

pub(super) fn compile_patterns() -> Vec<(Regex, String)> {
    let headers = REDACT_HEADERS.join("|");
    let query_keys = REDACT_QUERY_KEYS.join("|");
    let key = compound_key_pattern();
    [
        (PEM_PRIVATE_KEY.to_owned(), Mask::Whole),
        (
            format!(
                r#"(?i)(["']?\b(?:{headers}|x-{key})\b["']?\s*[:=]\s*)(?:(")(?:[^"\\\r\n]|\\.)*"|(')(?:[^'\\\r\n]|\\.)*'|[^\s{{\[][^\r\n]*)"#
            ),
            Mask::KeepQuotes,
        ),
        (
            format!(r#"(?i)([?&](?:{query_keys}|secret)=)[^&#\s"'<>]*"#),
            Mask::AfterKey,
        ),
        (
            format!(
                r#"(?i)\b({key}=)(?:(")(?:[^"\\\r\n]|\\.)*"|(')(?:[^'\\\r\n]|\\.)*'|[^&#\s"'<>]*)"#
            ),
            Mask::KeepQuotes,
        ),
        (
            format!(r#"(?i)("{key}"\s*:\s*)"(?:[^"\\]|\\.)+""#),
            Mask::QuotedAfterKey,
        ),
        (
            r"\b(Bearer\s+)[A-Za-z0-9._~+/-]+=*".to_owned(),
            Mask::AfterKey,
        ),
        // Any case, but only a token-length value: "the bearer of bad news" stays text.
        (
            r"(?i)\b(bearer\s+)[A-Za-z0-9._~+/-]{8,}=*".to_owned(),
            Mask::AfterKey,
        ),
        (token_shapes(), Mask::Whole),
    ]
    .into_iter()
    .filter_map(|(pattern, mask)| {
        let template = match mask {
            Mask::Whole => REDACTED.to_owned(),
            Mask::AfterKey => format!("${{1}}{REDACTED}"),
            Mask::QuotedAfterKey => format!("${{1}}\"{REDACTED}\""),
            Mask::KeepQuotes => format!("${{1}}${{2}}${{3}}{REDACTED}${{2}}${{3}}"),
        };
        Regex::new(&pattern).ok().map(|regex| (regex, template))
    })
    .collect()
}

/// Replaces every known secret value (plus its URL-encoded and JSON-escaped forms, for needles of
/// at least [`MIN_REDACTION_CHARS`] characters), then masks the rest of the corpus: credential
/// headers and fields (`Authorization`, `x-api-key`, `Cookie`, `X-Auth-Token`, …), URL query
/// parameters, form and JSON credential keys, `Bearer` values and well-known token shapes
/// (provider keys, GitHub/Slack/AWS tokens, `pd_…`/`pdk_…` gateway tokens, JWTs), with
/// [`REDACTED`]. Finally every URL path segment that is a run of at least
/// [`MIN_OPAQUE_SEGMENT_CHARS`] characters of `[A-Za-z0-9_-]` (an embedded key or account id,
/// as in `https://host/v1/<token>/balance`) becomes [`ELIDED`].
#[must_use]
pub fn redact_secrets(text: &str, needles: &[String]) -> String {
    let mut out = redact_known_values(text, needles).into_owned();
    for (pattern, template) in PATTERNS.iter() {
        if let Cow::Owned(masked) = pattern.replace_all(&out, template.as_str()) {
            out = masked;
        }
    }
    if let Some(urls) = URL_PATTERN.as_ref()
        && let Cow::Owned(masked) = urls.replace_all(&out, |found: &regex::Captures<'_>| {
            mask_path_segments(&found[0])
        })
    {
        out = masked;
    }
    out
}

/// R9 alone: `text` with every needle of at least [`MIN_REDACTION_CHARS`] characters replaced by
/// [`REDACTED`], raw, URL-encoded and JSON-escaped. For rewriting stored text where only values
/// the store knows are secrets (the migration scrub), without the pattern corpus.
#[must_use]
pub fn redact_known_values<'a>(text: &'a str, needles: &[String]) -> Cow<'a, str> {
    let mut forms: Vec<String> = Vec::new();
    for needle in needles
        .iter()
        .filter(|needle| needle.chars().count() >= MIN_REDACTION_CHARS)
    {
        forms.push(needle.clone());
        let encoded: String = url::form_urlencoded::byte_serialize(needle.as_bytes()).collect();
        let escaped = serde_json::to_string(needle)
            .map(|json| json.trim_matches('"').to_owned())
            .unwrap_or_default();
        for form in [encoded, escaped] {
            if form != *needle && form.chars().count() >= MIN_REDACTION_CHARS {
                forms.push(form);
            }
        }
    }
    // Longest first, so a secret that contains a shorter one is replaced whole.
    forms.sort_by_key(|form| std::cmp::Reverse(form.len()));
    forms.dedup();
    let mut out = Cow::Borrowed(text);
    for form in &forms {
        if out.contains(form.as_str()) {
            out = Cow::Owned(out.replace(form.as_str(), REDACTED));
        }
    }
    out
}

/// Replaces every secret value (raw, form-, path- and JSON-encoded) in `input` with the mask.
/// Longer values go first, so a secret that contains another one is masked whole instead of
/// leaving its tail behind.
pub fn mask_secrets(input: &str, secret_values: &[String]) -> String {
    let mut values: Vec<&String> = secret_values
        .iter()
        .filter(|value| !value.is_empty())
        .collect();
    values.sort_by_key(|value| std::cmp::Reverse(value.len()));
    values.into_iter().fold(input.to_owned(), |masked, value| {
        let masked = masked.replace(value.as_str(), REDACTED);
        let form_encoded: String = url::form_urlencoded::byte_serialize(value.as_bytes()).collect();
        let path_encoded = crate::url_encode::encode_path_segment(value);
        // Serialization of a `&str` cannot fail; if it somehow did, fall back
        // to the already masked plain form instead of panicking.
        let json = serde_json::to_string(value).unwrap_or_default();
        let json_escaped = json
            .strip_prefix('"')
            .and_then(|inner| inner.strip_suffix('"'))
            .filter(|inner| !inner.is_empty())
            .unwrap_or(value.as_str());
        masked
            .replace(form_encoded.as_str(), REDACTED)
            .replace(path_encoded.as_str(), REDACTED)
            .replace(json_escaped, REDACTED)
    })
}

/// What an opaque URL path segment or an identity value is replaced with.
pub const ELIDED: &str = "…";

/// Shortest URL path segment of `[A-Za-z0-9_-]` that [`redact_secrets`] elides.
pub const MIN_OPAQUE_SEGMENT_CHARS: usize = 24;

/// A URL: scheme, `://` and everything up to whitespace, a quote or a bracket.
static URL_PATTERN: LazyLock<Option<Regex>> =
    LazyLock::new(|| Regex::new(r#"[A-Za-z][A-Za-z0-9+.-]*://[^\s"'<>()\[\]{},;]+"#).ok());

/// `url` with each opaque path segment elided (see [`redact_url_path`]); the scheme, authority,
/// query and fragment are kept.
fn mask_path_segments(url: &str) -> String {
    let Some(scheme_end) = url.find("://") else {
        return url.to_owned();
    };
    let authority_start = scheme_end + 3;
    let Some(path_offset) = url[authority_start..].find(['/', '?', '#']) else {
        return url.to_owned();
    };
    let path_start = authority_start + path_offset;
    if !url[path_start..].starts_with('/') {
        return url.to_owned();
    }
    let path_end = url[path_start..]
        .find(['?', '#'])
        .map_or(url.len(), |end| path_start + end);
    let mut out = String::with_capacity(url.len());
    out.push_str(&url[..path_start]);
    out.push_str(&redact_url_path(&url[path_start..path_end]));
    out.push_str(&url[path_end..]);
    out
}

/// Whether `text` is non-empty and made only of `[A-Za-z0-9_-]`.
fn is_token_charset(text: &str) -> bool {
    !text.is_empty()
        && text
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'_' || byte == b'-')
}

/// Whether a URL path segment is an embedded credential or account id: at least
/// [`MIN_OPAQUE_SEGMENT_CHARS`] characters of `[A-Za-z0-9_-]`, or the Telegram bot shape
/// `bot<digits>:<20+ token characters>`.
fn segment_is_opaque(segment: &str) -> bool {
    if segment.len() >= MIN_OPAQUE_SEGMENT_CHARS && is_token_charset(segment) {
        return true;
    }
    segment
        .strip_prefix("bot")
        .and_then(|rest| rest.split_once(':'))
        .is_some_and(|(id, token)| {
            !id.is_empty()
                && id.bytes().all(|byte| byte.is_ascii_digit())
                && token.len() >= 20
                && is_token_charset(token)
        })
}

/// `path` (a URL path such as `/t/<token>/v1/models`, without query or fragment) with every
/// opaque segment replaced by [`ELIDED`] (see [`MIN_OPAQUE_SEGMENT_CHARS`]); other segments and
/// the slashes are kept. A trailing sentence period stays outside the elided segment. For URL
/// fields that are stored apart from the text [`redact_secrets`] scans.
#[must_use]
pub fn redact_url_path(path: &str) -> String {
    let mut out = String::with_capacity(path.len());
    for (index, segment) in path.split('/').enumerate() {
        if index > 0 {
            out.push('/');
        }
        let core = segment.trim_end_matches('.');
        if segment_is_opaque(core) {
            out.push_str(ELIDED);
            out.push_str(&segment[core.len()..]);
        } else {
            out.push_str(segment);
        }
    }
    out
}

/// Exact credential keys for [`redact_json`], compared lower-cased with `-` and `_` removed: the
/// body keys and the credential headers. [`key_is_credential`] adds compound names.
const JSON_CREDENTIAL_KEYS: [&str; 14] = [
    "password",
    "secret",
    "token",
    "apikey",
    "clientsecret",
    "refreshtoken",
    "accesstoken",
    "authorization",
    "proxyauthorization",
    "cookie",
    "setcookie",
    "xapikey",
    "xgoogapikey",
    "xauthtoken",
];

/// Keys whose value identifies a person or account (normalised like [`JSON_CREDENTIAL_KEYS`]).
const JSON_IDENTITY_KEYS: [&str; 6] = [
    "email",
    "userid",
    "username",
    "accountid",
    "nickname",
    "displayname",
];

/// `key` lower-cased with `-` and `_` removed.
fn normalised_key(key: &str) -> String {
    key.chars()
        .filter(|c| *c != '-' && *c != '_')
        .flat_map(char::to_lowercase)
        .collect()
}

/// Whether `key` (lower-cased, `-` and `_` removed) is one of `keys`.
fn key_is_in(key: &str, keys: &[&str]) -> bool {
    keys.contains(&normalised_key(key).as_str())
}

/// Whether `key` names a credential: one of [`JSON_CREDENTIAL_KEYS`], or an identifier that ends
/// in a credential word, optionally followed by one of [`KEY_ENDINGS`] (`DB_PASSWORD`,
/// `id_token`, `secretAccessKey`, `x-relay-token`). `max_tokens` and `token_type` are not.
pub(super) fn key_is_credential(key: &str) -> bool {
    if key_is_in(key, &JSON_CREDENTIAL_KEYS) {
        return true;
    }
    const CORES: [&str; 6] = [
        "password",
        "passwd",
        "secret",
        "token",
        "apikey",
        "privatekey",
    ];
    let normalised = normalised_key(key);
    let mut stems = std::iter::once(normalised.as_str()).chain(
        KEY_ENDINGS
            .iter()
            .filter_map(|ending| normalised.strip_suffix(ending)),
    );
    stems.any(|stem| CORES.iter().any(|core| stem.ends_with(core)))
}

/// A copy of `value` that is safe to store or show: the value of an object member with a
/// credential key ([`JSON_CREDENTIAL_KEYS`]) or an identity key (`email`, `user_id`, `username`,
/// `account_id`, `nickname`, `display_name`, in any case or separator style) becomes
/// [`ELIDED`] unless it is an object or array (those are walked); every other string goes
/// through [`redact_secrets`] with `needles`; numbers, booleans and `null` elsewhere are kept.
#[must_use]
pub fn redact_json(value: &serde_json::Value, needles: &[String]) -> serde_json::Value {
    use serde_json::Value;
    match value {
        Value::String(text) => Value::String(redact_secrets(text, needles)),
        Value::Array(items) => Value::Array(
            items
                .iter()
                .map(|item| redact_json(item, needles))
                .collect(),
        ),
        Value::Object(members) => Value::Object(
            members
                .iter()
                .map(|(key, member)| {
                    let sensitive = key_is_credential(key) || key_is_in(key, &JSON_IDENTITY_KEYS);
                    let redacted = match member {
                        Value::Object(_) | Value::Array(_) => redact_json(member, needles),
                        _ if sensitive => Value::String(ELIDED.to_owned()),
                        _ => redact_json(member, needles),
                    };
                    (key.clone(), redacted)
                })
                .collect(),
        ),
        Value::Null | Value::Bool(_) | Value::Number(_) => value.clone(),
    }
}
