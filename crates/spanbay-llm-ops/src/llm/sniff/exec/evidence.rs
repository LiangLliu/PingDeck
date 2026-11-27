//! Evidence of a scan: one redacted row per request that left, and the rules an answer
//! triggers (truncation warning, the stop on an HTML or challenge page, the redirect warning and
//! the address to offer instead).

use std::{sync::atomic::Ordering, time::Duration};

use memchr::memmem;
use serde_json::Value;
use url::Url;

use super::{Call, Exec, Shared, StopReason, path_without_query};
use crate::llm::{
    safe_http::{SafeResponse, SafeTransportError},
    sniff::{
        identify::clean_field,
        types::{EvidenceItem, SniffEvent, SniffWarning},
    },
};
use spanbay_base::{crypto::sha256_hex, secrets::redact_secrets};
use spanbay_llm_domain::llm::sniff_summary::PROXY_DNS_NOTE;

/// Longest evidence note.
const NOTE_CHARS: usize = 200;
/// Hex characters of the body hash kept in an evidence row.
const BODY_HASH_CHARS: usize = 16;
/// A Chinese relay's "too many requests" sentence, sent with any status.
const RATE_LIMIT_TEXT: &str = "请求过于频繁";
/// Body texts of bot-challenge pages (Cloudflare's interstitial and block page).
const CHALLENGE_TEXTS: [&str; 2] = ["Just a moment", "Attention Required"];

/// The default sentence of a request: nothing beyond the status line.
pub(in crate::llm::sniff) fn no_description(_: &SafeResponse) -> Option<String> {
    None
}

/// Cleans a JSON string field for use in a note.
pub(in crate::llm::sniff) fn note_text(value: Option<&Value>) -> Option<String> {
    value.and_then(Value::as_str).map(clean_field)
}

fn error_kind(result: &Result<SafeResponse, SafeTransportError>) -> &'static str {
    match result {
        Ok(_) => "none",
        Err(error) => error.kind(),
    }
}

fn status_meaning(status: u16) -> &'static str {
    match status {
        200..=299 => "ok",
        300..=399 => "redirect, not followed",
        401 | 403 => "credential refused",
        404 => "not found",
        405 => "method not allowed",
        429 => "rate limited",
        400..=499 => "client error",
        _ => "server error",
    }
}

/// `Retry-After` in whole seconds when the header is a number.
pub(super) fn retry_after(response: &SafeResponse) -> Option<Duration> {
    response
        .header("retry-after")
        .and_then(|value| value.trim().parse::<u64>().ok())
        .map(Duration::from_secs)
}

/// Whether `needle` occurs in the body. Searching the raw bytes finds the same matches as
/// searching the lossy UTF-8 text: every needle is valid UTF-8 that starts on a character
/// boundary and contains no U+FFFD, and invalid sequences only ever become U+FFFD.
fn body_contains(response: &SafeResponse, needle: &str) -> bool {
    memmem::find(&response.body, needle.as_bytes()).is_some()
}

/// Whether the content type names JSON (`application/json`, `application/problem+json`, ...).
fn has_json_content_type(response: &SafeResponse) -> bool {
    response.header("content-type").is_some_and(|value| {
        value
            .as_bytes()
            .windows(4)
            .any(|window| window.eq_ignore_ascii_case(b"json"))
    })
}

/// Whether the body texts of a challenge or rate-limit page mean anything: not in a 2xx JSON
/// answer, whose data (a model description, say) may quote them.
fn body_texts_count(response: &SafeResponse) -> bool {
    !(response.is_success() && has_json_content_type(response))
}

/// Whether the body says "too many requests" in a relay's words (any status but a 2xx JSON
/// answer).
pub(super) fn is_rate_limit_body(response: &SafeResponse) -> bool {
    body_texts_count(response) && body_contains(response, RATE_LIMIT_TEXT)
}

/// Whether the answer is a bot-challenge page: a `403`/`503` marked by Cloudflare, or a body
/// with the text of a challenge page outside a 2xx JSON answer.
fn is_challenge(response: &SafeResponse) -> bool {
    let status_matches = matches!(response.status, 403 | 503);
    (status_matches && response.header("cf-mitigated").is_some())
        || (body_texts_count(response)
            && CHALLENGE_TEXTS
                .iter()
                .any(|text| body_contains(response, text)))
}

/// The columns of an evidence row that depend on the answer.
struct Outcome {
    status: Option<u16>,
    latency_ms: u64,
    body_hash: String,
    note: String,
}

/// The status, latency, body hash and unredacted note of `result`. `looks_html` is the answer's
/// [`SafeResponse::looks_like_html`].
fn outcome_of(
    result: &Result<SafeResponse, SafeTransportError>,
    measured: Duration,
    describe: fn(&SafeResponse) -> Option<String>,
    looks_html: bool,
) -> Outcome {
    match result {
        Ok(response) => {
            let mut note = format!("{} {}", response.status, status_meaning(response.status));
            if looks_html {
                note.push_str(", web page");
            }
            if let Some(extra) = describe(response) {
                note.push_str(", ");
                note.push_str(&extra);
            }
            Outcome {
                status: Some(response.status),
                latency_ms: response.elapsed_ms(),
                body_hash: sha256_hex(&response.body)[..BODY_HASH_CHARS].to_owned(),
                note,
            }
        }
        Err(error) => Outcome {
            status: None,
            latency_ms: u64::try_from(measured.as_millis()).unwrap_or(u64::MAX),
            body_hash: String::new(),
            note: format!("no answer: {}", error.kind()),
        },
    }
}

