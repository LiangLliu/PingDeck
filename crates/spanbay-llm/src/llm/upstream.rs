//! One HTTP attempt against an upstream, and reading its stream.
//!
//! Single responsibility: send a prepared body to a [`Target`] and classify what comes back. No
//! routing, no failover, no conversion: callers (the gateway, the chat client, the probes) decide
//! what an [`AttemptError`] means (see `failover`). Secrets only ever go into request headers,
//! never into error text. Every attempt also yields an [`UpstreamMeta`]: time to headers, the
//! upstream's request id, the response headers worth capturing, and the connection facts of Spike
//! S1 (see `timing`).

use std::time::{Duration, Instant};

use bytes::Bytes;
use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde_json::Value;

use crate::llm::adapters::{StreamDecoder, codec};
use crate::llm::capture::PhaseValue;
use crate::llm::capture_record::AttemptTiming;
use crate::llm::failover::parse_retry_after;
use crate::llm::failure::{FailureKind, is_retryable_status};
use crate::llm::hub::HubEvent;
use crate::llm::router::Target;
use crate::llm::runtime::LlmRuntime;
use crate::llm::sse::{SseEvent, SseParser};
use crate::llm::wire::{Protocol, request_headers};
use spanbay_base::cancel::{CancelToken, until_cancelled};
use spanbay_base::error::AppError;
use spanbay_llm_ops::llm::timing;

pub use timing::{AttemptWatch, ConnFacts, ConnLedger, ConnectTimer, TimedResolver, ceil_ms};

/// Largest response body read into memory (32 MiB).
pub const MAX_BODY_BYTES: usize = 32 * 1024 * 1024;

/// Longest error body kept (64 KiB).
const MAX_ERROR_BYTES: usize = 64 * 1024;

/// The `kind` of the [`AttemptError::Stream`] of a stream that ended before any content.
const EMPTY_STREAM: &str = "empty_stream";
/// The `kind` of [`AttemptError::Stream`] for a stream that closed cleanly without its end
/// marker: the answer is incomplete.
pub const TRUNCATED_STREAM: &str = "truncated";

/// Most chunks [`UpstreamStream::prime`] keeps while it waits for the first content event.
const MAX_PRIME_CHUNKS: usize = 2048;

/// Most bytes [`UpstreamStream::prime`] keeps while it waits for the first content event (2 MiB).
const MAX_PRIME_BYTES: usize = 2 * 1024 * 1024;

/// Why one attempt failed.
#[derive(Debug, Clone)]
pub enum AttemptError {
    /// The upstream answered with a non-2xx status.
    Status {
        /// HTTP status.
        status: u16,
        /// Response body (truncated).
        body: String,
        /// The upstream's `Retry-After` (`retry-after-ms`, seconds or HTTP date), in milliseconds.
        retry_after_ms: Option<u64>,
    },
    /// The connection or transfer failed.
    Network {
        /// The error chain, without the URL.
        message: String,
        /// The transport cause, when the chain names one.
        cause: Option<spanbay_base::error::NetworkCause>,
    },
    /// A timeout elapsed.
    Timeout,
    /// A 2xx answer to a streaming request that is not an event stream.
    NotSse {
        /// HTTP status.
        status: u16,
        /// The content type received.
        content_type: String,
        /// Start of the body.
        preview: String,
    },
    /// The stream carried an error event before any content.
    Stream {
        /// Provider message.
        message: String,
        /// Provider error type.
        kind: Option<String>,
    },
    /// The operation was cancelled.
    Cancelled,
    /// A 2xx answer whose body the upstream got wrong: not JSON, over the size cap, a stream
    /// frame the decoder cannot read, or a stream with no content in its first
    /// [`MAX_PRIME_BYTES`]. Another candidate may answer, so it is retryable and counts against
    /// the upstream's health.
    BadBody(String),
    /// The request could not be built, or the local side failed: never the upstream's fault.
    Invalid(AppError),
}

impl AttemptError {
    /// Whether another candidate should be tried: timeouts, 408/429/5xx, network errors, a 2xx
    /// that is not a stream or has an unreadable body, and in-stream errors. Client errors and
    /// local faults are final.
    #[must_use]
    pub fn retryable(&self) -> bool {
        match self {
            Self::Status { status, .. } => is_retryable_status(*status),
            Self::Network { .. }
            | Self::Timeout
            | Self::NotSse { .. }
            | Self::Stream { .. }
            | Self::BadBody(_) => true,
            Self::Cancelled | Self::Invalid(_) => false,
        }
    }

