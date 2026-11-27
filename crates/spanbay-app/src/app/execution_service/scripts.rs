use tokio::task::spawn_blocking;

use super::super::{AppState, command_error};
use super::context::{ExecutionContext, ExecutionMeta, blocking};
use crate::cancel::{CancelToken, until_cancelled};
use crate::error::AppError;
use crate::executor::ExecutionResult;
use crate::scripts::{
    ScopeMutations, ScriptInfo, ScriptInput, ScriptLimits, ScriptMutations, ScriptOutput,
    ScriptPhase, ScriptSource, run_scripts,
};
use crate::workspace::{EnvironmentVariable, Request, WorkspaceId, folder_chain};
use spanbay_model::script_result::{ConsoleEntry, ScriptError, ScriptTestResult};

/// Collects the non-empty scripts of `phase` in execution order: collection, folders, request.
pub(crate) fn script_sources(
    context: &ExecutionContext,
    request: &Request,
    phase: ScriptPhase,
) -> Vec<ScriptSource> {
    let mut sources = Vec::new();
    if let Some(collection) = context.collection.as_ref() {
        let source = match phase {
            ScriptPhase::PreRequest => &collection.pre_request_script,
            ScriptPhase::PostResponse => &collection.test_script,
        };
        if !source.trim().is_empty() {
            sources.push(ScriptSource {
                label: format!("Collection · {}", collection.name),
                source: source.clone(),
            });
        }
    }

    for folder in folder_chain(request, &context.folders).folders {
        let source = match phase {
            ScriptPhase::PreRequest => &folder.pre_request_script,
            ScriptPhase::PostResponse => &folder.test_script,
        };
        if !source.trim().is_empty() {
            sources.push(ScriptSource {
                label: format!("Folder · {}", folder.name),
                source: source.clone(),
            });
        }
    }

    let source = match phase {
        ScriptPhase::PreRequest => &request.pre_request_script,
        ScriptPhase::PostResponse => &request.test_script,
    };
    if !source.trim().is_empty() {
        sources.push(ScriptSource {
            label: format!("Request · {}", request.name),
            source: source.clone(),
        });
    }

    sources
}

/// Tests, console output, error and run-control requests accumulated across script phases.
#[derive(Default)]
pub(crate) struct ScriptDiagnostics {
    pub(crate) tests: Vec<ScriptTestResult>,
    pub(crate) console: Vec<ConsoleEntry>,
    pub(crate) error: Option<ScriptError>,
    pub(crate) next_request: Option<Option<String>>,
}

impl ScriptDiagnostics {
    /// Moves the phase's diagnostics out of `output`, leaving its request/variables intact.
    pub(crate) fn merge(&mut self, output: &mut ScriptOutput) {
        self.tests.append(&mut output.tests);
        self.console.append(&mut output.console);
        if output.error.is_some() {
            self.error = output.error.take();
        }
        if output.next_request.is_some() {
            self.next_request = output.next_request.take();
        }
    }
}

/// Runs one script phase on a worker thread; [`AppError::Cancelled`] when `cancel` fires first.
///
/// The OS thread cannot be killed: a cancelled script finishes in the background and its output
/// (including variable changes) is discarded.
pub(crate) async fn run_script_phase(
    input: ScriptInput,
    cancel: Option<&CancelToken>,
) -> Result<ScriptOutput, AppError> {
    until_cancelled(cancel, spawn_blocking(move || run_scripts(input)))
        .await
        .ok_or_else(AppError::cancelled)?
        .map_err(|error| AppError::internal(format!("script worker failed: {error}")))
}

