//! Executable API flows: runs ordered steps of saved (or inline) requests,
//! feeding extracted variables into the next step and evaluating per-step
//! assertions.
//!
//! Single responsibility: flow concerns only (step order, conditions, overrides, delays,
//! retries, extraction, result assembly, cancellation, failure explanations and progress
//! events). Sending a step, including collection/folder/request scripts and inherited auth, is
//! delegated to the caller's step executor, which the app layer backs with the same pipeline as
//! Send (history rows per sent attempt, no cookie persistence). Every executed step keeps the
//! full call chain (variables, auth, transport and redirect hops) so a failing integration test
//! can be debugged without replaying it.

mod condition;
mod dag;
mod explain;
mod progress;

mod eval;
#[cfg(test)]
mod tests;

pub use condition::{MAX_CONDITION_BYTES, check_condition};
pub use dag::{MAX_FLOW_CONCURRENCY, check_flow_dag_cycles, has_dag_topology};
pub use explain::explain_failure;
pub use progress::{FlowEvent, forward_flow_events, summarize_run};

use condition::evaluate_condition;
use eval::{apply_local, extract_value, render_condition, set_variable};

use std::collections::{BTreeMap, HashMap, HashSet};
use std::future::Future;
use std::time::{Duration, Instant};

use tokio::time::sleep;

use crate::cancel::{CancelToken, until_cancelled};
use crate::error::AppError;
use crate::executor::ExecutionResult;
use crate::workspace::items::{
    ExecutedRequestSummary, ExplanationAction, FailureExplanation, FailureKind, Flow,
    FlowAttemptResult, FlowBodyKind, FlowRetryOn, FlowRunInput, FlowRunResult, FlowStep,
    FlowStepKind, FlowStepResult,
};
use crate::workspace::models::{
    HISTORY_BODY_PREVIEW_LIMIT_BYTES, NetworkField, RawLanguage, bounded_body_preview,
};
use crate::workspace::{EnvironmentVariable, KeyValue, Request, RequestBody, VariableScopes};

/// Observer of finished steps: the execution-order index and the step's final result.
pub type StepObserver<'a> = dyn Fn(usize, &FlowStepResult) + Sync + 'a;

/// What the step executor reports for one sent step.
#[derive(Debug, Clone)]
pub struct StepOutput {
    pub execution: ExecutionResult,
    /// The step's script-local (`pm.variables`) scope after it ran: the flow variables it was
    /// given, plus what its scripts set, minus what they unset. The flow variables follow it, so
    /// a value one step's script sets reaches the later steps of the same run. `None` leaves the
    /// flow variables as they were.
    pub local: Option<BTreeMap<String, String>>,
}

impl From<ExecutionResult> for StepOutput {
    fn from(execution: ExecutionResult) -> Self {
        Self {
            execution,
            local: None,
        }
    }
}

/// Identity, cancellation and progress hook of one flow run.
pub struct FlowRun<'a> {
    /// Id of this run, reported back on the result (the app layer generates it).
    pub run_id: String,
    /// When the run started, in Unix milliseconds, reported back on the result.
    pub started_at_ms: i64,
    /// Checked before every step and raced against delays and retry waits.
    pub cancel: Option<&'a CancelToken>,
    /// Called with the execution-order index and the result of each step the moment it is
    /// final (skipped and failed steps included, a cancelled step excluded).
    pub on_step: Option<&'a StepObserver<'a>>,
}

impl FlowRun<'_> {
    /// A run without cancellation or observer.
    #[must_use]
    pub fn new(run_id: impl Into<String>, started_at_ms: i64) -> Self {
        Self {
            run_id: run_id.into(),
            started_at_ms,
            cancel: None,
            on_step: None,
        }
    }
}

