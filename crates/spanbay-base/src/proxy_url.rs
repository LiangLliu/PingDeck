//! Proxy URLs whose password lives in the secret store.
//!
//! Single responsibility: split a proxy URL into the part that may be stored, shown and exported
//! (`http://user:****@host:port`) and its password, and put the two back together. Settings and
//! saved requests keep only the masked form on disk and in every DTO; the real URL exists in
//! memory (for the HTTP clients) and the password in the `secrets` table under the references
//! below. A URL that carries the mask as its password is "untouched": the stored password stays.

use serde::Serializer;
use url::Url;

/// What a stored or displayed proxy password is replaced with.
pub const MASK: &str = "****";

/// Secret reference of the password of `network.proxy_url` (Settings).
pub const SETTINGS_PASSWORD_SECRET: &str = "network.proxy_password";

/// Secret reference of the proxy password of request `request_id`.
#[must_use]
pub fn request_secret(request_id: &str) -> String {
    format!("request.{request_id}.proxy_password")
}

/// A proxy URL split for storage.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct Split {
    /// The URL with its password replaced by [`MASK`]; equal to the input when it had none.
    pub masked: String,
    /// The password as the URL spells it (percent-encoded); `None` when the URL has no password
    /// or already carries the mask.
    pub password: Option<String>,
    /// The URL has a password and it is the mask, so the stored password applies.
    pub is_masked: bool,
}

/// Splits `url` (a scheme-less `user:pass@host:port` is read as `http://`, as `reqwest` does).
/// A value that cannot be parsed is returned as is: it cannot be a working proxy either.
#[must_use]
pub fn split(url: &str) -> Split {
    let untouched = || Split {
        masked: url.to_owned(),
        password: None,
        is_masked: false,
    };
    let has_scheme = url.contains("://");
    let candidate = if has_scheme {
        url.to_owned()
    } else {
        format!("http://{url}")
    };
    let Ok(mut parsed) = Url::parse(&candidate) else {
        return untouched();
    };
    let Some(password) = parsed.password().map(str::to_owned) else {
        return untouched();
    };
    if password == MASK {
        return Split {
            masked: url.to_owned(),
            password: None,
            is_masked: true,
        };
    }
    if parsed.set_password(Some(MASK)).is_err() {
        return untouched();
    }
    Split {
        masked: render(&parsed, url),
        password: Some(password),
        is_masked: false,
    }
}

/// `parsed` written the way `original` was: without the `http://` it lacked and without the
/// trailing slash `Url` adds to a bare authority.
fn render(parsed: &Url, original: &str) -> String {
    let mut text = parsed.to_string();
    if !original.contains("://") {
        text = text.strip_prefix("http://").unwrap_or(&text).to_owned();
    }
    // `Url` writes a bare authority as `host:port/`: drop the slash the original did not have,
    // also in front of a query (`host:port?x` stays `host:port?x`).
    let after_scheme = original
        .split_once("://")
        .map_or(original, |(_, rest)| rest);
    if parsed.path() == "/" && !after_scheme.contains('/') {
        let start = text.find("://").map_or(0, |scheme_end| scheme_end + 3);
        if let Some(slash) = text[start..].find('/') {
            text.remove(start + slash);
        }
    }
    text
}

/// `masked` with `password` (as [`split`] returned it) put back; `masked` unchanged when it has
/// no mask to replace.
#[must_use]
pub fn restore(masked: &str, password: &str) -> String {
    let candidate = if masked.contains("://") {
        masked.to_owned()
    } else {
        format!("http://{masked}")
    };
    let Ok(mut parsed) = Url::parse(&candidate) else {
        return masked.to_owned();
    };
    if parsed.password() != Some(MASK) || parsed.set_password(Some(password)).is_err() {
        return masked.to_owned();
    }
    render(&parsed, masked)
}

/// `url` with its password (masked or not) removed and the user kept, so neither a stale stored
/// password nor the literal mask can be sent; `url` unchanged when it has no password.
#[must_use]
pub fn without_password(url: &str) -> String {
    let candidate = if url.contains("://") {
        url.to_owned()
    } else {
        format!("http://{url}")
    };
    let Ok(mut parsed) = Url::parse(&candidate) else {
        return url.to_owned();
    };
    if parsed.password().is_none() || parsed.set_password(None).is_err() {
        return url.to_owned();
    }
    render(&parsed, url)
}

/// The masked form of `url`, for display.
#[must_use]
pub fn mask(url: &str) -> String {
    split(url).masked
}

/// Serializes a proxy URL masked, so a settings row, a DTO and an export never carry the password
/// (`#[serde(serialize_with = "proxy_url::serialize_masked")]`).
///
/// # Errors
/// Whatever the serializer reports.
pub fn serialize_masked<S: Serializer>(url: &str, serializer: S) -> Result<S::Ok, S::Error> {
    serializer.serialize_str(&mask(url))
}

/// [`serialize_masked`] for an optional URL.
///
/// # Errors
/// Whatever the serializer reports.
pub fn serialize_masked_option<S: Serializer>(
    url: &Option<String>,
    serializer: S,
) -> Result<S::Ok, S::Error> {
    match url {
        Some(url) => serializer.serialize_some(&mask(url)),
        None => serializer.serialize_none(),
    }
}

