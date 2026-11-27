//! A hand-rolled MCP (Model Context Protocol, spec 2025-06-18) client.
//!
//! Single responsibility: connect to one MCP server over stdio or streamable HTTP, list its
//! tools, call tools, list and read its resources, and map the results to model content parts.
//! A server's `notifications/tools/list_changed` marks its cached tool list stale and its
//! `notifications/progress` keeps a long tool call alive ([`signals`]). Which servers exist is
//! decided by `app::agent_service`; [`McpManager`] caches the live [`McpConnection`]s, the
//! `tools/list` answers and the connection status, [`McpToolAdapter`] exposes one server tool to
//! the run loop, [`McpResourceTool`] a server's resources, and [`resolve_transport`] /
//! [`externalize_transport`] keep credentials in the secret store.

mod adapter;
mod credentials;
mod http;
mod manager;
mod resources;
mod result;
mod signals;
mod stdio;
#[cfg(test)]
mod tests;

use std::path::Path;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

pub use spanbay_model::agent::mcp::McpTransportConfig;

use serde::{Deserialize, Serialize};
use serde_json::{Value, json};

use crate::agent::untrusted::sanitize_untrusted;
use http::HttpTransport;
use signals::Signals;
use spanbay_base::cancel::{CancelToken, until_cancelled};
use spanbay_base::error::AppError;
use spanbay_base::secrets::redact_secrets;
use spanbay_llm::llm::hub::HubPart;
use stdio::StdioTransport;

pub use credentials::config_fingerprint;
pub(crate) use http::is_auth_rejection;
pub use stdio::{BASE_ENV, LogSink};

pub use adapter::{
    McpToolAdapter, effective_hints, mcp_name_key, mcp_policy_key, mcp_server_prefix, mcp_tool_name,
};
pub use credentials::{
    delete_server_secrets, externalize_transport, prune_server_secrets, resolve_transport,
};
pub use manager::{Clock, McpManager};
pub use resources::{McpResource, McpResourceTool};

use super::model::AgentMcpServer;

/// The protocol revision this client speaks.
const PROTOCOL_VERSION: &str = "2025-06-18";
/// Revisions a server may answer `initialize` with, newest first.
const SUPPORTED_VERSIONS: [&str; 4] = ["2025-11-25", PROTOCOL_VERSION, "2025-03-26", "2024-11-05"];
/// JSON-RPC "invalid params": an `initialize` answer naming the revisions the server speaks in
/// `data.supported`.
const INVALID_PARAMS: i64 = -32602;
/// Pages followed by `tools/list`.
const MAX_PAGES: usize = 20;
/// Raw `tools/list` entries read before the rest is ignored.
const MAX_LISTED_ITEMS: usize = 1024;
/// Tools kept per server.
const MAX_TOOLS: usize = 256;
/// Longest tool name kept, in bytes.
const MAX_TOOL_NAME_BYTES: usize = 128;
/// Longest tool description kept, in bytes.
const MAX_TOOL_DESCRIPTION_BYTES: usize = 2 * 1024;
/// Largest input schema (serialized) of one tool; a larger one drops the tool, since a cut schema
/// would be invalid.
const MAX_TOOL_SCHEMA_BYTES: usize = 16 * 1024;
/// Names, descriptions and schemas of one server's tools together.
const MAX_TOOLS_TOTAL_BYTES: usize = 256 * 1024;
/// How long the best-effort `notifications/cancelled` may take.
const CANCEL_NOTICE_TIMEOUT: Duration = Duration::from_secs(2);
/// Time to connect to an MCP server and list its tools.
pub const CONNECT_TIMEOUT: Duration = Duration::from_secs(15);
/// Time one MCP tool call may take without a word from the server (each
/// `notifications/progress` of the call restarts it). `ai.agent.mcp_call_timeout_s` sets it per
/// server.
pub const CALL_TIMEOUT: Duration = Duration::from_secs(120);
/// Longest a tool call may run however often it reports progress.
pub const MAX_CALL_DURATION: Duration = Duration::from_secs(30 * 60);

/// A tool a server offers.
#[derive(Debug, Clone, PartialEq)]
pub struct McpTool {
    /// The server-side tool name.
    pub name: String,
    /// What the tool does, for the model (empty when the server gave none).
    pub description: String,
    /// JSON Schema of the arguments object.
    pub input_schema: Value,
    /// `annotations.readOnlyHint`; false when absent.
    pub read_only: bool,
    /// `annotations.destructiveHint`; true when absent unless the tool is read-only.
    pub destructive: bool,
}

