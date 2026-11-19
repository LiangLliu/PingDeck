use std::{collections::BTreeMap, time::Duration};

use tokio::time::sleep;

use super::super::misc_logic_service::{RUNNER_MAX_DELAY_MS, RUNNER_MAX_ITERATIONS};
use super::super::prod_guard::guard_run;
use super::super::run_activity::RunKind;
use super::super::{
    AppState, CollectionRunStatus, CollectionRunSummary, RunCollectionInput, RunnerItem,
    RunnerResult, command_error, new_run_id, request_belongs_to_collection,
};
use super::context::{ExecutionContext, ExecutionMeta, blocking, execution_context_for};
use super::pipeline::{Persistence, run_pipeline};
use super::scripts::script_sources;
use crate::cancel::until_cancelled;
use crate::error::AppError;
use crate::events::{EventSink, RunnerEvent};
use crate::scripts::ScriptPhase;
use crate::workspace::{
    CollectionId, EnvironmentId, Folder, FolderId, HistoryOrigin, HistorySource, Request,
    WorkspaceId,
};
use pingdeck_base::time::now_ms;
use pingdeck_model::runner_item::data_preview;

/// Runs every request of a collection per iteration; `cancel_token` stops it early.
///
/// A cancelled run is not an error: it returns the items collected so far with `stopped: true`.
///
/// Every finished request is also pushed to `events` as it completes; the returned
/// [`RunnerResult`] still carries all items.
pub async fn run_collection_impl(
    state: &AppState,
    input: RunCollectionInput,
    cancel_token: Option<String>,
    events: &dyn EventSink<RunnerEvent>,
) -> Result<RunnerResult, AppError> {
    const MAX_RUNNER_STEPS: usize = 10_000;

    let started_at_ms = now_ms();
    let guard = state.cancellations.register(cancel_token.as_deref())?;
    let token = guard.token();
    let _running = state.track_run(RunKind::Collection);

    let workspace_id = WorkspaceId::new(&input.workspace_id).map_err(command_error)?;
    let collection_id = CollectionId::new(&input.collection_id).map_err(command_error)?;
    let environment_id = input
        .environment_id
        .as_deref()
        .map(EnvironmentId::new)
        .transpose()
        .map_err(command_error)?;
    let data = input.data.unwrap_or_default();
    let iterations = input
        .iterations
        .unwrap_or(if data.is_empty() {
            1
        } else {
            data.len() as u32
        })
        .clamp(1, RUNNER_MAX_ITERATIONS);
    let delay_ms = input.delay_ms.unwrap_or(0).min(RUNNER_MAX_DELAY_MS);
    let stop_on_error = input.stop_on_error.unwrap_or(false);
    let confirmed = input.confirmed;

    let (collection, requests, environment) = {
        let (workspace_id, collection_id) = (workspace_id.clone(), collection_id.clone());
        let (folder_id, request_ids) = (input.folder_id.clone(), input.request_ids.clone());
        blocking(state, move |state| {
            let store = state.store();
            let collection = store
                .get_collection(&workspace_id, &collection_id)
                .map_err(command_error)?
                .ok_or_else(|| {
                    AppError::not_found(format!("collection {} not found", collection_id.as_str()))
                })?;
            let environment = environment_id
                .as_ref()
                .map(|environment_id| {
                    store
                        .get_environment(&workspace_id, environment_id)
                        .map_err(command_error)?
                        .ok_or_else(|| {
                            AppError::not_found(format!(
                                "environment {} not found",
                                environment_id.as_str()
                            ))
                        })
                })
                .transpose()?;

            let folders = store
                .list_folders(&workspace_id, &collection.id)
                .map_err(command_error)?;
            // `list_requests` is ordered by stored position, which is the
            // collection author's order. Never sort by name here.
            let requests = store
                .list_requests(collection.workspace_id.as_str())
                .map_err(command_error)?
                .into_iter()
                .filter(|request| request_belongs_to_collection(request, &collection, &folders))
                .collect::<Vec<_>>();
            let requests = scope_run_requests(
                requests,
                &folders,
                folder_id.as_deref(),
                request_ids.as_deref(),
            )?;

            Ok((collection, requests, environment))
        })
        .await?
    };

    let run_id = new_run_id();
    if requests.is_empty() {
        return Ok(RunnerResult {
            run_id,
            collection_id: collection.id.as_str().to_owned(),
            iterations,
            total: 0,
            passed: 0,
            failed: 0,
            stopped: false,
            items: Vec::new(),
        });
    }

    // A request the agent changed that sends a credential to an undeclared host is reviewed by a
    // single Send first; nothing has been sent yet.
    super::refuse_unreviewed_agent_requests(state, requests.clone(), input.environment_id.clone())
        .await?;
    // One confirmation covers the whole run; nothing has been sent yet.
    guard_run(
        environment.as_ref(),
        requests.iter().map(Request::guard_method),
        confirmed,
    )?;
    let origin = HistoryOrigin::run(
        HistorySource::Runner,
        &collection.name,
        collection.id.as_str(),
        run_id.clone(),
    );

    let mut items = Vec::new();
    let mut stopped = false;
    let mut live = events.is_active();
    let mut steps = 0_usize;

    'iterations: for iteration in 0..iterations {
        let data_row = (!data.is_empty()).then(|| iteration as usize % data.len());
        let row = data_row.map_or_else(BTreeMap::new, |at| data[at].clone());
        let data_row = data_row.and_then(|at| u32::try_from(at).ok());
        let preview = data_preview(&row);
        // Loaded once per iteration (script-local variables start empty): a step carries the
        // context forward and only refreshes what the finished step can have changed.
        let mut context = execution_context_blocking(
            state,
            &workspace_id,
            &collection.id,
            input.environment_id.as_deref(),
        )
        .await?;
        let mut refresh = None;
        let mut index = 0_usize;

        loop {
            if index >= requests.len() || steps >= MAX_RUNNER_STEPS {
                if steps >= MAX_RUNNER_STEPS {
                    stopped = true;
                }
                break;
            }

            if token.is_cancelled() {
                stopped = true;
                break 'iterations;
            }
            if steps > 0 && delay_ms > 0 {
                let delay = Duration::from_millis(delay_ms);
                if until_cancelled(Some(token), sleep(delay)).await.is_none() {
                    stopped = true;
                    break 'iterations;
                }
            }

            if let Some(variables) = refresh.take() {
                refresh_run_context(state, &workspace_id, &mut context, variables).await?;
            }
            let request = requests[index].clone();
            // Script-local variables (`pm.variables`) live for one iteration: the context is
            // loaded fresh per iteration and carries them from request to request.
            let has_scripts = !script_sources(&context, &request, ScriptPhase::PreRequest)
                .is_empty()
                || !script_sources(&context, &request, ScriptPhase::PostResponse).is_empty();
            let meta = ExecutionMeta {
                iteration,
                iteration_count: iterations,
                data: row.clone(),
                confirmed,
                ..ExecutionMeta::default()
            };

            let failure_history_id = std::sync::Mutex::new(None);
            let outcome = run_pipeline(
                state,
                request.clone(),
                &mut context,
                meta,
                Some(token),
                Persistence::Record(origin.clone()),
                &failure_history_id,
            )
            .await;
            if outcome.as_ref().is_err_and(AppError::is_cancelled) {
                stopped = true;
                break 'iterations;
            }
            steps += 1;
            refresh = Some(has_scripts);

            let (item, next_request, stop_run) = match outcome {
                Ok((response, history_entry_id)) => {
                    let assertions_passed = response
                        .assertion_results
                        .iter()
                        .all(|result| result.passed);
                    let scripts_passed = response
                        .script_tests
                        .iter()
                        .all(|test| test.passed || test.skipped);
                    // The executor masked both URLs; the sent one is the request's own, the
                    // response one the last redirect hop.
                    // An AI run that did not end with an answer fails the row.
                    let ai_error = response.ai.as_ref().and_then(|ai| ai.error.clone());
                    let url = response
                        .sent_request
                        .as_ref()
                        .map_or_else(|| response.url.clone(), |sent| sent.url.clone());
                    let item = RunnerItem {
                        request_id: request.id.as_str().to_owned(),
                        request_name: request.name.clone(),
                        method: request.method,
                        url,
                        history_entry_id,
                        iteration,
                        data_row,
                        data_preview: preview.clone(),
                        status_code: Some(response.status_code),
                        status_text: response.status_text,
                        elapsed_ms: response.elapsed_ms,
                        attempt_count: response.attempt_count,
                        passed: assertions_passed
                            && scripts_passed
                            && response.script_error.is_none()
                            && ai_error.is_none(),
                        error: ai_error,
                        assertion_results: response.assertion_results,
                        script_tests: response.script_tests,
                        console: response.console,
                        script_error: response.script_error,
                        ai: response.ai,
                    };
                    (item, response.next_request, response.stop_run)
                }
                Err(error) => (
                    RunnerItem {
                        request_id: request.id.as_str().to_owned(),
                        request_name: request.name.clone(),
                        method: request.method,
                        url: unsent_request_url(&context, &request),
                        history_entry_id: failure_history_id
                            .into_inner()
                            .unwrap_or_else(std::sync::PoisonError::into_inner),
                        iteration,
                        data_row,
                        data_preview: preview.clone(),
                        status_code: None,
                        status_text: "Failed".into(),
                        elapsed_ms: 0,
                        attempt_count: 1,
                        passed: false,
                        error: Some(error.message().to_owned()),
                        assertion_results: Vec::new(),
                        script_tests: Vec::new(),
                        console: Vec::new(),
                        script_error: None,
                        ai: None,
                    },
                    None,
                    false,
                ),
            };

            let failed = !item.passed;
            // A sink that discards events or reported its consumer gone never gets (or costs) a clone.
            if live {
                live = events.emit(RunnerEvent::ItemCompleted { item: item.clone() });
            }
            items.push(item);
            if stop_run {
                stopped = true;
                break 'iterations;
            }
            if stop_on_error && failed {
                stopped = true;
                break 'iterations;
            }

            match next_request {
                Some(next) => {
                    if let Some(next_index) = requests.iter().position(|candidate| {
                        candidate.name == next || candidate.id.as_str() == next
                    }) {
                        index = next_index;
                    } else {
                        index += 1;
                    }
                }
                None => index += 1,
            }
        }
    }

    let passed = items.iter().filter(|item| item.passed).count();
    let total = items.len();
    let result = RunnerResult {
        run_id,
        collection_id: collection.id.as_str().to_owned(),
        iterations,
        total,
        passed,
        failed: total - passed,
        stopped,
        items,
    };
    let summary = CollectionRunSummary {
        run_id: result.run_id.clone(),
        workspace_id: workspace_id.as_str().to_owned(),
        collection_id: result.collection_id.clone(),
        collection_name: collection.name.clone(),
        folder_id: input.folder_id,
        request_ids: input.request_ids,
        environment_id: environment.as_ref().map(|e| e.id.as_str().to_owned()),
        environment_name: environment.map(|e| e.name),
        started_at_ms,
        finished_at_ms: now_ms(),
        status: if stopped {
            CollectionRunStatus::Stopped
        } else {
            CollectionRunStatus::Done
        },
        iterations,
        planned: (requests.len() * iterations as usize).min(MAX_RUNNER_STEPS),
        total,
        passed,
        failed: total - passed,
        // The console stays with the live result; the history rows hold each exchange.
        items: result
            .items
            .iter()
            .map(|item| RunnerItem {
                console: Vec::new(),
                ..item.clone()
            })
            .collect(),
    };
    // A failed write is reported, not returned: the live result is worth more (same rule as
    // flow runs and history).
    let saved = blocking(state, move |state| {
        state
            .store()
            .save_collection_run(&summary)
            .map_err(command_error)
    })
    .await;
    if let Err(error) = saved {
        crate::logging::log_line(
            crate::logging::LogLevel::Error,
            "runner.history",
            &format!("failed to save collection run {}: {error}", result.run_id),
        );
    }
    Ok(result)
}

