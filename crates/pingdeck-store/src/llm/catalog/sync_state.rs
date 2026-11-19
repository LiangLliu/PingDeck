//! The sync schedule and provenance of the catalog (`model_catalog_sync`): the stored state, the
//! outcome of the last attempt, the automatic interval, and the status views built from them.

use rusqlite::{Connection, OptionalExtension, Row, params};
use serde::{Deserialize, Serialize};

use pingdeck_base::error::AppError;
use pingdeck_model::model_catalog::{
    CatalogOrigin, DEFAULT_SYNC_INTERVAL_DAYS, MODELS_DEV_URL, ModelCatalogStatus,
    ModelCatalogSyncReport, SYNC_INTERVAL_DAYS,
};

use super::{db_error, models_dev};
use crate::sqlite::{to_i64, to_u32, to_u64};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;
/// After a failed attempt, the automatic sync waits this long before it tries again.
const FAILED_ATTEMPT_BACKOFF_MS: i64 = 6 * 60 * 60 * 1000;
/// A failure reason is stored cut to this many characters.
const MAX_ERROR_CHARS: usize = 500;

/// Whether and from where the catalog holds rows; read by the start-up readiness check. The full
/// picture is [`ModelCatalogStatus`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CatalogStatus {
    /// `bundled` until a sync succeeded, then `models.dev`.
    pub source: String,
    /// When the rows were last fetched (the snapshot date while bundled), Unix milliseconds.
    pub fetched_at_ms: i64,
    pub entry_count: u64,
}

/// The stored sync state (`model_catalog_sync`), or its defaults when no row exists yet.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SyncState {
    /// Bundled snapshot until a sync succeeded.
    pub origin: CatalogOrigin,
    /// Date of the bundled snapshot, Unix ms.
    pub snapshot_at_ms: i64,
    pub last_success_ms: Option<i64>,
    /// The last attempt, successful or not.
    pub last_attempt_ms: Option<i64>,
    /// Why the last attempt failed; `None` after a success.
    pub last_error: Option<String>,
    /// The last successful sync's report, `None` before the first.
    pub last_report: Option<ModelCatalogSyncReport>,
    pub auto_interval_days: u16,
}

impl Default for SyncState {
    fn default() -> Self {
        Self {
            origin: CatalogOrigin::Builtin,
            snapshot_at_ms: 0,
            last_success_ms: None,
            last_attempt_ms: None,
            last_error: None,
            last_report: None,
            auto_interval_days: DEFAULT_SYNC_INTERVAL_DAYS,
        }
    }
}

impl SyncState {
    /// When the next automatic sync is due: one interval after the last success (or after the
    /// snapshot date before the first), and not before the backoff of a failed attempt ended.
    #[must_use]
    pub fn next_due_ms(&self) -> i64 {
        let base = self.last_success_ms.unwrap_or(self.snapshot_at_ms);
        let interval = base.saturating_add(i64::from(self.auto_interval_days) * DAY_MS);
        let backoff = match (&self.last_error, self.last_attempt_ms) {
            (Some(_), Some(at)) => at.saturating_add(FAILED_ATTEMPT_BACKOFF_MS),
            _ => 0,
        };
        interval.max(backoff)
    }
}

/// Maps the `model_catalog_sync` row selected by [`sync_state`].
fn read_sync_state(row: &Row<'_>) -> rusqlite::Result<SyncState> {
    let last_success_ms: Option<i64> = row.get(2)?;
    Ok(SyncState {
        origin: CatalogOrigin::parse(&row.get::<_, String>(0)?).unwrap_or(CatalogOrigin::Builtin),
        snapshot_at_ms: row.get(1)?,
        last_success_ms,
        last_attempt_ms: row.get(3)?,
        last_error: row.get(4)?,
        last_report: match last_success_ms {
            Some(finished_at_ms) => Some(ModelCatalogSyncReport {
                added: to_u32(row.get(5)?),
                updated: to_u32(row.get(6)?),
                unchanged: to_u32(row.get(7)?),
                total_remote: to_u32(row.get(8)?),
                finished_at_ms,
                duration_ms: to_u64(row.get(9)?),
            }),
            None => None,
        },
        auto_interval_days: u16::try_from(row.get::<_, i64>(10)?)
            .ok()
            .filter(|days| SYNC_INTERVAL_DAYS.contains(days))
            .unwrap_or(DEFAULT_SYNC_INTERVAL_DAYS),
    })
}

/// The stored sync state, or [`SyncState::default`] before anything was stored.
///
/// # Errors
/// [`AppError::database`] on any `SQLite` failure.
pub(super) fn sync_state(conn: &Connection) -> Result<SyncState, AppError> {
    let read = || -> rusqlite::Result<Option<SyncState>> {
        conn.prepare_cached(
            "SELECT origin, snapshot_at_ms, last_success_ms, last_attempt_ms, last_error,
                    last_added, last_updated, last_unchanged, last_total_remote,
                    last_duration_ms, auto_interval_days
             FROM model_catalog_sync WHERE id = 1",
        )?
        .query_row([], read_sync_state)
        .optional()
    };
    Ok(read().map_err(db_error)?.unwrap_or_default())
}