/// Executes `flow` step by step.
///
/// `execute_step` sends one resolved step request given the current flow variables (initial
/// variables, earlier extractions and script-local values, the narrowest variable scope; secret
/// extractions and seeds with their names are flagged secret); it takes both by value so its
/// future needs no borrowed arguments (and stays `Send`). It returns a [`StepOutput`] (or a bare
/// [`ExecutionResult`]), and [`AppError::Cancelled`] as its error when the run was cancelled,
/// which ends the flow as cancelled. `condition_scopes` builds, for a step with a condition, the variable scopes the
/// condition is interpolated against, from the step's resolved request (when it resolves) and the
/// current flow variables; the caller backs it with the same context `execute_step` uses, so a
/// condition never sees an older view than the step's request does. `run.cancel` is checked
/// before every step and raced against delays and retry waits. The condition is evaluated before
/// the step's delay, so a skipped step does not wait; a condition that cannot be evaluated (see
/// [`check_condition`]) fails the step.
///
/// A step is judged by its saved request's assertions followed by its own (unless
/// `ignore_request_assertions`), read from the execution: `execute_step` must evaluate
/// `request.assertions` like the Send pipeline does. `input.only_step` runs just that step;
/// observer indexes are positions in the flow's execution order.
///
/// A step with `timeout_ms` sends its request with `network_options.timeout_ms` set to it, on
/// every attempt. A step with `retries > 0` and a non-empty `retry_on` is sent again, up to
/// `retries` more times and `retry_wait_ms` apart, while the attempt failed with a selected class
/// (see [`FlowRetryOn`]); the result keeps the last attempt's data and `attempts` counts the
/// sends. The step's `elapsed_ms` spans all attempts and waits.
pub async fn run_flow<Step, StepFuture, Sent, Scopes, ScopesFuture>(
    flow: &Flow,
    requests: &HashMap<String, Request>,
    input: &FlowRunInput,
    run: &FlowRun<'_>,
    condition_scopes: Scopes,
    execute_step: Step,
) -> FlowRunResult
where
    Step: Fn(Request, Vec<EnvironmentVariable>) -> StepFuture,
    StepFuture: Future<Output = Result<Sent, AppError>>,
    Sent: Into<StepOutput>,
    Scopes: Fn(Option<Request>, Vec<EnvironmentVariable>) -> ScopesFuture,
    ScopesFuture: Future<Output = VariableScopes>,
{
    let started = Instant::now();
    let cancel = run.cancel;
    let mut steps: Vec<&FlowStep> = flow.steps.iter().collect();
    steps.sort_by_key(|step| step.position);

    // A seed named like a secret extraction (the previous run's token) is a secret too.
    let secret_names: HashSet<&str> = flow
        .steps
        .iter()
        .flat_map(|step| &step.extractions)
        .filter(|extraction| extraction.is_secret())
        .map(|extraction| extraction.variable_name.as_str())
        .collect();
    let mut flow_variables: Vec<EnvironmentVariable> = input
        .initial_variables
        .iter()
        .filter_map(|(key, value)| EnvironmentVariable::plain(key, value.clone()).ok())
        .map(|variable| EnvironmentVariable {
            is_secret: secret_names.contains(variable.key.as_str()),
            ..variable
        })
        .collect();

    if has_dag_topology(flow) && input.only_step.is_none() && input.max_steps.is_none() {
        return dag::run_flow_dag(
            flow,
            requests,
            input,
            run,
            started,
            flow_variables,
            condition_scopes,
            execute_step,
        )
        .await;
    }

    let mut results = Vec::with_capacity(steps.len());
    let mut passed = true;
    let mut cancelled = false;

    for (index, step) in steps.into_iter().enumerate() {
        if let Some(only) = input.only_step {
            if index < only {
                continue;
            }
            if index > only {
                break;
            }
        }
        if input.max_steps.is_some_and(|max| index >= max) {
            break;
        }
        if cancel.is_some_and(CancelToken::is_cancelled) {
            cancelled = true;
            break;
        }
        if step.step_kind != FlowStepKind::Request {
            let res = blank_result(step, 0, Some(elapsed_ms(started)));
            record(&mut results, run, index, res);
            continue;
        }

        match execute_single_request_step(
            flow,
            step,
            index,
            requests,
            flow_variables.clone(),
            started,
            cancel,
            &condition_scopes,
            &execute_step,
        )
        .await
        {
            SingleStepOutcome::Cancelled => {
                cancelled = true;
                break;
            }
            SingleStepOutcome::Finished {
                result: step_result,
                local_updates,
                extracted,
            } => {
                if let Some(local) = local_updates {
                    apply_local(&mut flow_variables, local);
                }
                for (k, v, is_secret) in extracted {
                    set_variable(&mut flow_variables, &k, &v, is_secret);
                }
                passed &= step_result.passed;
                let failed = !step_result.passed;
                record(&mut results, run, index, *step_result);
                if failed && !step.continue_on_error {
                    break;
                }
            }
        }
    }

    FlowRunResult {
        run_id: run.run_id.clone(),
        flow_id: flow.id.clone(),
        started_at_ms: run.started_at_ms,
        passed: passed && !cancelled,
        cancelled,
        elapsed_ms: elapsed_ms(started),
        steps_total: flow.steps.len(),
        variables: flow_variables
            .iter()
            .map(|variable| KeyValue {
                key: variable.key.clone(),
                value: variable.value.clone().unwrap_or_default(),
                enabled: variable.enabled,
                description: None,
            })
            .collect(),
        secret_variables: flow_variables
            .iter()
            .filter(|variable| variable.is_secret)
            .map(|variable| variable.key.clone())
            .collect(),
        steps: results,
        scope: input.scope(),
        environment_id: None,
    }
}

