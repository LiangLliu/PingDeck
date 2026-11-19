//! The one cost estimate of a bench run.
//!
//! Single responsibility: price the plan a [`ProbeRunInput`] would run. The bench header, the
//! Models selection bar, the cost dialog and the run's own confirmation rule all call this, so
//! they show the same string and the same decision.

use std::collections::{HashMap, HashSet};

use super::plan::{Planned, plan_input, plan_sniff};
use super::types::{ProbeEstimateDto, ProbeRunInput, ProbeSniffInput};
use crate::app::AppState;
use crate::error::AppError;
use crate::llm::probe::estimate::{ProbeEstimate, estimate_run};
use crate::llm::router::RoutingSnapshot;
use crate::llm::store;
use crate::llm::usage::Price;
use crate::sniff_create::cached_report;
use crate::with_db_blocking;

/// Estimates an already planned run; prices come from the overrides, then the catalogue.
pub(super) async fn estimate_planned(
    state: &AppState,
    planned: &Planned,
) -> Result<ProbeEstimate, AppError> {
    let mut seen = HashSet::new();
    let pairs: Vec<(String, String)> = planned
        .items
        .iter()
        .map(|item| (item.provider_id.clone(), item.model_id.clone()))
        .filter(|pair| seen.insert(pair.clone()))
        .collect();
    let prices: HashMap<(String, String), Price> = with_db_blocking(state, move |connection| {
        let mut prices = HashMap::new();
        for (provider, model) in pairs {
            if let Some(price) = store::effective_price(connection, &provider, &model)? {
                prices.insert((provider, model), price);
            }
        }
        Ok(prices)
    })
    .await?;
    Ok(estimate_run(
        &planned.plan,
        &planned.items,
        &|provider, model| {
            prices
                .get(&(provider.to_owned(), model.to_owned()))
                .cloned()
        },
    ))
}

/// [`estimate_planned`] with the numbers the dialog needs, for a snapshot already in hand.
pub(super) async fn estimate_input(
    state: &AppState,
    snapshot: &RoutingSnapshot,
    input: &ProbeRunInput,
) -> Result<(Planned, ProbeEstimateDto), AppError> {
    let planned = plan_input(state, snapshot, input).await?;
    let estimate = estimate_planned(state, &planned).await?;
    let dto = ProbeEstimateDto::new(&estimate, planned.checks(), planned.plan.runnable_cells());
    Ok((planned, dto))
}

/// The estimate of the run `input` would start: tokens, cost, whether the cost dialog is due
/// and the string every surface shows. Writes and sends nothing.
///
/// # Errors
/// As [`super::llm_probe_run_impl`] before it starts a run: [`AppError::Validation`] for a bad
/// input, [`AppError::NotFound`] for a key, run or alias that does not exist.
pub async fn llm_probe_estimate_impl(
    state: &AppState,
    input: ProbeRunInput,
) -> Result<ProbeEstimateDto, AppError> {
    let snapshot = state.llm.snapshot().await?;
    estimate_input(state, &snapshot, &input)
        .await
        .map(|(_, estimate)| estimate)
}

/// The estimate of testing `sniff.model_ids` of a scan that is not saved yet, with the checks
/// and protocol of `input` (its items and run modes are not read). The models come from the
/// cached scan report and are priced by the same estimator and the same catalogue as a saved
/// provider's; no provider is needed and nothing is sent.
///
/// # Errors
/// [`AppError::NotFound`] when the scan result expired or never existed,
/// [`AppError::Validation`] for no model, an id the scan did not list or no check.
pub async fn llm_probe_estimate_sniff_impl(
    state: &AppState,
    sniff: &ProbeSniffInput,
    input: &ProbeRunInput,
) -> Result<ProbeEstimateDto, AppError> {
    let report = cached_report(state, &sniff.report_id)?;
    let planned = plan_sniff(&report, &sniff.model_ids, &input.checks, input.protocol)?;
    let estimate = estimate_planned(state, &planned).await?;
    Ok(ProbeEstimateDto::new(
        &estimate,
        planned.checks(),
        planned.plan.runnable_cells(),
    ))
}
