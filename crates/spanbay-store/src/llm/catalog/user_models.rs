//! The user's own models (`source = 'user'`): saving one next to the synced rows and deleting it.

use rusqlite::types::Value;
use rusqlite::{Connection, OptionalExtension, params};

use spanbay_base::error::AppError;
use spanbay_model::model_catalog::{CatalogSource, ModelCatalogRow, ModelRecord, UserModelInput};

use super::db_error;
use super::grouping::regroup;
use super::list::first_row;
use super::records::{Conflict, insert, insert_sql};
use crate::sqlite::atomic_with;

/// Adds or replaces a model of the user's own (`source = 'user'`) and returns its row.
///
/// # Errors
/// [`AppError::validation`] for a bad input, [`AppError::conflict`] when a models.dev row owns
/// that family and model id, [`AppError::database`] on any `SQLite` failure.
pub fn save_user_model(
    conn: &Connection,
    input: UserModelInput,
    now_ms: i64,
) -> Result<ModelCatalogRow, AppError> {
    let input = input.validated()?;
    let key = [
        Value::Text(input.family.clone()),
        Value::Text(input.model_id.clone()),
    ];
    atomic_with(conn, db_error, |tx| -> Result<(), AppError> {
        let owner = owner_source(tx, &input.family, &input.model_id).map_err(db_error)?;
        if owner.as_deref() == Some(CatalogSource::ModelsDev.as_str()) {
            return Err(AppError::conflict(format!(
                "{}/{} is a synced model; pick another family or model id",
                input.family, input.model_id
            )));
        }
        let family_name = family_display_name(tx, &input.family);
        let record = user_record(input, family_name, now_ms);
        // The owner check above ruled a models.dev row out: only a user row can be replaced.
        tx.prepare_cached(&insert_sql(Conflict::ReplaceAny))
            .and_then(|mut statement| insert(&mut statement, &record))
            .map_err(db_error)?;
        regroup(tx).map_err(db_error)
    })?;
    // Read back after the commit: the write lock is not held through the list query, and a
    // failed read leaves the saved row in place.
    first_row(conn, "WHERE c.family = ?1 AND c.model_id = ?2", &key)
        .map_err(db_error)?
        .ok_or_else(|| AppError::internal("the user model vanished right after saving"))
}

/// The `source` of the stored `(family, model_id)` row, `None` when there is none.
fn owner_source(tx: &Connection, family: &str, model_id: &str) -> rusqlite::Result<Option<String>> {
    tx.query_row(
        "SELECT source FROM model_catalog WHERE family = ?1 AND model_id = ?2",
        params![family, model_id],
        |row| row.get(0),
    )
    .optional()
}

/// The provider name some stored row of `family` carries, else `family` itself.
///
/// Long-standing behaviour kept as is: any `SQLite` error also falls back to `family`, and with no
/// `ORDER BY` the row it reads is unspecified when the rows of `family` disagree on the name.
fn family_display_name(tx: &Connection, family: &str) -> String {
    tx.query_row(
        "SELECT family_name FROM model_catalog WHERE family = ?1 LIMIT 1",
        params![family],
        |row| row.get(0),
    )
    .unwrap_or_else(|_| family.to_owned())
}

/// The catalog row of a validated user model.
fn user_record(input: UserModelInput, family_name: String, now_ms: i64) -> ModelRecord {
    ModelRecord {
        family: input.family,
        family_name,
        model_id: input.model_id,
        name: input.name,
        release_date: None,
        kind: input.kind,
        context_tokens: input.context_tokens,
        max_output_tokens: input.max_output_tokens,
        reasoning: input.reasoning,
        vision: input.vision,
        input_modalities: Vec::new(),
        tools: input.tools,
        prompt_caching: input.cache_read_per_mtok.is_some() || input.cache_write_per_mtok.is_some(),
        canonical_id: String::new(),
        input_per_mtok: input.input_per_mtok,
        output_per_mtok: input.output_per_mtok,
        cache_read_per_mtok: input.cache_read_per_mtok,
        cache_write_per_mtok: input.cache_write_per_mtok,
        reasoning_per_mtok: input.reasoning_per_mtok,
        currency: input.currency,
        source: CatalogSource::User,
        synced_at_ms: now_ms,
    }
}

/// Deletes a user's own model; `true` when one existed. A synced row is never deleted here.
///
/// # Errors
/// [`AppError::database`] on any `SQLite` failure.
pub fn delete_user_model(
    conn: &Connection,
    family: &str,
    model_id: &str,
) -> Result<bool, AppError> {
    conn.execute(
        "DELETE FROM model_catalog WHERE family = ?1 AND model_id = ?2 AND source = 'user'",
        params![family, model_id],
    )
    .map(|deleted| deleted > 0)
    .map_err(db_error)
}