/// The proxy value that means "no proxy at all, not even the system's". An empty value
/// means "follow": a provider follows Settings, Settings follows the system proxy.
pub const DIRECT: &str = "direct";

/// Where one outbound request goes.
#[derive(Debug, Clone, PartialEq, Eq, Hash)]
pub enum ProxyRoute {
    /// No proxy, not even the system's.
    Direct,
    /// The operating system's proxy settings (reqwest's default).
    System,
    /// This explicit proxy URL.
    Proxy(String),
}

impl ProxyRoute {
    /// Stable name for traces and failure cards: `direct`, `system` or `custom`.
    #[must_use]
    pub const fn name(&self) -> &'static str {
        match self {
            Self::Direct => "direct",
            Self::System => "system",
            Self::Proxy(_) => "custom",
        }
    }
}

/// The one proxy rule of every outbound client (LLM runtime, safe transport, request executor):
/// the caller's own proxy (`own`: a provider's or a request's) first, `direct` meaning none;
/// else a local host or one on `no_proxy` goes direct; else the global proxy (`direct`: none;
/// empty: the system proxy).
#[must_use]
pub fn route(own: &str, global: &str, no_proxy: &[String], host: &str) -> ProxyRoute {
    let own = own.trim();
    let global = global.trim();
    if own.eq_ignore_ascii_case(DIRECT) {
        ProxyRoute::Direct
    } else if !own.is_empty() {
        ProxyRoute::Proxy(own.to_owned())
    } else if crate::net_guard::bypasses_global_proxy(host, no_proxy)
        || global.eq_ignore_ascii_case(DIRECT)
    {
        ProxyRoute::Direct
    } else if global.is_empty() {
        ProxyRoute::System
    } else {
        ProxyRoute::Proxy(global.to_owned())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn one_rule_picks_the_route() {
        let no_proxy = vec!["corp.example".to_owned()];
        let table = [
            ("", "", "api.openai.com", ProxyRoute::System),
            ("", "direct", "api.openai.com", ProxyRoute::Direct),
            (
                "",
                "http://p:1",
                "api.openai.com",
                ProxyRoute::Proxy("http://p:1".into()),
            ),
            ("", "http://p:1", "127.0.0.1", ProxyRoute::Direct),
            ("", "", "git.corp.example", ProxyRoute::Direct),
            (
                "socks5h://s:1",
                "",
                "localhost",
                ProxyRoute::Proxy("socks5h://s:1".into()),
            ),
            ("DIRECT", "http://p:1", "api.openai.com", ProxyRoute::Direct),
        ];
        for (own, global, host, expected) in table {
            assert_eq!(
                route(own, global, &no_proxy, host),
                expected,
                "{own} {global} {host}"
            );
        }
    }

    #[test]
    fn a_password_is_split_off_and_put_back() {
        let split = split("http://u:pw-456@proxy:8080");
        assert_eq!(split.masked, "http://u:****@proxy:8080");
        assert_eq!(split.password.as_deref(), Some("pw-456"));
        assert!(!split.is_masked);
        assert_eq!(
            restore(&split.masked, "pw-456"),
            "http://u:pw-456@proxy:8080"
        );
    }

    #[test]
    fn urls_without_a_password_and_masked_urls_are_left_alone() {
        for url in [
            "http://proxy:8080",
            "http://user@proxy:8080",
            "",
            "not a url",
        ] {
            let split = split(url);
            assert_eq!(split.masked, url);
            assert_eq!(split.password, None);
            assert!(!split.is_masked);
        }
        let masked = split("socks5://u:****@proxy:1080");
        assert!(masked.is_masked);
        assert_eq!(masked.password, None);
        assert_eq!(masked.masked, "socks5://u:****@proxy:1080");
    }

    #[test]
    fn a_scheme_less_url_keeps_its_shape() {
        let split = split("u:pw-456@proxy:8080");
        assert!(!split.masked.contains("pw-456"), "{}", split.masked);
        assert!(split.masked.starts_with("u:****@proxy"), "{}", split.masked);
        assert_eq!(split.password.as_deref(), Some("pw-456"));
        assert!(restore(&split.masked, "pw-456").contains("pw-456"));
    }

    #[test]
    fn without_password_keeps_the_user_and_shape() {
        assert_eq!(
            without_password("http://u:****@proxy:8080"),
            "http://u@proxy:8080"
        );
        assert_eq!(without_password("u:pw@proxy:8080"), "u@proxy:8080");
        assert_eq!(without_password("http://proxy:8080"), "http://proxy:8080");
    }

    #[test]
    fn special_characters_survive_the_round_trip() {
        let split = split("http://u:p%40ss%3Aw@proxy");
        assert_eq!(split.password.as_deref(), Some("p%40ss%3Aw"));
        assert_eq!(
            restore(&split.masked, "p%40ss%3Aw"),
            "http://u:p%40ss%3Aw@proxy"
        );
    }
}
