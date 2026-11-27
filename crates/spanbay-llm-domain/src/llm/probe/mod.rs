//! Pure probe vocabulary, plans, estimates, capability derivations, apply plans and exports.

pub mod apply;
pub mod capability;
pub mod estimate;
pub mod export;
pub mod listing;
pub mod plan;
pub mod quirks;
pub mod status;
pub mod timeouts;

/// Mirrors `spanbay_llm::llm::probe::run` so `spanbay-store` can use the same
/// `probe::run::{CellRecord, RequestRecord}` paths.
pub mod run {
    pub use super::{CellRecord, RequestRecord};
}

use serde::{Deserialize, Serialize};

use crate::llm::failure::FailureKind;

pub use capability::CapabilityUpdate;
pub use plan::SkipReason;
pub use status::ProbeStatus;
pub use timeouts::CheckTimeouts;

/// One capability test.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeTest {
    /// The host answers HTTP at all.
    Connectivity,
    /// The key is accepted.
    Auth,
    /// The model list can be fetched.
    Models,
    /// A plain non-streaming answer.
    Chat,
    /// A streamed answer with time to first token and throughput.
    Stream,
    /// A tool-call round trip.
    Tools,
    /// Structured output by JSON schema.
    JsonSchema,
    /// Image input.
    Vision,
    /// Reasoning output.
    Reasoning,
    /// Implicit prompt caching.
    Cache,
    /// An embedding vector.
    Embeddings,
    /// Provider-native web search.
    Search,
    /// A PDF document as input (the bench column "Document").
    Pdf,
    /// A decision-API answer.
    Decision,
    /// The largest accepted input.
    Context,
}

impl ProbeTest {
    /// Every test, in the order the UI lists them.
    pub const ALL: [ProbeTest; 15] = [
        Self::Connectivity,
        Self::Auth,
        Self::Models,
        Self::Chat,
        Self::Stream,
        Self::Tools,
        Self::JsonSchema,
        Self::Vision,
        Self::Reasoning,
        Self::Cache,
        Self::Embeddings,
        Self::Search,
        Self::Pdf,
        Self::Decision,
        Self::Context,
    ];

    /// The snake_case identifier stored in `ProbeResult::test`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Connectivity => "connectivity",
            Self::Auth => "auth",
            Self::Models => "models",
            Self::Chat => "chat",
            Self::Stream => "stream",
            Self::Tools => "tools",
            Self::JsonSchema => "json_schema",
            Self::Vision => "vision",
            Self::Reasoning => "reasoning",
            Self::Cache => "cache",
            Self::Embeddings => "embeddings",
            Self::Search => "search",
            Self::Pdf => "pdf",
            Self::Decision => "decision",
            Self::Context => "context",
        }
    }

    /// Parses [`ProbeTest::as_str`] output.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|test| test.as_str() == text)
    }

    /// Checks that belong to the key, not the model: Connectivity, Auth and Models.
    #[must_use]
    pub(crate) const fn is_key_scoped(self) -> bool {
        matches!(self, Self::Connectivity | Self::Auth | Self::Models)
    }

    /// Checks that send a chat request: they depend on the model's Chat result and do not apply
    /// to embedding models.
    #[must_use]
    pub(crate) const fn is_chat_family(self) -> bool {
        matches!(
            self,
            Self::Chat
                | Self::Stream
                | Self::Tools
                | Self::JsonSchema
                | Self::Vision
                | Self::Reasoning
                | Self::Cache
                | Self::Search
                | Self::Pdf
        )
    }

    /// Whether the test spends model tokens (everything except the free key-scoped HTTP checks).
    #[must_use]
    pub const fn costs_tokens(self) -> bool {
        !self.is_key_scoped()
    }
}

/// One HTTP request a check sent, kept for the request log (one `probe` row per request).
/// Carries no secret and no body.
#[derive(Debug, Clone, PartialEq)]
pub struct ProbeRequest {
    /// `base`, `models` or the protocol identifier of the call.
    pub endpoint: String,
    /// When the request started (Unix ms).
    pub started_ms: i64,
    /// Time until the response was complete (or failed).
    pub latency_ms: u64,
    /// Time to the first content event of a streamed call.
    pub ttft_ms: Option<u64>,
    /// HTTP status when one was received.
    pub status: Option<u16>,
    /// Provider-reported prompt tokens.
    pub input_tokens: Option<u64>,
    /// Provider-reported output tokens.
    pub output_tokens: Option<u64>,
    /// Provider-reported cached prompt tokens.
    pub cached_input_tokens: Option<u64>,
    /// Failure category when the request failed.
    pub failure_kind: Option<FailureKind>,
    /// Redacted one-line error when the request failed.
    pub error: Option<String>,
    /// Whether the response was an event stream.
    pub stream: bool,
    /// The id of the request-log row this request becomes (`rq_<16 hex>`, `source = probe`).
    pub request_id: String,
}