    /// Whether the upstream rejected the credential or the account (401, 402, 403). That says
    /// nothing about the model or the provider as a whole, so a request moves on to the next key
    /// (see `failover::Failure::verdict`) and is final when none is left.
    #[must_use]
    pub fn auth_rejected(&self) -> bool {
        matches!(self, Self::Status { status, .. } if (401..=403).contains(status))
    }

    /// Whether the upstream stream ended without a single content event (an empty answer).
    #[must_use]
    pub fn is_empty_stream(&self) -> bool {
        matches!(self, Self::Stream { kind: Some(kind), .. } if kind == EMPTY_STREAM)
    }

    /// How long the upstream asked callers to wait (`Retry-After`), if it did.
    #[must_use]
    pub fn retry_after(&self) -> Option<Duration> {
        match self {
            Self::Status { retry_after_ms, .. } => retry_after_ms.map(Duration::from_millis),
            _ => None,
        }
    }

    /// The HTTP status the upstream sent, if any.
    #[must_use]
    pub fn status(&self) -> Option<u16> {
        match self {
            Self::Status { status, .. } | Self::NotSse { status, .. } => Some(*status),
            _ => None,
        }
    }

    /// A one-line description for logs and error bodies.
    #[must_use]
    pub fn message(&self) -> String {
        match self {
            Self::Status { status, .. } if (300..400).contains(status) => format!(
                "upstream answered HTTP {status}, a redirect; redirects are not followed, so check the provider's base URL"
            ),
            Self::Status { status, body, .. } => format!(
                "upstream returned HTTP {status}: {}",
                provider_message(body)
            ),
            Self::Network { message, .. } => format!("network error: {message}"),
            Self::Timeout => "the upstream timed out".to_owned(),
            Self::NotSse {
                status,
                content_type,
                ..
            } => {
                format!(
                    "upstream answered HTTP {status} with {content_type} instead of an event stream"
                )
            }
            Self::Stream { message, .. } => format!("upstream stream error: {message}"),
            Self::BadBody(message) => message.clone(),
            Self::Cancelled => "cancelled".to_owned(),
            Self::Invalid(error) => error.message().to_owned(),
        }
    }

    /// The failure category (for probes and logs): the kind failover judged the attempt with
    /// (`assess` over [`crate::llm::failover::observe`]), so the log's `failure_kind` is the final
    /// attempt's judgement.
    #[must_use]
    pub fn kind(&self) -> FailureKind {
        match self {
            Self::Cancelled => FailureKind::Cancelled,
            Self::Invalid(_) => FailureKind::ProtocolMismatch,
            other => crate::llm::failover::observe(other)
                .map_or(FailureKind::Unknown, |observed| {
                    crate::llm::failover::assess(&observed).kind
                }),
        }
    }

    /// Converts into the use-case error type.
    #[must_use]
    pub fn into_app_error(self) -> AppError {
        match self {
            Self::Cancelled => AppError::cancelled(),
            Self::Invalid(error) => error,
            other => AppError::network(other.message()),
        }
    }
}

/// A run of token characters long enough to be a piece of a key.
static TOKEN_RUN: std::sync::LazyLock<Option<regex::Regex>> =
    std::sync::LazyLock::new(|| regex::Regex::new(r"[A-Za-z0-9_-]{8,}").ok());

/// An upstream error body with the credentials the attempt sent masked: some providers
/// echo the key back, whole or as a fragment (`sk-abc12345…`), and the body reaches run records,
/// messages and exports.
fn redact_echo(body: &str, target: &Target) -> String {
    use spanbay_base::secrets::{REDACTED, is_secret_header_name, redact_secrets};
    let mut needles = vec![target.secret.trim().to_owned()];
    needles.extend(
        target
            .quirks
            .extra_headers
            .iter()
            .filter(|(name, _)| is_secret_header_name(name))
            .map(|(_, value)| value.trim().to_owned()),
    );
    let masked = redact_secrets(body, &needles);
    let Some(runs) = TOKEN_RUN.as_ref() else {
        return masked;
    };
    runs.replace_all(&masked, |found: &regex::Captures<'_>| {
        let run = &found[0];
        if needles.iter().any(|needle| needle.contains(run)) {
            REDACTED.to_owned()
        } else {
            run.to_owned()
        }
    })
    .into_owned()
}

