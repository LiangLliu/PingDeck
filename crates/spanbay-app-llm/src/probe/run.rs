//! Running a bench plan: one run at a time (later ones wait their turn), stored cell by cell,
//! announced as it goes.
//!
//! Single responsibility: wire the engine's orchestrator (`llm::probe::run`) to the app: the
//! single-active-run slot, the cost refusal, the run row and its stored plan, the store-backed
//! [`ProbePersist`], the mapping of engine events to [`ProbeEvent`], the end-of-run bookkeeping
//! and the `LlmEvent`s that tell the UI the models changed. A probe never touches the circuit
//! breaker or the health book.

use std::sync::PoisonError;
use std::sync::atomic::{AtomicBool, Ordering};

use futures_util::future::BoxFuture;

use super::estimate::estimate_input;
use super::plan::run_items;
use super::spec::RunDoc;
use super::types::{ProbeCellDto, ProbeEvent, ProbeRunInput, ProbeSummary};
use crate::app::run_activity::RunKind;
use crate::app::{AppState, current_time_ms};
use crate::cancel::{CancelToken, until_cancelled};
use crate::error::AppError;
use crate::events::EventSink;
use crate::llm::events::LlmEvent;
use crate::llm::probe::run::{
    CONCURRENCY_RANGE, CellRecord, ProbePersist, ProbeRun, ProbeRunEvent, RequestRecord,
    RunSettings, RunSummary, run_plan,
};
use crate::llm::services::ActiveProbeRun;
use crate::llm::store;
use crate::with_db_blocking;
use crate::workspace::variables::uuid_v4;

/// The claim on the one active-run slot; dropping it frees the slot, then the queue.
pub(super) struct ActiveRun<'a> {
    state: &'a AppState,
    run_id: String,
    _turn: tokio::sync::MutexGuard<'a, ()>,
}

impl<'a> ActiveRun<'a> {
    /// Waits for the runs started earlier to end (in arrival order: a second "Probe" is
    /// queued, never dropped), then takes the slot. Cancelling `cancel` while waiting gives up
    /// the place in the queue.
    pub(super) async fn reserve(
        state: &'a AppState,
        run_id: &str,
        cancel: &CancelToken,
    ) -> Result<Self, AppError> {
        let turn = until_cancelled(Some(cancel), state.llm.services.probe_queue.lock())
            .await
            .ok_or_else(AppError::cancelled)?;
        *state
            .llm
            .services
            .active_probe
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(ActiveProbeRun {
            run_id: run_id.to_owned(),
            cancel: cancel.clone(),
        });
        Ok(Self {
            state,
            run_id: run_id.to_owned(),
            _turn: turn,
        })
    }
}

impl Drop for ActiveRun<'_> {
    fn drop(&mut self) {
        let mut slot = self
            .state
            .llm
            .services
            .active_probe
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if slot.as_ref().is_some_and(|run| run.run_id == self.run_id) {
            *slot = None;
        }
    }
}

/// Stores a run's cells and request rows through short blocking tasks (the store lock is never
/// held across an `.await`).
struct StorePersist<'a> {
    state: &'a AppState,
}

impl ProbePersist for StorePersist<'_> {
    fn save_cell<'a>(
        &'a self,
        run_id: &'a str,
        cell: &'a CellRecord,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        let (run_id, cell) = (run_id.to_owned(), cell.clone());
        Box::pin(with_db_blocking(self.state, move |connection| {
            store::persist_cell(connection, &run_id, &cell, current_time_ms())
        }))
    }

    fn record_request<'a>(
        &'a self,
        run_id: &'a str,
        entry: &'a RequestRecord,
    ) -> BoxFuture<'a, Result<(), AppError>> {
        let (run_id, entry) = (run_id.to_owned(), entry.clone());
        let tz = self.state.llm.tz_offset_minutes();
        Box::pin(with_db_blocking(self.state, move |connection| {
            store::record_probe_request(connection, &run_id, &entry, tz).map(|_| ())
        }))
    }
}

/// Turns engine events into [`ProbeEvent`]s. A sink that reports it is gone is not called again;
/// the run itself goes on.
struct Forward<'a> {
    run_id: &'a str,
    events: &'a dyn EventSink<ProbeEvent>,
    listening: AtomicBool,
}

impl EventSink<ProbeRunEvent> for Forward<'_> {
    fn emit(&self, event: ProbeRunEvent) -> bool {
        let ProbeRunEvent::Cell { cell, done, total } = event;
        if self.listening.load(Ordering::Relaxed) {
            let delivered = self.events.emit(ProbeEvent::Cell {
                run_id: self.run_id.to_owned(),
                cell: ProbeCellDto::new(self.run_id, cell),
            }) && self.events.emit(ProbeEvent::Progress { done, total });
            if !delivered {
                self.listening.store(false, Ordering::Relaxed);
            }
        }
        true
    }
}

