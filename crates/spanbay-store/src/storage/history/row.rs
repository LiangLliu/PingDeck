//! Row mapping between `execution_history` SQLite rows and `ExecutionHistoryEntry`.

use spanbay_model::models::{
    EnvironmentId, ExecutionHistoryEntry, ExecutionHistoryResultKind, HistoryCheckSummary,
    HistoryEntryId, HistoryOriginRef, HistorySource, KeyValue, Request, RequestId, WorkspaceId,
};

use crate::storage::StorageError;
use crate::storage::requests::http_method_from_storage;

/// Every `execution_history` column. One order serves three places that must stay aligned:
/// the insert binds (`?1..?35`), the full-row read ([`SELECT_FULL_ROW_BY_ID`]) and
/// [`stored_execution_history_row`]; the list query's column list mirrors it position by
/// position.
macro_rules! history_columns {
    () => {
        "
    id, workspace_id, request_id, environment_id, result_kind, request_name, method, request_url,
    status_code, status_text, final_url, elapsed_ms, body_size_bytes, attempt_count,
    response_body_preview, response_body_truncated, error_kind, error_message, executed_at,
    request_snapshot_json, source, source_name, source_id, run_id, protocol, timing_json,
    environment_name, response_headers_json, response_headers_truncated, sent_request_json,
    call_chain_json, connect_phase, check_passed, check_failed, origin_ref
"
    };
}
pub(super) use history_columns;

/// One whole row of `workspace_id` (`?1`) by id (`?2`), blobs included.
pub(super) const SELECT_FULL_ROW_BY_ID: &str = concat!(
    "SELECT ",
    history_columns!(),
    " FROM execution_history WHERE workspace_id = ?1 AND id = ?2"
);

pub(super) struct StoredExecutionHistoryEntry {
    id: String,
    workspace_id: String,
    request_id: Option<String>,
    environment_id: Option<String>,
    result_kind: String,
    request_name: String,
    method: String,
    request_url: String,
    status_code: i64,
    status_text: String,
    final_url: String,
    elapsed_ms: i64,
    body_size_bytes: i64,
    attempt_count: i64,
    response_body_preview: String,
    response_body_truncated: bool,
    error_kind: Option<String>,
    error_message: Option<String>,
    executed_at: String,
    request_snapshot_json: Option<String>,
    source: String,
    source_name: String,
    source_id: String,
    run_id: Option<String>,
    protocol: String,
    timing_json: Option<String>,
    environment_name: String,
    response_headers_json: Option<String>,
    response_headers_truncated: bool,
    sent_request_json: Option<String>,
    call_chain_json: Option<String>,
    connect_phase: Option<bool>,
    check_passed: Option<i64>,
    check_failed: Option<i64>,
    origin_ref: Option<String>,
}

pub(super) fn stored_execution_history_row(
    row: &rusqlite::Row<'_>,
) -> rusqlite::Result<StoredExecutionHistoryEntry> {
    Ok(StoredExecutionHistoryEntry {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        request_id: row.get(2)?,
        environment_id: row.get(3)?,
        result_kind: row.get(4)?,
        request_name: row.get(5)?,
        method: row.get(6)?,
        request_url: row.get(7)?,
        status_code: row.get(8)?,
        status_text: row.get(9)?,
        final_url: row.get(10)?,
        elapsed_ms: row.get(11)?,
        body_size_bytes: row.get(12)?,
        attempt_count: row.get(13)?,
        response_body_preview: row.get(14)?,
        response_body_truncated: row.get(15)?,
        error_kind: row.get(16)?,
        error_message: row.get(17)?,
        executed_at: row.get(18)?,
        request_snapshot_json: row.get(19)?,
        source: row.get(20)?,
        source_name: row.get(21)?,
        source_id: row.get(22)?,
        run_id: row.get(23)?,
        protocol: row.get(24)?,
        timing_json: row.get(25)?,
        environment_name: row.get(26)?,
        response_headers_json: row.get(27)?,
        response_headers_truncated: row.get(28)?,
        sent_request_json: row.get(29)?,
        call_chain_json: row.get(30)?,
        connect_phase: row.get(31)?,
        check_passed: row.get(32)?,
        check_failed: row.get(33)?,
        origin_ref: row.get(34)?,
    })
}