/// Builds the script runtime input for `phase` from the request, optional response and context.
#[allow(clippy::too_many_arguments)]
pub(crate) fn script_input(
    phase: ScriptPhase,
    scripts: Vec<ScriptSource>,
    request: &Request,
    response: Option<ExecutionResult>,
    context: &ExecutionContext,
    meta: &ExecutionMeta,
    cancel: Option<&CancelToken>,
    limits: ScriptLimits,
) -> ScriptInput {
    ScriptInput {
        phase,
        scripts,
        request: request.clone(),
        response,
        // The AI run pipeline sets this on its own input after calling `script_input`.
        ai: None,
        environment: context.environment.clone(),
        collection_variables: context
            .collection
            .as_ref()
            .map(|collection| collection.variables.clone())
            .unwrap_or_default(),
        globals: context.globals.clone(),
        local: context.local.clone(),
        cancel: cancel.cloned(),
        iteration_data: meta.data.clone(),
        cookie_jar: context.cookie_jar.clone(),
        info: ScriptInfo {
            request_name: request.name.clone(),
            request_id: request.id.as_str().to_owned(),
            iteration: meta.iteration,
            iteration_count: meta.iteration_count,
            event_name: phase.event_name().to_owned(),
        },
        limits,
        confirmed: meta.confirmed == Some(true),
    }
}

/// Copies the variable scopes a script left behind onto `context`.
pub(crate) fn apply_script_variables(context: &mut ExecutionContext, output: &ScriptOutput) {
    if let Some(collection) = context.collection.as_mut() {
        collection.variables = output.collection_variables.clone();
    }
    context.globals = output.globals.clone();
    context.local = output.local.clone();
    // `pm.cookies.jar()` writes reach the main request of this send.
    context.cookie_jar = output.cookie_jar.clone();
    if output.environment.is_some() {
        context.environment = output.environment.clone();
    }
}

/// Persists the cookies `pm.cookies.jar()` set or removed when `enabled` (the send stores
/// cookies and the request uses the jar); a failure is logged, as for response cookies.
pub(crate) async fn persist_script_cookies(
    state: &AppState,
    workspace_id: &WorkspaceId,
    output: &ScriptOutput,
    enabled: bool,
) {
    if !enabled || (output.cookie_writes.is_empty() && output.cookie_deletes.is_empty()) {
        return;
    }
    let (workspace, writes, deletes) = (
        workspace_id.clone(),
        output.cookie_writes.clone(),
        output.cookie_deletes.clone(),
    );
    let saved = blocking(state, move |state| {
        let store = state.store();
        if !writes.is_empty() {
            store
                .save_cookie_jar_entries(workspace.as_str(), &writes)
                .map_err(command_error)?;
        }
        if !deletes.is_empty() {
            store
                .delete_cookie_jar_entries(workspace.as_str(), &deletes)
                .map_err(command_error)?;
        }
        Ok(())
    })
    .await;
    if let Err(error) = saved {
        crate::logging::log_line(
            crate::logging::LogLevel::Error,
            "execution.cookies",
            &format!("failed to persist script cookies: {error}"),
        );
    }
}

/// Applies a script's variable mutations to `current`; fails if the stored scope changed meanwhile.
fn merge_script_variables(
    before: &[EnvironmentVariable],
    after: &[EnvironmentVariable],
    current: &mut Vec<EnvironmentVariable>,
    mutations: &ScopeMutations,
    scope: &str,
) -> Result<bool, AppError> {
    if mutations.cleared {
        if current.as_slice() != before {
            return Err(AppError::conflict(format!(
                "{scope} variables changed concurrently"
            )));
        }
        if current.as_slice() != after {
            *current = after.to_vec();
            return Ok(true);
        }
        return Ok(false);
    }
    let mut changed = false;
    for key in &mutations.keys {
        let old: Vec<_> = before.iter().filter(|v| v.key == key.as_str()).collect();
        let new: Vec<_> = after.iter().filter(|v| v.key == key.as_str()).collect();
        let saved: Vec<_> = current.iter().filter(|v| v.key == key.as_str()).collect();
        if saved != old {
            return Err(AppError::conflict(format!(
                "{scope} variable {key} changed concurrently"
            )));
        }
        if saved != new {
            let position = current
                .iter()
                .position(|v| v.key == key.as_str())
                .unwrap_or(current.len());
            current.retain(|v| v.key != key.as_str());
            current.splice(position..position, new.into_iter().cloned());
            changed = true;
        }
    }
    Ok(changed)
}

