//! `ai_request_run`: one AI request, through the HTTP pipeline's stages.
//!
//! Single responsibility: variable resolution, collection/folder/request pre-request scripts,
//! the run in the core, the test scripts with `pm.response.ai`, the assertions (HTTP kinds on the
//! raw response, AI kinds on the merged reply), the console line and one history entry. The
//! production guard keyed on the call's environment and `ensure_sendable` run before anything
//! is resolved or sent. A script cannot change the destination of a provider target: after the
//! pre-request phase the core reverts URL, query, path variables and auth and drops the protected
//! headers, and lists their names in the result.

use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use super::AiRequestRunInput;
use super::history::{
    console_entry, execution_result, failure_entry, script_result, step_summary, success_entry,
    view,
};
use super::prepare::{Loaded, load, make_spec, resolve_call};
use crate::app::execution_service::{
    ExecutionContext, ExecutionMeta, Persistence, ScriptDiagnostics, apply_script_variables,
    persist_failure, persist_script_state, run_script_phase,
    save_execution_history_if_workspace_exists, script_input, script_sources,
};
use crate::app::prod_guard::guard_request;
use crate::app::{AppState, new_run_id};
use crate::cancel::CancelToken;
use crate::error::AppError;
use crate::events::{EventSink, NoEvents};
use crate::executor::ExecutionResult;
use crate::llm::ai_run::{AiResultDto, AiRunEvent, AiRunStatus, run_ai_request};
use crate::scripts::{ScriptLimits, ScriptPhase};
use crate::workspace::ai_request::{ScriptRequestPolicy, ensure_sendable};
use crate::workspace::assertions::evaluate_assertions_with_ai;
use crate::workspace::{ExecutionHistoryEntry, HistoryOrigin, HttpMethod, Request, effective_auth};
use pingdeck_model::script_result::{ConsoleEntry, ConsoleLevel, ScriptError};

/// Runs a saved request or draft and records its history entry; `cancel_token` lets
/// [`ai_request_cancel_impl`] stop it. A stop keeps the partial result (status `Stopped`); only a
/// stop during the scripts is [`AppError::Cancelled`].
///
/// `Started`, the coalesced `Frames` and, last, `Done` go to `sink`.
///
/// # Errors
/// [`AppError::ConfirmationRequired`] for a guarded environment without confirmation (nothing is
/// sent), [`AppError::Validation`] for a request that is not an AI request, still holds the
/// provider-key placeholder, has an unresolved variable or a failing script,
/// [`AppError::Unsupported`] naming a missing endpoint, [`AppError::NotFound`].
pub async fn ai_request_run_impl(
    state: &AppState,
    input: AiRequestRunInput,
    cancel_token: Option<String>,
    sink: &dyn EventSink<AiRunEvent>,
) -> Result<AiResultDto, AppError> {
    let guard = state.cancellations.register(cancel_token.as_deref())?;
    let Loaded {
        request,
        mut context,
    } = load(
        state,
        &input.workspace_id,
        input.request_id.as_deref(),
        input.request,
        input.environment_id.as_deref(),
    )?;
    ensure_sendable(&request)?;
    guard_request(
        context.environment.as_ref(),
        HttpMethod::Post,
        input.confirmed,
    )?;
    let recording = Recording {
        origin: &HistoryOrigin::request(),
        keep_exchange: input.keep_exchange,
        history: true,
        failure_slot: None,
    };
    let finished = execute(
        state,
        request,
        &mut context,
        &ExecutionMeta::default(),
        recording,
        guard.token(),
        sink,
    )
    .await?;
    Ok(finished.dto)
}

