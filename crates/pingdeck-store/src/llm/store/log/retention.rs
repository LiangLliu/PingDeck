//! Retention of the request log: the product's limits and the one pass that applies them.

use rusqlite::{Connection, params};
use serde::{Deserialize, Serialize};

use crate::llm::store::captures::{prune_capture_age, prune_capture_bytes};
use crate::llm::store::{LOG_ROWS_PER_SOURCE, atomic, db};
use pingdeck_base::error::AppError;
use pingdeck_model::settings::limits::{
    CAPTURE_MAX_BYTES, CAPTURE_MAX_DAYS, LOG_MAX_DAYS, LOG_MAX_ROWS,
};

const MS_PER_DAY: i64 = 86_400_000;

/// Retention limits for [`prune_log`]. They are constants of the product, not settings
/// (`ai.gateway` has no retention fields): [`PruneLimits::default`] is the product's values and
/// the only constructor callers outside tests need.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PruneLimits {
    /// Newest rows kept.
    pub max_rows: u32,
    /// Newest rows of each source kept whatever the others wrote: a long agent run never
    /// pushes the gateway's recent rows out.
    pub per_source_rows: u32,
    /// Rows older than this many days are deleted.
    pub max_days: u16,
    /// Captures older than this many days are deleted (their rows stay, `has_capture = 0`).
    pub max_capture_days: u16,
    /// Captures keep at most this many bytes (`bytes_total`); the oldest go first.
    pub max_capture_bytes: u64,
}

impl Default for PruneLimits {
    fn default() -> Self {
        Self {
            max_rows: LOG_MAX_ROWS,
            per_source_rows: LOG_ROWS_PER_SOURCE,
            max_days: LOG_MAX_DAYS,
            max_capture_days: CAPTURE_MAX_DAYS,
            max_capture_bytes: CAPTURE_MAX_BYTES,
        }
    }
}

/// What one [`prune_log`] deleted.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PruneReport {
    /// Log rows deleted by the row and age limits.
    pub rows_deleted: u64,
    /// Capture rows deleted, with their rows or by the byte cap.
    pub captures_deleted: u64,
}

/// Applies the retention limits atomically (its own `BEGIN IMMEDIATE` transaction, or a savepoint
/// of the caller's transaction): rows older than `max_days` and rows beyond
/// the newest `max_rows` (each source keeping its newest `per_source_rows`) are deleted with their
/// captures; then captures older than `max_capture_days` go, then the oldest ones until
/// `max_capture_bytes` is respected (their rows stay with `has_capture = 0`). The daily rollup is
/// never touched.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn prune_log(
    connection: &Connection,
    limits: &PruneLimits,
    now_ms: i64,
) -> Result<PruneReport, AppError> {
    let cutoff = now_ms.saturating_sub(i64::from(limits.max_days).saturating_mul(MS_PER_DAY));
    atomic(connection, |transaction| {
        let mut rows_deleted = transaction
            .execute(
                "DELETE FROM llm_request_log WHERE ts_ms < ?1",
                params![cutoff],
            )
            .map_err(db)?;
        // Rows beyond each source's floor compete for what the floors leave of `max_rows`, newest
        // first; the oldest of them go.
        rows_deleted += transaction
            .execute(
                "WITH ranked AS (SELECT id, ROW_NUMBER() OVER \
                     (PARTITION BY source ORDER BY id DESC) AS nth FROM llm_request_log), \
                 spare AS (SELECT id, ROW_NUMBER() OVER (ORDER BY id DESC) AS nth \
                     FROM ranked WHERE nth > ?2) \
                 DELETE FROM llm_request_log WHERE id IN (SELECT id FROM spare WHERE nth > \
                     MAX(0, ?1 - (SELECT COUNT(*) FROM ranked WHERE nth <= ?2)))",
                params![
                    i64::from(limits.max_rows),
                    i64::from(limits.per_source_rows)
                ],
            )
            .map_err(db)?;
        // Not every connection enforces foreign keys, so the captures of deleted rows go explicitly.
        let mut captures_deleted = transaction
            .execute(
                "DELETE FROM llm_debug_captures \
                 WHERE request_log_id NOT IN (SELECT id FROM llm_request_log)",
                [],
            )
            .map_err(db)? as u64;
        let capture_cutoff =
            now_ms.saturating_sub(i64::from(limits.max_capture_days).saturating_mul(MS_PER_DAY));
        captures_deleted += prune_capture_age(transaction, capture_cutoff)?;
        captures_deleted += prune_capture_bytes(transaction, limits.max_capture_bytes)?;
        Ok(PruneReport {
            rows_deleted: rows_deleted as u64,
            captures_deleted,
        })
    })
}
