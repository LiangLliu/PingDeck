//! Flow use-cases.
//!
//! Single responsibility: CRUD and execution of a workspace's flows, delegating step logic to
//! [`crate::flows`] and sending to the shared request pipeline.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::sync::{Mutex, PoisonError};

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::events::EventSink;
use crate::flows::{
    FlowEvent, FlowRun, StepOutput, forward_flow_events, run_flow, summarize_run, validate_runnable,
};
use crate::workspace::items::{FlowStepKind, FlowStepResult};
use crate::workspace::{
    CookieJarEntry, EnvironmentVariable, Flow, FlowRunInput, FlowRunResult, FlowRunSummary,
    HistoryOrigin, HistorySource, HttpMethod, Request, VariableScopes, WorkspaceId,
};

use super::execution_service::{
    refuse_unreviewed_agent_requests, refuse_unreviewed_agent_step, response_cookie_entries,
    run_pipeline,
};
use super::misc_logic_service::{first_flow_issue_error, validate_flow};
use super::prod_guard::guard_run;
use super::run_activity::RunKind;
use super::{
    AppState, ExecutionContext, ExecutionMeta, Persistence, command_error, current_time_ms,
    deleted_or_not_found, ensure_workspace_exists, execution_context, execution_context_for,
    new_run_id,
};

/// A flow with its most recent stored run, as `flow_list` returns it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct FlowListEntryDto {
    #[serde(flatten)]
    pub flow: Flow,
    /// The newest of the flow's stored runs; `None` when it never ran.
    pub last_run: Option<FlowRunSummary>,
}

/// A flow whose steps run a given saved request, as `flow_ref_list` returns it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FlowRequestRef {
    pub flow_id: String,
    pub flow_name: String,
    /// How many of the flow's steps run the request.
    pub step_count: usize,
}

/// The flows of a workspace that run `request_id` in at least one step, so deleting the request
/// can say what it breaks. Read-only.
pub fn flow_ref_list_impl(
    state: &AppState,
    workspace_id: String,
    request_id: String,
) -> Result<Vec<FlowRequestRef>, AppError> {
    let flows = state
        .store()
        .list_flows(&workspace_id)
        .map_err(command_error)?;
    let request_id = request_id.trim();
    Ok(flows
        .into_iter()
        .filter_map(|flow| {
            let step_count = flow
                .steps
                .iter()
                .filter(|step| step.request_id.trim() == request_id)
                .count();
            (step_count > 0).then_some(FlowRequestRef {
                flow_id: flow.id,
                flow_name: flow.name,
                step_count,
            })
        })
        .collect())
}

/// Lists the flows of a workspace with their last run (one query for all runs).
pub fn flow_list_impl(
    state: &AppState,
    workspace_id: String,
) -> Result<Vec<FlowListEntryDto>, AppError> {
    let store = state.store();
    let flows = store.list_flows(&workspace_id).map_err(command_error)?;
    let mut last_runs = store.last_flow_runs(&workspace_id).map_err(command_error)?;

    Ok(flows
        .into_iter()
        .map(|flow| FlowListEntryDto {
            last_run: last_runs.remove(&flow.id),
            flow,
        })
        .collect())
}

/// The stored runs of a flow, newest first (the newest 20 are kept). A flow that is unknown, or
/// owned by another workspace, is `NotFound`.
pub fn flow_run_list_impl(
    state: &AppState,
    workspace_id: String,
    flow_id: String,
) -> Result<Vec<FlowRunSummary>, AppError> {
    let store = state.store();
    store
        .get_flow(&flow_id)
        .map_err(command_error)?
        .filter(|flow| flow.workspace_id.as_str() == workspace_id)
        .ok_or_else(|| AppError::not_found(format!("flow {flow_id} not found")))?;
    store.list_flow_runs(&flow_id).map_err(command_error)
}

