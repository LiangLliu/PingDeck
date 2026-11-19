//! Request execution, draft execution, script orchestration, collection runs and
//! request-body file resolution.
//!
//! Single responsibility: turn a saved or draft request into an executed
//! [`ExecutionResult`] through one pipeline shared by Send, the collection runner, tools and
//! flows, persisting script variable changes (and, for [`Persistence::Record`], history) exactly
//! once per attempt. No UI, IPC or dialog code lives here.

mod context;
mod pipeline;
mod runner;
mod scripts;

pub(crate) use context::execution_context;
pub use context::{ExecutionContext, ExecutionMeta, SentCredentials, execution_context_for};
pub use pipeline::response_cookie_entries;
#[cfg(test)]
pub(crate) use pipeline::store_response_cookies;
pub use pipeline::{Persistence, execute_request_pipeline, execute_request_pipeline_traced};
pub(crate) use pipeline::{
    persist_failure, run_pipeline, save_execution_history_if_workspace_exists,
};
pub use runner::{collection_run_get_impl, run_collection_impl};
pub(crate) use scripts::{
    ScriptDiagnostics, apply_script_variables, persist_script_state, run_script_phase,
    script_input, script_sources,
};

/// Whether sending `request` in `context` runs any pre-request script (collection, folders or
/// its own), which may send requests of its own (`pm.sendRequest`).
#[must_use]
pub fn runs_pre_request_scripts(context: &ExecutionContext, request: &Request) -> bool {
    !script_sources(context, request, crate::scripts::ScriptPhase::PreRequest).is_empty()
}

use std::{fs, path::PathBuf};

use super::prod_guard::guard_request;
use super::secret_hosts::{carried_secret, declared_hosts, host_listed, host_of_url};
use super::{
    AppState, ExecuteDraftRequestInput, ExecuteRequestInput, command_error, request_from_input,
};
use crate::error::{AppError, ErrorCode};
use crate::executor::ExecutionResult;
use crate::platform::normalize_display_path;
use crate::proxy_url;
use crate::workspace::models::bounded_body_preview;
use crate::workspace::variables::resolve_template_lenient;
use crate::workspace::{
    HistoryOrigin, Request, RequestBodyFilePickResult, RequestBodyFileValidationResult, RequestId,
    VariableResolutionError, WorkspaceId, WorkspaceStore, effective_auth, resolve_template,
};
use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};
use context::blocking;

/// Executes a saved request and records history; `cancel_token` lets `execution_cancel_impl`
/// abort it. Before the response headers that returns [`AppError::Cancelled`] without a history
/// row; after them the partial response is returned and recorded (Stop on a stream).
pub async fn request_execute_impl(
    state: &AppState,
    input: ExecuteRequestInput,
    cancel_token: Option<String>,
) -> Result<ExecutionResult, AppError> {
    let guard = state.cancellations.register(cancel_token.as_deref())?;
    let workspace_id = WorkspaceId::new(&input.workspace_id).map_err(command_error)?;
    let request_id = RequestId::new(&input.request_id).map_err(command_error)?;

    let environment_id = input.environment_id;
    let (request, context) = blocking(state, move |state| {
        let store = state.store();
        let request = store
            .get_request(&workspace_id, &request_id)
            .map_err(command_error)?
            .ok_or_else(|| {
                AppError::not_found(format!("request {} not found", request_id.as_str()))
            })?;
        let context = execution_context(&store, &request, environment_id.as_deref())?;

        Ok((request, context))
    })
    .await?;
    let clears_marker = confirm_agent_request(state, &request, &context, input.confirmed).await?;
    guard_request(
        context.environment.as_ref(),
        request.method,
        input.confirmed,
    )?;

    let workspace_id = request.workspace_id.as_str().to_owned();
    let request_id = request.id.as_str().to_owned();
    let mut result = execute_request_pipeline(
        state,
        request,
        context,
        ExecutionMeta::confirmed(input.confirmed).interactive(),
        Some(guard.token()),
        Persistence::Record(HistoryOrigin::request()),
    )
    .await?;
    if clears_marker {
        clear_agent_marker(state, workspace_id, request_id).await;
    }
    externalize_body(state, &mut result);
    Ok(result)
}

