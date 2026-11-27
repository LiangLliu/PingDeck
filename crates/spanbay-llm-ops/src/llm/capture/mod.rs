//! Debug capture of model exchanges: the capture policy, the bounded copy on the request path,
//! redaction on the log-writer thread, and what a stored capture turns back into (merged reply,
//! curl, replay data, diagnostics bundle). No SQL (the store persists
//! [`CapturedExchange`](spanbay_llm_domain::llm::capture_record::CapturedExchange)), no
//! queue or writer thread (the gateway owns them), no UI.
//!
//! Data flow: the request path copies bytes with [`bound_body`] / [`FrameRecorder`] (the caps
//! plus a copy margin) into a [`RawExchange`]; the log-writer thread runs [`redact_exchange`]
//! (redact the full text, elide base64, cut to the caps; never on the request path) and
//! [`redact_row_texts`] for the row's own text, and the store writes the result. One redaction
//! table (R1-R10) serves all of it: the text rules live in `spanbay_base::secrets`, the
//! structural ones (headers, URL, JSON keys) in `redact`.

mod bound;
mod curl;
mod diagnostics;
mod model;
mod policy;
mod redact;
mod replay;
mod reply;
mod stored;

pub use bound::{
    BASE64_ELISION_THRESHOLD, BODY_COPY_MARGIN, BoundedBody, FRAME_COPY_MARGIN_BYTES,
    FRAME_COPY_MARGIN_FRAMES, FrameRecorder, MAX_BODY_BYTES, MAX_FRAME_BYTES, MAX_FRAMES,
    bound_body,
};
pub use curl::{CurlKind, CurlOutput, CurlRefusal, build_curl};
pub use diagnostics::{
    AppDiag, ClientDiag, DIAGNOSTICS_MAX_ROWS, DiagnosticsInput, DiagnosticsRefusal, FallbackDiag,
    GatewayDiag, LogSummaryDiag, ProviderDiag, RowDiag, build_diagnostics,
};
pub use model::{PhaseValue, RawExchange, RowFacts};
pub(crate) use spanbay_llm_domain::llm::capture_record::AttemptTiming;
// Named by the tests through the module.
pub use policy::{CaptureSource, LogOutcome, should_capture};
pub use redact::{redact_exchange, redact_exchange_hiding, redact_row_texts, redact_text};
pub use replay::{
    ApiClientDraft, ApiClientTarget, ReplayBlock, open_in_api_client, replay_eligibility,
};
pub use reply::{EmbeddingSummary, MergedReply, MergedSource, MergedToolCall, merged_reply};
#[cfg(test)]
pub(crate) use spanbay_llm_domain::llm::capture_record::{
    CapturedBody, CapturedExchange, CapturedFrame, CapturedFrames, RedactedHeader, TimingJson,
    TranslationKind, TranslationNote,
};
pub use spanbay_model::settings::limits::{
    CAPTURE_MAX_BYTES, CAPTURE_MAX_DAYS, CaptureMode, LOG_MAX_DAYS, LOG_MAX_ROWS,
};

// Named by the tests through the facade.
#[cfg(test)]
use diagnostics::DIAGNOSTICS_MAX_BYTES;

#[cfg(test)]
mod tests;