/// A stored collection run of `workspace_id` (the newest 20 of each collection are kept); an
/// unknown, pruned or foreign run is `NotFound`.
pub fn collection_run_get_impl(
    state: &AppState,
    workspace_id: String,
    run_id: String,
) -> Result<CollectionRunSummary, AppError> {
    state
        .store()
        .get_collection_run(&workspace_id, &run_id)
        .map_err(command_error)?
        .ok_or_else(|| AppError::not_found(format!("collection run {run_id} not found")))
}

/// The requests a run covers: `requests` (the collection's, in author order) narrowed to
/// `folder_id` and its sub-folders and then to `request_ids`; the order never follows the
/// given ids.
fn scope_run_requests(
    mut requests: Vec<Request>,
    folders: &[Folder],
    folder_id: Option<&str>,
    request_ids: Option<&[String]>,
) -> Result<Vec<Request>, AppError> {
    if let Some(folder_id) = folder_id {
        if !folders.iter().any(|folder| folder.id.as_str() == folder_id) {
            return Err(AppError::not_found(format!(
                "folder {folder_id} not found in the collection"
            )));
        }
        let mut subtree = vec![folder_id];
        let mut next = 0;
        while let Some(&parent) = subtree.get(next) {
            next += 1;
            subtree.extend(
                folders
                    .iter()
                    .filter(|folder| {
                        folder.parent_folder_id.as_ref().map(FolderId::as_str) == Some(parent)
                    })
                    .map(|folder| folder.id.as_str()),
            );
        }
        requests.retain(|request| {
            request
                .folder_id
                .as_ref()
                .is_some_and(|id| subtree.contains(&id.as_str()))
        });
    }
    if let Some(ids) = request_ids {
        if ids.is_empty() {
            return Err(AppError::validation("Select at least one request to run"));
        }
        if let Some(missing) = ids.iter().find(|id| {
            !requests
                .iter()
                .any(|request| request.id.as_str() == id.as_str())
        }) {
            return Err(AppError::not_found(format!(
                "request {missing} is not part of this run"
            )));
        }
        requests.retain(|request| ids.iter().any(|id| id == request.id.as_str()));
    }
    Ok(requests)
}