/// Extracts the human message from a provider error body (JSON `error.message`, `message`,
/// `detail`) or returns the trimmed text.
#[must_use]
pub fn provider_message(body: &str) -> String {
    let text = body.trim();
    if let Ok(json) = serde_json::from_str::<Value>(text) {
        let pick = |value: &Value| value.as_str().map(str::to_owned);
        let found = json
            .pointer("/error/message")
            .and_then(pick)
            .or_else(|| json.get("message").and_then(pick))
            .or_else(|| json.get("detail").and_then(pick))
            .or_else(|| json.get("error").and_then(pick))
            .or_else(|| json.pointer("/error/error/message").and_then(pick));
        if let Some(message) = found {
            return message;
        }
    }
    text.chars().take(500).collect()
}

/// Everything one attempt needs.
pub struct Call<'a> {
    /// Shared runtime (HTTP clients).
    pub runtime: &'a LlmRuntime,
    /// Where to send it.
    pub target: &'a Target,
    /// Protocol of `body`.
    pub protocol: Protocol,
    /// Whether the response is a stream.
    pub stream: bool,
    /// Explicit URL instead of the one derived from the target and protocol (Gemini embeddings).
    pub url: Option<&'a str>,
    /// The JSON body (for GET-less protocols; always POST).
    pub body: &'a Value,
    /// Whole-request timeout for non-streaming calls; for streams, time to response headers.
    pub timeout: Duration,
    /// Cancellation.
    pub cancel: Option<&'a CancelToken>,
}

/// Maps a transport error; the URL (which may carry path segments or tokens) is left out.
/// `proxied`: the request dialled an explicit proxy.
pub(crate) fn map_reqwest(error: reqwest::Error, proxied: bool) -> AttemptError {
    if error.is_timeout() {
        return AttemptError::Timeout;
    }
    let cause = spanbay_base::error::transport_cause(&error, proxied);
    // The causes say whether it was DNS, a refused connection or the local proxy.
    let error = error.without_url();
    let mut message = error.to_string();
    let mut source = std::error::Error::source(&error);
    while let Some(cause) = source {
        let cause_text = cause.to_string();
        if !message.contains(&cause_text) {
            message.push_str(": ");
            message.push_str(&cause_text);
        }
        source = cause.source();
    }
    AttemptError::Network { message, cause }
}

/// What an attempt learned besides its answer (filled as far as the attempt got).
#[derive(Debug, Clone)]
pub struct UpstreamMeta {
    started: Instant,
    /// HTTP status of the answer, once there was one.
    pub status: Option<u16>,
    /// Milliseconds from `send` to the response headers; `None` when no headers arrived.
    pub headers_ms: Option<u64>,
    /// Connection facts: DNS time and pooled-connection reuse.
    pub conn: ConnFacts,
    /// The id the upstream gave the request (`x-request-id`, `request-id`...), for support.
    pub upstream_request_id: Option<String>,
    /// The response headers worth keeping in a capture, in arrival order (the capture redacts).
    pub response_headers: Vec<(String, String)>,
}

/// Response headers kept for captures: exact names, then prefixes.
const CAPTURED_HEADERS: [&str; 8] = [
    "content-type",
    "retry-after",
    "retry-after-ms",
    "request-id",
    "server",
    "date",
    "via",
    "cf-ray",
];
const CAPTURED_PREFIXES: [&str; 6] = [
    "x-request-id",
    "x-ratelimit-",
    "ratelimit-",
    "anthropic-",
    "openai-",
    "x-goog-",
];

/// Headers that carry the upstream's own id of the request, best first.
const REQUEST_ID_HEADERS: [&str; 5] = [
    "x-request-id",
    "request-id",
    "openai-request-id",
    "x-amzn-requestid",
    "x-amzn-request-id",
];

/// Longest request id kept.
const MAX_REQUEST_ID: usize = 128;

impl UpstreamMeta {
    /// The meta of an attempt that never reached the network (nothing known yet).
    #[must_use]
    pub fn unsent() -> Self {
        Self {
            started: Instant::now(),
            status: None,
            headers_ms: None,
            conn: ConnFacts::UNKNOWN,
            upstream_request_id: None,
            response_headers: Vec::new(),
        }
    }

