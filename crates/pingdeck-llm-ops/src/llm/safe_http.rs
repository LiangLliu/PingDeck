//! The injectable, SSRF-safe HTTP transport of the sniffer, the balance adapters and custom-target
//! AI requests: a bounded request/response exchange ([`SafeRequest`], [`SafeResponse`],
//! [`SafeHttp`]) with a scripted double for tests (`scripted`, `test-support` feature).
//! No redirect is ever followed (a 3xx is returned as a plain response), bodies are capped, every
//! request carries a time limit, and the production client ([`client::ReqwestSafeHttp`]) checks
//! every target address with [`pingdeck_base::net_guard`] before it connects.

use std::time::Duration;

use futures_util::future::BoxFuture;

use pingdeck_base::cancel::CancelToken;

pub mod client;
/// Scripted HTTP fakes for tests of the balance and probe layers (`test-support` feature).
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod scripted;
pub mod stream;
#[cfg(test)]
mod test_server;

/// HTTP method of a safe request (the safe surface only needs these two).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum SafeMethod {
    /// `GET`.
    Get,
    /// `POST`.
    Post,
}

impl SafeMethod {
    /// The method name as sent on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Get => "GET",
            Self::Post => "POST",
        }
    }
}

/// One request. Credentials live in `headers`; nothing here is ever logged by the transport.
#[derive(Clone)]
pub struct SafeRequest {
    /// Method.
    pub method: SafeMethod,
    /// Absolute `http(s)` URL.
    pub url: String,
    /// Request headers in send order (names as given; the transport adds none except the ones
    /// the HTTP stack needs).
    pub headers: Vec<(String, String)>,
    /// Body of a `POST`.
    pub body: Option<Vec<u8>>,
    /// Time limit of the exchange once the target is admitted: sending, the response head and the
    /// body transfer (a stream applies its own [`stream::StreamLimits`] instead).
    pub timeout: Duration,
    /// Time limit of name resolution on the direct route, and separately of connecting (TCP and
    /// TLS).
    pub connect_timeout: Duration,
    /// Response bytes kept; more is cut (`SafeResponse::truncated`).
    pub max_body: usize,
    /// Whether loopback, private (RFC 1918, ULA, site-local) and CGNAT (`100.64.0.0/10`,
    /// Tailscale) targets are allowed; a name is judged by the addresses it resolves to, so a
    /// `.lan` or `.local` host on a private network qualifies. Link-local, metadata, multicast,
    /// unspecified, reserved and documentation targets are refused regardless.
    pub allow_private: bool,
}

/// Header values, URL credentials and the query may be secrets: `Debug` prints header names
/// only and the URL without its userinfo, query and fragment.
impl std::fmt::Debug for SafeRequest {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SafeRequest")
            .field("method", &self.method)
            .field("url", &url_for_debug(&self.url))
            .field(
                "header_names",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .field("body_bytes", &self.body.as_ref().map(Vec::len))
            .field("timeout", &self.timeout)
            .field("max_body", &self.max_body)
            .field("allow_private", &self.allow_private)
            .finish()
    }
}

/// `raw` without userinfo, query and fragment; text that is not a URL is never echoed.
fn url_for_debug(raw: &str) -> String {
    let Ok(mut url) = url::Url::parse(raw) else {
        return "<not a URL>".to_owned();
    };
    // Both setters only fail for URLs that cannot carry credentials, which then have none.
    let _ = url.set_username("");
    let _ = url.set_password(None);
    url.set_query(None);
    url.set_fragment(None);
    url.into()
}

