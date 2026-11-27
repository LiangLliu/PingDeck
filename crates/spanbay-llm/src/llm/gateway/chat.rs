//! Forwarding a client chat request to the best upstream: plan, prepare, send, fail over, answer.
//!
//! Single responsibility: the request pipeline of the chat routes. The name is planned with the
//! client's scope and the request's needs (`RoutingSnapshot::plan`); a [`Walk`] then names the key
//! to try next under the failover rules while this module prepares and sends the attempt: a
//! same-protocol target gets the client's JSON byte for byte except `model` and an output cap
//! above the target's (a body that echoes another protocol's signatures loses only those
//! blocks), any other goes through the hub format with the field classes applied (`fields`):
//! refused features skip the target, dropped fields become translation notes and
//! `x-spanbay-dropped`. A Messages target reached from another protocol gets prompt-cache
//! breakpoints after the system prompt and tools and after the last message, unless the provider
//! refused cache hints this session; a 400 that names `cache_control` sends the request once more
//! to the same target without them and marks the provider. A stream commits to a target at its
//! first content event;
//! before that a failure fails over.
//!
//! The response `model` is the upstream's in passthrough (its bytes are kept) and the name the
//! client asked for when the gateway re-encodes the answer.

use std::time::Duration;

use bytes::Bytes;
use hyper::Response;
use hyper::header::HeaderValue;
use serde_json::Value;

use crate::llm::adapters::{ChatCodec, InboundHint, StreamEncoder, codec};
use crate::llm::capture::{BoundedBody, CaptureMode, bound_body};
use crate::llm::capture_record::{TranslationKind, TranslationNote};
use crate::llm::client::recover::{cache_refused, refuse_cache, refuses_cache_hint};
use crate::llm::client::{capped_output, encode_for};
use crate::llm::failover::enables_reasoning;
use crate::llm::hub::{CacheBreakpoint, CacheHint, HubEvent, HubRequest, HubUsage};
use crate::llm::router::{PlanError, RequestNeeds, Target};
use crate::llm::runtime::LlmRuntime;
use crate::llm::upstream::{
    AttemptError, Call, UpstreamStream, bad_body, parse_json, read_body, send_measured_bytes,
};
use crate::llm::wire::{Protocol, rewrite_model};
use spanbay_base::cancel::CancelToken;
use spanbay_base::error::AppError;

use super::ending::{exhausted, fail, plan_failed, stopped};
use super::exchange::Sent;
use super::fields::{
    BETA_HEADER, conform, dropped_header, drops, feature_of_error, refusal, refusal_message,
};
use super::journal::{Ended, Journal};
use super::passthrough::{cap_output, strip_signature_tags, with_model};
use super::pump::Pump;
use super::reply::{bytes_response, with_dropped};
use super::server::{Context, GatewayBody, status_of};
use super::walk::{Gate, Refusal, Spent, Stop, Walk};

/// The Messages API version header a passthrough forwards as the client sent it.
const VERSION_HEADER: &str = "anthropic-version";

/// Whole-request timeout of a non-streaming upstream call.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);

/// A request as the gateway received it, from the wire or from a replay.
pub(super) struct Inbound {
    /// The protocol the client speaks (the route).
    pub(super) protocol: Protocol,
    /// What the URL says besides the body (Gemini's model and stream flag).
    pub(super) hint: InboundHint,
    /// The body exactly as received.
    pub(super) bytes: Bytes,
    /// The body parsed.
    pub(super) json: Value,
    /// The body holds a `pdk1.` signature tag: a same-protocol forward strips the tagged blocks.
    pub(super) tagged: bool,
    /// The request headers as received, for the capture and the `anthropic-beta` rule.
    pub(super) headers: Vec<(String, String)>,
    /// Replay on the served target: the provider and model to call, whatever the body names.
    pub(super) pin: Option<(String, String)>,
    /// The model the call is routed to when the client's own name was mapped to its default
    /// model (`unknown_model_to_default`); the log and the answer keep the client's name.
    pub(super) routed: Option<String>,
    /// Stops the call (a replay's Stop button); a wire request ends by being dropped.
    pub(super) cancel: Option<CancelToken>,
}

impl Inbound {
    /// The model the client asked for: the body's, else the URL's.
    pub(super) fn requested(&self) -> String {
        self.json
            .get("model")
            .and_then(Value::as_str)
            .map(str::to_owned)
            .or_else(|| self.hint.model.clone())
            .unwrap_or_default()
    }

