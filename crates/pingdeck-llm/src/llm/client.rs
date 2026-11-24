//! The chat client API: call any configured provider model through the hub format.
//!
//! Single responsibility: let in-process callers (the agent harness, the model tests) talk to an
//! alias or a provider model with [`HubRequest`] in and [`HubResponse`] / [`HubEvent`]s out,
//! whatever protocol the provider speaks. Routing, failover, `Retry-After`, the circuit breaker and
//! the first-byte timeout are the gateway's rules (`router::plan`, `failover`, `health`): a chat
//! session fails over exactly like a gateway request, with the settings in force when the call
//! starts. Every call writes one `agent` row (with its attempt list) to the runtime's log writer,
//! so Chat is logged with the gateway stopped.
//!
//! Failover applies until the first content arrives. A stream that fails after that point ends
//! with a typed error ([`ChatErrorKind`]); retrying a half-delivered answer would duplicate text.

mod attempts;
mod flow;
mod journal;
pub(crate) mod recover;
mod reshape;

use std::sync::Arc;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::stream::BoxStream;
use serde_json::Value;

use crate::llm::adapters::{EncodeCtx, codec};
use crate::llm::events::{announce_health, keys_changed_soon};
use crate::llm::failover::{Failure, FailureObservation, FailureScope};
use crate::llm::failure::FailureKind;
use crate::llm::health::{CircuitConfig, HealthBook, Outcome};
use crate::llm::hub::{HubEvent, HubRequest, HubResponse};
use crate::llm::router::Target;
use crate::llm::runtime::LlmRuntime;
use crate::llm::store::ModelRef;
use crate::llm::upstream::{AttemptError, Call, bad_body, read_json, send_measured};
use crate::llm::wire::Protocol;
use pingdeck_base::cancel::CancelToken;
use pingdeck_base::error::AppError;

pub(crate) use attempts::retry_wait;
use attempts::{Run, Spent};
use journal::Ended;
pub(crate) use journal::SkippedAttribution;
pub use reshape::{Reshape, TargetFacts, shape_for};

/// Whole-request timeout of a non-streaming call.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// What to call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ChatTarget {
    /// A model reference (`provider/model`, by name or id, or a legacy alias name), resolved
    /// with failover over the model and then the caller's own fallback chain (model references,
    /// see [`crate::llm::router::RoutingSnapshot::plan_with_fallbacks`]); the gateway rules do not apply.
    Named {
        /// The model reference.
        name: String,
        /// The caller's fallback chain, in order (empty: none).
        fallbacks: Vec<String>,
    },
    /// One provider model, optionally one key; no fallback chain.
    Pinned {
        /// Provider id.
        provider_id: String,
        /// A specific key, or `None` for the provider's enabled keys in order.
        key_id: Option<String>,
        /// Upstream model id.
        model: String,
    },
}

/// The distinct ways a call or stream can fail, so Chat can word each.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatErrorKind {
    /// The call failed before any content: unresolvable name, every attempt failed, a client error.
    Failed,
    /// The stream broke off after content was delivered; the partial text is the consumer's, and
    /// nothing was retried on another target.
    MidStream,
    /// The answer was empty or the provider refused (content filter).
    Refusal,
    /// 401 or 403 and no other key or target was left.
    KeyRejected,
    /// No content event for 30 s (or `idle_timeout_s` when shorter).
    IdleStall,
    /// The caller cancelled.
    Cancelled,
}

/// A failure of a call, typed.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatError {
    /// Which of the distinct failures it is.
    pub kind: ChatErrorKind,
    /// The error as the use-case layer reports it.
    pub error: AppError,
    /// The failure category of the last attempt, when there was one (the run loop tells an
    /// offline provider from a rejected key by it).
    pub failure: Option<FailureKind>,
    /// Every attempt that failed before the call ended, in order; empty when the failure was not
    /// the end of a walk over targets (a stream that broke after content, a cancellation).
    pub attempts: Vec<AttemptSummary>,
    /// HTTP status of the last attempt, when the upstream answered.
    pub status: Option<u16>,
    /// The upstream's `Retry-After` on the last attempt, in milliseconds.
    pub retry_after_ms: Option<u64>,
    /// What the last failure was about, when an attempt was judged.
    pub scope: Option<FailureScope>,
    /// The last attempt was already an in-place retry of the same target: the run loop must not
    /// retry this error again.
    pub retried: bool,
}

