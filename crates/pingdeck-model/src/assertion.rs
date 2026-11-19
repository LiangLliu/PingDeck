//! Assertion outcomes: the result type every run stores and every UI renders.

use serde::{Deserialize, Serialize};

/// Outcome of evaluating one assertion, identified by its index in the list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AssertionResult {
    pub index: usize,
    pub label: String,
    pub passed: bool,
    pub message: String,
    /// The value the assertion required: a status, a header or JSON value, a limit. `None` for a
    /// check without a value pair (an existence, containment or schema check): `message` says
    /// what was looked for.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub expected: Option<String>,
    /// The value the response carried, in the same form as `expected`; `None` when the check has
    /// no value pair.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub actual: Option<String>,
}

/// What the AI assertion kinds read from an AI API run: the merged reply and the
/// run's numbers, not the raw event stream the HTTP kinds see.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct AiResultView<'a> {
    /// HTTP status of the exchange.
    pub status: u16,
    /// Merged reply text.
    pub reply: &'a str,
    /// Finish reason the stream reported.
    pub finish_reason: Option<&'a str>,
    /// In-stream error event message, if any.
    pub error: Option<&'a str>,
    /// Milliseconds from Send to the first token.
    pub ttft_ms: Option<u64>,
    /// Input tokens.
    pub input_tokens: Option<u64>,
    /// Output tokens.
    pub output_tokens: Option<u64>,
}