    /// The model name the call is planned with: the mapped default, else the client's name.
    pub(super) fn route_name(&self) -> String {
        self.routed.clone().unwrap_or_else(|| self.requested())
    }

    /// The value of a client header, if it sent one.
    fn header(&self, wanted: &str) -> Option<&str> {
        self.headers
            .iter()
            .find(|(name, _)| name.eq_ignore_ascii_case(wanted))
            .map(|(_, value)| value.as_str())
    }

    /// The value of the client's `anthropic-beta` header, if it sent one.
    fn beta(&self) -> Option<&str> {
        self.header(BETA_HEADER)
    }
}

/// Whether the capture policy can want this exchange (`Off` never does for gateway rows).
pub(super) fn capture_wanted(context: &Context) -> bool {
    context.llm.settings().ai.gateway.capture_mode != CaptureMode::Off
}

/// How one provider model takes the request.
struct Prepared {
    protocol: Protocol,
    body: Bytes,
    /// The body as stored in the capture; `None` when it is the client's except `model`.
    bounded: Option<BoundedBody>,
    /// A verbatim forward: the response is the upstream's bytes.
    same: bool,
    notes: Vec<TranslationNote>,
    hub: Option<Value>,
    /// The client's `anthropic-beta` value to forward to a Messages target.
    beta: Option<String>,
    /// The client's `anthropic-version`, which wins over the provider's pinned one.
    version: Option<String>,
    /// A re-encoded body that carries prompt-cache breakpoints: an upstream that refuses them
    /// gets the request once more without them.
    hinted: bool,
}

enum Prep {
    Ready(Prepared),
    Refused(Refusal),
    /// The request is wrong for this protocol (a codec's validation error): the client's 400.
    Invalid(AppError),
}

/// The note of an output cap lowered to the target's.
fn capped_note(field: &str, asked: u64, cap: u32) -> TranslationNote {
    TranslationNote {
        field: field.to_owned(),
        kind: TranslationKind::Changed,
        reason: format!("lowered from {asked} to the target's output limit {cap}"),
    }
}

/// Prepares the request once per provider model and tells the walk which models cannot take it.
struct Preparer<'a> {
    llm: &'a LlmRuntime,
    inbound: &'a Inbound,
    decoded: &'a Result<HubRequest, AppError>,
    stream: bool,
    capture: bool,
    wants_hub: bool,
    cache: Vec<Option<Prep>>,
}