/// Validates the flow (see [`validate_flow`]), then persists it. A step issue names the step id
/// and the field in the error's keys.
pub fn flow_save_impl(state: &AppState, mut flow: Flow) -> Result<Flow, AppError> {
    flow.workspace_id = WorkspaceId::new(flow.workspace_id.as_str()).map_err(command_error)?;
    first_flow_issue_error(&flow, validate_flow(&flow))?;
    flow.updated_at_ms = current_time_ms();

    let store = state.store();
    ensure_workspace_exists(&store, flow.workspace_id.as_str())?;
    store.save_flow(&flow).map_err(command_error)?;
    state.sync.notify_changed();

    Ok(flow)
}

/// Deletes a flow, failing with not-found if it is absent from the workspace.
pub fn flow_delete_impl(
    state: &AppState,
    workspace_id: String,
    flow_id: String,
) -> Result<(), AppError> {
    let store = state.store();
    let deleted = store
        .delete_flow(&flow_id, &workspace_id)
        .map_err(command_error)?;

    state.sync.notify_changed();
    deleted_or_not_found(deleted, "flow", &flow_id, &workspace_id)
}

/// Runs a flow; `cancel_token` lets `execution_cancel_impl` abort it between and during steps.
///
/// `initial_variables` and `cookie_session` fall back to the flow's stored options. `only_step`
/// runs one step (execution order, from 0) from `initial_variables`; `max_steps` stops early.
/// Before anything is sent every step in scope must build its request: a step without a
/// request or URL, or whose saved request was deleted, fails the run with `Validation` naming it
/// by number. With the cookie session on, cookies a step's response sets are sent by the later
/// steps of this run (over the workspace jar) and dropped when it ends; the workspace jar is never
/// written (`Persistence::HistoryOnly`). Script-local variables (`pm.variables`) a step sets reach
/// the later steps of the same run; secret flow variables are masked in every history row.
///
/// Before anything is sent the production guard is asked once with the
/// effective method of every step that can run; a guarded environment without `confirmed: true`
/// fails with [`AppError::ConfirmationRequired`] and writes nothing. A pre-request script that
/// widens a step's method is re-checked per step against the same answer, so the flag a caller
/// never gave cannot be skipped. Every sent step attempt
/// leaves a history row (`source = flow`, the flow's name and the run's id, no cookies). At the
/// end, also for a cancelled run, the run's summary is stored in `flow_runs` (newest 20 per
/// flow). `events` receives the coalesced progress stream and is closed by
/// [`FlowEvent::RunFinished`]; a sink that discards events costs nothing.
#[allow(clippy::too_many_arguments)]
pub async fn flow_run_impl(
    state: &AppState,
    workspace_id: String,
    flow_id: String,
    environment_id: Option<String>,
    initial_variables: Option<BTreeMap<String, String>>,
    max_steps: Option<usize>,
    only_step: Option<usize>,
    cookie_session: Option<bool>,
    cancel_token: Option<String>,
    confirmed: Option<bool>,
    events: &dyn EventSink<FlowEvent>,
) -> Result<FlowRunResult, AppError> {
    let guard = state.cancellations.register(cancel_token.as_deref())?;
    let _running = state.track_run(RunKind::Flow);
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;

    let (flow, requests, environment) = {
        let store = state.store();
        let flow = store
            .get_flow(&flow_id)
            .map_err(command_error)?
            .ok_or_else(|| AppError::not_found(format!("flow {flow_id} not found")))?;
        if flow.workspace_id != workspace_id {
            return Err(AppError::not_found(format!("flow {flow_id} not found")));
        }

        let requests = store
            .list_requests(workspace_id.as_str())
            .map_err(command_error)?
            .into_iter()
            .map(|request| (request.id.as_str().to_owned(), request))
            .collect::<HashMap<_, _>>();
        // Fail fast on an unknown environment or one from another workspace.
        let context = execution_context_for(
            &store,
            workspace_id.as_str(),
            None,
            None,
            environment_id.as_deref(),
        )?;

        (flow, requests, context.environment)
    };

    let input = FlowRunInput {
        initial_variables: initial_variables
            .unwrap_or_else(|| flow.options.initial_variables.clone()),
        max_steps,
        only_step,
        cookie_session: cookie_session.unwrap_or(flow.options.cookie_session),
    };
    validate_runnable(&flow, &requests, &input)?;
    // The agent rule of collection runs (`refuse_unreviewed_agent_requests`): the saved requests
    // of the steps that can run, before anything is sent; each step is checked again as it is
    // sent (`refuse_unreviewed_agent_step`).
    let step_requests: Vec<Request> = {
        let mut steps: Vec<_> = flow.steps.iter().collect();
        steps.sort_by_key(|step| step.position);
        steps
            .into_iter()
            .take(input.max_steps.unwrap_or(usize::MAX))
            .enumerate()
            .filter(|(index, step)| {
                step.step_kind == FlowStepKind::Request
                    && input.only_step.is_none_or(|only| *index == only)
            })
            .filter_map(|(_, step)| requests.get(step.request_id.trim()).cloned())
            .collect()
    };
    refuse_unreviewed_agent_requests(state, step_requests, environment_id.clone()).await?;
    guard_run(
        environment.as_ref(),
        effective_methods(&flow, &requests, &input),
        confirmed,
    )?;

    let run_id = new_run_id();
    let started_at_ms = current_time_ms();
    let origin = HistoryOrigin::run(
        HistorySource::Flow,
        flow.name.clone(),
        flow.id.clone(),
        run_id.clone(),
    );

    // Every step runs through the Send pipeline, with history but without cookie persistence.
    // The context is rebuilt per step, so script-set variables reach later steps; the flow
    // variables are its narrowest (`local`) scope and the local scope a step leaves behind
    // becomes the next step's. Conditions are evaluated against the very same context.
    let token = guard.token();
    let environment_id = environment_id.as_deref();
    let workspace_key = workspace_id.as_str();
    // The run's own cookies: never stored, gone with the run.
    let run_jar: Mutex<Vec<CookieJarEntry>> = Mutex::new(Vec::new());
    let (run_jar, cookie_session) = (&run_jar, input.cookie_session);
    let condition_scopes = |request: Option<Request>, variables: Vec<EnvironmentVariable>| async move {
        // An unresolvable context (e.g. the environment was deleted mid-run) leaves only the flow
        // variables; the step itself then reports the real error.
        flow_step_context(
            state,
            workspace_key,
            request.as_ref(),
            environment_id,
            variables.clone(),
        )
        .map(|context| context.scopes())
        .unwrap_or_else(|_| VariableScopes::new().with_local_vars(variables))
    };
    let execute_step = |request: Request, variables: Vec<EnvironmentVariable>| {
        let origin = origin.clone();
        async move {
            let mut context = flow_step_context(
                state,
                workspace_key,
                Some(&request),
                environment_id,
                variables,
            )?;
            // The pre-run review saw the saved request with the environment only; the step sends
            // its overrides with the flow's variables, which earlier responses may have set.
            refuse_unreviewed_agent_step(state, &request, &context)?;
            let captures = cookie_session && request.network_options.send_cookies;
            if captures {
                let run_cookies = run_jar
                    .lock()
                    .unwrap_or_else(PoisonError::into_inner)
                    .clone();
                merge_cookies(&mut context.cookie_jar, run_cookies);
            }
            let sent = captures.then(|| request.clone());
            let failure_history_id = Mutex::new(None);
            let result = run_pipeline(
                state,
                request,
                &mut context,
                // A pre-request script that widens the method is re-checked against the flow's
                // `confirmed` answer (guard_method_change).
                ExecutionMeta::confirmed(confirmed),
                Some(token),
                Persistence::HistoryOnly(origin),
                &failure_history_id,
            )
            .await
            .map(|(response, _)| response);
            if let (Some(sent), Ok(response)) = (&sent, &result) {
                let set = response_cookie_entries(sent, response);
                merge_cookies(
                    &mut run_jar.lock().unwrap_or_else(PoisonError::into_inner),
                    set,
                );
            }
            result.map(|execution| StepOutput {
                execution,
                local: Some(context.local),
            })
        }
    };

    // The run and the event pump are polled together; the pump ends when the run's sender (moved
    // into `runner`, dropped with it) is gone.
    let live = events.is_active();
    let total = flow.steps.len();
    let (sender, receiver) = tokio::sync::mpsc::unbounded_channel();
    let (flow_ref, requests_ref, input_ref) = (&flow, &requests, &input);
    let runner = async move {
        let on_step = |index: usize, result: &FlowStepResult| {
            if live {
                // A closed pump means the consumer is gone: nothing to deliver.
                let _ = sender.send(FlowEvent::step_finished(&run_id, total, index, result));
            }
        };
        let run = FlowRun {
            run_id: run_id.clone(),
            started_at_ms,
            cancel: Some(token),
            on_step: Some(&on_step),
        };
        let mut result = run_flow(
            flow_ref,
            requests_ref,
            input_ref,
            &run,
            condition_scopes,
            execute_step,
        )
        .await;
        result.environment_id = environment_id.map(str::to_owned);
        store_run_summary(state, workspace_key, &result);
        if live {
            let _ = sender.send(FlowEvent::run_finished(&result));
        }
        result
    };
    let pump = async {
        if live {
            forward_flow_events(receiver, events).await;
        }
    };
    let (result, ()) = tokio::join!(runner, pump);

    Ok(result)
}

