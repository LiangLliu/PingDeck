//! Debug captures (`llm_debug_captures`): persist, read, byte-cap pruning and body search
//! (`search`).
//!
//! The capture arrives already redacted and bounded (`llm::capture`); the log writer inserts it
//! through `insert_log` in the transaction of the row it belongs to.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::de::DeserializeOwned;
use serde::{Deserialize, Serialize};

use super::{CaptureStats, atomic, db, from_json, json_text, to_i64, to_u64};
use crate::llm::capture_record::{
    CapturedBody, CapturedExchange, CapturedFrames, RedactedHeader, TimingJson, TranslationNote,
};
use crate::llm::wire::Protocol;
use spanbay_base::error::AppError;

mod search;

pub use search::{
    MAX_SEARCH_RESULTS, MAX_SEARCH_ROWS, SEARCH_TIME_BUDGET, SearchBudget, search_log_bodies,
};

// One column list for the SELECT and the INSERT: `concat!` needs a literal, so the list is a
// macro. Column order is what `capture_from_row` reads by index.
macro_rules! capture_columns {
    () => {
        "request_log_id, ts_ms, client_protocol, client_headers_json, client_body_text, \
         hub_request_json, upstream_url, upstream_headers_json, upstream_req_body_text, \
         upstream_status, upstream_resp_headers_json, upstream_resp_body_text, \
         client_resp_headers_json, client_resp_body_text, upstream_frames_json, \
         client_frames_json, translation_notes_json, phase_timings_json, bytes_total, \
         redacted_count, body_meta_json"
    };
}

const INSERT_CAPTURE_SQL: &str = concat!(
    "INSERT OR REPLACE INTO llm_debug_captures (",
    capture_columns!(),
    ") VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, \
     ?19, ?20, ?21)"
);

const GET_CAPTURE_SQL: &str = concat!(
    "SELECT ",
    capture_columns!(),
    " FROM llm_debug_captures WHERE request_log_id = ?1"
);

/// What a stored body keeps besides its text; a present slot reads back as `Some(CapturedBody)`.
#[derive(Debug, Default, Serialize, Deserialize)]
struct BodyMeta {
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_req: Option<SlotMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    upstream_req: Option<SlotMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    upstream_resp: Option<SlotMeta>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    client_resp: Option<SlotMeta>,
}

#[derive(Debug, Serialize, Deserialize)]
struct SlotMeta {
    truncated: bool,
    elided: bool,
    original_bytes: u64,
}

impl SlotMeta {
    fn of(body: Option<&CapturedBody>) -> Option<Self> {
        body.map(|body| Self {
            truncated: body.truncated,
            elided: body.elided,
            original_bytes: body.original_bytes,
        })
    }

    fn body(slot: Option<Self>, text: Option<String>) -> Option<CapturedBody> {
        slot.map(|meta| CapturedBody {
            text,
            truncated: meta.truncated,
            elided: meta.elided,
            original_bytes: meta.original_bytes,
        })
    }
}

fn text_of(body: Option<&CapturedBody>) -> Option<&str> {
    body.and_then(|body| body.text.as_deref())
}

fn optional_json<T: Serialize>(value: Option<&T>) -> Result<Option<String>, AppError> {
    value.map(json_text).transpose()
}

fn parse_optional<T: DeserializeOwned>(
    text: Option<&str>,
    what: &str,
) -> Result<Option<T>, AppError> {
    text.map(|text| from_json(text, what)).transpose()
}

/// Writes `capture` as the capture of log row `log_id` (replacing a previous one). The caller
/// sets `has_capture` on the row; `capture.request_log_id` is ignored.
pub(super) fn insert_capture(
    connection: &Connection,
    log_id: i64,
    capture: &CapturedExchange,
) -> Result<(), AppError> {
    let meta = BodyMeta {
        client_req: SlotMeta::of(capture.client_req_body.as_ref()),
        upstream_req: SlotMeta::of(capture.upstream_req_body.as_ref()),
        upstream_resp: SlotMeta::of(capture.upstream_resp_body.as_ref()),
        client_resp: SlotMeta::of(capture.client_resp_body.as_ref()),
    };
    connection
        .prepare_cached(INSERT_CAPTURE_SQL)
        .map_err(db)?
        .execute(params![
            log_id,
            capture.ts_ms,
            capture.client_protocol.as_str(),
            json_text(&capture.client_req_headers)?,
            text_of(capture.client_req_body.as_ref()),
            optional_json(capture.hub_request.as_ref())?,
            capture.upstream_url,
            json_text(&capture.upstream_req_headers)?,
            text_of(capture.upstream_req_body.as_ref()),
            capture.upstream_status.map(i64::from),
            json_text(&capture.upstream_resp_headers)?,
            text_of(capture.upstream_resp_body.as_ref()),
            json_text(&capture.client_resp_headers)?,
            text_of(capture.client_resp_body.as_ref()),
            optional_json(capture.upstream_frames.as_ref())?,
            optional_json(capture.client_frames.as_ref())?,
            json_text(&capture.translation_notes)?,
            json_text(&capture.timing)?,
            to_i64(capture.bytes_total),
            i64::from(capture.redacted_count),
            json_text(&meta)?,
        ])
        .map_err(db)?;
    Ok(())
}

