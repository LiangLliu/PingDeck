//! What a provider or model change touches, shown before the user commits to it: the dependants
//! of a delete, the reach of a rename, the order of the next request, the
//! state of each gateway rule, and the review mark of an imported provider.
//!
//! Each kind of dependant is read on its own; one that cannot be read is named in `unknown`
//! instead of coming back empty ("could not read" must never look like "none").

use pingdeck_store::DbConnection as Connection;
use serde::{Deserialize, Serialize};

use super::routing::{RenameImpact, llm_provider_rename_impact_in};
use super::with_connection;
use crate::app::{AppState, current_time_ms};
use crate::client_config;
use crate::error::AppError;
use crate::llm::events::LlmEvent;
use crate::llm::router::{RequestNeeds, RoutingSnapshot, RuleStatus};
use crate::llm::store::{self, ModelRef};

/// A gateway client a change reaches (`ImpactClientDto`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImpactClient {
    /// Client id.
    pub id: String,
    /// Its name.
    pub name: String,
    /// Every model it may use is among the affected ones: after a delete it is refused.
    pub only: bool,
}

/// The dependants of a provider or one of its models (`ImpactDto`).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct Impact {
    /// Clients whose allowed models or default model name it.
    pub clients: Vec<ImpactClient>,
    /// Gateway rules that name it, by primary model.
    pub gateway_rules: Vec<ModelRef>,
    /// Agent fallback chains (`main`, `fast`, `deep`, `vision`) that name it.
    pub agent_chains: Vec<String>,
    /// Sub-agent roles (`fast`, `deep`, `vision`) whose model it is.
    pub role_models: Vec<String>,
    /// Defaults that point at it (`agent`).
    pub default_targets: Vec<String>,
    /// Its keys (0 for a model).
    pub keys: u32,
    /// Its stored models (1 for a stored model).
    pub models: u32,
    /// The kinds above that could not be read.
    pub unknown: Vec<String>,
}