impl Preparer<'_> {
    fn prepared(&mut self, group: usize, target: &Target) -> &Prep {
        let prep = match self.cache[group].take() {
            Some(prep) => prep,
            None => self.build(target),
        };
        self.cache[group].insert(prep)
    }

    fn refused(&self, feature: &str, outbound: Protocol, target: &Target) -> Prep {
        Prep::Refused(Refusal {
            reason: format!("cannot carry {feature}"),
            message: refusal_message(
                feature,
                self.inbound.protocol,
                outbound,
                &target.provider_name,
                &target.model,
            ),
        })
    }

    fn verbatim(&self, protocol: Protocol, target: &Target) -> Prep {
        let inbound = self.inbound;
        // Signatures another protocol produced would be refused by this upstream: only those
        // blocks go, the rest of the body (its `cache_control` too) is forwarded as it is.
        let stripped = inbound.tagged.then(|| {
            let mut json = inbound.json.clone();
            strip_signature_tags(protocol, &mut json);
            json
        });
        let (bytes, json) = match &stripped {
            Some(json) => (
                Bytes::from(serde_json::to_vec(json).unwrap_or_default()),
                json,
            ),
            None => (inbound.bytes.clone(), &inbound.json),
        };
        let body = match protocol {
            // Gemini names the model in the URL, not the body.
            Protocol::Gemini => Some(bytes.clone()),
            _ => with_model(&bytes, &target.model).map(Bytes::from),
        };
        let mut body = body.unwrap_or_else(|| {
            let mut json = json.clone();
            rewrite_model(protocol, &mut json, &target.model);
            Bytes::from(serde_json::to_vec(&json).unwrap_or_default())
        });
        let mut notes = Vec::new();
        if let Some(cap) = target.max_output
            && let Some((capped, field, asked)) = cap_output(protocol, &body, json, cap)
        {
            body = Bytes::from(capped);
            notes.push(capped_note(field, asked, cap));
        }
        let changed = stripped.is_some() || !notes.is_empty();
        Prep::Ready(Prepared {
            protocol,
            bounded: (self.capture && changed).then(|| bound_body(&body)),
            body,
            same: true,
            notes,
            hub: None,
            beta: self.beta_for(protocol),
            version: self.version_for(protocol),
            hinted: false,
        })
    }

    fn beta_for(&self, protocol: Protocol) -> Option<String> {
        self.messages_header(protocol, BETA_HEADER)
    }

    fn version_for(&self, protocol: Protocol) -> Option<String> {
        self.messages_header(protocol, VERSION_HEADER)
    }

    /// A client header carried to a Messages target when the client also speaks Messages.
    fn messages_header(&self, protocol: Protocol, name: &str) -> Option<String> {
        (protocol == Protocol::AnthropicMessages && self.inbound.protocol == protocol)
            .then(|| self.inbound.header(name).map(str::to_owned))
            .flatten()
    }

    fn build(&self, target: &Target) -> Prep {
        let inbound = self.inbound;
        let Some(protocol) = target.chat_protocol(Some(inbound.protocol)) else {
            return Prep::Invalid(AppError::unsupported("the provider has no chat endpoint"));
        };
        if protocol == inbound.protocol {
            return self.verbatim(protocol, target);
        }
        if let Some(feature) = refusal(inbound.protocol, &inbound.json) {
            return self.refused(&feature, protocol, target);
        }
        let request = match self.decoded {
            Ok(request) => request,
            Err(error @ AppError::Unsupported { .. }) => {
                return self.refused(&feature_of_error(error.message()), protocol, target);
            }
            Err(error) => return Prep::Invalid(error.clone()),
        };
        let cached;
        let request = if protocol != Protocol::AnthropicMessages {
            request
        } else if cache_refused(self.llm, &target.provider_id) {
            // The provider refused cache hints this session: none goes, the client's included.
            if request.cache.is_some() {
                cached = HubRequest {
                    cache: None,
                    ..request.clone()
                };
                &cached
            } else {
                request
            }
        } else if request
            .cache
            .as_ref()
            .is_none_or(|cache| cache.breakpoints.is_empty())
        {
            // The client's protocol has no breakpoints to translate: the gateway places the two
            // that pay off for a conversation (the stable prefix, and the turn so far).
            cached = HubRequest {
                cache: Some(CacheHint {
                    breakpoints: vec![
                        CacheBreakpoint::SystemAndTools,
                        CacheBreakpoint::LastMessage,
                    ],
                    key: request.cache.as_ref().and_then(|cache| cache.key.clone()),
                }),
                ..request.clone()
            };
            &cached
        } else {
            request
        };
        let hinted = protocol == Protocol::AnthropicMessages
            && request
                .cache
                .as_ref()
                .is_some_and(|cache| !cache.breakpoints.is_empty());
        let mut encoded = match encode_for(target, protocol, request, self.stream) {
            Ok(encoded) => encoded,
            Err(error @ AppError::Unsupported { .. }) => {
                return self.refused(&feature_of_error(error.message()), protocol, target);
            }
            Err(error) => return Prep::Invalid(error),
        };
        conform(protocol, &mut encoded);
        let Ok(body) = serde_json::to_vec(&encoded) else {
            return Prep::Invalid(AppError::internal("the request could not be serialised"));
        };
        let cross = protocol != inbound.protocol;
        let mut notes = drops(
            inbound.protocol,
            protocol,
            &inbound.json,
            cross && inbound.beta().is_some(),
            request,
            &encoded,
        );
        if let (Some(asked), Some(cap)) = (request.max_tokens, target.max_output)
            && capped_output(Some(asked), Some(cap)) != Some(asked)
        {
            notes.push(capped_note("max_tokens", u64::from(asked), cap));
        }
        Prep::Ready(Prepared {
            protocol,
            bounded: self.capture.then(|| bound_body(&body)),
            body: Bytes::from(body),
            same: false,
            notes,
            hub: self
                .wants_hub
                .then(|| serde_json::to_value(request).ok())
                .flatten(),
            beta: self.beta_for(protocol),
            version: self.version_for(protocol),
            hinted,
        })
    }
}