/// The outcome of `tools/call` as content parts for the model.
#[derive(Debug, Clone, PartialEq)]
pub struct McpCallResult {
    /// Text blocks (capped at 100 KiB in total) and images (each capped at 5 MiB), in the order the
    /// server sent them; anything else is a short text marker.
    pub parts: Vec<HubPart>,
    /// The tool reported a failure (`isError`).
    pub is_error: bool,
}

impl McpCallResult {
    /// The text parts joined (for logs and tests).
    #[must_use]
    pub fn text(&self) -> String {
        spanbay_llm::llm::hub::text_of(&self.parts)
    }
}

/// What the server said about itself in `initialize`.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct McpServerInfo {
    /// `serverInfo.name`.
    pub name: String,
    /// `serverInfo.version`.
    pub version: String,
    /// The protocol revision the server chose.
    pub protocol_version: String,
    /// The server declared the `resources` capability.
    pub resources: bool,
}

/// The live state of a configured server, as the settings UI shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum McpServerStatus {
    /// Connected.
    Running,
    /// Not connected yet: a connect is under way or nobody has needed the server so far.
    Starting,
    /// A required token is missing (a `{{secret:…}}` placeholder without a stored value) or the
    /// server answered `401`/`403`.
    NeedsAuth,
    /// The last connect failed.
    Error {
        /// Why (secrets redacted).
        message: String,
    },
    /// The server is switched off.
    Disabled,
    /// Its configuration changed since the user last saved (reviewed) it, for example through a
    /// sync import: it does not run until the user saves it again.
    NeedsReview,
}

/// One server's status change; also one row of [`McpManager::status_list`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct McpStatusEvent {
    /// The server's id.
    pub server_id: String,
    /// Its status now.
    pub status: McpServerStatus,
    /// Tools in the cached `tools/list`; `None` when nothing is cached.
    #[serde(default)]
    pub tool_count: Option<usize>,
    /// How many of those are read-only: the server marks them so *and* the user trusts it (the
    /// same rule the server list applies). `None` when nothing is cached.
    #[serde(default)]
    pub read_only_count: Option<usize>,
}

/// One event of the MCP status stream (`agent_mcp_subscribe`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum McpStreamEvent {
    /// A server's status changed, with the counts of its cached tools.
    Status(McpStatusEvent),
    /// Events before `first_seq` were evicted from the replay ring: reload the status list.
    Gap {
        /// The oldest sequence number still replayed.
        first_seq: u64,
    },
}

/// Server ids starting with this are throwaway connections (the settings "Test" button); they
/// publish no status events and keep no log.
pub const TEST_ID_PREFIX: &str = "test:";

/// A configured server with its live state.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMcpServerView {
    /// The stored configuration (secrets only as placeholders).
    pub server: AgentMcpServer,
    /// Whether the server runs.
    pub status: McpServerStatus,
    /// Tools in the cached `tools/list`; `None` when nothing is cached.
    pub tool_count: Option<usize>,
    /// How many of those tools count as read-only (their server is trusted and marks them so).
    pub read_only_count: Option<usize>,
    /// Untrusted, trusted, or trusted with tools whose definition changed since they were pinned.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub trust_state: Option<McpTrustState>,
    /// Server-side names of the tools whose definition changed (each asks every time until the
    /// user reviews it).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub drifted_tools: Option<Vec<String>>,
}

/// How far the user trusts a server (computed by the core, shown by the UI).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum McpTrustState {
    /// New, imported or never trusted: hints are ignored and every call asks.
    Untrusted,
    /// The user trusts it and every cached tool matches its pin.
    Trusted,
    /// Trusted, but some tool's definition changed: that tool asks every time until reviewed.
    NeedsReview,
}

/// `agent_mcp_status_list`: the status of one configured server.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct McpServerStatusDto {
    /// The server's id.
    pub server_id: String,
    /// Its status now.
    pub status: McpServerStatus,
    /// Tools in the cached `tools/list`; `None` when nothing is cached.
    pub tool_count: Option<usize>,
    /// How many of those tools count as read-only (their server is trusted and marks them so).
    pub read_only_count: Option<usize>,
}