pub(super) enum SingleStepOutcome {
    Cancelled,
    Finished {
        result: Box<FlowStepResult>,
        local_updates: Option<BTreeMap<String, String>>,
        extracted: Vec<(String, String, bool)>,
    },
}

#[allow(clippy::too_many_arguments)]
pub(super) async fn execute_single_request_step<Step, StepFuture, Sent, Scopes, ScopesFuture>(
    flow: &Flow,
    step: &FlowStep,
    index: usize,
    requests: &HashMap<String, Request>,
    flow_variables: Vec<EnvironmentVariable>,
    run_started: Instant,
    cancel: Option<&CancelToken>,
    condition_scopes: &Scopes,
    execute_step: &Step,
) -> SingleStepOutcome
where
    Step: Fn(Request, Vec<EnvironmentVariable>) -> StepFuture,
    StepFuture: Future<Output = Result<Sent, AppError>>,
    Sent: Into<StepOutput>,
    Scopes: Fn(Option<Request>, Vec<EnvironmentVariable>) -> ScopesFuture,
    ScopesFuture: Future<Output = VariableScopes>,
{
    let step_started = Instant::now();
    let offset = Some(elapsed_ms(run_started));

    if !step.condition_expression.trim().is_empty() {
        let step_scopes = condition_scopes(
            resolve_step_request(flow, step, requests).ok(),
            flow_variables.clone(),
        )
        .await;
        let rendered = || Some(render_condition(&step.condition_expression, &step_scopes));
        match evaluate_condition(&step.condition_expression, &step_scopes) {
            Ok(true) => {}
            Ok(false) => {
                let skipped = FlowStepResult {
                    skipped: true,
                    condition: rendered(),
                    ..blank_result(step, elapsed_ms(step_started), offset)
                };
                return SingleStepOutcome::Finished {
                    result: Box::new(skipped),
                    local_updates: None,
                    extracted: Vec::new(),
                };
            }
            Err(reason) => {
                let failed = FlowStepResult {
                    passed: false,
                    error: Some(format!("condition cannot be evaluated: {reason}")),
                    condition: rendered(),
                    ..blank_result(step, elapsed_ms(step_started), offset)
                };
                return SingleStepOutcome::Finished {
                    result: Box::new(failed),
                    local_updates: None,
                    extracted: Vec::new(),
                };
            }
        }
    }

    let delay = Duration::from_millis(step.delay_ms);
    if until_cancelled(cancel, sleep(delay)).await.is_none() {
        return SingleStepOutcome::Cancelled;
    }

    let mut request = match resolve_step_request(flow, step, requests) {
        Ok(request) => request,
        Err(issue) => {
            return SingleStepOutcome::Finished {
                result: Box::new(FlowStepResult {
                    passed: false,
                    error: Some(issue.message(index, step)),
                    explanation: issue.explanation(index, step),
                    ..blank_result(step, elapsed_ms(step_started), offset)
                }),
                local_updates: None,
                extracted: Vec::new(),
            };
        }
    };

    let request_assertion_count = if step.ignore_request_assertions {
        request.assertions.clear();
        0
    } else {
        request.assertions.len()
    };
    request.assertions.extend(step.assertions.iter().cloned());
    if let Some(timeout_ms) = step.timeout_ms {
        request.network_options.timeout_ms = timeout_ms;
        request
            .network_options
            .inherit
            .retain(|field| *field != NetworkField::TimeoutMs);
    }

    let max_attempts = if step.retries > 0 && !step.retry_on.is_empty() {
        u32::from(step.retries) + 1
    } else {
        1
    };
    let mut attempts = 0;
    let mut attempt_list: Vec<FlowAttemptResult> = Vec::new();
    let outcome = loop {
        attempts += 1;
        let attempt_started = Instant::now();
        let outcome: Result<StepOutput, AppError> =
            execute_step(request.clone(), flow_variables.clone())
                .await
                .map(Into::into);
        let execution = outcome.as_ref().map(|output| &output.execution);
        attempt_list.push(FlowAttemptResult {
            attempt: attempts,
            status_code: execution.ok().map(|execution| execution.status_code),
            elapsed_ms: elapsed_ms(attempt_started),
            error: execution.err().map(std::string::ToString::to_string),
            timing: execution.ok().map(|execution| execution.timing),
        });
        if attempts >= max_attempts || !should_retry(step, execution) {
            break Some(outcome);
        }
        let wait = Duration::from_millis(step.retry_wait_ms);
        if until_cancelled(cancel, sleep(wait)).await.is_none() {
            break None;
        }
    };
    let Some(result) = outcome else {
        return SingleStepOutcome::Cancelled;
    };
    if result.as_ref().is_err_and(AppError::is_cancelled) {
        return SingleStepOutcome::Cancelled;
    }

    let mut step_result = blank_result(step, elapsed_ms(step_started), offset);
    step_result.attempts = attempts;
    step_result.attempt_list = attempt_list;

    let mut cause = None;
    let mut local_updates = None;
    let mut extracted = Vec::new();
    match result {
        Ok(StepOutput { execution, local }) => {
            local_updates = local;
            fill_step_result(&mut step_result, &execution);
            step_result.request_assertion_count = request_assertion_count;

            for extraction in &step.extractions {
                match extract_value(extraction, &execution) {
                    Ok(value) => {
                        extracted.push((
                            extraction.variable_name.clone(),
                            value.clone(),
                            extraction.is_secret(),
                        ));
                        step_result.extracted_variables.push(KeyValue {
                            key: extraction.variable_name.clone(),
                            value,
                            enabled: true,
                            description: None,
                        });
                    }
                    Err(error) => {
                        step_result.error = Some(error);
                        step_result.passed = false;
                    }
                }
            }

            if !step_result
                .assertion_results
                .iter()
                .all(|assertion| assertion.passed)
                || !execution
                    .script_tests
                    .iter()
                    .all(|test| test.passed || test.skipped)
            {
                step_result.passed = false;
            }
            if let Some(error) = &execution.script_error {
                step_result.passed = false;
                step_result.error.get_or_insert_with(|| {
                    format!("script failed in {}: {}", error.source, error.message)
                });
            }
            if let Some(error) = execution.ai.as_ref().and_then(|ai| ai.error.as_ref()) {
                step_result.passed = false;
                step_result.error.get_or_insert_with(|| error.clone());
            }
        }
        Err(error) => {
            if let AppError::Network { cause: network, .. } = &error {
                cause = *network;
            }
            step_result.passed = false;
            step_result.error = Some(error.to_string());
        }
    }

    step_result.explanation = explain_failure(step, &request.assertions, &step_result, cause);

    SingleStepOutcome::Finished {
        result: Box::new(step_result),
        local_updates,
        extracted,
    }
}

