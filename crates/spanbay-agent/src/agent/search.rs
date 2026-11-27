//! Pluggable web search for the agent's `web_search` tool.
//!
//! Single responsibility: turn a query into a short list of [`SearchHit`]s through the backend
//! chosen in Settings (Brave Search, Tavily, a SearXNG instance, DuckDuckGo's keyless HTML
//! fallback, or `Auto`: the first of Brave, Tavily, SearXNG that has its key or URL, else
//! DuckDuckGo) and render them as a numbered, citable tool result for the model. A provider's
//! native search is chosen by the runner before any tool call and never reaches this module.
//! API keys travel only in request headers and never appear in error text.

use std::time::Duration;

use futures_util::StreamExt;
use reqwest::header::{ACCEPT, CONTENT_TYPE, USER_AGENT};
use serde_json::{Value, json};
use url::Url;

use super::fetch::decode_entities;
use super::tools::ToolOutput;
use crate::agent::untrusted::sanitize_untrusted;
use spanbay_base::{
    cancel::{CancelToken, until_cancelled},
    error::AppError,
};
use spanbay_llm::llm::hub::{HubCitation, HubPart};
use spanbay_model::settings::{AiSearchSettings, SearchBackend};

const REQUEST_TIMEOUT: Duration = Duration::from_secs(20);
const CONNECT_TIMEOUT: Duration = Duration::from_secs(5);
const DDG_TIMEOUT: Duration = Duration::from_secs(10);
/// Largest DuckDuckGo page read; the result list is at the top.
const DDG_MAX_BODY: usize = 256 * 1024;
const MAX_QUERY_CHARS: usize = 400;
const MAX_RESULTS: usize = 10;
const TAVILY_URL: &str = "https://api.tavily.com/search";
const BRAVE_URL: &str = "https://api.search.brave.com/res/v1/web/search";
const DDG_URL: &str = "https://html.duckduckgo.com/html/";
/// DuckDuckGo's HTML endpoint turns away the bare `SpanBay` agent more often than a browser-like one.
const DDG_USER_AGENT: &str = "Mozilla/5.0 (compatible; SpanBay)";

/// Which backend to call, with its credentials and API endpoints.
#[derive(Clone)]
pub struct SearchConfig {
    /// The selected backend; `Auto` picks the first usable one ([`SearchConfig::resolved`]).
    pub backend: SearchBackend,
    /// Tavily API key.
    pub tavily_api_key: String,
    /// Brave Search API key.
    pub brave_api_key: String,
    /// SearXNG base URL (no trailing `/search`).
    pub searxng_base_url: String,
    /// Tavily endpoint; tests point it at a local server.
    pub tavily_url: String,
    /// Brave endpoint; tests point it at a local server.
    pub brave_url: String,
    /// DuckDuckGo HTML endpoint; tests point it at a local server.
    pub ddg_url: String,
    /// "Use DuckDuckGo when no search key is set": without it `Auto` never contacts DuckDuckGo.
    pub use_duckduckgo: bool,
}

// Hand-written so a stray `{:?}` can never print the API keys.
impl std::fmt::Debug for SearchConfig {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SearchConfig")
            .field("backend", &self.backend)
            .field("searxng_base_url", &self.searxng_base_url)
            .finish_non_exhaustive()
    }
}

impl SearchConfig {
    /// Builds the production config (real endpoints) from the search settings and the two API
    /// keys, which the caller reads from the secret store (empty when unset).
    #[must_use]
    pub fn from_settings(
        search: &AiSearchSettings,
        tavily_api_key: String,
        brave_api_key: String,
    ) -> Self {
        Self {
            backend: search.backend,
            tavily_api_key,
            brave_api_key,
            searxng_base_url: search.searxng_base_url.clone(),
            tavily_url: TAVILY_URL.to_owned(),
            brave_url: BRAVE_URL.to_owned(),
            ddg_url: DDG_URL.to_owned(),
            use_duckduckgo: search.use_duckduckgo,
        }
    }

    /// The backend a search runs on, in this order: the chosen one; for `Auto` the first
    /// of Brave (key), Tavily (key), SearXNG (URL) that is set, then DuckDuckGo only when the
    /// user switched it on. `None` means nothing is available. Choosing `duckduckgo` explicitly is
    /// the same opt-in.
    #[must_use]
    pub fn resolved(&self) -> Option<SearchBackend> {
        match self.backend {
            SearchBackend::Auto if !self.brave_api_key.trim().is_empty() => {
                Some(SearchBackend::Brave)
            }
            SearchBackend::Auto if !self.tavily_api_key.trim().is_empty() => {
                Some(SearchBackend::Tavily)
            }
            SearchBackend::Auto if !self.searxng_base_url.trim().is_empty() => {
                Some(SearchBackend::Searxng)
            }
            SearchBackend::Auto => self.use_duckduckgo.then_some(SearchBackend::DuckDuckGo),
            chosen => Some(chosen),
        }
    }