impl StoredExecutionHistoryEntry {
    pub(super) fn into_domain(self) -> Result<ExecutionHistoryEntry, StorageError> {
        let method = http_method_from_storage(&self.id, &self.method)?;
        let result_kind = execution_history_result_kind_from_storage(&self.id, &self.result_kind)?;
        let source = source_from_storage("execution_history", &self.id, &self.source)?;
        // Blobs a newer or older build wrote in another shape are dropped, not fatal: the row's
        // summary columns still describe the exchange.
        let response_headers = self
            .response_headers_json
            .and_then(|json| serde_json::from_str::<Vec<KeyValue>>(&json).ok())
            .unwrap_or_default();

        Ok(ExecutionHistoryEntry {
            id: HistoryEntryId::new(&self.id)?,
            workspace_id: WorkspaceId::new(&self.workspace_id)?,
            request_id: self.request_id.map(RequestId::new).transpose()?,
            environment_id: self.environment_id.map(EnvironmentId::new).transpose()?,
            result_kind,
            request_name: self.request_name,
            method,
            request_url: self.request_url,
            status_code: self.status_code.clamp(0, i64::from(u16::MAX)) as u16,
            status_text: self.status_text,
            final_url: self.final_url,
            elapsed_ms: self.elapsed_ms.max(0) as u128,
            body_size_bytes: usize::try_from(self.body_size_bytes.max(0)).unwrap_or(usize::MAX),
            attempt_count: u32::try_from(self.attempt_count.max(1)).unwrap_or(u32::MAX),
            response_body_preview: self.response_body_preview,
            response_body_truncated: self.response_body_truncated,
            error_kind: self.error_kind,
            error_message: self.error_message,
            executed_at: self.executed_at,
            request_snapshot: self
                .request_snapshot_json
                .and_then(|json| serde_json::from_str::<Request>(&json).ok()),
            source,
            source_name: self.source_name,
            source_id: self.source_id,
            run_id: self.run_id,
            protocol: self.protocol,
            timing: self
                .timing_json
                .and_then(|json| serde_json::from_str(&json).ok()),
            environment_name: self.environment_name,
            response_headers,
            response_headers_truncated: self.response_headers_truncated,
            sent_request: self
                .sent_request_json
                .and_then(|json| serde_json::from_str(&json).ok()),
            call_chain: self
                .call_chain_json
                .and_then(|json| serde_json::from_str(&json).ok()),
            collection_path: Vec::new(),
            connect_phase: self.connect_phase,
            check_summary: match (self.check_passed, self.check_failed) {
                (None, None) => None,
                (passed, failed) => Some(HistoryCheckSummary {
                    passed: count_column(passed),
                    failed: count_column(failed),
                }),
            },
            origin_ref: self
                .origin_ref
                .as_deref()
                .and_then(HistoryOriginRef::from_storage),
            origin_label: None,
            origin_turn_id: None,
        })
    }
}

pub(super) fn source_from_storage(
    table: &'static str,
    row_id: &str,
    source: &str,
) -> Result<HistorySource, StorageError> {
    HistorySource::parse(source).ok_or_else(|| StorageError::InvalidStoredValue {
        table,
        column: "source",
        value: format!("{source} (row {row_id})"),
    })
}

/// A stored count column: NULL and negatives read as 0.
fn count_column(value: Option<i64>) -> u32 {
    u32::try_from(value.unwrap_or(0).max(0)).unwrap_or(u32::MAX)
}

pub(super) fn execution_history_result_kind_to_storage(
    result_kind: &ExecutionHistoryResultKind,
) -> &'static str {
    match result_kind {
        ExecutionHistoryResultKind::Success => "Success",
        ExecutionHistoryResultKind::Failure => "Failure",
    }
}

fn execution_history_result_kind_from_storage(
    history_entry_id: &str,
    result_kind: &str,
) -> Result<ExecutionHistoryResultKind, StorageError> {
    match result_kind {
        "Success" => Ok(ExecutionHistoryResultKind::Success),
        "Failure" => Ok(ExecutionHistoryResultKind::Failure),
        _ => Err(StorageError::InvalidExecutionHistoryResultKind {
            history_entry_id: history_entry_id.to_owned(),
            result_kind: result_kind.to_owned(),
        }),
    }
}