impl Gate for Preparer<'_> {
    fn refuse(&mut self, group: usize, target: &Target) -> Option<Refusal> {
        match self.prepared(group, target) {
            Prep::Refused(refusal) => Some(Refusal {
                reason: refusal.reason.clone(),
                message: refusal.message.clone(),
            }),
            _ => None,
        }
    }
}

/// The target with the client's `anthropic-version` in place of the provider's pinned one and
/// its `anthropic-beta` merged into the provider's own extra header.
fn with_client_headers(target: &Target, beta: Option<&str>, version: Option<&str>) -> Target {
    let mut target = target.clone();
    if let Some(version) = version {
        target.quirks.anthropic_version = Some(version.to_owned());
    }
    let Some(beta) = beta else {
        return target;
    };
    let extras = &mut target.quirks.extra_headers;
    let existing = extras
        .keys()
        .find(|name| name.eq_ignore_ascii_case(BETA_HEADER))
        .cloned();
    match existing {
        Some(name) => {
            let joined = format!("{},{beta}", extras.get(&name).map_or("", String::as_str));
            extras.insert(name, joined);
        }
        None => {
            extras.insert(BETA_HEADER.to_owned(), beta.to_owned());
        }
    }
    target
}

/// What one successful attempt delivered.
enum Delivered {
    Json {
        body: Bytes,
        usage: HubUsage,
        text: String,
        /// The upstream's own bytes, when the client's differ.
        upstream: Option<Bytes>,
    },
    Stream(UpstreamStream, Vec<crate::llm::upstream::Chunk>),
}

