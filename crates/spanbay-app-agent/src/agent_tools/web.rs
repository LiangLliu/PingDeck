//! The `web_search` and `fetch_url` tools.
//!
//! Single responsibility: adapt [`crate::agent::search`] and [`crate::agent::fetch`] to the
//! [`AgentTool`] contract. Both are class `Network` and parallel-safe, and neither runs without
//! approval in Approve reads: a query or a URL can carry data out. `web_search`'s backend comes
//! from the search settings and its API key from the secret store, read before any other lock is
//! taken; the runner leaves `web_search` out when the provider searches natively. Both honour the
//! global proxy (`network.proxy_url`) and the run's cancellation.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};
use url::Url;

use crate::agent::untrusted::sanitize_untrusted;

use crate::agent::fetch::{MAX_URL_CHARS, fetch_readable};
use crate::agent::limits::FETCHES_PER_RUN;
use crate::agent::search::{SearchConfig, http_client, search_output, web_search};
#[cfg(test)]
use crate::agent::tools::ToolRegistry;
use crate::agent::tools::{
    AgentTool, CallInspection, ToolApprovalPreview, ToolClass, ToolContext, ToolOutput, ToolSource,
    ToolSpec,
};
use crate::agent_tools::spanbay::common::RunLimit;
use crate::app::AppState;
use crate::error::AppError;
use crate::secrets::{SEARCH_BRAVE, SEARCH_TAVILY};
use crate::settings::{AiSearchSettings, SearchBackend};

/// Name of the search tool.
pub const WEB_SEARCH: &str = "web_search";
/// Name of the fetch tool.
pub const FETCH_URL: &str = "fetch_url";

/// Results returned when the model gives no `max_results`.
const DEFAULT_RESULTS: usize = 5;
/// Largest page `fetch_url` downloads, in bytes.
const FETCH_MAX_BYTES: usize = 2 * 1024 * 1024;
/// Longest text `fetch_url` returns, in characters: everything that can be kept; the runner
/// cuts it for the model and keeps the rest for `offset` and `read_output`.
const FETCH_MAX_CHARS: usize = 8 * 1024 * 1024;

/// Registers `web_search` and `fetch_url`. With no search backend chosen `web_search` still runs:
/// it falls back to keyless DuckDuckGo.
#[cfg(test)]
pub fn register(registry: &mut ToolRegistry) {
    registry.register(Arc::new(WebSearchTool::new()));
    registry.register(Arc::new(FetchUrlTool::new()));
}

fn spec(name: &str, description: &str, read_only: bool, input_schema: Value) -> ToolSpec {
    ToolSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema,
        read_only,
        destructive: false,
        source: ToolSource::Builtin,
        class: ToolClass::Network,
        parallel_safe: true,
    }
}

use crate::agent_tools::spanbay::schema::parse_args;

/// `web_search {query, max_results?}`.
pub struct WebSearchTool {
    spec: ToolSpec,
}

impl WebSearchTool {
    /// The tool with its static spec.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: spec(
                WEB_SEARCH,
                "Search the web and return numbered results with title, URL and snippet.",
                true,
                json!({
                    "type": "object",
                    "properties": {
                        "query": {"type": "string", "description": "What to search for"},
                        "max_results": {"type": "integer", "minimum": 1, "maximum": 10, "description": "Results to return (default 5)"}
                    },
                    "required": ["query"]
                }),
            ),
        }
    }
}

impl Default for WebSearchTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct SearchArgs {
    query: String,
    max_results: Option<u64>,
}