/// Locks `mutex`, ignoring poisoning (the guarded data stays consistent in this module).
fn lock<T>(mutex: &Mutex<T>) -> std::sync::MutexGuard<'_, T> {
    mutex
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
}

/// The secret values `config` carries (environment and header values, plus the credential after a
/// scheme such as `Bearer`), for [`redact_secrets`].
fn secret_values(config: &McpTransportConfig) -> Vec<String> {
    let values = match config {
        McpTransportConfig::Stdio { env, .. } => env,
        McpTransportConfig::Http { headers, .. } => headers,
    };
    let mut out = Vec::new();
    for value in values.values().filter(|value| !value.is_empty()) {
        out.push(value.clone());
        if let Some((_, credential)) = value.rsplit_once(' ') {
            out.push(credential.to_owned());
        }
    }
    out
}

/// `error` with every secret value scrubbed from its text (the error kind is kept).
fn scrub(error: AppError, needles: &[String]) -> AppError {
    let message = redact_secrets(error.message(), needles);
    if message == error.message() {
        return error;
    }
    match error {
        AppError::Network { .. } => AppError::network(message),
        AppError::Io { .. } => AppError::io(message),
        AppError::Validation { .. } => AppError::validation(message),
        AppError::Unsupported { .. } => AppError::unsupported(message),
        other => other,
    }
}

/// A failed request: the server's JSON-RPC error or a transport/local error.
enum Failure {
    Rpc {
        message: String,
        code: Option<i64>,
        data: Option<Value>,
    },
    App(AppError),
}

impl From<AppError> for Failure {
    fn from(error: AppError) -> Self {
        Self::App(error)
    }
}

impl From<Failure> for AppError {
    fn from(failure: Failure) -> Self {
        match failure {
            Failure::Rpc { message, .. } => AppError::network(message),
            Failure::App(error) => error,
        }
    }
}

enum Transport {
    Stdio(StdioTransport),
    Http(Box<HttpTransport>),
}

impl Transport {
    async fn send_request(&self, id: u64, message: &Value) -> Result<Value, AppError> {
        match self {
            Self::Stdio(transport) => transport.send_request(id, message).await,
            Self::Http(transport) => transport.send_request(id, message).await,
        }
    }

    async fn send_notification(&self, message: &Value) -> Result<(), AppError> {
        match self {
            Self::Stdio(transport) => transport.send_notification(message).await,
            Self::Http(transport) => transport.send_notification(message).await,
        }
    }
}

/// A live connection to one MCP server. Share it with `Arc`; dropping the last handle kills a
/// stdio child.
pub struct McpConnection {
    transport: Transport,
    next_id: AtomicU64,
    info: McpServerInfo,
    /// Secret values scrubbed from every error this connection returns.
    needles: Vec<String>,
    /// What the server said without being asked.
    signals: Arc<Signals>,
}

/// The user's login-shell `PATH` for a stdio server, resolved off the async threads.
#[cfg(not(test))]
async fn login_path() -> Option<std::ffi::OsString> {
    tokio::task::spawn_blocking(spanbay_base::platform::login_path)
        .await
        .ok()
}

/// Unit tests keep the test process's `PATH`: none depends on the user's shell start-up files.
#[cfg(test)]
async fn login_path() -> Option<std::ffi::OsString> {
    None
}

impl McpConnection {
    /// Starts/reaches the server, runs the `initialize` handshake and announces `initialized`.
    ///
    /// `config` must already be resolved ([`resolve_transport`]). A stdio server runs in `cwd`
    /// ([`McpManager`] always passes one: the config's or the server's own neutral directory) and
    /// gets the proxy variables only when `proxy` is set. `timeout` bounds each handshake step. A stdio server that dies during the
    /// handshake adds its last stderr output to the error. Secret values never appear in a
    /// returned error.
    pub async fn connect(
        config: &McpTransportConfig,
        cwd: Option<&Path>,
        proxy: Option<&str>,
        timeout: Duration,
    ) -> Result<Self, AppError> {
        Self::connect_logged(config, cwd, proxy, timeout, None).await
    }

