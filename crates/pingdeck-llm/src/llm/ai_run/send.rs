//! Opening the response of one AI call: the provider path (the runtime's reqwest client, no
//! redirects) and the custom path (the guarded streaming transport).
//!
//! Single responsibility: send the planned request and return the response head with a chunk
//! stream, or say how the send failed, in the vocabulary of the result (`Cannot reach <host>:
//! <reason>`, `Timed out`, `Blocked address`). Both paths use a fresh connection for the call as
//! far as the stack allows and measure what the connection facts allow.

use std::time::Instant;

use super::types::{AiRunSpec, AiRunStatus, Destination};
use super::wire::Planned;
use crate::llm::capture::PhaseValue;
use crate::llm::failure::FailureKind;
use crate::llm::runtime::LlmRuntime;
use crate::llm::safe_http::client::{map_error, stream_of};
use crate::llm::safe_http::stream::{SafeStream, StreamLimits};
use crate::llm::safe_http::{SafeMethod, SafeRequest, SafeTransportError};
use crate::llm::upstream::{ConnFacts, ceil_ms};
use pingdeck_base::cancel::{CancelToken, until_cancelled};

/// Longest wait for a TCP connection plus TLS handshake.
const CONNECT_LIMIT: std::time::Duration = std::time::Duration::from_secs(15);

/// A send that got a response head.
pub(super) struct Opened {
    pub stream: SafeStream,
    pub conn: ConnFacts,
    /// Milliseconds from the send to the response head.
    pub headers_ms: u64,
}

/// A send that did not.
pub(super) struct SendFailure {
    pub status: AiRunStatus,
    pub message: String,
    pub kind: FailureKind,
    pub conn: ConnFacts,
}

/// The limits of a call: a stream is limited by silence, anything else by its whole duration.
pub(super) fn limits_of(spec: &AiRunSpec) -> StreamLimits {
    if spec.stream {
        StreamLimits::stream(spec.timeout)
    } else {
        StreamLimits::whole(spec.timeout)
    }
}

/// The host of `url` as messages name it.
pub(super) fn host_of(url: &str) -> String {
    url::Url::parse(url)
        .ok()
        .and_then(|parsed| parsed.host_str().map(str::to_owned))
        .unwrap_or_else(|| "the host".to_owned())
}

