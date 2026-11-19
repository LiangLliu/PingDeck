use std::time::Instant;

use super::super::prod_guard::{guard_method_change, guard_request};
use super::super::secret_hosts::{
    carried_secret, ensure_secret_hosts_declared, host_of_url, sent_credential_values,
};
use super::super::{
    AppState, clear_stale_history_refs, command_error, cookie_jar_timestamp, execution_history_id,
    execution_history_timestamp,
};
use super::context::{ExecutionContext, ExecutionMeta, SentCredentials, blocking};
use super::scripts::{
    ScriptDiagnostics, apply_script_variables, persist_script_cookies, persist_variables_blocking,
    run_script_phase, script_input, script_sources,
};
use crate::cancel::CancelToken;
use crate::error::AppError;
use crate::executor::{
    AuxiliaryNetwork, CertificatePassphrases, CookieJarOutcome, ExecutionError, ExecutionResult,
    ExecutorLimits, ExecutorOptions, OAuthTokenSource, SentRequest, StoredOAuthTokens, TlsMaterial,
    execute_request_with_scopes,
};
use crate::logging::{LogLevel, log_line};
use crate::scripts::{ScriptLimits, ScriptPhase};
use crate::workspace::assertions::evaluate_assertions;
use crate::workspace::items::ExecutedRequestSummary;
use crate::workspace::models::{
    HISTORY_BODY_PREVIEW_LIMIT_BYTES, HistoryCheckSummary, NetworkOptions, bounded_body_preview,
};
use crate::workspace::variables::resolve_template_lenient;
use crate::workspace::{
    CookieJarEntry, Environment, ExecutionHistoryEntry, HistoryOrigin, HistorySource, KeyValue,
    Request, RequestId, VariableScopes, WorkspaceId, effective_auth,
};
use pingdeck_model::script_result::{ConsoleEntry, ConsoleLevel, ScriptError};

/// A script-variable persistence failure reported with `message`, keeping the failure's category.
pub(crate) fn persist_failure(error: &AppError, message: String) -> AppError {
    match error {
        AppError::Conflict { .. } => AppError::conflict(message),
        AppError::NotFound { .. } => AppError::not_found(message),
        _ => AppError::database(message),
    }
}

/// The request as history keeps it: the definition the pipeline received, before inherited
/// settings, scripts and inherited auth changed it, without the editor's revision. "Run again"
/// then runs the request's own scripts once, on the same definition, and an inherited credential
/// stays a reference (`Inherit`, resolved again when the entry is reopened) instead of a copied
/// value in every row.
fn history_definition(request: &Request) -> Request {
    let mut definition = request.clone();
    definition.revision = None;
    definition
}

/// The history row of a failed execution, `None` (logged) when it cannot be built. `request` is
/// what was about to be sent (name, method, URL); `definition` the snapshot kept for reopening.
#[allow(clippy::too_many_arguments)]
fn failed_history_entry(
    request: &Request,
    definition: Option<&Request>,
    context: &ExecutionContext,
    error_kind: &str,
    error_message: &str,
    elapsed_ms: u128,
    attempt_count: u32,
    connect_phase: Option<bool>,
    origin: &HistoryOrigin,
) -> Option<ExecutionHistoryEntry> {
    match ExecutionHistoryEntry::from_failure(
        execution_history_id(&request.id),
        request,
        context
            .environment
            .as_ref()
            .map(|environment| environment.id.as_str()),
        error_kind,
        error_message,
        elapsed_ms,
        attempt_count,
        execution_history_timestamp(),
    ) {
        Ok(mut entry) => {
            entry.connect_phase = connect_phase;
            if let Some(definition) = definition {
                entry = entry.with_request_snapshot(definition);
            }
            Some(
                entry.with_origin(origin).with_environment_name(
                    context
                        .environment
                        .as_ref()
                        .map_or("", |environment| environment.name.as_str()),
                ),
            )
        }
        Err(error) => {
            log_line(
                LogLevel::Error,
                "execution.history",
                &format!("failed to build failed execution history: {error}"),
            );
            None
        }
    }
}

