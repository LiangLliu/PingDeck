//! The model roles of `task` sub-agents (`ai.agent.role_models`).
//!
//! Single responsibility: what Settings shows for each role (its model, whether that model or
//! its fallback chain can be called now, what is untested or unknown about it), the model the
//! capabilities and prices suggest for an empty role, the checks a save makes (also of the agent
//! fallback chains, `ai.agent.fallbacks`), and the model a sub-agent of a role calls. A role is
//! callable when its model or any entry of its chain plans; the run itself brings the chain to
//! the router (see `run::chat_target`).

use serde::{Deserialize, Serialize};
use spanbay_store::DbConnection as Connection;

use crate::agent::model::{AgentTarget, SubagentRole};
use crate::agent_tools::subagent::role_purpose;
use crate::error::AppError;
use crate::llm::capabilities::{Capability, ModelCapabilities};
use crate::llm::catalog::CatalogIndex;
use crate::llm::key_book::KeyBook;
use crate::llm::presets::ModelRole;
use crate::llm::router::{CallKind, PlanError, RequestNeeds, RoutingSnapshot};
use crate::llm::store::{self, ModelRef};
use crate::settings::RoleModels;

use crate::app::AppState;
use crate::app::settings_service::{load_routing_facts, role_kind_of};

/// Something Settings warns about on a role's model (`AgentRoleStatusDto.warnings`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RoleWarning {
    /// The vision role's model was never tested for image input.
    VisionUntested,
    /// The fast role's model was never tested for tool calls.
    ToolsUntested,
    /// The model has no price: the cost caps cannot see what it spends.
    NoPrice,
    /// The model is known to lack tool calls: its sub-agent answers once, without tools.
    NoTools,
}

/// One role as Settings shows it (`AgentRoleStatusDto`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRoleStatus {
    /// The role.
    pub role: SubagentRole,
    /// The configured model reference (id form), if any.
    pub model: Option<String>,
    /// The configured model in name form (`<provider name>/<model>`).
    pub model_label: Option<String>,
    /// The model the capabilities and prices suggest (id form); `None` when nothing fits.
    pub suggestion: Option<String>,
    /// The suggestion in name form.
    pub suggestion_label: Option<String>,
    /// Why the configured model cannot be called now; `None` when it can (or none is set).
    pub unavailable: Option<String>,
    /// What is untested or unknown about the configured model.
    pub warnings: Vec<RoleWarning>,
}

/// A model a role may be given: an enabled chat model of a routable provider with a key.
#[derive(Debug, Clone, PartialEq)]
pub(crate) struct Candidate {
    pub(crate) model: ModelRef,
    pub(crate) hint: Option<ModelRole>,
    pub(crate) capabilities: ModelCapabilities,
    /// Output price per million tokens, in the price's own currency; `None` when unpriced.
    pub(crate) output_price: Option<f64>,
}

fn supported(capabilities: &ModelCapabilities, capability: Capability) -> bool {
    capabilities.known(capability) == Some(true)
}

/// `candidates` ordered by output price, priced ones first (cheapest first, or dearest first with
/// `dearest`), then by reference so equal prices order the same way every time.
fn by_price(mut candidates: Vec<&Candidate>, dearest: bool) -> Vec<&Candidate> {
    candidates.sort_by(|a, b| {
        let order = match (a.output_price, b.output_price) {
            (Some(x), Some(y)) if dearest => y.total_cmp(&x),
            (Some(x), Some(y)) => x.total_cmp(&y),
            (Some(_), None) => std::cmp::Ordering::Less,
            (None, Some(_)) => std::cmp::Ordering::Greater,
            (None, None) => std::cmp::Ordering::Equal,
        };
        order.then_with(|| a.model.id_form().cmp(&b.model.id_form()))
    });
    candidates
}

