//! "Add the new models" of a model-list refresh: the dedicated apply.
//!
//! Single responsibility: turn the models a refresh found (`ModelDrift::added`, ids only) into
//! stored `live` rows, switched off, and clear the drift, in ONE transaction. Before this call the
//! page wrote each model and then acknowledged the drift, so a failure in between left a drift
//! bar over half-added rows.

use serde::{Deserialize, Serialize};

use super::sniff_create::in_transaction;
use super::with_connection;
use crate::app::{AppState, current_time_ms};
use crate::error::AppError;
use crate::llm::capabilities::ModelCapabilities;
use crate::llm::events::LlmEvent;
use crate::llm::store::{self, ListedModel};

/// What the apply did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DriftApplyReport {
    /// The models stored (switched off), in the order the refresh listed them.
    pub added: Vec<String>,
    /// Requested ids the drift does not list as new (nothing was stored for them).
    pub skipped: Vec<String>,
}

/// Stores the new models of the provider's pending drift as `live`, disabled rows and clears the
/// drift. `model_ids` picks some of them; `None` takes every new model. The "no longer listed"
/// flags stay as the refresh set them.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown provider, [`AppError::Conflict`] when it has no pending
/// drift (nothing was refreshed, or it was acknowledged already).
pub fn llm_models_drift_apply_impl(
    state: &AppState,
    provider_id: &str,
    model_ids: Option<Vec<String>>,
) -> Result<DriftApplyReport, AppError> {
    let now = current_time_ms();
    let report = with_connection(state, |connection| {
        in_transaction(connection, |connection| {
            let provider = store::get_provider(connection, provider_id)?
                .ok_or_else(|| AppError::not_found(format!("provider {provider_id} not found")))?;
            let drift = provider.model_drift.ok_or_else(|| {
                AppError::conflict("there is no refreshed model list to apply: refresh first")
            })?;
            let (added, skipped) = match model_ids {
                None => (drift.added, Vec::new()),
                Some(wanted) => {
                    let (taken, left): (Vec<String>, Vec<String>) =
                        wanted.into_iter().partition(|id| drift.added.contains(id));
                    // Keep the refresh's order, not the caller's.
                    let ordered = drift
                        .added
                        .into_iter()
                        .filter(|id| taken.contains(id))
                        .collect();
                    (ordered, left)
                }
            };
            let listed: Vec<(ListedModel, bool)> = added
                .iter()
                .map(|id| {
                    (
                        ListedModel {
                            id: id.clone(),
                            context: None,
                            max_output: None,
                            capabilities: ModelCapabilities::default(),
                        },
                        false,
                    )
                })
                .collect();
            store::insert_models_from_sniff(connection, provider_id, &listed, now)?;
            store::ack_model_drift(connection, provider_id, now)?;
            Ok(DriftApplyReport { added, skipped })
        })
    })?;
    state.llm.touch();
    state.sync.notify_changed();
    state.llm.emit(LlmEvent::ModelsChanged {
        provider_id: provider_id.to_owned(),
    });
    state.llm.emit(LlmEvent::ProvidersChanged);
    Ok(report)
}