/// The `chat` entry point: one request of a chat protocol.
pub(super) async fn chat(context: &Context, inbound: Inbound) -> Response<GatewayBody> {
    let protocol = inbound.protocol;
    let requested = inbound.requested();
    let route_name = inbound.route_name();
    let stream = inbound.hint.stream
        || inbound
            .json
            .get("stream")
            .and_then(Value::as_bool)
            .unwrap_or(false);
    let mut journal = Journal::new(
        context,
        &inbound,
        &requested,
        stream,
        capture_wanted(context),
    );
    if requested.trim().is_empty() {
        let message = "the request has no model";
        let kind = crate::llm::failure::FailureKind::Unknown;
        return fail(&mut journal, protocol, 400, message, None, kind);
    }
    let Some(codec_in) = codec(protocol) else {
        let message = "no codec for this endpoint";
        let kind = crate::llm::failure::FailureKind::Unknown;
        return fail(&mut journal, protocol, 500, message, None, kind);
    };
    let snapshot = match context.llm.snapshot().await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            let kind = crate::llm::failure::FailureKind::Unknown;
            return fail(
                &mut journal,
                protocol,
                status_of(&error),
                error.message(),
                None,
                kind,
            );
        }
    };
    let mut hint = inbound.hint.clone();
    hint.stream = stream;
    // A body the codec cannot read still passes verbatim to a same-protocol target; the error
    // only matters for the targets that need the hub format.
    let decoded = codec_in.decode_request(&inbound.json, &hint);
    let needs = decoded
        .as_ref()
        .map(RequestNeeds::from_hub)
        .unwrap_or_default();
    let identity = context.identity();
    let scope = |name: &str| identity.allows(name);
    let planned = match &inbound.pin {
        Some((provider, model)) => snapshot.plan_pinned_explained(
            provider,
            None,
            model,
            &needs,
            &context.llm.health,
            &context.llm.keys,
        ),
        None => snapshot.plan_explained(
            &route_name,
            &needs,
            Some(&scope),
            &context.llm.health,
            &context.llm.keys,
        ),
    };
    let plan = match planned {
        Ok(plan) => plan,
        Err(failure) => {
            let close = match &failure.error {
                PlanError::UnknownModel(name) => snapshot.close_names(name),
                _ => Vec::new(),
            };
            return plan_failed(&mut journal, protocol, &failure, &close);
        }
    };
    journal.planned(&plan);
    // The values the gateway started with: a Failover change applies after a restart.
    let failover = context.shared.failover;
    let first_byte = if enables_reasoning(protocol, &inbound.json) {
        failover
            .first_byte
            .max(context.shared.limits.reasoning_first_byte)
    } else {
        failover.first_byte
    };
    let max_retries = u8::try_from(failover.max_attempts.saturating_sub(1)).unwrap_or(u8::MAX);
    let mut gate = Preparer {
        llm: &context.llm,
        inbound: &inbound,
        decoded: &decoded,
        stream,
        capture: journal.capturing(),
        wants_hub: journal.wants_hub(),
        cache: (0..plan.groups.len()).map(|_| None).collect(),
    };
    let mut walk = Walk::new(plan, max_retries, failover.circuit);
    let cancel = inbound.cancel.as_ref();
    // The attempt to send once more, without cache hints, to the upstream that refused them.
    let mut again = None;
    loop {
        let (permitted, may_drop_hint) = match again.take() {
            Some(permitted) => (permitted, false),
            None => match walk.next(context, &mut journal, cancel, &mut gate).await {
                Ok(Some(permitted)) => (permitted, true),
                Ok(None) => return exhausted(&mut journal, context, protocol, &mut walk).await,
                Err(()) => return stopped(&mut journal, context, protocol, Stop::Canceled).await,
            },
        };
        let prepared = match gate.prepared(permitted.group, &permitted.target) {
            Prep::Ready(prepared) => prepared,
            Prep::Invalid(error) => {
                let mut permitted = permitted;
                // The request, not the upstream, is at fault; free a half-open probe slot.
                permitted
                    .guard
                    .finish(crate::llm::health::Outcome::Canceled);
                let kind = crate::llm::failure::FailureKind::Unknown;
                return fail(
                    &mut journal,
                    protocol,
                    status_of(error),
                    error.message(),
                    None,
                    kind,
                );
            }
            Prep::Refused(_) => continue,
        };
        let client_target;
        let target = if prepared.beta.is_some() || prepared.version.is_some() {
            client_target = with_client_headers(
                &permitted.target,
                prepared.beta.as_deref(),
                prepared.version.as_deref(),
            );
            &client_target
        } else {
            &permitted.target
        };
        let out = prepared.protocol;
        let hinted = prepared.hinted;
        let url = target.url(out, stream).unwrap_or_default();
        let sent = journal.capturing().then(|| {
            let mut sent = Sent::of(target, out, url, stream, prepared.bounded.clone());
            sent.reencoded = !prepared.same;
            sent.notes.clone_from(&prepared.notes);
            sent.hub.clone_from(&prepared.hub);
            sent
        });
        let null = Value::Null;
        let call = Call {
            runtime: &context.llm,
            target,
            protocol: out,
            stream,
            url: None,
            body: &null,
            timeout: if stream {
                first_byte.max(Duration::from_secs(1))
            } else {
                REQUEST_TIMEOUT
            },
            cancel,
        };
        journal.attempting(target, Some(out));
        let (sent_result, meta) = send_measured_bytes(&call, prepared.body.clone()).await;
        let outcome = match sent_result {
            Err(error) => Err(error),
            Ok(response) => {
                let reply_codec = codec(out);
                read_answer(
                    response,
                    stream,
                    out,
                    prepared.same,
                    reply_codec,
                    codec_in,
                    &requested,
                    failover.idle,
                    first_byte,
                    cancel,
                )
                .await
            }
        };
        match outcome {
            Ok(delivered) => {
                let notes = dropped_header(&prepared.notes);
                let same = prepared.same;
                let mut permitted = permitted;
                match delivered {
                    Delivered::Json {
                        body,
                        usage,
                        text,
                        upstream,
                    } => {
                        permitted.guard.finish(crate::llm::health::Outcome::Success);
                        journal.serving(&permitted.target, Some(out), meta, sent);
                        journal.add_usage(&usage);
                        journal.generated(&text);
                        journal.client_answer("application/json", notes.as_deref(), &body);
                        if let Some(upstream) = upstream {
                            journal.upstream_answer(&upstream);
                        }
                        journal.end(&Ended::Success);
                        return with_dropped(
                            bytes_response(200, "application/json", body),
                            notes.as_deref(),
                        );
                    }
                    Delivered::Stream(upstream, buffered) => {
                        journal.serving(&permitted.target, Some(out), meta, sent);
                        let committed =
                            buffered
                                .iter()
                                .flat_map(|chunk| &chunk.events)
                                .any(|event| {
                                    matches!(
                                        event,
                                        HubEvent::TextDelta { .. }
                                            | HubEvent::ReasoningDelta { .. }
                                            | HubEvent::ToolCallStart { .. }
                                    )
                                });
                        if committed {
                            journal.first_content();
                        }
                        journal.client_stream("text/event-stream", notes.as_deref());
                        let encoder =
                            (!same).then(|| rename_model(codec_in.stream_encoder(), &requested));
                        let pump = Pump::new(
                            upstream,
                            buffered,
                            encoder,
                            codec_in.stream_encoder(),
                            permitted.guard,
                            journal,
                        );
                        let mut response = Response::new(pump.into_body());
                        let headers = response.headers_mut();
                        headers.insert(
                            hyper::header::CONTENT_TYPE,
                            HeaderValue::from_static("text/event-stream"),
                        );
                        headers.insert(
                            hyper::header::CACHE_CONTROL,
                            HeaderValue::from_static("no-cache"),
                        );
                        headers.insert("x-accel-buffering", HeaderValue::from_static("no"));
                        return with_dropped(response, notes.as_deref());
                    }
                }
            }
            Err(error) if may_drop_hint && hinted && refuses_cache_hint(&error) => {
                // A relay that rejects `cache_control` says nothing about its health: the same
                // target gets the request once more without the hints, and the provider goes
                // without them for the rest of the session, in-app calls included.
                journal.failed(&permitted.target, Some(out), meta, &error, sent);
                refuse_cache(&context.llm, &permitted.target.provider_id);
                gate.cache[permitted.group] = None;
                again = Some(permitted);
            }
            Err(error) => {
                let spent = Spent {
                    protocol: Some(out),
                    meta,
                    sent,
                };
                if let Some(stop) = walk.failed(&mut journal, permitted, spent, error) {
                    return stopped(&mut journal, context, protocol, stop).await;
                }
            }
        }
    }
}

