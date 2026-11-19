//! "Copy as curl": a runnable command built from a stored capture. Credentials are always
//! placeholders (`$PINGDECK_TOKEN`, `$PROVIDER_API_KEY`), never the stored masked text; nothing
//! here touches the clipboard.

use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::model::{CapturedExchange, RowFacts};
use super::redact::{count_markers, in_list_ignore_case, is_secret_query_pair, userinfo_at};
use super::stored::{
    CUT_WARNING, NO_BODY, StoredBody, client_body, gateway_path, gateway_url,
    redacted_values_phrase, upstream_body,
};
use pingdeck_base::secrets::ELIDED;

/// Placeholder for the gateway client token.
const TOKEN_PLACEHOLDER: &str = "$PINGDECK_TOKEN";
/// Placeholder for a provider key (the sniffer's "Copy as curl" uses the same variable).
const KEY_PLACEHOLDER: &str = crate::llm::sniff::CURL_KEY_VARIABLE;
/// Headers curl sets itself or that describe the original transfer.
const SKIPPED_HEADERS: [&str; 6] = [
    "host",
    "content-length",
    "connection",
    "transfer-encoding",
    "accept-encoding",
    "expect",
];
/// Credential headers: a redacted value becomes the key placeholder. Kept apart from
/// `pingdeck_base::secrets::REDACT_HEADERS`: this list names the headers that carry the provider
/// key a user can put back, not every header redaction hides.
const CREDENTIAL_HEADERS: [&str; 6] = [
    "authorization",
    "proxy-authorization",
    "x-api-key",
    "x-goog-api-key",
    "api-key",
    "x-auth-token",
];
/// Client headers that change how the gateway reads the request.
const FORWARDED_CLIENT_HEADERS: [&str; 3] = ["anthropic-version", "anthropic-beta", "openai-beta"];

/// Which request the command re-sends.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CurlKind {
    /// The client request, sent to the local gateway with `$PINGDECK_TOKEN`.
    Gateway,
    /// The post-translation upstream request with `$PROVIDER_API_KEY`.
    Upstream,
}

/// The command and what the user should know before running it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurlOutput {
    /// The shell command.
    pub text: String,
    /// Visible warnings (cut body, redacted values, removed headers).
    pub warnings: Vec<String>,
}

/// The reason no command can be built (the action is disabled with it as tooltip).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurlRefusal {
    /// Human-readable reason.
    pub reason: String,
}

fn single_quoted(text: &str) -> String {
    format!("'{}'", text.replace('\'', "'\\''"))
}

/// Escapes `\`, `"`, `` ` `` and `$` for a double-quoted shell word.
fn double_quote_escaped(text: &str) -> String {
    let mut out = String::with_capacity(text.len());
    for c in text.chars() {
        if matches!(c, '\\' | '"' | '`' | '$') {
            out.push('\\');
        }
        out.push(c);
    }
    out
}

/// A heredoc delimiter no body line equals.
fn heredoc_delimiter(body: &str) -> String {
    let mut delimiter = "JSON".to_owned();
    let mut n = 0;
    while body.lines().any(|line| line == delimiter) {
        n += 1;
        delimiter = format!("JSON_{n}");
    }
    delimiter
}

/// The URL for the upstream command: user info dropped, key query values replaced by the
/// placeholder. Returned already double-quoted, with whether user info was dropped.
fn upstream_url_argument(url: &str) -> (String, bool) {
    let url = url.split('#').next().unwrap_or(url);
    let (head, query) = match url.split_once('?') {
        Some((head, query)) => (head, Some(query)),
        None => (url, None),
    };
    let mut head = head.to_owned();
    let mut userinfo_removed = false;
    if let Some((scheme, rest)) = head.split_once("://")
        && let Some(at) = userinfo_at(rest)
    {
        head = format!("{scheme}://{}", &rest[at + 1..]);
        userinfo_removed = true;
    }
    let mut argument = double_quote_escaped(&head);
    if let Some(query) = query {
        let pairs: Vec<String> = query
            .split('&')
            .map(|pair| match pair.split_once('=') {
                Some((key, value)) if is_secret_query_pair(key, value) => {
                    format!("{}={KEY_PLACEHOLDER}", double_quote_escaped(key))
                }
                _ => double_quote_escaped(pair),
            })
            .collect();
        argument.push('?');
        argument.push_str(&pairs.join("&"));
    }
    (format!("\"{argument}\""), userinfo_removed)
}

/// The `-H` argument for one upstream header, or `None` when it must not be copied.
fn upstream_header_argument(
    name: &str,
    value: &str,
    redacted: bool,
    warnings: &mut Vec<String>,
) -> Option<String> {
    if in_list_ignore_case(&SKIPPED_HEADERS, name) {
        return None;
    }
    if !redacted {
        return Some(single_quoted(&format!("{name}: {value}")));
    }
    if !in_list_ignore_case(&CREDENTIAL_HEADERS, name) {
        warnings.push(format!(
            "Header {name} was removed (its value was redacted)"
        ));
        return None;
    }
    let bearer = value
        .split_once(char::is_whitespace)
        .is_some_and(|(scheme, _)| scheme.eq_ignore_ascii_case("bearer"));
    let placeholder = if bearer {
        format!("Bearer {KEY_PLACEHOLDER}")
    } else {
        KEY_PLACEHOLDER.to_owned()
    };
    Some(format!("\"{name}: {placeholder}\""))
}

