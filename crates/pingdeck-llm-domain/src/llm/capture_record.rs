//! The stored shape of a model exchange capture: the redacted exchange the store persists and
//! `llm_log_get` returns, its headers, bodies, frames and translation notes, and the timing model.
//! Types and their small constructors only; no policy, no redaction.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::llm::wire::Protocol;
use pingdeck_model::phase::PhaseValue;

/// One header of a stored capture. `redacted` marks a value PingDeck removed before storage: it
/// cannot be shown or replayed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RedactedHeader {
    /// Header name as received (original case).
    pub name: String,
    /// Stored value, masked when `redacted`.
    pub value: String,
    /// Whether the value was changed by redaction.
    pub redacted: bool,
}

/// A stored body. `text` is `None` when no body was stored for this slot.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CapturedBody {
    /// The stored (redacted, possibly cut or elided) text.
    pub text: Option<String>,
    /// The body was cut at the 256 KiB cap.
    pub truncated: bool,
    /// At least one base64 payload was replaced by an elision marker.
    pub elided: bool,
    /// Size of the body as it crossed the wire.
    pub original_bytes: u64,
}

/// One stored SSE frame.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CapturedFrame {
    /// 1-based position in the stream.
    pub index: u32,
    /// Milliseconds since the first frame in log captures; for the frames of an API
    /// mode run (`AiResultDto.frames`, `AiRunEvent::Frames`, compare rows) milliseconds since
    /// the request was sent, so the first frame carries the time to first byte.
    pub t_ms: u64,
    /// Event name (`message` when the frame had none, as Chat streams do).
    pub event: String,
    /// The frame's data text (base64 payloads elided, redacted once stored).
    pub data: String,
}

/// The stored frames of one stream with the footer counts.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CapturedFrames {
    /// The first frames of the stream, up to the caps.
    pub frames: Vec<CapturedFrame>,
    /// Frames the stream carried ("Showing 2,000 of 6,413 frames").
    pub total_frames: u64,
    /// Not every frame is stored.
    pub truncated: bool,
}

/// How the client request was changed on its way upstream.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TranslationKind {
    /// A field was omitted (it is on the gateway's safe-drop list).
    Dropped,
    /// A field moved to a different name.
    Renamed,
    /// A field was added.
    Added,
    /// A field's value was changed.
    Changed,
}

/// One entry of the "Translation notes" list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TranslationNote {
    /// The field concerned (`top_k`).
    pub field: String,
    /// What happened to it.
    pub kind: TranslationKind,
    /// Why (`no equivalent in Chat Completions`).
    pub reason: String,
}

/// Timings of one upstream attempt. Every figure is a duration in milliseconds; the
/// attempt starts at its `send`. Connect and TLS are one segment (`connect_includes_tls`):
/// `tls` stays `NotMeasured` unless a stack can split it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AttemptTiming {
    /// Resolver time of the attempt's new connection.
    #[serde(rename = "dns_ms")]
    pub dns: PhaseValue,
    /// TCP connect (plus TLS when `connect_includes_tls`).
    #[serde(rename = "connect_ms")]
    pub connect: PhaseValue,
    /// TLS handshake as its own phase (`NotMeasured` unless a stack can split it from connect).
    #[serde(rename = "tls_ms")]
    pub tls: PhaseValue,
    /// `connect` covers the TLS handshake too.
    pub connect_includes_tls: bool,
    /// Connection setup end to response headers (TTFB).
    pub headers_ms: PhaseValue,
    /// Response headers to the first content event; streaming only.
    pub first_token_ms: PhaseValue,
    /// Duration of the whole attempt, `send` to the last byte (or to the failure).
    pub end_ms: PhaseValue,
    /// A pooled connection was used.
    pub reused: bool,
}

impl AttemptTiming {
    /// An attempt on a pooled connection: DNS, connect and TLS are `Reused`, never 0.
    #[must_use]
    pub fn on_reused_connection(headers_ms: u64, first_token_ms: PhaseValue, end_ms: u64) -> Self {
        Self {
            dns: PhaseValue::Reused,
            connect: PhaseValue::Reused,
            tls: PhaseValue::Reused,
            connect_includes_tls: false,
            headers_ms: PhaseValue::Ms(headers_ms),
            first_token_ms,
            end_ms: PhaseValue::Ms(end_ms),
            reused: true,
        }
    }

    /// Connection setup: the sum of the measured DNS, connect and TLS phases (reused, unneeded
    /// and unmeasured phases contribute nothing).
    #[must_use]
    pub fn setup_ms(&self) -> u64 {
        [self.dns, self.connect, self.tls]
            .into_iter()
            .filter_map(PhaseValue::ms)
            .sum()
    }