/// Runs a bench run: a new run over (key, model) pairs, "Retry failed" or "Run remaining N"
/// (see [`ProbeRunInput`]; an alias test is refused). Every finished cell is stored, announced on
/// `events` and counted; Stop (the cancel token) ends the run with `cancelled` set and what was
/// received kept. A run that needs the cost dialog and was not confirmed is refused
/// before anything is stored or sent. Probes never change the circuit or health state; after
/// the run the UI is told the provider status and the models (their measured capabilities)
/// changed.
///
/// # Errors
/// [`AppError::Cancelled`] when stopped while waiting for an earlier run,
/// [`AppError::ConfirmationRequired`] carrying the estimate when the cost dialog is due,
/// [`AppError::Validation`] for a bad input (item count, concurrency, mixed modes, nothing to
/// run), [`AppError::NotFound`] for a key, provider or run that cannot be used.
pub async fn llm_probe_run_impl(
    state: &AppState,
    input: ProbeRunInput,
    cancel_token: Option<String>,
    events: &dyn EventSink<ProbeEvent>,
) -> Result<ProbeSummary, AppError> {
    if let Some(concurrency) = input.concurrency
        && !(CONCURRENCY_RANGE.0..=CONCURRENCY_RANGE.1).contains(&concurrency)
    {
        return Err(AppError::validation(format!(
            "parallel checks must be between {} and {}",
            CONCURRENCY_RANGE.0, CONCURRENCY_RANGE.1
        )));
    }
    let guard = state.cancellations.register(cancel_token.as_deref())?;
    let token = guard.token().clone();
    let run_id = format!("run_{}", uuid_v4());
    let _active = ActiveRun::reserve(state, &run_id, &token).await?;
    // The quit confirm counts this run until the function returns, however it ends.
    let _running = state.track_run(RunKind::Bench);

    let snapshot = state.llm.snapshot().await?;
    let (planned, estimate) = estimate_input(state, &snapshot, &input).await?;
    if estimate.needs_confirmation && !input.confirmed_cost {
        return Err(AppError::confirmation_required(format!(
            "Confirm the cost before this run: {}",
            estimate.summary
        )));
    }
    let items = run_items(state, &snapshot, &planned)?;

    let doc = RunDoc {
        spec: planned.spec(),
        totals: None,
    };
    let started = (
        run_id.clone(),
        planned.provider_id().map(str::to_owned),
        planned.check_set.clone(),
        planned.previous_run_id.clone(),
        doc.to_value(),
    );
    with_db_blocking(state, move |connection| {
        let (run_id, provider_id, check_set, previous, doc) = started;
        store::start_probe_run(
            connection,
            &run_id,
            provider_id.as_deref(),
            &check_set,
            previous.as_deref(),
            current_time_ms(),
        )?;
        store::set_probe_run_summary(connection, &run_id, &doc)
    })
    .await?;

    let total = u32::try_from(planned.plan.total_cells()).unwrap_or(u32::MAX);
    let _ = events.emit(ProbeEvent::RunStarted {
        run_id: run_id.clone(),
        total,
        previous_run_id: planned.previous_run_id.clone(),
    });

    let settings = state.settings();
    let providers = &settings.ai.providers;
    let forward = Forward {
        run_id: &run_id,
        events,
        listening: AtomicBool::new(true),
    };
    let persist = StorePersist { state };
    let outcome = run_plan(&ProbeRun {
        run_id: &run_id,
        runtime: &state.llm,
        items: &items,
        plan: &planned.plan,
        settings: RunSettings::new(
            input.concurrency.unwrap_or(providers.probe_concurrency),
            providers.probe_timeout_s,
        ),
        persist: &persist,
        sink: &forward,
        cancel: &token,
    })
    .await;
    let (run_summary, failure) = match outcome {
        Ok(summary) => (summary, None),
        Err(error) => (
            RunSummary {
                total,
                not_tested: total,
                cancelled: true,
                ..RunSummary::default()
            },
            Some(error),
        ),
    };
    let summary = ProbeSummary::new(&run_id, &run_summary);

    let finished = RunDoc {
        spec: doc.spec,
        totals: Some(summary.clone()),
    }
    .to_value();
    let (id, passed, planned_cells) = (run_id.clone(), summary.passed, summary.total);
    let stored = with_db_blocking(state, move |connection| {
        store::finish_probe_run(
            connection,
            &id,
            passed,
            planned_cells,
            &finished,
            current_time_ms(),
        )
    })
    .await;

    // The capability matrix changed and the provider's latest test did.
    state.llm.touch();
    for provider_id in planned.provider_ids() {
        state.llm.emit(LlmEvent::ModelsChanged {
            provider_id: provider_id.to_owned(),
        });
        state.llm.emit(LlmEvent::ProviderStatusChanged {
            provider_id: provider_id.to_owned(),
        });
    }
    let _ = events.emit(ProbeEvent::RunFinished {
        summary: summary.clone(),
    });
    if let Some(error) = failure {
        return Err(error);
    }
    stored?;
    Ok(summary)
}
