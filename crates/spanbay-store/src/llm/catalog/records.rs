//! Writes of the models.dev rows: seeding the bundled snapshot (and backfilling what older rows
//! lack) and applying a sync. A sync writes `models.dev` rows only: a `user` row is never touched,
//! and a `model_pricing` row keeps its price (regrouping only recomputes which model it prices).
//! Pricing rows are only read here; `llm::store::pricing` owns their writes.

use std::collections::HashMap;

use rusqlite::{Connection, Statement, params};

use spanbay_base::error::AppError;
use spanbay_model::model_catalog::{CatalogSource, ModelCatalogSyncReport, ModelRecord};

use super::grouping::{needs_regroup, regroup};
use super::sync_state::record_success;
use super::{COLUMNS, db_error, models_dev, row_record};
use crate::sqlite::atomic;

/// What an insert does when the `(family, model_id)` row exists.
#[derive(Clone, Copy)]
pub(super) enum Conflict {
    /// Keep the stored row (seeding).
    Keep,
    /// Overwrite it unless the user owns it (a sync).
    Replace,
    /// Overwrite it (saving a user model, after the owner check).
    ReplaceAny,
}

/// The `INSERT` of one [`COLUMNS`] row, bound by [`insert`].
pub(super) fn insert_sql(conflict: Conflict) -> String {
    let columns = format!(
        "INSERT INTO model_catalog ({COLUMNS})
        VALUES (?1,?2,?3,?4,?5,?6,?7,?8,?9,?10,?11,?12,?13,?14,?15,?16,?17,?18,?19,?20,?21,?22)
        ON CONFLICT (family, model_id)"
    );
    let update = "DO UPDATE SET family_name = excluded.family_name,
          name = excluded.name, kind = excluded.kind, context_tokens = excluded.context_tokens,
          max_output_tokens = excluded.max_output_tokens, reasoning = excluded.reasoning,
          vision = excluded.vision, tools = excluded.tools,
          prompt_caching = excluded.prompt_caching, canonical_id = excluded.canonical_id,
          input_per_mtok = excluded.input_per_mtok, output_per_mtok = excluded.output_per_mtok,
          cache_read_per_mtok = excluded.cache_read_per_mtok,
          cache_write_per_mtok = excluded.cache_write_per_mtok,
          reasoning_per_mtok = excluded.reasoning_per_mtok, currency = excluded.currency,
          synced_at_ms = excluded.synced_at_ms, release_date = excluded.release_date,
          input_modalities = excluded.input_modalities";
    match conflict {
        Conflict::Keep => format!("{columns} DO NOTHING"),
        Conflict::Replace => format!("{columns} {update} WHERE model_catalog.source <> 'user'"),
        Conflict::ReplaceAny => format!("{columns} {update}"),
    }
}

/// Runs a statement of [`insert_sql`] for `record`, binding [`COLUMNS`] in order.
pub(super) fn insert(
    statement: &mut Statement<'_>,
    record: &ModelRecord,
) -> rusqlite::Result<usize> {
    // Out of `i64` range reads as unknown (`NULL`), not as a saturated limit.
    let limit = |value: Option<u64>| value.and_then(|tokens| i64::try_from(tokens).ok());
    statement.execute(params![
        record.family,
        record.family_name,
        record.model_id,
        record.name,
        record.kind.as_str(),
        limit(record.context_tokens),
        limit(record.max_output_tokens),
        record.reasoning,
        record.vision,
        record.tools,
        record.prompt_caching,
        record.canonical_id,
        record.input_per_mtok,
        record.output_per_mtok,
        record.cache_read_per_mtok,
        record.cache_write_per_mtok,
        record.reasoning_per_mtok,
        record.currency,
        record.source.as_str(),
        record.synced_at_ms,
        record.release_date,
        // `''` for none: `NULL` means "never filled" (see `seed_bundled_if_empty`).
        record.input_modalities.join(","),
    ])
}

/// Stores the bundled snapshot when the catalog holds no `models.dev` row yet (a fresh install);
/// `true` when it did. Otherwise it only backfills missing model keys and input modalities. Rows
/// already there (a user's own) are kept, and so is an existing sync state.
///
/// # Errors
/// [`AppError::internal`] when the bundled snapshot is corrupt; [`AppError::database`] on any
/// `SQLite` failure.
pub fn seed_bundled_if_empty(conn: &Connection) -> Result<bool, AppError> {
    let seeded: bool = conn
        .prepare_cached("SELECT EXISTS (SELECT 1 FROM model_catalog WHERE source = 'models.dev')")
        .and_then(|mut statement| statement.query_row([], |row| row.get(0)))
        .map_err(db_error)?;
    if seeded {
        backfill_model_keys(conn)?;
        backfill_modalities(conn)?;
        return Ok(false);
    }
    seed_snapshot(conn)?;
    Ok(true)
}

/// Gives rows without a model key (and pricing rows without a model) theirs.
fn backfill_model_keys(conn: &Connection) -> Result<(), AppError> {
    if needs_regroup(conn).map_err(db_error)? {
        atomic(conn, regroup).map_err(db_error)?;
    }
    Ok(())
}

