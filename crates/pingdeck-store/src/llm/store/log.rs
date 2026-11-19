//! The request log: one-transaction insert (row, capture, rollup), keyset paging, filtered
//! count/clear and retention pruning (`retention`).
//!
//! Single responsibility: SQL over `llm_request_log`. Retention is not applied per insert: the
//! caller runs [`prune_log`] at startup, periodically by the gateway log writer and hourly, with
//! [`PruneLimits`].

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, Row, params, params_from_iter};

use super::captures::{DELETE_BATCH_ROWS, get_capture, insert_capture};
use super::usage::{add_rollup_latency, upsert_minute, upsert_rollup};
use super::{
    ClearReport, LOG_SOURCES, LogDetail, LogFilter, LogPage, MAX_LOG_PAGE, RequestLogEntry, atomic,
    db, derive_log_reason, flag, json_text, to_i64, to_u64,
};
use crate::llm::capture_record::CapturedExchange;
use crate::llm::failure::FailureKind;
use crate::llm::usage::RollupDelta;
use crate::sqlite::i64_from_usize;
use pingdeck_base::error::AppError;

mod retention;

pub use retention::{PruneLimits, PruneReport, prune_log};

// One column list for the SELECTs and the INSERT: `concat!` needs a literal, so the list is a
// macro. The SELECTs prepend `id`; column order is what `entry_from_row` reads by index.
macro_rules! log_insert_columns {
    () => {
        "ts_ms, protocol_in, requested_model, provider_id, provider_name, key_id, \
         upstream_model, protocol_out, status, outcome, stream, latency_ms, ttft_ms, input_tokens, \
         output_tokens, cached_tokens, attempts_json, error, source, source_ref, request_id, \
         client_id, client_name, peer, alias, translated, reasoning_tokens, cache_write_tokens, \
         tokens_estimated, cost_micro_usd, cost_currency, price_source, failure_kind, \
         upstream_request_id, req_bytes, resp_bytes, timing_json, has_capture, replay_of, \
         session_id, run_id, message_id, purpose, finish_reason, price_basis"
    };
}

const INSERT_LOG_SQL: &str = concat!(
    "INSERT INTO llm_request_log (",
    log_insert_columns!(),
    ") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, \
     ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27, ?28, ?29, ?30, ?31, ?32, ?33, ?34, ?35, ?36, \
     ?37, ?38, ?39, ?40, ?41, ?42, ?43, ?44, ?45)"
);

const LIST_LOG_SQL: &str = concat!(
    "SELECT id, ",
    log_insert_columns!(),
    " FROM llm_request_log \
     WHERE (?1 IS NULL OR id < ?1) AND (?2 IS NULL OR source = ?2) \
       AND (?3 IS NULL OR provider_id = ?3) \
     ORDER BY id DESC LIMIT ?4"
);

const GET_LOG_SQL: &str = concat!(
    "SELECT id, ",
    log_insert_columns!(),
    " FROM llm_request_log WHERE id = ?1"
);

/// Everything one finished request persists, written by [`insert_log`] in one transaction.
#[derive(Debug, Clone, Copy)]
pub struct LogWrite<'a> {
    pub entry: &'a RequestLogEntry,
    /// The redacted capture, when the policy keeps one (`request_log_id` is ignored).
    pub capture: Option<&'a CapturedExchange>,
    /// What the request adds to the daily rollup.
    pub rollup: Option<&'a RollupDelta>,
}