/// Whether `entry` (`provider_id/model`, or a name form) names the provider (`model` `None`) or
/// the model.
fn names(snapshot: &RoutingSnapshot, entry: &str, provider_id: &str, model: Option<&str>) -> bool {
    snapshot.resolve_ref(entry).is_some_and(|found| {
        found.provider_id == provider_id && model.is_none_or(|model| found.model == model)
    })
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

fn impact_of(state: &AppState, provider_id: &str, model: Option<&str>) -> Result<Impact, AppError> {
    let snapshot = with_connection(state, |connection| {
        if store::get_provider(connection, provider_id)?.is_none() {
            return Err(AppError::not_found(format!(
                "provider {provider_id} not found"
            )));
        }
        crate::llm::runtime::build_snapshot(connection)
    })?;
    let hits = |entry: &str| names(&snapshot, entry, provider_id, model);
    let mut impact = Impact::default();
    match with_connection(state, store::list_clients) {
        Ok(clients) => {
            for client in clients.into_iter().filter(|c| c.revoked_at_ms.is_none()) {
                let listed = client.allowed_models.iter().filter(|m| hits(m)).count();
                let default = client.default_model.as_deref().is_some_and(hits);
                if listed > 0 || default {
                    impact.clients.push(ImpactClient {
                        only: !client.allow_all
                            && client.allowed_aliases.is_empty()
                            && listed == client.allowed_models.len(),
                        id: client.id,
                        name: client.name,
                    });
                }
            }
        }
        Err(_) => impact.unknown.push("clients".to_owned()),
    }
    let mut rules: Vec<ModelRef> = snapshot
        .gateway_fallbacks
        .iter()
        .filter(|(primary, chain)| {
            (primary.provider_id == provider_id && model.is_none_or(|m| primary.model == m))
                || chain.iter().any(|target| {
                    target.provider_id == provider_id && model.is_none_or(|m| target.model == m)
                })
        })
        .map(|(primary, _)| primary.clone())
        .collect();
    rules.sort_by_key(ModelRef::id_form);
    impact.gateway_rules = rules;
    let settings = state.settings();
    let agent = &settings.ai.agent;
    for (name, chain) in [
        ("main", &agent.fallbacks.main),
        ("fast", &agent.fallbacks.fast),
        ("deep", &agent.fallbacks.deep),
        ("vision", &agent.fallbacks.vision),
    ] {
        if chain.iter().any(|entry| hits(entry)) {
            impact.agent_chains.push(name.to_owned());
        }
    }
    for (name, role) in [
        ("fast", &agent.role_models.fast),
        ("deep", &agent.role_models.deep),
        ("vision", &agent.role_models.vision),
    ] {
        if role.as_deref().is_some_and(hits) {
            impact.role_models.push(name.to_owned());
        }
    }
    match serde_json::to_value(&agent.default_target) {
        Ok(target) => {
            let pinned = target["kind"] == "pinned"
                && target["provider_id"] == provider_id
                && model.is_none_or(|m| target["model"] == m);
            let named = target["kind"] == "named" && target["name"].as_str().is_some_and(hits);
            if pinned || named {
                impact.default_targets.push("agent".to_owned());
            }
        }
        Err(_) => impact.unknown.push("default_targets".to_owned()),
    }
    match model {
        Some(model) => {
            impact.models = count(usize::from(snapshot.has_model(&ModelRef {
                provider_id: provider_id.to_owned(),
                model: model.to_owned(),
            })))
        }
        None => {
            impact.models = count(
                snapshot
                    .models
                    .iter()
                    .filter(|m| m.provider_id == provider_id)
                    .count(),
            );
            match with_connection(state, |connection| {
                store::list_keys(connection, Some(provider_id))
            }) {
                Ok(keys) => impact.keys = count(keys.len()),
                Err(_) => impact.unknown.push("keys".to_owned()),
            }
        }
    }
    Ok(impact)
}

/// What deleting the provider takes with it or leaves dangling: the same references the
/// delete clears.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown provider.
pub fn llm_provider_impact_impl(state: &AppState, provider_id: &str) -> Result<Impact, AppError> {
    impact_of(state, provider_id, None)
}

/// [`llm_provider_impact_impl`] for one model of the provider.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown provider.
pub fn llm_model_impact_impl(
    state: &AppState,
    provider_id: &str,
    model_id: &str,
) -> Result<Impact, AppError> {
    impact_of(state, provider_id, Some(model_id))
}

/// A caller that used a provider's current name lately (`RecentCallerDto`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecentCaller {
    /// The client's name; empty for a call without a client token.
    pub name: String,
    /// Calls in the window.
    pub calls: u32,
}

/// The reach of a rename (`RenamePreviewDto`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RenamePreview {
    /// A model reference as clients send it today (`Old/model`).
    pub external_before: String,
    /// The same after the rename (`New/model`).
    pub external_after: String,
    /// Recorded client files that name the provider by its current name.
    pub files: Vec<RenameImpact>,
    /// Who called it by its current name in the last [`RECENT_DAYS`] days.
    pub recent_callers: Vec<RecentCaller>,
    /// The id of the provider that already answers to the new name (rename refused), if any.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub name_taken_by: Option<String>,
    /// `files` or `recent_callers` that could not be read.
    pub unknown: Vec<String>,
}

/// The window of [`RenamePreview::recent_callers`].
pub const RECENT_DAYS: i64 = 7;