/// Fills `NULL` input modalities from the bundled snapshot; rows the snapshot lacks get `''`.
fn backfill_modalities(conn: &Connection) -> Result<(), AppError> {
    if !needs_modalities(conn).map_err(db_error)? {
        return Ok(());
    }
    let snapshot = models_dev::bundled_snapshot()?;
    atomic(conn, |tx| -> rusqlite::Result<()> {
        {
            let mut statement = tx.prepare_cached(
                "UPDATE model_catalog SET input_modalities = ?3
                 WHERE family = ?1 AND model_id = ?2 AND input_modalities IS NULL",
            )?;
            for record in &snapshot.records {
                statement.execute(params![
                    record.family,
                    record.model_id,
                    record.input_modalities.join(",")
                ])?;
            }
        }
        // Rows the snapshot lacks stay unknown until a sync, without asking again.
        tx.execute(
            "UPDATE model_catalog SET input_modalities = ''
             WHERE source = 'models.dev' AND input_modalities IS NULL",
            [],
        )?;
        Ok(())
    })
    .map_err(db_error)
}

/// Inserts every snapshot row (keeping rows already there) and the builtin sync state.
fn seed_snapshot(conn: &Connection) -> Result<(), AppError> {
    let snapshot = models_dev::bundled_snapshot()?;
    atomic(conn, |tx| -> rusqlite::Result<()> {
        {
            let mut statement = tx.prepare_cached(&insert_sql(Conflict::Keep))?;
            for record in &snapshot.records {
                insert(&mut statement, record)?;
            }
        }
        tx.execute(
            "INSERT OR IGNORE INTO model_catalog_sync (id, origin, snapshot_at_ms)
             VALUES (1, 'builtin', ?1)",
            params![snapshot.snapshot_at_ms.unwrap_or_default()],
        )?;
        regroup(tx)
    })
    .map_err(db_error)
}

fn needs_modalities(conn: &Connection) -> rusqlite::Result<bool> {
    conn.prepare_cached(
        "SELECT EXISTS (SELECT 1 FROM model_catalog
         WHERE source = 'models.dev' AND input_modalities IS NULL)",
    )?
    .query_row([], |row| row.get(0))
}

/// Equal ignoring the sync stamp (`synced_at_ms`).
fn same_content(old: &ModelRecord, new: &ModelRecord) -> bool {
    old.source == new.source
        && ModelRecord {
            synced_at_ms: new.synced_at_ms,
            ..old.clone()
        } == *new
}

/// Applies a models.dev download in one transaction and records the success: new rows are added,
/// `models.dev` rows whose content changed are updated, the rest is counted unchanged. A `user`
/// row is never overwritten (it counts as unchanged); rows no longer upstream are kept;
/// `model_pricing` is not touched. A failure changes nothing.
///
/// # Errors
/// [`AppError::database`] on any `SQLite` failure.
pub fn apply_sync(
    conn: &Connection,
    records: &[ModelRecord],
    now_ms: i64,
    duration_ms: u64,
) -> Result<ModelCatalogSyncReport, AppError> {
    atomic(conn, |tx| -> rusqlite::Result<ModelCatalogSyncReport> {
        let existing = load_existing(tx)?;
        let (added, updated, unchanged) = upsert_changed(tx, &existing, records)?;
        let report = ModelCatalogSyncReport {
            added,
            updated,
            unchanged,
            total_remote: u32::try_from(records.len()).unwrap_or(u32::MAX),
            finished_at_ms: now_ms,
            duration_ms,
        };
        record_success(tx, &report)?;
        regroup(tx)?;
        Ok(report)
    })
    .map_err(db_error)
}

/// Every stored row.
fn load_existing(tx: &Connection) -> rusqlite::Result<Vec<ModelRecord>> {
    tx.prepare_cached(&format!("SELECT {COLUMNS} FROM model_catalog"))?
        .query_map([], row_record)?
        .collect()
}

/// Writes the `records` that are new or whose `models.dev` content changed; returns how many were
/// added, updated and left unchanged (a `user` row counts as unchanged).
fn upsert_changed(
    tx: &Connection,
    existing: &[ModelRecord],
    records: &[ModelRecord],
) -> rusqlite::Result<(u32, u32, u32)> {
    let existing: HashMap<(&str, &str), &ModelRecord> = existing
        .iter()
        .map(|record| ((record.family.as_str(), record.model_id.as_str()), record))
        .collect();
    let (mut added, mut updated, mut unchanged) = (0_u32, 0_u32, 0_u32);
    let mut statement = tx.prepare_cached(&insert_sql(Conflict::Replace))?;
    for record in records {
        match existing.get(&(record.family.as_str(), record.model_id.as_str())) {
            Some(old) if old.source == CatalogSource::User || same_content(old, record) => {
                unchanged += 1;
            }
            Some(_) => {
                insert(&mut statement, record)?;
                updated += 1;
            }
            None => {
                insert(&mut statement, record)?;
                added += 1;
            }
        }
    }
    Ok((added, updated, unchanged))
}