fn capture_from_row(row: &Row<'_>) -> Result<CapturedExchange, AppError> {
    let protocol: String = row.get(2).map_err(db)?;
    let client_protocol = Protocol::parse(&protocol).ok_or_else(|| {
        AppError::internal(format!("stored capture has unknown protocol `{protocol}`"))
    })?;
    let headers = |index: usize, what: &str| -> Result<Vec<RedactedHeader>, AppError> {
        let text: Option<String> = row.get(index).map_err(db)?;
        Ok(parse_optional(text.as_deref(), what)?.unwrap_or_default())
    };
    let frames = |index: usize, what: &str| -> Result<Option<CapturedFrames>, AppError> {
        let text: Option<String> = row.get(index).map_err(db)?;
        parse_optional(text.as_deref(), what)
    };
    let meta_text: Option<String> = row.get(20).map_err(db)?;
    let meta: BodyMeta =
        parse_optional(meta_text.as_deref(), "capture body metadata")?.unwrap_or_default();
    let hub: Option<String> = row.get(5).map_err(db)?;
    let notes: Option<String> = row.get(16).map_err(db)?;
    let timing: String = row.get(17).map_err(db)?;
    Ok(CapturedExchange {
        request_log_id: row.get(0).map_err(db)?,
        ts_ms: row.get(1).map_err(db)?,
        client_protocol,
        client_req_headers: headers(3, "capture client headers")?,
        client_req_body: SlotMeta::body(meta.client_req, row.get(4).map_err(db)?),
        hub_request: parse_optional(hub.as_deref(), "capture hub request")?,
        upstream_url: row.get(6).map_err(db)?,
        upstream_req_headers: headers(7, "capture upstream headers")?,
        upstream_req_body: SlotMeta::body(meta.upstream_req, row.get(8).map_err(db)?),
        upstream_status: row
            .get::<_, Option<i64>>(9)
            .map_err(db)?
            .and_then(|status| u16::try_from(status).ok()),
        upstream_resp_headers: headers(10, "capture upstream response headers")?,
        upstream_resp_body: SlotMeta::body(meta.upstream_resp, row.get(11).map_err(db)?),
        client_resp_headers: headers(12, "capture client response headers")?,
        client_resp_body: SlotMeta::body(meta.client_resp, row.get(13).map_err(db)?),
        upstream_frames: frames(14, "capture upstream frames")?,
        client_frames: frames(15, "capture client frames")?,
        translation_notes: parse_optional::<Vec<TranslationNote>>(
            notes.as_deref(),
            "capture translation notes",
        )?
        .unwrap_or_default(),
        timing: from_json::<TimingJson>(&timing, "capture timings")?,
        bytes_total: to_u64(row.get(18).map_err(db)?),
        redacted_count: u32::try_from(row.get::<_, i64>(19).map_err(db)?).unwrap_or(u32::MAX),
    })
}

/// The capture of log row `log_id`.
///
/// # Errors
/// [`AppError::Internal`] for a stored capture that no longer parses.
pub fn get_capture(
    connection: &Connection,
    log_id: i64,
) -> Result<Option<CapturedExchange>, AppError> {
    connection
        .query_row(GET_CAPTURE_SQL, params![log_id], |row| {
            Ok(capture_from_row(row))
        })
        .optional()
        .map_err(db)?
        .transpose()
}

/// The capture byte total, read from `idx_llm_debug_captures_bytes` alone.
const BYTES_TOTAL_SQL: &str = "SELECT COALESCE(SUM(bytes_total), 0) FROM llm_debug_captures";

/// Bytes the stored captures occupy (the sum of `bytes_total`), for the capture size cap.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn capture_bytes_total(connection: &Connection) -> Result<u64, AppError> {
    let total: i64 = connection
        .prepare_cached(BYTES_TOTAL_SQL)
        .map_err(db)?
        .query_row([], |row| row.get(0))
        .map_err(db)?;
    Ok(to_u64(total))
}

/// [`capture_stats`]' read; like [`BYTES_TOTAL_SQL`] it needs only the covering index.
const STATS_SQL: &str = "SELECT COUNT(*), COALESCE(SUM(bytes_total), 0) FROM llm_debug_captures";

/// How many captures are stored and the bytes they occupy (what "Delete captures only" frees).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn capture_stats(connection: &Connection) -> Result<CaptureStats, AppError> {
    let (count, bytes): (i64, i64) = connection
        .prepare_cached(STATS_SQL)
        .map_err(db)?
        .query_row([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(db)?;
    Ok(CaptureStats {
        count: to_u64(count),
        bytes: to_u64(bytes),
    })
}

fn clear_orphaned_flags(connection: &Connection) -> Result<(), AppError> {
    connection
        .execute(
            "UPDATE llm_request_log SET has_capture = 0
             WHERE has_capture = 1
               AND id NOT IN (SELECT request_log_id FROM llm_debug_captures)",
            [],
        )
        .map_err(db)?;
    Ok(())
}

/// The over-cap window: the captures, newest first, whose running byte total passes `?1`. The
/// materialized `sizes` makes SQLite read `(bytes_total, rowid)` from the covering index, not the
/// payload pages of the table.
const OVER_CAP_SQL: &str = "DELETE FROM llm_debug_captures WHERE request_log_id IN (
         WITH sizes AS MATERIALIZED (
             SELECT request_log_id, bytes_total FROM llm_debug_captures
         )
         SELECT request_log_id FROM (
             SELECT request_log_id,
                    SUM(bytes_total) OVER (ORDER BY request_log_id DESC) AS running
             FROM sizes
         ) WHERE running > ?1
     )";

