//! Which model a provider's model id stands for.
//!
//! Single responsibility: map `(provider, model id)` to a `model_key` (D-259). The precedence
//! (the user's alias, else the id itself when it is a key, else the key of the catalog row
//! [`super::lookup_record`] finds, else `id:<model id>` for an unlisted model) is
//! [`spanbay_model::model_catalog::resolve_with`]'s; this module supplies its three SQL lookups.
//! Also keeps `model_pricing.model_key` in step with the catalog.

use rusqlite::{Connection, OptionalExtension, params};

use super::{db_error, lookup_forms, lookup_ids};
use spanbay_base::error::AppError;
use spanbay_model::model_catalog::{ModelResolution, resolve_with};

/// [`resolve`] returning the raw `SQLite` error, for use inside catalog transactions.
fn resolve_in(
    conn: &Connection,
    provider_id: Option<&str>,
    model_id: &str,
) -> rusqlite::Result<ModelResolution> {
    resolve_with(
        provider_id,
        model_id,
        |provider, model| {
            conn.prepare_cached(
                "SELECT model_key FROM model_alias WHERE provider_id = ?1 AND model_id = ?2",
            )?
            .query_row(params![provider, model], |row| row.get(0))
            .optional()
        },
        |key| {
            conn.prepare_cached("SELECT EXISTS (SELECT 1 FROM model_catalog WHERE model_key = ?1)")?
                .query_row([key], |row| row.get(0))
        },
        |model| {
            for id in lookup_ids(model) {
                if let Some(record) = lookup_forms(conn, id)? {
                    let key: String = conn
                        .prepare_cached(
                            "SELECT model_key FROM model_catalog WHERE family = ?1 AND model_id = ?2",
                        )?
                        .query_row(params![record.family, record.model_id], |row| row.get(0))?;
                    if !key.is_empty() {
                        return Ok(Some(key));
                    }
                }
            }
            Ok(None)
        },
    )
}

/// The model `model_id` of `provider_id` (`None` or empty: any provider) stands for.
///
/// # Errors
/// [`AppError::database`] on any `SQLite` failure.
pub fn resolve(
    conn: &Connection,
    provider_id: Option<&str>,
    model_id: &str,
) -> Result<ModelResolution, AppError> {
    resolve_in(conn, provider_id, model_id).map_err(db_error)
}

/// Recomputes `model_pricing.model_key` of every row and writes the ones that changed: run after
/// every catalog or alias write (a sync can move an id to another model) and once for rows
/// written without keys.
///
/// # Errors
/// Any `SQLite` failure.
pub(crate) fn rekey_pricing(conn: &Connection) -> rusqlite::Result<()> {
    let rows: Vec<(String, String, String)> = conn
        .prepare_cached("SELECT provider_id, model_id, model_key FROM model_pricing")?
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?, row.get(2)?)))?
        .collect::<rusqlite::Result<_>>()?;
    let mut update = conn.prepare_cached(
        "UPDATE model_pricing SET model_key = ?3 WHERE provider_id = ?1 AND model_id = ?2",
    )?;
    for (provider_id, model_id, stored) in rows {
        let key = resolve_in(conn, Some(&provider_id), &model_id)?.model_key;
        if key != stored {
            update.execute(params![provider_id, model_id, key])?;
        }
    }
    Ok(())
}
