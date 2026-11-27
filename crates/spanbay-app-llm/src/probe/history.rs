//! Stored bench results: the run list, one run's cells, a pair's history and recovery of runs
//! the app quit under.
//!
//! Single responsibility: read `llm_probe_*` through the store and shape the "Last run 36/38 ok"
//! line, plus the launch-time repair that marks an unfinished run Stopped with the note
//! "Interrupted by quit" and keeps it listable and resumable.

use std::sync::PoisonError;

use super::spec::RunDoc;
use super::types::{ProbeRunSummaryDto, ProbeSummary, RecoveredRun};
use crate::app::{AppState, current_time_ms};
use crate::error::AppError;
use crate::llm::probe::run::mark_interrupted;
use crate::llm::store::{self, ProbeResult};
use crate::with_connection;

/// Runs a list returns when no limit is given, and at most.
const DEFAULT_RUNS: u32 = 25;
const MAX_RUNS: u32 = 200;
/// Rows of a pair's history returned when no limit is given, and at most.
const DEFAULT_HISTORY: u32 = 50;
const MAX_HISTORY: u32 = 500;

fn clamp(limit: Option<u32>, default: u32, max: u32) -> usize {
    usize::try_from(limit.unwrap_or(default).clamp(1, max)).unwrap_or(usize::MAX)
}

fn summary_of(row: store::ProbeRunRow) -> ProbeRunSummaryDto {
    let doc = RunDoc::of(&row);
    ProbeRunSummaryDto::new(row, doc.totals.as_ref())
}

/// The newest runs, newest first, optionally of one provider (`limit` defaults to 25, at most
/// 200).
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_probe_runs_list_impl(
    state: &AppState,
    provider_id: Option<&str>,
    limit: Option<u32>,
) -> Result<Vec<ProbeRunSummaryDto>, AppError> {
    let limit = clamp(limit, DEFAULT_RUNS, MAX_RUNS);
    with_connection(state, |connection| {
        Ok(store::list_probe_runs(connection, provider_id, limit)?
            .into_iter()
            .map(summary_of)
            .collect())
    })
}

/// The newest run of a provider (any provider when `None`) for the "Last run 36/38 ok" line.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_probe_last_run_impl(
    state: &AppState,
    provider_id: Option<&str>,
) -> Result<Option<ProbeRunSummaryDto>, AppError> {
    with_connection(state, |connection| {
        let row = match provider_id {
            Some(_) => store::list_probe_runs(connection, provider_id, 1)?.pop(),
            None => store::latest_probe_run(connection)?,
        };
        Ok(row.map(summary_of))
    })
}

/// Every stored cell of one run, in the order the cells finished.
///
/// # Errors
/// [`AppError::NotFound`] for a run that does not exist.
pub fn llm_probe_run_get_impl(
    state: &AppState,
    run_id: &str,
) -> Result<Vec<ProbeResult>, AppError> {
    with_connection(state, |connection| {
        store::get_probe_run(connection, run_id)?
            .ok_or_else(|| AppError::not_found(format!("probe run {run_id} not found")))?;
        store::probe_run_results(connection, run_id)
    })
}

/// The history of one key and model pair, newest first (`limit` rows, default 50, at most 500).
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_probe_history_list_impl(
    state: &AppState,
    key_id: &str,
    model_id: &str,
    limit: Option<u32>,
) -> Result<Vec<ProbeResult>, AppError> {
    let limit = clamp(limit, DEFAULT_HISTORY, MAX_HISTORY);
    with_connection(state, |connection| {
        store::list_probe_history(connection, key_id, model_id, limit)
    })
}

/// The latest stored result of every cell of `key_id` (all keys when `None`), newest first; each
/// row carries its status word and, for a skipped cell, the reason.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_probe_results_impl(
    state: &AppState,
    key_id: Option<&str>,
) -> Result<Vec<ProbeResult>, AppError> {
    with_connection(state, |connection| {
        store::list_probe_results(connection, key_id)
    })
}

/// Marks every run that never ended (the app quit under it) Stopped with the note "Interrupted by
/// quit", counting the cells that never ran as Not tested, and returns them so the shell can
/// offer "Run remaining N". The runs stay listed and resumable. The active run, if any, is left
/// alone.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_probe_recover_impl(state: &AppState) -> Result<Vec<RecoveredRun>, AppError> {
    let active = state
        .llm
        .services
        .active_probe
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .map(|run| run.run_id.clone());
    with_connection(state, |connection| {
        let mut recovered = Vec::new();
        for row in store::unfinished_probe_runs(connection)? {
            if active.as_deref() == Some(row.run_id.as_str()) {
                continue;
            }
            let spec = RunDoc::of(&row).spec;
            let results = store::probe_run_results(connection, &row.run_id)?;
            let summary = mark_interrupted(&spec.plan(), &spec.recorded_cells(&results));
            let totals = ProbeSummary::new(&row.run_id, &summary);
            store::finish_probe_run(
                connection,
                &row.run_id,
                totals.passed,
                totals.total,
                &RunDoc {
                    spec,
                    totals: Some(totals),
                }
                .to_value(),
                current_time_ms(),
            )?;
            recovered.push(RecoveredRun {
                run_id: row.run_id,
                provider_id: row.provider_id,
                not_tested: summary.not_tested,
            });
        }
        Ok(recovered)
    })
}
