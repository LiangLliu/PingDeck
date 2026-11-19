//! Model diagnostics: one function per capability test of a provider key and model, and the pure
//! logic of the Test bench around them.
//!
//! Single responsibility: run one [`ProbeTest`] against one [`Target`] and describe the outcome
//! as a [`ProbeResult`] (this file and the check modules), and turn results into the Test bench's
//! vocabulary, plans, estimates, capability bits, apply plans and exports (the pure modules).
//! Probes are diagnostics, so they bypass the gateway, the circuit breaker and the health book;
//! every call is a single attempt through [`crate::llm::upstream`]. A probe never returns an
//! error: a failure is a result with `ok == false`, a [`FailureKind`] and a message, and a fired
//! cancel token is `FailureKind::Cancelled`.
//!
//! Chat-family tests speak the hub format and pick their protocol with [`Target::chat_protocol`],
//! so they exercise the same adapters the gateway uses. Only the connectivity, auth and models
//! tests are free; cache, search and pdf run only in the Full check set (`plan::CheckSet`) and
//! the context test never runs in a set, because they are slow, expensive or opt-in.

pub use pingdeck_llm_domain::llm::probe::{
    CellRecord, CheckTimeouts, ExportFile, ProbeRequest, ProbeResult, ProbeStatus, ProbeTest,
    RequestRecord, apply, capability, estimate, export, listing, plan, quirks, status, timeouts,
};
mod checks;
mod context;
mod free;
mod http;
mod misc;
mod native;
pub mod preview;
pub mod run;
mod stream;

/// Test fakes of the probe (fake upstream, canned replies). The app crate's tests reach them
/// through the `test-support` feature; a release build never compiles them.
#[cfg(any(test, feature = "test-support"))]
#[doc(hidden)]
pub mod test_support;
#[cfg(test)]
mod tests;

use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::llm::adapters::codec;
use crate::llm::client::encode_for;
use crate::llm::failure::{FailureKind, is_retryable_status};
use crate::llm::hub::{HubRequest, HubResponse, HubUsage};
use crate::llm::router::Target;
use crate::llm::runtime::{LlmRuntime, now_ms};
use crate::llm::store::new_request_id;
use crate::llm::upstream::{AttemptError, Call, read_json, send_measured};
use crate::llm::wire::Protocol;
use pingdeck_base::cancel::{CancelToken, until_cancelled};
use pingdeck_base::error::AppError;
use pingdeck_base::secrets::redact_secrets;

pub use context::{CONTEXT_PROBE_WARNING, ContextProbeInfo};
pub use http::fetch_model_ids;

/// Inputs shared by every test of one run.
#[derive(Debug, Clone)]
pub struct ProbeOptions {
    /// Prompt of the chat and stream tests (`models.default_test_prompt`).
    pub prompt: String,
    /// Chat protocol to force; used only when the target speaks it.
    pub protocol: Option<Protocol>,
    /// Whether the user opted in to the expensive context test.
    pub allow_context_probe: bool,
    /// The provider's model-list path, relative to the base URL; `None` when it has none.
    pub models_endpoint: Option<String>,
    /// The context window the provider or catalog declares, in tokens.
    pub declared_context: Option<u64>,
    /// The model is known to reason before it answers: the Chat check's output cap is scaled up
    /// so the thinking does not eat the whole answer.
    pub reasoning_model: bool,
}

/// The requests of one check, in the order they were sent.
#[derive(Debug, Default)]
pub(crate) struct RequestLog(Mutex<Vec<ProbeRequest>>);

impl RequestLog {
    fn push(&self, request: ProbeRequest) {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .push(request);
    }

