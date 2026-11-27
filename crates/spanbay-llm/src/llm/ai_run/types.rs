//! The data of an AI API run: what goes in ([`AiRunSpec`]), the events and result that come out.
//!
//! Single responsibility: plain types. Every DTO here is a wire DTO (`Debug, Clone, PartialEq,
//! Serialize, Deserialize`); enums use `kind` tags or snake-case words.

use std::path::PathBuf;
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::llm::capture_record::{CapturedBody, CapturedFrame, CapturedFrames, TimingJson};
use crate::llm::router::Target;
use crate::llm::safe_http::client::ReqwestSafeHttp;
use crate::llm::usage::{Cost, Price, PriceResolution};
use crate::llm::wire::Protocol;
use spanbay_model::assertion::AssertionResult;
use spanbay_model::script_result::{ConsoleEntry, ScriptError, ScriptTestResult};

/// How a run ended (the status words of the compare table).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AiRunStatus {
    /// An answer arrived (possibly an empty one: see `notice`).
    Done,
    /// The server answered with an error status, a redirect, or an error inside the stream.
    Rejected,
    /// No answer: the host could not be reached.
    Unreachable,
    /// The answer is not valid for the protocol.
    InvalidResponse,
    /// The request cannot be sent: refused address, missing endpoint or attachment.
    Unsupported,
    /// A compare row that was not run.
    Skipped,
    /// A limit of `timeout_ms` passed; the partial reply is kept.
    TimedOut,
    /// The user stopped the run; the partial reply is kept.
    Stopped,
}

impl AiRunStatus {
    /// The word the UI shows.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::Done => "Done",
            Self::Rejected => "Rejected",
            Self::Unreachable => "Unreachable",
            Self::InvalidResponse => "Invalid response",
            Self::Unsupported => "Unsupported",
            Self::Skipped => "Skipped",
            Self::TimedOut => "Timed out",
            Self::Stopped => "Stopped",
        }
    }
}

/// Where the request goes and how.
pub enum Destination {
    /// A configured provider: the core derives URL and credential from the target.
    Provider(Box<Target>),
    /// The URL the user typed, through the guarded streaming transport.
    Custom {
        /// The absolute URL (variables resolved).
        url: String,
        /// The transport (direct or through the configured proxy).
        http: ReqwestSafeHttp,
    },
}

/// Everything one call needs; the use case resolves variables, scripts and credentials first.
pub struct AiRunSpec {
    /// Id of the run (the log row's `source_ref`).
    pub run_id: String,
    /// Wire protocol of `body`.
    pub protocol: Protocol,
    /// The model the call names.
    pub model: String,
    /// The request wants a stream (`stream: true`, or the Gemini streaming action).
    pub stream: bool,
    /// Provider or custom target.
    pub destination: Destination,
    /// The user's headers (variables resolved; for a provider target the protected ones are
    /// already gone).
    pub headers: Vec<(String, String)>,
    /// Header names the core dropped (`Authorization` ...), for the result.
    pub dropped_headers: Vec<String>,
    /// The body text; `{{attachment:<sha256 prefix>}}` placeholders are replaced at send time.
    pub body: String,
    /// The URL the Raw tab shows (a non-routable label for a provider target).
    pub shown_url: String,
    /// `timeout_ms`: first-byte limit and silent gap of a stream, whole-call limit otherwise.
    pub timeout: Duration,
    /// "Keep this exchange in the log": a capture is written under every policy.
    pub keep_exchange: bool,
    /// The effective price of a provider model, in three states (C-$5).
    pub price: PriceResolution,
    /// Secret variable values to mask in the sent request view.
    pub secrets: Vec<String>,
    /// The response bytes read at most (the response body cap of the settings).
    pub max_response_bytes: usize,
    /// The attachment store (`<data>/attachments`); `None` when the app has no data folder.
    pub attachments_dir: Option<PathBuf>,
}

/// One header of the sent request or the response.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiHeader {
    /// Name as sent or received.
    pub name: String,
    /// The value; a credential is masked in the sent request.
    pub value: String,
}

/// The request as sent, credentials masked ("Request" under Raw).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiSentRequest {
    /// Always `POST`.
    pub method: String,
    /// The URL shown: the typed one, or the label of a provider.
    pub url: String,
    /// Headers in send order, injected credentials masked.
    pub headers: Vec<AiHeader>,
    /// The body text with attachment placeholders (never their data), cut at 256 KiB.
    pub body: String,
    /// Size of the whole body in bytes (data of attachments included).
    pub body_bytes: usize,
    /// `body` is cut.
    pub body_truncated: bool,
}

/// A tool call of the reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiToolCallDto {
    /// Call id.
    pub id: String,
    /// Function name.
    pub name: String,
    /// Arguments as the JSON text the model wrote.
    pub arguments: String,
}

/// One embedding vector: its length and first values ("Vectors").
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiVectorDto {
    /// Length of the vector.
    pub dimensions: usize,
    /// The first 8 values.
    pub first_values: Vec<f64>,
}

/// Token counts of the run.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AiUsageDto {
    /// Input tokens.
    pub input_tokens: Option<u64>,
    /// Output tokens.
    pub output_tokens: Option<u64>,
    /// Cached input tokens.
    pub cached_tokens: Option<u64>,
    /// Cache-write tokens.
    pub cache_write_tokens: Option<u64>,
    /// Reasoning tokens.
    pub reasoning_tokens: Option<u64>,
    /// `input + output`.
    pub total_tokens: Option<u64>,
    /// At least one count is the characters/4 estimate (shown with `~`).
    pub estimated: bool,
}