/// The model the facts suggest for `role`, if any fits:
/// * fast: not known to lack tools; a model the provider tags fast or cheap, else the cheapest
///   priced one;
/// * deep: tagged reasoning or flagship and known to reason first, then any tagged or reasoning
///   one, dearest first;
/// * vision: known to read images, cheapest first.
pub(crate) fn suggest(role: SubagentRole, candidates: &[Candidate]) -> Option<&Candidate> {
    match role {
        SubagentRole::Fast => {
            let usable: Vec<&Candidate> = candidates
                .iter()
                .filter(|c| !c.capabilities.known_absent(Capability::Tools))
                .collect();
            let tagged: Vec<&Candidate> = usable
                .iter()
                .copied()
                .filter(|c| matches!(c.hint, Some(ModelRole::Fast | ModelRole::Cheap)))
                .collect();
            by_price(tagged, false).first().copied().or_else(|| {
                by_price(usable, false)
                    .into_iter()
                    .find(|c| c.output_price.is_some())
            })
        }
        SubagentRole::Deep => {
            let tagged =
                |c: &Candidate| matches!(c.hint, Some(ModelRole::Reasoning | ModelRole::Flagship));
            let reasons = |c: &Candidate| supported(&c.capabilities, Capability::Reasoning);
            let both: Vec<&Candidate> = candidates
                .iter()
                .filter(|c| tagged(c) && reasons(c))
                .collect();
            let either: Vec<&Candidate> = candidates
                .iter()
                .filter(|c| tagged(c) || reasons(c))
                .collect();
            by_price(both, true)
                .first()
                .copied()
                .or_else(|| by_price(either, true).first().copied())
        }
        SubagentRole::Vision => by_price(
            candidates
                .iter()
                .filter(|c| supported(&c.capabilities, Capability::Vision))
                .collect(),
            false,
        )
        .first()
        .copied(),
    }
}

/// Every model a role may be given.
fn candidates(
    connection: &Connection,
    snapshot: &RoutingSnapshot,
    catalog: &CatalogIndex,
) -> Result<Vec<Candidate>, AppError> {
    let mut out = Vec::new();
    for stored in snapshot.models.iter().filter(|model| model.enabled) {
        let routable = snapshot
            .providers
            .get(&stored.provider_id)
            .is_some_and(|provider| provider.routable());
        let has_key = snapshot
            .keys
            .get(&stored.provider_id)
            .is_some_and(|keys| keys.iter().any(|key| key.key.enabled));
        let model = ModelRef {
            provider_id: stored.provider_id.clone(),
            model: stored.model_id.clone(),
        };
        if !routable || !has_key || role_kind_of(snapshot, catalog, &model) != Some(CallKind::Chat)
        {
            continue;
        }
        let output_price = store::effective_price(connection, &model.provider_id, &model.model)?
            .map(|price| price.output_per_mtok);
        out.push(Candidate {
            model,
            hint: stored.role,
            capabilities: stored.capabilities,
            output_price,
        });
    }
    Ok(out)
}

/// Why `reference` and its fallback `chain` cannot be called now, from a plan that moves no key
/// cursor; `None` when the model or an entry of the chain can. Keys that are only cooling down do
/// not make a model unavailable.
fn unavailable(
    state: &AppState,
    snapshot: &RoutingSnapshot,
    reference: &str,
    chain: &[String],
) -> Option<String> {
    match snapshot.plan_with_fallbacks(
        reference,
        chain,
        &RequestNeeds::default(),
        &state.llm().health,
        &KeyBook::new(),
    ) {
        Ok(_) | Err(PlanError::NoTarget { .. }) => None,
        Err(error) => Some(error.answer().message),
    }
}