/// The search configuration for `search`: the settings plus the API keys the chosen backend may
/// use (both for `Auto`) from the secret store, read on a blocking thread and before any other
/// lock is taken.
async fn search_config(
    state: &AppState,
    search: &AiSearchSettings,
) -> Result<SearchConfig, AppError> {
    let (want_tavily, want_brave) = match search.backend {
        SearchBackend::Tavily => (true, false),
        SearchBackend::Brave => (false, true),
        SearchBackend::Auto => (true, true),
        SearchBackend::DuckDuckGo | SearchBackend::Searxng => (false, false),
    };
    let secrets = Arc::clone(state.secrets());
    let (tavily, brave) = tokio::task::spawn_blocking(move || {
        let get = |wanted: bool, secret_ref: &str| {
            if wanted {
                secrets.get(secret_ref)
            } else {
                Ok(None)
            }
        };
        Ok::<_, AppError>((
            get(want_tavily, SEARCH_TAVILY)?,
            get(want_brave, SEARCH_BRAVE)?,
        ))
    })
    .await
    .map_err(|_| AppError::internal("the secret lookup was interrupted"))??;
    Ok(SearchConfig::from_settings(
        search,
        tavily.unwrap_or_default(),
        brave.unwrap_or_default(),
    ))
}

/// [`SECRET_REFUSAL`](crate::agent::secret_guard::SECRET_REFUSAL) when `text` (a URL or a query the
/// model wrote) carries a resolved secret value in any wire form, in every permission mode. The
/// needles come from the secret store on a blocking thread, never while the workspace store is
/// held.
async fn secret_refusal(state: &AppState, text: &str) -> Result<Option<&'static str>, AppError> {
    let secrets = Arc::clone(state.secrets());
    let needles = tokio::task::spawn_blocking(move || secrets.active_redaction_needles())
        .await
        .map_err(|_| AppError::internal("the secret lookup was interrupted"))?;
    Ok(crate::agent::secret_guard::refusal_if_secret(
        text, &needles,
    ))
}

async fn run_search(
    ctx: &ToolContext<'_>,
    schema: &Value,
    arguments: &Value,
) -> Result<ToolOutput, AppError> {
    let args: SearchArgs = parse_args(schema, arguments)?;
    if let Some(refusal) = secret_refusal(crate::agent_tools::app_state(ctx), &args.query).await? {
        return Ok(ToolOutput::error(refusal));
    }
    let config = search_config(crate::agent_tools::app_state(ctx), &ctx.settings.ai.search).await?;
    // The one outbound rule (C-N1): a SearXNG on this machine or on `no_proxy` goes direct.
    let host = Url::parse(ctx.settings.ai.search.searxng_base_url.trim())
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .filter(|_| ctx.settings.ai.search.backend == SearchBackend::Searxng)
        .unwrap_or_default();
    let network = &ctx.settings.network;
    let proxy =
        match spanbay_base::proxy_url::route("", &network.proxy_url, &network.no_proxy, &host) {
            spanbay_base::proxy_url::ProxyRoute::Direct => {
                spanbay_base::proxy_url::DIRECT.to_owned()
            }
            spanbay_base::proxy_url::ProxyRoute::System => String::new(),
            spanbay_base::proxy_url::ProxyRoute::Proxy(proxy) => proxy,
        };
    let client = http_client(Some(&proxy))?;
    let max = args.max_results.map_or(DEFAULT_RESULTS, |max| {
        usize::try_from(max).unwrap_or(usize::MAX)
    });
    let hits = web_search(&config, &client, &args.query, max, Some(ctx.cancel)).await?;
    Ok(search_output(args.query.trim(), &hits))
}

impl AgentTool for WebSearchTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        _ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        let args: SearchArgs = parse_args(&self.spec.input_schema, arguments)?;
        let query = args.query.trim();
        if query.is_empty() {
            return Err(AppError::validation(
                "the `query` argument must not be blank",
            ));
        }
        Ok(CallInspection {
            read_only: true,
            arg_subject: Some(query.to_owned()),
            ..CallInspection::default()
        })
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            run_search(ctx, &self.spec.input_schema, &arguments)
                .await
                .unwrap_or_else(|error| ToolOutput::error(error.message()))
        })
    }
}

/// The proxy of a fetch by the one outbound rule (C-N1): `no_proxy` and local hosts go direct, an
/// explicit global proxy is used, and the system proxy reads as direct because a fetch pins the
/// addresses it validated (a proxy it cannot see would resolve the name again).
fn fetch_proxy(network: &crate::settings::NetworkSettings, url: &str) -> Option<String> {
    let host = Url::parse(url.trim())
        .ok()
        .and_then(|url| url.host_str().map(str::to_owned))
        .unwrap_or_default();
    match spanbay_base::proxy_url::route("", &network.proxy_url, &network.no_proxy, &host) {
        spanbay_base::proxy_url::ProxyRoute::Proxy(proxy) => Some(proxy),
        spanbay_base::proxy_url::ProxyRoute::Direct
        | spanbay_base::proxy_url::ProxyRoute::System => None,
    }
}