/// The HTTP pipeline's branch for an AI request: a collection run, a flow step or a
/// draft send of an AI request runs here and comes back as the [`ExecutionResult`] they read,
/// with the model, finish reason and cost in `ai`. Its assertions are judged like a Send of it.
///
/// `context` is left holding the variables the scripts changed. A run without an HTTP answer is
/// an error (a network error for an unreachable host, so a flow step retries it like HTTP); its
/// history row goes to `failure_slot`. A run stopped by `cancel` is [`AppError::Cancelled`].
///
/// # Errors
/// As [`ai_request_run_impl`], plus the ones above.
pub(crate) async fn run_in_pipeline(
    state: &AppState,
    mut request: Request,
    context: &mut ExecutionContext,
    meta: &ExecutionMeta,
    cancel: Option<&CancelToken>,
    persistence: &Persistence,
    failure_slot: &Mutex<Option<String>>,
) -> Result<(ExecutionResult, Option<String>), AppError> {
    // Fields the request inherits take today's Settings value, as in `ai_request_run`.
    state
        .settings()
        .network
        .apply_inherited(&mut request.network_options);
    ensure_sendable(&request)?;
    let own = CancelToken::new();
    let cancel = cancel.unwrap_or(&own);
    let recording = Recording {
        origin: persistence.origin(),
        keep_exchange: true,
        history: persistence.records_history(),
        failure_slot: Some(failure_slot),
    };
    let Finished { dto, mut response } =
        execute(state, request, context, meta, recording, cancel, &NoEvents).await?;
    if cancel.is_cancelled() {
        return Err(AppError::cancelled());
    }
    if dto.http_status.is_none() {
        *failure_slot.lock().unwrap_or_else(PoisonError::into_inner) = dto.history_id.clone();
        let message = dto
            .message
            .clone()
            .unwrap_or_else(|| dto.status.label().to_owned());
        return Err(if dto.status == AiRunStatus::Unsupported {
            AppError::unsupported(message)
        } else {
            AppError::network(message)
        });
    }
    response.ai = Some(step_summary(&dto));
    response.assertion_results = dto.assertion_results;
    response.script_tests = dto.script_tests;
    response.console = dto.console;
    response.script_error = dto.script_error;
    response.history_saved = dto.history_saved;
    response.history_id.clone_from(&dto.history_id);
    Ok((response, dto.history_id))
}

/// Stops the run registered under `cancel_token` (also a compare run, or one row of it as
/// `<token>#<index>`); `false` when none is running.
pub fn ai_request_cancel_impl(state: &AppState, cancel_token: String) -> bool {
    state.cancellations.cancel(&cancel_token)
}

/// How one run is recorded.
struct Recording<'a> {
    origin: &'a HistoryOrigin,
    /// "Keep this exchange in the log".
    keep_exchange: bool,
    /// Whether a history row is written (`Persistence::Unrecorded` writes none).
    history: bool,
    /// Receives the id of the row a run that fails before the core call writes.
    failure_slot: Option<&'a Mutex<Option<String>>>,
}

/// A finished run: the result and the response its scripts and assertions saw.
struct Finished {
    dto: AiResultDto,
    response: ExecutionResult,
}