/// One failed or skipped attempt of a call.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct AttemptSummary {
    /// `Provider/model` of the attempt.
    pub target: String,
    /// What went wrong (secrets already redacted by the transport).
    pub reason: String,
    /// The attempt ran and failed in a way a later retry may cure (5xx, 408, 425, a timeout or a
    /// dropped connection); `None` for a target that was skipped.
    pub transient: Option<bool>,
}

impl ChatError {
    /// Whether the run loop may make the whole call again: at least one attempt ran and every
    /// one that ran failed transiently.
    #[must_use]
    pub fn all_transient(&self) -> bool {
        let mut ran = self.attempts.iter().filter_map(|attempt| attempt.transient);
        ran.clone().next().is_some() && ran.all(|transient| transient)
    }
}

impl ChatError {
    pub fn new(kind: ChatErrorKind, error: AppError) -> Self {
        Self {
            kind,
            error,
            failure: None,
            attempts: Vec::new(),
            status: None,
            retry_after_ms: None,
            scope: None,
            retried: false,
        }
    }

    /// This error with what the rules judged about the attempt that caused it.
    #[must_use]
    pub(crate) fn with_judgement(mut self, failure: &Failure) -> Self {
        self.failure = Some(failure.kind);
        self.status = failure.status;
        self.retry_after_ms = failure.retry_after_ms;
        self.scope = Some(failure.scope);
        self.retried = failure.retried;
        self
    }

    /// This error with the failure category of the attempt that caused it.
    #[must_use]
    pub(crate) fn with_failure(mut self, failure: FailureKind) -> Self {
        self.failure = Some(failure);
        self
    }

    /// This error with the attempts of the walk that ended in it.
    #[must_use]
    pub fn with_attempts(mut self, attempts: Vec<AttemptSummary>) -> Self {
        self.attempts = attempts;
        self
    }
}

/// Whether a judged failure may pass if the same call is made again a few seconds later: a
/// provider fault, a rate limit, a timeout or a connection that broke, never a rejected key,
/// quota exhaustion or a request the upstream called wrong.
pub(crate) fn is_transient(failure: &Failure) -> bool {
    match failure.kind {
        FailureKind::ProviderError
        | FailureKind::RateLimit
        | FailureKind::Network
        | FailureKind::Timeout => true,
        FailureKind::Unknown => matches!(failure.status, Some(408 | 425 | 500..=599)),
        _ => false,
    }
}

impl From<ChatError> for AppError {
    fn from(error: ChatError) -> Self {
        error.error
    }
}

/// Encodes a hub request for `protocol` toward `target`.
///
/// # Errors
/// [`AppError::Unsupported`] for a non-chat protocol, or the codec's validation error.
pub(crate) fn encode_for(
    target: &Target,
    protocol: Protocol,
    request: &HubRequest,
    stream: bool,
) -> Result<Value, AppError> {
    let codec = codec(protocol).ok_or_else(|| {
        AppError::unsupported(format!("{} is not a chat protocol", protocol.as_str()))
    })?;
    let mut request = request.clone();
    request.model.clone_from(&target.model);
    request.stream = stream;
    // Messages requires `max_tokens`; the model's own cap beats the codec's fixed default.
    if protocol == Protocol::AnthropicMessages && request.max_tokens.is_none() {
        request.max_tokens = target.max_output;
    }
    request.max_tokens = capped_output(request.max_tokens, target.max_output);
    codec.encode_request(
        &request,
        &EncodeCtx {
            quirks: &target.quirks,
        },
    )
}

