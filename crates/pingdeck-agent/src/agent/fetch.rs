//! The `fetch_url` tool body: download a page and hand the model readable text.
//!
//! Single responsibility: GET an http(s) URL with a byte cap, then turn HTML into plain text
//! (links kept as `text (url)`), pretty-print JSON, pass other text through, and refuse binary
//! content. The tool is SSRF-guarded: every hop (first URL and each redirect) is resolved here and
//! refused when any address is loopback, private, link-local or otherwise non-public; a direct
//! request then connects only to the addresses that were validated. With an explicit proxy the
//! proxy resolves the host itself, so only the typed host is judged (an IP literal or `localhost`
//! is refused; a name goes to the proxy): the pinned-address guarantee only holds for direct
//! connections. `verify_tls = false` never applies here: the client always verifies.

use std::borrow::Cow;
use std::net::SocketAddr;
use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::CONTENT_TYPE;
use serde_json::Value;
use url::{Host, Url};

use pingdeck_base::{
    cancel::{CancelToken, until_cancelled},
    error::AppError,
    net_guard,
};

/// Longest URL `fetch_url` accepts, in characters.
pub const MAX_URL_CHARS: usize = 2_048;

/// The hosts a run has seen from a trusted source: the chat's user messages and
/// attachments, `web_search` results and fetches the user approved. A fetched page's links are
/// never added: that is the path a page's hidden instructions take. Kept in memory per run.
#[derive(Debug, Clone, Default)]
pub struct SeenHosts(std::collections::HashSet<String>);

impl SeenHosts {
    /// Adds every host named in `text`: the host of each `scheme://` URL and each bare
    /// `name.tld` word (`docs.rs/serde` names `docs.rs`).
    pub fn note_text(&mut self, text: &str) {
        for word in text.split(|c: char| c.is_whitespace() || "<>\"'`()[]{}|,;".contains(c)) {
            let word = word.trim_matches(|c: char| matches!(c, '.' | ':' | '!' | '?' | '*'));
            if let Some(host) = word_host(word) {
                self.0.insert(host);
            }
        }
    }

    /// Adds `host` (a host a shell command named, lower case).
    pub fn note_host(&mut self, host: &str) {
        let host = host.trim_end_matches('.').to_ascii_lowercase();
        let host = host.strip_prefix("www.").map(str::to_owned).unwrap_or(host);
        if !host.is_empty() {
            self.0.insert(host);
        }
    }

    /// Adds the host of `url`.
    pub fn note_url(&mut self, url: &str) {
        if let Some(host) = url_host(url) {
            self.0.insert(host);
        }
    }

    /// Whether the host of `url` was seen, or is a subdomain of a seen host.
    #[must_use]
    pub fn covers(&self, url: &str) -> bool {
        url_host(url).is_some_and(|host| self.covers_host(&host))
    }

    /// Whether `host` (lower case, as a shell command names it) was seen, or is a subdomain of a
    /// seen host.
    #[must_use]
    pub fn covers_host(&self, host: &str) -> bool {
        let host = host.trim_end_matches('.');
        let mut rest = host.strip_prefix("www.").unwrap_or(host);
        loop {
            if self.0.contains(rest) {
                return true;
            }
            match rest.split_once('.') {
                Some((_, parent)) if parent.contains('.') => rest = parent,
                _ => return false,
            }
        }
    }
}

/// The lower-case host of an http(s) URL, without a leading `www.`.
fn url_host(url: &str) -> Option<String> {
    let parsed = Url::parse(url.trim()).ok()?;
    let host = parsed
        .host_str()?
        .trim_end_matches('.')
        .to_ascii_lowercase();
    let host = host.strip_prefix("www.").map(str::to_owned).unwrap_or(host);
    (!host.is_empty()).then_some(host)
}

/// The host a word of a message names, if it names one.
fn word_host(word: &str) -> Option<String> {
    if word.contains("://") {
        return url_host(word);
    }
    let name = word.split(['/', '?', '#']).next()?;
    let name = name.split(':').next()?.to_ascii_lowercase();
    let tld = name.rsplit('.').next()?;
    let plausible = name.contains('.')
        && !name.starts_with('.')
        && !name.contains("..")
        && tld.len() >= 2
        && tld.bytes().all(|byte| byte.is_ascii_alphabetic())
        && name
            .bytes()
            .all(|byte| byte.is_ascii_alphanumeric() || byte == b'-' || byte == b'.');
    plausible.then(|| name.strip_prefix("www.").map(str::to_owned).unwrap_or(name))
}

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const MAX_REDIRECTS: usize = 5;
const SNIFF_BYTES: usize = 512;

