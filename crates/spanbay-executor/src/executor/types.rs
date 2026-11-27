//! Public data types and errors shared by the executor submodules.

use serde::{Deserialize, Serialize};
use spanbay_base::cancel::CancelToken;
use spanbay_base::error::NetworkCause;
use spanbay_model::models::TlsCertificate;
use spanbay_model::settings::{DataSettings, NetworkSettings};
use spanbay_model::{CookieJarEntry, VariableScopes};
use spanbay_response::bodies::INLINE_BODY_LIMIT_BYTES;
use std::time::Duration;
use thiserror::Error;

use super::response::MAX_RESPONSE_BODY_BYTES;

/// Per-run inputs for the executor: cookies, variable scopes, TLS material and cancellation.
#[derive(Debug, Clone, Default)]
pub struct ExecutorOptions {
    pub cookie_jar: Vec<CookieJarEntry>,
    pub variable_scopes: Option<VariableScopes>,
    /// Workspace TLS certificates. Enabled `Ca` entries are added as extra roots
    /// and an enabled `Client` entry is matched against the request host.
    pub certificates: Vec<TlsCertificate>,
    /// Aborts the in-flight request promptly when the token fires.
    pub cancel: Option<CancelToken>,
    /// Response size thresholds (`data` settings).
    pub limits: ExecutorLimits,
    /// Transport defaults for calls made besides the request itself (`network` settings).
    pub auxiliary: AuxiliaryNetwork,
    /// When set, a redirect may only be followed to one of these hosts (`host` or `host:port`,
    /// lowercase). Set for sends that carry a secret variable on behalf of the agent, whose
    /// secrets travel only to hosts the environment declares.
    pub redirect_hosts: Option<std::sync::Arc<Vec<String>>>,
    /// When set, every name lookup of the client is validated by the SSRF guard and the connection
    /// uses exactly the validated addresses (no second resolution), see [`ResolveGuard`].
    pub resolve_guard: Option<ResolveGuard>,
    /// Tokens fetched with the OAuth 2.0 token card; consulted before any send-time fetch.
    pub oauth_tokens: OAuthTokenSource,
    /// A cancel that fires after the final response's headers arrived returns the partial
    /// response (marked [`BodyIncomplete::Cancelled`]) instead of [`ExecutionError::Cancelled`].
    /// Set for a single send from the UI, where Stop ends a stream the user was watching.
    pub keep_partial_on_cancel: bool,
    /// Writes the whole final response body to this file as it arrives, with no size cap; the
    /// result still captures up to `limits.max_body_bytes` for scripts, assertions and the viewer.
    /// The path comes from a native save dialog, never from the webview.
    pub save_body_to: Option<std::path::PathBuf>,
}

/// Why a response body ended before the server finished sending it.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BodyIncomplete {
    /// No body bytes arrived for the request's idle timeout (`timeout_ms`).
    IdleTimeout,
    /// The request's total time limit (`total_timeout_ms`) ran out.
    TotalTimeout,
    /// The user stopped the send.
    Cancelled,
}

/// Pins an executor's connections to addresses [`spanbay_base::net_guard`] validated: the
/// resolver of a guarded client classifies every address a name resolves to, refuses
/// link-local/metadata/multicast/reserved ones always and private ones unless `private_hosts`
/// allows the host, and hands reqwest only the validated addresses, so a DNS answer that changes
/// between a pre-check and the connect (a rebind) cannot reach another address. An IP-literal host
/// skips DNS and is classified by [`ResolveGuard::check_literal`] before each hop. Behind an
/// explicit proxy the proxy resolves names, so only literals can be checked there.
#[derive(Clone, Default)]
pub struct ResolveGuard {
    /// Hosts (`host` or `host:port`, lowercase) that may resolve to loopback and private
    /// addresses; `None` allows private and loopback for every host (a saved request the user
    /// owns), `Some` only for the listed ones (an agent-built or agent-touched request).
    pub private_hosts: Option<std::sync::Arc<Vec<String>>>,
    /// Replaces the system lookup in tests, so a rebind is reproducible without DNS.
    #[cfg(test)]
    pub(crate) lookup: Option<LookupOverride>,
}

