//! "Apply results" of the bench, its preview and its undo.
//!
//! Single responsibility: read the unapplied evidence of the provider (the newest stored result
//! of every cell the bench shows, stored after the provider's last logged apply, from whichever
//! run produced it: the bench grid and Apply read the same cells), take the decisive check of
//! every tested model, plan with `llm::probe::apply`, and write the `enabled` flags, the measured capability facts
//! the user kept checked (the only path from a probe result to the routing matrix) and the
//! rows of passing "From provider" ghost models in one transaction, logging what an undo
//! restores. The undo is the store's one conditional restore, shared with the bulk switch. A run
//! older than 7 days is not applied.

use std::collections::{BTreeMap, HashMap};

use spanbay_store::DbConnection as Connection;

use super::spec::RunDoc;
use super::types::{ApplyReportDto, ProbeApplyPreviewDto};
use crate::app::{AppState, current_time_ms};
use crate::error::AppError;
use crate::llm::events::LlmEvent;
use crate::llm::probe::apply::{
    ApplyPlan, ApplyRow, ApplySelection, CurrentModel, FallbackUse, Measured, merge_measurements,
    plan_apply, plan_capabilities, run_applicable,
};
use crate::llm::probe::capability::derive_capability;
use crate::llm::probe::{ProbeStatus, ProbeTest};
use crate::llm::store::{
    self, EnabledChange, ListedModel, ProbeApplyRecord, ProbeResult, Provider, UndoReport,
};
use crate::with_connection;
use crate::workspace::variables::uuid_v4;

/// What an apply or its preview reads: the run, its provider and the plan.
struct Loaded {
    run_id: String,
    started_at_ms: i64,
    provider: Provider,
    plan: ApplyPlan,
    results: Vec<ProbeResult>,
}

/// Whether a key's own checks failed: Connectivity Unreachable or Timed out, Auth Rejected.
fn key_failed(results: &[ProbeResult], key_id: &str) -> bool {
    results
        .iter()
        .filter(|result| result.key_id == key_id)
        .any(|result| {
            let status = ProbeStatus::parse(&result.status_word);
            match ProbeTest::parse(&result.test) {
                Some(ProbeTest::Connectivity) => {
                    matches!(
                        status,
                        Some(ProbeStatus::Unreachable | ProbeStatus::TimedOut)
                    )
                }
                Some(ProbeTest::Auth) => status == Some(ProbeStatus::Rejected),
                _ => false,
            }
        })
}

/// The decisive row of every tested model: Chat, Embeddings for an embedding model, Decision for
/// a decision model, as the run that produced the result planned it. A model tested with several
/// keys counts as passing when any key passed.
fn apply_rows(docs: &HashMap<String, RunDoc>, results: &[ProbeResult]) -> Vec<ApplyRow> {
    let mut rows: Vec<ApplyRow> = Vec::new();
    for result in results {
        let doc = result.run_id.as_ref().and_then(|id| docs.get(id));
        let decisive = match doc {
            Some(doc) if doc.spec.is_embedding(&result.key_id, &result.model_id) => {
                ProbeTest::Embeddings
            }
            Some(doc) if doc.spec.is_decision(&result.key_id, &result.model_id) => {
                ProbeTest::Decision
            }
            _ => ProbeTest::Chat,
        };
        if ProbeTest::parse(&result.test) != Some(decisive) {
            continue;
        }
        let row = ApplyRow {
            model_id: result.model_id.clone(),
            status: ProbeStatus::parse(&result.status_word),
            http_status: result.status,
        };
        match rows.iter_mut().find(|known| known.model_id == row.model_id) {
            Some(known) if known.status != Some(ProbeStatus::Ready) => *known = row,
            Some(_) => {}
            None => rows.push(row),
        }
    }
    rows
}

/// The evidence an apply plans from: per key of the provider, the newest stored result of every
/// (model, check) the bench ran, stored after `since` (the provider's last logged apply) and
/// young enough to apply. The runs behind them are read for their decisive checks.
fn unapplied_evidence(
    connection: &Connection,
    provider_id: &str,
    since: Option<i64>,
    now_ms: i64,
) -> Result<(Vec<ProbeResult>, HashMap<String, RunDoc>), AppError> {
    let mut results = Vec::new();
    for key in store::list_keys(connection, Some(provider_id))? {
        results.extend(
            store::list_probe_results(connection, Some(&key.id))?
                .into_iter()
                .filter(|result| {
                    // Bench runs only: the key checks a scan stored are no bench evidence.
                    result
                        .run_id
                        .as_deref()
                        .is_some_and(|run| !run.starts_with("sniff:"))
                        && since.is_none_or(|since| result.checked_at_ms > since)
                        && run_applicable(result.checked_at_ms, now_ms)
                }),
        );
    }
    let mut docs: HashMap<String, RunDoc> = HashMap::new();
    for run_id in results.iter().filter_map(|result| result.run_id.as_ref()) {
        if docs.contains_key(run_id) {
            continue;
        }
        if let Some(run) = store::get_probe_run(connection, run_id)? {
            docs.insert(run_id.clone(), RunDoc::of(&run));
        }
    }
    Ok((results, docs))
}

