//! The opt-in context-window test as a run of one cell.
//!
//! Single responsibility: refuse the expensive ladder until the user acknowledged the cost, then
//! run `ProbeTest::Context` for one key and model through the same storage as a bench run (run
//! row, latest and history result, `source = probe` log rows) and keep the window it measured on
//! the stored model. It takes the one active-run slot, so no other run overlaps it, and it never
//! touches the circuit or the health book.

use serde_json::Value;

use super::plan::{model_facts, provider_of_key};
use super::run::ActiveRun;
use super::spec::RunDoc;
use super::types::{ProbeCellDto, ProbeEvent, ProbeSummary};
use crate::app::run_activity::RunKind;
use crate::app::{AppState, current_time_ms};
use crate::error::AppError;
use crate::events::EventSink;
use crate::llm::events::LlmEvent;
use crate::llm::probe::run::{CellRecord, RequestRecord, RunSummary};
use crate::llm::probe::status::{ReasonContext, reason_line};
use crate::llm::probe::{
    CheckTimeouts, ContextProbeInfo, ProbeOptions, ProbeStatus, ProbeTest, run_timed,
};
use crate::llm::store::{self, ProbeResult};
use crate::with_db_blocking;
use crate::workspace::variables::uuid_v4;

/// The words the user types or ticks to unlock the action.
const ACKNOWLEDGMENT: &str = "I understand this can cost real money";

/// The refusal: the acknowledgment sentence and the figures of [`ContextProbeInfo`] (the UI reads
/// them structured from `llm_context_probe_info_impl`; the message repeats them in words).
fn refusal(info: &ContextProbeInfo) -> AppError {
    AppError::validation(format!(
        "The context test needs your acknowledgment (\"{ACKNOWLEDGMENT}\"): it sends up to {} \
         input tokens in steps from {}. {}",
        info.total_tokens, info.start_tokens, info.warning
    ))
}

/// The window the ladder confirmed: its top step, once every step passed (the top reaches the
/// declared window, or the undeclared cap). A ladder that failed on the way writes nothing.
fn measured_limit(result: &ProbeResult) -> Option<u64> {
    let failed_step = result.detail["tried"]
        .as_array()?
        .last()
        .is_some_and(|step| step["ok"] == Value::Bool(false));
    let window = result.detail["max_ok_tokens"].as_u64().filter(|w| *w > 0)?;
    // A failed step may be a transient error as much as the window: only a ladder that passed
    // every step is written back; a failure leaves the stored window as it is.
    (result.ok && !failed_step).then_some(window)
}

fn summary_of(run_id: &str, status: Option<ProbeStatus>) -> ProbeSummary {
    let mut totals = RunSummary {
        total: 1,
        ..RunSummary::default()
    };
    match status {
        None => {
            totals.cancelled = true;
            totals.not_tested = 1;
        }
        Some(status) => {
            totals.done = 1;
            match status {
                ProbeStatus::Ready => totals.ready = 1,
                ProbeStatus::RateLimited => totals.rate_limited = 1,
                ProbeStatus::Unsupported => totals.unsupported = 1,
                ProbeStatus::NotObserved => totals.not_observed = 1,
                ProbeStatus::Skipped => totals.skipped = 1,
                _ => totals.failed = 1,
            }
        }
    }
    ProbeSummary::new(run_id, &totals)
}

