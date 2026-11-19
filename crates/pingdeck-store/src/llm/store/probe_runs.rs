//! Test bench runs, the apply log behind "Undo last apply", and probe rows of the request log.
//!
//! Single responsibility: SQL over `llm_probe_runs`, `llm_probe_apply_log` (the newest
//! [`MAX_APPLY_LOG`] applies) and the `source = probe` rows of `llm_request_log`.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::log::{LogWrite, insert_log};
use super::{
    ProbeApplyRecord, RequestLogEntry, atomic, book_entry, db, from_json, json_text, to_u64,
};
use crate::llm::failure::FailureKind;
use crate::llm::probe::ProbeRequest;
use crate::llm::probe::apply::{ApplyReport, undo_available};
use crate::llm::probe::run::RequestRecord;
use crate::llm::usage::CostUsage;
use crate::sqlite::i64_from_usize;
use pingdeck_base::error::AppError;

/// Applies the log keeps; older ones can no longer be undone.
pub const MAX_APPLY_LOG: usize = 5;

/// One row of `llm_probe_runs`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeRunRow {
    pub run_id: String,
    /// The provider the run covered; `None` for a run over several providers.
    pub provider_id: Option<String>,
    /// The check set (`quick`, `standard`, `full`, `custom`).
    pub check_set: String,
    /// When the run started, Unix ms.
    pub started_at_ms: i64,
    /// When it finished; `None` while it runs (or when the app quit mid-run).
    pub finished_at_ms: Option<i64>,
    /// Cells that passed.
    pub passed: u32,
    /// Cells that ran.
    pub total: u32,
    /// Totals by status and anything else the run summary carries.
    pub summary: Value,
    /// The run this one is compared with.
    pub previous_run_id: Option<String>,
}

const RUN_COLUMNS: &str = "run_id, provider_id, check_set, started_at_ms, finished_at_ms, passed, \
     total, summary_json, previous_run_id";

fn run_from_row(row: &Row<'_>) -> rusqlite::Result<ProbeRunRow> {
    let summary: String = row.get(7)?;
    Ok(ProbeRunRow {
        run_id: row.get(0)?,
        provider_id: row.get(1)?,
        check_set: row.get(2)?,
        started_at_ms: row.get(3)?,
        finished_at_ms: row.get(4)?,
        passed: u32::try_from(to_u64(row.get(5)?)).unwrap_or(u32::MAX),
        total: u32::try_from(to_u64(row.get(6)?)).unwrap_or(u32::MAX),
        summary: serde_json::from_str(&summary).unwrap_or(Value::Null),
        previous_run_id: row.get(8)?,
    })
}

/// # Errors
/// [`AppError::Database`] on SQLite failure (a run id that exists already is refused).
pub fn start_probe_run(
    connection: &Connection,
    run_id: &str,
    provider_id: Option<&str>,
    check_set: &str,
    previous_run_id: Option<&str>,
    now_ms: i64,
) -> Result<(), AppError> {
    connection
        .execute(
            "INSERT INTO llm_probe_runs (run_id, provider_id, check_set, started_at_ms, previous_run_id) \
             VALUES (?1, ?2, ?3, ?4, ?5)",
            params![run_id, provider_id, check_set, now_ms, previous_run_id],
        )
        .map_err(db)?;
    Ok(())
}

/// Replaces the summary of a run while it runs (the plan spec that lets an interrupted run be
/// resumed).
///
/// # Errors
/// [`AppError::NotFound`] when the run was never started, [`AppError::Database`] otherwise.
pub fn set_probe_run_summary(
    connection: &Connection,
    run_id: &str,
    summary: &Value,
) -> Result<(), AppError> {
    let updated = connection
        .execute(
            "UPDATE llm_probe_runs SET summary_json = ?2 WHERE run_id = ?1",
            params![run_id, json_text(summary)?],
        )
        .map_err(db)?;
    if updated == 0 {
        return Err(AppError::not_found(format!("probe run {run_id} not found")));
    }
    Ok(())
}