/// Checks, before anything is sent, that the flow DAG has no cycles and every HTTP request step
/// `input` runs can build its request: a saved request that still exists, or an inline URL.
pub fn validate_runnable(
    flow: &Flow,
    requests: &HashMap<String, Request>,
    input: &FlowRunInput,
) -> Result<(), AppError> {
    if has_dag_topology(flow)
        && let Err(cyclic) = check_flow_dag_cycles(flow)
    {
        return Err(AppError::validation(format!(
            "flow contains a dependency cycle involving: {}",
            cyclic.join(", ")
        )));
    }
    let mut steps: Vec<&FlowStep> = flow.steps.iter().collect();
    steps.sort_by_key(|step| step.position);
    for (index, step) in steps.into_iter().enumerate() {
        let runs = input.only_step.is_none_or(|only| index == only)
            && input.max_steps.is_none_or(|max| index < max);
        if !runs || step.step_kind != FlowStepKind::Request {
            continue;
        }
        if let Err(issue) = resolve_step_request(flow, step, requests) {
            return Err(
                AppError::validation(issue.message(index, step)).with_keys(vec![step.id.clone()])
            );
        }
    }
    Ok(())
}

/// A passing, empty result of `step`: the base every outcome starts from. `attempts` stays 0
/// until something was sent.
pub(super) fn blank_result(
    step: &FlowStep,
    elapsed: u64,
    started_offset_ms: Option<u64>,
) -> FlowStepResult {
    FlowStepResult {
        step_id: step.id.clone(),
        step_name: step_name(step).to_owned(),
        request_id: step.request_id.clone(),
        status_code: None,
        elapsed_ms: elapsed,
        passed: true,
        error: None,
        skipped: false,
        attempts: 0,
        extracted_variables: Vec::new(),
        assertion_results: Vec::new(),
        executed_request: None,
        response_headers: Vec::new(),
        response_body_preview: String::new(),
        timing: None,
        console_logs: Vec::new(),
        script_tests: Vec::new(),
        call_chain: None,
        explanation: None,
        attempt_list: Vec::new(),
        request_assertion_count: 0,
        history_entry_id: None,
        condition: None,
        ai: None,
        matched_branch_id: None,
        started_offset_ms,
    }
}

