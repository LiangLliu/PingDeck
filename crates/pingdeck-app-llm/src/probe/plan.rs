//! From a [`ProbeRunInput`] to rows, a plan and the targets to call.
//!
//! Single responsibility: resolve keys and models against the routing snapshot, derive each
//! row's kind and declared context, and build the plan of a new run, a retry or a resume with
//! the planner of `llm::probe::plan`. Reads only; the run and the estimate both
//! start here, so they can never disagree about what a run touches.

use std::collections::{BTreeMap, HashMap, HashSet};

use super::spec::{RunDoc, RunSpec, protocols_of};
use super::types::{MAX_PROBE_ITEMS, ProbeItem, ProbeRunInput};
use crate::app::AppState;
use crate::error::AppError;
use crate::llm::capabilities::Capability;
use crate::llm::catalog::CatalogIndex;
use crate::llm::probe::listing::{ModelKind, model_kind};
use crate::llm::probe::plan::{
    CheckSelection, CheckSet, PlanItem, ProbePlan, plan_remaining, plan_retry, plan_run,
};
use crate::llm::probe::run::RunItem;
use crate::llm::probe::{ProbeOptions, ProbeTest};
use crate::llm::router::RoutingSnapshot;
use crate::llm::sniff::SniffReport;
use crate::llm::store;
use crate::llm::wire::Protocol;
use crate::{ensure_catalog, with_db_blocking};

/// A run ready to be estimated or started.
pub(super) struct Planned {
    /// The rows the plan indexes.
    pub items: Vec<PlanItem>,
    /// The declared context window of each row, for the options of its checks.
    pub contexts: Vec<Option<u64>>,
    /// What to run.
    pub plan: ProbePlan,
    /// `quick`, `standard`, `full` or `custom`.
    pub check_set: String,
    /// The run this one retries or resumes.
    pub previous_run_id: Option<String>,
    /// The chat protocol to force.
    pub protocol: Option<crate::llm::wire::Protocol>,
}

impl Planned {
    /// The provider of the run; `None` when it covers several.
    pub(super) fn provider_id(&self) -> Option<&str> {
        let first = self.items.first()?.provider_id.as_str();
        self.items
            .iter()
            .all(|item| item.provider_id == first)
            .then_some(first)
    }

    /// The distinct provider ids of the rows, in row order.
    pub(super) fn provider_ids(&self) -> Vec<&str> {
        let mut seen = HashSet::new();
        self.items
            .iter()
            .map(|item| item.provider_id.as_str())
            .filter(|id| seen.insert(*id))
            .collect()
    }

    pub(super) fn spec(&self) -> RunSpec {
        RunSpec::new(&self.items, &self.plan, self.protocol)
    }

    /// The distinct checks that may send a request, in column order.
    pub(super) fn checks(&self) -> Vec<ProbeTest> {
        let keyed = self.plan.keys.iter().flat_map(|key| key.checks.iter());
        let rows = self
            .plan
            .rows
            .iter()
            .flat_map(|row| row.cells.iter().filter(|cell| cell.skip.is_none()))
            .map(|cell| &cell.test);
        let wanted: HashSet<ProbeTest> = keyed.chain(rows).copied().collect();
        ProbeTest::ALL
            .into_iter()
            .filter(|test| wanted.contains(test))
            .collect()
    }
}

fn set_name(selection: &CheckSelection) -> &'static str {
    match selection {
        CheckSelection::Set {
            set: CheckSet::Quick,
        } => "quick",
        CheckSelection::Set {
            set: CheckSet::Standard,
        } => "standard",
        CheckSelection::Set {
            set: CheckSet::Full,
        } => "full",
        CheckSelection::Custom { .. } => "custom",
    }
}

/// Kind and declared context of each `(provider, model)`, from the stored models and the
/// catalogue.
pub(super) async fn model_facts(
    state: &AppState,
    snapshot: &RoutingSnapshot,
    pairs: &[(String, String)],
) -> Result<Vec<(ModelKind, Option<u64>)>, AppError> {
    let stored: HashMap<(&str, &str), Option<u64>> = snapshot
        .models
        .iter()
        .map(|model| {
            (
                (model.provider_id.as_str(), model.model_id.as_str()),
                model.context,
            )
        })
        .collect();
    let stored_context: Vec<Option<u64>> = pairs
        .iter()
        .map(|(provider, model)| {
            stored
                .get(&(provider.as_str(), model.as_str()))
                .copied()
                .flatten()
        })
        .collect();
    let models: Vec<String> = pairs.iter().map(|(_, model)| model.clone()).collect();
    let _ = with_db_blocking(state, ensure_catalog).await;
    let entries = with_db_blocking(state, move |connection| {
        // One load for the whole plan: a per-model `catalog::lookup` is up to six table scans.
        let catalog = CatalogIndex::load(connection)?;
        let mut by_model: BTreeMap<String, (Option<String>, Option<u64>)> = BTreeMap::new();
        for model in models {
            if !by_model.contains_key(&model) {
                let entry = catalog.lookup(&model);
                by_model.insert(
                    model.clone(),
                    entry.map_or((None, None), |entry| {
                        (Some(entry.mode), entry.max_input_tokens)
                    }),
                );
            }
        }
        Ok(by_model)
    })
    .await?;
    Ok(pairs
        .iter()
        .zip(stored_context)
        .map(|((_, model), context)| {
            let (mode, catalog_context) = entries.get(model).cloned().unwrap_or_default();
            (
                model_kind(model, mode.as_deref(), None),
                context.or(catalog_context),
            )
        })
        .collect())
}