/// Every role, in order fast, deep, vision, as Settings shows it.
///
/// # Errors
/// [`AppError::Database`] when the routing facts cannot be read.
pub fn agent_roles_get_impl(state: &AppState) -> Result<Vec<AgentRoleStatus>, AppError> {
    let settings = state.settings();
    let roles = &settings.ai.agent.role_models;
    let (snapshot, catalog) = load_routing_facts(state)?;
    let (pool, prices) = {
        let store = state.store();
        let connection = store.connection();
        let pool = candidates(connection, &snapshot, &catalog)?;
        let mut prices = Vec::new();
        for (_, reference) in roles.configured() {
            if let Some(model) = snapshot.resolve_ref(reference) {
                let price = store::effective_price(connection, &model.provider_id, &model.model)?;
                prices.push((reference.to_owned(), price.is_some()));
            }
        }
        (pool, prices)
    };
    Ok(SubagentRole::ALL
        .into_iter()
        .map(|role| {
            let suggestion = suggest(role, &pool).map(|c| &c.model);
            let mut status = AgentRoleStatus {
                role,
                model: None,
                model_label: None,
                suggestion: suggestion.map(ModelRef::id_form),
                suggestion_label: suggestion
                    .map(|model| snapshot.display_ref(&model.provider_id, &model.model)),
                unavailable: None,
                warnings: Vec::new(),
            };
            let Some(reference) = roles.get(role) else {
                return status;
            };
            status.model = Some(reference.to_owned());
            let chain = settings.ai.agent.fallbacks.for_role(Some(role));
            // A model that is gone still leaves its fallback chain to answer.
            let Some(model) = snapshot.resolve_ref(reference) else {
                status.model_label = Some(reference.to_owned());
                status.unavailable = unavailable(state, &snapshot, reference, chain)
                    .map(|_| format!("model {reference} does not exist"));
                return status;
            };
            status.model_label = Some(snapshot.display_ref(&model.provider_id, &model.model));
            status.unavailable = unavailable(state, &snapshot, &model.id_form(), chain);
            let capabilities = snapshot
                .models
                .iter()
                .find(|m| m.provider_id == model.provider_id && m.model_id == model.model)
                .map(|m| m.capabilities)
                .unwrap_or_default();
            if role == SubagentRole::Vision && capabilities.known(Capability::Vision).is_none() {
                status.warnings.push(RoleWarning::VisionUntested);
            }
            if role == SubagentRole::Fast && capabilities.known(Capability::Tools).is_none() {
                status.warnings.push(RoleWarning::ToolsUntested);
            }
            if capabilities.known_absent(Capability::Tools) {
                status.warnings.push(RoleWarning::NoTools);
            }
            if prices
                .iter()
                .any(|(priced, has)| priced == reference && !has)
            {
                status.warnings.push(RoleWarning::NoPrice);
            }
            status
        })
        .collect())
}

/// The part of the main agent's system prompt that names each role's model and says how to split
/// work between them; empty when no role has a model.
pub(super) async fn roles_prompt_section(state: &AppState, roles: &RoleModels) -> String {
    let configured = roles.configured();
    if configured.is_empty() {
        return String::new();
    }
    let snapshot = state.llm().snapshot().await.ok();
    let mut lines = vec![
        "Sub-agent roles (the `role` of `task`), each running on a model of its own:".to_owned(),
    ];
    for (role, reference) in &configured {
        let model = snapshot.as_ref().and_then(|s| s.resolve_ref(reference));
        let label = match (&snapshot, &model) {
            (Some(snapshot), Some(model)) => snapshot.display_ref(&model.provider_id, &model.model),
            _ => (*reference).to_owned(),
        };
        let toolless = match (&snapshot, &model) {
            (Some(snapshot), Some(model)) => snapshot.models.iter().any(|m| {
                m.provider_id == model.provider_id
                    && m.model_id == model.model
                    && m.capabilities.known_absent(Capability::Tools)
            }),
            _ => false,
        };
        let note = if toolless {
            ", no tools: it answers once, so put everything it needs in `prompt`"
        } else {
            ""
        };
        lines.push(format!(
            "- {} ({label}{note}): {}.",
            role.as_str(),
            role_purpose(*role)
        ));
    }
    let mut rules = vec![
        "Put jobs that do not depend on each other in one turn so they run at the same time; chain dependent ones over turns.",
        "Check every report yourself before you use it.",
    ];
    if roles.get(SubagentRole::Deep).is_some() {
        rules.push("Give a conclusion you doubt to deep for review.");
    }
    rules.push("Do one- or two-step things yourself instead of delegating them.");
    let default = if roles.get(SubagentRole::Fast).is_some() {
        "Without `role` a sub-agent uses your model, except an explore job, which uses fast."
    } else {
        "Without `role` a sub-agent uses your model."
    };
    rules.push(default);
    lines.push(rules.join(" "));
    lines.join("\n")
}