/// Reopens a finished probe run as unfinished and drops specific test rows from its history.
/// Test-only: the statements are not one transaction.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure (the statements before it stay applied).
#[cfg(feature = "test-support")]
pub fn reopen_probe_run(
    connection: &Connection,
    run_id: &str,
    summary: &Value,
    dropped_tests: &[&str],
) -> Result<(), AppError> {
    connection
        .execute(
            "UPDATE llm_probe_runs SET finished_at_ms = NULL, passed = 0, total = 0, \
             summary_json = ?2 WHERE run_id = ?1",
            params![run_id, json_text(summary)?],
        )
        .map_err(db)?;
    for test in dropped_tests {
        connection
            .execute(
                "DELETE FROM llm_probe_history WHERE run_id = ?1 AND test = ?2",
                params![run_id, test],
            )
            .map_err(db)?;
    }
    Ok(())
}

/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn get_probe_run(
    connection: &Connection,
    run_id: &str,
) -> Result<Option<ProbeRunRow>, AppError> {
    connection
        .query_row(
            &format!("SELECT {RUN_COLUMNS} FROM llm_probe_runs WHERE run_id = ?1"),
            params![run_id],
            run_from_row,
        )
        .optional()
        .map_err(db)
}

/// Runs that never finished (still running, or the app quit mid-run), oldest first.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn unfinished_probe_runs(connection: &Connection) -> Result<Vec<ProbeRunRow>, AppError> {
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {RUN_COLUMNS} FROM llm_probe_runs WHERE finished_at_ms IS NULL \
             ORDER BY started_at_ms, rowid"
        ))
        .map_err(db)?;
    let rows = statement.query_map([], run_from_row).map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// Records the end of a run: how many cells passed of how many ran, and the run summary.
///
/// # Errors
/// [`AppError::NotFound`] when the run was never started, [`AppError::Database`] otherwise.
pub fn finish_probe_run(
    connection: &Connection,
    run_id: &str,
    passed: u32,
    total: u32,
    summary: &Value,
    now_ms: i64,
) -> Result<(), AppError> {
    let updated = connection
        .execute(
            "UPDATE llm_probe_runs SET finished_at_ms = ?2, passed = ?3, total = ?4, summary_json = ?5 \
             WHERE run_id = ?1",
            params![run_id, now_ms, passed, total, json_text(summary)?],
        )
        .map_err(db)?;
    if updated == 0 {
        return Err(AppError::not_found(format!("probe run {run_id} not found")));
    }
    Ok(())
}

/// The newest run of any provider (the "Last run 12/38 ok" line of the pane).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn latest_probe_run(connection: &Connection) -> Result<Option<ProbeRunRow>, AppError> {
    connection
        .query_row(
            &format!(
                "SELECT {RUN_COLUMNS} FROM llm_probe_runs ORDER BY started_at_ms DESC, rowid DESC LIMIT 1"
            ),
            [],
            run_from_row,
        )
        .optional()
        .map_err(db)
}

/// The newest `limit` runs, optionally only those of one provider, newest first.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn list_probe_runs(
    connection: &Connection,
    provider_id: Option<&str>,
    limit: usize,
) -> Result<Vec<ProbeRunRow>, AppError> {
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {RUN_COLUMNS} FROM llm_probe_runs WHERE (?1 IS NULL OR provider_id = ?1) \
             ORDER BY started_at_ms DESC, rowid DESC LIMIT ?2"
        ))
        .map_err(db)?;
    let rows = statement
        .query_map(params![provider_id, i64_from_usize(limit)], run_from_row)
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// What `previous_json` holds: the provider (the table has no column for it) and the report.
#[derive(Serialize, Deserialize)]
struct StoredApply {
    provider_id: String,
    report: ApplyReport,
}