    /// [`Self::connect`], with every line the stdio server writes to stderr also passed to `log`.
    pub async fn connect_logged(
        config: &McpTransportConfig,
        cwd: Option<&Path>,
        proxy: Option<&str>,
        timeout: Duration,
        log: Option<LogSink>,
    ) -> Result<Self, AppError> {
        let needles = secret_values(config);
        let signals = Arc::new(Signals::default());
        let transport = match config {
            McpTransportConfig::Stdio {
                command,
                args,
                env,
                env_allow,
                ..
            } => {
                let path = login_path().await;
                StdioTransport::spawn(
                    command,
                    args,
                    env,
                    env_allow,
                    cwd,
                    proxy,
                    path.as_deref(),
                    log,
                    Arc::clone(&signals),
                )
                .map(Transport::Stdio)
            }
            McpTransportConfig::Http { url, headers } => {
                HttpTransport::new(url, headers, Arc::clone(&signals))
                    .map(|http| Transport::Http(Box::new(http)))
            }
        }
        .map_err(|error| scrub(error, &needles))?;
        let mut connection = Self {
            transport,
            next_id: AtomicU64::new(0),
            info: McpServerInfo::default(),
            needles,
            signals,
        };
        let handshake = connection.handshake(timeout).await;
        match handshake {
            Ok(info) => {
                connection.info = info;
                Ok(connection)
            }
            Err(error) => Err(connection.explain(error).await),
        }
    }

    /// The server's name, version and negotiated protocol revision.
    #[must_use]
    pub fn server_info(&self) -> &McpServerInfo {
        &self.info
    }

    /// Lists the tools, following `nextCursor` pagination (at most 20 pages); a server's list is
    /// capped in count, description size, schema size and total size (see [`parse_tools`]).
    pub async fn list_tools(&self, timeout: Duration) -> Result<Vec<McpTool>, AppError> {
        Ok(self.list_tools_reporting(timeout).await?.0)
    }

    /// [`Self::list_tools`], plus why each skipped entry was left out (for the server log).
    pub async fn list_tools_reporting(
        &self,
        timeout: Duration,
    ) -> Result<(Vec<McpTool>, Vec<String>), AppError> {
        // A change announced while this list is read marks it again.
        self.signals.take_tools_changed();
        let items = self
            .list_paged("tools/list", "tools", timeout)
            .await
            .map_err(|failure| scrub(failure.into(), &self.needles))?;
        Ok(parse_tools(&items))
    }

    /// Calls tool `name`; a cancelled or timed-out call tells the server with
    /// `notifications/cancelled`. JSON-RPC errors become [`AppError::Network`]; a tool that
    /// reports `isError` is an `Ok` result with `is_error` set.
    pub async fn call_tool(
        &self,
        name: &str,
        arguments: Value,
        timeout: Duration,
        cancel: Option<&CancelToken>,
    ) -> Result<McpCallResult, AppError> {
        let arguments = if arguments.is_null() {
            json!({})
        } else {
            arguments
        };
        let params = json!({"name": name, "arguments": arguments});
        let result = self
            .request("tools/call", Some(params), timeout, cancel)
            .await
            .map_err(|failure| scrub(failure.into(), &self.needles))?;
        Ok(result::parse_call_result(&result))
    }

    /// Whether the server announced since the last `tools/list` that its tools changed.
    #[must_use]
    pub fn tools_changed(&self) -> bool {
        self.signals.tools_changed()
    }

    /// Lists the server's resources (`resources/list`, at most 20 pages), capped like the tool
    /// list ([`resources::parse_resources`]).
    pub async fn list_resources(&self, timeout: Duration) -> Result<Vec<McpResource>, AppError> {
        let items = self
            .list_paged("resources/list", "resources", timeout)
            .await
            .map_err(|failure| scrub(failure.into(), &self.needles))?;
        Ok(resources::parse_resources(&items))
    }

    /// Reads resource `uri` (`resources/read`) as content parts for the model.
    pub async fn read_resource(
        &self,
        uri: &str,
        timeout: Duration,
        cancel: Option<&CancelToken>,
    ) -> Result<McpCallResult, AppError> {
        let result = self
            .request("resources/read", Some(json!({"uri": uri})), timeout, cancel)
            .await
            .map_err(|failure| scrub(failure.into(), &self.needles))?;
        Ok(result::parse_resource_contents(&result))
    }

    /// Stdio: the child runs and its output is still read. Http: always true.
    #[must_use]
    pub fn is_alive(&self) -> bool {
        match &self.transport {
            Transport::Stdio(transport) => transport.is_alive(),
            Transport::Http(_) => true,
        }
    }

