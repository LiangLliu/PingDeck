//! Two small helpers around a finished scan: duplicate detection and "Copy as curl".
//!
//! The normalised provider identity (scheme, punycode host, port, primary mount) and the `curl`
//! line with the key replaced by `$PROVIDER_API_KEY`.

use url::Url;

use crate::llm::wire::{AuthStyle, Protocol, Quirks, request_headers};

/// The name of the environment variable the curl line expects.
pub const CURL_KEY_VARIABLE: &str = "$PROVIDER_API_KEY";

/// The identity two providers share when they are duplicates: scheme, host
/// (case-insensitive, punycode), port and the mount path, trailing slashes ignored.
/// `base_url` is a stored provider's base URL, `path_prefix` its primary endpoint's prefix.
#[must_use]
pub fn duplicate_key(base_url: &str, path_prefix: &str) -> Option<String> {
    let joined = format!(
        "{}/{}",
        base_url.trim().trim_end_matches('/'),
        path_prefix.trim().trim_matches('/')
    );
    let url = Url::parse(&joined).ok()?;
    if !matches!(url.scheme(), "http" | "https") {
        return None;
    }
    let port = url.port_or_known_default()?;
    let path: Vec<&str> = url
        .path()
        .split('/')
        .filter(|segment| !segment.is_empty())
        .collect();
    Some(format!(
        "{}://{}:{port}/{}",
        url.scheme(),
        url.host_str()?,
        path.join("/")
    ))
}

/// A `curl` line for the first model-list request; the key is `$PROVIDER_API_KEY` (a typed key
/// is never part of the text). Header values stay in double quotes so a shell expands the
/// variable.
#[must_use]
pub(super) fn models_curl(
    url: &str,
    protocol: Protocol,
    style: AuthStyle,
    has_key: bool,
) -> String {
    let secret = if has_key { CURL_KEY_VARIABLE } else { "" };
    let mut line = format!(
        "curl -sS '{}' -H \"Accept: application/json\"",
        url.replace('\'', "%27")
    );
    for (name, value) in request_headers(protocol, style, secret, &Quirks::default()) {
        line.push_str(" -H \"");
        line.push_str(&name);
        line.push_str(": ");
        line.push_str(&value.replace('"', "\\\""));
        line.push('"');
    }
    line
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn duplicates_ignore_case_default_ports_and_trailing_slashes() {
        let first = duplicate_key("https://relay.acme.dev/", "/v1");
        assert_eq!(first.as_deref(), Some("https://relay.acme.dev:443/v1"));
        assert_eq!(first, duplicate_key("HTTPS://RELAY.acme.dev:443", "/v1/"));
        assert_eq!(first, duplicate_key("https://relay.acme.dev/v1", ""));
        assert_ne!(first, duplicate_key("https://relay.acme.dev", "/v2"));
        assert_ne!(first, duplicate_key("http://relay.acme.dev", "/v1"));
        assert_ne!(first, duplicate_key("https://relay.acme.dev:8443", "/v1"));
        assert_eq!(
            duplicate_key("https://münchen.test", "").as_deref(),
            Some("https://xn--mnchen-3ya.test:443/")
        );
        assert_eq!(duplicate_key("ftp://x.test", ""), None);
        assert_eq!(duplicate_key("not a url", ""), None);
    }

    #[test]
    fn curl_lines_carry_the_placeholder_never_a_key() {
        let line = models_curl(
            "https://relay.acme.dev/v1/models",
            Protocol::OpenaiChat,
            AuthStyle::Bearer,
            true,
        );
        assert_eq!(
            line,
            "curl -sS 'https://relay.acme.dev/v1/models' -H \"Accept: application/json\" -H \"authorization: Bearer $PROVIDER_API_KEY\""
        );
        let anthropic = models_curl(
            "https://x.test/v1/models?limit=1000",
            Protocol::AnthropicMessages,
            AuthStyle::XApiKey,
            true,
        );
        assert!(
            anthropic.contains("x-api-key: $PROVIDER_API_KEY")
                && anthropic.contains("anthropic-version: 2023-06-01")
        );
        let keyless = models_curl(
            "https://x.test/v1/models",
            Protocol::OpenaiChat,
            AuthStyle::None,
            false,
        );
        assert!(!keyless.contains("PROVIDER_API_KEY"));
        assert!(
            models_curl(
                "https://x.test/a'b",
                Protocol::OpenaiChat,
                AuthStyle::None,
                false
            )
            .contains("a%27b")
        );
    }
}