/// Runs the context-window test of `model_id` with `key_id` and stores the result like a bench
/// cell; a larger step that failed after a smaller one passed becomes the model's `context`
/// (an existing, enabled or not, model only). `acknowledged` is the typed acknowledgment: without
/// it nothing is stored or sent. Progress goes to `events` as a run of one cell. Stop (the cancel
/// token) ends it with [`AppError::Cancelled`], keeps nothing of the cell and marks the run
/// stopped.
///
/// # Errors
/// [`AppError::Validation`] when not acknowledged (the message holds the warning and the token
/// figures), [`AppError::Cancelled`] when stopped while waiting for an earlier run,
/// [`AppError::NotFound`] for a key, provider or model that cannot be used,
/// [`AppError::Cancelled`] when stopped.
pub async fn llm_context_probe_run_impl(
    state: &AppState,
    key_id: &str,
    model_id: &str,
    acknowledged: bool,
    cancel_token: Option<String>,
    events: &dyn EventSink<ProbeEvent>,
) -> Result<ProbeResult, AppError> {
    let snapshot = state.llm.snapshot().await?;
    let provider_id = provider_of_key(&snapshot, key_id)?;
    let declared = model_facts(
        state,
        &snapshot,
        &[(provider_id.clone(), model_id.to_owned())],
    )
    .await?
    .first()
    .and_then(|(_, context)| *context);
    if !acknowledged {
        return Err(refusal(&ContextProbeInfo::for_window(declared)));
    }
    let target = snapshot.target_for(&provider_id, Some(key_id), model_id, None)?;
    let models_endpoint = snapshot
        .providers
        .get(&provider_id)
        .and_then(|provider| provider.models_endpoint.clone());

    let guard = state.cancellations.register(cancel_token.as_deref())?;
    let token = guard.token().clone();
    let run_id = format!("run_{}", uuid_v4());
    let _active = ActiveRun::reserve(state, &run_id, &token).await?;
    // The quit confirm counts this run until the function returns, however it ends.
    let _running = state.track_run(RunKind::Bench);
    let started = (run_id.clone(), provider_id.clone());
    with_db_blocking(state, move |connection| {
        store::start_probe_run(
            connection,
            &started.0,
            Some(&started.1),
            "context",
            None,
            current_time_ms(),
        )
    })
    .await?;
    let _ = events.emit(ProbeEvent::RunStarted {
        run_id: run_id.clone(),
        total: 1,
        previous_run_id: None,
    });

    let settings = state.settings();
    let timeouts = CheckTimeouts::from_setting(settings.ai.providers.probe_timeout_s);
    let options = ProbeOptions {
        prompt: settings.ai.providers.default_test_prompt.clone(),
        protocol: None,
        allow_context_probe: true,
        models_endpoint,
        declared_context: declared,
        reasoning_model: false,
    };
    let output = run_timed(
        &state.llm,
        &target,
        ProbeTest::Context,
        &options,
        &timeouts,
        Some(&token),
    )
    .await;
    let mut result = output.result;
    let status = ProbeStatus::from_result(&result);
    result.run_id = Some(run_id.clone());
    let cell = status.map(|status| CellRecord {
        provider_id: provider_id.clone(),
        key_id: key_id.to_owned(),
        model_id: model_id.to_owned(),
        test: ProbeTest::Context,
        status,
        reason: reason_line(
            &result,
            status,
            &ReasonContext {
                timeout_s: timeouts.headline(ProbeTest::Context).as_secs(),
                ..ReasonContext::default()
            },
        ),
        skip: None,
        result: Some(result.clone()),
        capability: None,
    });
    let summary = summary_of(&run_id, status);
    let doc = RunDoc {
        spec: Default::default(),
        totals: Some(summary.clone()),
    }
    .to_value();
    let limit = measured_limit(&result);
    let requests = output.requests;
    let stored = {
        let (run_id, provider_id, key_id, model_id, cell) = (
            run_id.clone(),
            provider_id.clone(),
            key_id.to_owned(),
            model_id.to_owned(),
            cell.clone(),
        );
        let (passed, planned) = (summary.passed, summary.total);
        let tz = state.llm.tz_offset_minutes();
        with_db_blocking(state, move |connection| {
            if let Some(cell) = &cell {
                store::persist_cell(connection, &run_id, cell, current_time_ms())?;
            }
            for request in requests {
                let entry = RequestRecord {
                    provider_id: provider_id.clone(),
                    key_id: key_id.clone(),
                    model_id: model_id.clone(),
                    request,
                };
                store::record_probe_request(connection, &run_id, &entry, tz)?;
            }
            if let Some(window) = limit {
                // The measured window never replaces the user's value.
                store::record_measured_context(connection, &provider_id, &model_id, window)?;
            }
            store::finish_probe_run(
                connection,
                &run_id,
                passed,
                planned,
                &doc,
                current_time_ms(),
            )
        })
        .await
    };
    state.llm.touch();
    state.llm.emit(LlmEvent::ModelsChanged {
        provider_id: provider_id.clone(),
    });
    state
        .llm
        .emit(LlmEvent::ProviderStatusChanged { provider_id });
    if let Some(cell) = cell {
        let _ = events.emit(ProbeEvent::Cell {
            run_id: run_id.clone(),
            cell: ProbeCellDto::new(&run_id, cell),
        });
        let _ = events.emit(ProbeEvent::Progress { done: 1, total: 1 });
    }
    let _ = events.emit(ProbeEvent::RunFinished { summary });
    stored?;
    if status.is_none() {
        return Err(AppError::cancelled());
    }
    Ok(result)
}
