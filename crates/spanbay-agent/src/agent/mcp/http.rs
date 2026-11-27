//! MCP streamable-HTTP transport: every client message is one POST.
//!
//! The server answers a request with a single JSON object or an SSE stream that ends with the
//! response (notifications on that stream, such as progress, go to the connection's [`Signals`]);
//! notifications get `202`. Dropping the request future aborts the HTTP exchange, which
//! is how timeouts and cancellation work here.

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};
use std::time::Duration;

use reqwest::StatusCode;
use reqwest::header::{ACCEPT, CONTENT_TYPE, HeaderMap, HeaderName, HeaderValue};
use serde_json::{Value, json};

use super::lock;
use super::signals::Signals;
use spanbay_base::error::AppError;
use spanbay_llm::llm::sse::{OVERFLOW_EVENT, SseParser};

/// Longest server error body quoted in an error message.
const MAX_ERROR_BODY: usize = 512;
/// Largest non-SSE response body accepted (16 MiB).
pub(super) const MAX_BODY_BYTES: usize = 16 * 1024 * 1024;
/// How long the best-effort session `DELETE` may take.
const CLOSE_TIMEOUT: Duration = Duration::from_secs(2);

/// Marks an error text as an answer that carried a `WWW-Authenticate` challenge.
const CHALLENGE_MARK: &str = " (authentication required)";

/// Whether `error` is the server asking for credentials: `401`, or `403` with a
/// `WWW-Authenticate` header. A bare `403` is often an Origin or Host rejection and stays an
/// ordinary error.
pub(crate) fn is_auth_rejection(error: &AppError) -> bool {
    let message = error.message();
    message.starts_with("MCP server returned HTTP 401")
        || (message.starts_with("MCP server returned HTTP 403") && message.contains(CHALLENGE_MARK))
}

/// The error for a non-success `status`, naming the challenge when the answer had one.
fn status_error(status: StatusCode, headers: &HeaderMap, body: Option<&str>) -> AppError {
    let mark = if headers.contains_key(reqwest::header::WWW_AUTHENTICATE) {
        CHALLENGE_MARK
    } else {
        ""
    };
    match body {
        Some(body) => AppError::network(format!("MCP server returned HTTP {status}{mark}: {body}")),
        None => AppError::network(format!("MCP server returned HTTP {status}{mark}")),
    }
}

/// A streamable-HTTP MCP endpoint.
pub(super) struct HttpTransport {
    client: reqwest::Client,
    url: url::Url,
    headers: HeaderMap,
    session_id: Mutex<Option<String>>,
    protocol_version: Mutex<Option<String>>,
    /// The `initialize` parameters of the first handshake, replayed for a new session.
    initialize_params: Mutex<Option<Value>>,
    /// Serializes session renewal so concurrent `404`s start one new session.
    renewing: tokio::sync::Mutex<()>,
    /// Where the server's notifications go.
    signals: Arc<Signals>,
}

impl HttpTransport {
    /// Validates `url` (http/https) and `headers`; makes no network call.
    pub(super) fn new(
        url: &str,
        headers: &BTreeMap<String, String>,
        signals: Arc<Signals>,
    ) -> Result<Self, AppError> {
        let url = url::Url::parse(url.trim())
            .map_err(|error| AppError::validation(format!("Invalid MCP server URL: {error}")))?;
        if !matches!(url.scheme(), "http" | "https") {
            return Err(AppError::validation(
                "MCP server URL must start with http:// or https://",
            ));
        }
        let mut map = HeaderMap::new();
        for (name, value) in headers {
            let name = HeaderName::from_bytes(name.trim().as_bytes())
                .map_err(|_| AppError::validation(format!("Invalid MCP header name `{name}`")))?;
            let value = HeaderValue::from_str(value).map_err(|_| {
                AppError::validation(format!("Invalid value for MCP header `{name}`"))
            })?;
            map.insert(name, value);
        }
        // No redirects: the default policy re-sends custom credential headers and the JSON-RPC
        // body (tool arguments) to whatever host a compromised server points at.
        let mut builder = reqwest::Client::builder()
            .connect_timeout(Duration::from_secs(15))
            .redirect(reqwest::redirect::Policy::none());
        let local = match url.host() {
            Some(url::Host::Domain(name)) => name == "localhost",
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            None => false,
        };
        if local {
            builder = builder.no_proxy();
        }
        let client = builder
            .build()
            .map_err(|error| AppError::internal(format!("Failed to build HTTP client: {error}")))?;
        Ok(Self {
            client,
            url,
            headers: map,
            session_id: Mutex::new(None),
            protocol_version: Mutex::new(None),
            initialize_params: Mutex::new(None),
            renewing: tokio::sync::Mutex::new(()),
            signals,
        })
    }