/// The value of the first header named `name` (case-insensitive) in `headers`.
pub(crate) fn find_header<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(candidate, _)| candidate.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// A response whose body was read up to [`SafeRequest::max_body`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct SafeResponse {
    /// Status code (a 3xx is returned as is, never followed).
    pub status: u16,
    /// Response headers with lower-case names, in arrival order.
    pub headers: Vec<(String, String)>,
    /// Body bytes read (at most `max_body`).
    pub body: Vec<u8>,
    /// Whether the body was cut at `max_body`.
    pub truncated: bool,
    /// Time from sending the request to the end of the body. The production transport starts the
    /// clock after the target was admitted, its name resolved and the client built, so this is
    /// the exchange alone (unlike the `total` limit of [`client::ReqwestSafeHttp::open_stream`],
    /// which counts from the start of the call); the scripted double reports its delay.
    pub elapsed: Duration,
}

impl SafeResponse {
    /// The first header named `name` (case-insensitive).
    #[must_use]
    pub fn header(&self, name: &str) -> Option<&str> {
        find_header(&self.headers, name)
    }

    /// [`SafeResponse::elapsed`] in whole milliseconds, saturating at `u64::MAX`.
    #[must_use]
    pub fn elapsed_ms(&self) -> u64 {
        u64::try_from(self.elapsed.as_millis()).unwrap_or(u64::MAX)
    }

    /// The body as lossy UTF-8.
    #[must_use]
    pub fn text(&self) -> String {
        String::from_utf8_lossy(&self.body).into_owned()
    }

    /// The body parsed as JSON; `None` when it is not valid JSON.
    #[must_use]
    pub fn json(&self) -> Option<serde_json::Value> {
        serde_json::from_slice(&self.body).ok()
    }

    /// Whether the content type says HTML or the body starts with `<`.
    #[must_use]
    pub fn looks_like_html(&self) -> bool {
        let content_type = self.header("content-type").unwrap_or_default();
        // ASCII case folding is per byte, so this equals a lower-cased `contains("text/html")`.
        content_type
            .as_bytes()
            .windows(9)
            .any(|window| window.eq_ignore_ascii_case(b"text/html"))
            || self
                .body
                .iter()
                .find(|byte| !byte.is_ascii_whitespace())
                .is_some_and(|byte| *byte == b'<')
    }

    /// Whether the status is 2xx.
    #[must_use]
    pub const fn is_success(&self) -> bool {
        self.status >= 200 && self.status < 300
    }

    /// Whether the status is 3xx (the redirect that was not followed).
    #[must_use]
    pub(crate) const fn is_redirect(&self) -> bool {
        self.status >= 300 && self.status < 400
    }
}

/// Why no response came back; [`SafeTransportError::kind`] is evidence for the sniffer.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum SafeTransportError {
    /// Name resolution failed.
    Dns,
    /// The connection was refused.
    Refused,
    /// The TLS handshake or certificate check failed.
    Tls,
    /// A time limit expired.
    Timeout,
    /// The peer reset or closed the connection.
    Reset,
    /// The proxy asked for credentials (407).
    ProxyAuth,
    /// The proxy refused the tunnel (`CONNECT` or SOCKS handshake).
    ProxyRefused,
    /// The proxy itself could not be reached.
    ProxyUnreachable,
    /// The target is refused; nothing was sent. The production transport gives `invalid_url`,
    /// `scheme`, `no_host`, `no_port`, `link_local`, `private` or `refused_address` as `reason`;
    /// a [`SafeHttp`] that wraps it (the sniffer's balance hook) adds its own, such as `origin`.
    Blocked {
        /// Short machine-readable reason.
        reason: String,
    },
    /// The cancel token fired.
    Cancelled,
    /// Anything else; the text never carries a URL or credential.
    Other(String),
}

impl SafeTransportError {
    /// Stable name used in evidence rows: `dns`, `refused`, `tls`, `timeout`, `reset`,
    /// `proxy_auth`, `proxy_refused`, `proxy_unreachable`, `blocked`, `cancelled`, `other`.
    #[must_use]
    pub const fn kind(&self) -> &'static str {
        match self {
            Self::Dns => "dns",
            Self::Refused => "refused",
            Self::Tls => "tls",
            Self::Timeout => "timeout",
            Self::Reset => "reset",
            Self::ProxyAuth => "proxy_auth",
            Self::ProxyRefused => "proxy_refused",
            Self::ProxyUnreachable => "proxy_unreachable",
            Self::Blocked { .. } => "blocked",
            Self::Cancelled => "cancelled",
            Self::Other(_) => "other",
        }
    }
}