    /// When the attempt's `send` began.
    #[must_use]
    pub fn started(&self) -> Instant {
        self.started
    }

    /// The attempt's timing: `first_token_ms` and `end_ms` are the caller's (the time
    /// to the first content event, and from `send` to the last byte or the failure).
    #[must_use]
    pub fn timing(&self, first_token_ms: PhaseValue, end_ms: u64) -> AttemptTiming {
        self.conn.timing(self.headers_ms, first_token_ms, end_ms)
    }

    fn record_headers(&mut self, headers: &reqwest::header::HeaderMap) {
        self.upstream_request_id = REQUEST_ID_HEADERS.iter().find_map(|name| {
            let value = headers.get(*name)?.to_str().ok()?.trim();
            (!value.is_empty() && value.chars().all(|c| c.is_ascii_graphic()))
                .then(|| value.chars().take(MAX_REQUEST_ID).collect())
        });
        self.response_headers = headers
            .iter()
            .filter(|(name, _)| {
                let name = name.as_str();
                CAPTURED_HEADERS.contains(&name)
                    || CAPTURED_PREFIXES
                        .iter()
                        .any(|prefix| name.starts_with(prefix))
            })
            .filter_map(|(name, value)| {
                Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
            })
            .collect();
    }
}

/// The upstream's requested wait in milliseconds: `retry-after-ms` (OpenAI), else `Retry-After` as
/// delay seconds or an HTTP date.
#[must_use]
pub fn retry_after_ms(headers: &reqwest::header::HeaderMap) -> Option<u64> {
    let text = |name: &str| headers.get(name)?.to_str().ok().map(str::trim);
    text("retry-after-ms")
        .and_then(|value| value.parse::<f64>().ok())
        .filter(|ms| ms.is_finite() && *ms >= 0.0)
        .map(|ms| ms.min(3.0e14) as u64)
        .or_else(|| parse_retry_after(text("retry-after")?, std::time::SystemTime::now()))
}

/// POSTs the body and returns the 2xx response, still unread, and what the attempt learned, on
/// success and on failure alike.
///
/// # Errors
/// [`AttemptError`] (first element) for transport failures, non-2xx statuses and (for streams) a
/// non-SSE answer.
pub async fn send_measured(
    call: &Call<'_>,
) -> (Result<reqwest::Response, AttemptError>, UpstreamMeta) {
    match serde_json::to_vec(call.body) {
        Ok(body) => send_measured_bytes(call, Bytes::from(body)).await,
        Err(error) => (
            Err(AttemptError::Invalid(AppError::from(error))),
            UpstreamMeta::unsent(),
        ),
    }
}

/// [`send_measured`] with the exact bytes to POST (`call.body` is not read): the gateway forwards a
/// same-protocol client request byte for byte, which a re-serialised JSON value would not.
pub async fn send_measured_bytes(
    call: &Call<'_>,
    body: Bytes,
) -> (Result<reqwest::Response, AttemptError>, UpstreamMeta) {
    let mut meta = UpstreamMeta::unsent();
    let result = dispatch(call, body, &mut meta).await;
    (result, meta)
}