/// `fetch_url {url, offset?}`.
pub struct FetchUrlTool {
    spec: ToolSpec,
    limit: RunLimit,
}

impl FetchUrlTool {
    /// The tool with its static spec. Not read-only: it contacts model-chosen URLs.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: spec(
                FETCH_URL,
                "Download a web page or JSON document and return its readable text. When the text was cut, call again with the same url and `offset` to read on without downloading again.",
                false,
                json!({
                    "type": "object",
                    "properties": {
                        "url": {"type": "string", "description": "An http or https URL"},
                        "offset": {"type": "integer", "minimum": 0, "description": "Byte offset into the text an earlier fetch of this url kept; reads it without contacting the site"}
                    },
                    "required": ["url"]
                }),
            ),
            limit: RunLimit::new(FETCHES_PER_RUN),
        }
    }
}

impl Default for FetchUrlTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct FetchArgs {
    url: String,
    offset: Option<u64>,
}

/// The rest of the text an earlier `fetch_url` of `url` in this chat kept, from `offset`.
fn kept_page(
    state: &AppState,
    session_id: &str,
    url: &str,
    offset: u64,
    cap: usize,
) -> Result<String, AppError> {
    let calls = state
        .store()
        .list_tool_calls(session_id)
        .map_err(|error| AppError::database(error.to_string()))?;
    let handle = calls
        .iter()
        .rev()
        .filter(|call| call.name == FETCH_URL && call.spill_ref.is_some())
        .find(|call| {
            serde_json::from_str::<FetchArgs>(&call.args_json)
                .is_ok_and(|args| args.url.trim() == url)
        })
        .map(|call| call.call_id.clone())
        .ok_or_else(|| {
            AppError::not_found(
                "no earlier fetch of this url kept more text; fetch it without offset first",
            )
        })?;
    crate::agent_tools::read_output::page_of(state, session_id, &handle, offset, cap)
}