/// Sends the request. `sent_at` is stamped just before the bytes leave.
pub(super) async fn open(
    runtime: &LlmRuntime,
    spec: &AiRunSpec,
    planned: &Planned,
    body: Vec<u8>,
    cancel: &CancelToken,
    sent_at: &mut Instant,
) -> Result<Opened, SendFailure> {
    match &spec.destination {
        Destination::Provider(target) => {
            let client = runtime
                .http_for(&planned.url, &target.proxy_url)
                .map_err(|error| SendFailure {
                    status: AiRunStatus::Unsupported,
                    message: error.message().to_owned(),
                    kind: FailureKind::Unknown,
                    conn: ConnFacts::UNKNOWN,
                })?;
            let proxied = runtime.proxied_for(&planned.url, &target.proxy_url);
            let mut request = client.post(&planned.url).body(body);
            for (name, value) in &planned.headers {
                request = request.header(name, value);
            }
            let parsed = url::Url::parse(&planned.url).ok();
            let watch = runtime
                .conn_ledger()
                .watch(parsed.as_ref().and_then(url::Url::host));
            let limits = limits_of(spec);
            *sent_at = Instant::now();
            let sending = async {
                tokio::time::timeout(limits.first_byte, request.send())
                    .await
                    .map_err(|_| SafeTransportError::Timeout)?
                    .map_err(|error| map_error(&error, proxied))
            };
            let response = match until_cancelled(Some(cancel), sending).await {
                None => Err(SafeTransportError::Cancelled),
                Some(result) => result,
            };
            let headers_ms = ceil_ms(sent_at.elapsed());
            match response {
                Ok(response) => {
                    let connection = response
                        .extensions()
                        .get::<hyper_util::client::legacy::connect::HttpInfo>()
                        .map(|info| (info.local_addr(), info.remote_addr()));
                    let conn = ConnFacts::answered(&watch, connection);
                    let stream = stream_of(response, None, limits, *sent_at, cancel.clone());
                    Ok(Opened {
                        stream,
                        conn,
                        headers_ms,
                    })
                }
                Err(error) => Err(failure(
                    &error,
                    &planned.url,
                    spec,
                    ConnFacts::unanswered(&watch),
                )),
            }
        }
        Destination::Custom { http, .. } => {
            let request = SafeRequest {
                method: SafeMethod::Post,
                url: planned.url.clone(),
                headers: planned.headers.clone(),
                body: Some(body),
                timeout: spec.timeout,
                connect_timeout: spec.timeout.min(CONNECT_LIMIT),
                max_body: 0,
                // The user typed this address: loopback and private ones are theirs to call.
                allow_private: true,
            };
            *sent_at = Instant::now();
            match http.open_stream(request, limits_of(spec), cancel).await {
                Ok(stream) => {
                    let dns = match stream.dns {
                        Some(took) => PhaseValue::Ms(ceil_ms(took)),
                        None if http.uses_proxy() => PhaseValue::NotMeasured,
                        None => PhaseValue::NotNeeded,
                    };
                    let connect = stream.connect.map_or(PhaseValue::NotMeasured, |took| {
                        PhaseValue::Ms(ceil_ms(took).max(1))
                    });
                    Ok(Opened {
                        headers_ms: ceil_ms(sent_at.elapsed()),
                        conn: ConnFacts {
                            dns,
                            connect,
                            reused: Some(false),
                        },
                        stream,
                    })
                }
                Err(error) => Err(failure(&error, &planned.url, spec, ConnFacts::UNKNOWN)),
            }
        }
    }
}

/// The shaped failure of a transport error before any response head.
pub(super) fn failure(
    error: &SafeTransportError,
    url: &str,
    spec: &AiRunSpec,
    conn: ConnFacts,
) -> SendFailure {
    let host = host_of(url);
    let (status, message, kind) = match error {
        SafeTransportError::Blocked { reason } => (
            AiRunStatus::Unsupported,
            format!(
                "Blocked address ({reason}): link-local and metadata addresses are never called (blocked_address)"
            ),
            FailureKind::Network,
        ),
        SafeTransportError::Timeout => (
            AiRunStatus::TimedOut,
            format!(
                "Timed out: no response from {host} within {} s",
                spec.timeout.as_secs_f64()
            ),
            FailureKind::Timeout,
        ),
        SafeTransportError::Cancelled => (
            AiRunStatus::Stopped,
            "Stopped before the server answered".to_owned(),
            FailureKind::Cancelled,
        ),
        other => (
            AiRunStatus::Unreachable,
            format!("Cannot reach {host}: {}", unreachable_reason(other)),
            FailureKind::Network,
        ),
    };
    SendFailure {
        status,
        message,
        kind,
        conn,
    }
}

/// The reason and, when known, the phase of an unreachable host.
pub(super) fn unreachable_reason(error: &SafeTransportError) -> String {
    match error {
        SafeTransportError::Dns => "the name could not be resolved (DNS)".to_owned(),
        SafeTransportError::Refused => "the connection was refused (connect)".to_owned(),
        SafeTransportError::Tls => "the TLS handshake failed (TLS)".to_owned(),
        SafeTransportError::Reset => "the connection was closed or reset".to_owned(),
        SafeTransportError::ProxyAuth => "the proxy asked for credentials (407)".to_owned(),
        SafeTransportError::ProxyRefused => "the proxy refused the tunnel (CONNECT)".to_owned(),
        SafeTransportError::ProxyUnreachable => "the proxy could not be reached".to_owned(),
        SafeTransportError::Other(text) => text.clone(),
        other => other.kind().to_owned(),
    }
}