/// A test-only replacement of the system name lookup.
#[cfg(test)]
pub(crate) type LookupOverride =
    std::sync::Arc<dyn Fn(&str) -> Vec<std::net::IpAddr> + Send + Sync>;

impl std::fmt::Debug for ResolveGuard {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ResolveGuard")
            .field("private_hosts", &self.private_hosts)
            .finish_non_exhaustive()
    }
}

impl ResolveGuard {
    /// A guard that allows loopback and private hosts only for `private_hosts` (`None`: for
    /// every host).
    #[must_use]
    pub fn new(private_hosts: Option<std::sync::Arc<Vec<String>>>) -> Self {
        Self {
            private_hosts,
            #[cfg(test)]
            lookup: None,
        }
    }

    /// Whether an address of `class` may be used for `host:port`. A private-host entry is the bare
    /// host (any port) or the exact `host:port` (clients are built per host and port).
    pub(crate) fn permits(
        &self,
        host: &str,
        port: u16,
        class: spanbay_base::net_guard::HostClass,
    ) -> bool {
        use spanbay_base::net_guard::HostClass;
        match class {
            HostClass::Public => true,
            HostClass::LinkLocalOrMetadata => false,
            HostClass::Loopback | HostClass::PrivateRfc1918 | HostClass::Cgnat => {
                self.private_hosts.as_ref().is_none_or(|hosts| {
                    let host = host.to_ascii_lowercase();
                    let with_port = format!("{host}:{port}");
                    hosts
                        .iter()
                        .any(|entry| *entry == host || *entry == with_port)
                })
            }
        }
    }

    /// Classifies an IP-literal or `localhost` URL host (no DNS) and refuses it when not allowed.
    pub(crate) fn check_literal(&self, host: &str, port: u16) -> Result<(), GuardRefusal> {
        match spanbay_base::net_guard::classify_host_literal(host) {
            Some(class) if !self.permits(host, port, class) => Err(GuardRefusal {
                host: host.to_owned(),
                address: None,
            }),
            _ => Ok(()),
        }
    }

    /// Part of the client cache key: a guarded client is never reused for another guard.
    pub(crate) fn cache_key(&self) -> String {
        match &self.private_hosts {
            None => "guard:any".to_owned(),
            Some(hosts) => {
                let mut hosts: Vec<&str> = hosts.iter().map(String::as_str).collect();
                hosts.sort_unstable();
                format!("guard:{}", hosts.join(","))
            }
        }
    }
}

/// The SSRF guard refused to connect to `host`. The resolver of a guarded client returns it boxed
/// inside the transport error and `check_literal` returns it directly; the send maps both to
/// [`ExecutionError::GuardRefused`], so a refusal is told apart from a connect failure.
#[derive(Debug)]
pub(crate) struct GuardRefusal {
    pub(crate) host: String,
    /// The resolved address that was refused; `None` for an IP-literal host.
    pub(crate) address: Option<std::net::IpAddr>,
}

impl std::fmt::Display for GuardRefusal {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "refusing to connect to {}", self.host)?;
        if let Some(address) = self.address {
            write!(f, " ({address})")?;
        }
        f.write_str(": the address is not one this request may reach")
    }
}

impl std::error::Error for GuardRefusal {}

impl GuardRefusal {
    /// The refusal inside `error`'s source chain, when the guard caused it.
    pub(crate) fn find<'a>(error: &'a (dyn std::error::Error + 'static)) -> Option<&'a Self> {
        let mut next = Some(error);
        while let Some(error) = next {
            if let Some(refusal) = error.downcast_ref::<Self>() {
                return Some(refusal);
            }
            next = error.source();
        }
        None
    }
}

/// Where the executor finds a token the user fetched with the OAuth 2.0 token card.
pub trait StoredOAuthTokens: Send + Sync {
    /// The stored access token for `key` (see `oauth::token_key`) when it has not expired.
    /// Blocking: the executor calls it on the blocking pool.
    fn valid_access_token(&self, key: &str) -> Option<String>;

