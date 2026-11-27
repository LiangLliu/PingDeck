//! Cookie jar matching, `Set-Cookie` parsing and the per-hop session cookie capture and replay.

use crate::executor::types::{CookieRejectReason, ExecutionCookie, ExecutionError, RejectedCookie};
use reqwest::header::{HeaderMap, HeaderValue, SET_COOKIE};
use spanbay_base::secrets::REDACTED;
use spanbay_model::CookieJarEntry;
use spanbay_model::models::{
    COOKIE_MAX_BYTES, cookie_domain_is_public_suffix, effective_cookie_domain,
    effective_cookie_path,
};
use spanbay_model::validation::WorkspaceError;
use std::time::{SystemTime, UNIX_EPOCH};
use url::Url;

const SESSION_COOKIE_WORKSPACE_ID: &str = "ws_execution";

/// Validates a composed `Cookie` header value.
pub(super) fn cookie_header_value(cookie_header: &str) -> Result<HeaderValue, ExecutionError> {
    HeaderValue::from_str(cookie_header).map_err(|error| ExecutionError::InvalidHeader {
        key: "Cookie".into(),
        message: error.to_string(),
    })
}

/// Records the `Set-Cookie` headers of one hop into the captured list and the session jar; a
/// header that is not kept goes to `rejected` with the reason.
pub(super) fn capture_hop_cookies(
    headers: &HeaderMap,
    url: &Url,
    captured_cookies: &mut Vec<ExecutionCookie>,
    session_cookies: &mut Vec<CookieJarEntry>,
    rejected: &mut Vec<RejectedCookie>,
) {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    for value in headers.get_all(SET_COOKIE).iter() {
        let reject = |name: &str, reason| RejectedCookie {
            name: name.to_owned(),
            host: host.clone(),
            reason,
        };
        // `to_str` rejects obs-text, so UTF-8 values are decoded from the raw bytes; anything
        // else is dropped rather than stored and replayed under a placeholder name.
        let Ok(value) = std::str::from_utf8(value.as_bytes()) else {
            rejected.push(reject("", CookieRejectReason::Malformed));
            continue;
        };
        let mut cookie = parse_set_cookie_header(value);
        if cookie.name.is_empty() {
            rejected.push(reject("", CookieRejectReason::Malformed));
            continue;
        }
        if let Err(reason) = resolve_cookie_scope(&mut cookie, url, &host) {
            rejected.push(reject(&cookie.name, reason));
            continue;
        }
        let size = cookie.name.len()
            + cookie.value.len()
            + cookie.domain.as_deref().map_or(0, str::len)
            + cookie.path.as_deref().map_or(0, str::len);
        if size > COOKIE_MAX_BYTES {
            rejected.push(reject(&cookie.name, CookieRejectReason::TooLarge));
            continue;
        }
        record_captured_cookie(captured_cookies, &cookie);
        apply_session_cookie(session_cookies, &cookie);
    }
}

/// Resolves the cookie's domain and path against the hop; the reason when the response tried
/// to set a cookie for a domain it does not own.
fn resolve_cookie_scope(
    cookie: &mut ExecutionCookie,
    url: &Url,
    host: &str,
) -> Result<(), CookieRejectReason> {
    let explicit_domain = cookie
        .domain
        .as_deref()
        .map(str::trim)
        .filter(|domain| !domain.is_empty());
    let (domain, host_only) =
        effective_cookie_domain(explicit_domain, host).map_err(|error| match error {
            WorkspaceError::CookiePublicSuffixDomain { .. } => CookieRejectReason::PublicSuffix,
            WorkspaceError::CookieDomainMismatch { .. } => CookieRejectReason::ForeignDomain,
            _ => CookieRejectReason::Malformed,
        })?;
    cookie.domain = Some(domain);
    cookie.host_only = host_only;
    cookie.path = Some(effective_cookie_path(cookie.path.as_deref(), url.path()));
    Ok(())
}

/// A sent `Cookie` header with every value masked and the names kept (`a=<REDACTED>; b=…`), so
/// the sent view shows which cookies went out without their values.
pub(super) fn mask_cookie_header(value: &str) -> String {
    value
        .split(';')
        .map(str::trim)
        .filter(|pair| !pair.is_empty())
        .map(|pair| match pair.split_once('=') {
            Some((name, _)) => format!("{}={REDACTED}", name.trim()),
            None => REDACTED.to_owned(),
        })
        .collect::<Vec<_>>()
        .join("; ")
}

fn record_captured_cookie(captured_cookies: &mut Vec<ExecutionCookie>, cookie: &ExecutionCookie) {
    let existing = captured_cookies.iter_mut().find(|existing| {
        existing.name == cookie.name
            && existing.domain == cookie.domain
            && existing.path == cookie.path
    });

    match existing {
        Some(existing) => *existing = cookie.clone(),
        None => captured_cookies.push(cookie.clone()),
    }
}

fn apply_session_cookie(session_cookies: &mut Vec<CookieJarEntry>, captured: &ExecutionCookie) {
    let Some(domain) = captured.domain.as_deref() else {
        return;
    };

    session_cookies.retain(|cookie| {
        !(cookie.name == captured.name
            && cookie.domain == domain
            && Some(cookie.path.as_str()) == captured.path.as_deref())
    });

    let max_age_expired = captured
        .max_age
        .as_deref()
        .and_then(|max_age| max_age.trim().parse::<i64>().ok())
        .is_some_and(|max_age| max_age <= 0);
    if max_age_expired {
        return;
    }

    let timestamp = now_millis().to_string();
    let entry = CookieJarEntry::from_resolved_cookie(
        SESSION_COOKIE_WORKSPACE_ID,
        None,
        &captured.name,
        captured.value.clone(),
        domain,
        captured.path.as_deref().unwrap_or("/"),
        captured.host_only,
        captured.expires.as_deref(),
        captured.max_age.as_deref(),
        captured.secure,
        captured.http_only,
        captured.same_site.as_deref(),
        &timestamp,
    );

    if let Ok(entry) = entry {
        session_cookies.push(entry);
    }
}