async fn dispatch(
    call: &Call<'_>,
    body: Bytes,
    meta: &mut UpstreamMeta,
) -> Result<reqwest::Response, AttemptError> {
    let url = match call.url {
        Some(url) => url.to_owned(),
        None => call
            .target
            .url(call.protocol, call.stream)
            .map_err(AttemptError::Invalid)?,
    };
    let style = call.target.auth_for(call.protocol);
    let client = call
        .runtime
        .http_for(&url, &call.target.proxy_url)
        .map_err(AttemptError::Invalid)?;
    let mut request = client
        .post(&url)
        .header("content-type", "application/json")
        .header(
            "accept",
            if call.stream {
                "text/event-stream"
            } else {
                "application/json"
            },
        )
        .body(body);
    for (name, value) in request_headers(
        call.protocol,
        style,
        &call.target.secret,
        &call.target.quirks,
    ) {
        request = request.header(name, value);
    }
    if !call.stream {
        request = request.timeout(call.timeout);
    }
    let parsed = url::Url::parse(&url).ok();
    let watch = call
        .runtime
        .conn_ledger()
        .watch(parsed.as_ref().and_then(url::Url::host));
    meta.started = watch.started();
    let proxied = call.runtime.proxied_for(&url, &call.target.proxy_url);
    let sending = async {
        match tokio::time::timeout(call.timeout, request.send()).await {
            Ok(Ok(response)) => Ok(response),
            Ok(Err(error)) => Err(map_reqwest(error, proxied)),
            Err(_) => Err(AttemptError::Timeout),
        }
    };
    let sent = until_cancelled(call.cancel, sending)
        .await
        .ok_or(AttemptError::Cancelled)
        .and_then(|result| result);
    let response = match sent {
        Ok(response) => response,
        Err(error) => {
            meta.conn = ConnFacts::unanswered(&watch);
            return Err(error);
        }
    };
    // Rounded up: a measured headers time is at least the DNS time inside it.
    meta.headers_ms = Some(timing::ceil_ms(meta.started.elapsed()));
    meta.status = Some(response.status().as_u16());
    meta.record_headers(response.headers());
    let connection = response
        .extensions()
        .get::<hyper_util::client::legacy::connect::HttpInfo>()
        .map(|info| (info.local_addr(), info.remote_addr()));
    meta.conn = ConnFacts::answered(&watch, connection);
    let status = response.status().as_u16();
    if !response.status().is_success() {
        let retry_after_ms = retry_after_ms(response.headers());
        let body = read_text(response, MAX_ERROR_BYTES, call.cancel).await;
        return Err(AttemptError::Status {
            status,
            body: redact_echo(&body, call.target),
            retry_after_ms,
        });
    }
    if call.stream {
        let content_type = response
            .headers()
            .get("content-type")
            .and_then(|value| value.to_str().ok())
            .unwrap_or("")
            .to_ascii_lowercase();
        if !content_type.contains("text/event-stream") {
            let preview = read_text(response, 2048, call.cancel).await;
            return Err(AttemptError::NotSse {
                status,
                content_type,
                preview,
            });
        }
    }
    Ok(response)
}

/// Reads at most `limit` bytes of a body as lossy UTF-8 (errors become the empty string).
async fn read_text(
    response: reqwest::Response,
    limit: usize,
    cancel: Option<&CancelToken>,
) -> String {
    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    let reading = async {
        while let Some(Ok(chunk)) = stream.next().await {
            buffer.extend_from_slice(&chunk);
            if buffer.len() >= limit {
                break;
            }
        }
    };
    let _ = until_cancelled(cancel, reading).await;
    buffer.truncate(limit);
    String::from_utf8_lossy(&buffer).into_owned()
}

/// Reads a whole response body (at most [`MAX_BODY_BYTES`]).
///
/// # Errors
/// [`AttemptError`] for transfer failures, cancellation or an oversized body.
pub async fn read_body(
    response: reqwest::Response,
    cancel: Option<&CancelToken>,
) -> Result<Bytes, AttemptError> {
    let mut stream = response.bytes_stream();
    let mut buffer: Vec<u8> = Vec::new();
    let reading = async {
        while let Some(chunk) = stream.next().await {
            let chunk = chunk.map_err(|error| map_reqwest(error, false))?;
            if buffer.len() + chunk.len() > MAX_BODY_BYTES {
                return Err(AttemptError::BadBody(
                    "the upstream response is larger than 32 MiB".to_owned(),
                ));
            }
            buffer.extend_from_slice(&chunk);
        }
        Ok(())
    };
    until_cancelled(cancel, reading)
        .await
        .ok_or(AttemptError::Cancelled)??;
    Ok(Bytes::from(buffer))
}

/// Parses the JSON of a response body read with [`read_body`].
///
/// # Errors
/// [`AttemptError::BadBody`] for text that is not JSON.
pub fn parse_json(body: &[u8]) -> Result<Value, AttemptError> {
    serde_json::from_slice(body).map_err(|error| {
        AttemptError::BadBody(format!("the upstream returned invalid JSON: {error}"))
    })
}

/// Reads a whole JSON response body (at most [`MAX_BODY_BYTES`]).
///
/// # Errors
/// [`AttemptError`] for transfer failures, an oversized body or invalid JSON.
pub async fn read_json(
    response: reqwest::Response,
    cancel: Option<&CancelToken>,
) -> Result<Value, AttemptError> {
    parse_json(&read_body(response, cancel).await?)
}