/// Writes the history rows of failed executions of one pipeline run, remembering the last row's id
/// in `slot` so a caller that reports the failure can link to it.
struct FailureRecorder<'a> {
    state: &'a AppState,
    persistence: &'a Persistence,
    /// The snapshot history keeps (see [`history_definition`]).
    definition: Option<&'a Request>,
    slot: &'a std::sync::Mutex<Option<String>>,
    started_at: Instant,
}

impl FailureRecorder<'_> {
    /// Records a failed execution of `request` unless `persistence` writes no history; logs and
    /// swallows storage errors (no id, as when the workspace is gone). True when the row should
    /// have been written and was not: the caller marks the error
    /// ([`AppError::with_history_unsaved`]) so the failure card says it is not in History.
    async fn record(
        &self,
        request: &Request,
        context: &ExecutionContext,
        error_kind: &str,
        message: &str,
        attempt_count: u32,
        connect_phase: Option<bool>,
    ) -> bool {
        if !self.persistence.records_history() {
            return false;
        }
        let Some(mut entry) = failed_history_entry(
            request,
            self.definition,
            context,
            error_kind,
            message,
            self.started_at.elapsed().as_millis(),
            attempt_count,
            connect_phase,
            self.persistence.origin(),
        ) else {
            return true;
        };
        let history_id = entry.id.as_str().to_owned();
        let saved = blocking(self.state, move |state| {
            save_execution_history_if_workspace_exists(state, &mut entry)
        })
        .await;
        let (saved, unsaved) = match saved {
            Ok(saved) => (saved.then_some(history_id), false),
            Err(error) => {
                log_line(
                    LogLevel::Error,
                    "execution.history",
                    &format!("failed to save failed execution history: {error}"),
                );
                (None, true)
            }
        };
        *self
            .slot
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner) = saved;
        unsaved
    }
}

/// Total time limit of a collection-run or flow request that sets none (`docs/decisions.md` §50).
pub(crate) const UNATTENDED_TOTAL_TIMEOUT_MS: u64 = 5 * 60 * 1000;

/// How an execution is recorded: a history row tagged with where it came from (except
/// [`Persistence::Unrecorded`]); only some also persist response cookies.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum Persistence {
    /// Write the history row (tagged with its origin) and persist response cookies: Send and the
    /// collection runner. An agent origin never writes the workspace jar, whatever the variant.
    Record(HistoryOrigin),
    /// Write the history row but no cookies: the steps of a flow and the agent's `send_request`
    /// (which sends with its run's own jar).
    HistoryOnly(HistoryOrigin),
    /// Write neither a history row nor cookies (`send_request` with `save_to_history: false`);
    /// the origin still decides the agent's host rules.
    Unrecorded(HistoryOrigin),
}

impl Persistence {
    /// Where the execution comes from.
    pub(crate) fn origin(&self) -> &HistoryOrigin {
        match self {
            Self::Record(origin) | Self::HistoryOnly(origin) | Self::Unrecorded(origin) => origin,
        }
    }

    /// Whether a history row is written.
    pub(crate) fn records_history(&self) -> bool {
        !matches!(self, Self::Unrecorded(_))
    }

    /// Whether response cookies are persisted to the jar.
    fn stores_cookies(&self) -> bool {
        matches!(self, Self::Record(origin) if origin.source != HistorySource::Agent)
    }
}