    /// Whether [`web_search`] can run: a backend resolved and it has its key (or base URL).
    #[must_use]
    pub fn is_configured(&self) -> bool {
        let required = match self.resolved() {
            None => return false,
            Some(SearchBackend::Tavily) => &self.tavily_api_key,
            Some(SearchBackend::Brave) => &self.brave_api_key,
            Some(SearchBackend::Searxng) => &self.searxng_base_url,
            Some(SearchBackend::Auto | SearchBackend::DuckDuckGo) => return true,
        };
        !required.trim().is_empty()
    }

    /// Why [`web_search`] cannot run, naming the Settings entry to fill in.
    fn not_configured(&self) -> AppError {
        let missing = match self.resolved() {
            Some(SearchBackend::Tavily) => "add the Tavily API key",
            Some(SearchBackend::Brave) => "add the Brave Search API key",
            Some(SearchBackend::Searxng) => "add the SearXNG URL",
            Some(SearchBackend::Auto | SearchBackend::DuckDuckGo) | None => {
                return AppError::validation(NO_BACKEND);
            }
        };
        AppError::validation(format!(
            "web search is not configured: {missing} in Settings > AI > Agent"
        ))
    }
}

/// What the tool says when no search backend is available (Settings > AI > Agent).
pub const NO_BACKEND: &str = "No search backend is set up. Add a Brave, Tavily or SearXNG key or URL, or turn on \"Use DuckDuckGo when no search key is set\", in Settings > AI > Agent.";

/// One search result.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SearchHit {
    /// Page title.
    pub title: String,
    /// Page URL.
    pub url: String,
    /// Short excerpt.
    pub snippet: String,
}

/// The HTTP client search requests use: 5 s connect timeout, at most 5 redirects, and `proxy`
/// (an `http`/`https`/`socks5`/`socks5h` URL) when non-blank.
///
/// # Errors
/// Validation for an unusable proxy URL (never echoed), `Network` when the client cannot be built.
pub fn http_client(proxy: Option<&str>) -> Result<reqwest::Client, AppError> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(CONNECT_TIMEOUT)
        .redirect(reqwest::redirect::Policy::limited(5))
        .user_agent("SpanBay");
    let proxy = proxy.map(str::trim).filter(|proxy| !proxy.is_empty());
    if proxy.is_some_and(|proxy| proxy.eq_ignore_ascii_case(spanbay_base::proxy_url::DIRECT)) {
        // Settings › Network "None": not even the system proxy.
        builder = builder.no_proxy();
    } else if let Some(proxy) = proxy {
        let proxy = reqwest::Proxy::all(proxy)
            .map_err(|_| AppError::validation("the network proxy URL is invalid"))?;
        builder = builder.proxy(proxy);
    }
    builder.build().map_err(|error| {
        AppError::network(format!(
            "could not build the HTTP client: {}",
            error.without_url()
        ))
    })
}

/// Searches the web through the configured backend; `max_results` is clamped to `1..=10`.
///
/// # Errors
/// Validation for a blank/over-long query or a missing key/URL (the text names the Settings entry),
/// `Network` for transport failures and non-2xx answers (the keyless DuckDuckGo fallback says it
/// is unreliable), `Cancelled` when `cancel` fires.
pub async fn web_search(
    config: &SearchConfig,
    client: &reqwest::Client,
    query: &str,
    max_results: usize,
    cancel: Option<&CancelToken>,
) -> Result<Vec<SearchHit>, AppError> {
    let query = query.trim();
    if query.is_empty() {
        return Err(AppError::validation("search query must not be blank"));
    }
    if query.chars().count() > MAX_QUERY_CHARS {
        return Err(AppError::validation(format!(
            "search query is longer than {MAX_QUERY_CHARS} characters"
        )));
    }
    if !config.is_configured() {
        return Err(config.not_configured());
    }
    let limit = max_results.clamp(1, MAX_RESULTS);
    until_cancelled(cancel, search(config, client, query, limit))
        .await
        .unwrap_or_else(|| Err(AppError::cancelled()))
}

