//! Model routing use-cases: per-model fallback chains (the gateway rules), the routing notices,
//! legacy alias names and the client files a provider rename affects.

use std::path::Path;

use serde::{Deserialize, Serialize};
use spanbay_store::DbConnection as Connection;

use super::client_rotation::tidy_kept;
use super::client_writers::backup_root;
use super::providers::{ProviderModelView, llm_provider_save_guarded_impl};
use super::{ensure_catalog, with_connection};
use crate::app::{AppState, current_time_ms};
use crate::client_config::{self, ClientApp, Clock, DetectEnv};
use crate::error::AppError;
use crate::llm::capabilities::Capability;
use crate::llm::catalog::CatalogIndex;
use crate::llm::events::LlmEvent;
use crate::llm::router::{CallKind, Incompatible, RoutingSnapshot, check_fallback};
use crate::llm::runtime::build_snapshot;
use crate::llm::store::{self, ModelRef, Provider};

/// A capability the primary model is known to have and a fallback is known to lack.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FallbackCapability {
    /// Tool calls.
    Tools,
    /// Image input.
    Vision,
    /// Document input.
    Document,
}

/// A saved fallback that requests needing `capability` will skip (`FallbackWarningDto`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackWarning {
    /// The fallback's provider id.
    pub provider_id: String,
    /// The fallback's model id.
    pub model: String,
    /// What it lacks.
    pub capability: FallbackCapability,
}

/// The result of [`llm_model_fallbacks_set_impl`] (`FallbacksSaveResultDto`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FallbacksSaveResult {
    /// The model with its new chain.
    pub model: ProviderModelView,
    /// Capability gaps of the chain (saved anyway).
    pub warnings: Vec<FallbackWarning>,
}

/// One model the fallback picker offers (`FallbackCandidateDto`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FallbackCandidate {
    /// Provider id.
    pub provider_id: String,
    /// Model id.
    pub model: String,
    /// Whether it may be a fallback of the model asked about.
    pub compatible: bool,
    /// Why not: `self`, `kind`, `embedding_model` or `decision_protocol`.
    pub reason: Option<String>,
}

/// The call kind of a model, with the catalogue's mode for its id.
fn kind_of(
    snapshot: &RoutingSnapshot,
    catalog: &CatalogIndex,
    model: &ModelRef,
) -> Option<CallKind> {
    let mode = catalog.lookup(&model.model).map(|entry| entry.mode);
    snapshot.call_kind(&model.provider_id, &model.model, mode.as_deref())
}

fn incompatible_message(reason: Incompatible) -> &'static str {
    match reason {
        Incompatible::SelfRef => "a model cannot be its own fallback",
        Incompatible::Kind => "it answers another kind of call (chat, embedding or decision)",
        Incompatible::EmbeddingModel => {
            "an embedding model can only fall back to the same model on another provider"
        }
        Incompatible::DecisionProtocol => "it speaks another decision protocol",
    }
}