    /// Closes the connection right away even while other handles exist: kills a stdio child's
    /// process group, ends an http session (best effort, in the background).
    pub fn shutdown(&self) {
        match &self.transport {
            Transport::Stdio(transport) => transport.shutdown(),
            Transport::Http(transport) => transport.shutdown(),
        }
    }

    async fn handshake(&self, timeout: Duration) -> Result<McpServerInfo, Failure> {
        let initialize = |version: &str| {
            json!({
                "protocolVersion": version,
                "capabilities": {},
                "clientInfo": {"name": "spanbay", "version": env!("CARGO_PKG_VERSION")},
            })
        };
        let mut params = initialize(PROTOCOL_VERSION);
        let result = match self
            .request("initialize", Some(params.clone()), timeout, None)
            .await
        {
            // The server refused our revision and named its own: retry once with the newest
            // one both sides speak.
            Err(Failure::Rpc {
                code: Some(INVALID_PARAMS),
                data: Some(data),
                message,
            }) => {
                let Some(version) = negotiated(&data) else {
                    return Err(Failure::Rpc {
                        message,
                        code: Some(INVALID_PARAMS),
                        data: Some(data),
                    });
                };
                params = initialize(version);
                self.request("initialize", Some(params.clone()), timeout, None)
                    .await?
            }
            other => other?,
        };
        let protocol_version = result
            .get("protocolVersion")
            .and_then(Value::as_str)
            .unwrap_or(PROTOCOL_VERSION);
        if !SUPPORTED_VERSIONS.contains(&protocol_version) {
            return Err(AppError::unsupported(format!(
                "MCP server uses unsupported protocol version {protocol_version}"
            ))
            .into());
        }
        let text = |key: &str| {
            result
                .pointer(&format!("/serverInfo/{key}"))
                .and_then(Value::as_str)
                .unwrap_or_default()
                .to_owned()
        };
        let info = McpServerInfo {
            name: text("name"),
            version: text("version"),
            protocol_version: protocol_version.to_owned(),
            resources: result
                .pointer("/capabilities/resources")
                .is_some_and(Value::is_object),
        };
        if let Transport::Http(transport) = &self.transport {
            transport.set_handshake(protocol_version, params);
        }
        let initialized = json!({"jsonrpc": "2.0", "method": "notifications/initialized"});
        tokio::time::timeout(timeout, self.transport.send_notification(&initialized))
            .await
            .map_err(|_| timed_out(timeout))??;
        Ok(info)
    }

    /// Adds the server's stderr to a handshake error of a stdio server and scrubs secrets.
    async fn explain(&self, failure: Failure) -> AppError {
        let error = scrub(AppError::from(failure), &self.needles);
        let Transport::Stdio(transport) = &self.transport else {
            return error;
        };
        if transport.is_alive() {
            return error;
        }
        // Give the stderr thread a moment to catch up with a process that just exited.
        tokio::time::sleep(Duration::from_millis(100)).await;
        match transport.stderr_tail() {
            tail if tail.is_empty() => error,
            tail => scrub(
                AppError::network(format!("{}; server stderr: {tail}", error.message())),
                &self.needles,
            ),
        }
    }

    async fn list_paged(
        &self,
        method: &str,
        key: &str,
        timeout: Duration,
    ) -> Result<Vec<Value>, Failure> {
        let mut items = Vec::new();
        let mut cursor: Option<String> = None;
        for _ in 0..MAX_PAGES {
            let params = cursor.as_ref().map(|cursor| json!({"cursor": cursor}));
            let page = self.request(method, params, timeout, None).await?;
            if let Some(entries) = page.get(key).and_then(Value::as_array) {
                let room = MAX_LISTED_ITEMS.saturating_sub(items.len());
                items.extend(entries.iter().take(room).cloned());
            }
            cursor = page
                .get("nextCursor")
                .and_then(Value::as_str)
                .filter(|cursor| !cursor.is_empty())
                .map(str::to_owned);
            if cursor.is_none() || items.len() >= MAX_LISTED_ITEMS {
                break;
            }
        }
        Ok(items)
    }