/// One network chunk of a stream: the raw bytes plus the hub events decoded from them.
#[derive(Debug, Clone, Default)]
pub struct Chunk {
    /// Bytes exactly as received (empty for the closing chunk).
    pub raw: Bytes,
    /// Events decoded from the SSE frames this chunk completed.
    pub events: Vec<HubEvent>,
}

/// `data: [DONE]`: the end of an OpenAI-style stream. Decoders read it as no event, and some
/// compatible upstreams send it without any `finish_reason`.
fn is_done_marker(sse: &SseEvent) -> bool {
    sse.data.trim() == "[DONE]"
}

/// A streaming upstream response, decoded.
pub struct UpstreamStream {
    inner: BoxStream<'static, Result<Bytes, reqwest::Error>>,
    parser: SseParser,
    decoder: Box<dyn StreamDecoder>,
    idle: Duration,
    /// When the last SSE event arrived. The idle limit counts from it, so keep-alive comments
    /// (`: PROCESSING`) and other bytes that complete no event do not defeat it.
    progress: tokio::time::Instant,
    cancel: Option<CancelToken>,
    closed: bool,
    /// A decoder saw the protocol's own end of the answer (`[DONE]`, `message_stop`,
    /// `response.completed`, a finish reason).
    ended: bool,
    /// The error the read after the closing chunk returns (a truncated answer).
    pending: Option<AttemptError>,
    /// An event the decoder cannot read is skipped, not an error: a same-protocol pass-through
    /// forwards the upstream's bytes and only watches them.
    lenient: bool,
}

impl UpstreamStream {
    /// Wraps a successful streaming response of a chat `protocol`.
    ///
    /// # Errors
    /// [`AttemptError::Invalid`] when `protocol` is not a chat protocol.
    pub fn new(
        response: reqwest::Response,
        protocol: Protocol,
        idle: Duration,
        cancel: Option<CancelToken>,
    ) -> Result<Self, AttemptError> {
        Self::over(response.bytes_stream().boxed(), protocol, idle, cancel)
    }

    /// [`UpstreamStream::new`] over any byte stream (tests script one).
    fn over(
        inner: BoxStream<'static, Result<Bytes, reqwest::Error>>,
        protocol: Protocol,
        idle: Duration,
        cancel: Option<CancelToken>,
    ) -> Result<Self, AttemptError> {
        let codec = codec(protocol).ok_or_else(|| {
            AttemptError::Invalid(AppError::unsupported(format!(
                "{} cannot be streamed through the hub",
                protocol.as_str()
            )))
        })?;
        Ok(Self {
            inner,
            parser: SseParser::new(),
            decoder: codec.stream_decoder(),
            idle,
            progress: tokio::time::Instant::now(),
            cancel,
            closed: false,
            ended: false,
            pending: None,
            lenient: false,
        })
    }

    /// The same stream skipping events its decoder cannot read (see `lenient`).
    #[must_use]
    pub fn lenient(mut self) -> Self {
        self.lenient = true;
        self
    }

    /// The next chunk; `None` after the closing chunk. No SSE event for longer than the idle
    /// limit, however many keep-alive bytes arrive, is [`AttemptError::Timeout`].
    pub async fn next_chunk(&mut self) -> Option<Result<Chunk, AttemptError>> {
        self.read(self.idle.saturating_sub(self.progress.elapsed()))
            .await
    }

