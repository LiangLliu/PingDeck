//! Persisted probe outcomes: SQL over `llm_probe_results` (latest per `(key, model, endpoint,
//! test)`) and `llm_probe_history` (every result, pruned to the newest [`HISTORY_RUNS_PER_PAIR`]
//! runs per key and model pair), plus the one-transaction write of a finished cell. Runs and the
//! apply log live in `probe_runs`.

use rusqlite::{Connection, Row, params};

use super::{ProbeResult, atomic, db, enum_text, flag, json_text, to_i64, to_u64};
use crate::llm::failure::FailureKind;
use crate::llm::probe::run::CellRecord;
use crate::llm::probe::{ProbeStatus, ProbeTest};
use crate::sqlite::i64_from_usize;
use pingdeck_base::error::AppError;

/// Runs of one key and model pair that [`persist_cell`] keeps in the history.
pub const HISTORY_RUNS_PER_PAIR: usize = 25;

/// The status word to store for `result`: its own when set, else derived. A result
/// that derives no status is a cancelled check: it reads as skipped.
fn status_word_of(result: &ProbeResult) -> String {
    if result.status_word.is_empty() {
        ProbeStatus::from_result(result)
            .unwrap_or(ProbeStatus::Skipped)
            .as_str()
            .to_owned()
    } else {
        result.status_word.clone()
    }
}

/// Columns of a result row, shared by the latest and history tables; `result_from_row` reads
/// them by position.
const RESULT_COLUMNS: &str = "key_id, model_id, endpoint, test, ok, status, latency_ms, ttft_ms, tps, \
     input_tokens, output_tokens, failure_kind, message, detail_json, checked_at_ms, run_id, \
     status_word, skip_reason";

fn result_from_row(row: &Row<'_>) -> rusqlite::Result<ProbeResult> {
    let status: Option<i64> = row.get(5)?;
    let kind: Option<String> = row.get(11)?;
    let detail: Option<String> = row.get(13)?;
    let mut detail: serde_json::Value = detail
        .and_then(|text| serde_json::from_str(&text).ok())
        .unwrap_or(serde_json::Value::Null);
    let request_id = take_request_id(&mut detail);
    Ok(ProbeResult {
        key_id: row.get(0)?,
        model_id: row.get(1)?,
        endpoint: row.get(2)?,
        test: row.get(3)?,
        ok: row.get::<_, i64>(4)? != 0,
        status: status.and_then(|value| u16::try_from(value).ok()),
        latency_ms: row.get::<_, Option<i64>>(6)?.map(to_u64),
        ttft_ms: row.get::<_, Option<i64>>(7)?.map(to_u64),
        tps: row.get(8)?,
        input_tokens: row.get::<_, Option<i64>>(9)?.map(to_u64),
        output_tokens: row.get::<_, Option<i64>>(10)?.map(to_u64),
        failure_kind: kind.as_deref().and_then(FailureKind::parse),
        message: row.get(12)?,
        detail,
        checked_at_ms: row.get(14)?,
        run_id: row.get(15)?,
        status_word: row.get(16)?,
        skip_reason: row.get(17)?,
        request_id,
    })
}

/// Removes `request_id` from a stored detail object (an object left empty reads back as null).
fn take_request_id(detail: &mut serde_json::Value) -> Option<String> {
    let object = detail.as_object_mut()?;
    let id = object.remove("request_id")?;
    if object.is_empty() {
        *detail = serde_json::Value::Null;
    }
    id.as_str().map(str::to_owned)
}

/// The `detail_json` text of `result`: its detail with the request id folded in.
fn detail_text(result: &ProbeResult) -> Result<String, AppError> {
    let Some(id) = &result.request_id else {
        return json_text(&result.detail);
    };
    let mut detail = result.detail.clone();
    if detail.is_null() {
        detail = serde_json::json!({});
    }
    // Only an object detail can carry the id, as every probe's detail is one (or null).
    if let Some(object) = detail.as_object_mut() {
        object.insert(
            "request_id".to_owned(),
            serde_json::Value::from(id.as_str()),
        );
    }
    json_text(&detail)
}

fn failure_text(result: &ProbeResult) -> Option<&'static str> {
    result.failure_kind.map(FailureKind::as_str)
}