/// `WHERE` terms of a [`LogFilter`] over `?1..?5`; every term is true when its value is NULL.
const FILTER_SQL: &str = "(?1 IS NULL OR source = ?1) AND (?2 IS NULL OR provider_id = ?2) \
     AND (?3 IS NULL OR client_id = ?3) AND (?4 IS NULL OR ts_ms >= ?4) \
     AND (?5 IS NULL \
          OR (?5 = 'success' AND status BETWEEN 200 AND 299 AND outcome <> 'canceled') \
          OR (?5 = 'client_error' AND status BETWEEN 400 AND 499) \
          OR (?5 = 'server_error' AND status BETWEEN 500 AND 599) \
          OR (?5 = 'failed_over' AND CASE WHEN json_valid(attempts_json) \
                  THEN json_array_length(attempts_json) ELSE 0 END > 1) \
          OR (?5 = 'canceled' AND outcome = 'canceled'))";

fn filter_values(filter: &LogFilter) -> [Value; 5] {
    let text = |value: &Option<String>| value.clone().map_or(Value::Null, Value::Text);
    [
        text(&filter.source),
        text(&filter.provider_id),
        text(&filter.client_id),
        filter.since_ms.map_or(Value::Null, Value::Integer),
        filter
            .status_class
            .map_or(Value::Null, |class| Value::Text(class.as_str().to_owned())),
    ]
}

fn entry_from_row(row: &Row<'_>) -> rusqlite::Result<RequestLogEntry> {
    let count = |index: usize| row.get::<_, Option<i64>>(index).map(|v| v.map(to_u64));
    let attempts: String = row.get(17)?;
    let failure: Option<String> = row.get(33)?;
    let timing: Option<String> = row.get(37)?;
    Ok(RequestLogEntry {
        id: row.get(0)?,
        ts_ms: row.get(1)?,
        protocol_in: row.get(2)?,
        requested_model: row.get(3)?,
        provider_id: row.get(4)?,
        provider_name: row.get(5)?,
        key_id: row.get(6)?,
        upstream_model: row.get(7)?,
        protocol_out: row.get(8)?,
        status: u16::try_from(row.get::<_, i64>(9)?).unwrap_or(0),
        outcome: row.get(10)?,
        stream: row.get::<_, i64>(11)? != 0,
        latency_ms: to_u64(row.get(12)?),
        ttft_ms: count(13)?,
        input_tokens: count(14)?,
        output_tokens: count(15)?,
        cached_tokens: count(16)?,
        attempts: serde_json::from_str(&attempts).unwrap_or(serde_json::Value::Null),
        error: row.get(18)?,
        source: row.get(19)?,
        source_ref: row.get(20)?,
        request_id: row.get(21)?,
        client_id: row.get(22)?,
        client_name: row.get(23)?,
        peer: row.get(24)?,
        alias: row.get(25)?,
        translated: row.get::<_, i64>(26)? != 0,
        reasoning_tokens: count(27)?,
        cache_write_tokens: count(28)?,
        tokens_estimated: row.get::<_, i64>(29)? != 0,
        cost_micro_usd: row.get(30)?,
        cost_currency: row.get(31)?,
        price_source: row.get(32)?,
        failure_kind: failure.as_deref().and_then(FailureKind::parse),
        upstream_request_id: row.get(34)?,
        req_bytes: count(35)?,
        resp_bytes: count(36)?,
        timing: timing.and_then(|text| serde_json::from_str(&text).ok()),
        has_capture: row.get::<_, i64>(38)? != 0,
        replay_of: row.get(39)?,
        session_id: row.get(40)?,
        run_id: row.get(41)?,
        message_id: row.get(42)?,
        purpose: row.get(43)?,
        finish_reason: row.get(44)?,
        price_basis: row.get(45)?,
        retry_after_s: None,
        reason: None,
    })
    .map(|mut entry| {
        derive_log_reason(&mut entry);
        entry
    })
}