/// Replaces the fallback chain of a stored model (at most five, no self, no duplicate, each
/// compatible with the model's call kind: chat for chat, the same embedding model for an
/// embedding, the same decision protocol for a decision). A chat fallback known to lack tools,
/// vision or document input the model is known to have is saved with a warning.
///
/// An empty chain also deletes the rule of a model that is no longer stored (an orphan rule).
///
/// # Errors
/// [`AppError::NotFound`] for an unknown model, [`AppError::Validation`] for an unknown
/// fallback provider or an incompatible or malformed chain.
pub fn llm_model_fallbacks_set_impl(
    state: &AppState,
    provider_id: &str,
    model_id: &str,
    fallbacks: Vec<ModelRef>,
) -> Result<FallbacksSaveResult, AppError> {
    let primary = ModelRef {
        provider_id: provider_id.to_owned(),
        model: model_id.to_owned(),
    };
    let (view, warnings) = with_connection(state, |connection| {
        ensure_catalog(connection)?;
        // From the connection, not the runtime cache: this also works on the in-memory store.
        let snapshot = build_snapshot(connection)?;
        let Some(stored) = store::get_model(connection, provider_id, model_id)? else {
            // The rule of a deleted model can still be deleted.
            if fallbacks.is_empty() && snapshot.gateway_fallbacks.contains_key(&primary) {
                store::set_model_fallbacks(connection, &primary, &[])?;
                let gone = store::ProviderModel {
                    provider_id: provider_id.to_owned(),
                    model_id: model_id.to_owned(),
                    ..store::ProviderModel::default()
                };
                let catalog = CatalogIndex::load(connection)?;
                return Ok((ProviderModelView::new(gone, &catalog), Vec::new()));
            }
            return Err(AppError::not_found(format!("model {model_id} not found")));
        };
        let catalog = CatalogIndex::load(connection)?;
        let primary_kind = kind_of(&snapshot, &catalog, &primary)
            .ok_or_else(|| AppError::not_found(format!("provider {provider_id} not found")))?;
        let mut warnings = Vec::new();
        for fallback in &fallbacks {
            let shown = snapshot.display_ref(&fallback.provider_id, &fallback.model);
            let kind = kind_of(&snapshot, &catalog, fallback).ok_or_else(|| {
                AppError::validation(format!(
                    "fallback {shown}: provider {} not found",
                    fallback.provider_id
                ))
            })?;
            check_fallback((&primary, primary_kind), (fallback, kind)).map_err(|reason| {
                AppError::validation(format!(
                    "{shown} cannot be a fallback of {}: {}",
                    snapshot.display_ref(provider_id, model_id),
                    incompatible_message(reason)
                ))
            })?;
            let Some(other) = store::get_model(connection, &fallback.provider_id, &fallback.model)?
            else {
                continue;
            };
            for (capability, flag) in [
                (Capability::Tools, FallbackCapability::Tools),
                (Capability::Vision, FallbackCapability::Vision),
                (Capability::Document, FallbackCapability::Document),
            ] {
                if primary_kind == CallKind::Chat
                    && stored.capabilities.known(capability) == Some(true)
                    && other.capabilities.known_absent(capability)
                {
                    warnings.push(FallbackWarning {
                        provider_id: fallback.provider_id.clone(),
                        model: fallback.model.clone(),
                        capability: flag,
                    });
                }
            }
        }
        store::set_model_fallbacks(connection, &primary, &fallbacks)?;
        let model = store::get_model(connection, provider_id, model_id)?
            .ok_or_else(|| AppError::internal("the model vanished right after saving"))?;
        Ok((ProviderModelView::new(model, &catalog), warnings))
    })?;
    state.llm.touch();
    state.sync.notify_changed();
    state.llm.emit(LlmEvent::ModelsChanged {
        provider_id: provider_id.to_owned(),
    });
    Ok(FallbacksSaveResult {
        model: view,
        warnings,
    })
}

/// Every enabled model of every enabled provider, each saying whether it may be a fallback of
/// `provider_id`/`model_id` and why not.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown provider, [`AppError::Database`] on failure.
pub fn llm_model_fallback_candidates_impl(
    state: &AppState,
    provider_id: &str,
    model_id: &str,
) -> Result<Vec<FallbackCandidate>, AppError> {
    let primary = ModelRef {
        provider_id: provider_id.to_owned(),
        model: model_id.to_owned(),
    };
    let (snapshot, catalog) = with_connection(state, |connection| {
        ensure_catalog(connection)?;
        Ok((build_snapshot(connection)?, CatalogIndex::load(connection)?))
    })?;
    let primary_kind = kind_of(&snapshot, &catalog, &primary)
        .ok_or_else(|| AppError::not_found(format!("provider {provider_id} not found")))?;
    let mut candidates: Vec<FallbackCandidate> = snapshot
        .models
        .iter()
        .filter(|model| {
            model.enabled
                && snapshot
                    .providers
                    .get(&model.provider_id)
                    .is_some_and(|provider| provider.enabled)
        })
        .filter_map(|model| {
            let candidate = ModelRef {
                provider_id: model.provider_id.clone(),
                model: model.model_id.clone(),
            };
            let kind = kind_of(&snapshot, &catalog, &candidate)?;
            let verdict = check_fallback((&primary, primary_kind), (&candidate, kind));
            Some(FallbackCandidate {
                provider_id: candidate.provider_id,
                model: candidate.model,
                compatible: verdict.is_ok(),
                reason: verdict.err().map(|reason| reason.code().to_owned()),
            })
        })
        .collect();
    let position = |id: &str| snapshot.providers.get(id).map_or(i64::MAX, |p| p.position);
    candidates.sort_by(|a, b| {
        position(&a.provider_id)
            .cmp(&position(&b.provider_id))
            .then_with(|| a.provider_id.cmp(&b.provider_id))
            .then_with(|| a.model.cmp(&b.model))
    });
    Ok(candidates)
}

/// The routing notices (the v46 migration report, renamed providers), oldest first.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_routing_notices_impl(state: &AppState) -> Result<Vec<String>, AppError> {
    with_connection(state, store::list_routing_notices)
}

/// Dismisses every routing notice.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_routing_notices_dismiss_impl(state: &AppState) -> Result<(), AppError> {
    with_connection(state, store::clear_routing_notices)
}

// ------------------------------------------------------------------ legacy names