/// Whether the outcome of one attempt is a failure the step is configured to retry: a transport
/// error under `Network`, or a 5xx/429 response under `Status5xx`/`Status429`. Everything else
/// (assertions, scripts, extractions, unresolved variables, cancellation) is final.
fn should_retry(step: &FlowStep, outcome: Result<&ExecutionResult, &AppError>) -> bool {
    let class = match outcome {
        Ok(execution) => match execution.status_code {
            429 => FlowRetryOn::Status429,
            500..=599 => FlowRetryOn::Status5xx,
            _ => return false,
        },
        Err(AppError::Network { .. }) => FlowRetryOn::Network,
        Err(_) => return false,
    };
    step.retry_on.contains(&class)
}

/// Hands a finished step (at execution-order `index`) to the observer, then keeps it.
pub(super) fn record(
    results: &mut Vec<FlowStepResult>,
    run: &FlowRun<'_>,
    index: usize,
    result: FlowStepResult,
) {
    if let Some(on_step) = run.on_step {
        on_step(index, &result);
    }
    results.push(result);
}

/// Copies the verdicts and response-side debugging details of one execution onto the step.
fn fill_step_result(step_result: &mut FlowStepResult, execution: &ExecutionResult) {
    step_result.status_code = Some(execution.status_code);
    step_result.assertion_results = execution.assertion_results.clone();
    step_result.history_entry_id = execution.history_id.clone();
    step_result.response_headers = execution
        .headers
        .iter()
        .map(|header| KeyValue {
            key: header.key.clone(),
            value: header.value.clone(),
            enabled: true,
            description: None,
        })
        .collect();
    let (preview, _) = bounded_body_preview(&execution.body_text, HISTORY_BODY_PREVIEW_LIMIT_BYTES);
    step_result.response_body_preview = preview;
    step_result.timing = Some(execution.timing);
    step_result.console_logs = execution.console.clone();
    step_result.script_tests = execution.script_tests.clone();
    step_result.call_chain = Some(execution.call_chain.clone());
    step_result.ai.clone_from(&execution.ai);
    step_result.executed_request =
        execution
            .sent_request
            .as_ref()
            .map(|sent| ExecutedRequestSummary {
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
                body_preview: sent.body_text.clone().unwrap_or_default(),
            });
}