    /// The next chunk, waiting at most `wait` for it.
    async fn read(&mut self, wait: Duration) -> Option<Result<Chunk, AttemptError>> {
        if self.closed {
            return self.pending.take().map(Err);
        }
        let waiting = tokio::time::timeout(wait, self.inner.next());
        let next = match until_cancelled(self.cancel.as_ref(), waiting).await {
            None => return Some(Err(AttemptError::Cancelled)),
            Some(Err(_)) => return Some(Err(AttemptError::Timeout)),
            Some(Ok(next)) => next,
        };
        match next {
            Some(Ok(raw)) => {
                let mut events = Vec::new();
                let frames = self.parser.feed(&raw);
                if !frames.is_empty() {
                    self.progress = tokio::time::Instant::now();
                }
                for sse in frames {
                    self.ended |= is_done_marker(&sse);
                    match self.decoder.push(&sse) {
                        Ok(mut decoded) => events.append(&mut decoded),
                        Err(_) if self.lenient => {}
                        Err(error) => return Some(Err(bad_body(&error))),
                    }
                }
                self.ended |= events.iter().any(is_finish);
                Some(Ok(Chunk { raw, events }))
            }
            Some(Err(error)) => Some(Err(map_reqwest(error, false))),
            None => {
                self.closed = true;
                let mut events = Vec::new();
                for sse in self.parser.finish() {
                    self.ended |= is_done_marker(&sse);
                    match self.decoder.push(&sse) {
                        Ok(mut decoded) => events.append(&mut decoded),
                        Err(_) if self.lenient => {}
                        Err(error) => return Some(Err(bad_body(&error))),
                    }
                }
                self.ended |= events.iter().any(is_finish);
                match self.decoder.finish() {
                    Ok(mut decoded) => {
                        // A finish the decoder makes up at the close means the upstream never
                        // sent its end: the answer is incomplete, never a full one.
                        if !self.ended && decoded.iter().any(is_finish) {
                            decoded.retain(|event| !is_finish(event));
                            self.pending = Some(AttemptError::Stream {
                                message: "the upstream stream closed without its end marker; the answer is incomplete".to_owned(),
                                kind: Some(TRUNCATED_STREAM.to_owned()),
                            });
                        }
                        events.append(&mut decoded);
                    }
                    Err(error) => return Some(Err(bad_body(&error))),
                }
                Some(Ok(Chunk {
                    raw: Bytes::new(),
                    events,
                }))
            }
        }
    }

    /// Reads chunks until real content (or a finish or error) shows up, so the caller can fail
    /// over before it commits a response to its own client. Returns the chunks read, to replay.
    ///
    /// # Errors
    /// [`AttemptError::Stream`] when the first meaningful event is an error or the stream ends with
    /// nothing (see [`AttemptError::is_empty_stream`]), [`AttemptError::Timeout`] after `first_byte`,
    /// [`AttemptError::BadBody`] when a frame cannot be decoded or [`MAX_PRIME_CHUNKS`] chunks or
    /// [`MAX_PRIME_BYTES`] bytes pass without content (keep-alives, usage events), or any error of
    /// reading.
    pub async fn prime(&mut self, first_byte: Duration) -> Result<Vec<Chunk>, AttemptError> {
        let deadline = tokio::time::Instant::now() + first_byte;
        let mut buffered = Vec::new();
        let mut bytes = 0_usize;
        loop {
            // The first-byte limit governs here, not the (shorter) idle limit: a reasoning model
            // may stay silent for minutes before its first token.
            let left = deadline.saturating_duration_since(tokio::time::Instant::now());
            let next = self.read(left).await;
            let Some(chunk) = next else {
                return Err(AttemptError::Stream {
                    message: "the upstream stream ended with no content".to_owned(),
                    kind: Some(EMPTY_STREAM.to_owned()),
                });
            };
            let chunk = chunk?;
            let mut decided = false;
            for event in &chunk.events {
                match event {
                    HubEvent::Error { message, kind } => {
                        return Err(AttemptError::Stream {
                            message: message.clone(),
                            kind: kind.clone(),
                        });
                    }
                    // Bookkeeping, not content: the target is committed at the first content event.
                    HubEvent::Start { .. } | HubEvent::Origin { .. } | HubEvent::Usage { .. } => {}
                    _ => decided = true,
                }
            }
            bytes += chunk.raw.len();
            buffered.push(chunk);
            if decided {
                return Ok(buffered);
            }
            if buffered.len() >= MAX_PRIME_CHUNKS || bytes >= MAX_PRIME_BYTES {
                return Err(AttemptError::BadBody(format!(
                    "the upstream stream sent {} chunks ({bytes} bytes) without any content",
                    buffered.len()
                )));
            }
        }
    }
}

fn is_finish(event: &HubEvent) -> bool {
    matches!(event, HubEvent::Finish { .. })
}

/// An upstream answer the decoder cannot read is the upstream's fault, not ours.
pub(crate) fn bad_body(error: &AppError) -> AttemptError {
    AttemptError::BadBody(error.message().to_owned())
}

#[cfg(test)]
#[path = "upstream_tests.rs"]
mod tests;

#[cfg(test)]
#[path = "upstream_stream_tests.rs"]
mod stream_tests;