#[derive(Clone, Copy, PartialEq, Eq)]
enum Kind {
    Html,
    Json,
    Text,
    /// No usable `Content-Type`: decide from the first bytes.
    Unknown,
    Binary,
}

fn classify(content_type: &str) -> Kind {
    match content_type {
        "" => Kind::Unknown,
        "text/html" | "application/xhtml+xml" => Kind::Html,
        "application/json" => Kind::Json,
        ct if ct.ends_with("+json") => Kind::Json,
        ct if ct.starts_with("text/")
            || ct.ends_with("+xml")
            || matches!(ct, "application/xml" | "application/javascript") =>
        {
            Kind::Text
        }
        _ => Kind::Binary,
    }
}

/// Fetches `url` (http/https only) and returns `URL:`/`Status:` lines plus the readable body.
///
/// The body download stops after `max_bytes`; the rendered text is cut to `max_chars`. Non-2xx
/// responses still return their body. Up to 5 redirects are followed, each re-validated; hosts
/// that are or resolve to a non-public address are refused. A non-empty `proxy`
/// (`http`/`https`/`socks5`/`socks5h` URL) carries every hop; the target is validated first.
///
/// # Errors
/// Validation for a non-http(s) URL, an invalid proxy URL, a forbidden address or a binary content
/// type, `Network` for transport failures, too many redirects and the 20 s timeout, `Cancelled`
/// when `cancel` fires.
pub async fn fetch_readable(
    url: &str,
    proxy: Option<&str>,
    max_bytes: usize,
    max_chars: usize,
    cancel: Option<&CancelToken>,
) -> Result<String, AppError> {
    fetch_guarded(url, proxy, max_bytes, max_chars, cancel, false).await
}

/// [`fetch_readable`] with the loopback refusal switchable; only tests pass `allow_loopback`.
async fn fetch_guarded(
    url: &str,
    proxy: Option<&str>,
    max_bytes: usize,
    max_chars: usize,
    cancel: Option<&CancelToken>,
    allow_loopback: bool,
) -> Result<String, AppError> {
    let parsed = Url::parse(url.trim()).map_err(|_| AppError::validation("invalid URL"))?;
    let proxy = parse_proxy(proxy)?;
    until_cancelled(
        cancel,
        download(parsed, proxy.as_ref(), max_bytes, max_chars, allow_loopback),
    )
    .await
    .unwrap_or_else(|| Err(AppError::cancelled()))
}

/// The proxy to route through, `None` for an absent or blank value. The error text never echoes
/// the value, which may carry credentials.
fn parse_proxy(proxy: Option<&str>) -> Result<Option<reqwest::Proxy>, AppError> {
    // `direct` (Settings › Network "None") is no proxy, as is blank.
    let Some(raw) = proxy.map(str::trim).filter(|raw| {
        !raw.is_empty() && !raw.eq_ignore_ascii_case(pingdeck_base::proxy_url::DIRECT)
    }) else {
        return Ok(None);
    };
    let invalid = || {
        AppError::validation(
            "the proxy URL must start with http://, https://, socks5:// or socks5h:// and name a host",
        )
    };
    let url = Url::parse(raw).map_err(|_| invalid())?;
    if !matches!(url.scheme(), "http" | "https" | "socks5" | "socks5h") || !url.has_host() {
        return Err(invalid());
    }
    reqwest::Proxy::all(raw).map(Some).map_err(|_| invalid())
}

fn network_error(error: reqwest::Error) -> AppError {
    if error.is_timeout() {
        return AppError::network(format!(
            "fetch timed out after {} s",
            REQUEST_TIMEOUT.as_secs()
        ));
    }
    let mut message = format!("fetch failed: {error}");
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        message.push_str(": ");
        message.push_str(&cause.to_string());
        source = cause.source();
    }
    AppError::network(message)
}

