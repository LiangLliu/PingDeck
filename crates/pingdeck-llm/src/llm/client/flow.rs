//! The stream of one in-process call: opening with failover, then relaying committed events.
//!
//! Single responsibility: turn a [`Run`] into a `Stream` of hub events. Failover happens only
//! before the first content event (`UpstreamStream::prime`); after it a failure ends the stream
//! with a typed [`ChatError`] (mid-stream loss, idle stall, empty answer or refusal) and the text
//! already delivered stays with the consumer. Dropping the stream cancels the upstream request,
//! frees a half-open probe and logs a `canceled` row.

use std::collections::VecDeque;
use std::sync::Arc;
use std::time::Duration;

use futures_util::StreamExt;
use futures_util::stream::{self, BoxStream};

use crate::llm::failover::first_byte_timeout;
use crate::llm::failure::FailureKind;
use crate::llm::health::Outcome;
use crate::llm::hub::{HubEvent, HubFinish, HubRequest};
use crate::llm::runtime::LlmRuntime;
use crate::llm::upstream::{AttemptError, Call, UpstreamStream, send_measured};
use pingdeck_base::cancel::CancelToken;
use pingdeck_base::error::AppError;

use super::attempts::{Run, Spent};
use super::journal::{Ended, Journal};
use super::{ChatError, ChatErrorKind, ChatTarget, HealthGuard, Reshape, encode_for, shape_for};

/// No SSE event for this long ends the stream with an idle stall (the setting `idle_timeout_s`
/// may only make it shorter): 30 s. Keep-alive comments are bytes, not
/// events, and do not restart it (`UpstreamStream::next_chunk`).
const CHAT_IDLE_CAP: Duration = Duration::from_secs(30);

/// A committed stream.
pub(super) struct Flow {
    stream: Box<UpstreamStream>,
    pending: VecDeque<HubEvent>,
    guard: HealthGuard,
    journal: Journal,
    content_seen: bool,
    finish: Option<HubFinish>,
    idle: Duration,
}

// One transient state value per stream: boxing the one-shot `Opening` payload would only add an
// allocation.
#[allow(clippy::large_enum_variant)]
enum State {
    Opening {
        runtime: Arc<LlmRuntime>,
        tag: super::CallTag,
        target: ChatTarget,
        request: HubRequest,
        reshape: Option<Reshape>,
        cancel: Option<CancelToken>,
    },
    Flowing(Flow),
    Done,
}

type Item = Result<HubEvent, ChatError>;

/// The stream of a call; the upstream is opened when it is first polled.
pub(super) fn events(
    runtime: Arc<LlmRuntime>,
    tag: super::CallTag,
    target: ChatTarget,
    request: HubRequest,
    reshape: Option<Reshape>,
    cancel: Option<CancelToken>,
) -> BoxStream<'static, Item> {
    stream::unfold(
        State::Opening {
            runtime,
            tag,
            target,
            request,
            reshape,
            cancel,
        },
        step,
    )
    .boxed()
}

/// Whether `event` is content (text, reasoning or a tool call), as opposed to bookkeeping.
fn carries_content(event: &HubEvent) -> bool {
    matches!(
        event,
        HubEvent::TextDelta { .. }
            | HubEvent::ReasoningDelta { .. }
            | HubEvent::ReasoningItem { .. }
            | HubEvent::RedactedReasoning { .. }
            | HubEvent::ToolCallStart { .. }
            | HubEvent::ServerToolStart { .. }
    )
}

async fn step(state: State) -> Option<(Item, State)> {
    match state {
        State::Done => None,
        State::Opening {
            runtime,
            tag,
            target,
            request,
            reshape,
            cancel,
        } => match open(&runtime, &tag, &target, &request, reshape.as_ref(), cancel).await {
            Ok(flow) => Box::pin(step(State::Flowing(flow))).await,
            Err(error) => Some((Err(error), State::Done)),
        },
        State::Flowing(mut flow) => loop {
            if let Some(event) = flow.pending.pop_front() {
                match relay(&mut flow, event) {
                    Ok(event) => return Some((Ok(event), State::Flowing(flow))),
                    Err(error) => return Some((Err(error), State::Done)),
                }
            }
            match flow.stream.next_chunk().await {
                Some(Ok(chunk)) => flow.pending.extend(chunk.events),
                Some(Err(error)) => return Some((Err(broke(&mut flow, &error)), State::Done)),
                None => return end(flow),
            }
        },
    }
}

/// Passes one event on, noting what it means for the row and the typed failures.
fn relay(flow: &mut Flow, event: HubEvent) -> Result<HubEvent, ChatError> {
    match &event {
        HubEvent::Error { message, .. } => {
            let kind = if flow.content_seen {
                ChatErrorKind::MidStream
            } else {
                ChatErrorKind::Failed
            };
            let text = format!("upstream stream error: {message}");
            flow.guard.finish(Outcome::Failure);
            flow.journal.end(&Ended::Failed {
                status: 200,
                error: text.clone(),
                kind: FailureKind::Network,
            });
            return Err(
                ChatError::new(kind, AppError::network(text)).with_failure(FailureKind::Network)
            );
        }
        HubEvent::Usage { usage } => flow.journal.add_usage(usage),
        HubEvent::Finish { reason } => {
            flow.journal.finished(reason);
            flow.finish = Some(reason.clone());
        }
        HubEvent::ToolCallDelta { arguments, .. } => flow.journal.generated(arguments),
        other if carries_content(other) => {
            if !flow.content_seen {
                flow.journal.first_content();
            }
            flow.content_seen = true;
            if let HubEvent::TextDelta { text } | HubEvent::ReasoningDelta { text } = other {
                flow.journal.generated(text);
            }
        }
        _ => {}
    }
    Ok(event)
}

