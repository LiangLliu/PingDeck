//! Writes of a model's capability matrix: user overrides, applied measurements and the guarded
//! swaps of apply and undo.

use rusqlite::{Connection, params};

use super::{ProviderModel, atomic, db, get_model, json_text, to_i64};
use crate::llm::capabilities::{Capability, CapabilitySource, ModelCapabilities};
use crate::llm::probe::apply::CapabilityChange;
use crate::llm::probe::capability::CapabilityUpdate;
use spanbay_base::error::AppError;

/// Overwrites the stored matrix of one model (a missing row is left alone).
fn write_capabilities(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
    capabilities: &ModelCapabilities,
) -> Result<(), AppError> {
    connection
        .execute(
            "UPDATE llm_provider_models SET capabilities_json = ?3 \
             WHERE provider_id = ?1 AND model_id = ?2",
            params![provider_id, model_id, json_text(capabilities)?],
        )
        .map_err(db)?;
    Ok(())
}

/// Applies the user's capability overrides from the Settings matrix and returns the updated row.
///
/// For every capability of `requested`: a `UserOverride` state is stored (it beats every other
/// source); an `Unknown` state *resets* the override, which leaves the bit unknown until the
/// next probe or list refresh; any other source is ignored, so the UI may send back the whole
/// matrix it received without freezing measurements into overrides.
///
/// # Errors
/// [`AppError::NotFound`] when the model does not exist, [`AppError::Database`] otherwise.
pub fn set_model_capabilities_override(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
    requested: &ModelCapabilities,
) -> Result<ProviderModel, AppError> {
    atomic(connection, |connection| {
        let mut model = get_model(connection, provider_id, model_id)?
            .ok_or_else(|| AppError::not_found(format!("model {model_id} not found")))?;
        for capability in Capability::ALL {
            let wanted = requested.get(capability);
            match wanted.source {
                CapabilitySource::UserOverride => {
                    model.capabilities.apply(
                        capability,
                        wanted.supported,
                        CapabilitySource::UserOverride,
                    );
                }
                CapabilitySource::Unknown => model.capabilities.clear_override(capability),
                _ => {}
            }
        }
        write_capabilities(connection, provider_id, model_id, &model.capabilities)?;
        get_model(connection, provider_id, model_id)?
            .ok_or_else(|| AppError::internal("model vanished after override"))
    })
}

/// Records what an applied probe run measured, dated `now_ms` (`Measured` strength with its date
/// and sample count; a `UserOverride` bit is kept, an older result never replaces a newer one).
/// Only "Apply results" calls it: probing alone writes evidence, never the matrix routing filters
/// on. A model that is not stored has nothing to update. Returns whether a stored bit changed.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn record_measured(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
    updates: &[CapabilityUpdate],
    now_ms: i64,
) -> Result<bool, AppError> {
    let Some(model) = get_model(connection, provider_id, model_id)? else {
        return Ok(false);
    };
    let mut capabilities = model.capabilities;
    let mut changed = false;
    for update in updates {
        changed |= capabilities.apply_measurement(update.capability, update.measurement, now_ms);
    }
    if changed {
        write_capabilities(connection, provider_id, model_id, &capabilities)?;
    }
    Ok(changed)
}

/// Records the context window a full context probe measured: it beats the listed value and never
/// the user's. Returns whether a stored model changed.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn record_measured_context(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
    window: u64,
) -> Result<bool, AppError> {
    let changed = connection
        .execute(
            "UPDATE llm_provider_models SET measured_context = ?3 \
             WHERE provider_id = ?1 AND model_id = ?2 \
               AND (measured_context IS NULL OR measured_context <> ?3)",
            params![provider_id, model_id, to_i64(window)],
        )
        .map_err(db)?;
    Ok(changed > 0)
}

/// Writes the capability states an apply planned, or restores them on undo (`undo = true`): a
/// change lands only while the stored state still equals the one it expects (`previous` for an
/// apply, `applied` for an undo), so a later edit, override or measurement survives. Returns the
/// ids of the models written, once each.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn swap_capability_states(
    connection: &Connection,
    provider_id: &str,
    changes: &[CapabilityChange],
    undo: bool,
) -> Result<Vec<String>, AppError> {
    let mut written: Vec<String> = Vec::new();
    for change in changes {
        let Some(model) = get_model(connection, provider_id, &change.model_id)? else {
            continue;
        };
        let (expect, write) = if undo {
            (change.applied, change.previous)
        } else {
            (change.previous, change.applied)
        };
        if model.capabilities.get(change.capability) != expect {
            continue;
        }
        let mut capabilities = model.capabilities;
        capabilities.set(change.capability, write);
        write_capabilities(connection, provider_id, &change.model_id, &capabilities)?;
        if !written.contains(&change.model_id) {
            written.push(change.model_id.clone());
        }
    }
    Ok(written)
}