/// Refuses `url` when its typed host is a loopback (unless allowed), private, CGNAT, link-local,
/// metadata or reserved literal (`localhost` included); a name is left to the proxy.
fn typed_host_allowed(url: &Url, allow_loopback: bool) -> Result<(), AppError> {
    let host = url
        .host_str()
        .ok_or_else(|| AppError::validation("the URL has no host"))?;
    match net_guard::classify_host_literal(host) {
        None | Some(net_guard::HostClass::Public) => Ok(()),
        Some(net_guard::HostClass::Loopback) if allow_loopback => Ok(()),
        Some(_) => Err(AppError::validation(format!(
            "refusing to connect to {host}: it is a local, private or reserved address, which fetch_url never contacts"
        ))),
    }
}

/// The addresses to connect to for `url`, all validated by [`net_guard`]; `None` for an IP
/// literal (nothing to pin). Loopback is allowed only for tests; private networks never.
async fn validated_addrs(
    url: &Url,
    allow_loopback: bool,
) -> Result<Option<(String, Vec<SocketAddr>)>, AppError> {
    let host = url
        .host()
        .ok_or_else(|| AppError::validation("the URL has no host"))?;
    let port = url.port_or_known_default().unwrap_or(80);
    let (_, addrs) =
        net_guard::resolve_with(&host.to_string(), port, allow_loopback, false).await?;
    Ok(match host {
        Host::Domain(name) => Some((name.to_owned(), addrs)),
        Host::Ipv4(_) | Host::Ipv6(_) => None,
    })
}

/// Sends the GET for `url`, following up to [`MAX_REDIRECTS`] redirects by hand so each hop is
/// validated and pinned to the addresses that were checked (a shared client would re-resolve).
async fn send_guarded(
    mut url: Url,
    proxy: Option<&reqwest::Proxy>,
    allow_loopback: bool,
) -> Result<(reqwest::Response, Url), AppError> {
    for _ in 0..=MAX_REDIRECTS {
        if !matches!(url.scheme(), "http" | "https") {
            return Err(AppError::validation(
                "only http and https URLs can be fetched",
            ));
        }
        let pinned = match proxy {
            // The proxy resolves the name: judge the typed host, so a fake-IP resolver in
            // front of the proxy cannot turn a public name into a refusal; nothing is pinned.
            Some(_) => {
                typed_host_allowed(&url, allow_loopback)?;
                None
            }
            None => validated_addrs(&url, allow_loopback).await?,
        };
        let mut builder = reqwest::Client::builder()
            .redirect(reqwest::redirect::Policy::none())
            .user_agent("PingDeck");
        // A proxy resolves the host itself, so there is nothing to pin.
        builder = match (proxy, &pinned) {
            (Some(proxy), _) => builder.proxy(proxy.clone()),
            (None, Some((name, addrs))) => builder.no_proxy().resolve_to_addrs(name, addrs),
            (None, None) => builder.no_proxy(),
        };
        let response = builder
            .build()
            .map_err(network_error)?
            .get(url.clone())
            .timeout(REQUEST_TIMEOUT)
            .send()
            .await
            .map_err(network_error)?;
        let next = response
            .status()
            .is_redirection()
            .then(|| response.headers().get(reqwest::header::LOCATION))
            .flatten()
            .and_then(|value| value.to_str().ok())
            .and_then(|location| url.join(location).ok());
        match next {
            Some(next) => url = next,
            None => return Ok((response, url)),
        }
    }
    Err(AppError::network(format!(
        "too many redirects (more than {MAX_REDIRECTS})"
    )))
}

