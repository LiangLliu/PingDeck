//! Provider identity over time: which provider holds a name now, former names that keep routing
//! for a while after a rename, tombstones of deleted ids (a new provider never inherits an old
//! id), the clean-up of rows that name a deleted provider or model by string, and the review mark
//! of an imported provider.

use rusqlite::{Connection, OptionalExtension, params};

use super::routing::{fallbacks_of, list_fallbacks, set_model_fallbacks};
use super::{ModelRef, db, json_text};
use crate::llm::providers::FORMER_NAME_DAYS;
use crate::sqlite::escape_like;
use pingdeck_base::error::AppError;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// An old provider name that still resolves to the provider until `expires_at_ms`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FormerName {
    pub name: String,
    pub provider_id: String,
    pub expires_at_ms: i64,
}

/// The unexpired former names, oldest expiry first.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn list_former_names(
    connection: &Connection,
    now_ms: i64,
) -> Result<Vec<FormerName>, AppError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT name, provider_id, expires_at_ms FROM llm_provider_former_names \
             WHERE expires_at_ms > ?1 ORDER BY expires_at_ms, name",
        )
        .map_err(db)?;
    let rows = statement
        .query_map(params![now_ms], |row| {
            Ok(FormerName {
                name: row.get(0)?,
                provider_id: row.get(1)?,
                expires_at_ms: row.get(2)?,
            })
        })
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// The id of the provider other than `id` whose name, id or unexpired former name equals `name`
/// (trimmed, case-insensitive): any would make a model reference ambiguous.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn provider_name_holder(
    connection: &Connection,
    id: &str,
    name: &str,
) -> Result<Option<String>, AppError> {
    let wanted = name.trim().to_lowercase();
    let mut statement = connection
        .prepare_cached("SELECT id, name FROM llm_providers WHERE id <> ?1")
        .map_err(db)?;
    let rows = statement
        .query_map(params![id], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })
        .map_err(db)?;
    for row in rows {
        let (other_id, other_name) = row.map_err(db)?;
        if other_id.to_lowercase() == wanted || other_name.trim().to_lowercase() == wanted {
            return Ok(Some(other_id));
        }
    }
    // Former names expire by the wall clock, not the caller's (possibly imported) stamp.
    former_name_holder(connection, id, name, pingdeck_base::time::now_ms())
}

/// `base`, or `base 2`, `base 3`, … : the first that no provider other than `id` holds (see
/// [`provider_name_holder`]); for names the app picked itself, not ones the user typed.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn unique_provider_name(
    connection: &Connection,
    id: &str,
    base: &str,
) -> Result<String, AppError> {
    let base = base.trim();
    let mut candidate = base.to_owned();
    let mut counter = 2;
    while provider_name_holder(connection, id, &candidate)?.is_some() {
        candidate = format!("{base} {counter}");
        counter += 1;
    }
    Ok(candidate)
}

/// The provider other than `id` that still answers to `name` as a former name.
pub(super) fn former_name_holder(
    connection: &Connection,
    id: &str,
    name: &str,
    now_ms: i64,
) -> Result<Option<String>, AppError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT provider_id FROM llm_provider_former_names \
             WHERE name = ?1 AND provider_id <> ?2 AND expires_at_ms > ?3",
        )
        .map_err(db)?;
    let mut rows = statement
        .query(params![name.trim(), id, now_ms])
        .map_err(db)?;
    rows.next()
        .map_err(db)?
        .map(|row| row.get(0))
        .transpose()
        .map_err(db)
}

/// Keeps `old_name` resolving to `provider_id` for [`FORMER_NAME_DAYS`] days, and forgets former
/// names that expired or that the provider took back.
pub(super) fn rename_recorded(
    connection: &Connection,
    provider_id: &str,
    old_name: Option<&str>,
    new_name: &str,
    now_ms: i64,
) -> Result<(), AppError> {
    connection
        .execute(
            "DELETE FROM llm_provider_former_names WHERE expires_at_ms <= ?1 OR name = ?2",
            params![now_ms, new_name.trim()],
        )
        .map_err(db)?;
    if let Some(old) = old_name.map(str::trim).filter(|old| !old.is_empty()) {
        connection
            .execute(
                "INSERT OR REPLACE INTO llm_provider_former_names (name, provider_id, expires_at_ms) \
                 VALUES (?1, ?2, ?3)",
                params![old, provider_id, now_ms.saturating_add(FORMER_NAME_DAYS * DAY_MS)],
            )
            .map_err(db)?;
    }
    Ok(())
}

/// The ids of deleted providers, lower case: a new provider never takes one.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn tombstone_ids(connection: &Connection) -> Result<Vec<String>, AppError> {
    let mut statement = connection
        .prepare_cached("SELECT lower(id) FROM llm_provider_tombstones")
        .map_err(db)?;
    let rows = statement.query_map([], |row| row.get(0)).map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

pub(super) fn bury(connection: &Connection, id: &str) -> Result<(), AppError> {
    connection
        .execute(
            "INSERT OR REPLACE INTO llm_provider_tombstones (id) VALUES (?1)",
            params![id],
        )
        .map_err(db)?;
    Ok(())
}

/// What a deletion took with it from rows that named the provider or model by string.
#[derive(Debug, Default, Clone, PartialEq, Eq)]
pub struct Purged {
    /// Primaries whose gateway rule lost an entry.
    pub chains: Vec<ModelRef>,
    /// Clients whose allowed models lost an entry.
    pub clients: Vec<String>,
}