fn insert_row(
    connection: &Connection,
    entry: &RequestLogEntry,
    has_capture: bool,
) -> Result<i64, AppError> {
    let timing = entry.timing.as_ref().map(json_text).transpose()?;
    connection
        .prepare_cached(INSERT_LOG_SQL)
        .map_err(db)?
        .execute(params![
            entry.ts_ms,
            entry.protocol_in,
            entry.requested_model,
            entry.provider_id,
            entry.provider_name,
            entry.key_id,
            entry.upstream_model,
            entry.protocol_out,
            i64::from(entry.status),
            entry.outcome,
            flag(entry.stream),
            to_i64(entry.latency_ms),
            entry.ttft_ms.map(to_i64),
            entry.input_tokens.map(to_i64),
            entry.output_tokens.map(to_i64),
            entry.cached_tokens.map(to_i64),
            json_text(&entry.attempts)?,
            entry.error,
            entry.source,
            entry.source_ref,
            entry.request_id,
            entry.client_id,
            entry.client_name,
            entry.peer,
            entry.alias,
            flag(entry.translated),
            entry.reasoning_tokens.map(to_i64),
            entry.cache_write_tokens.map(to_i64),
            flag(entry.tokens_estimated),
            entry.cost_micro_usd,
            entry.cost_currency,
            entry.price_source,
            entry.failure_kind.map(FailureKind::as_str),
            entry.upstream_request_id,
            entry.req_bytes.map(to_i64),
            entry.resp_bytes.map(to_i64),
            timing,
            flag(has_capture),
            entry.replay_of,
            entry.session_id,
            entry.run_id,
            entry.message_id,
            entry.purpose,
            entry.finish_reason,
            entry.price_basis,
        ])
        .map_err(db)?;
    Ok(connection.last_insert_rowid())
}

/// Writes the row, its capture (`has_capture = 1`) and the rollup (daily counters plus the
/// failure counter) atomically (its own `BEGIN IMMEDIATE` transaction, or a savepoint of the
/// caller's transaction); any failure leaves nothing behind.
/// Returns the row id. Nothing is redacted here: the log writer redacts `error`, `attempts` and
/// the capture before it calls this.
///
/// # Errors
/// [`AppError::Validation`] for a `source` outside [`LOG_SOURCES`],
/// [`AppError::Database`] on SQLite failure (a duplicate `request_id` included),
/// [`AppError::Internal`] when a value cannot be serialised.
pub fn insert_log(connection: &Connection, write: &LogWrite<'_>) -> Result<i64, AppError> {
    if !LOG_SOURCES.contains(&write.entry.source.as_str()) {
        return Err(AppError::validation(format!(
            "a log row's source must be gateway, agent, api or probe, not `{}`",
            write.entry.source
        )));
    }
    atomic(connection, |transaction| {
        let id = insert_row(transaction, write.entry, write.capture.is_some())?;
        if let Some(capture) = write.capture {
            insert_capture(transaction, id, capture)?;
        }
        if let Some(rollup) = write.rollup {
            upsert_rollup(transaction, rollup)?;
            add_rollup_latency(transaction, rollup, write.entry.latency_ms)?;
            upsert_minute(
                transaction,
                rollup,
                write.entry.ts_ms,
                write.entry.latency_ms,
            )?;
        }
        Ok(id)
    })
}

/// One page of rows, newest first (keyset on `id`).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn list_log(connection: &Connection, page: &LogPage) -> Result<Vec<RequestLogEntry>, AppError> {
    let limit = page.limit.clamp(1, MAX_LOG_PAGE);
    let mut statement = connection.prepare_cached(LIST_LOG_SQL).map_err(db)?;
    let rows = statement
        .query_map(
            params![
                page.before_id,
                page.source,
                page.provider_id,
                i64_from_usize(limit)
            ],
            entry_from_row,
        )
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// A row with its capture.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn get_log(connection: &Connection, id: i64) -> Result<Option<LogDetail>, AppError> {
    let entry = connection
        .query_row(GET_LOG_SQL, params![id], entry_from_row)
        .optional()
        .map_err(db)?;
    entry
        .map(|entry| {
            let capture = if entry.has_capture {
                get_capture(connection, id)?
            } else {
                None
            };
            Ok(LogDetail { entry, capture })
        })
        .transpose()
}

