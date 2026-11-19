//! `model_alias` rows: an unlisted id of a provider mapped to a catalog model, and the rekey of
//! pricing rows it triggers.

use rusqlite::{Connection, params};

use crate::llm::catalog::rekey_pricing;
use crate::llm::store::{atomic, db};
use pingdeck_base::error::AppError;
use pingdeck_model::model_catalog::{MAX_ID_CHARS, ModelAlias, UNLISTED_PREFIX};

/// Every alias.
///
/// # Errors
/// [`AppError::Database`] on `SQLite` failure.
pub(crate) fn list_aliases(connection: &Connection) -> Result<Vec<ModelAlias>, AppError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT provider_id, model_id, model_key, updated_at_ms FROM model_alias \
             ORDER BY provider_id, model_id",
        )
        .map_err(db)?;
    let rows = statement
        .query_map([], |row| {
            Ok(ModelAlias {
                provider_id: row.get(0)?,
                model_id: row.get(1)?,
                model_key: row.get(2)?,
                updated_at_ms: row.get(3)?,
            })
        })
        .map_err(db)?;
    rows.collect::<rusqlite::Result<_>>().map_err(db)
}

fn check_alias_id(label: &str, value: &str) -> Result<(), AppError> {
    if value.is_empty() || value.chars().count() > MAX_ID_CHARS {
        return Err(AppError::validation(format!(
            "{label} must be 1 to {MAX_ID_CHARS} characters"
        )));
    }
    Ok(())
}

/// Saves that `alias.model_id` of `alias.provider_id` is the catalog model `alias.model_key`,
/// and moves that id's pricing rows to it. Returns the stored alias.
///
/// # Errors
/// [`AppError::Validation`] for an empty id or a key the catalog does not list,
/// [`AppError::Database`] on `SQLite` failure.
pub fn save_alias(
    connection: &Connection,
    alias: ModelAlias,
    now_ms: i64,
) -> Result<ModelAlias, AppError> {
    let alias = ModelAlias {
        provider_id: alias.provider_id.trim().to_owned(),
        model_id: alias.model_id.trim().to_owned(),
        model_key: alias.model_key.trim().to_owned(),
        updated_at_ms: now_ms,
    };
    check_alias_id("Provider", &alias.provider_id)?;
    check_alias_id("Model id", &alias.model_id)?;
    let listed: bool = connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM model_catalog WHERE model_key = ?1)",
            [&alias.model_key],
            |row| row.get(0),
        )
        .map_err(db)?;
    if !listed || alias.model_key.starts_with(UNLISTED_PREFIX) {
        return Err(AppError::validation(format!(
            "The model catalog has no model {}",
            alias.model_key
        )));
    }
    atomic(connection, |tx| {
        tx.execute(
            "INSERT INTO model_alias (provider_id, model_id, model_key, updated_at_ms) \
             VALUES (?1, ?2, ?3, ?4) \
             ON CONFLICT (provider_id, model_id) DO UPDATE SET model_key = excluded.model_key, \
                 updated_at_ms = excluded.updated_at_ms",
            params![alias.provider_id, alias.model_id, alias.model_key, now_ms],
        )
        .map_err(db)?;
        rekey_pricing(tx).map_err(db)
    })?;
    Ok(alias)
}

/// Removes an alias (the id is unlisted again, or matches the catalog by itself); `true` when
/// one existed.
///
/// # Errors
/// [`AppError::Database`] on `SQLite` failure.
pub fn delete_alias(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
) -> Result<bool, AppError> {
    atomic(connection, |tx| {
        let deleted = tx
            .execute(
                "DELETE FROM model_alias WHERE provider_id = ?1 AND model_id = ?2",
                params![provider_id, model_id],
            )
            .map_err(db)?;
        rekey_pricing(tx).map_err(db)?;
        Ok(deleted > 0)
    })
}