fn now_millis() -> u128 {
    SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map_or(0, |elapsed| elapsed.as_millis())
}

/// Composes the `Cookie` header for `url` from the unexpired matching jar entries. `direct` is
/// whether the request dials the origin itself (no proxy), see [`cookie_matches_url`].
pub(super) fn cookie_header_for_url(
    url: &Url,
    cookie_jar: &[CookieJarEntry],
    direct: bool,
) -> Option<String> {
    let now = std::time::SystemTime::now();
    let mut matching_cookies = cookie_jar
        .iter()
        .filter(|cookie| !cookie_is_expired(cookie, now))
        .filter(|cookie| cookie_matches_url(cookie, url, direct))
        .collect::<Vec<_>>();

    if matching_cookies.is_empty() {
        return None;
    }

    matching_cookies.sort_by(|left, right| {
        right
            .path
            .len()
            .cmp(&left.path.len())
            .then_with(|| left.created_at.cmp(&right.created_at))
            .then_with(|| left.name.cmp(&right.name))
    });

    Some(
        matching_cookies
            .into_iter()
            .map(|cookie| format!("{}={}", cookie.name, cookie.value))
            .collect::<Vec<_>>()
            .join("; "),
    )
}

/// Whether the cookie has expired at `now` by `Max-Age` or `Expires`.
pub(crate) fn cookie_is_expired(cookie: &CookieJarEntry, now: std::time::SystemTime) -> bool {
    cookie.is_expired_at(now)
}

/// Whether the cookie's domain, path and secure flag allow sending it to `url`. A `Secure` cookie
/// goes over plain HTTP only to a loopback host the request dials itself (`direct`): through a
/// proxy the cleartext would cross the proxy.
pub(crate) fn cookie_matches_url(cookie: &CookieJarEntry, url: &Url, direct: bool) -> bool {
    let Some(host) = url.host_str().map(str::to_ascii_lowercase) else {
        return false;
    };
    if cookie.secure && url.scheme() != "https" && !(direct && is_loopback_host(&host)) {
        return false;
    }

    let domain = cookie.domain.to_ascii_lowercase();
    if !cookie.host_only && cookie_domain_is_public_suffix(&domain) {
        return false;
    }
    let domain_matches = if cookie.host_only {
        host == domain
    } else {
        host == domain || host.ends_with(&format!(".{domain}"))
    };

    domain_matches && cookie_path_matches(&cookie.path, url.path())
}

/// A loopback host is a secure context for `Secure` cookies: `localhost`, `127.0.0.0/8` and `::1`.
/// `*.localhost` is not: unlike a browser, the resolver does not pin it to loopback, so it may
/// reach whatever DNS answers. `host` is lowercase, an IPv6 literal in brackets.
fn is_loopback_host(host: &str) -> bool {
    host == "localhost"
        || host
            .trim_start_matches('[')
            .trim_end_matches(']')
            .parse::<std::net::IpAddr>()
            .is_ok_and(|ip| ip.is_loopback())
}

fn cookie_path_matches(cookie_path: &str, request_path: &str) -> bool {
    cookie_path == "/"
        || request_path == cookie_path
        || (request_path.starts_with(cookie_path)
            && (cookie_path.ends_with('/')
                || request_path.as_bytes().get(cookie_path.len()) == Some(&b'/')))
}

/// Parses one `Set-Cookie` header value into a captured cookie using `cookie::Cookie::parse`.
pub(crate) fn parse_set_cookie_header(value: &str) -> ExecutionCookie {
    if let Ok(parsed) = cookie::Cookie::parse(value) {
        let mut raw_path = parsed.path().map(str::to_owned);
        let mut raw_expires = None;
        for segment in value.split(';').skip(1) {
            if let Some((k, v)) = segment.trim().split_once('=') {
                let k = k.trim();
                let v = v.trim();
                if raw_path.is_none() && k.eq_ignore_ascii_case("path") {
                    raw_path = Some(v.to_owned());
                } else if k.eq_ignore_ascii_case("expires") {
                    raw_expires = Some(v.to_owned());
                }
            }
        }
        return ExecutionCookie {
            name: parsed.name().to_owned(),
            value: parsed.value().to_owned(),
            domain: parsed.domain().map(str::to_owned),
            path: raw_path,
            expires: raw_expires,
            max_age: parsed.max_age().map(|d| d.whole_seconds().to_string()),
            secure: parsed.secure().unwrap_or(false),
            http_only: parsed.http_only().unwrap_or(false),
            same_site: parsed.same_site().map(|s| s.to_string()),
            host_only: false,
        };
    }

    let (name, cookie_value) = value
        .split(';')
        .next()
        .unwrap_or_default()
        .trim()
        .split_once('=')
        .map(|(n, v)| (n.trim().to_owned(), v.trim().to_owned()))
        .unwrap_or_default();
    ExecutionCookie {
        name,
        value: cookie_value,
        domain: None,
        path: None,
        expires: None,
        max_age: None,
        secure: false,
        http_only: false,
        same_site: None,
        host_only: false,
    }
}