/// What an answer does to the scan: the truncation warning (once), and, unless HTML is
/// expected, the stop on the first challenge or 2xx HTML page.
fn apply_response_rules(shared: &mut Shared, truncated: bool, challenge: bool, html: bool) {
    shared.any_response = true;
    if truncated && !shared.warnings.contains(&SniffWarning::BodyTruncated) {
        shared.warnings.push(SniffWarning::BodyTruncated);
    }
    if shared.stop.is_some() {
        return;
    }
    if challenge {
        shared.stop = Some(StopReason::Challenge);
        shared.warnings.push(SniffWarning::ChallengePage);
    } else if html {
        shared.stop = Some(StopReason::Html);
        shared.warnings.push(SniffWarning::HtmlPage);
    }
}

/// The address to offer after a redirect from `requested` (a path without query) to `target`
/// on the typed host: the typed mount behind the target's scheme, port and path prefix; `None`
/// when the request was for the root or the target path does not end with the requested one.
fn suggested_url(target: &Url, host: &str, requested: &str, typed_mount: &str) -> Option<String> {
    let requested = requested.trim_end_matches('/');
    if requested.is_empty() {
        // A root request ends with every path: nothing maps back to the typed address.
        return None;
    }
    let pointed = target.path().trim_end_matches('/');
    let prefix = if pointed == requested {
        ""
    } else {
        pointed.strip_suffix(requested)?
    };
    let port = target
        .port()
        .map(|port| format!(":{port}"))
        .unwrap_or_default();
    Some(format!(
        "{}://{host}{port}{prefix}{typed_mount}",
        target.scheme()
    ))
}

impl Exec<'_> {
    /// Text built from response data, made safe for evidence and events: control characters
    /// removed, secrets and opaque URL path segments redacted by the one table of
    /// [`spanbay_base::secrets`], then cut.
    pub(in crate::llm::sniff) fn sanitize(&self, text: &str) -> String {
        let cleaned: String = text.chars().filter(|c| !c.is_control()).collect();
        let redacted = redact_secrets(&cleaned, &self.secrets);
        redacted.chars().take(NOTE_CHARS).collect()
    }

    /// Writes the evidence row, applies the redirect, truncation and stop rules, emits the
    /// finished event.
    pub(super) fn record(
        &self,
        id: u32,
        spec: &Call,
        result: &Result<SafeResponse, SafeTransportError>,
        measured: Duration,
    ) {
        let looks_html = result.as_ref().is_ok_and(SafeResponse::looks_like_html);
        let outcome = outcome_of(result, measured, spec.describe, looks_html);
        let mut note = self.sanitize(&outcome.note);
        if self.proxy_dns && !self.proxy_noted.swap(true, Ordering::SeqCst) {
            note.push_str(PROXY_DNS_NOTE);
        }
        let item = EvidenceItem {
            id,
            phase: spec.purpose.phase().to_owned(),
            method: spec.method.as_str().to_owned(),
            url: self.sanitize(&format!(
                "{}{}",
                self.origin,
                path_without_query(&spec.path)
            )),
            status: outcome.status,
            latency_ms: outcome.latency_ms,
            body_sha256_prefix: outcome.body_hash,
            note: note.clone(),
            conclusion: spec.purpose.conclusion(),
            error_kind: error_kind(result).to_owned(),
        };
        // Both stop checks are pure, so they are worked out before the lock is taken.
        let stop_checks = match result {
            Ok(response) if !spec.html_ok => {
                Some((is_challenge(response), response.is_success() && looks_html))
            }
            _ => None,
        };
        {
            let mut shared = self.shared();
            shared.open_rows.remove(&id);
            shared.evidence.push(item);
            match result {
                Ok(response) => {
                    let (challenge, html) = stop_checks.unwrap_or_default();
                    apply_response_rules(&mut shared, response.truncated, challenge, html);
                }
                Err(error) => {
                    shared.blocked |= matches!(error, SafeTransportError::Blocked { .. });
                    shared.first_error.get_or_insert_with(|| error.clone());
                }
            }
        }
        if let Ok(response) = result
            && response.is_redirect()
        {
            self.note_redirect(response, &spec.path);
        }
        self.emit(SniffEvent::RequestFinished {
            id,
            status: outcome.status,
            latency_ms: outcome.latency_ms,
            note,
        });
    }

    /// A 3xx is a warning with the target host (never the query). A redirect to the typed host
    /// also becomes the address to offer instead (see [`suggested_url`]).
    fn note_redirect(&self, response: &SafeResponse, path: &str) {
        let Some(location) = response.header("location") else {
            self.warn(SniffWarning::RedirectIgnored {
                location_host: None,
            });
            return;
        };
        let base = Url::parse(&format!("{}{path}", self.origin)).ok();
        let target = base
            .as_ref()
            .and_then(|base| base.join(location.trim()).ok())
            .or_else(|| Url::parse(location.trim()).ok());
        let host = target.as_ref().and_then(Url::host_str);
        self.warn(SniffWarning::RedirectIgnored {
            location_host: host.map(str::to_owned),
        });
        let (Some(target), Some(host)) = (&target, host) else {
            return;
        };
        let typed_host = base.as_ref().and_then(Url::host_str);
        if !typed_host.is_some_and(|typed| typed.eq_ignore_ascii_case(host))
            || !matches!(target.scheme(), "http" | "https")
        {
            return;
        }
        if let Some(url) = suggested_url(target, host, path_without_query(path), &self.typed_mount)
        {
            self.shared().suggested_url.get_or_insert(url);
        }
    }
}