async fn planned_items(
    state: &AppState,
    snapshot: &RoutingSnapshot,
    rows: Vec<(String, String, String)>,
) -> Result<(Vec<PlanItem>, Vec<Option<u64>>), AppError> {
    let pairs: Vec<(String, String)> = rows
        .iter()
        .map(|(provider, _, model)| (provider.clone(), model.clone()))
        .collect();
    let facts = model_facts(state, snapshot, &pairs).await?;
    let (items, contexts) = rows
        .into_iter()
        .zip(facts)
        .map(|((provider_id, key_id, model_id), (kind, context))| {
            (
                PlanItem {
                    protocols: protocols_of(snapshot, &provider_id),
                    provider_id,
                    key_id,
                    model_id,
                    kind,
                },
                context,
            )
        })
        .unzip();
    Ok((items, contexts))
}

fn empty_plan(plan: &ProbePlan, what: &str) -> Result<(), AppError> {
    if plan.total_cells() == 0 {
        Err(AppError::validation(what))
    } else {
        Ok(())
    }
}

/// Plans the run `input` asks for.
///
/// # Errors
/// [`AppError::Validation`] for a bad item list, mixed modes or an empty plan,
/// [`AppError::NotFound`] for a key or run that does not exist; [`AppError::Validation`] for
/// an alias test (retired).
pub(super) async fn plan_input(
    state: &AppState,
    snapshot: &RoutingSnapshot,
    input: &ProbeRunInput,
) -> Result<Planned, AppError> {
    if input.alias.is_some() {
        return Err(AppError::validation(
            "aliases were removed: test the models of the chain instead",
        ));
    }
    if input.retry_of.is_some() && input.remaining_of.is_some() {
        return Err(AppError::validation("choose one of retry or run remaining"));
    }
    if let Some(run_id) = &input.retry_of {
        return plan_resume(state, snapshot, run_id, true, input).await;
    }
    if let Some(run_id) = &input.remaining_of {
        return plan_resume(state, snapshot, run_id, false, input).await;
    }
    plan_new(state, snapshot, input).await
}

/// The provider of an enabled key.
///
/// # Errors
/// [`AppError::NotFound`] when the key is missing or disabled.
pub(super) fn provider_of_key(
    snapshot: &RoutingSnapshot,
    key_id: &str,
) -> Result<String, AppError> {
    key_providers(snapshot)
        .get(key_id)
        .map(|provider_id| (*provider_id).to_owned())
        .ok_or_else(|| AppError::not_found(format!("key {key_id} is missing or disabled")))
}

fn key_providers(snapshot: &RoutingSnapshot) -> HashMap<&str, &str> {
    snapshot
        .keys
        .iter()
        .flat_map(|(provider_id, keys)| {
            keys.iter()
                .map(move |key| (key.key.id.as_str(), provider_id.as_str()))
        })
        .collect()
}

async fn plan_new(
    state: &AppState,
    snapshot: &RoutingSnapshot,
    input: &ProbeRunInput,
) -> Result<Planned, AppError> {
    if input.items.is_empty() || input.items.len() > MAX_PROBE_ITEMS {
        return Err(AppError::validation(format!(
            "select between 1 and {MAX_PROBE_ITEMS} models to test"
        )));
    }
    let providers = key_providers(snapshot);
    let mut seen: HashSet<&ProbeItem> = HashSet::new();
    let mut rows = Vec::new();
    for item in input.items.iter().filter(|item| seen.insert(item)) {
        let provider_id = providers.get(item.key_id.as_str()).ok_or_else(|| {
            AppError::not_found(format!("key {} is missing or disabled", item.key_id))
        })?;
        rows.push((
            (*provider_id).to_owned(),
            item.key_id.clone(),
            item.model_id.clone(),
        ));
    }
    let (items, contexts) = planned_items(state, snapshot, rows).await?;
    let plan = plan_run(&items, &input.checks);
    empty_plan(&plan, "select at least one check to run")?;
    Ok(Planned {
        items,
        contexts,
        plan,
        check_set: set_name(&input.checks).to_owned(),
        previous_run_id: None,
        protocol: input.protocol,
    })
}