/// Executes an unsaved draft request; cancellation behaves like [`request_execute_impl`].
///
/// The UI sends through here, so a draft clears the agent marker of the stored request with the
/// same id, but only when the stored request targets the host the draft went to: a draft edited
/// to another host leaves the agent's target unreviewed.
pub async fn request_execute_draft_impl(
    state: &AppState,
    input: ExecuteDraftRequestInput,
    cancel_token: Option<String>,
) -> Result<ExecutionResult, AppError> {
    execute_draft(state, input, cancel_token, None).await
}

/// "Send and save to file": executes a draft like [`request_execute_draft_impl`] and streams the
/// whole response body to `path` as it arrives, with no size cap; the result captures what the
/// cap allows and names the file. `path` comes from the native save dialog the shell opened.
pub async fn request_execute_draft_to_file_impl(
    state: &AppState,
    input: ExecuteDraftRequestInput,
    cancel_token: Option<String>,
    path: PathBuf,
) -> Result<ExecutionResult, AppError> {
    execute_draft(state, input, cancel_token, Some(path)).await
}

async fn execute_draft(
    state: &AppState,
    input: ExecuteDraftRequestInput,
    cancel_token: Option<String>,
    save_body_to: Option<PathBuf>,
) -> Result<ExecutionResult, AppError> {
    let guard = state.cancellations.register(cancel_token.as_deref())?;
    let mut request = request_from_input(input.request)?;
    let persistence = if input.save_to_history == Some(false) {
        Persistence::Unrecorded(HistoryOrigin::request())
    } else {
        Persistence::Record(HistoryOrigin::request())
    };

    let environment_id = input.environment_id;
    let (request, context) = blocking(state, move |state| {
        let store = state.store();
        hydrate_draft_proxy(&store, &mut request)?;
        let context = execution_context(&store, &request, environment_id.as_deref())?;
        Ok((request, context))
    })
    .await?;
    let clears_marker = confirm_agent_request(state, &request, &context, input.confirmed).await?;
    guard_request(
        context.environment.as_ref(),
        request.method,
        input.confirmed,
    )?;

    let ids = (
        request.workspace_id.as_str().to_owned(),
        request.id.as_str().to_owned(),
    );
    let meta = ExecutionMeta::confirmed(input.confirmed).interactive();
    let meta = match save_body_to {
        Some(path) => meta.saving_body_to(path),
        None => meta,
    };
    let mut result = execute_request_pipeline(
        state,
        request,
        context,
        meta,
        Some(guard.token()),
        persistence,
    )
    .await?;
    if clears_marker {
        clear_agent_marker(state, ids.0, ids.1).await;
    }
    externalize_body(state, &mut result);
    Ok(result)
}

/// How a request stands against the agent rules when the user (or a run) sends it.
enum AgentReview {
    /// The agent never touched it, or the user has sent it since.
    Untouched,
    /// The agent touched it and its target host is declared without it.
    Declared,
    /// The agent touched it, its target host is not declared, and it carries no credential.
    Undeclared,
    /// The agent touched it and it carries `carried` to the undeclared `host` (`None`: the host
    /// cannot be determined).
    Carries {
        carried: String,
        host: Option<String>,
    },
}

/// The one policy for a request the agent wrote or changed, shared by Send, collection runs and
/// flows: what it carries (with its inherited auth expanded) and whether the host it goes to is
/// declared without it. Blocking: the store.
fn review_agent_request(
    state: &AppState,
    request: &Request,
    context: &ExecutionContext,
    untrusted: &std::collections::HashSet<String>,
) -> Result<AgentReview, AppError> {
    if !untrusted.contains(request.id.as_str()) {
        return Ok(AgentReview::Untouched);
    }
    let scopes = context.scopes();
    let mut sent = request.clone();
    sent.auth = effective_auth(request, context.collection.as_ref(), &context.folders);
    let target = host_of_url(&resolve_template_lenient(&request.url, &scopes));
    let declared: Vec<String> = declared_hosts(
        state,
        &request.workspace_id,
        context.environment.as_ref(),
        untrusted,
    )?
    .into_iter()
    .map(|entry| entry.host)
    .collect();
    if target
        .as_deref()
        .is_some_and(|target| host_listed(&declared, target))
    {
        return Ok(AgentReview::Declared);
    }
    Ok(match carried_secret(&sent, &scopes) {
        Some(carried) => AgentReview::Carries {
            carried,
            host: target,
        },
        None => AgentReview::Undeclared,
    })
}