/// What renaming the provider to `name` reaches: the client files that name it by its
/// current name, who called it so in the last week, and whether the name is free. Reads files:
/// run it off the async runtime.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown provider.
pub fn llm_provider_rename_preview_impl(
    state: &AppState,
    provider_id: &str,
    name: &str,
) -> Result<RenamePreview, AppError> {
    let (provider, sample, holder) = with_connection(state, |connection: &Connection| {
        let provider = store::get_provider(connection, provider_id)?
            .ok_or_else(|| AppError::not_found(format!("provider {provider_id} not found")))?;
        let sample = store::list_models(connection, Some(provider_id))?
            .into_iter()
            .find(|model| model.enabled)
            .map_or_else(|| "model".to_owned(), |model| model.model_id);
        let holder = store::provider_name_holder(connection, provider_id, name)?;
        Ok((provider, sample, holder))
    })?;
    let mut unknown = Vec::new();
    let files = client_config::home_dir()
        .and_then(|home| llm_provider_rename_impact_in(state, provider_id, name, &home))
        .unwrap_or_else(|_| {
            unknown.push("files".to_owned());
            Vec::new()
        });
    let since = current_time_ms() - RECENT_DAYS * 24 * 60 * 60 * 1000;
    let recent_callers = with_connection(state, |connection| {
        store::recent_callers(connection, &provider.name, since)
    })
    .map(|rows| {
        rows.into_iter()
            .map(|(name, calls)| RecentCaller {
                name,
                calls: u32::try_from(calls).unwrap_or(u32::MAX),
            })
            .collect()
    })
    .unwrap_or_else(|_| {
        unknown.push("recent_callers".to_owned());
        Vec::new()
    });
    Ok(RenamePreview {
        external_before: format!("{}/{sample}", provider.name.trim()),
        external_after: format!("{}/{sample}", name.trim()),
        files,
        recent_callers,
        name_taken_by: holder,
        unknown,
    })
}

/// Clears the "imported, not reviewed" mark: the provider may serve as a fallback again.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown provider.
pub fn llm_provider_mark_reviewed_impl(
    state: &AppState,
    provider_id: &str,
) -> Result<(), AppError> {
    let cleared = with_connection(state, |connection| {
        store::mark_reviewed(connection, provider_id)
    })?;
    if cleared {
        state.llm.touch();
        state.sync.notify_changed();
        state.llm.emit(LlmEvent::ProvidersChanged);
    }
    Ok(())
}

/// One model of the route preview, in chain order (`RoutePreviewItemDto`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutePreviewItem {
    /// Provider id.
    pub provider_id: String,
    /// Provider name.
    pub provider_name: String,
    /// Model id.
    pub model: String,
    /// The names of the keys it would try, in order.
    pub keys_order: Vec<String>,
    /// Keys left out, as `name: reason`.
    pub keys_skipped: Vec<String>,
    /// `first` (tried first), `fallback` (tried after a failure) or `skip`.
    pub verdict: String,
    /// Why it is skipped (the log's skip words: `disabled`, `no key`, `not reviewed`…).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub skip_reason: Option<String>,
}

/// The order the next request for a model would take (`RoutePreviewDto`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RoutePreview {
    /// The name asked about.
    pub requested: String,
    /// The primary, then its gateway rule, in order.
    pub items: Vec<RoutePreviewItem>,
    /// Upstream attempts one request may make (the gateway's retries plus one).
    pub max_attempts: u32,
    /// Why nothing would be tried (unknown name, not allowed for the client…), if so.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// The order of the next gateway request for `requested`, as `client_id` would get it