/// Plans the checks of `checks` over models of a scan that is not saved yet, for the estimate
/// only: there is no provider or key, so every row carries the report id as both. Nothing can
/// be run from it (a run takes its keys from the snapshot and refuses the unknown one).
///
/// # Errors
/// [`AppError::Validation`] for no model, more than 500, an id the scan did not list or no check.
pub(super) fn plan_sniff(
    report: &SniffReport,
    model_ids: &[String],
    checks: &CheckSelection,
    protocol: Option<Protocol>,
) -> Result<Planned, AppError> {
    if model_ids.is_empty() || model_ids.len() > MAX_PROBE_ITEMS {
        return Err(AppError::validation(format!(
            "select between 1 and {MAX_PROBE_ITEMS} models to test"
        )));
    }
    let protocols: Vec<Protocol> = report
        .endpoints
        .iter()
        .filter(|endpoint| endpoint.supported)
        .map(|endpoint| endpoint.protocol)
        .collect();
    let mut seen = HashSet::new();
    let mut items = Vec::new();
    for id in model_ids.iter().filter(|id| seen.insert(id.as_str())) {
        let model = report
            .models
            .iter()
            .find(|model| &model.id == id)
            .ok_or_else(|| AppError::validation(format!("the scan did not list the model {id}")))?;
        items.push(PlanItem {
            provider_id: report.report_id.clone(),
            key_id: report.report_id.clone(),
            model_id: model.id.clone(),
            kind: model.kind.into(),
            protocols: protocols.clone(),
        });
    }
    let plan = plan_run(&items, checks);
    empty_plan(&plan, "select at least one check to run")?;
    Ok(Planned {
        contexts: vec![None; items.len()],
        items,
        plan,
        check_set: set_name(checks).to_owned(),
        previous_run_id: None,
        protocol,
    })
}

async fn plan_resume(
    state: &AppState,
    snapshot: &RoutingSnapshot,
    run_id: &str,
    retry: bool,
    input: &ProbeRunInput,
) -> Result<Planned, AppError> {
    let id = run_id.to_owned();
    let (row, results) = with_db_blocking(state, move |connection| {
        let row = store::get_probe_run(connection, &id)?
            .ok_or_else(|| AppError::not_found(format!("probe run {id} not found")))?;
        let results = store::probe_run_results(connection, &id)?;
        Ok((row, results))
    })
    .await?;
    let spec = RunDoc::of(&row).spec;
    if spec.is_empty() {
        return Err(AppError::validation(
            "this run has no stored plan and cannot be resumed",
        ));
    }
    let items = spec.plan_items(snapshot);
    let previous = spec.previous_cells(&results);
    let resumed = if retry {
        plan_retry(&items, &spec.plan(), &previous)
    } else {
        plan_remaining(&items, &spec.plan(), &previous)
    };
    empty_plan(
        &resumed.plan,
        if retry {
            "Nothing to retry: no cell of this run failed"
        } else {
            "Nothing remaining: every cell of this run has run"
        },
    )?;
    let rows = resumed
        .items
        .iter()
        .map(|item| {
            (
                item.provider_id.clone(),
                item.key_id.clone(),
                item.model_id.clone(),
            )
        })
        .collect();
    let (_, contexts) = planned_items(state, snapshot, rows).await?;
    Ok(Planned {
        items: resumed.items,
        contexts,
        plan: resumed.plan,
        check_set: row.check_set,
        previous_run_id: Some(row.run_id),
        protocol: input.protocol.or(spec.protocol),
    })
}

/// The targets to call and the options of each row. The snapshot resolves the provider's extra
/// headers from the secret store; the key and the resolved headers live in the returned
/// [`RunItem`]s only.
///
/// # Errors
/// [`AppError::NotFound`] for a provider that is disabled or gone, [`AppError::Validation`] for
/// a key that is gone or a provider header that names a missing secret.
pub(super) fn run_items(
    state: &AppState,
    snapshot: &RoutingSnapshot,
    planned: &Planned,
) -> Result<Vec<RunItem>, AppError> {
    let prompt = state.settings().ai.providers.default_test_prompt.clone();
    planned
        .items
        .iter()
        .zip(&planned.contexts)
        .map(|(item, context)| {
            let target =
                snapshot.target_for(&item.provider_id, Some(&item.key_id), &item.model_id, None)?;
            let models_endpoint = snapshot
                .providers
                .get(&item.provider_id)
                .and_then(|provider| provider.models_endpoint.clone());
            let reasoning_model = snapshot.models.iter().any(|model| {
                model.provider_id == item.provider_id
                    && model.model_id == item.model_id
                    && model.capabilities.known(Capability::Reasoning) == Some(true)
            });
            Ok(RunItem {
                target,
                options: ProbeOptions {
                    prompt: prompt.clone(),
                    protocol: planned.protocol,
                    allow_context_probe: false,
                    models_endpoint,
                    declared_context: *context,
                    reasoning_model,
                },
            })
        })
        .collect()
}