/// Stores a probe result, replacing the previous one of the same `(key, model, endpoint, test)`.
/// It does not touch the history, the capability matrix or any run: [`persist_cell`] writes the
/// history of a run's cell.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure (an unknown key violates the foreign key).
pub fn save_probe_result(connection: &Connection, result: &ProbeResult) -> Result<(), AppError> {
    connection
        .execute(
            &format!(
                "INSERT OR REPLACE INTO llm_probe_results ({RESULT_COLUMNS}) \
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)"
            ),
            params![
                result.key_id,
                result.model_id,
                result.endpoint,
                result.test,
                flag(result.ok),
                result.status.map(i64::from),
                result.latency_ms.map(to_i64),
                result.ttft_ms.map(to_i64),
                result.tps,
                result.input_tokens.map(to_i64),
                result.output_tokens.map(to_i64),
                failure_text(result),
                result.message,
                detail_text(result)?,
                result.checked_at_ms,
                result.run_id,
                status_word_of(result),
                result.skip_reason,
            ],
        )
        .map_err(db)?;
    Ok(())
}

/// Probe results of `key_id`, or all of them when `None`, newest first.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn list_probe_results(
    connection: &Connection,
    key_id: Option<&str>,
) -> Result<Vec<ProbeResult>, AppError> {
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {RESULT_COLUMNS} FROM llm_probe_results \
             WHERE (?1 IS NULL OR key_id = ?1) \
             ORDER BY checked_at_ms DESC, key_id, model_id, endpoint, test"
        ))
        .map_err(db)?;
    let rows = statement
        .query_map(params![key_id], result_from_row)
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// The result row a cell stores: the check's own result stamped with the run and the cell's
/// status, or, for a skipped cell (nothing was measured), a row that carries the reason.
fn cell_row(run_id: &str, cell: &CellRecord, now_ms: i64) -> Result<ProbeResult, AppError> {
    let skip_reason = cell.skip.map(|reason| enum_text(&reason)).transpose()?;
    let mut row = cell.result.clone().unwrap_or_else(|| ProbeResult {
        key_id: cell.key_id.clone(),
        model_id: cell.model_id.clone(),
        test: cell.test.as_str().to_owned(),
        message: cell.reason.clone(),
        checked_at_ms: now_ms,
        ..ProbeResult::default()
    });
    row.run_id = Some(run_id.to_owned());
    row.status_word = cell.status.as_str().to_owned();
    row.skip_reason = skip_reason;
    Ok(row)
}

/// Keeps the newest [`HISTORY_RUNS_PER_PAIR`] runs of one key and model pair.
fn prune_history(connection: &Connection, key_id: &str, model_id: &str) -> Result<(), AppError> {
    connection
        .execute(
            "DELETE FROM llm_probe_history WHERE key_id = ?1 AND model_id = ?2 AND run_id NOT IN ( \
                 SELECT run_id FROM llm_probe_history WHERE key_id = ?1 AND model_id = ?2 \
                 GROUP BY run_id ORDER BY MAX(id) DESC LIMIT ?3)",
            params![key_id, model_id, i64_from_usize(HISTORY_RUNS_PER_PAIR)],
        )
        .map_err(db)?;
    Ok(())
}

/// Stores a finished (or skipped) cell of run `run_id` in one transaction:
///
/// 1. the cell becomes the latest of its `(key, model, test)` (a result of another endpoint of
///    the same cell is replaced: the bench shows one cell per check),
/// 2. it is appended to `llm_probe_history` and the pair keeps its newest
///    [`HISTORY_RUNS_PER_PAIR`] runs.
///
/// The capability matrix is not touched: a result is evidence of one key, and routing filters on
/// the matrix, so only "Apply results" records what a run measured.
///
/// `now_ms` stamps a skipped cell, which carries no result (and so no time).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure (an unknown key violates the foreign key).
pub fn persist_cell(
    connection: &Connection,
    run_id: &str,
    cell: &CellRecord,
    now_ms: i64,
) -> Result<(), AppError> {
    let row = cell_row(run_id, cell, now_ms)?;
    // The implicit keyless target has no key row to hang results on: its cells stay live only.
    if super::key_secrets::is_keyless(&row.key_id) {
        return Ok(());
    }
    atomic(connection, |connection| {
        delete_superseded(connection, &row, cell.test)?;
        save_probe_result(connection, &row)?;
        insert_history(connection, run_id, &row)?;
        prune_history(connection, &row.key_id, &row.model_id)
    })
}