/// The method each step that can run will send: its override, else the saved request's, else GET
/// (an inline step, or a saved request that no longer exists, which fails when it runs).
fn effective_methods(
    flow: &Flow,
    requests: &HashMap<String, Request>,
    input: &FlowRunInput,
) -> Vec<HttpMethod> {
    let mut steps: Vec<_> = flow.steps.iter().collect();
    steps.sort_by_key(|step| step.position);
    steps
        .into_iter()
        .take(input.max_steps.unwrap_or(usize::MAX))
        .enumerate()
        .filter(|(index, step)| {
            step.step_kind == FlowStepKind::Request
                && input.only_step.is_none_or(|only| *index == only)
        })
        .map(|(_, step)| {
            step.method_override
                .or_else(|| {
                    requests
                        .get(step.request_id.trim())
                        .map(Request::guard_method)
                })
                .unwrap_or(HttpMethod::Get)
        })
        .collect()
}

/// Adds `entries` to `jar`, each replacing the cookie with the same name, domain and path.
fn merge_cookies(jar: &mut Vec<CookieJarEntry>, entries: Vec<CookieJarEntry>) {
    for entry in entries {
        jar.retain(|old| {
            !(old.name == entry.name
                && old.path == entry.path
                && old.domain.eq_ignore_ascii_case(&entry.domain))
        });
        jar.push(entry);
    }
}