/// One persisted probe outcome for `(key, model, endpoint, test)`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ProbeResult {
    /// Key used.
    pub key_id: String,
    /// Model tested.
    pub model_id: String,
    /// Protocol identifier tested (`openai_chat`, …, or `models` for the list test); empty for a
    /// skipped cell.
    pub endpoint: String,
    /// Test name (`connectivity`, `chat`, `stream`, …).
    pub test: String,
    /// Whether it passed.
    pub ok: bool,
    /// HTTP status when one was received.
    #[serde(default)]
    pub status: Option<u16>,
    /// Total latency.
    #[serde(default)]
    pub latency_ms: Option<u64>,
    /// Time to first token.
    #[serde(default)]
    pub ttft_ms: Option<u64>,
    /// Output tokens per second.
    #[serde(default)]
    pub tps: Option<f64>,
    /// Prompt tokens.
    #[serde(default)]
    pub input_tokens: Option<u64>,
    /// Output tokens.
    #[serde(default)]
    pub output_tokens: Option<u64>,
    /// Failure category when `ok` is false.
    #[serde(default)]
    pub failure_kind: Option<FailureKind>,
    /// One-line outcome or error text.
    pub message: String,
    /// Test-specific detail (embedding dimension, cache figures, context size, …).
    #[serde(default)]
    pub detail: serde_json::Value,
    /// When it ran.
    pub checked_at_ms: i64,
    /// The run that produced it; `None` for a result of a single test outside a run.
    #[serde(default)]
    pub run_id: Option<String>,
    /// The status word ([`ProbeStatus::as_str`]) the result was stored with.
    #[serde(default)]
    pub status_word: String,
    /// Why a skipped cell did not run (`connectivity_failed`, `key_rejected`, …).
    #[serde(default)]
    pub skip_reason: Option<String>,
    /// The `llm_request_log` row id (`rq_<16 hex>`, `source = probe`) of the request that decided
    /// the result.
    #[serde(default)]
    pub request_id: Option<String>,
}

/// A finished or skipped cell, as persisted.
#[derive(Debug, Clone, PartialEq)]
pub struct CellRecord {
    /// Provider of the row.
    pub provider_id: String,
    /// Key of the row.
    pub key_id: String,
    /// Model of the row (key-scoped checks carry the key's first model).
    pub model_id: String,
    /// The check.
    pub test: ProbeTest,
    /// The status word.
    pub status: ProbeStatus,
    /// One-line reason: `reason_line` for a failure, the skip text for a skip, else empty.
    pub reason: String,
    /// Why the cell was skipped.
    pub skip: Option<SkipReason>,
    /// The check's result; `None` for a skipped cell (nothing was measured).
    pub result: Option<ProbeResult>,
    /// The capability bit the result measured (`derive_capability`).
    pub capability: Option<CapabilityUpdate>,
}

/// One HTTP request of a check, for the request log (`source = probe`).
#[derive(Debug, Clone, PartialEq)]
pub struct RequestRecord {
    /// Provider called.
    pub provider_id: String,
    /// Key used.
    pub key_id: String,
    /// Model tested.
    pub model_id: String,
    /// The request.
    pub request: ProbeRequest,
}

/// A file for the save dialog (bench CSV/JSON exports and compare runs).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExportFile {
    /// Suggested filename.
    pub file_name: String,
    /// The content.
    pub bytes: Vec<u8>,
}

#[cfg(test)]
pub(crate) mod test_support;

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::ProbeTest;

    #[test]
    fn the_column_order_is_the_derived_order() {
        // Plans sort cells with the derived `Ord`, so `ALL` must list the variants in order.
        assert!(ProbeTest::ALL.is_sorted());
    }

    #[test]
    fn identifiers_round_trip_and_only_the_key_checks_are_free() {
        for test in ProbeTest::ALL {
            assert_eq!(ProbeTest::parse(test.as_str()), Some(test));
            assert_eq!(serde_json::to_value(test).unwrap(), json!(test.as_str()));
        }
        assert_eq!(ProbeTest::parse("nope"), None);
        let free: Vec<_> = ProbeTest::ALL
            .into_iter()
            .filter(|test| !test.costs_tokens())
            .collect();
        assert_eq!(
            free,
            [ProbeTest::Connectivity, ProbeTest::Auth, ProbeTest::Models]
        );
    }
}