fn apply_from_row(row: &Row<'_>) -> rusqlite::Result<Result<ProbeApplyRecord, AppError>> {
    let apply_id: String = row.get(0)?;
    let run_id: String = row.get(1)?;
    let applied_at_ms: i64 = row.get(2)?;
    let previous: String = row.get(3)?;
    Ok(
        from_json::<StoredApply>(&previous, "probe apply log").map(|stored| ProbeApplyRecord {
            apply_id,
            run_id,
            provider_id: stored.provider_id,
            applied_at_ms,
            report: stored.report,
        }),
    )
}

/// Logs an apply (the previous flags are what "Undo" restores) and keeps the newest
/// [`MAX_APPLY_LOG`].
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn record_probe_apply(
    connection: &Connection,
    apply_id: &str,
    run_id: &str,
    provider_id: &str,
    report: &ApplyReport,
    now_ms: i64,
) -> Result<(), AppError> {
    let stored = json_text(&StoredApply {
        provider_id: provider_id.to_owned(),
        report: report.clone(),
    })?;
    atomic(connection, |connection| {
        connection
            .execute(
                "INSERT INTO llm_probe_apply_log (apply_id, run_id, applied_at_ms, previous_json) \
                 VALUES (?1, ?2, ?3, ?4)",
                params![apply_id, run_id, now_ms, stored],
            )
            .map_err(db)?;
        connection
            .execute(
                "DELETE FROM llm_probe_apply_log WHERE apply_id NOT IN ( \
                     SELECT apply_id FROM llm_probe_apply_log \
                     ORDER BY applied_at_ms DESC, rowid DESC LIMIT ?1)",
                params![i64_from_usize(MAX_APPLY_LOG)],
            )
            .map_err(db)?;
        Ok(())
    })
}

/// # Errors
/// [`AppError::Database`] on SQLite failure or an unreadable stored report.
pub fn get_probe_apply(
    connection: &Connection,
    apply_id: &str,
) -> Result<Option<ProbeApplyRecord>, AppError> {
    connection
        .query_row(
            "SELECT apply_id, run_id, applied_at_ms, previous_json FROM llm_probe_apply_log \
             WHERE apply_id = ?1",
            params![apply_id],
            apply_from_row,
        )
        .optional()
        .map_err(db)?
        .transpose()
}

/// The newest logged apply.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure or an unreadable stored report.
#[cfg(test)]
pub(crate) fn latest_probe_apply(
    connection: &Connection,
) -> Result<Option<ProbeApplyRecord>, AppError> {
    connection
        .query_row(
            "SELECT apply_id, run_id, applied_at_ms, previous_json FROM llm_probe_apply_log \
             ORDER BY applied_at_ms DESC, rowid DESC LIMIT 1",
            [],
            apply_from_row,
        )
        .optional()
        .map_err(db)?
        .transpose()
}

/// The apply "Undo last apply" offers: the newest apply of probe results (never a bulk switch
/// of the Models tab, run id `bulk:`) of `provider_id` (any provider when `None`), while it is
/// younger than 24 hours. The bench of provider A never offers to undo an apply on provider B.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure or an unreadable stored report.
pub fn latest_undoable_apply(
    connection: &Connection,
    now_ms: i64,
    provider_id: Option<&str>,
) -> Result<Option<ProbeApplyRecord>, AppError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT apply_id, run_id, applied_at_ms, previous_json FROM llm_probe_apply_log \
             WHERE run_id NOT LIKE 'bulk:%' ORDER BY applied_at_ms DESC, rowid DESC",
        )
        .map_err(db)?;
    let rows = statement.query_map([], apply_from_row).map_err(db)?;
    for row in rows {
        let record = row.map_err(db)??;
        if provider_id.is_some_and(|id| id != record.provider_id) {
            continue;
        }
        return Ok(undo_available(record.applied_at_ms, now_ms, false).then_some(record));
    }
    Ok(None)
}