/// The stream ended on its own: success, or an answer with nothing in it.
fn end(mut flow: Flow) -> Option<(Item, State)> {
    flow.guard.finish(Outcome::Success);
    if flow.content_seen {
        flow.journal.end(&Ended::Success);
        return None;
    }
    let message = if flow.finish == Some(HubFinish::ContentFilter) {
        "the provider refused to answer (content filter)"
    } else {
        "the model returned an empty answer"
    };
    flow.journal.end(&Ended::Failed {
        status: 200,
        error: message.to_owned(),
        kind: FailureKind::EmptyOutput,
    });
    Some((
        Err(
            ChatError::new(ChatErrorKind::Refusal, AppError::network(message))
                .with_failure(FailureKind::EmptyOutput),
        ),
        State::Done,
    ))
}

/// Reading the stream failed after it was committed.
fn broke(flow: &mut Flow, error: &AttemptError) -> ChatError {
    if matches!(error, AttemptError::Cancelled) {
        flow.guard.finish(Outcome::Canceled);
        flow.journal.end(&Ended::Canceled);
        return ChatError::cancelled();
    }
    let (kind, failure, message) = if matches!(error, AttemptError::Timeout) {
        (
            ChatErrorKind::IdleStall,
            FailureKind::Timeout,
            format!("the upstream sent nothing for {} s", flow.idle.as_secs()),
        )
    } else {
        (
            ChatErrorKind::MidStream,
            error.kind(),
            format!("the stream broke off: {}", error.message()),
        )
    };
    flow.guard.finish(if error.retryable() {
        Outcome::Failure
    } else {
        Outcome::Success
    });
    flow.journal.end(&Ended::Failed {
        status: 200,
        error: message.clone(),
        kind: failure,
    });
    ChatError::new(kind, AppError::network(message)).with_failure(failure)
}

/// Opens the upstream stream: tries the plan's targets until one delivers its first content. With
/// `reshape`, each attempt sends the request adapted to its own target.
async fn open(
    runtime: &Arc<LlmRuntime>,
    tag: &super::CallTag,
    target: &ChatTarget,
    original: &HubRequest,
    reshape: Option<&Reshape>,
    cancel: Option<CancelToken>,
) -> Result<Flow, ChatError> {
    let mut run = Run::begin(
        runtime,
        tag,
        target,
        original,
        true,
        reshape.is_some(),
        cancel.clone(),
    )
    .await?;
    let first_byte = first_byte_timeout(run.first_byte, original);
    let idle = run.idle.min(CHAT_IDLE_CAP);
    while let Some(attempt) = run.next().await? {
        let uncached = original.cache.is_some()
            && super::recover::cache_refused(runtime, &attempt.target.provider_id);
        let shaped = (reshape.is_some() || uncached).then(|| {
            let mut shaped = original.clone();
            if let Some(reshape) = reshape {
                shape_for(reshape, &run.facts(&attempt.target), &mut shaped);
            }
            if uncached {
                shaped.cache = None;
            }
            shaped
        });
        let request = shaped.as_ref().unwrap_or(original);
        let (mut result, mut spent) = open_once(
            runtime,
            &attempt.target,
            request,
            first_byte,
            idle,
            cancel.clone(),
        )
        .await;
        if let Err(error) = &result
            && let Some(retry) = super::recover::retry_request(error, request).or_else(|| {
                super::recover::without_cache(runtime, &attempt.target.provider_id, error, request)
            })
        {
            run.journal
                .failed(&attempt.target, spent.protocol, spent.meta, error);
            run.journal.attempting(&attempt.target);
            (result, spent) = open_once(
                runtime,
                &attempt.target,
                &retry,
                first_byte,
                idle,
                cancel.clone(),
            )
            .await;
        }
        match result {
            Ok((stream, mut pending)) => {
                run.journal
                    .serving(&attempt.target, spent.protocol, spent.meta);
                pending.push_front(HubEvent::Served {
                    provider_id: attempt.target.provider_id.clone(),
                    model: attempt.target.model.clone(),
                    attempts: std::mem::take(&mut run.not_served),
                });
                return Ok(Flow {
                    stream: Box::new(stream),
                    pending,
                    guard: attempt.guard,
                    journal: run.journal,
                    content_seen: false,
                    finish: None,
                    idle,
                });
            }
            Err(error) => {
                if let Some(error) = run.failed(attempt, spent, error) {
                    return Err(error);
                }
            }
        }
    }
    Err(run.exhausted())
}

type Primed = (UpstreamStream, VecDeque<HubEvent>);

async fn open_once(
    runtime: &Arc<LlmRuntime>,
    target: &crate::llm::router::Target,
    request: &HubRequest,
    first_byte: Duration,
    idle: Duration,
    cancel: Option<CancelToken>,
) -> (Result<Primed, AttemptError>, Spent) {
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
    let body = match encode_for(target, protocol, request, true) {
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
        stream: true,
        url: None,
        body: &body,
        timeout: first_byte.max(Duration::from_secs(1)),
        cancel: cancel.as_ref(),
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
    let mut stream = match UpstreamStream::new(response, protocol, idle, cancel) {
        Ok(stream) => stream,
        Err(error) => return (Err(error), spent),
    };
    match stream.prime(first_byte).await {
        Ok(buffered) => {
            let pending = buffered
                .into_iter()
                .flat_map(|chunk| chunk.events)
                .collect();
            (Ok((stream, pending)), spent)
        }
        Err(error) => (Err(error), spent),
    }
}
