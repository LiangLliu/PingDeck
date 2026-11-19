//! What changes a provider's models without editing them: the read-only list refresh and its
//! drift, "Mark as seen", list confirmations and rows created from a scan.
//!
//! SQL over `llm_provider_models` for the server-owned columns (`capabilities_json` of a refresh,
//! `presence`, `first_seen_ms`, `new_cleared_at_ms`, `listed_confirmed_at_ms`). The user-editable
//! columns are written by `save_model`, the matrix otherwise by `model_capabilities`.

use std::collections::{BTreeMap, BTreeSet};

use rusqlite::{Connection, params};

use super::provider_state::{clear_model_drift, set_model_drift};
use super::providers::get_provider;
use super::{
    ModelSource, Provider, ProviderModel, atomic, db, enum_text, flag, json_text, list_models,
    to_i64,
};
use crate::llm::capabilities::{Capability, CapabilitySource, ModelCapabilities};
use crate::llm::probe::listing::{ModelDrift, Presence, compute_drift};
use pingdeck_base::error::AppError;

/// One entry of a provider's model list.
#[derive(Debug, Clone, PartialEq, Default)]
pub struct ListedModel {
    pub id: String,
    /// Context window in tokens.
    pub context: Option<u64>,
    /// Maximum output tokens.
    pub max_output: Option<u64>,
    /// Declared and catalogue capabilities (a `Measured` or `UserOverride` bit here is clamped to
    /// `Declared` by [`apply_model_listing`]).
    pub capabilities: ModelCapabilities,
}

/// The read-only "Refresh models": compares the provider's fresh list with the stored rows and
/// stores the comparison on the provider. Call it only with a list that was fetched successfully.
///
/// Of the rows that already exist it updates only metadata: the listed context and max output
/// (when the list gives them; the user's values stay), the declared capability bits (at
/// `Declared` strength at most, so a `Measured` or `UserOverride` bit is never overridden) and
/// `presence` (`listed` / `not_in_provider`). It adds no row, deletes none and never touches
/// `enabled`. The returned drift is what was stored (`added` = listed ids with no row, `removed`
/// = stored ids the list lacks); an empty drift is stored too, so `checked_at_ms` is known.
///
/// Only a `live` row can be "removed" or flagged `not_in_provider`: a manual or preset row is the
/// user's word. A list that is not `complete` (truncated or paged past the cap) or empty removes
/// nothing and flags nothing; the drift says so with `listing_complete = false`.
///
/// A drift the user already marked as seen is not reported again: an added id every key listing
/// of which carries `acked_at_ms`, and a removed id that was already flagged `not_in_provider` and
/// is no longer in the stored drift.
///
/// # Errors
/// [`AppError::NotFound`] when the provider does not exist, [`AppError::Database`] otherwise.
pub fn apply_model_listing(
    connection: &Connection,
    provider_id: &str,
    listed: &[ListedModel],
    complete: bool,
    now_ms: i64,
) -> Result<ModelDrift, AppError> {
    let complete = complete && !listed.is_empty();
    atomic(connection, |connection| {
        let Some(provider) = get_provider(connection, provider_id)? else {
            return Err(AppError::not_found(format!(
                "provider {provider_id} not found"
            )));
        };
        let existing = list_models(connection, Some(provider_id))?;
        let mut by_id: BTreeMap<&str, &ListedModel> = BTreeMap::new();
        for model in listed {
            by_id.entry(model.id.as_str()).or_insert(model);
        }
        let drift = reported_drift(connection, &provider, &existing, listed, complete, now_ms)?;
        let mut update = connection
            .prepare_cached(
                "UPDATE llm_provider_models SET listed_context = ?3, listed_max_output = ?4, \
                     capabilities_json = ?5, presence = ?6 \
                 WHERE provider_id = ?1 AND model_id = ?2",
            )
            .map_err(db)?;
        for row in &existing {
            let entry = by_id.get(row.model_id.as_str()).copied();
            let (capabilities, presence) = refreshed_state(row, entry, complete);
            update
                .execute(params![
                    provider_id,
                    row.model_id,
                    entry
                        .and_then(|e| e.context)
                        .or(row.listed_context)
                        .map(to_i64),
                    entry
                        .and_then(|e| e.max_output)
                        .or(row.listed_max_output)
                        .map(to_i64),
                    json_text(&capabilities)?,
                    enum_text(&presence)?,
                ])
                .map_err(db)?;
        }
        set_model_drift(connection, provider_id, &drift)?;
        Ok(drift)
    })
}

/// The drift a refresh reports: listed ids with no row and stored `live` ids the list lacks
/// (removals only from a complete list), minus what the user already marked as seen.
fn reported_drift(
    connection: &Connection,
    provider: &Provider,
    existing: &[ProviderModel],
    listed: &[ListedModel],
    complete: bool,
    now_ms: i64,
) -> Result<ModelDrift, AppError> {
    let stored_ids: Vec<String> = existing.iter().map(|row| row.model_id.clone()).collect();
    let listed_ids: Vec<String> = listed.iter().map(|model| model.id.clone()).collect();
    let mut drift = compute_drift(&stored_ids, &listed_ids, now_ms);
    let from_list: BTreeSet<&str> = existing
        .iter()
        .filter(|row| row.source == ModelSource::Live)
        .map(|row| row.model_id.as_str())
        .collect();
    drift.limit_removals(complete, |id| from_list.contains(id));
    let acked = acked_models(connection, &provider.id)?;
    drift.added.retain(|id| !acked.contains(id.as_str()));
    let pending: BTreeSet<&str> = provider
        .model_drift
        .as_ref()
        .map(|prior| prior.removed.iter().map(String::as_str).collect())
        .unwrap_or_default();
    drift.removed.retain(|id| {
        pending.contains(id.as_str())
            || !existing
                .iter()
                .any(|row| row.model_id == *id && row.presence == Presence::NotInProvider)
    });
    Ok(drift)
}

