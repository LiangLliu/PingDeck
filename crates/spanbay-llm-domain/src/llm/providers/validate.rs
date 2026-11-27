//! Checks of user-typed provider fields: names, proxy URL, base URL, relative paths and extra
//! headers. Every error is an [`AppError::Validation`] that never echoes a credential.

use std::collections::BTreeMap;

use reqwest::header::{HeaderName, HeaderValue};

use spanbay_base::error::{AppError, ErrorCode};

/// Longest provider or key name, in characters.
pub const MAX_NAME_CHARS: usize = 60;
/// Longest key note, in characters.
pub const MAX_KEY_NOTES_CHARS: usize = 200;

/// Checks a provider or key name: 1 to [`MAX_NAME_CHARS`] characters after trimming.
///
/// # Errors
/// [`AppError::Validation`] naming `what` when the name is blank or too long.
pub fn validate_name(what: &str, name: &str) -> Result<(), AppError> {
    let length = name.trim().chars().count();
    if length == 0 {
        return Err(AppError::validation(format!("a {what} needs a name")));
    }
    if length > MAX_NAME_CHARS {
        return Err(AppError::validation(format!(
            "a {what} name has at most {MAX_NAME_CHARS} characters"
        )));
    }
    Ok(())
}

/// Checks the characters of a provider name: it appears verbatim in `<name>/<model>` references
/// and client config files, so no `/`, quotes, backslash or control characters.
///
/// # Errors
/// [`AppError::Validation`] naming the first character that is not allowed; coded
/// [`ErrorCode::NameHasSlash`] for `/`.
pub fn validate_provider_chars(name: &str) -> Result<(), AppError> {
    match name
        .chars()
        .find(|ch| matches!(ch, '/' | '"' | '\'' | '`' | '\\') || ch.is_control())
    {
        Some('/') => Err(AppError::validation("a provider name cannot contain '/'")
            .coded(ErrorCode::NameHasSlash, None)),
        Some(ch) => Err(AppError::validation(format!(
            "a provider name cannot contain {ch:?}"
        ))),
        None => Ok(()),
    }
}

/// Checks a proxy value: empty follows Settings, `direct` uses no proxy, anything else
/// must be a proxy URL.
///
/// # Errors
/// [`AppError::Validation`] when the URL is not an `http`, `https`, `socks5` or `socks5h` proxy
/// URL the HTTP client accepts.
pub fn validate_proxy_url(proxy_url: &str) -> Result<(), AppError> {
    let proxy_url = proxy_url.trim();
    if proxy_url.is_empty() || proxy_url.eq_ignore_ascii_case(spanbay_base::proxy_url::DIRECT) {
        return Ok(());
    }
    let invalid = || {
        AppError::validation(
            "invalid proxy URL: use http://, https://, socks5:// or socks5h:// with a host",
        )
    };
    let usable = url::Url::parse(proxy_url).is_ok_and(|parsed| {
        matches!(parsed.scheme(), "http" | "https" | "socks5" | "socks5h")
            && parsed.host_str().is_some()
    });
    if !usable {
        return Err(invalid());
    }
    reqwest::Proxy::all(proxy_url)
        .map(|_| ())
        .map_err(|_| invalid())
}

/// `raw` with any `user:password@` replaced by `***@`; text that is not a URL is never echoed.
#[must_use]
pub fn redact_proxy_url(raw: &str) -> String {
    let Some(mut parsed) = url::Url::parse(raw.trim())
        .ok()
        .filter(|parsed| parsed.host_str().is_some())
    else {
        return "invalid proxy URL".to_owned();
    };
    if !parsed.username().is_empty() || parsed.password().is_some() {
        let _ = parsed.set_username("***");
        let _ = parsed.set_password(None);
    }
    parsed.to_string()
}

/// Requires an `http(s)` URL with a host and no credentials, query or fragment, whose host is
/// not link-local, a cloud metadata address or name, multicast, unspecified or reserved.
///
/// # Errors
/// [`AppError::Validation`] when the URL does not parse, is not `http(s)` with a host, carries
/// credentials, a query or a fragment, or names a link-local, metadata or reserved host.
pub fn validate_base_url(raw: &str) -> Result<(), AppError> {
    let parsed = url::Url::parse(raw.trim())
        .map_err(|_| AppError::validation("the base URL is not a valid URL"))?;
    if !matches!(parsed.scheme(), "http" | "https") || parsed.host_str().is_none() {
        return Err(AppError::validation(
            "the base URL must start with http:// or https:// and name a host",
        ));
    }
    if !parsed.username().is_empty() || parsed.password().is_some() {
        return Err(AppError::validation(
            "the base URL must not contain credentials; add the key as a provider key",
        ));
    }
    if parsed.query().is_some() || parsed.fragment().is_some() {
        return Err(AppError::validation(
            "the base URL must not contain a query or fragment",
        ));
    }
    if parsed
        .host_str()
        .and_then(spanbay_base::net_guard::classify_host_literal)
        == Some(spanbay_base::net_guard::HostClass::LinkLocalOrMetadata)
    {
        return Err(AppError::validation(
            "the base URL points at a link-local, cloud metadata or reserved address",
        ));
    }
    Ok(())
}