/// The one execution pipeline behind Send, collection runs, the agent's tools and flows.
///
/// Runs collection, folder and request pre-request scripts, applies inherited auth, sends
/// `request`, runs the post-response scripts, evaluates assertions and persists script variable
/// changes. `persistence` decides whether a history row and response cookies are written.
///
/// `cancel` aborts any phase with [`AppError::Cancelled`]; a cancelled run writes no history row and
/// persists no script variable changes. The one exception is a send whose meta keeps partial
/// responses: a cancel after the response headers returns what arrived, records it and skips the
/// post-response scripts.
pub async fn execute_request_pipeline(
    state: &AppState,
    request: Request,
    context: ExecutionContext,
    meta: ExecutionMeta,
    cancel: Option<&CancelToken>,
    persistence: Persistence,
) -> Result<ExecutionResult, AppError> {
    execute_request_pipeline_traced(state, request, context, meta, cancel, persistence)
        .await
        .map(|(response, _)| response)
}

/// [`execute_request_pipeline`] that also returns the id of the history row it wrote (`None`
/// when the execution records no row or its workspace is gone).
pub async fn execute_request_pipeline_traced(
    state: &AppState,
    request: Request,
    mut context: ExecutionContext,
    meta: ExecutionMeta,
    cancel: Option<&CancelToken>,
    persistence: Persistence,
) -> Result<(ExecutionResult, Option<String>), AppError> {
    let failure_history_id = std::sync::Mutex::new(None);
    run_pipeline(
        state,
        request,
        &mut context,
        meta,
        cancel,
        persistence,
        &failure_history_id,
    )
    .await
}