async fn download(
    url: Url,
    proxy: Option<&reqwest::Proxy>,
    max_bytes: usize,
    max_chars: usize,
    allow_loopback: bool,
) -> Result<String, AppError> {
    let (response, final_url) = send_guarded(url, proxy, allow_loopback).await?;
    let status = response.status().as_u16();
    let final_url = final_url.to_string();
    let content_type = response
        .headers()
        .get(CONTENT_TYPE)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.split(';').next())
        .map(|value| value.trim().to_ascii_lowercase())
        .unwrap_or_default();
    let mut kind = classify(&content_type);
    if kind == Kind::Binary {
        return Err(AppError::validation(format!(
            "unsupported content type {content_type}"
        )));
    }

    let mut body = Vec::new();
    let mut cut = false;
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(network_error)?;
        let room = max_bytes - body.len();
        if chunk.len() > room {
            body.extend_from_slice(&chunk[..room]);
            cut = true;
            break;
        }
        body.extend_from_slice(&chunk);
    }

    // shortcut: the charset parameter is ignored and bytes are decoded as lossy UTF-8.
    let mut text = String::from_utf8_lossy(&body).into_owned();
    if cut && text.ends_with('\u{FFFD}') {
        text.pop();
    }
    if kind == Kind::Unknown {
        kind = sniff(&body)?;
    }
    let rendered = match kind {
        Kind::Html => html_to_text(&text),
        Kind::Json => serde_json::from_str::<Value>(&text)
            .and_then(|value| serde_json::to_string_pretty(&value))
            .unwrap_or(text),
        _ => text,
    };
    let note = if cut {
        format!("\n… [download truncated at {max_bytes} bytes]")
    } else {
        String::new()
    };
    Ok(format!(
        "URL: {final_url}\nStatus: {status}\n\n{}{note}",
        truncate_chars(&rendered, max_chars)
    ))
}

fn sniff(body: &[u8]) -> Result<Kind, AppError> {
    let head = &body[..body.len().min(SNIFF_BYTES)];
    if memchr::memchr(0, head).is_some() {
        return Err(AppError::validation(
            "unsupported content type (unknown binary data)",
        ));
    }
    let head = String::from_utf8_lossy(head).to_ascii_lowercase();
    Ok(
        if head.contains("<html") || head.contains("<!doctype html") {
            Kind::Html
        } else {
            Kind::Text
        },
    )
}

fn truncate_chars(text: &str, max_chars: usize) -> String {
    let Some((cut, _)) = text.char_indices().nth(max_chars) else {
        return text.to_owned();
    };
    let more = text[cut..].chars().count();
    format!("{}\n… [truncated, {more} more characters]", &text[..cut])
}

/// Converts an HTML document to readable plain text using `dom_query` (`html5ever`).
///
/// Drops `script`/`style`/`noscript`/`svg`/`head` blocks, hidden nodes (`hidden`,
/// `aria-hidden="true"`, inline `display:none`) and comments, turns block tags and
/// `<br>` into line breaks, strips remaining tags, decodes HTML5 entities,
/// collapses whitespace and renders `<a href>` with an absolute http(s) target as `text (url)`.
#[must_use]
pub fn html_to_text(html: &str) -> String {
    let doc = dom_query::Document::from(html);
    doc.select("script, style, noscript, svg, head, title")
        .remove();
    // Text a reader never sees must not reach the model either.
    doc.select(
        r#"[hidden], [aria-hidden="true" i], [style*="display:none" i], [style*="display: none" i]"#,
    )
    .remove();
    for a in doc.select("a[href]").iter() {
        if let Some(href) = a.attr("href") {
            let href = href.trim();
            if Url::parse(href).is_ok_and(|url| matches!(url.scheme(), "http" | "https")) {
                let label = a.text();
                let label = label.trim();
                if !label.is_empty() && label != href {
                    a.set_text(&format!("{label} ({href})"));
                }
            }
        }
    }
    for br in doc.select("br").iter() {
        br.replace_with_html("\x1f");
    }
    for cell in doc.select("td, th").iter() {
        cell.after_html(" ");
    }
    for el in doc
        .select("div, li, tr, section, article, header, footer, nav, main, aside, form, dt, dd, figure, figcaption, pre, hr")
        .iter()
    {
        el.before_html("\x1f");
        el.after_html("\x1f");
    }
    for el in doc
        .select("p, h1, h2, h3, h4, h5, h6, ul, ol, table, blockquote")
        .iter()
    {
        el.before_html("\x1e");
        el.after_html("\x1e");
    }
    collapse_lines(&doc.text())
}