/// (`None`: a caller without a client scope). Read-only: no round-robin cursor moves.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown client.
pub fn llm_route_preview_impl(
    state: &AppState,
    requested: &str,
    client_id: Option<&str>,
) -> Result<RoutePreview, AppError> {
    let (snapshot, client) = with_connection(state, |connection| {
        let client = client_id
            .map(|id| store::get_client(connection, id))
            .transpose()?;
        Ok((crate::llm::runtime::build_snapshot(connection)?, client))
    })?;
    let scope = |name: &str| client.as_ref().is_none_or(|client| client.allows(name));
    let planned = snapshot.plan_preview(
        requested,
        &RequestNeeds::default(),
        client.as_ref().map(|_| &scope as &dyn Fn(&str) -> bool),
        &state.llm.health,
        &state.llm.keys,
    );
    let max_attempts = u32::from(state.settings().ai.gateway.max_retries) + 1;
    let (groups, skipped, error) = match planned {
        Ok(plan) => (plan.groups, plan.skipped, None),
        Err(failure) => (
            Vec::new(),
            failure.skipped,
            Some(failure.error.answer().message),
        ),
    };
    let key_name = |provider_id: &str, key_id: &str| {
        snapshot
            .keys
            .get(provider_id)
            .and_then(|keys| keys.iter().find(|key| key.key.id == key_id))
            .map_or_else(|| key_id.to_owned(), |key| key.key.name.clone())
    };
    let mut chain: Vec<ModelRef> = Vec::new();
    if let Some(primary) = snapshot.resolve_ref(requested) {
        chain.extend(
            snapshot
                .gateway_fallbacks
                .get(&primary)
                .cloned()
                .unwrap_or_default(),
        );
        chain.insert(0, primary);
    }
    let mut items = Vec::new();
    let mut first = true;
    for model in chain {
        let same =
            |provider_id: &str, id: &str| provider_id == model.provider_id && id == model.model;
        let keys_skipped: Vec<String> = skipped
            .iter()
            .filter(|s| same(&s.provider_id, &s.model) && s.key_id.is_some())
            .map(|s| {
                format!(
                    "{}: {}",
                    key_name(&s.provider_id, s.key_id.as_deref().unwrap_or_default()),
                    s.reason
                )
            })
            .collect();
        let group = groups.iter().find(|g| same(&g.provider_id, &g.model));
        let whole = skipped
            .iter()
            .find(|s| same(&s.provider_id, &s.model) && s.key_id.is_none());
        let verdict = match (group, first) {
            (Some(_), true) => "first",
            (Some(_), false) => "fallback",
            (None, _) => "skip",
        };
        first &= group.is_none();
        items.push(RoutePreviewItem {
            provider_name: snapshot
                .providers
                .get(&model.provider_id)
                .map_or_else(|| model.provider_id.clone(), |p| p.name.clone()),
            keys_order: group
                .map(|g| {
                    g.targets
                        .iter()
                        .map(|t| key_name(&t.provider_id, &t.key_id))
                        .collect()
                })
                .unwrap_or_default(),
            keys_skipped,
            verdict: verdict.to_owned(),
            skip_reason: group
                .is_none()
                .then(|| whole.map_or_else(|| "circuit open".to_owned(), |s| s.reason.clone())),
            provider_id: model.provider_id,
            model: model.model,
        });
    }
    Ok(RoutePreview {
        requested: requested.trim().to_owned(),
        items,
        max_attempts,
        error,
    })
}

/// One gateway rule with how its models stand (`GatewayRuleDto`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayRule {
    /// The primary model.
    pub primary: ModelRef,
    /// Its fallbacks, in order.
    pub fallbacks: Vec<ModelRef>,
    /// How the primary stands.
    pub status: RuleStatus,
    /// How each fallback stands, in the order of `fallbacks`.
    pub fallback_status: Vec<RuleStatus>,
    /// The primary is no stored model any more (deleted): the rule can only be deleted.
    pub orphan: bool,
}

/// Every gateway rule with its resolution state; a rule whose primary model was deleted is
/// listed too, as an orphan.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_gateway_rules_impl(state: &AppState) -> Result<Vec<GatewayRule>, AppError> {
    let snapshot = with_connection(state, crate::llm::runtime::build_snapshot)?;
    let health = &state.llm.health;
    let mut rules: Vec<GatewayRule> = snapshot
        .gateway_fallbacks
        .iter()
        .map(|(primary, chain)| GatewayRule {
            status: snapshot.rule_status(primary, health),
            fallback_status: chain
                .iter()
                .map(|m| snapshot.rule_status(m, health))
                .collect(),
            orphan: !snapshot.has_model(primary),
            primary: primary.clone(),
            fallbacks: chain.clone(),
        })
        .collect();
    rules.sort_by_key(|rule| rule.primary.id_form());
    Ok(rules)
}

#[cfg(test)]
#[path = "impact_tests.rs"]
mod tests;