/// Stores the finished run in `flow_runs`. A failure is reported, not returned: the live result
/// the caller already waited for is worth more than the stored summary (same rule as history).
fn store_run_summary(state: &AppState, workspace_id: &str, result: &FlowRunResult) {
    let summary = summarize_run(result, current_time_ms());
    if let Err(error) = state.store().save_flow_run(workspace_id, &summary) {
        crate::logging::log_line(
            crate::logging::LogLevel::Error,
            "flow.history",
            &format!("failed to save flow run {}: {error}", result.run_id),
        );
    }
}

/// Builds the execution context of one flow step (or of its condition): the request's
/// collection, folders, environment, globals and certificates, with the flow variables as the
/// narrowest (local) scope, secret ones flagged. Without a request only the workspace-level
/// scopes apply.
fn flow_step_context(
    state: &AppState,
    workspace_id: &str,
    request: Option<&Request>,
    environment_id: Option<&str>,
    variables: Vec<EnvironmentVariable>,
) -> Result<ExecutionContext, AppError> {
    let mut context = {
        let store = state.store();
        match request {
            Some(request) => execution_context(&store, request, environment_id)?,
            None => execution_context_for(&store, workspace_id, None, None, environment_id)?,
        }
    };
    context.local_secrets = variables
        .iter()
        .filter(|variable| variable.is_secret)
        .map(|variable| variable.key.clone())
        .collect::<BTreeSet<_>>();
    context.local = variables
        .into_iter()
        .filter_map(|variable| Some((variable.key, variable.value?)))
        .collect();
    Ok(context)
}