fn body_warnings(body: &StoredBody<'_>, warnings: &mut Vec<String>) {
    if body.truncated {
        warnings.push(CUT_WARNING.to_owned());
    }
    let redacted = count_markers(&body.text);
    if redacted > 0 {
        warnings.push(format!(
            "{}; the command sends the redacted text",
            redacted_values_phrase(redacted)
        ));
    }
}

fn refuse(reason: &str) -> CurlRefusal {
    CurlRefusal {
        reason: reason.to_owned(),
    }
}

/// The request body `kind` re-sends; refused when none was stored or media was elided from it.
fn request_body<'a>(
    capture: &'a CapturedExchange,
    facts: &RowFacts,
    kind: CurlKind,
) -> Result<StoredBody<'a>, CurlRefusal> {
    let body = match kind {
        CurlKind::Gateway => client_body(capture),
        CurlKind::Upstream => {
            if capture.upstream_url.is_empty() {
                return Err(refuse("No upstream request was captured"));
            }
            upstream_body(capture, facts)
        }
    }
    .ok_or_else(|| refuse(NO_BODY))?;
    if body.elided {
        return Err(refuse(
            "The request body was not stored in full: media was elided",
        ));
    }
    Ok(body)
}

/// The quoted gateway URL and the headers: the token placeholder plus the unredacted client
/// headers that change how the gateway reads the request.
fn gateway_parts(
    capture: &CapturedExchange,
    facts: &RowFacts,
    base: &str,
) -> (String, Vec<String>) {
    let mut headers = vec![format!("\"Authorization: Bearer {TOKEN_PLACEHOLDER}\"")];
    for header in &capture.client_req_headers {
        if !header.redacted && in_list_ignore_case(&FORWARDED_CLIENT_HEADERS, &header.name) {
            headers.push(single_quoted(&format!("{}: {}", header.name, header.value)));
        }
    }
    let path = gateway_path(
        capture.client_protocol,
        &facts.requested_model,
        facts.stream,
    );
    (single_quoted(&gateway_url(base, &path)), headers)
}

/// The quoted upstream URL and headers. Warnings are pushed in this order: one per removed
/// header, redacted text left in the URL the command sends, a removed path segment, then the
/// removed user info.
fn upstream_parts(capture: &CapturedExchange, warnings: &mut Vec<String>) -> (String, Vec<String>) {
    let mut headers = Vec::new();
    for header in &capture.upstream_req_headers {
        headers.extend(upstream_header_argument(
            &header.name,
            &header.value,
            header.redacted,
            warnings,
        ));
    }
    let (url, userinfo_removed) = upstream_url_argument(&capture.upstream_url);
    if count_markers(&url) > 0 {
        warnings.push("The URL contains redacted text".to_owned());
    }
    if capture
        .upstream_url
        .split('?')
        .next()
        .is_some_and(|path| path.contains(ELIDED))
    {
        warnings.push(
            "A token-like segment of the URL path was removed (…); put the real segment back"
                .to_owned(),
        );
    }
    if userinfo_removed {
        warnings.push(
            "The URL's user info was removed; add credentials if the host needs them".to_owned(),
        );
    }
    (url, headers)
}

/// Adds a JSON content type unless a header already sets one.
fn ensure_content_type(headers: &mut Vec<String>) {
    let has_content_type = headers.iter().any(|h| {
        h.to_ascii_lowercase()
            .trim_start_matches(['\'', '"'])
            .starts_with("content-type:")
    });
    if !has_content_type {
        headers.push("'content-type: application/json'".to_owned());
    }
}

/// `curl -sS URL`, one `-H` line per header, and the body in a heredoc.
fn render_command(url: &str, headers: &[String], body: &str) -> String {
    let delimiter = heredoc_delimiter(body);
    let mut text = format!("curl -sS {url}");
    for header in headers {
        text.push_str(" \\\n  -H ");
        text.push_str(header);
    }
    // Writing to a `String` cannot fail.
    let _ = write!(
        text,
        " \\\n  --data-binary @- <<'{delimiter}'\n{body}\n{delimiter}\n"
    );
    text
}

/// Builds the command for `kind` from `capture`.
///
/// Gateway: `POST {gateway_base}{path}` with `Authorization: Bearer $PINGDECK_TOKEN`. Upstream:
/// the stored upstream URL and headers, every redacted credential replaced by
/// `$PROVIDER_API_KEY`. The body travels in a heredoc.
///
/// # Errors
/// [`CurlRefusal`] when no body was stored, when media was elided from it, or (upstream) when no
/// upstream request was captured. A body cut at the cap is allowed with a warning.
pub fn build_curl(
    capture: &CapturedExchange,
    facts: &RowFacts,
    kind: CurlKind,
    gateway_base: &str,
) -> Result<CurlOutput, CurlRefusal> {
    let body = request_body(capture, facts, kind)?;
    let mut warnings = Vec::new();
    body_warnings(&body, &mut warnings);
    let (url, mut headers) = match kind {
        CurlKind::Gateway => gateway_parts(capture, facts, gateway_base),
        CurlKind::Upstream => upstream_parts(capture, &mut warnings),
    };
    ensure_content_type(&mut headers);
    Ok(CurlOutput {
        text: render_command(&url, &headers, &body.text),
        warnings,
    })
}