    /// The stored refresh token for `key`, if any. Blocking.
    fn refresh_token(&self, _key: &str) -> Option<String> {
        None
    }

    /// Saves `grant` (a refreshed token) under `key`; `false` when it could not be stored.
    /// Blocking.
    fn store_grant(&self, _key: &str, _grant: &super::auth::TokenGrant) -> bool {
        false
    }

    /// Marks the stored access token under `key` as expired when it is still `token` (the
    /// server refused it); its refresh token stays. Blocking.
    fn invalidate(&self, _key: &str, _token: &str) {}

    /// `token` is the access token a send attaches, whichever way it was had (stored, cached,
    /// refreshed or fetched). Called on the async runtime, so it must not block.
    fn sent(&self, _token: &str) {}
}

/// A shared [`StoredOAuthTokens`]; the default holds none.
#[derive(Clone, Default)]
pub struct OAuthTokenSource(Option<std::sync::Arc<dyn StoredOAuthTokens>>);

impl OAuthTokenSource {
    /// A source reading `tokens`.
    #[must_use]
    pub fn new(tokens: std::sync::Arc<dyn StoredOAuthTokens>) -> Self {
        Self(Some(tokens))
    }

    /// The valid stored access token for `key`, if any.
    pub(crate) fn valid_access_token(&self, key: &str) -> Option<String> {
        self.0.as_ref()?.valid_access_token(key)
    }

    /// The stored refresh token for `key`, if any.
    pub(crate) fn refresh_token(&self, key: &str) -> Option<String> {
        self.0.as_ref()?.refresh_token(key)
    }

    /// Saves a refreshed `grant` under `key`.
    pub(crate) fn store_grant(&self, key: &str, grant: &super::auth::TokenGrant) -> bool {
        self.0
            .as_ref()
            .is_some_and(|tokens| tokens.store_grant(key, grant))
    }

    /// Marks the stored token under `key` as expired when it is still `token`.
    pub(crate) fn invalidate(&self, key: &str, token: &str) {
        if let Some(tokens) = self.0.as_ref() {
            tokens.invalidate(key, token);
        }
    }

    /// Reports `token` as attached to the send.
    pub(crate) fn sent(&self, token: &str) {
        if let Some(tokens) = self.0.as_ref() {
            tokens.sent(token);
        }
    }
}

impl std::fmt::Debug for OAuthTokenSource {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str("OAuthTokenSource(..)")
    }
}

/// Response size thresholds: how much body is read and how much stays inline.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ExecutorLimits {
    /// Largest response body read; the rest is cut.
    pub max_body_bytes: usize,
    /// Bodies up to this size keep their base64 inline.
    pub inline_body_bytes: usize,
}

impl Default for ExecutorLimits {
    fn default() -> Self {
        Self {
            max_body_bytes: MAX_RESPONSE_BODY_BYTES,
            inline_body_bytes: INLINE_BODY_LIMIT_BYTES,
        }
    }
}

impl From<&DataSettings> for ExecutorLimits {
    fn from(data: &DataSettings) -> Self {
        Self {
            max_body_bytes: data.response_body_cap(),
            inline_body_bytes: usize::try_from(data.inline_threshold_bytes).unwrap_or(usize::MAX),
        }
    }
}

/// Transport settings for the HTTP calls SpanBay makes on its own behalf (the OAuth 2 token
/// fetch, `pm.sendRequest`), taken from the global network settings.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AuxiliaryNetwork {
    /// Per-call timeout.
    pub timeout: Duration,
    /// `network.proxy_url` as stored: a URL, `direct`, or empty for the system proxy.
    pub proxy_url: String,
    /// Hosts that skip the global proxy (`network.no_proxy`).
    pub no_proxy: Vec<String>,
    /// Extra trusted roots (`network.ca_bundle_path`); empty for none.
    pub ca_bundle_path: String,
    /// Verify TLS certificates.
    pub verify_tls: bool,
    /// The workspace certificates and their key passphrases: the CA certificates are trusted
    /// besides `ca_bundle_path`, and a matching client certificate is presented.
    pub tls: super::tls::TlsMaterial,
}