/// `asked` lowered to the target's output cap `cap` when it is above it: a fallback to a model
/// with a smaller cap would otherwise answer 400 for a request it can serve.
#[must_use]
pub(crate) fn capped_output(asked: Option<u32>, cap: Option<u32>) -> Option<u32> {
    match (asked, cap) {
        (Some(asked), Some(cap)) if cap > 0 && asked > cap => Some(cap),
        _ => asked,
    }
}

/// What an agent's model call is for (`llm_request_log.purpose`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallPurpose {
    /// A step of the run's loop.
    Step,
    /// A compaction summary.
    Compaction,
    /// A chat title.
    Title,
    /// A step of a sub-agent of the run.
    Subagent,
}

impl CallPurpose {
    /// The stored word.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Step => "step",
            Self::Compaction => "compaction",
            Self::Title => "title",
            Self::Subagent => "subagent",
        }
    }
}

/// The run key of one agent call: its log row carries them.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CallMeta {
    /// The top-level run (a sub-agent's calls carry their root run's id).
    pub run_id: String,
    /// The assistant message the call writes (one per step); `None` for a summary or a title.
    pub message_id: Option<String>,
    pub purpose: CallPurpose,
}

/// Whose row a call writes: the chat and the run key.
#[derive(Debug, Clone, Default)]
pub(crate) struct CallTag {
    pub(crate) session: Option<String>,
    pub(crate) meta: Option<CallMeta>,
}

/// Cheap, cloneable handle for in-process chat calls.
#[derive(Clone)]
pub struct LlmClient {
    runtime: Arc<LlmRuntime>,
    /// The chat session and run key whose calls this handle logs (the `agent` rows).
    tag: CallTag,
}

impl LlmClient {
    /// A client over `runtime`.
    #[must_use]
    pub fn new(runtime: Arc<LlmRuntime>) -> Self {
        Self {
            runtime,
            tag: CallTag::default(),
        }
    }

    /// The same client logging its calls under chat session `session_id`.
    #[must_use]
    pub fn for_session(&self, session_id: impl Into<String>) -> Self {
        Self {
            runtime: Arc::clone(&self.runtime),
            tag: CallTag {
                session: Some(session_id.into()),
                meta: self.tag.meta.clone(),
            },
        }
    }

    /// The same client logging its calls with the run key `meta`.
    #[must_use]
    pub fn for_call(&self, meta: &CallMeta) -> Self {
        Self {
            runtime: Arc::clone(&self.runtime),
            tag: CallTag {
                session: self.tag.session.clone(),
                meta: Some(meta.clone()),
            },
        }
    }

    /// Sends `request` and returns the complete answer. `request.stream` is ignored; the call is
    /// not streamed.
    ///
    /// Walks the plan with the gateway's failover rules (key rotation on 401/402/403/429, next
    /// target on 5xx, timeouts, connection errors, model-not-found and context-length errors, one
    /// `Retry-After` wait when nothing else is left; at most `1 + max_retries` attempts) and
    /// returns any other client error unchanged.
    ///
    /// # Errors
    /// [`AppError::NotFound`] / [`AppError::Validation`] when the target cannot be resolved,
    /// [`AppError::Cancelled`] when `cancel` fires, [`AppError::Network`] with the last upstream
    /// failure when every attempt failed (or "No target available for 'x'. The earliest retry is
    /// in N s." when every circuit is open).
    pub async fn complete(
        &self,
        target: &ChatTarget,
        request: HubRequest,
        cancel: Option<&CancelToken>,
    ) -> Result<HubResponse, AppError> {
        self.complete_served(target, request, cancel)
            .await
            .map(|(response, _)| response)
    }