async fn search(
    config: &SearchConfig,
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, AppError> {
    match config.resolved() {
        None => Err(AppError::validation(NO_BACKEND)),
        Some(SearchBackend::Auto | SearchBackend::DuckDuckGo) => {
            duckduckgo(config, client, query, limit).await
        }
        Some(SearchBackend::Tavily) => {
            let body = json!({ "query": query, "max_results": limit, "search_depth": "basic" });
            let request = client
                .post(&config.tavily_url)
                .bearer_auth(config.tavily_api_key.trim())
                .header(CONTENT_TYPE, "application/json")
                .body(body.to_string());
            let value = send_json(request, "Tavily").await?;
            Ok(parse_hits(&value["results"], "content", limit, false))
        }
        Some(SearchBackend::Brave) => {
            let url = with_query(
                &config.brave_url,
                &[("q", query), ("count", &limit.to_string())],
            )?;
            let request = client
                .get(url)
                .header("X-Subscription-Token", config.brave_api_key.trim())
                .header(ACCEPT, "application/json");
            let value = send_json(request, "Brave").await?;
            Ok(parse_hits(
                &value["web"]["results"],
                "description",
                limit,
                true,
            ))
        }
        Some(SearchBackend::Searxng) => {
            let base = config.searxng_base_url.trim().trim_end_matches('/');
            let url = with_query(
                &format!("{base}/search"),
                &[("q", query), ("format", "json")],
            )?;
            let value = send_json(client.get(url), "SearXNG").await?;
            Ok(parse_hits(&value["results"], "content", limit, false))
        }
    }
}

fn with_query(base: &str, pairs: &[(&str, &str)]) -> Result<Url, AppError> {
    let mut url =
        Url::parse(base).map_err(|_| AppError::validation("the search endpoint URL is invalid"))?;
    url.query_pairs_mut().extend_pairs(pairs);
    Ok(url)
}

/// Sends `request` and parses a 2xx JSON body; the error text never carries the URL or headers.
async fn send_json(request: reqwest::RequestBuilder, backend: &str) -> Result<Value, AppError> {
    let failed = |error: reqwest::Error| {
        if error.is_timeout() {
            AppError::network(format!("{backend} request timed out"))
        } else {
            AppError::network(format!("{backend} request failed: {}", error.without_url()))
        }
    };
    let response = request
        .timeout(REQUEST_TIMEOUT)
        .send()
        .await
        .map_err(failed)?;
    let status = response.status();
    if !status.is_success() {
        return Err(AppError::network(format!(
            "{backend} returned HTTP {}",
            status.as_u16()
        )));
    }
    let bytes = response.bytes().await.map_err(failed)?;
    serde_json::from_slice(&bytes)
        .map_err(|_| AppError::network(format!("{backend} returned an invalid response")))
}

fn parse_hits(
    results: &Value,
    snippet_key: &str,
    limit: usize,
    strip_markup: bool,
) -> Vec<SearchHit> {
    let text = |item: &Value, key: &str| {
        let raw = item[key].as_str().unwrap_or_default().trim();
        if strip_markup {
            raw.replace("<strong>", "").replace("</strong>", "")
        } else {
            raw.to_owned()
        }
    };
    results
        .as_array()
        .into_iter()
        .flatten()
        .filter_map(|item| {
            let url = item["url"].as_str()?.trim();
            (!url.is_empty()).then(|| SearchHit {
                title: text(item, "title"),
                url: url.to_owned(),
                snippet: text(item, snippet_key),
            })
        })
        .take(limit)
        .collect()
}

/// The failure of the keyless fallback: always says it is unreliable and where to fix it.
fn ddg_failed(reason: &str) -> AppError {
    AppError::network(format!(
        "DuckDuckGo search failed ({reason}). The keyless fallback is unreliable because DuckDuckGo blocks automated queries; choose a search backend and key in Settings > AI > Agent."
    ))
}

/// Queries DuckDuckGo's HTML endpoint and scrapes the result list; best effort by nature.
async fn duckduckgo(
    config: &SearchConfig,
    client: &reqwest::Client,
    query: &str,
    limit: usize,
) -> Result<Vec<SearchHit>, AppError> {
    let failed = |error: reqwest::Error| {
        ddg_failed(&if error.is_timeout() {
            "the request timed out".to_owned()
        } else {
            format!("the request failed: {}", error.without_url())
        })
    };
    let url = with_query(&config.ddg_url, &[("q", query)])?;
    let response = client
        .get(url)
        .header(USER_AGENT, DDG_USER_AGENT)
        .header(ACCEPT, "text/html")
        .timeout(DDG_TIMEOUT)
        .send()
        .await
        .map_err(failed)?;
    let status = response.status();
    if !status.is_success() {
        return Err(ddg_failed(&format!("it returned HTTP {}", status.as_u16())));
    }
    let mut body = Vec::new();
    let mut stream = response.bytes_stream();
    while let Some(chunk) = stream.next().await {
        let chunk = chunk.map_err(failed)?;
        let room = DDG_MAX_BODY - body.len();
        body.extend_from_slice(&chunk[..chunk.len().min(room)]);
        if chunk.len() >= room {
            break;
        }
    }
    parse_duckduckgo(&String::from_utf8_lossy(&body), limit)
}

/// Extracts up to `limit` hits from DuckDuckGo result markup using `dom_query`: `a.result__a`
/// carries the title and the (redirect-wrapped) link, the following `a.result__snippet` the
/// excerpt. Ads are skipped.
fn parse_duckduckgo(html: &str, limit: usize) -> Result<Vec<SearchHit>, AppError> {
    let doc = dom_query::Document::from(html);
    let mut hits: Vec<SearchHit> = Vec::new();
    // A snippet is expected next: false after an ad or once the snippet is read.
    let mut open = false;
    for anchor in doc.select("a.result__a, a.result__snippet").iter() {
        if anchor.has_class("result__a") {
            if hits.len() >= limit {
                break;
            }
            let url = anchor
                .attr("href")
                .and_then(|href| resolve_duckduckgo_link(&href));
            open = url.is_some();
            if let Some(url) = url {
                hits.push(SearchHit {
                    title: anchor
                        .text()
                        .split_whitespace()
                        .collect::<Vec<_>>()
                        .join(" "),
                    url,
                    snippet: String::new(),
                });
            }
        } else if open && anchor.has_class("result__snippet") {
            if let Some(hit) = hits.last_mut() {
                hit.snippet = anchor
                    .text()
                    .split_whitespace()
                    .collect::<Vec<_>>()
                    .join(" ");
            }
            open = false;
        }
    }
    if hits.is_empty() {
        return if html.contains("no-results") {
            Ok(hits)
        } else if html.contains("anomaly") {
            Err(ddg_failed("blocked: it asked for a bot check"))
        } else {
            Err(ddg_failed("its page layout was not recognised"))
        };
    }
    Ok(hits)
}

/// The target of a result link: DuckDuckGo wraps results as `//duckduckgo.com/l/?uddg=<target>`,
/// ads go through `y.js` (no `uddg`, skipped), anything else is taken as the direct link.
fn resolve_duckduckgo_link(href: &str) -> Option<String> {
    let link = Url::parse("https://duckduckgo.com/")
        .ok()?
        .join(&decode_entities(href.trim()))
        .ok()?;
    let host = link.host_str()?;
    let target = if host == "duckduckgo.com" || host.ends_with(".duckduckgo.com") {
        link.query_pairs()
            .find(|(key, _)| key == "uddg")?
            .1
            .into_owned()
    } else {
        link.into()
    };
    Url::parse(&target)
        .ok()
        .filter(|target| matches!(target.scheme(), "http" | "https") && target.has_host())
        .map(String::from)
}

/// Renders hits as a numbered plain-text list for the model; titles and snippets are page text,
/// so their hidden characters are made visible (`sanitize_untrusted`).
fn format_hits(hits: &[SearchHit]) -> String {
    hits.iter()
        .enumerate()
        .map(|(index, hit)| {
            let mut entry = format!(
                "{}. {}\n   {}",
                index + 1,
                sanitize_untrusted(&hit.title),
                sanitize_untrusted(&hit.url)
            );
            if !hit.snippet.is_empty() {
                entry.push_str("\n   ");
                entry.push_str(&sanitize_untrusted(&hit.snippet));
            }
            entry
        })
        .collect::<Vec<_>>()
        .join("\n\n")
}

/// The tool result for `hits`: one text part listing them (title, URL, snippet) whose citations
/// carry one source per hit, so the UI can render the sources next to the answer. It is external
/// data ([`ToolOutput::external`]).
#[must_use]
pub fn search_output(query: &str, hits: &[SearchHit]) -> ToolOutput {
    if hits.is_empty() {
        return ToolOutput::text(format!("No results for \"{query}\"."));
    }
    let citations = hits
        .iter()
        .map(|hit| HubCitation {
            url: hit.url.clone(),
            title: hit.title.clone(),
            snippet: (!hit.snippet.is_empty()).then(|| hit.snippet.clone()),
            start_index: None,
            end_index: None,
        })
        .collect();
    ToolOutput {
        parts: vec![HubPart::Text {
            text: format_hits(hits),
            signature: None,
            origin: None,
            citations,
        }],
        is_error: false,
        meta: None,
        external: true,
        cap: None,
        exit_code: None,
    }
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

    fn read_request(stream: &mut TcpStream) -> String {
        let mut data = Vec::new();
        let mut buf = [0_u8; 1024];
        loop {
            let read = stream.read(&mut buf).unwrap_or(0);
            if read == 0 {
                break;
            }
            data.extend_from_slice(&buf[..read]);
            if let Some(end) = data.windows(4).position(|window| window == b"\r\n\r\n") {
                let head = String::from_utf8_lossy(&data[..end]).to_lowercase();
                let length = head
                    .lines()
                    .find_map(|line| line.strip_prefix("content-length:"))
                    .and_then(|value| value.trim().parse::<usize>().ok())
                    .unwrap_or(0);
                if data.len() >= end + 4 + length {
                    break;
                }
            }
        }
        String::from_utf8_lossy(&data).into_owned()
    }

    /// Answers one request with `status`/`body`; the raw request text arrives on the channel.
    fn serve_once(status: &str, body: &str) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (sender, receiver) = mpsc::channel();
        let response = format!(
            "HTTP/1.1 {status}\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        thread::spawn(move || {
            if let Ok((mut stream, _)) = listener.accept() {
                let _ = sender.send(read_request(&mut stream));
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (url, receiver)
    }

    fn client() -> reqwest::Client {
        reqwest::Client::builder().no_proxy().build().unwrap()
    }

    fn config(backend: SearchBackend, url: &str) -> SearchConfig {
        SearchConfig {
            backend,
            tavily_api_key: "tvly-secret".to_owned(),
            brave_api_key: "brave-secret".to_owned(),
            searxng_base_url: format!("{url}/"),
            tavily_url: format!("{url}/search"),
            brave_url: format!("{url}/res/v1/web/search"),
            ddg_url: format!("{url}/html/"),
            use_duckduckgo: true,
        }
    }

    #[tokio::test]
    async fn search_backends_send_their_wire_format_and_parse_hits() {
        for (backend, query, limit, response, request_prefix, auth, title, snippet, count) in [
            (
                SearchBackend::Tavily,
                "  rust  ",
                5,
                r#"{"results":[{"title":"A","url":"https://a.test","content":"alpha"},{"title":"B","url":"https://b.test","content":"beta"}]}"#,
                "POST /search ",
                "authorization: bearer tvly-secret",
                "A",
                "alpha",
                2,
            ),
            (
                SearchBackend::Brave,
                "rust lang",
                3,
                r#"{"web":{"results":[{"title":"<strong>Rust</strong> lang","url":"https://rust-lang.org","description":"A <strong>fast</strong> language"}]}}"#,
                "GET /res/v1/web/search?q=rust+lang&count=3 ",
                "x-subscription-token: brave-secret",
                "Rust lang",
                "A fast language",
                1,
            ),
            (
                SearchBackend::Searxng,
                "q",
                2,
                r#"{"results":[{"title":"1","url":"https://1.test","content":"one"},{"title":"2","url":"https://2.test","content":"two"},{"title":"3","url":"https://3.test","content":"three"}]}"#,
                "GET /search?q=q&format=json ",
                "host: 127.0.0.1:",
                "1",
                "one",
                2,
            ),
        ] {
            let (url, request) = serve_once("200 OK", response);
            let hits = web_search(&config(backend, &url), &client(), query, limit, None)
                .await
                .unwrap();
            assert_eq!(
                (hits.len(), hits[0].title.as_str(), hits[0].snippet.as_str()),
                (count, title, snippet)
            );
            let raw = request.recv().unwrap();
            assert!(raw.starts_with(request_prefix), "{raw}");
            assert!(raw.to_lowercase().contains(auth), "{raw}");
            if backend == SearchBackend::Tavily {
                let body: Value =
                    serde_json::from_str(raw.split("\r\n\r\n").nth(1).unwrap()).unwrap();
                assert_eq!(
                    body,
                    json!({"query": "rust", "max_results": 5, "search_depth": "basic"})
                );
            }
        }
        for (limit, expected) in [(99, 10), (0, 1)] {
            let (url, request) = serve_once("200 OK", r#"{"results":[]}"#);
            web_search(
                &config(SearchBackend::Tavily, &url),
                &client(),
                "q",
                limit,
                None,
            )
            .await
            .unwrap();
            assert!(
                request
                    .recv()
                    .unwrap()
                    .contains(&format!(r#""max_results":{expected}"#))
            );
        }
    }

    #[tokio::test]
    async fn missing_keys_point_at_settings_and_bad_queries_are_validation_errors() {
        let mut cfg = config(SearchBackend::Tavily, "http://127.0.0.1:1");
        cfg.tavily_api_key = "  ".to_owned();
        assert!(!cfg.is_configured());
        let error = web_search(&cfg, &client(), "q", 3, None).await.unwrap_err();
        assert_eq!(
            error,
            AppError::validation(
                "web search is not configured: add the Tavily API key in Settings > AI > Agent"
            )
        );
        cfg.backend = SearchBackend::Brave;
        cfg.brave_api_key.clear();
        assert!(
            web_search(&cfg, &client(), "q", 3, None)
                .await
                .unwrap_err()
                .message()
                .contains("Brave Search API key in Settings > AI > Agent")
        );
        cfg.backend = SearchBackend::Searxng;
        cfg.searxng_base_url = " ".to_owned();
        assert!(
            web_search(&cfg, &client(), "q", 3, None)
                .await
                .unwrap_err()
                .message()
                .contains("SearXNG URL in Settings > AI > Agent")
        );
        cfg.backend = SearchBackend::Auto;
        cfg.tavily_api_key.clear();
        cfg.brave_api_key.clear();
        cfg.searxng_base_url.clear();
        assert!(
            cfg.is_configured(),
            "Auto without keys falls back to DuckDuckGo when the user turned it on"
        );
        assert_eq!(cfg.resolved(), Some(SearchBackend::DuckDuckGo));
        cfg.backend = SearchBackend::Tavily;
        cfg.tavily_api_key = "k".to_owned();
        assert!(cfg.is_configured());
        assert!(matches!(
            web_search(&cfg, &client(), "   ", 3, None).await,
            Err(AppError::Validation { .. })
        ));
        let long = "x".repeat(401);
        assert!(matches!(
            web_search(&cfg, &client(), &long, 3, None).await,
            Err(AppError::Validation { .. })
        ));
        // No backend chosen: the keyless fallback counts as configured.
        cfg.backend = SearchBackend::DuckDuckGo;
        assert!(cfg.is_configured());
    }

    #[tokio::test]
    async fn search_errors_and_cancellation_never_echo_credentials() {
        for (status, body, expected) in [
            (
                "401 Unauthorized",
                r#"{"error":"bad key tvly-secret"}"#,
                "Tavily returned HTTP 401",
            ),
            (
                "500 Internal Server Error",
                "tvly-secret",
                "Tavily returned HTTP 500",
            ),
            (
                "200 OK",
                "<html>tvly-secret</html>",
                "Tavily returned an invalid response",
            ),
        ] {
            let (url, _) = serve_once(status, body);
            let error = web_search(
                &config(SearchBackend::Tavily, &url),
                &client(),
                "q",
                3,
                None,
            )
            .await
            .unwrap_err();
            assert_eq!(error, AppError::network(expected));
            assert!(!error.message().contains("secret"));
        }
        let mut cfg = config(SearchBackend::Brave, "http://127.0.0.1:1");
        cfg.brave_api_key = "bad\nkey-secret".to_owned();
        let error = web_search(&cfg, &client(), "q", 3, None).await.unwrap_err();
        assert!(matches!(error, AppError::Network { .. }));
        assert!(!error.message().contains("secret"));

        let token = CancelToken::new();
        token.cancel();
        for backend in [SearchBackend::Brave, SearchBackend::DuckDuckGo] {
            let error = web_search(
                &config(backend, "http://127.0.0.1:1"),
                &client(),
                "q",
                3,
                Some(&token),
            )
            .await
            .unwrap_err();
            assert!(error.is_cancelled());
        }
    }

    #[test]
    fn from_settings_fills_production_endpoints_and_never_debug_prints_keys() {
        let search = AiSearchSettings {
            backend: SearchBackend::Brave,
            ..AiSearchSettings::default()
        };
        let cfg = SearchConfig::from_settings(&search, String::new(), "k".to_owned());
        assert!(cfg.is_configured());
        assert_eq!(cfg.tavily_url, "https://api.tavily.com/search");
        assert_eq!(
            cfg.brave_url,
            "https://api.search.brave.com/res/v1/web/search"
        );
        assert_eq!(cfg.ddg_url, "https://html.duckduckgo.com/html/");
        let debug = format!("{cfg:?}");
        assert!(!debug.contains("brave_api_key") && !debug.contains("\"k\""));
        let none =
            SearchConfig::from_settings(&AiSearchSettings::default(), String::new(), String::new());
        assert_eq!(none.backend, SearchBackend::Auto);
        assert!(
            !none.is_configured(),
            "DuckDuckGo is off unless the user switched it on"
        );
        let with_ddg = SearchConfig::from_settings(
            &AiSearchSettings {
                use_duckduckgo: true,
                ..AiSearchSettings::default()
            },
            String::new(),
            String::new(),
        );
        assert!(with_ddg.is_configured());
    }

    fn hit(title: &str, url: &str, snippet: &str) -> SearchHit {
        SearchHit {
            title: title.into(),
            url: url.into(),
            snippet: snippet.into(),
        }
    }

    #[test]
    fn search_output_lists_numbered_hits_and_cites_every_source() {
        let hits = vec![
            hit("A", "https://a.test", "alpha"),
            hit("B", "https://b.test", ""),
        ];
        let output = search_output("q", &hits);
        assert!(!output.is_error);
        assert_eq!(
            output.text_content(),
            "1. A\n   https://a.test\n   alpha\n\n2. B\n   https://b.test"
        );
        let [HubPart::Text { citations, .. }] = output.parts.as_slice() else {
            panic!("one text part expected: {:?}", output.parts);
        };
        assert_eq!(
            citations,
            &vec![
                HubCitation {
                    url: "https://a.test".into(),
                    title: "A".into(),
                    snippet: Some("alpha".into()),
                    start_index: None,
                    end_index: None,
                },
                HubCitation {
                    url: "https://b.test".into(),
                    title: "B".into(),
                    snippet: None,
                    start_index: None,
                    end_index: None,
                },
            ]
        );
        let empty = search_output("rust", &[]);
        assert_eq!(empty.text_content(), "No results for \"rust\".");
        assert!(!empty.is_error);
    }

    #[test]
    fn parse_hits_skips_entries_without_a_url_and_honours_the_limit() {
        let value = json!([
            {"title": "no url", "content": "x"},
            {"title": "blank", "url": "  ", "content": "x"},
            {"title": "1", "url": "https://1.test"},
            {"title": "2", "url": "https://2.test", "content": " two "},
            {"title": "3", "url": "https://3.test", "content": "three"},
        ]);
        assert_eq!(
            parse_hits(&value, "content", 2, false),
            vec![
                hit("1", "https://1.test", ""),
                hit("2", "https://2.test", "two")
            ]
        );
        assert!(parse_hits(&json!({"not": "an array"}), "content", 5, false).is_empty());
    }

    const DDG_PAGE: &str = r#"<html><body><div id="links" class="results">
<div class="result results_links results_links_deep web-result ">
  <div class="links_main links_deep result__body">
    <h2 class="result__title"><a rel="nofollow" class="result__a" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2Flearn%3Fa%3D1%26b%3D2&amp;rut=abc123">Rust <b>Programming</b> Language</a></h2>
    <a class="result__snippet" href="//duckduckgo.com/l/?uddg=https%3A%2F%2Fwww.rust-lang.org%2Flearn&amp;rut=abc123">A language empowering <b>everyone</b> to build &amp; ship.</a>
  </div>
</div>
<div class="result result--ad">
  <a rel="nofollow" class="result__a" href="https://duckduckgo.com/y.js?ad_domain=ads.test&amp;u3=zzz">Buy now</a>
  <a class="result__snippet" href="https://duckduckgo.com/y.js?ad_domain=ads.test">Ad copy</a>
</div>
<div class="result results_links web-result">
  <a class="result__a" href="https://direct.test/page">Direct &amp; plain</a>
</div>
<div class="result results_links web-result">
  <a href="/l/?uddg=http%3A%2F%2Fthird.test%2F&amp;rut=1" class="big result__a">Third</a>
  <a class="result__snippet" href="/l/?uddg=http%3A%2F%2Fthird.test%2F">Third snippet</a>
</div>
<div class="result"><a class="result__a" href="//duckduckgo.com/l/?uddg=javascript%3Aalert(1)">Bad scheme</a></div>
</div></body></html>"#;

    #[test]
    fn duckduckgo_results_are_parsed_and_redirects_decoded() {
        let hits = parse_duckduckgo(DDG_PAGE, 10).unwrap();
        assert_eq!(
            hits,
            vec![
                hit(
                    "Rust Programming Language",
                    "https://www.rust-lang.org/learn?a=1&b=2",
                    "A language empowering everyone to build & ship."
                ),
                hit("Direct & plain", "https://direct.test/page", ""),
                hit("Third", "http://third.test/", "Third snippet"),
            ]
        );
        let two = parse_duckduckgo(DDG_PAGE, 1).unwrap();
        assert_eq!(two.len(), 1);
    }

    #[test]
    fn duckduckgo_links_resolve_only_to_http_targets() {
        let resolve = resolve_duckduckgo_link;
        assert_eq!(
            resolve("//duckduckgo.com/l/?uddg=https%3A%2F%2Fa.test%2Fx%3Fq%3D1&rut=r").as_deref(),
            Some("https://a.test/x?q=1")
        );
        assert_eq!(
            resolve("/l/?uddg=https%3A%2F%2Fa.test%2F&amp;rut=r").as_deref(),
            Some("https://a.test/")
        );
        assert_eq!(
            resolve("https://other.test/page").as_deref(),
            Some("https://other.test/page")
        );
        assert_eq!(resolve("https://duckduckgo.com/y.js?ad_domain=x"), None);
        assert_eq!(
            resolve("//duckduckgo.com/l/?uddg=ftp%3A%2F%2Fa.test%2F"),
            None
        );
        assert_eq!(resolve("//duckduckgo.com/l/?uddg=not-a-url"), None);
        assert_eq!(resolve("javascript:alert(1)"), None);
    }

    #[test]
    fn duckduckgo_failures_are_told_apart_and_all_say_unreliable() {
        assert!(
            parse_duckduckgo(r#"<div class="no-results">No results.</div>"#, 5)
                .unwrap()
                .is_empty()
        );
        for (html, reason) in [
            (
                "<div class=\"anomaly-modal\">Select all squares</div>",
                "bot check",
            ),
            ("<html>new layout</html>", "layout was not recognised"),
        ] {
            let error = parse_duckduckgo(html, 5).unwrap_err();
            assert!(error.message().contains(reason), "{error:?}");
            assert!(error.message().contains("keyless fallback is unreliable"));
            assert!(error.message().contains("Settings > AI > Agent"));
        }
    }

    #[tokio::test]
    async fn no_backend_searches_duckduckgo_with_a_query_and_a_browser_like_agent() {
        let (url, request) = serve_once("200 OK", DDG_PAGE);
        let hits = web_search(
            &config(SearchBackend::DuckDuckGo, &url),
            &client(),
            " rust lang ",
            2,
            None,
        )
        .await
        .unwrap();
        assert_eq!(hits.len(), 2);
        assert_eq!(hits[0].url, "https://www.rust-lang.org/learn?a=1&b=2");
        let raw = request.recv().unwrap();
        assert!(raw.starts_with("GET /html/?q=rust+lang "), "{raw}");
        assert!(
            raw.to_lowercase()
                .contains("user-agent: mozilla/5.0 (compatible; spanbay)"),
            "{raw}"
        );
    }

    #[tokio::test]
    async fn duckduckgo_errors_name_the_fallback_and_settings() {
        let (url, _request) = serve_once("202 Accepted", "<html>challenge</html>");
        let error = web_search(
            &config(SearchBackend::DuckDuckGo, &url),
            &client(),
            "q",
            3,
            None,
        )
        .await
        .unwrap_err();
        assert!(matches!(error, AppError::Network { .. }));
        assert!(
            error.message().contains("layout was not recognised"),
            "{error:?}"
        );
        assert!(error.message().contains("keyless fallback is unreliable"));
        assert!(error.message().contains("Settings > AI > Agent"));

        let (url, _request) = serve_once("503 Service Unavailable", "");
        let error = web_search(
            &config(SearchBackend::DuckDuckGo, &url),
            &client(),
            "q",
            3,
            None,
        )
        .await
        .unwrap_err();
        assert!(
            error.message().contains("it returned HTTP 503"),
            "{error:?}"
        );
        assert!(error.message().contains("unreliable"));

        let error = web_search(
            &config(SearchBackend::DuckDuckGo, "http://127.0.0.1:1"),
            &client(),
            "q",
            3,
            None,
        )
        .await
        .unwrap_err();
        assert!(error.message().contains("the request failed"), "{error:?}");
        assert!(error.message().contains("unreliable"));
    }

    #[tokio::test]
    async fn duckduckgo_bodies_are_read_only_up_to_the_cap() {
        let late = r#"<a class="result__a" href="https://late.test/">Late</a>"#;
        let body = format!("{DDG_PAGE}{}{late}", " ".repeat(DDG_MAX_BODY),);
        let (url, _request) = serve_once("200 OK", &body);
        let hits = web_search(
            &config(SearchBackend::DuckDuckGo, &url),
            &client(),
            "q",
            10,
            None,
        )
        .await
        .unwrap();
        assert_eq!(hits.len(), 3);
        assert!(hits.iter().all(|hit| hit.url != "https://late.test/"));
    }

    #[test]
    fn the_search_client_rejects_a_bad_proxy_without_echoing_it() {
        assert!(http_client(None).is_ok());
        assert!(http_client(Some("  ")).is_ok());
        assert!(http_client(Some("http://user:pw@proxy.test:8080")).is_ok());
        let error = http_client(Some("http://")).unwrap_err();
        assert!(matches!(error, AppError::Validation { .. }));
    }

    #[test]
    fn auto_picks_the_backend_in_order_and_duckduckgo_needs_the_switch() {
        // (brave key, tavily key, searxng url, duckduckgo switch) -> resolved backend
        let table = [
            (true, true, true, false, Some(SearchBackend::Brave)),
            (false, true, true, false, Some(SearchBackend::Tavily)),
            (false, false, true, false, Some(SearchBackend::Searxng)),
            (false, false, false, true, Some(SearchBackend::DuckDuckGo)),
            (false, false, false, false, None),
            (true, false, false, true, Some(SearchBackend::Brave)),
        ];
        for (brave, tavily, searxng, ddg, expected) in table {
            let mut cfg = config(SearchBackend::Auto, "http://127.0.0.1:9");
            cfg.brave_api_key = if brave { "k" } else { "" }.to_owned();
            cfg.tavily_api_key = if tavily { "k" } else { "" }.to_owned();
            cfg.searxng_base_url = if searxng { "http://s" } else { "" }.to_owned();
            cfg.use_duckduckgo = ddg;
            assert_eq!(cfg.resolved(), expected, "{brave} {tavily} {searxng} {ddg}");
            assert_eq!(cfg.is_configured(), expected.is_some());
        }
    }

    #[tokio::test]
    async fn with_nothing_available_no_request_is_made_and_the_tool_says_so() {
        // Port 9 (discard) is never reached: there is no request to make.
        let mut cfg = config(SearchBackend::Auto, "http://127.0.0.1:9");
        cfg.brave_api_key.clear();
        cfg.tavily_api_key.clear();
        cfg.searxng_base_url.clear();
        cfg.use_duckduckgo = false;
        let error = web_search(&cfg, &client(), "rust", 3, None)
            .await
            .unwrap_err();
        assert!(error.message().contains("No search backend is set up"));
        assert!(error.message().contains("Settings > AI > Agent"));
    }
}