    /// Remembers the negotiated protocol version, announced on every later request, and the
    /// `initialize` parameters, replayed when the server forgets the session.
    pub(super) fn set_handshake(&self, version: &str, initialize_params: Value) {
        *lock(&self.protocol_version) = Some(version.to_owned());
        *lock(&self.initialize_params) = Some(initialize_params);
    }

    /// Sends `request` with the standard headers plus the session and protocol version, if known.
    fn decorate(&self, request: reqwest::RequestBuilder) -> reqwest::RequestBuilder {
        let mut request = request.headers(self.headers.clone());
        if let Some(version) = lock(&self.protocol_version).clone() {
            request = request.header("MCP-Protocol-Version", version);
        }
        if let Some(session) = lock(&self.session_id).clone() {
            request = request.header("Mcp-Session-Id", session);
        }
        request
    }

    async fn post(&self, message: &Value) -> Result<reqwest::Response, AppError> {
        let request = self
            .decorate(self.client.post(self.url.clone()))
            .header(CONTENT_TYPE, "application/json")
            .header(ACCEPT, "application/json, text/event-stream")
            .body(serde_json::to_vec(message)?);
        let response = request.send().await.map_err(|error| {
            AppError::network(format!("MCP request failed: {}", error.without_url()))
        })?;
        // Only a successful answer may set the session (an error page must not).
        if response.status().is_success()
            && let Some(session) = response
                .headers()
                .get("mcp-session-id")
                .and_then(|value| value.to_str().ok())
        {
            *lock(&self.session_id) = Some(session.to_owned());
        }
        Ok(response)
    }

    /// Ends the session with a best-effort `DELETE` on a helper thread (2 s limit); never blocks.
    pub(super) fn shutdown(&self) {
        let Some(session) = lock(&self.session_id).take() else {
            return;
        };
        let mut request = self
            .client
            .delete(self.url.clone())
            .headers(self.headers.clone());
        if let Some(version) = lock(&self.protocol_version).clone() {
            request = request.header("MCP-Protocol-Version", version);
        }
        let request = request.header("Mcp-Session-Id", session);
        let _ = std::thread::Builder::new()
            .name("mcp-http-close".to_owned())
            .spawn(move || {
                // The caller may be outside any runtime (the app exit hook), so bring our own.
                let Ok(runtime) = tokio::runtime::Builder::new_current_thread()
                    .enable_all()
                    .build()
                else {
                    return;
                };
                let _ = runtime
                    .block_on(async { tokio::time::timeout(CLOSE_TIMEOUT, request.send()).await });
            });
    }

    /// POSTs a request and returns the response object with `id`. A `404` on a request that
    /// carried a session means the server dropped it: start a new session (`initialize` +
    /// `notifications/initialized`) and send the request once more.
    pub(super) async fn send_request(&self, id: u64, message: &Value) -> Result<Value, AppError> {
        let sent_session = lock(&self.session_id).clone();
        let mut response = self.post(message).await?;
        if response.status() == StatusCode::NOT_FOUND
            && let Some(stale) = sent_session
        {
            self.renew_session(&stale).await?;
            response = self.post(message).await?;
        }
        self.read_response(response, id).await
    }

    /// Replaces the session `stale` unless a concurrent request already did.
    async fn renew_session(&self, stale: &str) -> Result<(), AppError> {
        let _turn = self.renewing.lock().await;
        if lock(&self.session_id).as_deref() != Some(stale) {
            return Ok(());
        }
        *lock(&self.session_id) = None;
        let params = lock(&self.initialize_params)
            .clone()
            .ok_or_else(|| AppError::network("MCP session expired"))?;
        // Real request ids start at 1.
        let initialize =
            json!({"jsonrpc": "2.0", "id": 0, "method": "initialize", "params": params});
        let reply = self.read_response(self.post(&initialize).await?, 0).await?;
        if let Some(error) = reply.get("error") {
            return Err(AppError::network(format!(
                "MCP server refused to start a new session: {error}"
            )));
        }
        self.send_notification(&json!({"jsonrpc": "2.0", "method": "notifications/initialized"}))
            .await
    }