/// The transport. Implementations must honour `cancel` promptly (≤ 250 ms) and never follow
/// redirects.
pub trait SafeHttp: Send + Sync {
    /// Sends one request.
    fn send<'a>(
        &'a self,
        request: SafeRequest,
        cancel: &'a CancelToken,
    ) -> BoxFuture<'a, Result<SafeResponse, SafeTransportError>>;
}

#[cfg(test)]
mod tests {
    use super::*;

    fn response(status: u16, headers: &[(&str, &str)], body: &str) -> SafeResponse {
        SafeResponse {
            status,
            headers: headers
                .iter()
                .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
                .collect(),
            body: body.as_bytes().to_vec(),
            ..SafeResponse::default()
        }
    }

    #[test]
    fn html_is_recognised_by_content_type_or_a_leading_angle_bracket() {
        let html = response(200, &[("content-type", "Text/HTML; charset=utf-8")], "{}");
        assert!(html.looks_like_html());
        let tagged = response(200, &[("content-type", "application/json")], "  <html>");
        assert!(tagged.looks_like_html());
        let json = response(200, &[("content-type", "application/json")], "{\"a\":1}");
        assert!(!json.looks_like_html());
        assert!(!response(200, &[], "").looks_like_html());
    }

    #[test]
    fn header_lookup_ignores_case_and_returns_the_first_match() {
        let reply = response(200, &[("x-id", "first"), ("X-Id", "second")], "");
        assert_eq!(reply.header("X-ID"), Some("first"));
        assert_eq!(reply.header("x-missing"), None);
    }

    #[test]
    fn status_classes_have_exact_bounds() {
        let classes = |status| {
            let reply = response(status, &[], "");
            (reply.is_success(), reply.is_redirect())
        };
        assert_eq!(classes(199), (false, false));
        assert_eq!(classes(200), (true, false));
        assert_eq!(classes(299), (true, false));
        assert_eq!(classes(300), (false, true));
        assert_eq!(classes(399), (false, true));
        assert_eq!(classes(400), (false, false));
    }

    #[test]
    fn request_debug_prints_neither_credentials_nor_the_query() {
        let request = SafeRequest {
            method: SafeMethod::Get,
            url: "https://user:hunter2@relay.test/v1/models?key=sk-secret#frag".to_owned(),
            headers: vec![("authorization".to_owned(), "Bearer sk-header".to_owned())],
            body: None,
            timeout: Duration::from_secs(1),
            connect_timeout: Duration::from_secs(1),
            max_body: 1,
            allow_private: false,
        };
        let printed = format!("{request:?}");
        for secret in ["hunter2", "user:", "sk-secret", "frag", "sk-header"] {
            assert!(!printed.contains(secret), "{secret}: {printed}");
        }
        assert!(
            printed.contains("https://relay.test/v1/models"),
            "{printed}"
        );
        assert!(printed.contains("authorization"), "{printed}");
        let unparsable = SafeRequest {
            url: "not a url ?key=sk-secret".to_owned(),
            ..request
        };
        assert!(!format!("{unparsable:?}").contains("sk-secret"));
    }

    #[test]
    fn elapsed_ms_truncates_to_whole_milliseconds_and_saturates() {
        let mut reply = response(200, &[], "");
        reply.elapsed = Duration::from_micros(1_999);
        assert_eq!(reply.elapsed_ms(), 1);
        reply.elapsed = Duration::MAX;
        assert_eq!(reply.elapsed_ms(), u64::MAX);
    }
}
