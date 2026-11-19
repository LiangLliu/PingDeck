//! What a run needs to know about its model: context window, price, capabilities.
//!
//! Single responsibility: resolve a chat's [`AgentTarget`] to the provider model it will call (a
//! model reference takes its first routable target) and read that model's facts
//! from the store and the catalog: a value the provider model row holds beats the catalog row,
//! which beats unknown. Anything unknown stays unknown: no price means no cost cap, no
//! capabilities mean no native web search and PDFs sent as text.

use crate::agent::model::AgentTarget;
use crate::agent::runner::ModelInfo;
use crate::llm::catalog;
use crate::llm::key_book::KeyBook;
use crate::llm::router::RequestNeeds;
use crate::llm::store;
use crate::llm::usage::{PriceResolution, UnknownReason};

use crate::app::AppState;

/// The provider model `target` calls first, if it can be resolved.
async fn first_model(state: &AppState, target: &AgentTarget) -> Option<(String, String)> {
    match target {
        AgentTarget::Pinned {
            provider_id, model, ..
        } => Some((provider_id.clone(), model.clone())),
        AgentTarget::Named { name } => {
            let snapshot = state.llm().snapshot().await.ok()?;
            // A throwaway key book: only the first provider model matters, so no round-robin
            // cursor moves for a lookup.
            let plan = snapshot
                .plan(
                    name,
                    &RequestNeeds::default(),
                    None,
                    &state.llm().health,
                    &KeyBook::new(),
                )
                .ok()?;
            plan.groups
                .into_iter()
                .next()
                .map(|group| (group.provider_id, group.model))
        }
    }
}

/// The facts about the model `target` calls; every field falls back to unknown.
pub(super) async fn resolve_model(state: &AppState, target: &AgentTarget) -> ModelInfo {
    let Some((provider_id, model)) = first_model(state, target).await else {
        return ModelInfo::default();
    };
    model_facts(state, provider_id, model).await
}

/// The facts about `provider_id`/`model` (also a fallback that answered instead of the run's
/// model); every field falls back to unknown.
pub(super) async fn model_facts(state: &AppState, provider_id: String, model: String) -> ModelInfo {
    let state = state.clone();
    tokio::task::spawn_blocking(move || {
        let store = state.store();
        let connection = store.connection();
        // A fresh database has no catalog yet; seeding is a no-op once it has one.
        let _ = catalog::seed_bundled_if_empty(connection);
        let stored = store::get_model(connection, &provider_id, &model)
            .ok()
            .flatten();
        let listed = catalog::lookup(connection, &model).ok().flatten();
        let free = store::get_provider(connection, &provider_id)
            .ok()
            .flatten()
            .is_some_and(|provider| crate::llm::usage::is_free_provider(&provider));
        let price = match store::effective_price(connection, &provider_id, &model) {
            Ok(Some(price)) => PriceResolution::Known(price),
            _ if free => PriceResolution::Free,
            _ if listed.is_some() => PriceResolution::Unknown(UnknownReason::CatalogWithoutPrice),
            _ => PriceResolution::Unknown(UnknownReason::NotInCatalog),
        };
        ModelInfo {
            context_limit: stored
                .as_ref()
                .and_then(|stored| stored.context)
                .or_else(|| listed.and_then(|entry| entry.max_input_tokens)),
            price,
            capabilities: stored.map(|stored| stored.capabilities).unwrap_or_default(),
        }
    })
    .await
    .unwrap_or_default()
}