    /// Sends one request and returns its `result`. A `tools/call` asks for progress (its id is the
    /// `progressToken`): each progress notification restarts `timeout`, up to
    /// [`MAX_CALL_DURATION`] in all; any other request has `timeout` in all.
    async fn request(
        &self,
        method: &str,
        params: Option<Value>,
        timeout: Duration,
        cancel: Option<&CancelToken>,
    ) -> Result<Value, Failure> {
        if cancel.is_some_and(CancelToken::is_cancelled) {
            return Err(AppError::cancelled().into());
        }
        let id = self.next_id.fetch_add(1, Ordering::SeqCst) + 1;
        let progress = method == "tools/call";
        let mut message = json!({"jsonrpc": "2.0", "id": id, "method": method});
        if let Some(mut params) = params {
            if progress && let Some(object) = params.as_object_mut() {
                object.insert("_meta".to_owned(), json!({"progressToken": id}));
            }
            message["params"] = params;
        }
        let call = self.await_response(id, &message, timeout, progress);
        let error = match until_cancelled(cancel, call).await {
            Some(Some(Ok(response))) => return result_of(response),
            Some(Some(Err(error))) => return Err(error.into()),
            Some(None) => timed_out(timeout),
            None => AppError::cancelled(),
        };
        // The request future was dropped, which already forgot the pending id. The spec forbids
        // cancelling `initialize`.
        if method != "initialize" {
            let notice = json!({
                "jsonrpc": "2.0",
                "method": "notifications/cancelled",
                "params": {"requestId": id, "reason": error.message()},
            });
            let _ = tokio::time::timeout(
                CANCEL_NOTICE_TIMEOUT,
                self.transport.send_notification(&notice),
            )
            .await;
        }
        Err(error.into())
    }
}

impl McpConnection {
    /// The response to request `id`, or `None` when it timed out: `timeout` after the start, or
    /// with `progress` after the last progress notification (at most [`MAX_CALL_DURATION`]).
    async fn await_response(
        &self,
        id: u64,
        message: &Value,
        timeout: Duration,
        progress: bool,
    ) -> Option<Result<Value, AppError>> {
        if !progress {
            return tokio::time::timeout(timeout, self.transport.send_request(id, message))
                .await
                .ok();
        }
        let _tracking = self.signals.track(id);
        let started = tokio::time::Instant::now();
        let cap = started + MAX_CALL_DURATION.max(timeout);
        let send = self.transport.send_request(id, message);
        tokio::pin!(send);
        loop {
            let last = self
                .signals
                .last_progress(id)
                .map_or(started, tokio::time::Instant::from_std)
                .max(started);
            let until = (last + timeout).min(cap);
            match tokio::time::timeout_at(until, &mut send).await {
                Ok(response) => return Some(response),
                Err(_) => {
                    let moved = self
                        .signals
                        .last_progress(id)
                        .map(tokio::time::Instant::from_std)
                        .is_some_and(|now| now > last);
                    if !moved || tokio::time::Instant::now() >= cap {
                        return None;
                    }
                }
            }
        }
    }
}

fn timed_out(timeout: Duration) -> AppError {
    AppError::network(format!(
        "MCP request timed out after {} s",
        timeout.as_secs_f64()
    ))
}

/// The newest revision we speak among a `-32602` answer's `data.supported`.
fn negotiated(data: &Value) -> Option<&'static str> {
    let offered: Vec<&str> = data
        .get("supported")?
        .as_array()?
        .iter()
        .filter_map(Value::as_str)
        .collect();
    SUPPORTED_VERSIONS
        .into_iter()
        .find(|version| offered.contains(version))
}

/// The `result` of a response object, or its `error`.
fn result_of(response: Value) -> Result<Value, Failure> {
    if let Some(error) = response.get("error") {
        return Err(Failure::Rpc {
            message: error
                .get("message")
                .and_then(Value::as_str)
                .unwrap_or("MCP server returned an error")
                .to_owned(),
            code: error.get("code").and_then(Value::as_i64),
            data: error.get("data").cloned(),
        });
    }
    Ok(response.get("result").cloned().unwrap_or(Value::Null))
}