/// The URL of a request that failed before it was sent: its template with the variables of
/// `context` filled in and every secret value masked.
fn unsent_request_url(context: &ExecutionContext, request: &Request) -> String {
    let scopes = context.scopes();
    pingdeck_base::secrets::mask_secrets(
        &crate::workspace::resolve_template_lenient(&request.url, &scopes),
        &scopes.secret_values(),
    )
}

/// [`execution_context_for`] on a blocking worker, for the collection `collection_id` (no folder).
async fn execution_context_blocking(
    state: &AppState,
    workspace_id: &WorkspaceId,
    collection_id: &CollectionId,
    environment_id: Option<&str>,
) -> Result<ExecutionContext, AppError> {
    let (workspace_id, collection_id) = (workspace_id.clone(), collection_id.clone());
    let environment_id = environment_id.map(str::to_owned);
    blocking(state, move |state| {
        let store = state.store();
        execution_context_for(
            &store,
            workspace_id.as_str(),
            Some(collection_id.as_str()),
            None,
            environment_id.as_deref(),
        )
    })
    .await
}

/// Reloads what a finished runner step can have changed in `context`: the cookie jar (the
/// response may have set cookies) and, when `variables` (a script ran), the collection,
/// environment and globals as persisted. Folders and certificates stay as loaded.
async fn refresh_run_context(
    state: &AppState,
    workspace_id: &WorkspaceId,
    context: &mut ExecutionContext,
    variables: bool,
) -> Result<(), AppError> {
    let workspace_id = workspace_id.clone();
    let collection_id = context.collection.as_ref().map(|c| c.id.clone());
    let environment_id = context.environment.as_ref().map(|e| e.id.clone());
    let (cookie_jar, fresh) = blocking(state, move |state| {
        let store = state.store();
        let cookie_jar = store
            .list_cookie_jar(workspace_id.as_str())
            .map_err(command_error)?;
        if !variables {
            return Ok((cookie_jar, None));
        }
        let collection = collection_id
            .map(|id| {
                store
                    .get_collection(&workspace_id, &id)
                    .map_err(command_error)?
                    .ok_or_else(|| {
                        AppError::not_found(format!("collection {} not found", id.as_str()))
                    })
            })
            .transpose()?;
        let environment = environment_id
            .map(|id| {
                store
                    .get_environment(&workspace_id, &id)
                    .map_err(command_error)?
                    .ok_or_else(|| {
                        AppError::not_found(format!("environment {} not found", id.as_str()))
                    })
            })
            .transpose()?;
        let globals = store
            .list_global_variables(workspace_id.as_str())
            .map_err(command_error)?;
        Ok((cookie_jar, Some((collection, environment, globals))))
    })
    .await?;
    context.cookie_jar = cookie_jar;
    if let Some((collection, environment, globals)) = fresh {
        context.collection = collection;
        context.environment = environment;
        context.globals = globals;
    }
    Ok(())
}