    fn take(&self) -> Vec<ProbeRequest> {
        std::mem::take(&mut *self.0.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Records one finished request. `usage` is `None` for calls that report none.
    #[allow(clippy::too_many_arguments)]
    fn record(
        &self,
        target: &Target,
        endpoint: &str,
        started_ms: i64,
        started: Instant,
        status: Option<u16>,
        usage: Option<&HubUsage>,
        error: Option<&AttemptError>,
        stream: bool,
    ) {
        let error_status = error.and_then(AttemptError::status);
        self.push(ProbeRequest {
            endpoint: endpoint.to_owned(),
            started_ms,
            latency_ms: elapsed_ms(started),
            ttft_ms: None,
            status: error_status.or(status),
            input_tokens: usage.and_then(|usage| usage.input_tokens),
            output_tokens: usage.and_then(|usage| usage.output_tokens),
            cached_input_tokens: usage.and_then(|usage| usage.cached_input_tokens),
            failure_kind: error.map(AttemptError::kind),
            error: error.map(|error| {
                preview(
                    &redact_secrets(&error.message(), std::slice::from_ref(&target.secret)),
                    300,
                )
            }),
            stream,
            request_id: new_request_id(),
        });
    }
}

impl RequestLog {
    /// Records a streamed call whose response headers were accepted (status 200).
    fn record_stream(
        &self,
        target: &Target,
        endpoint: &str,
        (started_ms, started): (i64, Instant),
        ttft_ms: Option<u64>,
        usage: &HubUsage,
        error: Option<&AttemptError>,
    ) {
        self.record(
            target,
            endpoint,
            started_ms,
            started,
            Some(200),
            Some(usage),
            error,
            true,
        );
        if let Some(last) = self
            .0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last_mut()
        {
            last.ttft_ms = ttft_ms;
        }
    }

    /// The status of the last request, if it got one.
    fn last_status(&self) -> Option<u16> {
        self.0
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .last()
            .and_then(|request| request.status)
    }
}

/// The outcome of one check: the stored result and the requests it sent.
#[derive(Debug)]
pub struct CheckOutput {
    /// The result to store and show.
    pub result: ProbeResult,
    /// One entry per HTTP request sent.
    pub requests: Vec<ProbeRequest>,
}

/// Everything a test function needs.
struct Ctx<'a> {
    runtime: &'a LlmRuntime,
    target: &'a Target,
    options: &'a ProbeOptions,
    timeouts: &'a CheckTimeouts,
    cancel: Option<&'a CancelToken>,
    log: RequestLog,
}

/// `Ok` carries a finished verdict written into the result (pass or soft failure); `Err` is a
/// failed call that [`run_timed`] turns into the failure fields.
type Outcome = Result<(), AttemptError>;

impl Ctx<'_> {
    /// The chat protocol of this run.
    fn chat_proto(&self) -> Result<Protocol, AttemptError> {
        self.target
            .chat_protocol(self.options.protocol)
            .ok_or_else(|| {
                AttemptError::Invalid(AppError::unsupported("provider speaks no chat protocol"))
            })
    }

    fn models_endpoint(&self) -> Option<&str> {
        self.options
            .models_endpoint
            .as_deref()
            .map(str::trim)
            .filter(|endpoint| !endpoint.is_empty())
    }