/// A request the agent wrote or changed (not sent from the UI since) that carries a secret
/// or credential to a host its environment does not declare asks once before the UI sends it:
/// [`AppError::ConfirmationRequired`] coded `agent_touched_secret` with the host, or
/// `agent_touched_secret_guarded` when the environment also guards the method, since the one
/// `confirmed` flag then answers the production guard too.
///
/// The result says whether the send clears the agent marker (after which the request's host
/// counts as declared): only when the target host was already declared without it, or when the
/// user confirmed the card that named the host, and in both cases only when the stored request
/// targets the host this send goes to. A send that carries nothing to an undeclared host goes out
/// without a question and leaves the marker, so a request the agent wrote cannot make its host a
/// declared one through a click on Send.
async fn confirm_agent_request(
    state: &AppState,
    request: &Request,
    context: &ExecutionContext,
    confirmed: Option<bool>,
) -> Result<bool, AppError> {
    let (request, context) = (request.clone(), context.clone());
    blocking(state, move |state| {
        let untrusted = state
            .store()
            .agent_touched_request_ids(request.workspace_id.as_str())
            .map_err(command_error)?;
        let clears = || -> Result<bool, AppError> {
            let stored = state
                .store()
                .get_request(&request.workspace_id, &request.id)
                .map_err(command_error)?;
            Ok(stored.is_some_and(|stored| host_of_url(&stored.url) == host_of_url(&request.url)))
        };
        match review_agent_request(state, &request, &context, &untrusted)? {
            AgentReview::Untouched | AgentReview::Undeclared => Ok(false),
            AgentReview::Declared => clears(),
            AgentReview::Carries { .. } if confirmed == Some(true) => clears(),
            AgentReview::Carries { carried, host } => {
                let host = host.unwrap_or_else(|| "an undetermined host".to_owned());
                let guarded = context
                    .environment
                    .as_ref()
                    .filter(|environment| environment.requires_confirmation(request.method));
                let (code, also) = match guarded {
                    Some(environment) => (
                        ErrorCode::AgentTouchedSecretGuarded,
                        format!(" {} is guarded too.", environment.name),
                    ),
                    None => (ErrorCode::AgentTouchedSecret, String::new()),
                };
                Err(AppError::confirmation_required(format!(
                    "The agent wrote this request and it has not been sent from PingDeck yet: \
                     {carried} would go to {host}, which this environment does not declare.{also}"
                ))
                .coded(code, host))
            }
        }
    })
    .await
}

/// Most requests a refused run names.
const UNREVIEWED_NAMED: usize = 5;

/// Refuses a collection run or a flow, before anything is sent, when one of `requests` was
/// written or changed by the agent and carries a secret or credential (inherited auth included) to
/// a host `environment_id` does not declare. A run has no per-request card and its one
/// `confirmed` flag answers the production guard, so it cannot confirm such a target: the user
/// reviews each one with a single Send from the Requests module (which shows the host), then
/// runs again.
///
/// # Errors
/// [`AppError::Validation`] naming the requests and hosts.
pub(crate) async fn refuse_unreviewed_agent_requests(
    state: &AppState,
    requests: Vec<Request>,
    environment_id: Option<String>,
) -> Result<(), AppError> {
    let Some(workspace_id) = requests.first().map(|request| request.workspace_id.clone()) else {
        return Ok(());
    };
    blocking(state, move |state| {
        let untrusted = state
            .store()
            .agent_touched_request_ids(workspace_id.as_str())
            .map_err(command_error)?;
        let touched: Vec<Request> = requests
            .into_iter()
            .filter(|request| untrusted.contains(request.id.as_str()))
            .collect();
        if touched.is_empty() {
            return Ok(());
        }
        let contexts = {
            let store = state.store();
            touched
                .iter()
                .map(|request| execution_context(&store, request, environment_id.as_deref()))
                .collect::<Result<Vec<_>, _>>()?
        };
        let mut refused: Vec<String> = Vec::new();
        for (request, context) in touched.iter().zip(&contexts) {
            if let AgentReview::Carries { carried, host } =
                review_agent_request(state, request, context, &untrusted)?
            {
                refused.push(format!(
                    "\"{}\" ({carried} to {})",
                    request.name,
                    host.as_deref().unwrap_or("an undetermined host")
                ));
            }
        }
        if refused.is_empty() {
            return Ok(());
        }
        let more = refused.len().saturating_sub(UNREVIEWED_NAMED);
        refused.truncate(UNREVIEWED_NAMED);
        Err(AppError::validation(format!(
            "The agent wrote or changed {} that would send a credential to a host this \
             environment does not declare: {}{}. Send each once from the Requests module to review \
             its target, then run again.",
            if refused.len() + more == 1 {
                "a request".to_owned()
            } else {
                format!("{} requests", refused.len() + more)
            },
            refused.join(", "),
            if more > 0 {
                format!(", and {more} more")
            } else {
                String::new()
            }
        )))
    })
    .await
}