/// The pipeline proper. `context` is left holding the variable scopes the scripts changed, so
/// the collection runner can carry it into its next step instead of reloading it. The id of the
/// history row of a failed execution is left in `failure_history_id`, so a caller that reports
/// the failure can link to it.
pub(crate) async fn run_pipeline(
    state: &AppState,
    mut request: Request,
    context: &mut ExecutionContext,
    meta: ExecutionMeta,
    cancel: Option<&CancelToken>,
    persistence: Persistence,
    failure_history_id: &std::sync::Mutex<Option<String>>,
) -> Result<(ExecutionResult, Option<String>), AppError> {
    let origin = persistence.origin();
    context.data.clone_from(&meta.data);
    if request.runs_as_ai() {
        // The agent's host rules cannot cover a provider target, whose address only the AI run
        // service knows; its custom-URL AI requests stay plain HTTP sends under those rules.
        if origin.source == HistorySource::Agent {
            if request.is_provider_target() {
                return Err(AppError::unsupported(
                    "This request uses an AI provider; the agent cannot send it",
                ));
            }
        } else {
            // An AI request always POSTs; a run confirmed the guard for its stored method only.
            guard_request(
                context.environment.as_ref(),
                request.guard_method(),
                meta.confirmed,
            )?;
            return crate::app::ai_run_service::run_in_pipeline(
                state,
                request,
                context,
                &meta,
                cancel,
                &persistence,
                failure_history_id,
            )
            .await;
        }
    }
    let settings = state.settings();
    // The send, its OAuth 2.0 token fetch and `pm.sendRequest` trust the same roots and present
    // the same client certificate.
    let tls = TlsMaterial {
        certificates: context.certificates.clone(),
        passphrases: CertificatePassphrases::new(std::sync::Arc::clone(state.secrets())),
    };
    let mut script_limits = ScriptLimits::from(&*settings);
    script_limits.network.tls = tls.clone();
    // Fields the request inherits take today's Settings value (Settings › Requests / Network).
    settings
        .network
        .apply_inherited(&mut request.network_options);
    // `0` would fire reqwest's total timeout at once (imported Postman requests mean "none" by
    // it): it runs with the default, as at save.
    if request.network_options.timeout_ms == 0 {
        request.network_options.timeout_ms = NetworkOptions::default().timeout_ms;
    }
    let definition = persistence
        .records_history()
        .then(|| history_definition(&request));
    let pre_scripts = script_sources(context, &request, ScriptPhase::PreRequest);
    let post_scripts = script_sources(context, &request, ScriptPhase::PostResponse);
    let mut diagnostics = ScriptDiagnostics::default();
    let failures = FailureRecorder {
        state,
        persistence: &persistence,
        definition: definition.as_ref(),
        slot: failure_history_id,
        started_at: Instant::now(),
    };

    // An agent send that the host rules refuse as written (inherited auth expanded) stops before
    // its pre-request scripts can have any effect (`pm.sendRequest`, variable writes); what the
    // scripts change is checked again below.
    if origin.source == HistorySource::Agent && !pre_scripts.is_empty() {
        let mut written = request.clone();
        written.auth = effective_auth(&request, context.collection.as_ref(), &context.folders);
        agent_send_policy(
            state,
            &written,
            context.environment.as_ref(),
            &context.scopes(),
        )
        .await?;
    }

    if !pre_scripts.is_empty() {
        let before = context.variable_scopes();
        let method_before = request.method;
        let input = script_input(
            ScriptPhase::PreRequest,
            pre_scripts,
            &request,
            None,
            context,
            &meta,
            cancel,
            script_limits.clone(),
        );
        let mut output = run_script_phase(input, cancel).await?;
        let refused = output.confirmation_required.take();
        apply_script_variables(context, &output);
        persist_script_cookies(
            state,
            &request.workspace_id,
            &output,
            persistence.stores_cookies() && request.network_options.send_cookies,
        )
        .await;
        let saved = persist_variables_blocking(
            state,
            &request.workspace_id,
            before,
            context,
            &output.mutations,
        )
        .await;
        diagnostics.merge(&mut output);
        request = output.request;
        // A `pm.sendRequest` the production guard refused stops the send like the main
        // request's own guard: nothing is sent, the caller confirms and resends.
        if let Some(message) = refused {
            return Err(AppError::confirmation_required(message));
        }
        if let Some(error) = diagnostics.error.take() {
            let message = format!(
                "pre-request script failed in {}: {}",
                error.source, error.message
            );
            let message = match saved {
                Ok(()) => message,
                Err(error) => {
                    format!("{message}; failed to persist pre-request variables: {error}")
                }
            };
            let unsaved = failures
                .record(&request, context, "ScriptError", &message, 1, None)
                .await;
            return Err(AppError::validation(message)
                .coded(crate::error::ErrorCode::ScriptFailed, error.source.clone())
                .with_history_unsaved(unsaved));
        }
        if let Err(error) = saved {
            let message = format!("failed to persist pre-request variables: {error}");
            let unsaved = failures
                .record(&request, context, "ScriptError", &message, 1, None)
                .await;
            return Err(persist_failure(&error, message).with_history_unsaved(unsaved));
        }
        // A script may widen the method (`pm.request.method`); the caller only confirmed what
        // it saw before the script ran. Nothing is sent when it is refused.
        guard_method_change(
            context.environment.as_ref(),
            method_before,
            request.method,
            meta.confirmed,
        )?;
    }

    // Auth is resolved after pre-request scripts so `pm.request` mutations and
    // variable updates can affect the request, but before it is sent. History keeps the
    // definition from before the scripts (`history_definition`), so an inherited credential is
    // not copied into the row.
    request.auth = effective_auth(&request, context.collection.as_ref(), &context.folders);
    // An HTTP-mode copy of a provider request still holds the text `{{provider key}}`: nothing
    // is sent until the user adds authentication.
    crate::workspace::ai_request::ensure_sendable(&request)?;
    let scopes = context.scopes();
    if let Some(sink) = &meta.sent_credentials {
        sink.extend(sent_credential_values(&request, &scopes));
    }
    // A request the agent built may carry a secret or other credential only to hosts the
    // environment declares, and every redirect hop it follows must stay within those
    // hosts; saved requests the agent touched and the user has not sent since declare
    // nothing. What the user sends from the UI is never restricted. Nothing is sent and no
    // history row is written when it is refused.
    let mut carries_secret = false;
    let (redirect_hosts, resolve_guard) = if origin.source == HistorySource::Agent {
        let (mut declared, strict) =
            agent_send_policy(state, &request, context.environment.as_ref(), &scopes).await?;
        carries_secret = carried_secret(&request, &scopes).is_some();
        // The request's own host counts too: a redirect that stays on it is followed (AI-apiwb-5).
        // Pinning still checks only the declared hosts.
        let own = host_of_url(&resolve_template_lenient(&request.url, &scopes));
        let pinned = std::sync::Arc::new(declared.clone());
        if let Some(own) = own.filter(|own| !declared.contains(own)) {
            declared.push(own);
        }
        let declared = std::sync::Arc::new(declared);
        // Connections of an agent send are pinned to addresses the SSRF guard validated (DNS
        // rebinding). A draft or an agent-touched request may reach loopback and
        // private hosts only when the environment declares them; a saved request the user owns
        // keeps local access (link-local and metadata are never reachable).
        let guard = crate::executor::ResolveGuard::new(strict.then_some(pinned));
        (Some(declared), Some(guard))
    } else {
        (None, None)
    };

    // Nobody watches a collection run or a flow step: a stream that keeps sending heartbeats
    // must not hold it forever, so a request without its own total limit gets one there.
    if matches!(origin.source, HistorySource::Runner | HistorySource::Flow)
        && request.network_options.total_timeout_ms.is_none()
    {
        request.network_options.total_timeout_ms = Some(UNATTENDED_TOTAL_TIMEOUT_MS);
    }
    let outcome = execute_request_with_scopes(
        &request,
        scopes,
        ExecutorOptions {
            cookie_jar: context.cookie_jar.clone(),
            variable_scopes: None,
            certificates: context.certificates.clone(),
            cancel: cancel.cloned(),
            limits: ExecutorLimits::from(&settings.data),
            auxiliary: AuxiliaryNetwork::from(&settings.network).with_tls(tls),
            redirect_hosts,
            resolve_guard,
            oauth_tokens: {
                let stored = crate::oauth::SecretTokens(std::sync::Arc::clone(state.secrets()));
                match &meta.sent_credentials {
                    Some(sink) => OAuthTokenSource::new(std::sync::Arc::new(ReportingTokens {
                        inner: stored,
                        sink: sink.clone(),
                    })),
                    None => OAuthTokenSource::new(std::sync::Arc::new(stored)),
                }
            },
            keep_partial_on_cancel: meta.keep_partial_on_cancel,
            save_body_to: meta.save_body_to.clone(),
        },
    )
    .await;

    let mut response = match outcome {
        Ok(response) => response,
        Err(error) if error.is_cancelled() => return Err(AppError::from(error)),
        Err(error) => {
            let kind = execution_error_kind(&error);
            let attempt_count = error.attempt_count();
            let error = match error {
                // The executor's text says the request carries a secret; say so only when it does.
                ExecutionError::RedirectHostNotDeclared { host } if !carries_secret => {
                    AppError::validation(format!(
                        "a redirect to {host} was refused: {host} is not a host this environment \
                         declares"
                    ))
                    .coded(crate::error::ErrorCode::RedirectRefused, host)
                }
                error => AppError::from(error),
            };
            let connect_phase = match &error {
                AppError::Network { connect_phase, .. } => *connect_phase,
                _ => None,
            };
            let unsaved = failures
                .record(
                    &request,
                    context,
                    kind,
                    error.message(),
                    attempt_count,
                    connect_phase,
                )
                .await;
            return Err(error.with_history_unsaved(unsaved));
        }
    };

    // A send the user stopped after the headers keeps its partial response; its cancel token has
    // fired, so the post-response scripts would only be cancelled.
    let stopped = response.body_incomplete == Some(crate::executor::BodyIncomplete::Cancelled);
    if !post_scripts.is_empty() && !stopped {
        let before = context.variable_scopes();
        let input = script_input(
            ScriptPhase::PostResponse,
            post_scripts,
            &request,
            Some(response),
            context,
            &meta,
            cancel,
            script_limits,
        );
        let mut output = run_script_phase(input, cancel).await?;
        response = output
            .response
            .take()
            .ok_or_else(|| AppError::internal("post-response script runtime lost the response"))?;
        apply_script_variables(context, &output);
        persist_script_cookies(
            state,
            &request.workspace_id,
            &output,
            persistence.stores_cookies() && request.network_options.send_cookies,
        )
        .await;
        let saved = persist_variables_blocking(
            state,
            &request.workspace_id,
            before,
            context,
            &output.mutations,
        )
        .await;
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

    response.script_tests = diagnostics.tests;
    response.console = diagnostics.console;
    response.script_error = diagnostics.error;
    if let Some(next) = diagnostics.next_request {
        match next {
            Some(name) => response.next_request = Some(name),
            None => response.stop_run = true,
        }
    }
    response.assertion_results = evaluate_assertions(&request.assertions, &response);

    let (history_id, jar) = if persistence.records_history() {
        persist_successful_execution(
            state,
            &request,
            definition.as_ref(),
            context.environment.as_ref(),
            &response,
            origin,
            persistence.stores_cookies(),
        )
        .await
        .unwrap_or_else(|error| {
            log_line(
                LogLevel::Error,
                "execution.history",
                &format!("failed to persist successful execution: {error}"),
            );
            (None, CookieJarOutcome::NotStored)
        })
    } else {
        (None, CookieJarOutcome::NotStored)
    };
    response.cookie_report.jar = jar;
    response.history_saved = history_id.is_some();
    response.history_id.clone_from(&history_id);

    Ok((response, history_id))
}

/// The stored OAuth 2.0 tokens, reporting every access token a send attaches (stored, cached,
/// refreshed or fetched during the send) to its [`SentCredentials`].
struct ReportingTokens {
    inner: crate::oauth::SecretTokens,
    sink: SentCredentials,
}

impl StoredOAuthTokens for ReportingTokens {
    fn valid_access_token(&self, key: &str) -> Option<String> {
        self.inner.valid_access_token(key)
    }

    fn refresh_token(&self, key: &str) -> Option<String> {
        self.inner.refresh_token(key)
    }

    fn store_grant(&self, key: &str, grant: &crate::executor::TokenGrant) -> bool {
        self.inner.store_grant(key, grant)
    }

    fn invalidate(&self, key: &str, token: &str) {
        self.inner.invalidate(key, token);
    }

    fn sent(&self, token: &str) {
        self.sink.extend([token.to_owned()]);
    }
}

/// What an agent send may reach: the hosts its environment declares (refused with a validation
/// error when the request carries a secret or credential to another host) and whether the send
/// is `strict`, i.e. a draft or a request the agent touched and the user has not sent since.
///
/// A request that is not in the store is a draft; that is how an agent-built request is told
/// apart, not by its id.
async fn agent_send_policy(
    state: &AppState,
    request: &Request,
    environment: Option<&Environment>,
    scopes: &VariableScopes,
) -> Result<(Vec<String>, bool), AppError> {
    let (request, environment, scopes) = (request.clone(), environment.cloned(), scopes.clone());
    blocking(state, move |state| {
        let (untrusted, saved) = {
            let store = state.store();
            (
                store
                    .agent_touched_request_ids(request.workspace_id.as_str())
                    .map_err(command_error)?,
                store
                    .get_request(&request.workspace_id, &request.id)
                    .map_err(command_error)?
                    .is_some(),
            )
        };
        let declared = ensure_secret_hosts_declared(
            state,
            &request,
            environment.as_ref(),
            &scopes,
            &untrusted,
        )?;
        let strict = !saved || untrusted.contains(request.id.as_str());
        Ok((declared, strict))
    })
    .await
}

/// Writes the history row of a successful execution and, when `stores_cookies` and the request
/// uses the jar (`send_cookies`), its response cookies (logged when they fail); returns the id of
/// the row (`None` when its workspace is gone) and whether the cookies reached the jar.
async fn persist_successful_execution(
    state: &AppState,
    request: &Request,
    definition: Option<&Request>,
    environment: Option<&Environment>,
    response: &ExecutionResult,
    origin: &HistoryOrigin,
    stores_cookies: bool,
) -> Result<(Option<String>, CookieJarOutcome), AppError> {
    let response_headers = response
        .headers
        .iter()
        .map(|header| KeyValue {
            key: header.key.clone(),
            value: header.value.clone(),
            enabled: true,
            description: None,
        })
        .collect::<Vec<_>>();
    let mut history_entry = ExecutionHistoryEntry::from_success(
        execution_history_id(&request.id),
        request,
        environment.map(|environment| environment.id.as_str()),
        response.status_code,
        &response.status_text,
        &response.url,
        response.elapsed_ms,
        &response.body_text,
        response.body_size_bytes,
        response.attempt_count,
        execution_history_timestamp(),
    )
    .map(|entry| {
        let entry = match definition {
            Some(definition) => entry.with_request_snapshot(definition),
            None => entry,
        };
        entry
            .with_origin(origin)
            .with_environment_name(environment.map_or("", |environment| environment.name.as_str()))
            .with_check_summary(HistoryCheckSummary::of(
                &response.assertion_results,
                &response.script_tests,
            ))
            .with_response_details(
                response.timing,
                &response_headers,
                response.sent_request.as_ref().map(sent_request_summary),
                &response.call_chain,
            )
    })
    .map_err(command_error)?;
    // "Disable cookie jar" also keeps this request's response cookies out of the jar.
    let cookies = if stores_cookies && request.network_options.send_cookies {
        response_cookie_entries(request, response)
    } else {
        Vec::new()
    };
    let history_id = history_entry.id.as_str().to_owned();
    let (workspace_id, request_id) = (request.workspace_id.clone(), request.id.clone());
    blocking(state, move |state| {
        let saved = save_execution_history_if_workspace_exists(state, &mut history_entry)?;
        // The row is written: a cookie failure must not report it as missing.
        let jar = if !saved || cookies.is_empty() {
            CookieJarOutcome::NotStored
        } else {
            match store_response_cookies(state, &workspace_id, &request_id, cookies) {
                Ok(true) => CookieJarOutcome::Stored,
                Ok(false) => CookieJarOutcome::NotStored,
                Err(error) => {
                    log_line(
                        LogLevel::Error,
                        "execution.cookies",
                        &format!("failed to persist response cookies: {error}"),
                    );
                    CookieJarOutcome::Failed
                }
            }
        };
        Ok((saved.then_some(history_id), jar))
    })
    .await
}

/// The wire summary history keeps of a sent request: method, URL, headers and a body preview
/// capped like the response preview (the executor already masked secret values).
fn sent_request_summary(sent: &SentRequest) -> ExecutedRequestSummary {
    let (body_preview, _) = bounded_body_preview(
        sent.body_text.as_deref().unwrap_or_default(),
        HISTORY_BODY_PREVIEW_LIMIT_BYTES,
    );
    ExecutedRequestSummary {
        method: sent.method.clone(),
        url: sent.url.clone(),
        headers: sent
            .headers
            .iter()
            .map(|header| KeyValue {
                key: header.key.clone(),
                value: header.value.clone(),
                enabled: true,
                description: None,
            })
            .collect(),
        body_preview,
    }
}

/// Saves a history entry unless its workspace is gone; clears references to deleted items.
pub(crate) fn save_execution_history_if_workspace_exists(
    state: &AppState,
    history_entry: &mut ExecutionHistoryEntry,
) -> Result<bool, AppError> {
    let store = state.store();
    if store
        .get_workspace(&history_entry.workspace_id)
        .map_err(command_error)?
        .is_none()
    {
        return Ok(false);
    }

    clear_stale_history_refs(&store, history_entry)?;

    store
        .save_execution_history(history_entry)
        .map_err(command_error)?;

    Ok(true)
}

/// The jar entries for the cookies a response set; a cookie that cannot be stored is logged and
/// skipped.
pub fn response_cookie_entries(
    request: &Request,
    response: &ExecutionResult,
) -> Vec<CookieJarEntry> {
    let cookie_timestamp = cookie_jar_timestamp();
    let mut entries = Vec::new();
    for cookie in &response.cookies {
        let Some(domain) = cookie.domain.as_deref() else {
            log_line(
                LogLevel::Warn,
                "execution.cookies",
                &format!(
                    "failed to persist response cookie {}: no resolved domain",
                    cookie.name
                ),
            );
            continue;
        };

        match CookieJarEntry::from_resolved_cookie(
            request.workspace_id.as_str(),
            Some(request.id.as_str()),
            &cookie.name,
            cookie.value.clone(),
            domain,
            cookie.path.as_deref().unwrap_or("/"),
            cookie.host_only,
            cookie.expires.as_deref(),
            cookie.max_age.as_deref(),
            cookie.secure,
            cookie.http_only,
            cookie.same_site.as_deref(),
            &cookie_timestamp,
        ) {
            Ok(entry) => entries.push(entry),
            Err(error) => log_line(
                LogLevel::Warn,
                "execution.cookies",
                &format!("failed to persist response cookie: {error}"),
            ),
        }
    }
    entries
}

/// Saves the cookies of one response in one transaction unless the workspace is gone;
/// `Ok(false)` when it is.
pub(crate) fn store_response_cookies(
    state: &AppState,
    workspace_id: &WorkspaceId,
    request_id: &RequestId,
    mut entries: Vec<CookieJarEntry>,
) -> Result<bool, AppError> {
    if entries.is_empty() {
        return Ok(true);
    }

    let store = state.store();
    if store
        .get_workspace(workspace_id)
        .map_err(command_error)?
        .is_none()
    {
        return Ok(false);
    }

    let source_request_id = store
        .get_request(workspace_id, request_id)
        .map_err(command_error)?
        .map(|_| request_id.clone());
    for entry in &mut entries {
        entry.source_request_id = source_request_id.clone();
    }
    store
        .save_cookie_jar_entries(workspace_id.as_str(), &entries)
        .map_err(command_error)?;

    Ok(true)
}

/// Short stable name of an executor failure, stored in history as the error kind.
#[must_use]
fn execution_error_kind(error: &ExecutionError) -> &'static str {
    match error {
        ExecutionError::UnsupportedProtocol => "UnsupportedProtocol",
        ExecutionError::UnsupportedGraphqlMethod => "UnsupportedGraphqlMethod",
        ExecutionError::UnsupportedBody => "UnsupportedBody",
        ExecutionError::UnresolvedVariable { .. } => "UnresolvedVariable",
        ExecutionError::RedirectHostNotDeclared { .. } => "RedirectHostNotDeclared",
        ExecutionError::GuardRefused { .. } => "GuardRefused",
        ExecutionError::InvalidUrl(_) => "InvalidUrl",
        ExecutionError::ClientBuild(_) => "ClientBuild",
        ExecutionError::Network { .. } => "Network",
        ExecutionError::BodyRead(_) => "BodyRead",
        ExecutionError::BodyWrite { .. } => "BodyWrite",
        ExecutionError::InvalidHeader { .. } => "InvalidHeader",
        ExecutionError::MultipartFileRead { .. } => "MultipartFileRead",
        ExecutionError::InvalidGraphqlVariablesJson(_) => "InvalidGraphqlVariablesJson",
        ExecutionError::GraphqlBodySerialize(_) => "GraphqlBodySerialize",
        ExecutionError::OAuth2Token { .. } => "OAuth2Token",
        ExecutionError::Cancelled { .. } => "Cancelled",
    }
}