impl AgentTool for FetchUrlTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        _ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        let args: FetchArgs = parse_args(&self.spec.input_schema, arguments)?;
        let url = args.url.trim();
        if url.chars().count() > MAX_URL_CHARS {
            return Err(AppError::validation(format!(
                "the URL is longer than {MAX_URL_CHARS} characters"
            )));
        }
        let parsed = Url::parse(url).map_err(|_| AppError::validation("invalid URL"))?;
        if !matches!(parsed.scheme(), "http" | "https") {
            return Err(AppError::validation(
                "only http and https URLs can be fetched",
            ));
        }
        // Not read-only: the model can leak data through the URL or poke at servers.
        Ok(CallInspection {
            read_only: false,
            preview: Some(ToolApprovalPreview::Url {
                url: url.to_owned(),
            }),
            arg_subject: Some(url.to_owned()),
            ..CallInspection::default()
        })
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let args: FetchArgs = match parse_args(&self.spec.input_schema, &arguments) {
                Ok(args) => args,
                Err(error) => return ToolOutput::error(error.message()),
            };
            if let Some(offset) = args.offset {
                let state = crate::agent_tools::app_state(ctx).clone();
                let (session, cap) = (ctx.session_id.to_owned(), ctx.result_cap);
                let url = args.url.trim().to_owned();
                let read = tokio::task::spawn_blocking(move || {
                    kept_page(&state, &session, &url, offset, cap)
                });
                return match read.await {
                    Ok(Ok(text)) => ToolOutput::text(text).external(),
                    Ok(Err(error)) => ToolOutput::error(error.message()),
                    Err(error) => {
                        ToolOutput::error(format!("reading the kept text failed: {error}"))
                    }
                };
            }
            if !self.limit.take(ctx.run_id) {
                return ToolOutput::error(format!(
                    "Not run: this run already made {FETCHES_PER_RUN} web fetches, the most one run may make."
                ));
            }
            match secret_refusal(crate::agent_tools::app_state(ctx), &args.url).await {
                Ok(Some(refusal)) => return ToolOutput::error(refusal),
                Ok(None) => {}
                Err(error) => return ToolOutput::error(error.message()),
            }
            match fetch_readable(
                &args.url,
                fetch_proxy(&ctx.settings.network, &args.url).as_deref(),
                FETCH_MAX_BYTES,
                FETCH_MAX_CHARS,
                Some(ctx.cancel),
            )
            .await
            {
                // Page text is external data: hidden characters made visible, then enveloped.
                Ok(text) => ToolOutput::text(sanitize_untrusted(&text).into_owned()).external(),
                Err(error) => ToolOutput::error(error.message()),
            }
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;
    use std::io::{Read, Write};
    use std::net::{TcpListener, TcpStream};
    use std::sync::mpsc;
    use std::thread;

    use super::*;
    use crate::agent::tools::TestParts;
    use crate::cancel::CancelToken;
    use crate::llm::hub::HubPart;
    use crate::settings::Settings;

    struct Harness {
        state: AppState,
        settings: Settings,
        cancel: CancelToken,
        parts: TestParts,
    }

    impl Harness {
        fn new() -> Self {
            Self {
                state: AppState::open_in_memory().unwrap(),
                settings: Settings::default(),
                cancel: CancelToken::new(),
                parts: TestParts::default(),
            }
        }

        fn ctx(&self) -> ToolContext<'_> {
            ToolContext::for_test(
                Some(&self.state as &(dyn std::any::Any + Send + Sync)),
                &self.settings,
                &self.cancel,
                &self.parts,
            )
        }
    }

    fn read_head(stream: &mut TcpStream) -> String {
        let mut data = Vec::new();
        let mut buf = [0_u8; 1024];
        while !data.windows(4).any(|window| window == b"\r\n\r\n") {
            match stream.read(&mut buf) {
                Ok(read) if read > 0 => data.extend_from_slice(&buf[..read]),
                _ => break,
            }
        }
        String::from_utf8_lossy(&data).into_owned()
    }

    /// Answers every request with `200 content_type body`; request heads arrive on the channel.
    fn serve(content_type: &str, body: &str) -> (String, mpsc::Receiver<String>) {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let url = format!("http://{}", listener.local_addr().unwrap());
        let (sender, receiver) = mpsc::channel();
        let response = format!(
            "HTTP/1.1 200 OK\r\nContent-Type: {content_type}\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
            body.len()
        );
        thread::spawn(move || {
            while let Ok((mut stream, _)) = listener.accept() {
                let _ = sender.send(read_head(&mut stream));
                let _ = stream.write_all(response.as_bytes());
            }
        });
        (url, receiver)
    }

    fn searxng(url: &str) -> Harness {
        let mut harness = Harness::new();
        harness.settings.ai.search.backend = SearchBackend::Searxng;
        harness.settings.ai.search.searxng_base_url = url.to_owned();
        harness
    }

    const RESULTS: &str = r#"{"results":[{"title":"Rust","url":"https://rust-lang.org","content":"A language"},{"title":"Crates","url":"https://crates.io","content":"Registry"},{"title":"Docs","url":"https://docs.rs","content":"Docs"}]}"#;

    #[test]
    fn specs_and_inspection_classify_each_tool() {
        let harness = Harness::new();
        let ctx = harness.ctx();
        let search = WebSearchTool::new();
        assert!(search.spec().read_only && !search.spec().destructive);
        assert_eq!(search.spec().source, ToolSource::Builtin);
        assert_eq!(
            (search.spec().class, search.spec().parallel_safe),
            (ToolClass::Network, true)
        );
        let inspection = search
            .inspect(&ctx, &json!({"query": "  rust async  "}))
            .unwrap();
        assert_eq!(
            inspection,
            CallInspection {
                read_only: true,
                arg_subject: Some("rust async".into()),
                ..CallInspection::default()
            }
        );
        for bad in [json!({}), json!({"query": "  "}), json!({"query": 3})] {
            assert!(matches!(
                search.inspect(&ctx, &bad),
                Err(AppError::Validation { .. })
            ));
        }

        let fetch = FetchUrlTool::new();
        assert!(!fetch.spec().read_only);
        let inspection = fetch
            .inspect(&ctx, &json!({"url": " https://example.com/a?q=1 "}))
            .unwrap();
        assert_eq!(
            inspection,
            CallInspection {
                read_only: false,
                preview: Some(ToolApprovalPreview::Url {
                    url: "https://example.com/a?q=1".into()
                }),
                arg_subject: Some("https://example.com/a?q=1".into()),
                ..CallInspection::default()
            }
        );
        for bad in [
            json!({}),
            json!({"url": "ftp://example.com/x"}),
            json!({"url": "file:///etc/passwd"}),
            json!({"url": "not a url"}),
        ] {
            assert!(
                matches!(fetch.inspect(&ctx, &bad), Err(AppError::Validation { .. })),
                "{bad}"
            );
        }
    }

    #[test]
    fn both_tools_are_registered_as_parallel_safe_network_tools() {
        let mut registry = ToolRegistry::new();
        register(&mut registry);
        let specs = registry.specs();
        let names: Vec<_> = specs.iter().map(|spec| spec.name.as_str()).collect();
        assert_eq!(names, [WEB_SEARCH, FETCH_URL]);
        assert!(
            specs
                .iter()
                .all(|spec| spec.class == ToolClass::Network && spec.parallel_safe)
        );
    }

    #[tokio::test]
    async fn web_search_returns_numbered_hits_with_citations() {
        let (url, requests) = serve("application/json", RESULTS);
        let harness = searxng(&url);
        let output = WebSearchTool::new()
            .call(&harness.ctx(), json!({"query": " rust ", "max_results": 2}))
            .await;
        assert!(!output.is_error, "{}", output.text_content());
        assert_eq!(
            output.text_content(),
            "1. Rust\n   https://rust-lang.org\n   A language\n\n2. Crates\n   https://crates.io\n   Registry"
        );
        let [HubPart::Text { citations, .. }] = output.parts.as_slice() else {
            panic!("one text part expected");
        };
        assert_eq!(
            citations.iter().map(|c| c.url.as_str()).collect::<Vec<_>>(),
            ["https://rust-lang.org", "https://crates.io"]
        );
        assert_eq!(citations[0].title, "Rust");
        assert_eq!(citations[0].snippet.as_deref(), Some("A language"));
        let head = requests.recv().unwrap();
        assert!(
            head.starts_with("GET /search?q=rust&format=json "),
            "{head}"
        );
    }

    #[tokio::test]
    async fn web_search_without_a_key_points_at_settings() {
        let mut harness = Harness::new();
        harness.settings.ai.search.backend = SearchBackend::Tavily;
        let output = WebSearchTool::new()
            .call(&harness.ctx(), json!({"query": "rust"}))
            .await;
        assert!(output.is_error);
        assert!(
            output
                .text_content()
                .contains("add the Tavily API key in Settings > AI > Agent"),
            "{}",
            output.text_content()
        );
    }

    #[tokio::test]
    async fn the_backend_key_is_read_from_the_secret_store() {
        let harness = Harness::new();
        harness
            .state
            .secrets()
            .put(SEARCH_TAVILY, "tvly-1")
            .unwrap();
        harness
            .state
            .secrets()
            .put(SEARCH_BRAVE, "brave-1")
            .unwrap();
        let mut search = AiSearchSettings {
            backend: SearchBackend::Tavily,
            ..AiSearchSettings::default()
        };
        let config = search_config(&harness.state, &search).await.unwrap();
        assert_eq!(
            (
                config.tavily_api_key.as_str(),
                config.brave_api_key.as_str()
            ),
            ("tvly-1", "")
        );
        search.backend = SearchBackend::Brave;
        let config = search_config(&harness.state, &search).await.unwrap();
        assert_eq!(
            (
                config.tavily_api_key.as_str(),
                config.brave_api_key.as_str()
            ),
            ("", "brave-1")
        );
        search.backend = SearchBackend::DuckDuckGo;
        let config = search_config(&harness.state, &search).await.unwrap();
        assert!(config.tavily_api_key.is_empty() && config.brave_api_key.is_empty());
    }

    #[tokio::test]
    async fn web_search_errors_and_cancellation_become_error_results() {
        let harness = searxng("http://127.0.0.1:1");
        let tool = WebSearchTool::new();
        let output = tool.call(&harness.ctx(), json!({"query": 5})).await;
        assert!(output.is_error);
        // The shared schema check names the faulty argument.
        assert!(
            output.text_content().contains("`query`"),
            "{}",
            output.text_content()
        );
        let output = tool.call(&harness.ctx(), json!({"query": "q"})).await;
        assert!(output.is_error, "{}", output.text_content());

        harness.cancel.cancel();
        let output = tool.call(&harness.ctx(), json!({"query": "q"})).await;
        assert!(output.is_error);
        assert_eq!(output.text_content(), AppError::cancelled().message());
    }

    #[tokio::test]
    async fn web_search_uses_the_configured_proxy() {
        let (proxy, requests) = serve("application/json", RESULTS);
        let mut harness = searxng("http://93.184.216.34:8080");
        harness.settings.network.proxy_url = proxy;
        let output = WebSearchTool::new()
            .call(&harness.ctx(), json!({"query": "rust", "max_results": 1}))
            .await;
        assert!(!output.is_error, "{}", output.text_content());
        let head = requests.recv().unwrap();
        assert!(
            head.starts_with("GET http://93.184.216.34:8080/search?q=rust&format=json "),
            "{head}"
        );
    }

    #[tokio::test]
    async fn fetch_url_refuses_non_public_targets_and_bad_urls() {
        let harness = Harness::new();
        let tool = FetchUrlTool::new();
        for url in ["http://127.0.0.1:1/", "ftp://example.com/x"] {
            let output = tool.call(&harness.ctx(), json!({ "url": url })).await;
            assert!(output.is_error, "{url}");
            assert!(
                output.text_content().contains("refusing")
                    || output.text_content().contains("only http"),
                "{url}: {}",
                output.text_content()
            );
        }
        let output = tool.call(&harness.ctx(), json!({})).await;
        assert!(output.is_error);
    }

    #[tokio::test]
    async fn fetch_url_honours_the_proxy_setting_and_validates_it() {
        let (proxy, requests) = serve("text/html", "<html><body><h1>Via proxy</h1></body></html>");
        let mut harness = Harness::new();
        harness.settings.network.proxy_url = proxy;
        let output = FetchUrlTool::new()
            .call(&harness.ctx(), json!({"url": "http://93.184.216.34/page"}))
            .await;
        assert!(!output.is_error, "{}", output.text_content());
        assert_eq!(
            output.text_content(),
            "URL: http://93.184.216.34/page\nStatus: 200\n\nVia proxy"
        );
        let head = requests.recv().unwrap();
        assert!(head.starts_with("GET http://93.184.216.34/page "), "{head}");

        // A private target is refused even though a proxy is configured; nothing reaches it.
        let output = FetchUrlTool::new()
            .call(&harness.ctx(), json!({"url": "http://10.0.0.1/"}))
            .await;
        assert!(output.is_error);
        assert!(requests.try_recv().is_err());

        harness.settings.network.proxy_url = "ftp://proxy.test".into();
        let output = FetchUrlTool::new()
            .call(&harness.ctx(), json!({"url": "http://93.184.216.34/"}))
            .await;
        assert!(output.is_error);
        assert!(output.text_content().contains("proxy URL"));
    }

    #[tokio::test]
    async fn fetch_url_stops_when_the_run_is_cancelled() {
        let harness = Harness::new();
        harness.cancel.cancel();
        let output = FetchUrlTool::new()
            .call(&harness.ctx(), json!({"url": "http://93.184.216.34/"}))
            .await;
        assert!(output.is_error);
        assert_eq!(output.text_content(), AppError::cancelled().message());
    }
}