/// Chat-family results a model's decisive check supersedes: a model that now runs Decision or
/// Embeddings answers no chat-family check, so rows of an earlier run, from before the provider
/// spoke that protocol, would otherwise keep reporting a failure the bench no longer tests.
const DROP_CHAT_FAMILY_FOR_DECISION_SQL: &str = "DELETE FROM llm_probe_results \
     WHERE key_id = ?1 AND model_id = ?2 \
       AND test NOT IN ('connectivity', 'auth', 'models', 'decision')";

/// [`DROP_CHAT_FAMILY_FOR_DECISION_SQL`] for the Embeddings check.
const DROP_CHAT_FAMILY_FOR_EMBEDDINGS_SQL: &str = "DELETE FROM llm_probe_results \
     WHERE key_id = ?1 AND model_id = ?2 \
       AND test NOT IN ('connectivity', 'auth', 'models', 'embeddings')";

/// Deletes the latest results `row` replaces: the same cell's result of another endpoint, and,
/// for a decisive `test`, the model's chat-family results.
fn delete_superseded(
    connection: &Connection,
    row: &ProbeResult,
    test: ProbeTest,
) -> Result<(), AppError> {
    connection
        .execute(
            "DELETE FROM llm_probe_results \
             WHERE key_id = ?1 AND model_id = ?2 AND test = ?3 AND endpoint <> ?4",
            params![row.key_id, row.model_id, row.test, row.endpoint],
        )
        .map_err(db)?;
    let chat_family = match test {
        ProbeTest::Decision => Some(DROP_CHAT_FAMILY_FOR_DECISION_SQL),
        ProbeTest::Embeddings => Some(DROP_CHAT_FAMILY_FOR_EMBEDDINGS_SQL),
        _ => None,
    };
    if let Some(sql) = chat_family {
        connection
            .execute(sql, params![row.key_id, row.model_id])
            .map_err(db)?;
    }
    Ok(())
}

/// Appends `row` to `llm_probe_history` under run `run_id`.
fn insert_history(
    connection: &Connection,
    run_id: &str,
    row: &ProbeResult,
) -> Result<(), AppError> {
    connection
        .execute(
            "INSERT INTO llm_probe_history (run_id, key_id, model_id, endpoint, test, ok, status, \
                 latency_ms, ttft_ms, tps, input_tokens, output_tokens, failure_kind, message, \
                 detail_json, checked_at_ms, status_word, skip_reason) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18)",
            params![
                run_id,
                row.key_id,
                row.model_id,
                row.endpoint,
                row.test,
                flag(row.ok),
                row.status.map(i64::from),
                // `llm_probe_history.latency_ms` is NOT NULL (unlike the latest table): a result
                // without one stores 0.
                to_i64(row.latency_ms.unwrap_or(0)),
                row.ttft_ms.map(to_i64),
                row.tps,
                row.input_tokens.map(to_i64),
                row.output_tokens.map(to_i64),
                failure_text(row),
                row.message,
                detail_text(row)?,
                row.checked_at_ms,
                row.status_word,
                row.skip_reason,
            ],
        )
        .map_err(db)?;
    Ok(())
}

/// The history of one key and model pair, newest first (`llm_probe_history_list`).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn list_probe_history(
    connection: &Connection,
    key_id: &str,
    model_id: &str,
    limit: usize,
) -> Result<Vec<ProbeResult>, AppError> {
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {RESULT_COLUMNS} FROM llm_probe_history WHERE key_id = ?1 AND model_id = ?2 \
             ORDER BY id DESC LIMIT ?3"
        ))
        .map_err(db)?;
    let rows = statement
        .query_map(
            params![key_id, model_id, i64_from_usize(limit)],
            result_from_row,
        )
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// Every result row of run `run_id`, in the order the cells finished (the compare view and the
/// apply preview read the results of one run).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn probe_run_results(
    connection: &Connection,
    run_id: &str,
) -> Result<Vec<ProbeResult>, AppError> {
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {RESULT_COLUMNS} FROM llm_probe_history WHERE run_id = ?1 ORDER BY id"
        ))
        .map_err(db)?;
    let rows = statement
        .query_map(params![run_id], result_from_row)
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}