/// Forgets the legacy alias names once SpanBay has recorded at least one client config file and
/// none of the recorded files names any of them any more (they were all rewritten to model names).
/// Keeps them all while nothing is recorded (a hand-written config may still send an alias), while
/// any recorded file still names one, or when a recorded tool's config cannot be read.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub(super) fn prune_legacy_names(state: &AppState, home: &Path) -> Result<(), AppError> {
    let legacy = with_connection(state, store::list_legacy_names)?;
    if legacy.is_empty() {
        return Ok(());
    }
    let env = DetectEnv::system(home);
    let mut recorded_any = false;
    for app in ClientApp::ALL {
        let recorded = with_connection(state, |connection| {
            store::latest_applied_for_app(connection, app)
        })?;
        if recorded.is_empty() {
            continue;
        }
        recorded_any = true;
        match client_config::config_models(&env, app) {
            Ok(models)
                if models
                    .iter()
                    .any(|model| legacy.iter().any(|l| &l.name == model)) =>
            {
                return Ok(());
            }
            Ok(_) => {}
            Err(_) => return Ok(()),
        }
    }
    if !recorded_any {
        return Ok(());
    }
    llm_legacy_names_clear_impl(state)
}

/// One legacy alias name that still routes (`LegacyNameDto`): the alias and its model in name form.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LegacyNameView {
    /// The old alias name clients may still send.
    pub name: String,
    /// The model it calls, in name form (`<provider name>/<model>`).
    pub model: String,
}

/// The legacy alias names that still route, sorted by name.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_legacy_names_impl(state: &AppState) -> Result<Vec<LegacyNameView>, AppError> {
    with_connection(state, |connection| {
        let providers = store::list_providers(connection)?;
        let mut names: Vec<LegacyNameView> = store::list_legacy_names(connection)?
            .into_iter()
            .map(|legacy| {
                let provider = providers
                    .iter()
                    .find(|p| p.id == legacy.target.provider_id)
                    .map_or(legacy.target.provider_id.as_str(), |p| p.name.as_str());
                LegacyNameView {
                    model: format!("{provider}/{}", legacy.target.model),
                    name: legacy.name,
                }
            })
            .collect();
        names.sort_by(|a, b| a.name.cmp(&b.name));
        Ok(names)
    })
}

/// Forgets every legacy alias name (the user's "No longer needed").
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_legacy_names_clear_impl(state: &AppState) -> Result<(), AppError> {
    with_connection(state, |connection| {
        for legacy in store::list_legacy_names(connection)? {
            store::delete_legacy_name(connection, &legacy.name)?;
        }
        Ok(())
    })?;
    state.llm.touch();
    Ok(())
}

// ------------------------------------------------------------------ provider rename

/// A recorded client file whose model names a provider by the name about to change
/// (`RenameImpactDto`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameImpact {
    /// The gateway client of the file.
    pub client_id: String,
    /// Its name.
    pub client_name: String,
    /// The tool.
    pub app: ClientApp,
    /// The file.
    pub path: String,
    /// The model reference found, in the old name form.
    pub model: String,
}

/// A file [`llm_provider_rename_rewrite_impl`] could not update (`RenameRewriteFailureDto`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenameRewriteFailure {
    /// The file.
    pub path: String,
    /// Why.
    pub message: String,
}

/// The result of [`llm_provider_rename_rewrite_impl`] (`RenameRewriteResultDto`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RenameRewriteResult {
    /// The renamed provider.
    pub provider: Provider,
    /// Files now holding the new name form.
    pub rewritten: Vec<String>,
    /// Files that could not follow.
    pub failed: Vec<RenameRewriteFailure>,
}

/// The model `found` with its provider part replaced by `name`, when its provider part is the
/// provider's current name (case-insensitive) and not its id (an id-form reference survives a
/// rename).
fn renamed(found: &str, provider: &Provider, name: &str) -> Option<String> {
    let (left, model) = found.trim().split_once('/')?;
    let left = left.trim();
    (left.to_lowercase() == provider.name.trim().to_lowercase()
        && left != provider.id
        && name.trim().to_lowercase() != provider.name.trim().to_lowercase())
    .then(|| format!("{}/{model}", name.trim()))
}

fn rename_impact_in(
    state: &AppState,
    provider: &Provider,
    name: &str,
    home: &Path,
) -> Result<Vec<RenameImpact>, AppError> {
    let env = DetectEnv::system(home);
    let mut out = Vec::new();
    for app in ClientApp::ALL {
        let recorded = with_connection(state, |connection| {
            store::latest_applied_for_app(connection, app)
        })?;
        let Some(entry) = recorded.first() else {
            continue;
        };
        let Ok(models) = client_config::config_models(&env, app) else {
            continue;
        };
        let client_name = with_connection(state, |connection: &Connection| {
            store::get_client(connection, &entry.client_id)
        })
        .map(|client| client.name)
        .unwrap_or_default();
        for model in models {
            if renamed(&model, provider, name).is_some() {
                out.push(RenameImpact {
                    client_id: entry.client_id.clone(),
                    client_name: client_name.clone(),
                    app,
                    path: entry.file.path.clone(),
                    model,
                });
            }
        }
    }
    Ok(out)
}