impl Default for AuxiliaryNetwork {
    fn default() -> Self {
        Self {
            timeout: Duration::from_secs(30),
            proxy_url: String::new(),
            no_proxy: Vec::new(),
            ca_bundle_path: String::new(),
            verify_tls: true,
            tls: super::tls::TlsMaterial::default(),
        }
    }
}

impl AuxiliaryNetwork {
    /// The explicit global proxy URL, if any (not empty, not `direct`). Script and OAuth calls
    /// use only this one, never the system proxy.
    #[must_use]
    pub fn explicit_proxy(&self) -> Option<&str> {
        let url = self.proxy_url.trim();
        (!url.is_empty() && !url.eq_ignore_ascii_case(spanbay_base::proxy_url::DIRECT))
            .then_some(url)
    }

    /// These settings with the workspace TLS material.
    #[must_use]
    pub fn with_tls(mut self, tls: super::tls::TlsMaterial) -> Self {
        self.tls = tls;
        self
    }
}

impl From<&NetworkSettings> for AuxiliaryNetwork {
    fn from(network: &NetworkSettings) -> Self {
        Self {
            timeout: Duration::from_millis(network.timeout_ms),
            proxy_url: network.proxy_url.clone(),
            no_proxy: network.no_proxy.clone(),
            ca_bundle_path: network.ca_bundle_path.trim().to_owned(),
            verify_tls: network.verify_tls,
            tls: super::tls::TlsMaterial::default(),
        }
    }
}

impl ExecutorOptions {
    /// Fails with [`ExecutionError::Cancelled`] when the cancellation token has fired.
    pub(super) fn check_cancelled(&self) -> Result<(), ExecutionError> {
        if self.cancel.as_ref().is_some_and(CancelToken::is_cancelled) {
            Err(ExecutionError::cancelled())
        } else {
            Ok(())
        }
    }
}

/// Everything captured from one executed request: response, trace, timing and script output.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionResult {
    pub status_code: u16,
    pub status_text: String,
    pub url: String,
    pub elapsed_ms: u128,
    pub attempt_count: u32,
    pub headers: Vec<ExecutionHeader>,
    pub cookies: Vec<ExecutionCookie>,
    /// The body as text (lossy for non-UTF-8); only a preview when `body_handle` is set.
    pub body_text: String,
    pub body_size_bytes: usize,
    /// The captured bytes, for the shell-facing layer to store; never serialized.
    #[serde(skip)]
    pub raw_body: Option<bytes::Bytes>,
    /// Handle of the full body in [`spanbay_response::bodies::BodyStore`] when it was too large to return
    /// inline; `body_text` is then the first 64 KiB and `body_base64` is empty.
    #[serde(default)]
    pub body_handle: Option<String>,
    /// The view a shell should open by default, from the content type and a content sniff of the
    /// full body (JSON validity included).
    #[serde(default)]
    pub suggested_view: super::view::SuggestedView,
    /// The size cap cut the captured body: `body_size_bytes` is what was kept, not the full size.
    #[serde(default)]
    pub body_truncated: bool,
    /// The full body size when it differs from the captured size and is known: every byte
    /// written by a save-to-file send, or the declared `Content-Length` of an uncompressed body
    /// the cap cut. `None` when the body is whole or its full size is unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_total_bytes: Option<u64>,
    /// Set when the body stopped early (idle or total timeout, Stop); the status, headers and
    /// the bytes received so far are kept.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub body_incomplete: Option<BodyIncomplete>,
    /// The file a save-to-file send wrote the whole body to (display form).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub saved_body_path: Option<String>,
    /// Base64 of captured bytes when the body is not UTF-8 or is previewable as an image.
    #[serde(default)]
    pub body_base64: Option<String>,
    /// Whether the captured body can be saved losslessly from this result.
    #[serde(default)]
    pub raw_body_available: bool,
    /// What was actually put on the wire for the last hop, secrets masked.
    #[serde(default)]
    pub sent_request: Option<SentRequest>,
    #[serde(default)]
    pub assertion_results: Vec<spanbay_model::assertion::AssertionResult>,
    #[serde(default)]
    pub script_tests: Vec<spanbay_model::script_result::ScriptTestResult>,
    #[serde(default)]
    pub console: Vec<spanbay_model::script_result::ConsoleEntry>,
    #[serde(default)]
    pub script_error: Option<spanbay_model::script_result::ScriptError>,
    #[serde(default)]
    pub next_request: Option<String>,
    #[serde(default)]
    pub stop_run: bool,
    /// How long the request and the response body each took.
    #[serde(default)]
    pub timing: spanbay_model::models::PhaseTiming,
    /// Every variable, auth decision and transport detail behind this exchange.
    #[serde(default)]
    pub call_chain: spanbay_model::models::CallChainTrace,
    /// Id of the history row this execution wrote; `None` when it wrote none.
    #[serde(default)]
    pub history_id: Option<String>,
    /// Whether that row was written: `false` when the send records no history, its workspace
    /// is gone or the write failed.
    #[serde(default)]
    pub history_saved: bool,
    /// Model, finish reason and cost when an AI request ran through the AI run service.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai: Option<spanbay_model::models::AiStepSummary>,
    /// What became of the `Set-Cookie` headers: the ones not kept, and whether the kept ones
    /// reached the workspace jar.
    #[serde(default)]
    pub cookie_report: CookieReport,
}