    /// [`LlmClient::complete`], with the provider model that answered (a fallback may differ from
    /// the requested one).
    ///
    /// # Errors
    /// As [`LlmClient::complete`].
    pub async fn complete_served(
        &self,
        target: &ChatTarget,
        request: HubRequest,
        cancel: Option<&CancelToken>,
    ) -> Result<(HubResponse, ModelRef), AppError> {
        let mut run = Run::begin(
            &self.runtime,
            &self.tag,
            target,
            &request,
            false,
            false,
            cancel.cloned(),
        )
        .await?;
        while let Some(attempt) = run.next().await? {
            let uncached = (request.cache.is_some()
                && recover::cache_refused(&self.runtime, &attempt.target.provider_id))
            .then(|| HubRequest {
                cache: None,
                ..request.clone()
            });
            let request = uncached.as_ref().unwrap_or(&request);
            let (mut result, mut spent) =
                complete_once(&self.runtime, &attempt.target, request, cancel).await;
            if let Err(error) = &result
                && let Some(retry) = recover::retry_request(error, request).or_else(|| {
                    recover::without_cache(
                        &self.runtime,
                        &attempt.target.provider_id,
                        error,
                        request,
                    )
                })
            {
                run.journal
                    .failed(&attempt.target, spent.protocol, spent.meta, error);
                run.journal.attempting(&attempt.target);
                (result, spent) =
                    complete_once(&self.runtime, &attempt.target, &retry, cancel).await;
            }
            match result {
                Ok(response) => {
                    let mut guard = attempt.guard;
                    guard.finish(Outcome::Success);
                    run.journal
                        .serving(&attempt.target, spent.protocol, spent.meta);
                    run.journal.add_usage(&response.usage);
                    run.journal.finished(&response.finish);
                    run.journal.end(&Ended::Success);
                    let served = ModelRef {
                        provider_id: attempt.target.provider_id.clone(),
                        model: attempt.target.model.clone(),
                    };
                    return Ok((response, served));
                }
                Err(error) => {
                    if let Some(error) = run.failed(attempt, spent, error) {
                        return Err(error.into());
                    }
                }
            }
        }
        Err(run.exhausted().into())
    }

    /// Sends `request` as a stream of [`HubEvent`]s; failures are typed ([`ChatError`]).
    ///
    /// The stream yields `Err` once and ends when the call cannot start (every attempt failed),
    /// the upstream reports an error, a read fails, the stream stalls or `cancel` fires. Dropping
    /// the stream cancels the upstream request. Failover happens before the first content event
    /// only.
    pub fn stream_typed(
        &self,
        target: ChatTarget,
        request: HubRequest,
        cancel: Option<CancelToken>,
    ) -> BoxStream<'static, Result<HubEvent, ChatError>> {
        flow::events(
            Arc::clone(&self.runtime),
            self.tag.clone(),
            target,
            request,
            None,
            cancel,
        )
    }

    /// [`LlmClient::stream_typed`] with `request` adapted to each attempt's target before it is
    /// sent ([`shape_for`]); a target known not to read images is tried, without them, after every
    /// target that reads them.
    pub fn stream_reshaped(
        &self,
        target: ChatTarget,
        request: HubRequest,
        reshape: Reshape,
        cancel: Option<CancelToken>,
    ) -> BoxStream<'static, Result<HubEvent, ChatError>> {
        flow::events(
            Arc::clone(&self.runtime),
            self.tag.clone(),
            target,
            request,
            Some(reshape),
            cancel,
        )
    }

    /// [`LlmClient::stream_typed`] with the failures flattened to [`AppError`].
    pub fn stream(
        &self,
        target: ChatTarget,
        request: HubRequest,
        cancel: Option<CancelToken>,
    ) -> BoxStream<'static, Result<HubEvent, AppError>> {
        self.stream_typed(target, request, cancel)
            .map(|item| item.map_err(AppError::from))
            .boxed()
    }
}