/// A fresh `rq_<16 hex>` id: the one generator of the gateway, Chat, API-mode runs and probes.
#[must_use]
pub fn new_request_id() -> String {
    pingdeck_base::id::prefixed_hex::<8>("rq")
}

/// The row id of a `request_id` (`rq_<16 hex>`).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn log_id_for_request_id(
    connection: &Connection,
    request_id: &str,
) -> Result<Option<i64>, AppError> {
    connection
        .query_row(
            "SELECT id FROM llm_request_log WHERE request_id = ?1",
            params![request_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db)
}

/// How many rows `filter` selects.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn count_log(connection: &Connection, filter: &LogFilter) -> Result<u64, AppError> {
    let count: i64 = connection
        .query_row(
            &format!("SELECT COUNT(*) FROM llm_request_log WHERE {FILTER_SQL}"),
            params_from_iter(filter_values(filter)),
            |row| row.get(0),
        )
        .map_err(db)?;
    Ok(to_u64(count))
}

/// Deletes the rows `filter` selects with their captures (the default filter clears the log),
/// oldest first, `DELETE_BATCH_ROWS` rows per transaction so the writer is never kept waiting
/// behind one long delete. The daily rollup is never touched.
///
/// Call it outside a transaction: inside one, every batch becomes a savepoint of the caller's
/// transaction and the writer waits for all of them.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure; the batches committed before it stay deleted.
pub fn clear_log(connection: &Connection, filter: &LogFilter) -> Result<ClearReport, AppError> {
    let mut report = ClearReport::default();
    while let Some((deleted, capture_bytes)) =
        atomic(connection, |transaction| clear_batch(transaction, filter))?
    {
        report.rows = report.rows.saturating_add(deleted as u64);
        report.capture_bytes = report.capture_bytes.saturating_add(to_u64(capture_bytes));
    }
    Ok(report)
}

/// Deletes the oldest [`DELETE_BATCH_ROWS`] rows `filter` selects with their captures. Returns
/// how many rows went and the bytes of their captures, `None` when no row is left.
fn clear_batch(
    transaction: &Connection,
    filter: &LogFilter,
) -> Result<Option<(usize, i64)>, AppError> {
    // `?6` is the batch size here and the batch's newest id in the statements below.
    let mut values = filter_values(filter).to_vec();
    values.push(Value::Integer(DELETE_BATCH_ROWS));
    let last: Option<i64> = transaction
        .query_row(
            &format!(
                "SELECT MAX(id) FROM (SELECT id FROM llm_request_log WHERE {FILTER_SQL} \
                 ORDER BY id LIMIT ?6)"
            ),
            params_from_iter(&values),
            |row| row.get(0),
        )
        .map_err(db)?;
    let Some(last) = last else {
        return Ok(None);
    };
    values[5] = Value::Integer(last);
    let capture_bytes: i64 = transaction
        .query_row(
            &format!(
                "SELECT COALESCE(SUM(bytes_total), 0) FROM llm_debug_captures \
                 WHERE request_log_id IN \
                 (SELECT id FROM llm_request_log WHERE {FILTER_SQL} AND id <= ?6)"
            ),
            params_from_iter(&values),
            |row| row.get(0),
        )
        .map_err(db)?;
    transaction
        .execute(
            &format!(
                "DELETE FROM llm_debug_captures WHERE request_log_id IN \
                 (SELECT id FROM llm_request_log WHERE {FILTER_SQL} AND id <= ?6)"
            ),
            params_from_iter(&values),
        )
        .map_err(db)?;
    let deleted = transaction
        .execute(
            &format!("DELETE FROM llm_request_log WHERE {FILTER_SQL} AND id <= ?6"),
            params_from_iter(&values),
        )
        .map_err(db)?;
    Ok(Some((deleted, capture_bytes)))
}

#[cfg(test)]
mod tests;
