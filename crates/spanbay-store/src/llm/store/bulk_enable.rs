//! Bulk and per-row enable and disable with Undo.
//!
//! Single responsibility: change the `enabled` flag of a set of models and log the previous
//! values in `llm_probe_apply_log` (run id `bulk:<id>`), so the Undo of a bulk switch and the Undo
//! of "Apply results" are one mechanism with one retention (the newest [`super::MAX_APPLY_LOG`]).
//! An undo restores a flag or a capability state only where it still equals the value the
//! operation wrote.

use std::collections::BTreeMap;
#[cfg(test)]
use std::collections::BTreeSet;

use rusqlite::{Connection, params};

use super::model_capabilities::swap_capability_states;
use super::probe_runs::{delete_probe_apply, get_probe_apply, record_probe_apply};
use super::{BulkEnableReport, UndoReport, atomic, db, flag, list_models};
use crate::llm::probe::apply::{ApplyReport, CurrentModel, PreviousState, plan_undo};
use spanbay_base::error::AppError;
use spanbay_model::variables::uuid_v4;

/// One `enabled` change of [`set_models_enabled`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct EnabledChange {
    pub model_id: String,
    pub enabled: bool,
    /// Write only when the flag currently has this value (an undo restores a flag only where it
    /// still equals what the apply wrote); `None` writes unconditionally.
    pub only_if_current: Option<bool>,
}

/// Sets the `enabled` flag of several models in one transaction (apply and undo of a probe run).
/// A model that has no row, or whose flag does not satisfy `only_if_current`, is skipped. Returns
/// the ids that changed.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn set_models_enabled(
    connection: &Connection,
    provider_id: &str,
    changes: &[EnabledChange],
) -> Result<Vec<String>, AppError> {
    atomic(connection, |connection| {
        let mut statement = connection
            .prepare_cached(
                "UPDATE llm_provider_models SET enabled = ?3 \
                 WHERE provider_id = ?1 AND model_id = ?2 AND enabled <> ?3 \
                   AND (?4 IS NULL OR enabled = ?4)",
            )
            .map_err(db)?;
        let mut changed = Vec::new();
        for change in changes {
            let updated = statement
                .execute(params![
                    provider_id,
                    change.model_id,
                    flag(change.enabled),
                    change.only_if_current.map(flag),
                ])
                .map_err(db)?;
            if updated > 0 {
                changed.push(change.model_id.clone());
            }
        }
        Ok(changed)
    })
}

/// Every stored model that is switched off, as `(provider_id, model_id)`: the routing snapshot
/// refuses these. A model with no row at all still routes.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
#[cfg(test)]
pub(crate) fn disabled_models(
    connection: &Connection,
) -> Result<BTreeSet<(String, String)>, AppError> {
    let mut statement = connection
        .prepare_cached("SELECT provider_id, model_id FROM llm_provider_models WHERE enabled = 0")
        .map_err(db)?;
    let rows = statement
        .query_map([], |row| Ok((row.get(0)?, row.get(1)?)))
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// Sets `enabled` on `changes` (`(model_id, enabled)`; one entry for the row switch) of a provider
/// and logs what changed. A model with no row, or that already has the value, is skipped; a
/// repeated id counts once (the first wins). The newest [`super::MAX_APPLY_LOG`] operations of
/// this log and of probe applies are kept.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure (nothing is changed).
pub fn set_models_enabled_bulk(
    connection: &Connection,
    provider_id: &str,
    changes: &[(String, bool)],
    now_ms: i64,
) -> Result<BulkEnableReport, AppError> {
    let id = format!("bulk_{}", uuid_v4());
    atomic(connection, |connection| {
        let current: BTreeMap<String, bool> = list_models(connection, Some(provider_id))?
            .into_iter()
            .map(|model| (model.model_id, model.enabled))
            .collect();
        let mut previous: Vec<PreviousState> = Vec::new();
        for (model_id, enabled) in changes {
            let Some(&was) = current.get(model_id) else {
                continue;
            };
            if was != *enabled && !previous.iter().any(|state| &state.model_id == model_id) {
                previous.push(PreviousState {
                    model_id: model_id.clone(),
                    previous_enabled: was,
                    applied_enabled: *enabled,
                });
            }
        }
        if !previous.is_empty() {
            let writes: Vec<EnabledChange> = previous
                .iter()
                .map(|state| EnabledChange {
                    model_id: state.model_id.clone(),
                    enabled: state.applied_enabled,
                    only_if_current: None,
                })
                .collect();
            set_models_enabled(connection, provider_id, &writes)?;
            let ids = |enabled: bool| -> Vec<String> {
                previous
                    .iter()
                    .filter(|state| state.applied_enabled == enabled)
                    .map(|state| state.model_id.clone())
                    .collect()
            };
            let report = ApplyReport {
                enabled: ids(true),
                disabled: ids(false),
                created: Vec::new(),
                previous: previous.clone(),
                capabilities: Vec::new(),
            };
            record_probe_apply(
                connection,
                &id,
                &format!("bulk:{id}"),
                provider_id,
                &report,
                now_ms,
            )?;
        }
        Ok(BulkEnableReport {
            id: id.clone(),
            provider_id: provider_id.to_owned(),
            previous,
            applied_at_ms: now_ms,
        })
    })
}

/// Undoes a logged operation, a bulk switch or an "Apply results", and removes it from the log:
/// every model whose flag still equals the value the operation wrote gets its previous value
/// back; a model edited since (or deleted) is left alone, so later manual edits survive.
///
/// # Errors
/// [`AppError::NotFound`] when the operation is not in the log (never logged, already undone, or
/// older than the newest [`super::MAX_APPLY_LOG`]).
pub fn undo_bulk(connection: &Connection, op_id: &str) -> Result<UndoReport, AppError> {
    atomic(connection, |connection| {
        let record = get_probe_apply(connection, op_id)?
            .ok_or_else(|| AppError::not_found(format!("operation {op_id} not found")))?;
        let current: Vec<CurrentModel> = list_models(connection, Some(&record.provider_id))?
            .into_iter()
            .map(|model| CurrentModel {
                model_id: model.model_id,
                enabled: model.enabled,
            })
            .collect();
        let plan = plan_undo(&record.report, &current);
        let writes: Vec<EnabledChange> = plan
            .restore
            .iter()
            .map(|restore| EnabledChange {
                model_id: restore.model_id.clone(),
                enabled: restore.enabled,
                only_if_current: Some(!restore.enabled),
            })
            .collect();
        let mut restored = set_models_enabled(connection, &record.provider_id, &writes)?;
        let mut kept = plan.kept;
        let states = swap_capability_states(
            connection,
            &record.provider_id,
            &record.report.capabilities,
            true,
        )?;
        for change in &record.report.capabilities {
            let target = if states.contains(&change.model_id) {
                &mut restored
            } else {
                &mut kept
            };
            if !target.contains(&change.model_id) {
                target.push(change.model_id.clone());
            }
        }
        delete_probe_apply(connection, op_id)?;
        Ok(UndoReport { restored, kept })
    })
}