/// Deletes the oldest captures until at most `max_bytes` remain (the newest are kept; a single
/// capture bigger than the cap goes too) and clears `has_capture` on their rows. Returns how many
/// captures were deleted. Runs inside the caller's transaction.
pub(super) fn prune_capture_bytes(
    connection: &Connection,
    max_bytes: u64,
) -> Result<u64, AppError> {
    let deleted = connection
        .execute(OVER_CAP_SQL, params![to_i64(max_bytes)])
        .map_err(db)?;
    if deleted > 0 {
        clear_orphaned_flags(connection)?;
    }
    Ok(deleted as u64)
}

/// Deletes the captures made before `cutoff_ms` and clears `has_capture` on their rows (the rows
/// stay). Returns how many were deleted. Runs inside the caller's transaction.
pub(super) fn prune_capture_age(connection: &Connection, cutoff_ms: i64) -> Result<u64, AppError> {
    let deleted = connection
        .execute(
            "DELETE FROM llm_debug_captures WHERE ts_ms < ?1",
            params![cutoff_ms],
        )
        .map_err(db)?;
    if deleted > 0 {
        clear_orphaned_flags(connection)?;
    }
    Ok(deleted as u64)
}

/// Captures one "Delete captures only" transaction removes, so the writer's busy timeout is not
/// held off by one long delete.
pub(super) const DELETE_BATCH_ROWS: i64 = 500;

/// "Delete captures only": removes every capture, keeps the log rows (`has_capture = 0`) and the
/// rollup. Returns the bytes freed (`bytes_total`). Oldest first, `DELETE_BATCH_ROWS` captures
/// per transaction.
///
/// Call it outside a transaction: inside one, every batch becomes a savepoint of the caller's
/// transaction and the writer waits for all of them.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure; the batches committed before it stay deleted.
pub fn delete_captures(connection: &Connection) -> Result<u64, AppError> {
    let mut freed: i64 = 0;
    while let Some(batch) = atomic(connection, delete_capture_batch)? {
        freed = freed.saturating_add(batch);
    }
    Ok(to_u64(freed))
}

/// Deletes the oldest [`DELETE_BATCH_ROWS`] captures and clears `has_capture` on their rows.
/// Returns the bytes they held, `None` when no capture is left.
fn delete_capture_batch(transaction: &Connection) -> Result<Option<i64>, AppError> {
    let last: Option<i64> = transaction
        .query_row(
            "SELECT MAX(request_log_id) FROM (
                 SELECT request_log_id FROM llm_debug_captures
                 ORDER BY request_log_id LIMIT ?1)",
            params![DELETE_BATCH_ROWS],
            |row| row.get(0),
        )
        .map_err(db)?;
    let Some(last) = last else {
        return Ok(None);
    };
    let batch: i64 = transaction
        .query_row(
            "SELECT COALESCE(SUM(bytes_total), 0) FROM llm_debug_captures
             WHERE request_log_id <= ?1",
            params![last],
            |row| row.get(0),
        )
        .map_err(db)?;
    transaction
        .execute(
            "DELETE FROM llm_debug_captures WHERE request_log_id <= ?1",
            params![last],
        )
        .map_err(db)?;
    transaction
        .execute(
            "UPDATE llm_request_log SET has_capture = 0 WHERE has_capture = 1 AND id <= ?1",
            params![last],
        )
        .map_err(db)?;
    Ok(Some(batch))
}

/// Deletes the capture of one row (the row stays, `has_capture = 0`) atomically (its own
/// `BEGIN IMMEDIATE` transaction, or a savepoint of the caller's transaction). Returns the bytes
/// freed, 0 when the row had none.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure (nothing is deleted).
pub fn delete_capture(connection: &Connection, log_id: i64) -> Result<u64, AppError> {
    atomic(connection, |transaction| {
        let freed: i64 = transaction
            .query_row(
                "SELECT COALESCE(SUM(bytes_total), 0) FROM llm_debug_captures WHERE request_log_id = ?1",
                params![log_id],
                |row| row.get(0),
            )
            .map_err(db)?;
        transaction
            .execute(
                "DELETE FROM llm_debug_captures WHERE request_log_id = ?1",
                params![log_id],
            )
            .map_err(db)?;
        transaction
            .execute(
                "UPDATE llm_request_log SET has_capture = 0 WHERE id = ?1",
                params![log_id],
            )
            .map_err(db)?;
        Ok(to_u64(freed))
    })
}

#[cfg(test)]
mod tests;
