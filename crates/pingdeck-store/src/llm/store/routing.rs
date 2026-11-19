//! Per-model fallback chains, the legacy alias-name map and the routing notices.

use std::collections::HashMap;

use rusqlite::{Connection, params};

use super::{ModelRef, atomic, db};
use crate::sqlite::i64_from_usize;
use pingdeck_base::error::AppError;

/// Longest fallback chain of one model.
pub const MAX_FALLBACKS: usize = 5;

/// An old alias name that still resolves to the model it named (until no recorded client file
/// uses it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LegacyName {
    pub name: String,
    pub target: ModelRef,
}

/// Every stored chain, keyed by its primary model.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn list_fallbacks(
    connection: &Connection,
) -> Result<HashMap<ModelRef, Vec<ModelRef>>, AppError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT provider_id, model_id, target_provider_id, target_model_id \
             FROM llm_model_fallbacks ORDER BY provider_id, model_id, position",
        )
        .map_err(db)?;
    let rows = statement
        .query_map([], |row| {
            Ok((
                ModelRef {
                    provider_id: row.get(0)?,
                    model: row.get(1)?,
                },
                ModelRef {
                    provider_id: row.get(2)?,
                    model: row.get(3)?,
                },
            ))
        })
        .map_err(db)?;
    let mut chains: HashMap<ModelRef, Vec<ModelRef>> = HashMap::new();
    for row in rows {
        let (primary, fallback) = row.map_err(db)?;
        chains.entry(primary).or_default().push(fallback);
    }
    Ok(chains)
}

/// The chain of one model, in order (empty when it has none).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub(super) fn fallbacks_of(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
) -> Result<Vec<ModelRef>, AppError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT target_provider_id, target_model_id FROM llm_model_fallbacks \
             WHERE provider_id = ?1 AND model_id = ?2 ORDER BY position",
        )
        .map_err(db)?;
    let rows = statement
        .query_map(params![provider_id, model_id], |row| {
            Ok(ModelRef {
                provider_id: row.get(0)?,
                model: row.get(1)?,
            })
        })
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// Replaces the chain of `primary`. Checks the shape only (at most [`MAX_FALLBACKS`], no
/// self-reference, no duplicate); call-kind compatibility is the caller's.
///
/// # Errors
/// [`AppError::Validation`] for a bad chain, [`AppError::Database`] otherwise.
pub fn set_model_fallbacks(
    connection: &Connection,
    primary: &ModelRef,
    fallbacks: &[ModelRef],
) -> Result<(), AppError> {
    if fallbacks.len() > MAX_FALLBACKS {
        return Err(AppError::validation(format!(
            "a model has at most {MAX_FALLBACKS} fallbacks"
        )));
    }
    for (index, fallback) in fallbacks.iter().enumerate() {
        if fallback.provider_id.trim().is_empty() || fallback.model.trim().is_empty() {
            return Err(AppError::validation(
                "a fallback needs a provider and a model",
            ));
        }
        if fallback == primary {
            return Err(AppError::validation("a model cannot be its own fallback"));
        }
        if fallbacks[..index].contains(fallback) {
            return Err(AppError::validation(format!(
                "fallback {} is listed twice",
                fallback.id_form()
            )));
        }
    }
    atomic(connection, |connection| {
        connection
            .execute(
                "DELETE FROM llm_model_fallbacks WHERE provider_id = ?1 AND model_id = ?2",
                params![primary.provider_id, primary.model],
            )
            .map_err(db)?;
        let mut insert = connection
            .prepare_cached(
                "INSERT INTO llm_model_fallbacks \
                 (provider_id, model_id, position, target_provider_id, target_model_id) \
                 VALUES (?1, ?2, ?3, ?4, ?5)",
            )
            .map_err(db)?;
        for (position, fallback) in fallbacks.iter().enumerate() {
            insert
                .execute(params![
                    primary.provider_id,
                    primary.model,
                    i64_from_usize(position),
                    fallback.provider_id,
                    fallback.model
                ])
                .map_err(db)?;
        }
        Ok(())
    })
}

/// Every legacy alias name, by name.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn list_legacy_names(connection: &Connection) -> Result<Vec<LegacyName>, AppError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT name, provider_id, model_id FROM llm_model_legacy_names ORDER BY name",
        )
        .map_err(db)?;
    let rows = statement
        .query_map([], |row| {
            Ok(LegacyName {
                name: row.get(0)?,
                target: ModelRef {
                    provider_id: row.get(1)?,
                    model: row.get(2)?,
                },
            })
        })
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// Records a legacy name; for test setups, the app never creates them.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure (an unknown provider violates the foreign key).
#[cfg(any(test, feature = "test-support"))]
pub fn save_legacy_name(
    connection: &Connection,
    name: &str,
    target: &ModelRef,
) -> Result<(), AppError> {
    connection
        .execute(
            "INSERT OR REPLACE INTO llm_model_legacy_names (name, provider_id, model_id) \
             VALUES (?1, ?2, ?3)",
            params![name, target.provider_id, target.model],
        )
        .map_err(db)?;
    Ok(())
}

/// Forgets a legacy alias name (an unknown name is no error).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn delete_legacy_name(connection: &Connection, name: &str) -> Result<(), AppError> {
    connection
        .execute(
            "DELETE FROM llm_model_legacy_names WHERE name = ?1",
            params![name],
        )
        .map_err(db)?;
    Ok(())
}

/// The routing notices, oldest first.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn list_routing_notices(connection: &Connection) -> Result<Vec<String>, AppError> {
    let mut statement = connection
        .prepare_cached("SELECT message FROM llm_routing_notices ORDER BY id")
        .map_err(db)?;
    let rows = statement.query_map([], |row| row.get(0)).map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// Removes every routing notice.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn clear_routing_notices(connection: &Connection) -> Result<(), AppError> {
    connection
        .execute("DELETE FROM llm_routing_notices", [])
        .map_err(db)?;
    Ok(())
}
