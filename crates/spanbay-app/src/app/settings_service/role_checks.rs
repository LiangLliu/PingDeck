//! Validation checks for `ai.agent.role_models` and `ai.agent.fallbacks` when saving AI
//! settings. Extracted here so `settings_service` validates settings patches against routing and
//! catalog state without depending on `agent_service`.

use crate::agent::model::SubagentRole;
use crate::llm::capabilities::Capability;
use crate::llm::catalog::{self, CatalogIndex};
use crate::llm::router::{CallKind, RoutingSnapshot};
use crate::llm::runtime::build_snapshot;
use crate::llm::store::ModelRef;
use crate::settings::{AgentFallbacks, RoleModels};

use crate::app::AppState;
use crate::error::AppError;

/// The call kind of `model`, with the catalogue's mode for its id.
pub fn role_kind_of(
    snapshot: &RoutingSnapshot,
    catalog: &CatalogIndex,
    model: &ModelRef,
) -> Option<CallKind> {
    let mode = catalog.lookup(&model.model).map(|entry| entry.mode);
    snapshot.call_kind(&model.provider_id, &model.model, mode.as_deref())
}

/// Loads routing snapshot and catalog index for role and fallback checks (also on in-memory stores).
pub fn load_routing_facts(state: &AppState) -> Result<(RoutingSnapshot, CatalogIndex), AppError> {
    let store = state.store();
    let connection = store.connection();
    catalog::seed_bundled_if_empty(connection)?;
    Ok((build_snapshot(connection)?, CatalogIndex::load(connection)?))
}

/// Checks each newly selected role model in `next` against the routing facts: the model must
/// exist, answer chat calls, and not have a known-absent required capability.
///
/// # Errors
/// [`AppError::Validation`] naming the role's field.
pub(super) fn check_role_models(
    state: &AppState,
    current: &RoleModels,
    next: &RoleModels,
) -> Result<(), AppError> {
    let changed: Vec<(SubagentRole, &str)> = next
        .configured()
        .into_iter()
        .filter(|(role, model)| current.get(*role) != Some(*model))
        .collect();
    if changed.is_empty() {
        return Ok(());
    }
    let (snapshot, catalog) = load_routing_facts(state)?;
    for (role, reference) in changed {
        let field = format!("ai.agent.role_models.{}", role.as_str());
        let model = snapshot.resolve_ref(reference).ok_or_else(|| {
            AppError::validation(format!("{field}: model {reference} does not exist"))
        })?;
        let shown = snapshot.display_ref(&model.provider_id, &model.model);
        if role_kind_of(&snapshot, &catalog, &model) != Some(CallKind::Chat) {
            return Err(AppError::validation(format!(
                "{field}: {shown} is not a chat model"
            )));
        }
        let capabilities = snapshot
            .models
            .iter()
            .find(|m| m.provider_id == model.provider_id && m.model_id == model.model)
            .map(|m| m.capabilities)
            .unwrap_or_default();
        if role == SubagentRole::Vision && capabilities.known_absent(Capability::Vision) {
            return Err(AppError::validation(format!(
                "{field}: {shown} cannot read images (its vision test failed)"
            )));
        }
        if role == SubagentRole::Fast && capabilities.known_absent(Capability::Tools) {
            return Err(AppError::validation(format!(
                "{field}: {shown} cannot call tools"
            )));
        }
    }
    Ok(())
}

/// Checks the agent fallback chains `next` changes against the routing facts: every entry must
/// name a model of an existing provider that answers chat calls.
///
/// # Errors
/// [`AppError::Validation`] naming the site's field.
pub(super) fn check_agent_fallbacks(
    state: &AppState,
    current: &AgentFallbacks,
    next: &AgentFallbacks,
) -> Result<(), AppError> {
    let changed: Vec<(&str, &[String])> = next
        .sites()
        .into_iter()
        .zip(current.sites())
        .filter(|((_, chain), (_, before))| chain != before)
        .map(|(site, _)| site)
        .collect();
    if changed.iter().all(|(_, chain)| chain.is_empty()) {
        return Ok(());
    }
    let (snapshot, catalog) = load_routing_facts(state)?;
    for (site, chain) in changed {
        let field = format!("ai.agent.fallbacks.{site}");
        for reference in chain {
            let model = snapshot
                .resolve_ref(reference)
                .filter(|model| snapshot.providers.contains_key(&model.provider_id))
                .ok_or_else(|| {
                    AppError::validation(format!("{field}: model {reference} does not exist"))
                })?;
            if role_kind_of(&snapshot, &catalog, &model) != Some(CallKind::Chat) {
                let shown = snapshot.display_ref(&model.provider_id, &model.model);
                return Err(AppError::validation(format!(
                    "{field}: {shown} is not a chat model"
                )));
            }
        }
    }
    Ok(())
}