/// What each result measured, as the apply plans it.
fn measurements(results: &[ProbeResult]) -> Vec<Measured<'_>> {
    results
        .iter()
        .filter_map(|result| {
            derive_capability(result).map(|update| Measured {
                model_id: &result.model_id,
                key_id: &result.key_id,
                update,
                at_ms: result.checked_at_ms,
                http_status: result.status,
            })
        })
        .collect()
}

fn load(connection: &Connection, run_id: &str) -> Result<Loaded, AppError> {
    let row = store::get_probe_run(connection, run_id)?
        .ok_or_else(|| AppError::not_found(format!("probe run {run_id} not found")))?;
    if !run_applicable(row.started_at_ms, current_time_ms()) {
        return Err(AppError::validation(
            "This run is older than 7 days; run the bench again before applying",
        ));
    }
    let provider_id = row
        .provider_id
        .clone()
        .ok_or_else(|| AppError::validation("Apply results works on a run over one provider"))?;
    let provider = store::get_provider(connection, &provider_id)?
        .ok_or_else(|| AppError::not_found(format!("provider {provider_id} not found")))?;
    let now_ms = current_time_ms();
    let since = store::latest_probe_apply_at(connection, &provider_id)?;
    let (results, docs) = unapplied_evidence(connection, &provider_id, since, now_ms)?;
    let stored = store::list_models(connection, Some(&provider_id))?;
    let matrices: Vec<(String, _)> = stored
        .iter()
        .map(|model| (model.model_id.clone(), model.capabilities))
        .collect();
    let current: Vec<CurrentModel> = stored
        .into_iter()
        .map(|model| CurrentModel {
            model_id: model.model_id,
            enabled: model.enabled,
        })
        .collect();
    // Which of this provider's models stand in for which primaries (the "Fallback of" note).
    let mut by_model: BTreeMap<String, Vec<String>> = BTreeMap::new();
    let mut chains: Vec<_> = store::list_fallbacks(connection)?.into_iter().collect();
    chains.sort();
    for (primary, chain) in chains {
        for fallback in chain.iter().filter(|f| f.provider_id == provider_id) {
            let name = match store::get_provider(connection, &primary.provider_id)? {
                Some(owner) => format!("{}/{}", owner.name, primary.model),
                None => primary.id_form(),
            };
            let names = by_model.entry(fallback.model.clone()).or_default();
            if !names.contains(&name) {
                names.push(name);
            }
        }
    }
    let aliases: Vec<FallbackUse> = by_model
        .into_iter()
        .map(|(model_id, primaries)| FallbackUse {
            model_id,
            primaries,
        })
        .collect();
    let mut keys: Vec<&str> = results
        .iter()
        .map(|result| result.key_id.as_str())
        .collect();
    keys.sort_unstable();
    keys.dedup();
    let host_failed = !keys.is_empty() && keys.iter().all(|key| key_failed(&results, key));
    let mut plan = plan_apply(
        &apply_rows(&docs, &results),
        &current,
        &aliases,
        host_failed,
    );
    if !host_failed {
        plan.capabilities = plan_capabilities(&measurements(&results), &matrices, &aliases);
    }
    Ok(Loaded {
        run_id: row.run_id,
        started_at_ms: row.started_at_ms,
        provider,
        plan,
        results,
    })
}

/// The preview of "Apply results" for the provider of `run_id`: models to enable, disable and
/// create, each with its previous state and the reason, and the capability facts with their
/// evidence; rows checked except a disable and a change to "not supported", and the reason the
/// button reads "Nothing to apply" when there is nothing. The plan covers every cell the bench
/// stored since the provider's last apply, whichever run (a full bench run or a one-model test)
/// produced it.
///
/// # Errors
/// [`AppError::NotFound`] for a run or provider that does not exist, [`AppError::Validation`]
/// for a run over several providers.
pub fn llm_probe_apply_preview_impl(
    state: &AppState,
    run_id: &str,
) -> Result<ProbeApplyPreviewDto, AppError> {
    let loaded = with_connection(state, |connection| load(connection, run_id))?;
    let empty_reason = if loaded.plan.host_level_failure {
        Some("Nothing to apply, the key failed".to_owned())
    } else if loaded.plan.is_empty() {
        Some("Nothing to apply".to_owned())
    } else {
        None
    };
    Ok(ProbeApplyPreviewDto {
        run_id: loaded.run_id,
        default_selection: loaded.plan.default_selection(),
        plan: loaded.plan,
        provider_name: loaded.provider.name,
        provider_id: loaded.provider.id,
        started_at_ms: loaded.started_at_ms,
        empty_reason,
    })
}