/// Trims and space-collapses every line (`\x1f`) and paragraph (`\x1e`).
fn collapse_lines(text: &str) -> String {
    let mut result = String::with_capacity(text.len());
    let mut blank = false;
    for para in text.split('\x1e') {
        for line in para.split('\x1f') {
            let mut words = line.split_whitespace();
            let Some(first) = words.next() else {
                continue;
            };
            if !result.is_empty() {
                result.push('\n');
                if blank {
                    result.push('\n');
                }
            }
            blank = false;
            result.push_str(first);
            for word in words {
                result.push(' ');
                result.push_str(word);
            }
        }
        if !result.is_empty() {
            blank = true;
        }
    }
    result
}

/// Decodes HTML entities via `dom_query` (`html5ever`).
pub(crate) fn decode_entities(text: &str) -> Cow<'_, str> {
    if !text.contains('&') {
        return Cow::Borrowed(text);
    }
    Cow::Owned(dom_query::Document::from(text).text().to_string())
}

#[cfg(test)]
mod tests {
    use std::{
        io::{Read, Write},
        net::{TcpListener, TcpStream},
        sync::mpsc,
        thread,
    };

    use super::*;

    /// [`fetch_guarded`] with loopback allowed, so the local fixture servers are reachable.
    async fn local(url: &str, max_bytes: usize, max_chars: usize) -> Result<String, AppError> {
        fetch_guarded(url, None, max_bytes, max_chars, None, true).await
    }

    /// Answers every request with `302 Location: target` (itself when `None`); returns its URL.
    fn serve_redirect(target: Option<&str>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/hop", listener.local_addr().unwrap());
        let head = format!(
            "HTTP/1.1 302 Found\r\nLocation: {}\r\nContent-Length: 0\r\nConnection: close\r\n\r\n",
            target.unwrap_or(&url)
        );
        thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                drain_request(&mut stream);
                let _ = stream.write_all(head.as_bytes());
            }
        });
        url
    }

    fn drain_request(stream: &mut TcpStream) {
        let mut data = Vec::new();
        let mut buf = [0_u8; 1024];
        while !data.windows(4).any(|window| window == b"\r\n\r\n") {
            match stream.read(&mut buf) {
                Ok(read) if read > 0 => data.extend_from_slice(&buf[..read]),
                _ => break,
            }
        }
    }

    /// Serves `body` to every connection on a fresh local port; returns the base URL.
    fn serve(status: &str, content_type: &str, body: Vec<u8>) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}/page", listener.local_addr().unwrap());
        let head = format!(
            "HTTP/1.1 {status}\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n",
            body.len()
        );
        thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                drain_request(&mut stream);
                // The client may hang up early on oversized bodies; write errors are expected.
                let _ = stream.write_all(head.as_bytes());
                let _ = stream.write_all(&body);
            }
        });
        url
    }

    const PAGE: &str = r#"<!DOCTYPE html><html><head><title>PageTitle</title><style>p{color:red}</style></head>