/// Builds the request one step sends: the saved request with per-step overrides
/// applied, or a fully inline request when `request_id` is empty.
fn resolve_step_request(
    flow: &Flow,
    step: &FlowStep,
    requests: &HashMap<String, Request>,
) -> Result<Request, StepIssue> {
    let mut request = if step.request_id.trim().is_empty() {
        let url = step.url_override.clone().unwrap_or_default();
        if url.trim().is_empty() {
            return Err(StepIssue::Incomplete);
        }
        Request::new(
            format!("req_step_{}", step.id),
            flow.workspace_id.as_str(),
            step_name(step),
            url,
        )
        .map_err(|error| StepIssue::Invalid(error.to_string()))?
    } else {
        requests
            .get(step.request_id.trim())
            .cloned()
            .ok_or(StepIssue::RequestDeleted)?
    };

    if let Some(method) = step.method_override {
        request.method = method;
    }
    if let Some(url) = step
        .url_override
        .as_deref()
        .filter(|url| !url.trim().is_empty())
    {
        request.url = url.to_owned();
    }
    merge_rows(&mut request.headers, &step.headers_override, true);
    merge_rows(&mut request.query_params, &step.query_override, false);
    if let Some(body) = &step.body_override {
        request.body = override_body(&request.body, step.body_override_kind, body);
    }

    Ok(request)
}

/// The body `text` overrides `saved` with: sent as `kind` when the step names one, else in the
/// saved body's own type (JSON stays JSON, raw text keeps its language, a form takes `key=value`
/// pairs, GraphQL replaces the query and keeps its variables and operation). A saved body with no
/// text form (none, multipart, binary) is replaced by plain text.
fn override_body(saved: &RequestBody, kind: Option<FlowBodyKind>, text: &str) -> RequestBody {
    let content = text.to_owned();
    match kind {
        Some(FlowBodyKind::Json) => RequestBody::Json { content },
        Some(FlowBodyKind::Text) => RequestBody::RawText {
            content,
            language: Some(RawLanguage::Text),
        },
        Some(FlowBodyKind::Xml) => RequestBody::RawText {
            content,
            language: Some(RawLanguage::Xml),
        },
        Some(FlowBodyKind::Html) => RequestBody::RawText {
            content,
            language: Some(RawLanguage::Html),
        },
        Some(FlowBodyKind::Javascript) => RequestBody::RawText {
            content,
            language: Some(RawLanguage::Javascript),
        },
        Some(FlowBodyKind::FormUrlencoded) => RequestBody::FormUrlEncoded {
            fields: form_fields(text),
        },
        None => match saved {
            RequestBody::Json { .. } => RequestBody::Json { content },
            RequestBody::RawText { language, .. } => RequestBody::RawText {
                content,
                language: *language,
            },
            RequestBody::FormUrlEncoded { .. } => RequestBody::FormUrlEncoded {
                fields: form_fields(text),
            },
            RequestBody::Graphql {
                variables_json,
                operation_name,
                ..
            } => RequestBody::Graphql {
                query: content,
                variables_json: variables_json.clone(),
                operation_name: operation_name.clone(),
            },
            RequestBody::None | RequestBody::Multipart { .. } | RequestBody::Binary { .. } => {
                RequestBody::RawText {
                    content,
                    language: None,
                }
            }
        },
    }
}