/// Why a run's cost line reads as it does (C-$4).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CostNoteCode {
    /// The model has no price.
    NoPrice,
    /// Priced per token, but the upstream reported no usage.
    NoUsage,
    /// A local runtime without a key: nothing to pay.
    LocalFree,
    /// The cost uses a catalogue price or estimated tokens.
    CatalogEstimate,
}

/// The result of one run, as the response tabs show it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiResultDto {
    /// Id of the run.
    pub run_id: String,
    /// How it ended.
    pub status: AiRunStatus,
    /// HTTP status of the answer, when there was one.
    pub http_status: Option<u16>,
    /// The shaped line for a failure: `Rejected 401: <provider message>`, `Cannot reach
    /// api.deepseek.com: <reason>`, `Timed out`, `Stopped at 1.0 s`.
    pub message: Option<String>,
    /// A remark on a finished run: `The model returned no text (finish reason: length)`.
    pub notice: Option<String>,
    /// The error event of the stream, if one arrived.
    pub stream_error: Option<String>,
    /// The model the call named.
    pub model: String,
    /// The model the answer says it came from.
    pub response_model: Option<String>,
    /// Protocol of the call.
    pub protocol: Protocol,
    /// The answer was an event stream.
    pub streamed: bool,
    /// The merged reply text (the user's own response: not redacted).
    pub reply: String,
    /// The merged reasoning text.
    pub reasoning: String,
    /// Tool calls in order.
    pub tool_calls: Vec<AiToolCallDto>,
    /// Embedding vectors (Embeddings protocol).
    pub vectors: Vec<AiVectorDto>,
    /// Why generation stopped, when the answer said.
    pub finish_reason: Option<String>,
    /// Token counts.
    pub usage: AiUsageDto,
    /// Milliseconds from the call's start to the first content event (streams only).
    pub ttft_ms: Option<u64>,
    /// Milliseconds from the call's start to the last byte.
    pub total_ms: u64,
    /// Milliseconds from the first content event to the last byte (streams only).
    pub stream_ms: Option<u64>,
    /// Output tokens per second; only with at least 300 reported output tokens.
    pub tokens_per_second: Option<f64>,
    /// The phases of the single attempt.
    pub timing: TimingJson,
    /// The cost, when a price is known.
    pub cost: Option<Cost>,
    /// The price the cost used (per million tokens).
    pub price: Option<Price>,
    /// `No price for this model` when `cost` is absent for lack of a price; the English fallback of
    /// `cost_note_code` for older front ends.
    pub cost_note: Option<String>,
    /// Why the cost line reads as it does (C-$4); the front end words it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_note_code: Option<CostNoteCode>,
    /// Header names the core dropped from the request.
    pub dropped_headers: Vec<String>,
    /// Response headers: `retry-after`, `x-ratelimit-*` and request ids first; cookies shown.
    pub response_headers: Vec<AiHeader>,
    /// The request as sent.
    pub request: AiSentRequest,
    /// The stream's frames (at most 2,000) with the footer counts. `t_ms` of each frame is the
    /// time since the request was SENT (the first frame carries the time to first byte); log
    /// captures of the same stream keep offsets from their first frame.
    pub frames: CapturedFrames,
    /// The response body of a non-streaming answer or an error, cut at 256 KiB.
    pub response_body: Option<CapturedBody>,
    /// Number of SSE events received.
    pub event_count: u64,
    /// A log row was written with a capture ("Kept in log").
    pub kept_in_log: bool,
    /// The log row of this run (`source = api`), once stored.
    pub log_row_id: Option<i64>,
    /// The `request_id` (`rq_<16 hex>`) of that row: the id Usage › Calls opens a call by.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub log_request_id: Option<String>,
    /// Results of the request's assertions (filled by the use case).
    #[serde(default)]
    pub assertion_results: Vec<AssertionResult>,
    /// Results of the test script's `pm.test` calls (filled by the use case).
    #[serde(default)]
    pub script_tests: Vec<ScriptTestResult>,
    /// Console output of the scripts and the run's own line (filled by the use case).
    #[serde(default)]
    pub console: Vec<ConsoleEntry>,
    /// An uncaught script failure (filled by the use case).
    #[serde(default)]
    pub script_error: Option<ScriptError>,
    /// Id of the history row the run wrote (filled by the use case); `None` when it wrote none.
    #[serde(default)]
    pub history_id: Option<String>,
    /// Whether that row was written (filled by the use case): `false` when the workspace is
    /// gone or the write failed.
    #[serde(default)]
    pub history_saved: bool,
}

/// The events of one run, in order: `Started`, `Frames` (at most 30 per second), `Done`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AiRunEvent {
    /// The call is about to be sent.
    Started {
        /// Id of the run.
        run_id: String,
        /// The model the call names.
        model: String,
    },
    /// Frames that arrived since the last event (`t_ms` counts from the request's send).
    Frames {
        /// The frames in order.
        items: Vec<CapturedFrame>,
    },
    /// The run ended; the last event.
    Done {
        /// The result.
        result: Box<AiResultDto>,
    },
}

/// What [`super::run_ai_request`] returns besides the events it sent.
pub struct AiRunOutcome {
    /// The result (assertion, test and console fields are empty: the use case fills them).
    pub result: AiResultDto,
    /// The response body as scripts see it: the event-stream text or the JSON body, cut at the
    /// response cap.
    pub raw_body: String,
}