/// Persists collection, environment and global variable changes made by a script.
pub(crate) fn persist_script_state(
    state: &AppState,
    request: &Request,
    before: &ExecutionContext,
    after: &ExecutionContext,
    mutations: &ScriptMutations,
) -> Result<(), AppError> {
    persist_variables(
        state,
        request.workspace_id.as_str(),
        before,
        after,
        mutations,
    )
}

/// Whether `mutations` touched any persistent variable scope.
fn changes_variables(mutations: &ScriptMutations) -> bool {
    [
        &mutations.collection,
        &mutations.environment,
        &mutations.globals,
    ]
    .iter()
    .any(|scope| scope.cleared || !scope.keys.is_empty())
}

/// [`persist_variables`] on a blocking worker; nothing is read or written when the script
/// changed no variable. `before` and `after` come from [`ExecutionContext::variable_scopes`].
pub(super) async fn persist_variables_blocking(
    state: &AppState,
    workspace_id: &WorkspaceId,
    before: ExecutionContext,
    after: &ExecutionContext,
    mutations: &ScriptMutations,
) -> Result<(), AppError> {
    if !changes_variables(mutations) {
        return Ok(());
    }
    let (workspace_id, after, mutations) = (
        workspace_id.clone(),
        after.variable_scopes(),
        mutations.clone(),
    );
    blocking(state, move |state| {
        persist_variables(state, workspace_id.as_str(), &before, &after, &mutations)
    })
    .await
}

/// [`persist_script_state`] for the variables of `workspace_id`.
fn persist_variables(
    state: &AppState,
    workspace_id: &str,
    before: &ExecutionContext,
    after: &ExecutionContext,
    mutations: &ScriptMutations,
) -> Result<(), AppError> {
    let store = state.store();
    let mut collection_to_save = None;
    if let (Some(old), Some(new)) = (&before.collection, &after.collection)
        && (mutations.collection.cleared || !mutations.collection.keys.is_empty())
    {
        let mut current = store
            .get_collection(&old.workspace_id, &old.id)
            .map_err(command_error)?
            .ok_or_else(|| {
                AppError::not_found(format!(
                    "collection {} was deleted during script execution",
                    old.id.as_str()
                ))
            })?;
        if merge_script_variables(
            &old.variables,
            &new.variables,
            &mut current.variables,
            &mutations.collection,
            "collection",
        )? {
            collection_to_save = Some(current);
        }
    }
    let mut environment_to_save = None;
    if let (Some(old), Some(new)) = (&before.environment, &after.environment)
        && (mutations.environment.cleared || !mutations.environment.keys.is_empty())
    {
        let mut current = store
            .get_environment(&old.workspace_id, &old.id)
            .map_err(command_error)?
            .ok_or_else(|| {
                AppError::not_found(format!(
                    "environment {} was deleted during script execution",
                    old.id.as_str()
                ))
            })?;
        if merge_script_variables(
            &old.variables,
            &new.variables,
            &mut current.variables,
            &mutations.environment,
            "environment",
        )? {
            environment_to_save = Some(current);
        }
    }
    let mut globals_to_save = None;
    if mutations.globals.cleared || !mutations.globals.keys.is_empty() {
        let mut current = store
            .list_global_variables(workspace_id)
            .map_err(command_error)?;
        if merge_script_variables(
            &before.globals,
            &after.globals,
            &mut current,
            &mutations.globals,
            "global",
        )? {
            globals_to_save = Some(current);
        }
    }
    store
        .save_script_state(
            workspace_id,
            collection_to_save.as_ref(),
            environment_to_save.as_ref(),
            globals_to_save.as_deref(),
        )
        .map_err(command_error)
}