fn provider_of(state: &AppState, provider_id: &str) -> Result<Provider, AppError> {
    with_connection(state, |connection| {
        store::get_provider(connection, provider_id)
    })?
    .ok_or_else(|| AppError::not_found(format!("provider {provider_id} not found")))
}

/// The recorded client files whose model names the provider by its current name, which renaming
/// it to `name` would break.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown provider.
pub fn llm_provider_rename_impact_impl(
    state: &AppState,
    provider_id: &str,
    name: &str,
) -> Result<Vec<RenameImpact>, AppError> {
    llm_provider_rename_impact_in(state, provider_id, name, &client_config::home_dir()?)
}

/// [`llm_provider_rename_impact_impl`] for an explicit home directory.
pub(crate) fn llm_provider_rename_impact_in(
    state: &AppState,
    provider_id: &str,
    name: &str,
    home: &Path,
) -> Result<Vec<RenameImpact>, AppError> {
    let provider = provider_of(state, provider_id)?;
    rename_impact_in(state, &provider, name, home)
}

/// Renames the provider to `name`, then rewrites the model reference in every file
/// [`llm_provider_rename_impact_impl`] lists to the new name form (backups and ledger rows as
/// any write). A file that cannot follow is reported, the rename stands.
///
/// # Errors
/// As [`llm_provider_save_impl`] (a taken name is a conflict); [`AppError::NotFound`] for an
/// unknown provider.
pub fn llm_provider_rename_rewrite_impl(
    state: &AppState,
    provider_id: &str,
    name: &str,
    keep_former_name: bool,
) -> Result<RenameRewriteResult, AppError> {
    let home = client_config::home_dir()?;
    llm_provider_rename_rewrite_in(state, provider_id, name, keep_former_name, &home)
}

/// [`llm_provider_rename_rewrite_impl`] for an explicit home directory.
pub(crate) fn llm_provider_rename_rewrite_in(
    state: &AppState,
    provider_id: &str,
    name: &str,
    keep_former_name: bool,
    home: &Path,
) -> Result<RenameRewriteResult, AppError> {
    let before = provider_of(state, provider_id)?;
    let impact = rename_impact_in(state, &before, name, home)?;
    let provider = llm_provider_save_guarded_impl(
        state,
        Provider {
            name: name.trim().to_owned(),
            ..before.clone()
        },
        store::SaveProviderOptions {
            keep_former_name,
            ..store::SaveProviderOptions::default()
        },
    )?;
    let root = backup_root(state, home);
    let mut rewritten = Vec::new();
    let mut failed = Vec::new();
    for item in &impact {
        let Some(new_model) = renamed(&item.model, &before, name) else {
            continue;
        };
        let entries = with_connection(state, |connection| {
            store::latest_applied_for_app(connection, item.app)
        })?;
        let files: Vec<_> = entries.iter().map(|entry| entry.file.clone()).collect();
        let report = client_config::rewrite_model(
            home,
            &root,
            item.app,
            &files,
            &item.model,
            &new_model,
            Clock::system(),
        )?;
        if !report.updated.is_empty() {
            with_connection(state, |connection| {
                store::record_applied(
                    connection,
                    &item.client_id,
                    item.app,
                    &report.updated,
                    current_time_ms(),
                )
                .map(|_| ())
            })?;
            tidy_kept(state, home, &[item.app]);
        }
        rewritten.extend(report.updated.into_iter().map(|file| file.path));
        failed.extend(
            report
                .failed
                .into_iter()
                .map(|(path, message)| RenameRewriteFailure { path, message }),
        );
    }
    rewritten.dedup();
    Ok(RenameRewriteResult {
        provider,
        rewritten,
        failed,
    })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn provider(id: &str, name: &str) -> Provider {
        Provider {
            id: id.into(),
            name: name.into(),
            ..Provider::default()
        }
    }

    #[test]
    fn only_the_old_name_form_is_renamed() {
        let hub = provider("hub", "My Hub");
        assert_eq!(
            renamed("my hub/gpt/4o", &hub, "Team Hub").as_deref(),
            Some("Team Hub/gpt/4o")
        );
        // The id form survives a rename, another provider's name is not ours, a case-only rename
        // still resolves.
        assert_eq!(renamed("hub/gpt", &hub, "Team Hub"), None);
        assert_eq!(renamed("Other/gpt", &hub, "Team Hub"), None);
        assert_eq!(renamed("My Hub/gpt", &hub, "MY HUB"), None);
        assert_eq!(renamed("legacy-alias", &hub, "Team Hub"), None);
    }
}