    /// A GET to the provider that is logged under `endpoint`.
    fn get(&self, endpoint: &'static str, timeout: Duration) -> http::Get<'_> {
        http::Get {
            runtime: self.runtime,
            target: self.target,
            cancel: self.cancel,
            log: Some(&self.log),
            endpoint,
            timeout,
        }
    }

    fn request(&self, prompt: impl Into<String>, max_tokens: u32) -> HubRequest {
        user_request(&self.target.model, prompt, max_tokens)
    }
}

/// A one-prompt request with an output cap: the base of every chat-family check.
fn user_request(model: &str, prompt: impl Into<String>, max_tokens: u32) -> HubRequest {
    let mut request = HubRequest::user_prompt(model, prompt);
    request.max_tokens = Some(max_tokens);
    request
}

/// Runs one test with the default timeouts ([`CheckTimeouts::default`]). Never fails: every
/// problem is part of the returned result.
pub async fn run_test(
    runtime: &LlmRuntime,
    target: &Target,
    test: ProbeTest,
    options: &ProbeOptions,
    cancel: Option<&CancelToken>,
) -> ProbeResult {
    run_timed(
        runtime,
        target,
        test,
        options,
        &CheckTimeouts::default(),
        cancel,
    )
    .await
    .result
}

/// Runs one test with explicit per-check timeouts and returns the requests it sent. Never
/// fails: every problem is part of the returned result.
pub async fn run_timed(
    runtime: &LlmRuntime,
    target: &Target,
    test: ProbeTest,
    options: &ProbeOptions,
    timeouts: &CheckTimeouts,
    cancel: Option<&CancelToken>,
) -> CheckOutput {
    let ctx = Ctx {
        runtime,
        target,
        options,
        timeouts,
        cancel,
        log: RequestLog::default(),
    };
    let endpoint = match test {
        ProbeTest::Connectivity => "base",
        ProbeTest::Auth | ProbeTest::Models => "models",
        _ => target
            .chat_protocol(options.protocol)
            .map_or("none", Protocol::as_str),
    };
    let mut result = ProbeResult {
        key_id: target.key_id.clone(),
        model_id: target.model.clone(),
        endpoint: endpoint.to_owned(),
        test: test.as_str().to_owned(),
        ok: false,
        status: None,
        latency_ms: None,
        ttft_ms: None,
        tps: None,
        input_tokens: None,
        output_tokens: None,
        failure_kind: None,
        message: String::new(),
        detail: Value::Null,
        checked_at_ms: now_ms(),
        ..ProbeResult::default()
    };
    let started = Instant::now();
    let outcome = match test {
        ProbeTest::Connectivity => free::test_connectivity(&ctx, &mut result).await,
        ProbeTest::Auth => free::test_auth(&ctx, &mut result).await,
        ProbeTest::Models => free::test_models(&ctx, &mut result).await,
        ProbeTest::Chat => checks::test_chat(&ctx, &mut result).await,
        ProbeTest::Stream => stream::test_stream(&ctx, &mut result).await,
        ProbeTest::Tools => checks::test_tools(&ctx, &mut result).await,
        ProbeTest::JsonSchema => checks::test_json_schema(&ctx, &mut result).await,
        ProbeTest::Vision => checks::test_vision(&ctx, &mut result).await,
        ProbeTest::Reasoning => checks::test_reasoning(&ctx, &mut result).await,
        ProbeTest::Cache => checks::test_cache(&ctx, &mut result).await,
        ProbeTest::Embeddings => misc::test_embeddings(&ctx, &mut result).await,
        ProbeTest::Search => native::test_search(&ctx, &mut result).await,
        ProbeTest::Pdf => native::test_pdf(&ctx, &mut result).await,
        ProbeTest::Decision => misc::test_decision(&ctx, &mut result).await,
        ProbeTest::Context => context::test_context(&ctx, &mut result).await,
    };
    match outcome {
        Ok(()) => {
            if result.status.is_none() {
                result.status = ctx.log.last_status();
            }
        }
        Err(error) => {
            fail(&mut result, error.kind(), error.message());
            result.status = error.status().or(result.status);
            if let Some(wait) = error.retry_after() {
                // The reason line of a rate-limited cell names the wait ("retry in 8 s").
                if !result.detail.is_object() {
                    result.detail = Value::Object(serde_json::Map::new());
                }
                result.detail["retry_after_s"] = Value::from(wait.as_secs().max(1));
            }
        }
    }
    result.message = redact_secrets(&result.message, std::slice::from_ref(&target.secret));
    result.latency_ms.get_or_insert_with(|| elapsed_ms(started));
    let requests = ctx.log.take();
    // The last request decided the result (its status, its error): the cell links to its row.
    result.request_id = requests.last().map(|request| request.request_id.clone());
    CheckOutput { result, requests }
}

// ---------------------------------------------------------------------------------------------
// Small helpers
// ---------------------------------------------------------------------------------------------

fn pass(result: &mut ProbeResult, message: impl Into<String>) {
    result.ok = true;
    result.failure_kind = None;
    result.message = message.into();
}

fn fail(result: &mut ProbeResult, kind: FailureKind, message: impl Into<String>) {
    result.ok = false;
    result.failure_kind = Some(kind);
    result.message = message.into();
}

fn elapsed_ms(started: Instant) -> u64 {
    u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX)
}

fn preview(text: &str, chars: usize) -> String {
    text.chars().take(chars).collect()
}

fn set_tokens(result: &mut ProbeResult, usage: &HubUsage) {
    result.input_tokens = usage.input_tokens;
    result.output_tokens = usage.output_tokens;
}

fn add_tokens(a: Option<u64>, b: Option<u64>) -> Option<u64> {
    match (a, b) {
        (None, None) => None,
        _ => Some(a.unwrap_or(0).saturating_add(b.unwrap_or(0))),
    }
}

fn sum_usage(a: &HubUsage, b: &HubUsage) -> HubUsage {
    HubUsage {
        input_tokens: add_tokens(a.input_tokens, b.input_tokens),
        output_tokens: add_tokens(a.output_tokens, b.output_tokens),
        cached_input_tokens: add_tokens(a.cached_input_tokens, b.cached_input_tokens),
        cache_write_tokens: add_tokens(a.cache_write_tokens, b.cache_write_tokens),
        cache_write_long_tokens: add_tokens(a.cache_write_long_tokens, b.cache_write_long_tokens),
        reasoning_tokens: add_tokens(a.reasoning_tokens, b.reasoning_tokens),
    }
}

