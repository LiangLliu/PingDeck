//! The JSON-only routes: embeddings, completions, rerank, token counts and the decision APIs.
//!
//! Single responsibility: calls that always answer with one JSON body. `POST /v1/embeddings` goes
//! to an OpenAI-style embeddings endpoint (body forwarded byte for byte except `model`) or is
//! converted to Gemini's `embedContent` / `batchEmbedContents`; Gemini's own `:embedContent` and
//! `:batchEmbedContents` go to a Gemini target as they are. `POST /v1/completions` and
//! `/v1/rerank` are forwarded verbatim (except `model`) to the same path next to the target's
//! OpenAI-style chat or embeddings endpoint, without conversion and without streaming.
//! `POST /v1/systemone` and `/v1/decisions` go to a provider that speaks the same decision
//! protocol (the two request shapes differ, so there is no conversion). Planning, failover,
//! health, the log row and the capture are the chat pipeline's (`Walk`, `Journal`); the
//! upstream's reported usage is logged. `/v1/messages/count_tokens` is asked of the Messages
//! targets with neither log row nor health record ([`count_tokens`]).

use std::time::Duration;

use bytes::Bytes;
use hyper::Response;
use serde_json::{Value, json};

use crate::llm::adapters::{decision, embeddings};
use crate::llm::capture::{BoundedBody, bound_body};
use crate::llm::failure::FailureKind;
use crate::llm::hub::HubUsage;
use crate::llm::router::{PlanError, RequestNeeds, Target};
use crate::llm::upstream::{Call, bad_body, parse_json, read_body, send_measured_bytes};
use crate::llm::wire::{Protocol, gemini_embed_url, rewrite_model};
use spanbay_base::error::AppError;

use super::chat::{Inbound, capture_wanted};
use super::ending::{exhausted, fail, plan_failed, stopped};
use super::exchange::Sent;
use super::journal::{Ended, Journal};
use super::passthrough::with_model;
use super::reply::bytes_response;
use super::server::{Context, GatewayBody, status_of};
use super::walk::{Gate, Refusal, Spent, Stop, Walk};

/// Whole-request timeout of an upstream call.
const REQUEST_TIMEOUT: Duration = Duration::from_secs(300);
/// Whole-request timeout of an upstream token count; a slower one gives way to the estimate.
const COUNT_TIMEOUT: Duration = Duration::from_secs(30);

/// How one provider model takes the request.
struct Prepared {
    protocol: Protocol,
    url: Option<String>,
    body: Bytes,
    /// The body as stored in the capture; `None` when it is the client's except `model`.
    bounded: Option<BoundedBody>,
    /// Forwarded verbatim: the client's bytes out, the upstream's bytes back.
    same: bool,
}

enum Prep {
    Ready(Prepared),
    Skip(Refusal),
    Invalid(AppError),
}

/// Prepares the request once per provider model.
struct Preparer<F> {
    prepare: F,
    capture: bool,
    cache: Vec<Option<Prep>>,
}

impl<F: FnMut(&Target) -> Result<Option<Prepared>, AppError>> Preparer<F> {
    fn prepared(&mut self, group: usize, target: &Target) -> &Prep {
        let prep = match self.cache[group].take() {
            Some(prep) => prep,
            None => match (self.prepare)(target) {
                Ok(Some(mut prepared)) => {
                    if !prepared.same {
                        // A converted body differs from the client's: keep it in the capture.
                        prepared.bounded = self.capture.then(|| bound_body(&prepared.body));
                    }
                    Prep::Ready(prepared)
                }
                Ok(None) => Prep::Skip(Refusal {
                    reason: "the provider has no matching endpoint".to_owned(),
                    message: format!(
                        "provider {} has no endpoint for this request",
                        target.provider_name
                    ),
                }),
                Err(error) => Prep::Invalid(error),
            },
        };
        self.cache[group].insert(prep)
    }
}

impl<F: FnMut(&Target) -> Result<Option<Prepared>, AppError> + Send> Gate for Preparer<F> {
    fn refuse(&mut self, group: usize, target: &Target) -> Option<Refusal> {
        match self.prepared(group, target) {
            Prep::Skip(refusal) => Some(Refusal {
                reason: refusal.reason.clone(),
                message: refusal.message.clone(),
            }),
            _ => None,
        }
    }
}