/// Requires a plain relative path (no scheme or authority, no fragment, whitespace or control
/// characters) that is appended to the base URL.
///
/// # Errors
/// [`AppError::Validation`] naming `what` when the path has a scheme or authority, a fragment, a
/// backslash, whitespace or a control character.
pub fn validate_relative_path(what: &str, path: &str) -> Result<(), AppError> {
    let path = path.trim();
    let bad = path.contains("://")
        || path.starts_with("//")
        || path.contains(['#', '\\'])
        || path.chars().any(|c| c.is_whitespace() || c.is_control());
    if bad {
        return Err(AppError::validation(format!(
            "{what} must be a path relative to the base URL"
        )));
    }
    Ok(())
}

/// Most extra headers a provider may add to its requests.
pub const MAX_EXTRA_HEADERS: usize = 32;
/// Longest extra header value, in bytes.
pub const MAX_HEADER_VALUE_BYTES: usize = 4096;
/// Headers owned by the HTTP client, the protocol adapters or the provider's auth style.
const RESERVED_HEADERS: [&str; 17] = [
    "host",
    "content-length",
    "content-type",
    "accept",
    "transfer-encoding",
    "connection",
    "keep-alive",
    "proxy-authorization",
    "proxy-connection",
    "te",
    "trailer",
    "upgrade",
    "authorization",
    "x-api-key",
    "x-goog-api-key",
    "api-key",
    "anthropic-version",
];

/// Checks the provider's extra request headers (names, values, count, size, reserved names).
///
/// # Errors
/// [`AppError::Validation`] when there are more than [`MAX_EXTRA_HEADERS`] headers, a name is not
/// a valid header name or is reserved, or a value is invalid or longer than
/// [`MAX_HEADER_VALUE_BYTES`].
pub fn validate_extra_headers(headers: &BTreeMap<String, String>) -> Result<(), AppError> {
    if headers.len() > MAX_EXTRA_HEADERS {
        return Err(AppError::validation(format!(
            "at most {MAX_EXTRA_HEADERS} extra headers are allowed"
        )));
    }
    for (name, value) in headers {
        let header = HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| AppError::validation(format!("{name:?} is not a valid header name")))?;
        if RESERVED_HEADERS.contains(&header.as_str()) {
            return Err(AppError::validation(format!(
                "the {header} header is set by SpanBay (use the provider's auth style and \
                 anthropic version instead) and cannot be an extra header"
            )));
        }
        if value.len() > MAX_HEADER_VALUE_BYTES || HeaderValue::from_str(value).is_err() {
            return Err(AppError::validation(format!(
                "the value of the {header} header is invalid or longer than {MAX_HEADER_VALUE_BYTES} bytes"
            )));
        }
    }
    Ok(())
}

#[cfg(test)]
mod tests {
    use super::{redact_proxy_url, validate_proxy_url};

    #[test]
    fn proxy_urls_are_checked_and_never_echoed() {
        for bad in [
            "ftp://user:hunter2@h:1",
            "http://",
            "user:hunter2@h:1",
            "socks4://u:hunter2@h",
        ] {
            let error = validate_proxy_url(bad).unwrap_err();
            assert!(!error.message().contains("hunter2"), "{bad}: {error}");
        }
        assert!(validate_proxy_url("http://user:hunter2@h:1").is_ok());
        for ok in [
            "",
            "http://h:1",
            "https://h:1",
            "socks5://h:1080",
            "socks5h://h:1080",
        ] {
            assert!(validate_proxy_url(ok).is_ok(), "{ok}");
        }
        assert_eq!(
            redact_proxy_url("http://user:hunter2@proxy.test:8080"),
            "http://***@proxy.test:8080/"
        );
        assert_eq!(
            redact_proxy_url("http://proxy.test:8080"),
            "http://proxy.test:8080/"
        );
        assert_eq!(
            redact_proxy_url("user:hunter2@nonsense"),
            "invalid proxy URL"
        );
    }
}
