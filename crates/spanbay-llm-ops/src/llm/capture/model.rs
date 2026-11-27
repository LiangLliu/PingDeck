//! The capture data model of the request path: the raw exchange it hands over and the row facts
//! curl and replay need. The redacted exchange the store persists and the timing model live in
//! `spanbay_llm_domain::llm::capture_record`. Types only; no policy, no redaction.

use std::fmt;

use serde::{Deserialize, Serialize};
use serde_json::Value;

pub use spanbay_model::phase::PhaseValue;

use super::bound::BoundedBody;
use crate::llm::wire::Protocol;
pub(super) use spanbay_llm_domain::llm::capture_record::{
    CapturedBody, CapturedExchange, CapturedFrame, CapturedFrames, RedactedHeader, TimingJson,
    TranslationNote,
};

impl From<BoundedBody> for CapturedBody {
    fn from(body: BoundedBody) -> Self {
        Self {
            text: Some(body.text),
            truncated: body.truncated,
            elided: body.elided,
            original_bytes: body.original_bytes,
        }
    }
}

/// The facts of a log row that curl, replay and "Open in API client" need beyond the capture
/// (the capture holds the wire text, the row holds who served it).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RowFacts {
    /// The protocol spoken upstream (`protocol_out`); `None` when no upstream was reached.
    pub protocol_out: Option<Protocol>,
    /// The model the client asked for.
    pub requested_model: String,
    /// The model sent upstream.
    pub upstream_model: Option<String>,
    /// The client asked for a stream.
    pub stream: bool,
    /// The provider that served the call.
    pub provider_id: Option<String>,
    /// That provider still exists (a deleted one opens in Custom URL mode).
    pub provider_exists: bool,
}

/// What the request path hands to the log writer: bounded, not yet redacted. Redaction happens in
/// [`super::redact_exchange`] on the writer thread. `Debug` shows header names, sizes and counts,
/// never a header value, body, frame, URL, note or the hub request.
#[derive(Clone, PartialEq)]
pub struct RawExchange {
    /// The log row this capture belongs to.
    pub request_log_id: i64,
    /// When the request arrived.
    pub ts_ms: i64,
    /// The protocol the client spoke.
    pub client_protocol: Protocol,
    /// Client request headers as received.
    pub client_req_headers: Vec<(String, String)>,
    /// Client request body.
    pub client_req_body: Option<BoundedBody>,
    /// Canonical hub request after the inbound decode.
    pub hub_request: Option<Value>,
    /// Upstream request URL as sent (empty when no upstream was reached).
    pub upstream_url: String,
    /// Upstream request headers as sent.
    pub upstream_req_headers: Vec<(String, String)>,
    /// Upstream request body; `None` when equal to the client's except `model`.
    pub upstream_req_body: Option<BoundedBody>,
    /// Upstream response status.
    pub upstream_status: Option<u16>,
    /// Upstream response headers.
    pub upstream_resp_headers: Vec<(String, String)>,
    /// Upstream response body (non-streaming).
    pub upstream_resp_body: Option<BoundedBody>,
    /// Client response headers.
    pub client_resp_headers: Vec<(String, String)>,
    /// Client response body (non-streaming).
    pub client_resp_body: Option<BoundedBody>,
    /// Upstream stream frames (streaming).
    pub upstream_frames: Option<CapturedFrames>,
    /// Client stream frames (streaming).
    pub client_frames: Option<CapturedFrames>,
    /// Translation notes.
    pub translation_notes: Vec<TranslationNote>,
    /// Per-phase timings.
    pub timing: TimingJson,
}

/// The names of `headers`, for a `Debug` that must not show their values.
fn header_names(headers: &[(String, String)]) -> Vec<&str> {
    headers.iter().map(|(name, _)| name.as_str()).collect()
}

impl fmt::Debug for RawExchange {
    fn fmt(&self, f: &mut fmt::Formatter<'_>) -> fmt::Result {
        let frames = |frames: &Option<CapturedFrames>| frames.as_ref().map(|f| f.frames.len());
        f.debug_struct("RawExchange")
            .field("request_log_id", &self.request_log_id)
            .field("ts_ms", &self.ts_ms)
            .field("client_protocol", &self.client_protocol)
            .field(
                "client_req_headers",
                &header_names(&self.client_req_headers),
            )
            .field("client_req_body", &self.client_req_body)
            .field("hub_request", &self.hub_request.is_some())
            .field("upstream_url_len", &self.upstream_url.len())
            .field(
                "upstream_req_headers",
                &header_names(&self.upstream_req_headers),
            )
            .field("upstream_req_body", &self.upstream_req_body)
            .field("upstream_status", &self.upstream_status)
            .field(
                "upstream_resp_headers",
                &header_names(&self.upstream_resp_headers),
            )
            .field("upstream_resp_body", &self.upstream_resp_body)
            .field(
                "client_resp_headers",
                &header_names(&self.client_resp_headers),
            )
            .field("client_resp_body", &self.client_resp_body)
            .field("upstream_frames", &frames(&self.upstream_frames))
            .field("client_frames", &frames(&self.client_frames))
            .field("translation_notes", &self.translation_notes.len())
            .field("timing", &self.timing)
            .finish()
    }
}