/// Whether an allow-list entry (`provider_id/model`) names the purged provider (`model` `None`)
/// or the purged model.
fn names(entry: &str, provider_id: &str, model: Option<&str>) -> bool {
    entry.split_once('/').is_some_and(|(left, right)| {
        left.eq_ignore_ascii_case(provider_id) && model.is_none_or(|model| right == model)
    })
}

fn purge_allowlists(
    connection: &Connection,
    provider_id: &str,
    model: Option<&str>,
) -> Result<Vec<String>, AppError> {
    let mut statement = connection
        .prepare_cached("SELECT id, allowed_models_json, default_model FROM llm_gateway_clients")
        .map_err(db)?;
    let rows: Vec<(String, String, Option<String>)> = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))
        .map_err(db)?
        .collect::<Result<_, _>>()
        .map_err(db)?;
    let mut update = connection
        .prepare_cached(
            "UPDATE llm_gateway_clients SET allowed_models_json = ?2, \
                 default_model = CASE WHEN ?3 THEN NULL ELSE default_model END WHERE id = ?1",
        )
        .map_err(db)?;
    let mut touched = Vec::new();
    for (id, json, default_model) in rows {
        // An allow-list that does not parse is left as it is: the gateway reads it the same way.
        let Ok(models) = serde_json::from_str::<Vec<String>>(&json) else {
            continue;
        };
        let kept: Vec<&String> = models
            .iter()
            .filter(|entry| !names(entry, provider_id, model))
            .collect();
        let default_gone = default_model
            .as_deref()
            .is_some_and(|entry| names(entry, provider_id, model));
        if kept.len() == models.len() && !default_gone {
            continue;
        }
        update
            .execute(params![id, json_text(&kept)?, default_gone])
            .map_err(db)?;
        touched.push(id);
    }
    Ok(touched)
}

/// Removes, on the caller's transaction, every row that names `provider_id` (all its models when
/// `model` is `None`) by string: user prices, catalogue links, legacy alias names, gateway rule
/// entries and client allow-list entries. Rows that hang off the provider by foreign key go with
/// it.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn purge_references(
    connection: &Connection,
    provider_id: &str,
    model: Option<&str>,
) -> Result<Purged, AppError> {
    let by_model = "AND (?2 IS NULL OR model_id = ?2)";
    for table in ["model_pricing", "model_alias", "llm_model_legacy_names"] {
        connection
            .execute(
                &format!("DELETE FROM {table} WHERE provider_id = ?1 {by_model}"),
                params![provider_id, model],
            )
            .map_err(db)?;
    }
    if model.is_none() {
        connection
            .execute(
                "DELETE FROM provider_pricing WHERE provider_id = ?1",
                params![provider_id],
            )
            .map_err(db)?;
    }
    let mut purged = Purged::default();
    if let Some(model) = model {
        let primary = ModelRef {
            provider_id: provider_id.to_owned(),
            model: model.to_owned(),
        };
        if !fallbacks_of(connection, provider_id, model)?.is_empty() {
            set_model_fallbacks(connection, &primary, &[])?;
            purged.chains.push(primary);
        }
    }
    for (primary, chain) in list_fallbacks(connection)? {
        let kept: Vec<ModelRef> = chain
            .iter()
            .filter(|target| {
                !(target.provider_id == provider_id && model.is_none_or(|m| target.model == m))
            })
            .cloned()
            .collect();
        if kept.len() != chain.len() {
            set_model_fallbacks(connection, &primary, &kept)?;
            purged.chains.push(primary);
        }
    }
    purged.clients = purge_allowlists(connection, provider_id, model)?;
    Ok(purged)
}

/// Clears the "imported, not reviewed" mark. Returns whether one was set.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown provider, [`AppError::Database`] otherwise.
pub fn mark_reviewed(connection: &Connection, provider_id: &str) -> Result<bool, AppError> {
    let exists: Option<i64> = connection
        .query_row(
            "SELECT 1 FROM llm_providers WHERE id = ?1",
            params![provider_id],
            |row| row.get(0),
        )
        .optional()
        .map_err(db)?;
    if exists.is_none() {
        return Err(AppError::not_found(format!(
            "provider {provider_id} not found"
        )));
    }
    let cleared = connection
        .execute(
            "UPDATE llm_providers SET review_pending_since_ms = NULL \
             WHERE id = ?1 AND review_pending_since_ms IS NOT NULL",
            params![provider_id],
        )
        .map_err(db)?;
    Ok(cleared > 0)
}

/// Who called a model by the provider name `name` (`name/…`, case-insensitive) since `since_ms`:
/// the client's name (empty: no client token) and how many calls, busiest first, at most 20.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn recent_callers(
    connection: &Connection,
    name: &str,
    since_ms: i64,
) -> Result<Vec<(String, i64)>, AppError> {
    let escaped = escape_like(name.trim());
    let mut statement = connection
        .prepare_cached(
            "SELECT COALESCE(client_name, ''), COUNT(*) FROM llm_request_log \
             WHERE ts_ms >= ?2 AND requested_model LIKE ?1 ESCAPE '\\' \
             GROUP BY 1 ORDER BY 2 DESC, 1 LIMIT 20",
        )
        .map_err(db)?;
    let rows = statement
        .query_map(params![format!("{escaped}/%"), since_ms], |row| {
            Ok((row.get(0)?, row.get(1)?))
        })
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}
