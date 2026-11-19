//! One executed request inside a collection run, as the runner reports it.

use serde::{Deserialize, Serialize};

use crate::{
    assertion::AssertionResult,
    models::{AiStepSummary, HttpMethod},
    script_result::{ConsoleEntry, ScriptError, ScriptTestResult},
};

/// One executed request inside a collection run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerItem {
    /// Request id.
    pub request_id: String,
    /// Request name.
    pub request_name: String,
    /// HTTP method.
    pub method: HttpMethod,
    /// The URL that was sent, with secret variable values masked; for a request that failed
    /// before sending, its URL with the known variables filled in.
    pub url: String,
    /// The history row this execution wrote; `None` when none was written (its workspace is
    /// gone).
    pub history_entry_id: Option<String>,
    /// Zero-based iteration.
    pub iteration: u32,
    /// Zero-based data row this iteration used; `None` when the run had no data.
    pub data_row: Option<u32>,
    /// The first [`DATA_PREVIEW_COLUMNS`] columns of that row in key order,
    /// values cut to [`DATA_PREVIEW_CHARS`] characters.
    pub data_preview: Vec<(String, String)>,
    /// Status code, `None` when the exchange failed before a response.
    pub status_code: Option<u16>,
    /// Status text, or `"Failed"`.
    pub status_text: String,
    /// Wall time for this request.
    pub elapsed_ms: u128,
    /// HTTP attempts spent (retries included).
    pub attempt_count: u32,
    /// Whether assertions and script tests passed.
    pub passed: bool,
    /// Transport or script failure message.
    pub error: Option<String>,
    /// Assertion outcomes.
    pub assertion_results: Vec<AssertionResult>,
    /// `pm.test` outcomes.
    pub script_tests: Vec<ScriptTestResult>,
    /// Script console output.
    pub console: Vec<ConsoleEntry>,
    /// Script failure, when one ended the request.
    pub script_error: Option<ScriptError>,
    /// Model, finish reason and cost of an AI request.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub ai: Option<AiStepSummary>,
}

/// Data columns a [`RunnerItem`] previews.
pub const DATA_PREVIEW_COLUMNS: usize = 2;
/// Characters kept of each previewed data value.
pub const DATA_PREVIEW_CHARS: usize = 64;

/// The preview of one data row for [`RunnerItem::data_preview`].
pub fn data_preview<'a>(
    row: impl IntoIterator<Item = (&'a String, &'a String)>,
) -> Vec<(String, String)> {
    row.into_iter()
        .take(DATA_PREVIEW_COLUMNS)
        .map(|(key, value)| {
            (
                key.clone(),
                value.chars().take(DATA_PREVIEW_CHARS).collect(),
            )
        })
        .collect()
}

/// How a stored collection run ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum CollectionRunStatus {
    /// Every planned request ran.
    Done,
    /// Cancelled, `stop_on_error`, `setNextRequest(null)` or the step ceiling.
    Stopped,
}

/// A finished collection run as `collection_runs` keeps it, so the run can be reopened after a
/// restart. Items carry no script console; their history rows hold the exchange.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct CollectionRunSummary {
    /// Run id; also the `run_id` of the history rows the run wrote.
    pub run_id: String,
    /// Workspace that owns the collection.
    pub workspace_id: String,
    /// Collection that ran.
    pub collection_id: String,
    /// Collection name when the run started.
    pub collection_name: String,
    /// Folder scope, when the run covered one folder subtree.
    pub folder_id: Option<String>,
    /// Requests the run was narrowed to, when it was.
    pub request_ids: Option<Vec<String>>,
    /// Environment the run used.
    pub environment_id: Option<String>,
    /// Environment name when the run started.
    pub environment_name: Option<String>,
    /// Unix milliseconds.
    pub started_at_ms: i64,
    /// Unix milliseconds.
    pub finished_at_ms: i64,
    /// How it ended.
    pub status: CollectionRunStatus,
    /// Iterations requested.
    pub iterations: u32,
    /// Requests the run planned (requests in scope times iterations, capped).
    pub planned: usize,
    /// Requests executed.
    pub total: usize,
    /// Requests that passed every check.
    pub passed: usize,
    /// Requests that failed.
    pub failed: usize,
    /// Per-request outcomes in execution order, without console output.
    pub items: Vec<RunnerItem>,
}