/// What became of the cookies a response set, for the Cookies tab.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CookieReport {
    /// Whether the kept cookies (`ExecutionResult::cookies`) were written to the jar.
    pub jar: CookieJarOutcome,
    /// `Set-Cookie` headers that were not kept, in the order received.
    pub rejected: Vec<RejectedCookie>,
}

/// Whether a response's cookies were written to the workspace jar.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CookieJarOutcome {
    /// Not written: the send does not store cookies (the request's jar is off, a Flow step, a
    /// send without history) or nothing was kept.
    #[default]
    NotStored,
    /// Written to the workspace jar.
    Stored,
    /// The write failed; the cookies applied to this exchange (its redirects) only.
    Failed,
}

/// One `Set-Cookie` header that was not kept.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RejectedCookie {
    /// The cookie name, or empty when the header had none to read.
    pub name: String,
    /// The host of the hop that sent it.
    pub host: String,
    /// Why it was not kept.
    pub reason: CookieRejectReason,
}

/// Why a `Set-Cookie` header was not kept.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CookieRejectReason {
    /// The header is not UTF-8 or has no `name=value` pair.
    Malformed,
    /// Its `Domain` does not cover the host that sent it.
    ForeignDomain,
    /// Its `Domain` is a public suffix (`com`, `co.uk`) other than the host itself.
    PublicSuffix,
    /// Name, value, domain and path exceed the 4096-byte cap (RFC 6265 §6.1); not stored.
    TooLarge,
}

/// One header as sent or received.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionHeader {
    pub key: String,
    pub value: String,
}

/// The request actually sent on the last hop, with secret values masked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SentRequest {
    pub method: String,
    pub url: String,
    pub headers: Vec<ExecutionHeader>,
    pub body_text: Option<String>,
    pub body_size_bytes: usize,
    pub body_truncated: bool,
    /// Names of the headers in `headers` the executor generated rather than the editor's
    /// enabled rows: the body's Content-Type, auth and jar cookies (the "auto" row).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub implicit_headers: Vec<String>,
}

/// A cookie captured from a `Set-Cookie` response header.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecutionCookie {
    pub name: String,
    pub value: String,
    pub domain: Option<String>,
    pub path: Option<String>,
    pub expires: Option<String>,
    pub max_age: Option<String>,
    pub secure: bool,
    pub http_only: bool,
    pub same_site: Option<String>,
    #[serde(default)]
    pub host_only: bool,
}