/// Reads the 2xx answer of an attempt: the whole JSON (converted for the client unless it is a
/// verbatim forward), or the first events of a stream (failover is still possible until its first
/// content event).
#[allow(clippy::too_many_arguments)] // one call site; a struct would only rename the arguments
async fn read_answer(
    response: reqwest::Response,
    stream: bool,
    out: Protocol,
    same: bool,
    reply_codec: Option<&'static dyn ChatCodec>,
    codec_in: &dyn ChatCodec,
    requested: &str,
    idle: Duration,
    first_byte: Duration,
    cancel: Option<&CancelToken>,
) -> Result<Delivered, AttemptError> {
    if stream {
        let mut upstream = UpstreamStream::new(response, out, idle, cancel.cloned())?;
        if same {
            upstream = upstream.lenient();
        }
        let buffered = upstream.prime(first_byte).await?;
        return Ok(Delivered::Stream(upstream, buffered));
    }
    let raw = read_body(response, cancel).await?;
    let json = parse_json(&raw)?;
    let decoded = reply_codec
        .map(|codec| codec.decode_response(&json))
        .unwrap_or_else(|| Err(AppError::internal("chat codec missing")));
    if same {
        // A same-protocol body the codec cannot read is still the upstream's answer: pass it on.
        let (usage, text) = decoded.map_or_else(
            |_| (HubUsage::default(), String::new()),
            |response| (response.usage.clone(), response.text()),
        );
        return Ok(Delivered::Json {
            body: raw,
            usage,
            text,
            upstream: None,
        });
    }
    let mut response = decoded.map_err(|error| bad_body(&error))?;
    let (usage, text) = (response.usage.clone(), response.text());
    requested.clone_into(&mut response.model);
    let body = serde_json::to_vec(&codec_in.encode_response(&response))
        .map_err(|error| AttemptError::Invalid(AppError::from(error)))?;
    Ok(Delivered::Json {
        body: Bytes::from(body),
        usage,
        text,
        upstream: Some(raw),
    })
}

/// Re-labels events with the model the client asked for, so a re-encoded stream from an alias
/// never shows the upstream's model name.
fn rename_model(inner: Box<dyn StreamEncoder>, requested: &str) -> Box<dyn StreamEncoder> {
    Box::new(RenameModel {
        inner,
        model: requested.to_owned(),
    })
}

struct RenameModel {
    inner: Box<dyn StreamEncoder>,
    model: String,
}

impl StreamEncoder for RenameModel {
    fn push(&mut self, event: &HubEvent) -> Vec<u8> {
        match event {
            HubEvent::Start { id, .. } => self.inner.push(&HubEvent::Start {
                id: id.clone(),
                model: self.model.clone(),
            }),
            other => self.inner.push(other),
        }
    }

    fn finish(&mut self) -> Vec<u8> {
        self.inner.finish()
    }
}