/// The same rule for one flow step as it is about to be sent: the request with the step's
/// overrides applied, resolved in the step's own context (the flow variables and what earlier
/// steps extracted), so a target the pre-run review could not see is held to the rule too.
///
/// # Errors
/// [`AppError::Validation`] naming the request and host; [`AppError::Database`] when the store
/// cannot be read.
pub(crate) fn refuse_unreviewed_agent_step(
    state: &AppState,
    request: &Request,
    context: &ExecutionContext,
) -> Result<(), AppError> {
    let untrusted = state
        .store()
        .agent_touched_request_ids(request.workspace_id.as_str())
        .map_err(command_error)?;
    match review_agent_request(state, request, context, &untrusted)? {
        AgentReview::Carries { carried, host } => Err(AppError::validation(format!(
            "The agent wrote or changed \"{}\", which would send {carried} to {}, a host this \
             environment does not declare. Send it once from the Requests module to review its \
             target, then run again.",
            request.name,
            host.as_deref().unwrap_or("an undetermined host")
        ))),
        _ => Ok(()),
    }
}

/// Puts the stored proxy password back into a draft whose proxy URL carries the mask: the editor
/// echoes the masked URL of the saved request (`user:****@host`), and sending it as is would
/// authenticate with the literal mask. The same holds for a history snapshot re-sent as a
/// draft.
fn hydrate_draft_proxy(store: &WorkspaceStore, request: &mut Request) -> Result<(), AppError> {
    let Some(url) = request.network_options.proxy_url.as_deref() else {
        return Ok(());
    };
    if !proxy_url::split(url).is_masked {
        return Ok(());
    }
    let reference = proxy_url::request_secret(request.id.as_str());
    if let Some(password) = store.secret_get(&reference).map_err(command_error)? {
        request.network_options.proxy_url = Some(proxy_url::restore(url, &password));
    }
    Ok(())
}

/// A request the user sent from the UI is no longer "touched by the agent": its host counts as
/// declared again. Storage errors are logged, never fail the send.
async fn clear_agent_marker(state: &AppState, workspace_id: String, request_id: String) {
    let cleared = blocking(state, {
        let request_id = request_id.clone();
        move |state| {
            state
                .store()
                .clear_request_agent_touched(&workspace_id, &request_id)
                .map_err(command_error)
        }
    })
    .await;
    if let Err(error) = cleared {
        crate::logging::log_line(
            crate::logging::LogLevel::Warn,
            "execution.agent-marker",
            &format!("failed to clear the agent marker of {request_id}: {error}"),
        );
    }
}

/// Keeps a response body above the inline threshold (`data.inline_threshold_bytes`, 1 MiB by
/// default) out of the result a shell serializes: the full bytes go to the body store and the
/// result keeps a text preview (`data.preview_bytes`, 64 KiB by default), the body size
/// and a `body_handle` for [`response_body_fetch_impl`]. Smaller bodies stay inline. Scripts,
/// assertions and history have already seen the full body by the time this runs.
pub(crate) fn externalize_body(state: &AppState, result: &mut ExecutionResult) {
    let raw = result.raw_body.take();
    let data = &state.settings().data;
    if result.body_size_bytes as u64 <= data.inline_threshold_bytes {
        return;
    }
    let raw = raw.unwrap_or_else(|| match result.body_base64.as_deref() {
        Some(encoded) => BASE64_STANDARD
            .decode(encoded)
            .map_or_else(|_| result.body_text.clone().into_bytes().into(), Into::into),
        None => result.body_text.clone().into_bytes().into(),
    });
    result.body_text = bounded_body_preview(
        &result.body_text,
        usize::try_from(data.preview_bytes).unwrap_or(usize::MAX),
    )
    .0;
    result.body_base64 = None;
    let content_type = result
        .headers
        .iter()
        .find(|header| header.key.eq_ignore_ascii_case("content-type"))
        .map(|header| header.value.clone());
    result.body_handle = Some(state.bodies.put_with_content_type(raw, content_type));
}