    /// POSTs a notification; the server answers `202` (any success status is accepted).
    pub(super) async fn send_notification(&self, message: &Value) -> Result<(), AppError> {
        let response = self.post(message).await?;
        let status = response.status();
        if status.is_success() {
            Ok(())
        } else {
            Err(status_error(status, response.headers(), None))
        }
    }
}

impl Drop for HttpTransport {
    fn drop(&mut self) {
        self.shutdown();
    }
}

impl HttpTransport {
    /// Reads the answer to request `id` from a JSON body or an SSE stream, with size caps. A
    /// `ping` the server sends on the stream is answered (best effort) before reading on.
    async fn read_response(
        &self,
        mut response: reqwest::Response,
        id: u64,
    ) -> Result<Value, AppError> {
        let status = response.status();
        if !status.is_success() {
            let mut body = Vec::new();
            while body.len() < MAX_ERROR_BODY
                && let Ok(Some(chunk)) = response.chunk().await
            {
                body.extend_from_slice(&chunk);
            }
            let body: String = String::from_utf8_lossy(&body)
                .chars()
                .take(MAX_ERROR_BODY)
                .collect();
            return Err(status_error(status, response.headers(), Some(&body)));
        }
        let is_sse = response
            .headers()
            .get(CONTENT_TYPE)
            .and_then(|value| value.to_str().ok())
            .is_some_and(|value| value.to_ascii_lowercase().starts_with("text/event-stream"));
        if !is_sse {
            let mut body = Vec::new();
            while let Some(chunk) = response.chunk().await.map_err(|error| {
                AppError::network(format!(
                    "Failed to read MCP response: {}",
                    error.without_url()
                ))
            })? {
                if body.len() + chunk.len() > MAX_BODY_BYTES {
                    return Err(AppError::network("MCP server response is over 16 MiB"));
                }
                body.extend_from_slice(&chunk);
            }
            return serde_json::from_slice(&body).map_err(|error| {
                AppError::network(format!("MCP server sent invalid JSON: {error}"))
            });
        }
        let mut parser = SseParser::new();
        let matches = |data: &str| {
            serde_json::from_str::<Value>(data).ok().filter(|value| {
                value.get("method").is_none() && value.get("id") == Some(&Value::from(id))
            })
        };
        loop {
            let (events, ended) = match response.chunk().await {
                Ok(Some(chunk)) => (parser.feed(&chunk), false),
                Ok(None) => (parser.finish(), true),
                Err(error) => {
                    return Err(AppError::network(format!(
                        "Failed to read MCP event stream: {}",
                        error.without_url()
                    )));
                }
            };
            // The parser caps lines and events (8 MiB) and reports a breach as a synthetic event.
            if let Some(overflow) = events
                .iter()
                .find(|event| event.event.as_deref() == Some(OVERFLOW_EVENT))
            {
                return Err(AppError::network(format!(
                    "MCP event stream rejected: {}",
                    overflow.data
                )));
            }
            for ping in events.iter().filter_map(|event| ping_id(&event.data)) {
                let reply = json!({"jsonrpc": "2.0", "id": ping, "result": {}});
                let _ = self.post(&reply).await;
            }
            for event in &events {
                if let Ok(message) = serde_json::from_str::<Value>(&event.data)
                    && message.get("method").is_some()
                    && message.get("id").is_none()
                {
                    self.signals.notice(&message);
                }
            }
            if let Some(found) = events.iter().find_map(|event| matches(&event.data)) {
                return Ok(found);
            }
            if ended {
                return Err(AppError::network(
                    "MCP event stream ended without a response",
                ));
            }
        }
    }
}

/// The id of a server-to-client `ping` request carried by an SSE event.
fn ping_id(data: &str) -> Option<Value> {
    let value = serde_json::from_str::<Value>(data).ok()?;
    (value.get("method").and_then(Value::as_str) == Some("ping"))
        .then(|| value.get("id").cloned())
        .flatten()
}