/// Why a request could not be executed or completed.
#[derive(Debug, Error)]
pub enum ExecutionError {
    #[error(
        "WebSocket and gRPC requests are not supported yet; only HTTP REST and GraphQL can be sent"
    )]
    UnsupportedProtocol,
    #[error("only GraphQL POST execution is supported")]
    UnsupportedGraphqlMethod,
    #[error(
        "only JSON, Raw Text, Form URL Encoded, Multipart Form Data, Binary, and GraphQL bodies are supported for execution"
    )]
    UnsupportedBody,
    /// Every unresolved name of the request, in the order found; the message names the first.
    #[error("unresolved variable {}", keys.first().map_or("unknown", String::as_str))]
    UnresolvedVariable { keys: Vec<String> },
    #[error(
        "a redirect to {host} was refused: the request carries a secret and {host} is not a host this environment declares"
    )]
    RedirectHostNotDeclared { host: String },
    #[error("refusing to connect to {host}: the address is not one this request may reach")]
    GuardRefused { host: String },
    #[error("invalid request URL: {0}")]
    InvalidUrl(String),
    #[error("failed to build HTTP client: {0}")]
    ClientBuild(String),
    #[error("network request failed: {message}")]
    Network {
        message: String,
        attempt_count: u32,
        /// Whether the failure happened while establishing the connection.
        connect_phase: bool,
        /// Why the transport failed, read from the client error's type.
        cause: Option<NetworkCause>,
    },
    #[error("failed to read response body: {0}")]
    BodyRead(String),
    /// Writing the body of a save-to-file send failed (the destination is gone, full or
    /// not writable).
    #[error("failed to write the response body to {path}: {message}")]
    BodyWrite { path: String, message: String },
    #[error("invalid request header {key}: {message}")]
    InvalidHeader { key: String, message: String },
    #[error("failed to read multipart file {path}: {message}")]
    MultipartFileRead {
        path: String,
        message: String,
        /// The file or body is over the request body size limit (`RequestTooLarge`).
        too_large: bool,
    },
    #[error("invalid GraphQL variables JSON: {0}")]
    InvalidGraphqlVariablesJson(String),
    #[error("failed to serialize GraphQL request body: {0}")]
    GraphqlBodySerialize(String),
    #[error("OAuth 2.0 token request failed: {message}")]
    OAuth2Token { message: String },
    #[error("Request cancelled")]
    Cancelled {
        /// Response body bytes (after content decoding) read before the abort; 0 when it came
        /// before any body was read.
        bytes_received: u64,
    },
}

impl ExecutionError {
    #[must_use]
    /// Number of attempts made when this error was produced (always at least 1).
    pub fn attempt_count(&self) -> u32 {
        match self {
            Self::Network { attempt_count, .. } => (*attempt_count).max(1),
            _ => 1,
        }
    }

    /// A cancellation that interrupted nothing but waiting: no body bytes were received.
    #[must_use]
    pub(crate) const fn cancelled() -> Self {
        Self::Cancelled { bytes_received: 0 }
    }

    /// Whether the request was aborted through its cancel token.
    #[must_use]
    pub fn is_cancelled(&self) -> bool {
        matches!(self, Self::Cancelled { .. })
    }

    /// Stamps a network error with the number of attempts made.
    pub(super) fn with_attempt_count(self, attempt_count: u32) -> Self {
        match self {
            Self::Network {
                message,
                connect_phase,
                cause,
                ..
            } => Self::Network {
                message,
                attempt_count: attempt_count.max(1),
                connect_phase,
                cause,
            },
            error => error,
        }
    }
}

/// Truncates `value` to at most `limit` bytes on a UTF-8 boundary, appending an ellipsis.
pub(super) fn truncate_message(value: &str, limit: usize) -> String {
    if value.len() <= limit {
        value.to_owned()
    } else {
        let mut end = limit;
        while !value.is_char_boundary(end) {
            end -= 1;
        }
        format!("{}…", &value[..end])
    }
}