/// The full bytes behind a `body_handle` returned by a use-case.
///
/// Fails with `NotFound` when the body was evicted (the store keeps the 8 most recent bodies,
/// 64 MiB in total) or the handle came from an earlier session.
pub fn response_body_fetch_impl(state: &AppState, handle: &str) -> Result<bytes::Bytes, AppError> {
    state.bodies.get(handle)
}

/// Cancels the operation registered under `cancel_token`; `false` when none is running.
pub fn execution_cancel_impl(state: &AppState, cancel_token: String) -> bool {
    state.cancellations.cancel(&cancel_token)
}

/// Turns a path chosen in the native picker into the display path, or "canceled" when none.
pub fn request_body_pick_file_from_path_impl(
    selected_path: Option<PathBuf>,
) -> Result<RequestBodyFilePickResult, AppError> {
    let Some(path) = selected_path else {
        return Ok(RequestBodyFilePickResult::canceled());
    };

    Ok(RequestBodyFilePickResult::selected(normalize_display_path(
        &path,
    )))
}

/// Checks whether a literal request-body file path exists and is a readable file.
pub(crate) fn request_body_validate_file_path_impl(
    path: String,
) -> Result<RequestBodyFileValidationResult, AppError> {
    let trimmed_path = path.trim();
    if trimmed_path.is_empty() {
        return Ok(RequestBodyFileValidationResult::empty());
    }

    let file_path = PathBuf::from(trimmed_path);
    let metadata = match fs::metadata(&file_path) {
        Ok(metadata) => metadata,
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => {
            return Ok(RequestBodyFileValidationResult::missing());
        }
        Err(error) => {
            return Ok(RequestBodyFileValidationResult::unreadable(
                error.to_string(),
            ));
        }
    };

    if !metadata.is_file() {
        return Ok(RequestBodyFileValidationResult::not_file());
    }
    // The same limit the send applies, so a file shown ready also sends.
    if metadata.len() > crate::executor::MAX_REQUEST_BODY_BYTES {
        return Ok(RequestBodyFileValidationResult::too_large(
            crate::executor::MAX_REQUEST_BODY_BYTES,
        ));
    }

    match fs::File::open(&file_path) {
        Ok(_) => Ok(RequestBodyFileValidationResult::ready()),
        Err(error) => Ok(RequestBodyFileValidationResult::unreadable(
            error.to_string(),
        )),
    }
}

/// Like [`request_body_validate_file_path_impl`] after resolving `{{variables}}` in `path`.
pub fn request_body_validate_file_path_with_context_impl(
    state: &AppState,
    workspace_id: String,
    request_id: String,
    environment_id: Option<String>,
    path: String,
) -> Result<RequestBodyFileValidationResult, AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let request_id = RequestId::new(&request_id).map_err(command_error)?;
    let scopes = {
        let store = state.store();
        let request = store
            .get_request(&workspace_id, &request_id)
            .map_err(command_error)?
            .ok_or_else(|| {
                AppError::not_found(format!("request {} not found", request_id.as_str()))
            })?;
        execution_context(&store, &request, environment_id.as_deref())?.scopes()
    };
    let resolved_path = match resolve_template(&path, &scopes) {
        Ok(path) => path,
        Err(VariableResolutionError::UnresolvedVariables { keys }) => {
            return Ok(RequestBodyFileValidationResult::unresolved_variable(
                keys.into_iter()
                    .next()
                    .unwrap_or_else(|| "unknown".to_owned()),
            ));
        }
    };

    request_body_validate_file_path_impl(resolved_path)
}