/// The matrix and presence a stored row gets from its list `entry`: declared bits at `Declared`
/// strength at most, `listed` when listed, `not_in_provider` for a `live` row a complete list
/// lacks, else the stored presence.
fn refreshed_state(
    row: &ProviderModel,
    entry: Option<&ListedModel>,
    complete: bool,
) -> (ModelCapabilities, Presence) {
    let mut capabilities = row.capabilities;
    if let Some(entry) = entry {
        for capability in Capability::ALL {
            let state = entry.capabilities.get(capability);
            if state.source != CapabilitySource::Unknown {
                let source = state.source.min(CapabilitySource::Declared);
                capabilities.apply(capability, state.supported, source);
            }
        }
    }
    let presence = if entry.is_some() {
        Presence::Listed
    } else if complete && row.source == ModelSource::Live {
        Presence::NotInProvider
    } else {
        row.presence
    };
    (capabilities, presence)
}

/// "Mark as seen": removes the stored drift and stamps `new_cleared_at_ms` on every
/// model of the provider, which clears the "New" chip of the rows first seen before `now_ms`. The
/// `not_in_provider` flags stay.
///
/// # Errors
/// [`AppError::NotFound`] when the provider does not exist, [`AppError::Database`] otherwise.
pub fn ack_model_drift(
    connection: &Connection,
    provider_id: &str,
    now_ms: i64,
) -> Result<(), AppError> {
    atomic(connection, |connection| {
        clear_model_drift(connection, provider_id)?;
        connection
            .execute(
                "UPDATE llm_provider_models SET new_cleared_at_ms = ?2 WHERE provider_id = ?1",
                params![provider_id, now_ms],
            )
            .map_err(db)?;
        connection
            .execute(
                "UPDATE llm_key_models SET acked_at_ms = ?2 WHERE acked_at_ms IS NULL \
                   AND key_id IN (SELECT id FROM llm_keys WHERE provider_id = ?1)",
                params![provider_id, now_ms],
            )
            .map_err(db)?;
        Ok(())
    })
}

/// The model ids some key of `provider_id` lists, every such listing marked as seen.
fn acked_models(connection: &Connection, provider_id: &str) -> Result<BTreeSet<String>, AppError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT m.model_id FROM llm_key_models m JOIN llm_keys k ON k.id = m.key_id \
             WHERE k.provider_id = ?1 GROUP BY m.model_id HAVING count(m.acked_at_ms) = count(*)",
        )
        .map_err(db)?;
    let rows = statement
        .query_map(params![provider_id], |row| row.get(0))
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// Stamps `listed_confirmed_at_ms` on the stored models of `provider_id` among `ids` that carry
/// none yet: some key's list named them, so a key whose list lacks them no longer calls them.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn confirm_listed(
    connection: &Connection,
    provider_id: &str,
    ids: &BTreeSet<String>,
    now_ms: i64,
) -> Result<(), AppError> {
    let mut statement = connection
        .prepare_cached(
            "UPDATE llm_provider_models SET listed_confirmed_at_ms = ?3 \
             WHERE provider_id = ?1 AND model_id = ?2 AND listed_confirmed_at_ms IS NULL",
        )
        .map_err(db)?;
    for id in ids {
        statement
            .execute(params![provider_id, id, now_ms])
            .map_err(db)?;
    }
    Ok(())
}

/// Stores listed models as new `live` rows (create-from-sniff, "Apply" of a probe run, a re-sniff's
/// added models): `first_seen_ms = now_ms`, `presence = listed`, the given matrix. The `bool` of
/// each pair is the `enabled` flag (a deselected row is saved disabled, never dropped); the
/// caller picks the defaults. A model that already has a row is left untouched. Returns how many
/// rows were created.
///
/// # Errors
/// [`AppError::NotFound`] when the provider does not exist, [`AppError::Validation`] for a blank
/// id, [`AppError::Database`] otherwise.
pub fn insert_models_from_sniff(
    connection: &Connection,
    provider_id: &str,
    models: &[(ListedModel, bool)],
    now_ms: i64,
) -> Result<usize, AppError> {
    atomic(connection, |connection| {
        if get_provider(connection, provider_id)?.is_none() {
            return Err(AppError::not_found(format!(
                "provider {provider_id} not found"
            )));
        }
        let mut statement = connection
            .prepare_cached(
                "INSERT OR IGNORE INTO llm_provider_models (provider_id, model_id, source, \
                     listed_context, listed_max_output, enabled, \
                     capabilities_json, presence, first_seen_ms, listed_confirmed_at_ms) \
                 VALUES (?1, ?2, 'live', ?3, ?4, ?5, ?6, 'listed', ?7, ?7)",
            )
            .map_err(db)?;
        let mut created = 0;
        for (model, enabled) in models {
            let id = model.id.trim();
            if id.is_empty() {
                return Err(AppError::validation("a model needs an id"));
            }
            created += statement
                .execute(params![
                    provider_id,
                    id,
                    model.context.map(to_i64),
                    model.max_output.map(to_i64),
                    flag(*enabled),
                    json_text(&model.capabilities)?,
                    now_ms,
                ])
                .map_err(db)?;
        }
        Ok(created)
    })
}