<body><script>var secret = "<b>";</script><!-- a comment --><h1>Hello &amp; welcome</h1>
<p>Fish &lt;3 &#39;chips&#39; &#x41; caf&eacute; AT&T</p>
<p>See <a href="https://example.com/a?x=1&amp;y=2">the docs</a> or <a href='/relative'>rel</a>.<br>line two</p>
<noscript>enable js</noscript><svg><path d="M0"/></svg><ul><li>x</li><li>y</li></ul></body></html>"#;

    #[tokio::test]
    async fn html_is_reduced_to_readable_text_with_links() {
        let url = serve(
            "200 OK",
            "text/html; charset=utf-8",
            PAGE.as_bytes().to_vec(),
        );
        let out = local(&url, 2 << 20, 10_000).await.unwrap();
        assert!(
            out.starts_with(&format!("URL: {url}\nStatus: 200\n\n")),
            "{out}"
        );
        for expected in [
            "Hello & welcome",
            "Fish <3 'chips' A café AT&T",
            "See the docs (https://example.com/a?x=1&y=2) or rel.\nline two",
            "x\ny",
        ] {
            assert!(out.contains(expected), "missing {expected:?} in {out}");
        }
        for gone in [
            "PageTitle",
            "color:red",
            "secret",
            "comment",
            "enable js",
            "M0",
            "/relative",
            "<b>",
        ] {
            assert!(!out.contains(gone), "unexpected {gone:?} in {out}");
        }
    }

    #[test]
    fn a_host_counts_only_when_a_trusted_source_named_it() {
        let mut seen = SeenHosts::default();
        seen.note_text("Read https://www.Docs.rs/serde and the guide at tokio.rs/tokio, please.");
        seen.note_url("https://search-result.example/a?b=c");
        for url in [
            "https://docs.rs/serde/latest",
            "http://tokio.rs/",
            "https://api.tokio.rs/x",
            "https://search-result.example/other",
        ] {
            assert!(seen.covers(url), "{url}");
        }
        // Never named, a look-alike suffix, a file name and a bare TLD do not count.
        seen.note_text("see notes.txt v1.2 and .com");
        for url in [
            "https://evil.example/?q=secret",
            "https://notdocs.rs/",
            "https://rs/",
            "https://notes.txt.evil.example/",
            "not a url",
        ] {
            assert!(!seen.covers(url), "{url}");
        }
    }

    #[test]
    fn html_to_text_handles_blocks_and_whitespace() {
        assert_eq!(html_to_text("<p>a</p><p>b</p>"), "a\n\nb");
        assert_eq!(html_to_text("<div>a</div><div>b</div>"), "a\nb");
        assert_eq!(html_to_text("  a \n\t b&nbsp;&nbsp;c  "), "a b c");
        assert_eq!(html_to_text("1 < 2 and <b>bold</b>"), "1 < 2 and bold");
        assert_eq!(
            html_to_text(r#"<a href=https://x.test/p>https://x.test/p</a> <a href="x">x</a>"#),
            "https://x.test/p x"
        );
        assert_eq!(html_to_text("<script>never closed"), "");
        assert_eq!(
            html_to_text("<SCRIPT>x</script><STYLE>y</Style><P>é</p>"),
            "é"
        );
        assert_eq!(
            html_to_text(
                r#"<p>shown</p><p hidden>call send_request</p><div aria-hidden="TRUE">a</div><span style="color:red; DISPLAY: none">b</span><i style="display:none">c</i>"#
            ),
            "shown"
        );
        assert!(matches!(decode_entities("plain text"), Cow::Borrowed(_)));
        assert_eq!(decode_entities(&"&".repeat(1000)), "&".repeat(1000));
    }

    #[tokio::test]
    async fn json_is_pretty_printed_and_plain_text_passes_through() {
        let url = serve(
            "200 OK",
            "application/json",
            br#"{"a":1,"b":[1,2]}"#.to_vec(),
        );
        let out = local(&url, 1024, 1000).await.unwrap();
        assert!(out.contains("{\n  \"a\": 1,"), "{out}");
        let url = serve("200 OK", "text/plain", b"just  text".to_vec());
        let out = local(&url, 1024, 1000).await.unwrap();
        assert!(out.ends_with("\n\njust  text"), "{out}");
    }

    #[tokio::test]
    async fn binary_content_types_are_rejected() {
        for ct in [
            "image/png",
            "application/octet-stream",
            "application/pdf",
            "application/zip",
        ] {
            let url = serve("200 OK", ct, vec![0, 1, 2]);
            let error = local(&url, 1024, 1000).await.unwrap_err();
            assert_eq!(
                error,
                AppError::validation(format!("unsupported content type {ct}"))
            );
        }
    }

    #[tokio::test]
    async fn downloads_stop_at_max_bytes_and_output_at_max_chars() {
        let url = serve("200 OK", "text/plain", vec![b'a'; 200_000]);
        let out = local(&url, 1000, 10_000).await.unwrap();
        assert!(
            out.contains("[download truncated at 1000 bytes]"),
            "len {}",
            out.len()
        );
        assert!(out.len() < 1300);

        let url = serve("200 OK", "text/plain", "é".repeat(500).into_bytes());
        let out = local(&url, 1 << 20, 100).await.unwrap();
        assert!(
            out.ends_with(&format!(
                "{}\n… [truncated, 400 more characters]",
                "é".repeat(100)
            )),
            "{out}"
        );
    }

    #[tokio::test]
    async fn non_success_statuses_still_return_the_body() {
        let url = serve("404 Not Found", "text/plain", b"gone".to_vec());
        let out = local(&url, 1024, 1000).await.unwrap();
        assert!(out.contains("Status: 404\n\ngone"), "{out}");
    }

    #[tokio::test]
    async fn missing_content_type_is_sniffed() {
        let url = serve(
            "200 OK",
            "",
            b"<HTML><body><p>hi</p></body></HTML>".to_vec(),
        );
        let out = local(&url, 1024, 1000).await.unwrap();
        assert!(out.ends_with("\n\nhi"), "{out}");
    }

    #[tokio::test]
    async fn only_http_urls_are_fetched_and_cancel_is_honoured() {
        for bad in ["ftp://example.com/x", "file:///etc/passwd", "not a url"] {
            let error = fetch_readable(bad, None, 1024, 1000, None)
                .await
                .unwrap_err();
            assert!(matches!(error, AppError::Validation { .. }), "{bad}");
        }
        let token = CancelToken::new();
        token.cancel();
        let error = fetch_guarded("http://127.0.0.1:1/", None, 1024, 1000, Some(&token), true)
            .await
            .unwrap_err();
        assert!(error.is_cancelled());
    }

    #[tokio::test]
    async fn production_fetch_refuses_loopback_private_and_metadata_hosts() {
        for url in [
            "http://127.0.0.1:1/",
            "http://localhost:1/",
            "http://api.localhost:1/",
            "http://[::1]:1/",
            "http://[::ffff:127.0.0.1]:1/",
            "http://[::ffff:7f00:1]:1/",
            "http://0x7f.1:1/",
            "http://2130706433:1/",
            "http://10.0.0.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://[fe80::1]/",
            "http://[fd00::1]/",
            "http://0.0.0.0:1/",
        ] {
            let error = fetch_readable(url, None, 1024, 1000, None)
                .await
                .unwrap_err();
            assert!(
                matches!(error, AppError::Validation { .. }),
                "{url}: {error:?}"
            );
        }
    }

    #[tokio::test]
    async fn every_redirect_hop_is_revalidated() {
        for target in [
            "http://169.254.169.254/latest/meta-data/",
            "http://10.0.0.1/",
            "http://[::ffff:192.168.0.1]/",
            "http://[fe80::1]/",
            "ftp://example.com/x",
        ] {
            let url = serve_redirect(Some(target));
            let error = local(&url, 1024, 1000).await.unwrap_err();
            assert!(
                matches!(error, AppError::Validation { .. }),
                "{target}: {error:?}"
            );
        }
    }

    #[tokio::test]
    async fn redirects_are_followed_up_to_the_limit() {
        let page = serve("200 OK", "text/plain", b"landed".to_vec());
        let url = serve_redirect(Some(&page));
        let out = local(&url, 1024, 1000).await.unwrap();
        assert!(
            out.starts_with(&format!("URL: {page}\nStatus: 200")),
            "{out}"
        );
        assert!(out.ends_with("\n\nlanded"), "{out}");

        let looping = serve_redirect(None);
        let error = local(&looping, 1024, 1000).await.unwrap_err();
        assert!(error.message().contains("too many redirects"), "{error:?}");
    }

    /// A fake forward proxy answering `200 text/plain` with `body`; every connection is counted
    /// and each request head is sent on the channel.
    fn serve_proxy(body: &str) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (sender, receiver) = mpsc::channel();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: text/plain\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let mut data = Vec::new();
                let mut buf = [0_u8; 1024];
                while !data.windows(4).any(|window| window == b"\r\n\r\n") {
                    match stream.read(&mut buf) {
                        Ok(read) if read > 0 => data.extend_from_slice(&buf[..read]),
                        _ => break,
                    }
                }
                let _ = sender.send(String::from_utf8_lossy(&data).into_owned());
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (url, receiver)
    }

    /// A listener nobody answers; `connected` says whether anything ever connected to it.
    struct Tripwire(TcpListener);

    impl Tripwire {
        fn new() -> (Self, String) {
            let listener = TcpListener::bind("127.0.0.1:0").unwrap();
            listener.set_nonblocking(true).unwrap();
            let url = format!("http://{}", listener.local_addr().unwrap());
            (Self(listener), url)
        }

        /// A connect completes in the kernel backlog before `send` can fail or return, so a
        /// non-blocking `accept` after the awaited call sees every earlier connection.
        fn connected(&self) -> bool {
            self.0.accept().is_ok()
        }
    }

    #[tokio::test]
    async fn a_proxy_carries_the_request_to_a_validated_public_target() {
        let (proxy, requests) = serve_proxy("via proxy");
        // A public IP literal: validated, never contacted directly because the proxy answers.
        let out = fetch_guarded(
            "http://93.184.216.34/page?q=1",
            Some(&proxy),
            1024,
            1000,
            None,
            false,
        )
        .await
        .unwrap();
        assert!(
            out.starts_with("URL: http://93.184.216.34/page?q=1\nStatus: 200"),
            "{out}"
        );
        assert!(out.ends_with("\n\nvia proxy"), "{out}");
        let head = requests.recv().unwrap();
        assert!(
            head.starts_with("GET http://93.184.216.34/page?q=1 HTTP/1.1"),
            "{head}"
        );
    }

    #[tokio::test]
    async fn a_blank_proxy_means_a_direct_connection() {
        let url = serve("200 OK", "text/plain", b"direct".to_vec());
        for proxy in [None, Some(""), Some("  ")] {
            let out = fetch_guarded(&url, proxy, 1024, 1000, None, true)
                .await
                .unwrap();
            assert!(out.ends_with("\n\ndirect"), "{out}");
        }
    }

    #[tokio::test]
    async fn forbidden_targets_are_refused_before_the_proxy_is_contacted() {
        let (tripwire, proxy) = Tripwire::new();
        for url in [
            "http://127.0.0.1:1/",
            "http://localhost:1/",
            "http://api.localhost/",
            "http://10.0.0.1/",
            "http://169.254.169.254/latest/meta-data/",
            "http://100.64.0.1/",
            "http://[::1]/",
            "http://[fd00::1]/",
            "http://[::ffff:192.168.0.1]/",
            "http://[64:ff9b::a00:1]/",
            "http://224.0.0.1/",
            "http://0.0.0.0/",
        ] {
            let error = fetch_readable(url, Some(&proxy), 1024, 1000, None)
                .await
                .unwrap_err();
            assert!(
                matches!(error, AppError::Validation { .. }),
                "{url}: {error:?}"
            );
        }
        assert!(!tripwire.connected());
    }

    #[tokio::test]
    async fn redirects_through_a_proxy_are_revalidated() {
        // The fake proxy answers every request with a redirect to a forbidden host.
        let redirector = serve_redirect(Some("http://169.254.169.254/latest/meta-data/"));
        let proxy = redirector.trim_end_matches("/hop");
        let error = fetch_guarded(
            "http://93.184.216.34/",
            Some(proxy),
            1024,
            1000,
            None,
            false,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, AppError::Validation { .. }), "{error:?}");
        assert!(error.message().contains("169.254.169.254"), "{error:?}");
    }

    #[tokio::test]
    async fn invalid_proxy_urls_are_rejected_without_echoing_them() {
        for proxy in [
            "ftp://user:hunter2@proxy.test:21",
            "not a url",
            "http://",
            "user:hunter2@proxy.test:8080",
        ] {
            let error = fetch_readable("http://93.184.216.34/", Some(proxy), 1024, 1000, None)
                .await
                .unwrap_err();
            assert!(matches!(error, AppError::Validation { .. }), "{proxy}");
            assert!(!error.message().contains("hunter2"), "{error:?}");
        }
        for proxy in [
            "http://p.test:8080",
            "https://p.test",
            "socks5://u:p@p.test:1080",
            "socks5h://p.test:1080",
        ] {
            assert!(parse_proxy(Some(proxy)).unwrap().is_some(), "{proxy}");
        }
    }

    #[tokio::test]
    async fn a_dead_proxy_is_a_network_error_not_a_direct_fallback() {
        // Accepts and hangs up at once: the request fails, it is never retried directly.
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let dead = format!("http://{}", listener.local_addr().unwrap());
        thread::spawn(move || while listener.accept().is_ok() {});
        let error = fetch_readable("http://93.184.216.34/", Some(&dead), 1024, 1000, None)
            .await
            .unwrap_err();
        assert!(matches!(error, AppError::Network { .. }), "{error:?}");
    }
}