    /// The Stream (receive) phase: what is left of `end_ms` after setup, headers and first token.
    /// `None` when the end or the headers were not measured, or the parts exceed the end.
    #[must_use]
    pub fn receive_ms(&self) -> Option<u64> {
        let end = self.end_ms.ms()?;
        let headers = self.headers_ms.ms()?;
        end.checked_sub(self.setup_ms() + headers + self.first_token_ms.ms().unwrap_or(0))
    }
}

/// The `timing_json` of a log row (and of a capture): one shared type for the log, the API client,
/// probes and chat footers.
#[derive(Debug, Clone, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct TimingJson {
    /// Body read (t0) to the first attempt's `send`: decode, translation, routing and circuit
    /// checks. Failed attempts are listed in `attempts` with their own `end_ms`, so the wire
    /// strip's hollow segment is `queue_ms` plus the failed attempts' `end_ms`.
    pub queue_ms: u64,
    /// One entry per attempt, in order; the last one served the response.
    pub attempts: Vec<AttemptTiming>,
}

/// How far the timing arithmetic may be off, in milliseconds (clock reads and rounding).
const TIMING_TOLERANCE_MS: u64 = 1;

impl TimingJson {
    /// The arithmetic invariant: queue + the failed attempts' `end_ms` + the serving
    /// attempt's `end_ms` equals `total_ms` within 1 ms, and the serving attempt's measured
    /// phases (setup, headers, first token) fit inside its `end_ms`, so its receive phase is not
    /// negative. False when an attempt's end was not measured or there is no attempt.
    #[must_use]
    pub fn check_total(&self, total_ms: u64) -> bool {
        let Some((serving, failed)) = self.attempts.split_last() else {
            return false;
        };
        let mut sum = self.queue_ms;
        for attempt in failed.iter().chain(std::iter::once(serving)) {
            let Some(end) = attempt.end_ms.ms() else {
                return false;
            };
            sum += end;
        }
        sum.abs_diff(total_ms) <= TIMING_TOLERANCE_MS && serving.receive_ms().is_some()
    }
}

/// A stored capture: every column of `llm_debug_captures`, and what `llm_log_get` returns.
/// Everything in it has been through [`super::redact_exchange`].
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct CapturedExchange {
    /// The log row this capture belongs to.
    pub request_log_id: i64,
    /// When the request arrived.
    pub ts_ms: i64,
    /// The protocol the client spoke.
    pub client_protocol: Protocol,
    /// Client request headers (`client_headers_json`).
    pub client_req_headers: Vec<RedactedHeader>,
    /// Client request body (`client_body_text`).
    pub client_req_body: Option<CapturedBody>,
    /// Canonical hub request (`hub_request_json`).
    pub hub_request: Option<Value>,
    /// Upstream request URL, userinfo and secret query values masked (`upstream_url`).
    pub upstream_url: String,
    /// Upstream request headers (`upstream_headers_json`).
    pub upstream_req_headers: Vec<RedactedHeader>,
    /// Upstream request body (`upstream_req_body_text`); `None` means
    /// "forwarded unchanged except model".
    pub upstream_req_body: Option<CapturedBody>,
    /// Upstream status (`upstream_status`).
    pub upstream_status: Option<u16>,
    /// Upstream response headers (`upstream_resp_headers_json`).
    pub upstream_resp_headers: Vec<RedactedHeader>,
    /// Upstream response body (`upstream_resp_body_text`).
    pub upstream_resp_body: Option<CapturedBody>,
    /// Client response headers (`client_resp_headers_json`).
    pub client_resp_headers: Vec<RedactedHeader>,
    /// Client response body (`client_resp_body_text`).
    pub client_resp_body: Option<CapturedBody>,
    /// Upstream frames (`upstream_frames_json`).
    pub upstream_frames: Option<CapturedFrames>,
    /// Client frames (`client_frames_json`).
    pub client_frames: Option<CapturedFrames>,
    /// Translation notes (`translation_notes_json`).
    pub translation_notes: Vec<TranslationNote>,
    /// Phase timings (`phase_timings_json`).
    pub timing: TimingJson,
    /// Bytes this capture occupies (`bytes_total`): body text, frame data, headers, URL and the
    /// hub request.
    pub bytes_total: u64,
    /// Values removed from bodies, frames, the hub request and the URL (`redacted_count`); headers
    /// carry their own `redacted` tag and are not counted.
    pub redacted_count: u32,
}

#[cfg(test)]
mod tests;