/// Runs one JSON-only call over the targets of `inbound`'s model. `convert` turns an upstream
/// answer into the client's body (and says what it used); a verbatim forward keeps the bytes.
async fn run(
    context: &Context,
    inbound: &Inbound,
    needs: RequestNeeds,
    prepare: impl FnMut(&Target) -> Result<Option<Prepared>, AppError> + Send,
    convert: impl Fn(&Prepared, &Value) -> Result<(Bytes, HubUsage), AppError> + Send,
) -> Response<GatewayBody> {
    let protocol = inbound.protocol;
    let requested = inbound.requested();
    let route_name = inbound.route_name();
    let mut journal = Journal::new(context, inbound, &requested, false, capture_wanted(context));
    if requested.trim().is_empty() {
        let message = "the request has no model";
        return fail(
            &mut journal,
            protocol,
            400,
            message,
            None,
            FailureKind::Unknown,
        );
    }
    let snapshot = match context.llm.snapshot().await {
        Ok(snapshot) => snapshot,
        Err(error) => {
            return fail(
                &mut journal,
                protocol,
                status_of(&error),
                error.message(),
                None,
                FailureKind::Unknown,
            );
        }
    };
    let identity = context.identity();
    let scope = |name: &str| identity.allows(name);
    let planned = snapshot.plan_explained(
        &route_name,
        &needs,
        Some(&scope),
        &context.llm.health,
        &context.llm.keys,
    );
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
    let failover = context.shared.failover;
    let max_retries = u8::try_from(failover.max_attempts.saturating_sub(1)).unwrap_or(u8::MAX);
    let mut gate = Preparer {
        prepare,
        capture: journal.capturing(),
        cache: (0..plan.groups.len()).map(|_| None).collect(),
    };
    let mut walk = Walk::new(plan, max_retries, failover.circuit);
    let cancel = inbound.cancel.as_ref();
    loop {
        let permitted = match walk.next(context, &mut journal, cancel, &mut gate).await {
            Ok(Some(permitted)) => permitted,
            Ok(None) => return exhausted(&mut journal, context, protocol, &mut walk).await,
            Err(()) => return stopped(&mut journal, context, protocol, Stop::Canceled).await,
        };
        let prepared = match gate.prepared(permitted.group, &permitted.target) {
            Prep::Ready(prepared) => prepared,
            Prep::Invalid(error) => {
                let mut permitted = permitted;
                permitted
                    .guard
                    .finish(crate::llm::health::Outcome::Canceled);
                return fail(
                    &mut journal,
                    protocol,
                    status_of(error),
                    error.message(),
                    None,
                    FailureKind::Unknown,
                );
            }
            Prep::Skip(_) => continue,
        };
        let target = &permitted.target;
        let url = match &prepared.url {
            Some(url) => url.clone(),
            None => target.url(prepared.protocol, false).unwrap_or_default(),
        };
        let sent = journal.capturing().then(|| {
            Sent::of(
                target,
                prepared.protocol,
                url,
                false,
                prepared.bounded.clone(),
            )
        });
        let null = Value::Null;
        let call = Call {
            runtime: &context.llm,
            target,
            protocol: prepared.protocol,
            stream: false,
            url: prepared.url.as_deref(),
            body: &null,
            timeout: REQUEST_TIMEOUT,
            cancel,
        };
        journal.attempting(target, Some(prepared.protocol));
        let (sent_result, meta) = send_measured_bytes(&call, prepared.body.clone()).await;
        // What the upstream sent that this side cannot read (not JSON, an answer the converter
        // rejects) is the upstream's fault: the walk moves on and the breaker counts it.
        let answer = match sent_result {
            Ok(response) => match read_body(response, cancel).await {
                Ok(raw) => parse_json(&raw).and_then(|json| {
                    let converted = if prepared.same {
                        Ok((raw.clone(), usage_of(protocol, &json)))
                    } else {
                        convert(prepared, &json).map_err(|error| bad_body(&error))
                    };
                    converted.map(|(body, usage)| (raw, body, usage))
                }),
                Err(error) => Err(error),
            },
            Err(error) => Err(error),
        };
        match answer {
            Ok((raw, body, usage)) => {
                let mut permitted = permitted;
                permitted.guard.finish(crate::llm::health::Outcome::Success);
                journal.serving(&permitted.target, Some(prepared.protocol), meta, sent);
                journal.add_usage(&usage);
                journal.client_answer("application/json", None, &body);
                if !prepared.same {
                    journal.upstream_answer(&raw);
                }
                journal.end(&Ended::Success);
                return bytes_response(200, "application/json", body);
            }
            Err(error) => {
                let spent = Spent {
                    protocol: Some(prepared.protocol),
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

/// The usage a verbatim answer reports (embeddings and decisions name it differently).
fn usage_of(protocol: Protocol, json: &Value) -> HubUsage {
    let count = |path: &str| json.pointer(path).and_then(Value::as_u64);
    match protocol {
        Protocol::Embeddings => embeddings::usage(json),
        Protocol::OpenaiChat => HubUsage {
            input_tokens: count("/usage/prompt_tokens"),
            output_tokens: count("/usage/completion_tokens"),
            ..HubUsage::default()
        },
        Protocol::Gemini => HubUsage {
            input_tokens: gemini_prompt_tokens(json),
            ..HubUsage::default()
        },
        Protocol::AnthropicMessages => HubUsage::default(),
        _ => decision::read_response(json)
            .map(|summary| summary.usage)
            .unwrap_or_default(),
    }
}

/// The URL of `path` next to the first of `protocols` the target speaks with an OpenAI-style
/// path (`…/v1/chat/completions` → `…/v1/completions`), and that protocol (its authentication
/// applies). `None` when the target has no such endpoint.
fn sibling_url(target: &Target, protocols: &[Protocol], path: &str) -> Option<(Protocol, String)> {
    protocols.iter().find_map(|protocol| {
        let suffix = match protocol {
            Protocol::OpenaiChat => "/chat/completions",
            Protocol::Embeddings => "/embeddings",
            Protocol::AnthropicMessages => "/messages",
            _ => return None,
        };
        target.endpoint(*protocol)?;
        let url = target.url(*protocol, false).ok()?;
        // A query (`?api-version=…`) belongs to the endpoint, not to the path: it is kept.
        let (address, query) = match url.split_once('?') {
            Some((address, query)) => (address, Some(query)),
            None => (url.as_str(), None),
        };
        let root = address.trim_end_matches('/').strip_suffix(suffix)?;
        Some((
            *protocol,
            match query {
                Some(query) => format!("{root}{path}?{query}"),
                None => format!("{root}{path}"),
            },
        ))
    })
}

/// A verbatim forward of `inbound` to `path` next to one of `protocols` of the target.
fn forward_to<'a>(
    inbound: &'a Inbound,
    protocols: &'static [Protocol],
    path: &'static str,
) -> impl FnMut(&Target) -> Result<Option<Prepared>, AppError> + Send + 'a {
    move |target: &Target| {
        let Some((protocol, url)) = sibling_url(target, protocols, path) else {
            return Ok(None);
        };
        Ok(Some(Prepared {
            protocol,
            url: Some(url),
            body: forwarded(inbound, protocol, &target.model),
            bounded: None,
            same: true,
        }))
    }
}

/// Answers of verbatim forwards are never converted.
fn never_converted(_: &Prepared, _: &Value) -> Result<(Bytes, HubUsage), AppError> {
    Err(AppError::internal("verbatim answers are never converted"))
}

/// `POST /v1/completions`: prompt and fill-in-the-middle completions, forwarded verbatim to the
/// `completions` path of an OpenAI-style target. Streaming is refused: the answer is one JSON body.
pub(super) async fn completions(context: &Context, inbound: Inbound) -> Response<GatewayBody> {
    if inbound.json.get("stream").and_then(Value::as_bool) == Some(true) {
        return super::errors::error_response(
            None,
            400,
            "The SpanBay gateway forwards /v1/completions without streaming: send \"stream\": false.",
            "invalid_request_error",
        );
    }
    let needs = RequestNeeds {
        endpoints: &[Protocol::OpenaiChat],
        ..RequestNeeds::default()
    };
    let prepare = forward_to(&inbound, &[Protocol::OpenaiChat], "/completions");
    run(context, &inbound, needs, prepare, never_converted).await
}

/// `POST /v1/rerank`, forwarded verbatim to the `rerank` path next to the target's embeddings (or
/// chat) endpoint.
pub(super) async fn rerank(context: &Context, inbound: Inbound) -> Response<GatewayBody> {
    let needs = RequestNeeds {
        endpoints: &[Protocol::Embeddings, Protocol::OpenaiChat],
        ..RequestNeeds::default()
    };
    let prepare = forward_to(
        &inbound,
        &[Protocol::Embeddings, Protocol::OpenaiChat],
        "/rerank",
    );
    run(context, &inbound, needs, prepare, never_converted).await
}

/// `POST /v1/messages/count_tokens` asked of the Messages targets of the model in plan order, at
/// most as many as the failover attempt budget: `Some` with the first count an upstream answered,
/// `None` when none did (no such target, an error status, a body without `input_tokens`), and the
/// caller answers with its estimate. A count spends nothing and many Anthropic-compatible relays
/// lack the endpoint, so it writes no log row and its failures weigh on no key's health or rest.
pub(super) async fn count_tokens(
    context: &Context,
    inbound: &Inbound,
) -> Option<Response<GatewayBody>> {
    let snapshot = context.llm.snapshot().await.ok()?;
    let plan = {
        let identity = context.identity();
        let scope = |name: &str| identity.allows(name);
        let needs = RequestNeeds {
            endpoints: &[Protocol::AnthropicMessages],
            ..RequestNeeds::default()
        };
        snapshot
            .plan_explained(
                &inbound.route_name(),
                &needs,
                Some(&scope),
                &context.llm.health,
                &context.llm.keys,
            )
            .ok()?
    };
    let cancel = inbound.cancel.as_ref();
    let null = Value::Null;
    let targets = plan.groups.iter().flat_map(|group| &group.targets);
    for target in targets.take(context.shared.failover.max_attempts.max(1)) {
        let Some((protocol, url)) = sibling_url(
            target,
            &[Protocol::AnthropicMessages],
            "/messages/count_tokens",
        ) else {
            continue;
        };
        let call = Call {
            runtime: &context.llm,
            target,
            protocol,
            stream: false,
            url: Some(&url),
            body: &null,
            timeout: COUNT_TIMEOUT,
            cancel,
        };
        let body = forwarded(inbound, protocol, &target.model);
        let Ok(response) = send_measured_bytes(&call, body).await.0 else {
            continue;
        };
        let Ok(raw) = read_body(response, cancel).await else {
            continue;
        };
        if parse_json(&raw).is_ok_and(|json| json.get("input_tokens").is_some_and(Value::is_u64)) {
            return Some(bytes_response(200, "application/json", raw));
        }
    }
    None
}

/// Gemini `:embedContent` / `:batchEmbedContents` of a Gemini client, forwarded to a Gemini
/// target with the target's model in the URL and in each request.
pub(super) async fn gemini_embed(
    context: &Context,
    inbound: Inbound,
    batch: bool,
) -> Response<GatewayBody> {
    let needs = RequestNeeds {
        endpoints: &[Protocol::Gemini],
        ..RequestNeeds::default()
    };
    let json = &inbound.json;
    let prepare = |target: &Target| -> Result<Option<Prepared>, AppError> {
        let Some(endpoint) = target.endpoint(Protocol::Gemini) else {
            return Ok(None);
        };
        let mut url = gemini_embed_url(&target.base_url, &endpoint.path_prefix, &target.model)?;
        let model = format!("models/{}", target.model.trim_start_matches("models/"));
        let mut body = json.clone();
        if batch {
            url = url.replace(":embedContent", ":batchEmbedContents");
            for request in body
                .get_mut("requests")
                .and_then(Value::as_array_mut)
                .into_iter()
                .flatten()
                .filter_map(Value::as_object_mut)
            {
                request.insert("model".to_owned(), Value::String(model.clone()));
            }
        } else if let Some(object) = body.as_object_mut()
            && object.contains_key("model")
        {
            object.insert("model".to_owned(), Value::String(model));
        }
        Ok(Some(Prepared {
            protocol: Protocol::Gemini,
            url: Some(url),
            body: Bytes::from(serde_json::to_vec(&body).unwrap_or_default()),
            bounded: None,
            same: true,
        }))
    };
    run(context, &inbound, needs, prepare, never_converted).await
}

/// The strings of an OpenAI `input` (a string or an array of strings).
fn embedding_inputs(raw: &Value) -> Result<Vec<String>, AppError> {
    match raw.get("input") {
        Some(Value::String(text)) => Ok(vec![text.clone()]),
        Some(Value::Array(items)) if !items.is_empty() => items
            .iter()
            .map(|item| {
                item.as_str().map(str::to_owned).ok_or_else(|| {
                    AppError::unsupported(
                        "only string inputs can be converted to Gemini embeddings",
                    )
                })
            })
            .collect(),
        _ => Err(AppError::validation(
            "`input` must be a string or a non-empty array of strings",
        )),
    }
}

/// `body` with `model` set to the target's, bytes preserved where the text allows.
fn forwarded(inbound: &Inbound, protocol: Protocol, model: &str) -> Bytes {
    with_model(&inbound.bytes, model)
        .map(Bytes::from)
        .unwrap_or_else(|| {
            let mut json = inbound.json.clone();
            rewrite_model(protocol, &mut json, model);
            Bytes::from(serde_json::to_vec(&json).unwrap_or_default())
        })
}

/// The tokens a Gemini embedding answer reports (`usageMetadata`).
fn gemini_prompt_tokens(body: &Value) -> Option<u64> {
    ["usageMetadata", "usage_metadata"]
        .iter()
        .find_map(|key| body.get(*key))
        .and_then(|usage| {
            usage
                .get("promptTokenCount")
                .or_else(|| usage.get("prompt_token_count"))
        })
        .and_then(Value::as_u64)
}

/// `POST /v1/embeddings`.
pub(super) async fn embeddings(context: &Context, inbound: Inbound) -> Response<GatewayBody> {
    let needs = RequestNeeds {
        endpoints: &[Protocol::Embeddings, Protocol::Gemini],
        ..RequestNeeds::default()
    };
    let requested = inbound.requested();
    let (json, bytes_source) = (inbound.json.clone(), &inbound);
    let prepare = |target: &Target| -> Result<Option<Prepared>, AppError> {
        if target.endpoint(Protocol::Embeddings).is_some() {
            return Ok(Some(Prepared {
                protocol: Protocol::Embeddings,
                url: None,
                body: forwarded(bytes_source, Protocol::Embeddings, &target.model),
                bounded: None,
                same: true,
            }));
        }
        let Some(endpoint) = target.endpoint(Protocol::Gemini) else {
            return Ok(None);
        };
        let texts = embedding_inputs(&json)?;
        let model = format!("models/{}", target.model.trim_start_matches("models/"));
        let url = gemini_embed_url(&target.base_url, &endpoint.path_prefix, &target.model)?;
        // The OpenAI `dimensions` is Gemini's `outputDimensionality`.
        let dimensions = json.get("dimensions").and_then(Value::as_u64);
        let content = |text: &str| {
            let mut one = json!({"model": model, "content": {"parts": [{"text": text}]}});
            if let (Some(dimensions), Some(map)) = (dimensions, one.as_object_mut()) {
                map.insert("outputDimensionality".into(), json!(dimensions));
            }
            one
        };
        let (url, body) = if let [single] = texts.as_slice() {
            (url, content(single))
        } else {
            (
                url.replace(":embedContent", ":batchEmbedContents"),
                json!({"requests": texts.iter().map(|text| content(text)).collect::<Vec<_>>()}),
            )
        };
        Ok(Some(Prepared {
            protocol: Protocol::Gemini,
            url: Some(url),
            body: Bytes::from(serde_json::to_vec(&body).unwrap_or_default()),
            bounded: None,
            same: false,
        }))
    };
    let convert = move |_: &Prepared, body: &Value| -> Result<(Bytes, HubUsage), AppError> {
        let vectors: Vec<Value> = match (body.get("embedding"), body.get("embeddings")) {
            (Some(single), _) => vec![single.get("values").cloned().unwrap_or(Value::Null)],
            (None, Some(Value::Array(many))) => many
                .iter()
                .map(|e| e.get("values").cloned().unwrap_or(Value::Null))
                .collect(),
            _ => {
                return Err(AppError::network(
                    "the Gemini embedding response has no embeddings",
                ));
            }
        };
        let tokens = gemini_prompt_tokens(body);
        let answer = json!({
            "object": "list",
            "model": requested,
            "data": vectors.into_iter().enumerate().map(|(index, values)| json!({
                "object": "embedding", "index": index, "embedding": values,
            })).collect::<Vec<_>>(),
            "usage": {"prompt_tokens": tokens.unwrap_or(0), "total_tokens": tokens.unwrap_or(0)},
        });
        let usage = HubUsage {
            input_tokens: tokens,
            ..HubUsage::default()
        };
        Ok((
            Bytes::from(serde_json::to_vec(&answer).unwrap_or_default()),
            usage,
        ))
    };
    run(context, &inbound, needs, prepare, convert).await
}

/// `POST /v1/systemone` and `/v1/decisions`.
pub(super) async fn decision(context: &Context, inbound: Inbound) -> Response<GatewayBody> {
    let protocol = inbound.protocol;
    let needs = RequestNeeds {
        endpoints: match protocol {
            Protocol::DecisionOpenai => &[Protocol::DecisionOpenai],
            _ => &[Protocol::DecisionSystemone],
        },
        ..RequestNeeds::default()
    };
    let source = &inbound;
    let prepare = |target: &Target| -> Result<Option<Prepared>, AppError> {
        if target.endpoint(protocol).is_none() {
            return Ok(None);
        }
        Ok(Some(Prepared {
            protocol,
            url: None,
            body: forwarded(source, protocol, &target.model),
            bounded: None,
            same: true,
        }))
    };
    let convert = |_: &Prepared, _: &Value| -> Result<(Bytes, HubUsage), AppError> {
        Err(AppError::internal("decision answers are never converted"))
    };
    run(context, &inbound, needs, prepare, convert).await
}