/// Whether a status on a credentialed request says the key does not work. A redirect is not
/// followed, so it proves nothing about the key either.
fn rejected(status: u16) -> bool {
    matches!(status, 300..=399 | 401..=403) || is_retryable_status(status)
}

/// One non-streaming chat call: encode, send, read, decode. Returns the answer and its latency.
async fn chat(
    ctx: &Ctx<'_>,
    protocol: Protocol,
    request: &HubRequest,
    timeout: Duration,
) -> Result<(HubResponse, u64), AttemptError> {
    let body = encode_for(ctx.target, protocol, request, false).map_err(AttemptError::Invalid)?;
    let first = Instant::now();
    let mut retried = false;
    loop {
        // The wait of a retry counts toward the check's timeout.
        let remaining = timeout.saturating_sub(first.elapsed());
        let started_ms = now_ms();
        let started = Instant::now();
        let outcome = send_and_decode(ctx, protocol, &body, remaining, started).await;
        ctx.log.record(
            ctx.target,
            protocol.as_str(),
            started_ms,
            started,
            outcome.as_ref().ok().map(|_| 200),
            outcome.as_ref().ok().map(|(response, _)| &response.usage),
            outcome.as_ref().err(),
            false,
        );
        match outcome {
            Err(error) if !retried && wait_before_retry(ctx, &error, remaining).await => {
                retried = true;
            }
            Ok((response, _)) if retried => return Ok((response, elapsed_ms(first))),
            other => return other,
        }
    }
}

/// Longest `Retry-After` a probe waits for before its one retry.
const MAX_RETRY_WAIT: Duration = Duration::from_secs(5);

/// On a 429 whose `Retry-After` is at most five seconds (and fits the time left), waits for it
/// and returns `true`: the check retries once. `false` when there is nothing to wait for or the
/// run was stopped while waiting (the next request then fails as cancelled).
async fn wait_before_retry(ctx: &Ctx<'_>, error: &AttemptError, remaining: Duration) -> bool {
    let Some(wait) = error
        .retry_after()
        .filter(|wait| error.status() == Some(429) && *wait <= MAX_RETRY_WAIT && *wait < remaining)
    else {
        return false;
    };
    until_cancelled(ctx.cancel, tokio::time::sleep(wait))
        .await
        .is_some()
}

async fn send_and_decode(
    ctx: &Ctx<'_>,
    protocol: Protocol,
    body: &Value,
    timeout: Duration,
    started: Instant,
) -> Result<(HubResponse, u64), AttemptError> {
    let response = send_measured(&Call {
        runtime: ctx.runtime,
        target: ctx.target,
        protocol,
        stream: false,
        url: None,
        body,
        timeout,
        cancel: ctx.cancel,
    })
    .await
    .0?;
    let json = read_json(response, ctx.cancel).await?;
    let codec = codec(protocol).ok_or_else(|| {
        AttemptError::Invalid(AppError::unsupported(format!(
            "{} is not a chat protocol",
            protocol.as_str()
        )))
    })?;
    let decoded = codec
        .decode_response(&json)
        .map_err(AttemptError::Invalid)?;
    Ok((decoded, elapsed_ms(started)))
}

/// POSTs a ready JSON body (embeddings, decisions) and reads the JSON answer.
async fn post_json(
    ctx: &Ctx<'_>,
    protocol: Protocol,
    url: Option<&str>,
    body: &Value,
    timeout: Duration,
) -> Result<Value, AttemptError> {
    let started_ms = now_ms();
    let started = Instant::now();
    let outcome = async {
        let response = send_measured(&Call {
            runtime: ctx.runtime,
            target: ctx.target,
            protocol,
            stream: false,
            url,
            body,
            timeout,
            cancel: ctx.cancel,
        })
        .await
        .0?;
        read_json(response, ctx.cancel).await
    }
    .await;
    ctx.log.record(
        ctx.target,
        protocol.as_str(),
        started_ms,
        started,
        outcome.as_ref().ok().map(|_| 200),
        None,
        outcome.as_ref().err(),
        false,
    );
    outcome
}