/// The pipeline of one run.
async fn execute(
    state: &AppState,
    mut request: Request,
    context: &mut ExecutionContext,
    meta: &ExecutionMeta,
    recording: Recording<'_>,
    cancel: &CancelToken,
    sink: &dyn EventSink<AiRunEvent>,
) -> Result<Finished, AppError> {
    let started = Instant::now();
    let settings = state.settings();
    let mut limits = ScriptLimits::from(&*settings);
    limits.network.tls = crate::executor::TlsMaterial {
        certificates: context.certificates.clone(),
        passphrases: crate::executor::CertificatePassphrases::new(std::sync::Arc::clone(
            state.secrets(),
        )),
    };
    let Recording {
        origin,
        keep_exchange,
        history,
        failure_slot,
    } = recording;
    let pre = script_sources(context, &request, ScriptPhase::PreRequest);
    let post = script_sources(context, &request, ScriptPhase::PostResponse);
    let mut diagnostics = ScriptDiagnostics::default();
    let original = request.clone();
    let refuse =
        |request: &Request, context: &ExecutionContext, kind: &str, message: &str| -> bool {
            if !history {
                return false;
            }
            let Ok(mut entry) = failure_entry(
                request,
                context.environment.as_ref(),
                kind,
                message,
                started.elapsed().as_millis(),
                origin,
            ) else {
                return true;
            };
            let id = entry.id.as_str().to_owned();
            match save_execution_history_if_workspace_exists(state, &mut entry) {
                Ok(saved) => {
                    if let Some(slot) = failure_slot {
                        *slot.lock().unwrap_or_else(PoisonError::into_inner) = saved.then_some(id);
                    }
                    false
                }
                Err(error) => {
                    crate::logging::log_line(
                        crate::logging::LogLevel::Error,
                        "ai-run.history",
                        &format!("failed to save failed AI run history: {error}"),
                    );
                    true
                }
            }
        };

    if !pre.is_empty() {
        let before = context.clone();
        let input = script_input(
            ScriptPhase::PreRequest,
            pre,
            &request,
            None,
            context,
            meta,
            Some(cancel),
            limits.clone(),
        );
        let mut output = run_script_phase(input, Some(cancel)).await?;
        apply_script_variables(context, &output);
        let saved =
            persist_script_state(state, &output.request, &before, context, &output.mutations);
        diagnostics.merge(&mut output);
        request = output.request;
        if let Some(error) = diagnostics.error.take() {
            let message = format!(
                "pre-request script failed in {}: {}",
                error.source, error.message
            );
            let unsaved = refuse(&request, context, "ScriptError", &message);
            return Err(AppError::validation(message).with_history_unsaved(unsaved));
        }
        if let Err(error) = saved {
            let message = format!("failed to persist pre-request variables: {error}");
            let unsaved = refuse(&request, context, "ScriptError", &message);
            return Err(persist_failure(&error, message).with_history_unsaved(unsaved));
        }
    }
    // The destination of a provider target is the core's: whatever a script changed is undone.
    let dropped = ScriptRequestPolicy::for_request(&original).enforce(&original, &mut request);
    request.auth = effective_auth(&request, context.collection.as_ref(), &context.folders);

    let call = match resolve_call(&request, context, dropped) {
        Ok(call) => call,
        Err(error) => {
            let unsaved = refuse(&request, context, "Validation", error.message());
            return Err(error.with_history_unsaved(unsaved));
        }
    };
    let run_id = new_run_id();
    let spec = match make_spec(state, &call, &run_id, keep_exchange).await {
        Ok(spec) => spec,
        Err(error) => {
            let unsaved = refuse(&request, context, "Validation", error.message());
            return Err(error.with_history_unsaved(unsaved));
        }
    };
    let outcome = run_ai_request(&state.llm, spec, sink, cancel).await;
    let mut dto = outcome.result;

    diagnostics.console.push(console_entry(&dto));
    let mut response = execution_result(&dto, &outcome.raw_body);
    if !post.is_empty() {
        let before = context.clone();
        let mut input = script_input(
            ScriptPhase::PostResponse,
            post,
            &request,
            Some(response),
            context,
            meta,
            Some(cancel),
            limits,
        );
        input.ai = Some(script_result(&dto));
        let mut output = run_script_phase(input, Some(cancel)).await?;
        response = output
            .response
            .take()
            .ok_or_else(|| AppError::internal("post-response script runtime lost the response"))?;
        apply_script_variables(context, &output);
        let saved = persist_script_state(state, &request, &before, context, &output.mutations);
        diagnostics.merge(&mut output);
        if let Err(error) = saved {
            let message = format!("failed to persist post-response variables: {error}");
            let source = "Script variable persistence".to_owned();
            diagnostics.console.push(ConsoleEntry {
                level: ConsoleLevel::Error,
                message: message.clone(),
                source: source.clone(),
            });
            if diagnostics.error.is_none() {
                diagnostics.error = Some(ScriptError { source, message });
            }
        }
    }
    dto.assertion_results =
        evaluate_assertions_with_ai(&request.assertions, &response, Some(&view(&dto)));
    dto.script_tests = diagnostics.tests;
    dto.console = diagnostics.console;
    dto.script_error = diagnostics.error;

    // `setNextRequest` steers a collection run like after an HTTP send.
    match diagnostics.next_request {
        Some(Some(name)) => response.next_request = Some(name),
        Some(None) => response.stop_run = true,
        None => {}
    }

    dto.history_id = if history {
        record(state, &request, context, &dto, origin)
    } else {
        None
    };
    dto.history_saved = dto.history_id.is_some();
    sink.emit(AiRunEvent::Done {
        result: Box::new(dto.clone()),
    });
    Ok(Finished { dto, response })
}

/// Saves the history entry of a finished run and returns its id; `None` when the workspace is
/// gone or the write failed (logged, never fatal).
pub(super) fn record(
    state: &AppState,
    request: &Request,
    context: &ExecutionContext,
    dto: &AiResultDto,
    origin: &HistoryOrigin,
) -> Option<String> {
    let saved = success_entry(request, context.environment.as_ref(), dto, origin).and_then(
        |mut entry: ExecutionHistoryEntry| {
            let id = entry.id.as_str().to_owned();
            save_execution_history_if_workspace_exists(state, &mut entry)
                .map(|saved| saved.then_some(id))
        },
    );
    saved.unwrap_or_else(|error| {
        crate::logging::log_line(
            crate::logging::LogLevel::Error,
            "ai-run.history",
            &format!("failed to save AI run history: {error}"),
        );
        None
    })
}