/// Form fields from `key=value` lines (the editor's `name=value` text): blank lines are skipped,
/// a `# ` prefix disables the field, the first `=` separates (values may contain `=` and `&`) and
/// a line without one has an empty value. Values are taken as written; the sender encodes them.
fn form_fields(text: &str) -> Vec<KeyValue> {
    text.lines()
        .filter_map(|line| {
            let line = line.trim();
            let (enabled, line) = match line.strip_prefix('#') {
                Some(rest) if rest.is_empty() || rest.starts_with(' ') => (false, rest.trim()),
                _ => (true, line),
            };
            if line.is_empty() {
                return None;
            }
            let (key, value) = line.split_once('=').unwrap_or((line, ""));
            Some(KeyValue {
                key: key.trim().to_owned(),
                value: value.trim().to_owned(),
                enabled,
                description: None,
            })
        })
        .collect()
}

/// Why a step cannot build its request.
#[derive(Debug)]
enum StepIssue {
    /// Neither a saved request nor an inline URL.
    Incomplete,
    /// The saved request no longer exists.
    RequestDeleted,
    /// The inline request is malformed.
    Invalid(String),
}

impl StepIssue {
    /// The error text, naming the step by its number (from 1) and name, never its id.
    fn message(&self, index: usize, step: &FlowStep) -> String {
        let step_label = format!("step {} \"{}\"", index + 1, step_name(step));
        match self {
            Self::Incomplete => format!("{step_label} has neither a saved request nor a URL"),
            Self::RequestDeleted => format!("{step_label} uses a saved request that was deleted"),
            Self::Invalid(error) => format!("{step_label}: {error}"),
        }
    }

    /// The failure explanation, for the two issues that have a kind.
    fn explanation(&self, index: usize, step: &FlowStep) -> Option<FailureExplanation> {
        let kind = match self {
            Self::Incomplete => FailureKind::StepIncomplete,
            Self::RequestDeleted => FailureKind::RequestDeleted,
            Self::Invalid(_) => return None,
        };
        Some(FailureExplanation {
            kind,
            message: self.message(index, step),
            // The step's request section is where both are fixed.
            action: ExplanationAction::EditAssertion,
            detail: None,
            facts: None,
        })
    }
}

/// Overrides rows in place and appends the ones that do not exist yet.
///
/// Disabled and blank-key override rows are ignored. Keys match case-insensitively when
/// `ignore_case` is set (header names).
fn merge_rows(base: &mut Vec<KeyValue>, overrides: &[KeyValue], ignore_case: bool) {
    for row in overrides
        .iter()
        .filter(|row| row.enabled && !row.key.trim().is_empty())
    {
        let same_key = |existing: &KeyValue| {
            if ignore_case {
                existing.key.eq_ignore_ascii_case(&row.key)
            } else {
                existing.key == row.key
            }
        };
        match base.iter_mut().find(|existing| same_key(existing)) {
            Some(existing) => *existing = row.clone(),
            None => base.push(row.clone()),
        }
    }
}

/// The step's display name: its own name, or its id when the name is blank.
pub(super) fn step_name(step: &FlowStep) -> &str {
    if step.name.trim().is_empty() {
        &step.id
    } else {
        &step.name
    }
}

pub(super) fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}