/// Records a successful sync and its `report` (inside the transaction of the sync).
pub(super) fn record_success(
    tx: &Connection,
    report: &ModelCatalogSyncReport,
) -> rusqlite::Result<()> {
    tx.execute(
        "INSERT INTO model_catalog_sync (id, origin, last_success_ms, last_attempt_ms,
             last_error, last_added, last_updated, last_unchanged, last_total_remote,
             last_duration_ms)
         VALUES (1, 'models.dev', ?1, ?1, NULL, ?2, ?3, ?4, ?5, ?6)
         ON CONFLICT (id) DO UPDATE SET origin = 'models.dev',
             last_success_ms = excluded.last_success_ms,
             last_attempt_ms = excluded.last_attempt_ms, last_error = NULL,
             last_added = excluded.last_added, last_updated = excluded.last_updated,
             last_unchanged = excluded.last_unchanged,
             last_total_remote = excluded.last_total_remote,
             last_duration_ms = excluded.last_duration_ms",
        params![
            report.finished_at_ms,
            report.added,
            report.updated,
            report.unchanged,
            report.total_remote,
            to_i64(report.duration_ms),
        ],
    )?;
    Ok(())
}

/// Records a failed attempt (time and reason); nothing else changes.
///
/// # Errors
/// [`AppError::database`] on any `SQLite` failure.
pub fn record_failure(conn: &Connection, message: &str, now_ms: i64) -> Result<(), AppError> {
    let message: String = message.chars().take(MAX_ERROR_CHARS).collect();
    conn.execute(
        "INSERT INTO model_catalog_sync (id, last_attempt_ms, last_error) VALUES (1, ?1, ?2)
         ON CONFLICT (id) DO UPDATE SET last_attempt_ms = excluded.last_attempt_ms,
             last_error = excluded.last_error",
        params![now_ms, message],
    )
    .map(|_| ())
    .map_err(db_error)
}

/// Saves the automatic sync interval.
///
/// # Errors
/// [`AppError::validation`] unless `days` is 1, 7 or 30.
pub fn save_config(conn: &Connection, days: u16) -> Result<(), AppError> {
    if !SYNC_INTERVAL_DAYS.contains(&days) {
        return Err(AppError::validation(
            "The sync interval must be 1, 7 or 30 days",
        ));
    }
    conn.execute(
        "INSERT INTO model_catalog_sync (id, auto_interval_days) VALUES (1, ?1)
         ON CONFLICT (id) DO UPDATE SET auto_interval_days = excluded.auto_interval_days",
        params![days],
    )
    .map(|_| ())
    .map_err(db_error)
}

/// Whether the automatic sync is due at `now_ms` (`SyncState::next_due_ms`).
///
/// # Errors
/// [`AppError::database`] on any `SQLite` failure.
pub fn auto_sync_due(conn: &Connection, now_ms: i64) -> Result<bool, AppError> {
    Ok(now_ms >= sync_state(conn)?.next_due_ms())
}

/// The full catalog status; `auto_sync` is the live value of the setting
/// `ai.providers.catalog_auto_refresh`.
///
/// # Errors
/// [`AppError::database`] on any `SQLite` failure.
pub fn sync_status(conn: &Connection, auto_sync: bool) -> Result<ModelCatalogStatus, AppError> {
    let state = sync_state(conn)?;
    let (models_dev, user): (i64, i64) = conn
        .prepare_cached(
            "SELECT COALESCE(SUM(source = 'models.dev'), 0), COALESCE(SUM(source = 'user'), 0)
             FROM model_catalog",
        )
        .and_then(|mut statement| statement.query_row([], |row| Ok((row.get(0)?, row.get(1)?))))
        .map_err(db_error)?;
    let error = state.last_error.as_deref().map(models_dev::error_kind);
    Ok(ModelCatalogStatus {
        origin: state.origin,
        source_url: MODELS_DEV_URL.to_owned(),
        snapshot_at_ms: state.snapshot_at_ms,
        entry_count: to_u32(models_dev + user),
        models_dev_count: to_u32(models_dev),
        user_count: to_u32(user),
        last_success_ms: state.last_success_ms,
        last_attempt_ms: state.last_attempt_ms,
        last_error: state.last_error.clone(),
        last_error_kind: error.map(|(kind, _)| kind),
        last_error_http_status: error.and_then(|(_, code)| code),
        last_report: state.last_report,
        auto_sync,
        auto_interval_days: state.auto_interval_days,
        next_due_ms: auto_sync.then(|| state.next_due_ms()),
    })
}

/// The stored catalog's provenance and size; `None` when it holds nothing.
///
/// # Errors
/// [`AppError::database`] on any `SQLite` failure.
pub fn status(conn: &Connection) -> Result<Option<CatalogStatus>, AppError> {
    let entries: i64 = conn
        .prepare_cached("SELECT COUNT(*) FROM model_catalog")
        .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
        .map_err(db_error)?;
    if entries == 0 {
        return Ok(None);
    }
    let state = sync_state(conn)?;
    Ok(Some(CatalogStatus {
        source: match state.origin {
            CatalogOrigin::Builtin => "bundled",
            CatalogOrigin::ModelsDev => "models.dev",
        }
        .to_owned(),
        fetched_at_ms: state.last_success_ms.unwrap_or(state.snapshot_at_ms),
        entry_count: to_u64(entries),
    }))
}