/// The model a sub-agent of `role` calls, as a session target, or why it cannot be called now
/// (the text the parent model reads).
///
/// # Errors
/// [`AppError::Validation`] when the role has no model or its model cannot be called.
pub(crate) async fn role_target(
    state: &AppState,
    role: SubagentRole,
) -> Result<(AgentTarget, String), AppError> {
    let name = role.as_str();
    let settings = state.settings();
    let agent = &settings.ai.agent;
    let reference = agent
        .role_models
        .get(role)
        .map(str::to_owned)
        .ok_or_else(|| AppError::validation(format!("the {name} role has no model")))?;
    let snapshot = state.llm().snapshot().await?;
    let chain = agent.fallbacks.for_role(Some(role));
    let model = snapshot.resolve_ref(&reference);
    let (name_form, label) = match &model {
        Some(model) => (
            model.id_form(),
            snapshot.display_ref(&model.provider_id, &model.model),
        ),
        None => (reference.clone(), reference.clone()),
    };
    // A model that is gone still leaves its fallback chain to answer.
    if let Some(reason) = unavailable(state, &snapshot, &name_form, chain) {
        let reason = if model.is_none() {
            format!("model {reference} does not exist")
        } else {
            reason
        };
        return Err(AppError::validation(format!(
            "the {name} role is unavailable: {reason}. Do it yourself or use another role"
        )));
    }
    // C-S1: an imported provider is no role model until the user reviewed it.
    if let Some(provider) = snapshot.awaiting_review(&name_form) {
        return Err(AppError::validation(format!(
            "the {name} role is unavailable: provider {provider} was imported and is not reviewed yet. Do it yourself or use another role"
        )));
    }
    Ok((AgentTarget::Named { name: name_form }, label))
}

#[cfg(test)]
mod tests {
    use super::*;

    fn candidate(id: &str, hint: Option<ModelRole>, price: Option<f64>) -> Candidate {
        Candidate {
            model: ModelRef {
                provider_id: "p".to_owned(),
                model: id.to_owned(),
            },
            hint,
            capabilities: ModelCapabilities::default(),
            output_price: price,
        }
    }

    fn with(mut c: Candidate, capability: Capability, supported: bool) -> Candidate {
        c.capabilities.apply(
            capability,
            supported,
            crate::llm::capabilities::CapabilitySource::Measured,
        );
        c
    }

    fn picked(role: SubagentRole, pool: &[Candidate]) -> Option<String> {
        suggest(role, pool).map(|c| c.model.model.clone())
    }

    #[test]
    fn fast_prefers_a_model_tagged_fast_or_cheap_then_the_cheapest_priced_one() {
        let pool = [
            candidate("big", None, Some(10.0)),
            candidate("small", None, Some(1.0)),
            candidate("tagged", Some(ModelRole::Fast), Some(5.0)),
        ];
        assert_eq!(picked(SubagentRole::Fast, &pool).as_deref(), Some("tagged"));
        assert_eq!(
            picked(SubagentRole::Fast, &pool[..2]).as_deref(),
            Some("small")
        );
        // Without a price or a tag nothing is guessed.
        assert_eq!(
            picked(SubagentRole::Fast, &[candidate("x", None, None)]),
            None
        );
        // A model measured without tools is never the fast one.
        let toolless = with(
            candidate("cheap", Some(ModelRole::Cheap), Some(0.1)),
            Capability::Tools,
            false,
        );
        assert_eq!(
            picked(
                SubagentRole::Fast,
                &[toolless, candidate("ok", None, Some(2.0))]
            )
            .as_deref(),
            Some("ok")
        );
    }

    #[test]
    fn deep_prefers_a_tagged_reasoning_model_dearest_first() {
        let reasoning = |id: &str, hint, price| {
            with(
                candidate(id, hint, Some(price)),
                Capability::Reasoning,
                true,
            )
        };
        let pool = [
            reasoning("r-cheap", None, 2.0),
            reasoning("flag", Some(ModelRole::Flagship), 15.0),
            reasoning("think", Some(ModelRole::Reasoning), 8.0),
            candidate("plain", None, Some(40.0)),
        ];
        assert_eq!(picked(SubagentRole::Deep, &pool).as_deref(), Some("flag"));
        assert_eq!(
            picked(SubagentRole::Deep, &[pool[0].clone(), pool[3].clone()]).as_deref(),
            Some("r-cheap")
        );
        assert_eq!(picked(SubagentRole::Deep, &pool[3..]), None);
    }

    #[test]
    fn vision_takes_the_cheapest_model_known_to_read_images() {
        let seeing = |id: &str, price| with(candidate(id, None, price), Capability::Vision, true);
        let pool = [
            seeing("dear", Some(9.0)),
            seeing("unpriced", None),
            seeing("cheap", Some(3.0)),
            candidate("blind", None, Some(0.5)),
        ];
        assert_eq!(
            picked(SubagentRole::Vision, &pool).as_deref(),
            Some("cheap")
        );
        assert_eq!(picked(SubagentRole::Vision, &pool[3..]), None);
    }
}