/// Parses one `tools/list` entry, or says why it is skipped: no name, a name or an input schema
/// over the caps, or a root schema that is not an object. A root schema without `type` or
/// `properties` gets `type: object` and `properties: {}`. The description is made safe with
/// [`sanitize_untrusted`] and cut to [`MAX_TOOL_DESCRIPTION_BYTES`] (at a character boundary): it
/// goes verbatim into the model's tool list, so a server cannot use it to flood the context or
/// hide text in it.
fn parse_tool(value: &Value) -> Result<McpTool, String> {
    let hint = |key: &str| {
        value
            .pointer(&format!("/annotations/{key}"))
            .and_then(Value::as_bool)
    };
    let read_only = hint("readOnlyHint").unwrap_or(false);
    let name = value
        .get("name")
        .and_then(Value::as_str)
        .ok_or_else(|| "a tool without a name".to_owned())?;
    let label = || name.chars().take(64).collect::<String>();
    if name.len() > MAX_TOOL_NAME_BYTES {
        return Err(format!("{}…: the name is over 128 bytes", label()));
    }
    let input_schema =
        normalized_schema(value.get("inputSchema")).map_err(|why| format!("{}: {why}", label()))?;
    if input_schema.to_string().len() > MAX_TOOL_SCHEMA_BYTES {
        return Err(format!("{}: the input schema is over 16 KiB", label()));
    }
    let raw = value
        .get("description")
        .and_then(Value::as_str)
        .unwrap_or_default();
    let mut description = sanitize_untrusted(raw).into_owned();
    if description.len() > MAX_TOOL_DESCRIPTION_BYTES {
        let mut end = MAX_TOOL_DESCRIPTION_BYTES;
        while !description.is_char_boundary(end) {
            end -= 1;
        }
        description.truncate(end);
        description.push('…');
    }
    Ok(McpTool {
        name: name.to_owned(),
        description,
        input_schema,
        read_only,
        destructive: !read_only && hint("destructiveHint").unwrap_or(true),
    })
}

/// The root input schema as an object schema: `type: object` and `properties: {}` added when
/// missing; anything else at the root is refused.
fn normalized_schema(schema: Option<&Value>) -> Result<Value, &'static str> {
    let mut schema = match schema {
        None | Some(Value::Null) => serde_json::Map::new(),
        Some(Value::Object(schema)) => schema.clone(),
        Some(_) => return Err("the input schema is not a JSON object"),
    };
    match schema.get("type") {
        None => {
            schema.insert("type".to_owned(), json!("object"));
        }
        Some(kind) if kind == "object" => {}
        Some(_) => return Err("the input schema's root type is not `object`"),
    }
    match schema.get("properties") {
        None => {
            schema.insert("properties".to_owned(), json!({}));
        }
        Some(Value::Object(_)) => {}
        Some(_) => return Err("the input schema's `properties` is not an object"),
    }
    Ok(Value::Object(schema))
}

/// The usable tools of a `tools/list` answer: at most [`MAX_TOOLS`] and [`MAX_TOOLS_TOTAL_BYTES`]
/// of names, descriptions and schemas together, in the server's order; plus why each skipped
/// entry was skipped.
fn parse_tools(items: &[Value]) -> (Vec<McpTool>, Vec<String>) {
    let mut total = 0;
    let mut tools = Vec::new();
    let mut skipped = Vec::new();
    for item in items {
        let tool = match parse_tool(item) {
            Ok(tool) => tool,
            Err(why) => {
                skipped.push(why);
                continue;
            }
        };
        total += tool.name.len() + tool.description.len() + tool.input_schema.to_string().len();
        if tools.len() >= MAX_TOOLS || total > MAX_TOOLS_TOTAL_BYTES {
            break;
        }
        tools.push(tool);
    }
    // Sort deterministically by tool name so provider prompt-cache prefixes never bust when an
    // MCP server returns tools from an unordered map across restarts or TTL refreshes.
    tools.sort_by(|a, b| a.name.cmp(&b.name));
    (tools, skipped)
}

/// The definition fingerprint of `tool` (hex SHA-256 over its name, sanitized description,
/// normalized input schema with sorted keys, and hints): what a trusted server's tool is pinned
/// to.
#[must_use]
pub fn tool_fingerprint(tool: &McpTool) -> String {
    // `serde_json` maps are sorted (no `preserve_order`), so key order never matters.
    let canonical = json!({
        "name": tool.name,
        "description": tool.description,
        "input_schema": tool.input_schema,
        "read_only": tool.read_only,
        "destructive": tool.destructive,
    });
    spanbay_base::crypto::sha256_hex(canonical.to_string().as_bytes())
}