async fn complete_once(
    runtime: &Arc<LlmRuntime>,
    target: &Target,
    request: &HubRequest,
    cancel: Option<&CancelToken>,
) -> (Result<HubResponse, AttemptError>, Spent) {
    let needs = crate::llm::router::RequestNeeds::from_hub(request);
    let Some(protocol) = target.chat_protocol_for(&needs) else {
        return (
            Err(AttemptError::Invalid(AppError::unsupported(format!(
                "provider {} speaks no chat protocol",
                target.provider_name
            )))),
            Spent::unsent(None),
        );
    };
    let body = match encode_for(target, protocol, request, false) {
        Ok(body) => body,
        Err(error) => {
            return (
                Err(AttemptError::Invalid(error)),
                Spent::unsent(Some(protocol)),
            );
        }
    };
    let call = Call {
        runtime,
        target,
        protocol,
        stream: false,
        url: None,
        body: &body,
        timeout: REQUEST_TIMEOUT,
        cancel,
    };
    let (sent, meta) = send_measured(&call).await;
    let spent = Spent {
        protocol: Some(protocol),
        meta,
    };
    let response = match sent {
        Ok(response) => response,
        Err(error) => return (Err(error), spent),
    };
    let json = match read_json(response, cancel).await {
        Ok(json) => json,
        Err(error) => return (Err(error), spent),
    };
    let decoded = match codec(protocol) {
        Some(codec) => codec
            .decode_response(&json)
            .map_err(|error| bad_body(&error)),
        None => Err(AttemptError::Invalid(AppError::internal(
            "chat codec missing",
        ))),
    };
    (decoded, spent)
}

/// Records an attempt's health outcome exactly once, even when the caller drops the future or the
/// consumer drops the stream early (then it counts as cancelled, which frees a half-open probe).
/// The key's own record (last use, last error, cooldown) goes to the runtime's key book.
pub(crate) struct HealthGuard {
    runtime: Arc<LlmRuntime>,
    provider_id: String,
    key_id: String,
    model: String,
    key_cooldown: bool,
    started: Instant,
    circuit: CircuitConfig,
    recorded: bool,
}

impl HealthGuard {
    /// Starts timing an attempt of `target` with the circuit thresholds the call started with;
    /// the caller already holds a [`crate::llm::health::Permit`].
    pub(crate) fn with_config(
        runtime: &Arc<LlmRuntime>,
        target: &Target,
        circuit: CircuitConfig,
    ) -> Self {
        Self {
            runtime: Arc::clone(runtime),
            provider_id: target.provider_id.clone(),
            key_id: target.key_id.clone(),
            model: target.model.clone(),
            key_cooldown: target.key_cooldown,
            started: Instant::now(),
            circuit,
            recorded: false,
        }
    }

    /// Records a failed attempt (once): the key's last error and any cooldown it earns, then the
    /// health outcome ([`FailureObservation::health_outcome`]). `line` is the redacted error line
    /// ([`crate::llm::key_book::error_line`]). An in-place `retry` of an attempt that already
    /// counted is not counted again: one retry is one circuit failure.
    pub(crate) fn fail(&mut self, observation: &FailureObservation, line: String, retry: bool) {
        if self.recorded {
            return;
        }
        if self.runtime.keys.failed(
            &self.key_id,
            &self.model,
            observation,
            line,
            self.key_cooldown,
        ) {
            keys_changed_soon(&self.runtime, &self.provider_id);
        }
        let outcome = observation.health_outcome(self.key_cooldown);
        self.record(if retry && outcome == Outcome::Failure {
            Outcome::Canceled
        } else {
            outcome
        });
    }

    /// Records a fault of this side, which reached no upstream: health counts it as a success (the
    /// upstream said nothing about itself), the key book only as a use.
    pub(crate) fn fault(&mut self) {
        if self.recorded {
            return;
        }
        let _ = self.runtime.keys.used(&self.key_id, &self.model, false);
        self.record(Outcome::Success);
    }

    /// Records `outcome` (once).
    pub(crate) fn finish(&mut self, outcome: Outcome) {
        if self.recorded {
            return;
        }
        if self
            .runtime
            .keys
            .used(&self.key_id, &self.model, outcome == Outcome::Success)
        {
            keys_changed_soon(&self.runtime, &self.provider_id);
        }
        self.record(outcome);
    }

    fn record(&mut self, outcome: Outcome) {
        self.recorded = true;
        let health: &HealthBook = &self.runtime.health;
        if health.record(
            &self.key_id,
            &self.model,
            outcome,
            self.started.elapsed(),
            self.circuit,
        ) {
            announce_health(&self.runtime);
        }
    }
}

impl Drop for HealthGuard {
    fn drop(&mut self) {
        self.finish(Outcome::Canceled);
    }
}