/// When the newest still-logged apply of probe results of `provider_id` happened (never a bulk
/// switch): the evidence an apply plans from is what the bench stored after it. An undone apply
/// is no longer logged, so its evidence counts as unapplied again.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure or an unreadable stored report.
pub fn latest_probe_apply_at(
    connection: &Connection,
    provider_id: &str,
) -> Result<Option<i64>, AppError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT apply_id, run_id, applied_at_ms, previous_json FROM llm_probe_apply_log \
             WHERE run_id NOT LIKE 'bulk:%' ORDER BY applied_at_ms DESC, rowid DESC",
        )
        .map_err(db)?;
    let rows = statement.query_map([], apply_from_row).map_err(db)?;
    for row in rows {
        let record = row.map_err(db)??;
        if record.provider_id == provider_id {
            return Ok(Some(record.applied_at_ms));
        }
    }
    Ok(None)
}

/// Removes a logged apply once it was undone, so the menu stops offering it.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub(crate) fn delete_probe_apply(connection: &Connection, apply_id: &str) -> Result<(), AppError> {
    connection
        .execute(
            "DELETE FROM llm_probe_apply_log WHERE apply_id = ?1",
            params![apply_id],
        )
        .map_err(db)?;
    Ok(())
}

/// Writes the `llm_request_log` row (and its `source = probe` usage rollup) of one probe request:
/// the Tests chip of the usage view counts them. The row is booked like a gateway row, priced at
/// the model's effective price and rolled up on the local day of `tz_offset_minutes`. Returns the
/// row id.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn record_probe_request(
    connection: &Connection,
    run_id: &str,
    record: &RequestRecord,
    tz_offset_minutes: i16,
) -> Result<i64, AppError> {
    let provider_name: Option<String> = connection
        .query_row(
            "SELECT name FROM llm_providers WHERE id = ?1",
            params![record.provider_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db)?;
    let mut entry = probe_log_entry(run_id, record, provider_name);
    let usage =
        (entry.input_tokens.is_some() || entry.output_tokens.is_some()).then(|| CostUsage {
            input_tokens: entry.input_tokens.unwrap_or(0),
            output_tokens: entry.output_tokens.unwrap_or(0),
            cached_tokens: entry.cached_tokens.unwrap_or(0),
            ..CostUsage::default()
        });
    let price = super::resolve_price(connection, &record.provider_id, &record.model_id)?;
    let delta = book_entry(&mut entry, &price, usage.as_ref(), tz_offset_minutes);
    insert_log(
        connection,
        &LogWrite {
            entry: &entry,
            capture: None,
            rollup: Some(&delta),
        },
    )
}

/// The log outcome word of one probe request.
fn probe_outcome(request: &ProbeRequest) -> &'static str {
    match request.failure_kind {
        Some(FailureKind::Cancelled) => "canceled",
        Some(_) => "failure",
        None if request
            .status
            .is_some_and(|status| (200..300).contains(&status)) =>
        {
            "success"
        }
        None => "failure",
    }
}

/// The unpriced `source = probe` log row of one probe request of run `run_id`.
fn probe_log_entry(
    run_id: &str,
    record: &RequestRecord,
    provider_name: Option<String>,
) -> RequestLogEntry {
    let request = &record.request;
    RequestLogEntry {
        ts_ms: request.started_ms,
        protocol_in: "probe".to_owned(),
        requested_model: record.model_id.clone(),
        provider_id: Some(record.provider_id.clone()),
        provider_name,
        key_id: Some(record.key_id.clone()),
        upstream_model: Some(record.model_id.clone()),
        protocol_out: Some(request.endpoint.clone()),
        status: request.status.unwrap_or(0),
        outcome: probe_outcome(request).to_owned(),
        stream: request.stream,
        latency_ms: request.latency_ms,
        ttft_ms: request.ttft_ms,
        input_tokens: request.input_tokens,
        output_tokens: request.output_tokens,
        cached_tokens: request.cached_input_tokens,
        error: request.error.clone(),
        source: "probe".to_owned(),
        source_ref: Some(run_id.to_owned()),
        request_id: Some(request.request_id.clone()),
        failure_kind: request.failure_kind,
        ..RequestLogEntry::default()
    }
}
