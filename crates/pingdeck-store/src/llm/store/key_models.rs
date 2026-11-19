//! What each provider key's own model list returned (`llm_key_models`): relays such as sub2api and
//! new-api bind a key to a group, and the group decides the models and protocols (D-258).
//!
//! Single responsibility: SQL over `llm_key_models` and the list stamps of `llm_keys`
//! (`models_listed_at_ms`, `preferred_protocol`). Routing reads it through
//! `pingdeck_llm::llm::router::KeyScopes`.

use rusqlite::{Connection, params};

use super::atomic;
use super::keys::protocols_from_json;
use super::{db, json_text};
use crate::llm::wire::Protocol;
use pingdeck_base::error::AppError;

/// One model a key's list returned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyModel {
    pub key_id: String,
    pub model_id: String,
    /// The protocols the relay declared for it (new-api `supported_endpoint_types`); `None` when
    /// it declared none.
    pub protocols: Option<Vec<Protocol>>,
}

/// The listed models of every key of `provider_id` (every key of every provider when `None`).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn list_key_models(
    connection: &Connection,
    provider_id: Option<&str>,
) -> Result<Vec<KeyModel>, AppError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT m.key_id, m.model_id, m.protocols_json FROM llm_key_models m \
             JOIN llm_keys k ON k.id = m.key_id \
             WHERE ?1 IS NULL OR k.provider_id = ?1 ORDER BY m.key_id, m.model_id",
        )
        .map_err(db)?;
    let rows = statement
        .query_map(params![provider_id], |row| {
            Ok(KeyModel {
                key_id: row.get(0)?,
                model_id: row.get(1)?,
                protocols: protocols_from_json(row.get(2)?),
            })
        })
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// Replaces what key `key_id`'s list returned with `models` (id and declared protocols) and
/// stamps the key as listed at `now_ms` with its inferred native protocol.
///
/// # Errors
/// [`AppError::NotFound`] when the key does not exist, [`AppError::Database`] otherwise.
pub fn replace_key_models(
    connection: &Connection,
    key_id: &str,
    models: &[(String, Option<Vec<Protocol>>)],
    preferred: Option<Protocol>,
    now_ms: i64,
) -> Result<(), AppError> {
    atomic(connection, |connection| {
        let updated = connection
            .execute(
                "UPDATE llm_keys SET models_listed_at_ms = ?2, preferred_protocol = ?3 WHERE id = ?1",
                params![key_id, now_ms, preferred.map(Protocol::as_str)],
            )
            .map_err(db)?;
        if updated == 0 {
            return Err(AppError::not_found(format!("key {key_id} not found")));
        }
        // Rows that stay keep their `acked_at_ms` (a drift marked as seen stays seen).
        let fresh: std::collections::BTreeSet<&str> =
            models.iter().map(|(id, _)| id.as_str()).collect();
        let stale: Vec<String> = {
            let mut statement = connection
                .prepare_cached("SELECT model_id FROM llm_key_models WHERE key_id = ?1")
                .map_err(db)?;
            let rows = statement
                .query_map(params![key_id], |row| row.get::<_, String>(0))
                .map_err(db)?;
            rows.collect::<Result<Vec<_>, _>>()
                .map_err(db)?
                .into_iter()
                .filter(|id| !fresh.contains(id.as_str()))
                .collect()
        };
        let mut delete = connection
            .prepare_cached("DELETE FROM llm_key_models WHERE key_id = ?1 AND model_id = ?2")
            .map_err(db)?;
        for model_id in &stale {
            delete.execute(params![key_id, model_id]).map_err(db)?;
        }
        let mut insert = connection
            .prepare_cached(
                "INSERT INTO llm_key_models (key_id, model_id, protocols_json) VALUES (?1, ?2, ?3) \
                 ON CONFLICT(key_id, model_id) DO UPDATE SET protocols_json = excluded.protocols_json",
            )
            .map_err(db)?;
        for (model_id, protocols) in models {
            let protocols = protocols
                .as_ref()
                .filter(|list| !list.is_empty())
                .map(json_text)
                .transpose()?;
            insert
                .execute(params![key_id, model_id, protocols])
                .map_err(db)?;
        }
        Ok(())
    })
}

/// Forgets key `key_id`'s list (its secret was replaced: another secret may sit in another
/// group), so it may call every model again until its list is read.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub(super) fn forget_key_models(connection: &Connection, key_id: &str) -> Result<(), AppError> {
    connection
        .execute(
            "DELETE FROM llm_key_models WHERE key_id = ?1",
            params![key_id],
        )
        .map_err(db)?;
    connection
        .execute(
            "UPDATE llm_keys SET models_listed_at_ms = NULL, preferred_protocol = NULL WHERE id = ?1",
            params![key_id],
        )
        .map_err(db)?;
    Ok(())
}