/// Applies the rows the user left checked: enables and disables stored models, records the
/// checked capability facts, creates passing ghost models as `live` and enabled (with the
/// capabilities the run measured), and logs the previous flags and states for
/// [`llm_probe_apply_undo_impl`]. A key that failed at host level, or a selection that matches
/// nothing, changes nothing and logs nothing (`apply_id` is `None`).
///
/// # Errors
/// As [`llm_probe_apply_preview_impl`], [`AppError::Database`] on failure (nothing is written).
pub fn llm_probe_apply_impl(
    state: &AppState,
    run_id: &str,
    selection: &ApplySelection,
) -> Result<ApplyReportDto, AppError> {
    let report = with_connection(state, |connection| {
        store::in_write_transaction(connection, |transaction| {
            let loaded = load(transaction, run_id)?;
            let report = loaded.plan.report(selection);
            let provider_id = loaded.provider.id.as_str();
            let now = current_time_ms();
            let apply_id = (!report.previous.is_empty() || !report.capabilities.is_empty())
                .then(|| format!("apply_{}", uuid_v4()));
            if let Some(apply_id) = &apply_id {
                let writes: Vec<EnabledChange> = report
                    .enabled
                    .iter()
                    .map(|id| (id, true))
                    .chain(report.disabled.iter().map(|id| (id, false)))
                    .map(|(model_id, enabled)| EnabledChange {
                        model_id: model_id.clone(),
                        enabled,
                        only_if_current: None,
                    })
                    .collect();
                store::set_models_enabled(transaction, provider_id, &writes)?;
                store::swap_capability_states(
                    transaction,
                    provider_id,
                    &report.capabilities,
                    false,
                )?;
                let ghosts: Vec<(ListedModel, bool)> = report
                    .created
                    .iter()
                    .map(|id| {
                        (
                            ListedModel {
                                id: id.clone(),
                                ..ListedModel::default()
                            },
                            true,
                        )
                    })
                    .collect();
                store::insert_models_from_sniff(transaction, provider_id, &ghosts, now)?;
                // The run measured these models before they existed: record it now, merged over
                // the keys as for a stored model (any pass wins, D-276).
                let measured = measurements(&loaded.results);
                for merged in merge_measurements(&measured)
                    .into_iter()
                    .filter(|merged| report.created.iter().any(|id| id == merged.model_id))
                {
                    store::record_measured(
                        transaction,
                        provider_id,
                        merged.model_id,
                        &[merged.update],
                        merged.at_ms,
                    )?;
                }
                store::record_probe_apply(
                    transaction,
                    apply_id,
                    &loaded.run_id,
                    provider_id,
                    &report,
                    now,
                )?;
            }
            Ok((apply_id, loaded.run_id, loaded.provider.id, report, now))
        })
    })?;
    let (apply_id, run_id, provider_id, report, applied_at_ms) = report;
    if apply_id.is_some() {
        changed(state, &provider_id);
    }
    Ok(ApplyReportDto {
        apply_id,
        run_id,
        provider_id,
        enabled: report.enabled,
        disabled: report.disabled,
        created: report.created,
        applied_at_ms,
    })
}

/// Restores the previous `enabled` value of each model an apply (or a bulk switch) changed, only
/// where the current value still equals the applied one, so later manual edits survive; a model
/// the apply created is disabled, not deleted. The log entry is removed.
///
/// # Errors
/// [`AppError::NotFound`] when the operation is not in the log (already undone, or older than
/// the newest 5).
pub fn llm_probe_apply_undo_impl(state: &AppState, apply_id: &str) -> Result<UndoReport, AppError> {
    let (provider_id, report) = with_connection(state, |connection| {
        let record = store::get_probe_apply(connection, apply_id)?
            .ok_or_else(|| AppError::not_found(format!("operation {apply_id} not found")))?;
        store::undo_bulk(connection, apply_id).map(|report| (record.provider_id, report))
    })?;
    changed(state, &provider_id);
    Ok(report)
}

/// The apply "Undo last apply" offers: the newest apply of probe results of `provider_id` (any
/// provider when `None`; never a bulk switch) while it is younger than 24 hours.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_probe_apply_latest_impl(
    state: &AppState,
    provider_id: Option<&str>,
) -> Result<Option<ProbeApplyRecord>, AppError> {
    with_connection(state, |connection| {
        store::latest_undoable_apply(connection, current_time_ms(), provider_id)
    })
}

/// A model's enabled state changed: routing rebuilds and the Models tab reloads.
fn changed(state: &AppState, provider_id: &str) {
    state.llm.touch();
    state.llm.emit(LlmEvent::ModelsChanged {
        provider_id: provider_id.to_owned(),
    });
}
