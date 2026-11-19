//! Request routing and the small endpoints of the gateway.
//!
//! Single responsibility: turn an HTTP request into a [`Route`], run the guards and the token
//! check in the documented order (Host, Origin, route 404, token 401, an expired client token 401
//! on every route, the client's limits on spending routes (`quota`), allow-list 403 once the body
//! is parsed and the model is known, body limits,
//! forwarding), read the body, and hand chat, embedding, decision and passthrough calls to `chat`
//! and `calls`. It also answers the endpoints that need no upstream: `/health`, the per-client
//! model lists and the token estimates (marked `x-pingdeck-token-estimate`), and stamps every
//! response with `x-pingdeck-request-id`.
//!
//! Supported routes: `GET /health`; `GET /v1/models`, `GET /v1beta/models`;
//! `POST /v1/chat/completions`, `/v1/responses`, `/v1/messages`, `/v1/messages/count_tokens`
//! (forwarded to a Messages target, estimated when none counts it), `/v1/embeddings`, `/v1/completions` and
//! `/v1/rerank` (OpenAI-style passthrough, not streamed), `/v1/systemone`, `/v1/decisions`;
//! `POST /v1beta/models/{model}:generateContent`, `:streamGenerateContent`, `:countTokens`
//! (estimated), `:embedContent` and `:batchEmbedContents` (also under `/v1/`).

use std::sync::Arc;
use std::time::Duration;

use http_body_util::{BodyExt, LengthLimitError, Limited};
use hyper::header::HeaderValue;
use hyper::{Method, Request, Response};
use serde_json::{Value, json};

use crate::llm::adapters::{InboundHint, codec};
use crate::llm::hub::HubRequest;
use crate::llm::router::RoutingSnapshot;
use crate::llm::wire::Protocol;

use super::auth;
use super::calls;
use super::chat::{self, Inbound};
use super::errors::{Refusal, error_response};
use super::guard::{self, Verdict};
use super::server::{AbortLog, Context, GatewayBody, InFlight, json as json_response};

/// How long reading one request body may take.
pub(super) const BODY_TIMEOUT: Duration = Duration::from_secs(120);

/// The response header that carries the log row's request id.
const REQUEST_ID_HEADER: &str = "x-pingdeck-request-id";
/// The response header of a token count PingDeck estimated itself (no upstream counted it).
pub(super) const ESTIMATE_HEADER: &str = "x-pingdeck-token-estimate";
/// The response header that names the client's default model a bare unknown name was mapped to.
const MAPPED_HEADER: &str = "x-pingdeck-model-mapped";

/// What a request is asking for.
#[derive(Debug, Clone, PartialEq, Eq)]
pub(super) enum Route {
    Health,
    Models,
    Chat {
        protocol: Protocol,
        hint: InboundHint,
    },
    CountTokens,
    /// Gemini `:countTokens` of the model in the path.
    GeminiCount(String),
    /// Gemini `:embedContent` (or `:batchEmbedContents` when `batch`) of the model in the path.
    GeminiEmbed {
        model: String,
        batch: bool,
    },
    Embeddings,
    /// `POST /v1/completions` (prompt and fill-in-the-middle completions).
    Completions,
    /// `POST /v1/rerank`.
    Rerank,
    Decision(Protocol),
    NotFound,
}

/// Decodes `%XX` escapes (a model id may be percent-encoded in a Gemini path).
fn percent_decode(text: &str) -> String {
    percent_encoding::percent_decode_str(text)
        .decode_utf8_lossy()
        .into_owned()
}

/// Maps a method and path to a [`Route`].
pub(super) fn route(method: &Method, path: &str) -> Route {
    let path = path.trim_end_matches('/');
    let get = method == Method::GET;
    let post = method == Method::POST;
    match path {
        "/health" if get => Route::Health,
        "/v1/models" | "/v1beta/models" if get => Route::Models,
        "/v1/chat/completions" if post => Route::Chat {
            protocol: Protocol::OpenaiChat,
            hint: InboundHint::default(),
        },
        "/v1/responses" if post => Route::Chat {
            protocol: Protocol::OpenaiResponses,
            hint: InboundHint::default(),
        },
        "/v1/messages" if post => Route::Chat {
            protocol: Protocol::AnthropicMessages,
            hint: InboundHint::default(),
        },
        "/v1/messages/count_tokens" if post => Route::CountTokens,
        "/v1/embeddings" if post => Route::Embeddings,
        "/v1/completions" if post => Route::Completions,
        "/v1/rerank" if post => Route::Rerank,
        "/v1/systemone" if post => Route::Decision(Protocol::DecisionSystemone),
        "/v1/decisions" if post => Route::Decision(Protocol::DecisionOpenai),
        _ => {
            let rest = path
                .strip_prefix("/v1beta/models/")
                .or_else(|| path.strip_prefix("/v1/models/"));
            match rest.and_then(|rest| rest.rsplit_once(':')) {
                Some((model, action)) if post && !model.is_empty() => match action {
                    "generateContent" => Route::Chat {
                        protocol: Protocol::Gemini,
                        hint: InboundHint {
                            model: Some(percent_decode(model)),
                            stream: false,
                        },
                    },
                    "streamGenerateContent" => Route::Chat {
                        protocol: Protocol::Gemini,
                        hint: InboundHint {
                            model: Some(percent_decode(model)),
                            stream: true,
                        },
                    },
                    "countTokens" => Route::GeminiCount(percent_decode(model)),
                    "embedContent" | "batchEmbedContents" => Route::GeminiEmbed {
                        model: percent_decode(model),
                        batch: action == "batchEmbedContents",
                    },
                    _ => Route::NotFound,
                },
                _ => Route::NotFound,
            }
        }
    }
}

impl Route {
    /// The dialect of this route's error bodies: its protocol's for chat routes and token
    /// counts, OpenAI's (`None`) for everything else.
    pub(super) fn family(&self) -> Option<Protocol> {
        match self {
            Self::Chat { protocol, .. } => Some(*protocol),
            Self::CountTokens => Some(Protocol::AnthropicMessages),
            Self::GeminiCount(_) | Self::GeminiEmbed { .. } => Some(Protocol::Gemini),
            _ => None,
        }
    }

    /// The `protocol_in` column of this route's log rows.
    pub(super) fn protocol_in(&self) -> &'static str {
        match self {
            Self::Chat { protocol, .. } | Self::Decision(protocol) => protocol.as_str(),
            Self::CountTokens => Protocol::AnthropicMessages.as_str(),
            Self::GeminiCount(_) | Self::GeminiEmbed { .. } => Protocol::Gemini.as_str(),
            Self::Embeddings | Self::Rerank => Protocol::Embeddings.as_str(),
            Self::Completions => Protocol::OpenaiChat.as_str(),
            Self::Models => "models",
            Self::Health | Self::NotFound => "unknown",
        }
    }

    /// Whether the route can spend: the client's limits apply to it.
    fn spends(&self) -> bool {
        matches!(
            self,
            Self::Chat { .. }
                | Self::Embeddings
                | Self::Completions
                | Self::Rerank
                | Self::GeminiEmbed { .. }
                | Self::Decision(_)
        )
    }
}

/// The model a call names, when the route has one: the body's `model`, else the Gemini path's.
/// Token counts name one too but spend nothing, so they are not allow-listed.
fn requested_model(route: &Route, json: &Value) -> Option<String> {
    let from_body = json.get("model").and_then(Value::as_str).map(str::to_owned);
    let model = match route {
        Route::Chat { hint, .. } => from_body.or_else(|| hint.model.clone()),
        Route::GeminiEmbed { model, .. } => Some(model.clone()),
        Route::Embeddings | Route::Completions | Route::Rerank | Route::Decision(_) => from_body,
        _ => None,
    };
    model.filter(|model| !model.trim().is_empty())
}

/// Serves one request.
pub(super) async fn handle(
    context: Context,
    request: Request<hyper::body::Incoming>,
) -> Response<GatewayBody> {
    let (parts, body) = request.into_parts();
    let route = route(&parts.method, parts.uri.path());
    let in_flight = InFlight::enter(&context.shared);
    let mut abort = AbortLog {
        context: &context,
        route: &route,
        armed: true,
    };
    let settings = context.llm.settings();
    let mut response = match guard::check(&context.shared, &settings.ai.gateway, &parts).await {
        Verdict::Refuse(refusal) => context.refuse(&refusal, &route, None, true),
        Verdict::Pass => serve(&context, &route, &parts, body).await,
    };
    abort.armed = false;
    super::reply::with_target(&mut response, context.attempted());
    if let Ok(id) = HeaderValue::from_str(&context.request_id) {
        response.headers_mut().insert(REQUEST_ID_HEADER, id);
    }
    in_flight.follow(response)
}

/// Everything after the guards: route, token, body, allow-list, forwarding.
async fn serve(
    context: &Context,
    route: &Route,
    parts: &hyper::http::request::Parts,
    body: hyper::body::Incoming,
) -> Response<GatewayBody> {
    match route {
        Route::Health => return json_response(200, &json!({"status": "ok", "app": "pingdeck"})),
        Route::NotFound => return error_response(None, 404, "unknown endpoint", "not_found_error"),
        _ => {}
    }
    if let Some(refusal) = auth::authorize(context, parts, route).await {
        return refusal;
    }
    let identity = context.identity();
    // An expired token is dead on every route, the model list and token counts included; the
    // other limits only weigh on routes that spend.
    if identity
        .client()
        .is_some_and(|client| client.limits.expired(crate::llm::runtime::now_ms()))
    {
        return context.refuse(&Refusal::Expired, route, None, true);
    }
    if *route == Route::Models {
        return models(context, parts).await;
    }
    let slot = match identity.client() {
        Some(client) if route.spends() => {
            match context.shared.quotas.admit(&context.llm, client).await {
                Ok(slot) => slot,
                Err(refusal) => return context.refuse(&refusal, route, None, true),
            }
        }
        _ => None,
    };
    let response = forward(context, route, parts, body).await;
    match slot {
        // The place among the client's requests in flight is held until the body ends.
        Some(slot) => response.map(|body| {
            body.map_frame(move |frame| {
                let _held = &slot;
                frame
            })
            .boxed_unsync()
        }),
        None => response,
    }
}

/// Body limits, the allow-list and the hand-over to the pipeline.
async fn forward(
    context: &Context,
    route: &Route,
    parts: &hyper::http::request::Parts,
    body: hyper::body::Incoming,
) -> Response<GatewayBody> {
    let family = route.family();
    let too_large = || context.refuse(&Refusal::BodyTooLarge, route, None, true);
    let limit = context.shared.limits.max_body;
    let declared = parts
        .headers
        .get(hyper::header::CONTENT_LENGTH)
        .and_then(|value| value.to_str().ok())
        .and_then(|value| value.parse::<u64>().ok());
    if declared.is_some_and(|length| length > u64::try_from(limit).unwrap_or(u64::MAX)) {
        return too_large();
    }
    // Held until the request is answered (or, for a stream, until its head is): the body is
    // parsed, decoded and re-encoded meanwhile, several times its size. A body of unknown length
    // is accounted at the limit.
    let weight = declared.map_or(limit, |length| usize::try_from(length).unwrap_or(limit));
    let Ok(_budget) = Arc::clone(&context.shared.body_budget)
        .try_acquire_many_owned(u32::try_from(weight).unwrap_or(u32::MAX))
    else {
        return context.refuse(&Refusal::BodyBudget, route, None, true);
    };
    let bytes = match tokio::time::timeout(
        context.shared.limits.body_timeout,
        Limited::new(body, limit).collect(),
    )
    .await
    {
        Ok(Ok(collected)) => collected.to_bytes(),
        Ok(Err(error)) if error.is::<LengthLimitError>() => return too_large(),
        Ok(Err(_)) => {
            return error_response(
                family,
                400,
                "the request body could not be read",
                "invalid_request_error",
            );
        }
        Err(_) => {
            return error_response(
                family,
                408,
                "timed out reading the request body",
                "invalid_request_error",
            );
        }
    };
    let json: Value = match serde_json::from_slice(&bytes) {
        Ok(json) => json,
        Err(error) => {
            return error_response(
                family,
                400,
                &format!("the request body is not valid JSON: {error}"),
                "invalid_request_error",
            );
        }
    };
    // A bare name the gateway does not know goes to the client's default model; the body keeps
    // the client's name (the log and the answer show it), only the routing uses the mapping.
    let routed = unknown_model_default(context, route, &json).await;
    if let Some(model) = routed.clone().or_else(|| requested_model(route, &json))
        && context.identity().client().is_some()
        && !scope_allows(context, &model).await
    {
        let refusal = Refusal::ModelNotAllowed {
            model: model.clone(),
        };
        return context.refuse(&refusal, route, Some(&model), true);
    }
    let tagged = matches!(route, Route::Chat { .. }) && carries_signature_tag(&bytes);
    let (protocol, hint) = match route {
        Route::Chat { protocol, hint } => (*protocol, hint.clone()),
        Route::Embeddings | Route::Rerank => (Protocol::Embeddings, InboundHint::default()),
        Route::Completions => (Protocol::OpenaiChat, InboundHint::default()),
        Route::GeminiEmbed { model, .. } => (
            Protocol::Gemini,
            InboundHint {
                model: Some(model.clone()),
                stream: false,
            },
        ),
        Route::Decision(protocol) => (*protocol, InboundHint::default()),
        Route::CountTokens => (Protocol::AnthropicMessages, InboundHint::default()),
        Route::GeminiCount(_) => return gemini_count_tokens(&json),
        Route::Health | Route::Models | Route::NotFound => {
            return error_response(None, 404, "unknown endpoint", "not_found_error");
        }
    };
    let asked = requested_model(route, &json);
    let inbound = Inbound {
        protocol,
        hint,
        bytes,
        json,
        tagged,
        headers: parts
            .headers
            .iter()
            .filter_map(|(name, value)| {
                Some((name.as_str().to_owned(), value.to_str().ok()?.to_owned()))
            })
            .collect(),
        pin: None,
        routed: routed.clone(),
        cancel: None,
    };
    let mut response = match route {
        Route::Chat { .. } => chat::chat(context, inbound).await,
        Route::Embeddings => calls::embeddings(context, inbound).await,
        Route::Completions => calls::completions(context, inbound).await,
        Route::Rerank => calls::rerank(context, inbound).await,
        Route::GeminiEmbed { batch, .. } => calls::gemini_embed(context, inbound, *batch).await,
        Route::CountTokens => count_tokens(context, inbound).await,
        _ => calls::decision(context, inbound).await,
    };
    if let Some(target) = routed
        && let Some(asked) = asked
        && let Ok(value) = HeaderValue::from_str(&format!("{asked} -> {target}"))
    {
        response.headers_mut().insert(MAPPED_HEADER, value);
    }
    response
}

/// Whether a raw JSON body holds a wrapped signature (`hub::wrap_signature`): a string value
/// that starts with `pdk1.<protocol>.`. Text that mentions the tag mid-string does not count.
pub(super) fn carries_signature_tag(body: &[u8]) -> bool {
    const NEEDLE: &[u8] = b"\"pdk1.";
    memchr::memmem::find_iter(body, NEEDLE).any(|start| {
        let escaped = start > 0 && body[start - 1] == b'\\';
        let tail = &body[start + NEEDLE.len()..];
        !escaped
            && memchr::memchr(b'.', tail)
                .and_then(|end| std::str::from_utf8(&tail[..end]).ok())
                .and_then(Protocol::parse)
                .is_some()
    })
}

/// The calling client's default model when the body names a bare model this gateway cannot
/// resolve and the client maps unknown names; `None` otherwise.
async fn unknown_model_default(context: &Context, route: &Route, json: &Value) -> Option<String> {
    let model = json.get("model").and_then(Value::as_str)?;
    if !matches!(
        route,
        Route::Chat { .. }
            | Route::Embeddings
            | Route::Completions
            | Route::Rerank
            | Route::Decision(_)
    ) {
        return None;
    }
    let client = {
        let identity = context.identity();
        let client = identity.client()?;
        (client.unknown_model_to_default && client.default_model.is_some())
            .then(|| client.clone())?
    };
    let snapshot = context.llm.snapshot().await.ok()?;
    client
        .unknown_model_target(model, &|name: &str| snapshot.resolve_ref(name).is_some())
        .map(str::to_owned)
}

/// Whether the calling client may use `model` (see [`RoutingSnapshot::allowed_by`]); without a
/// snapshot only the name as sent counts.
async fn scope_allows(context: &Context, model: &str) -> bool {
    let identity = context.identity();
    match context.llm.snapshot().await {
        Ok(snapshot) => snapshot.allowed_by(model, &|name: &str| identity.allows(name)),
        Err(_) => identity.allows(model),
    }
}

/// The model list in the shape the request asks for, filtered to what the caller may use:
/// `<provider name>/<model>` references, each only when the client's allow-list (or
/// `allow_all`) includes it.
async fn models(context: &Context, parts: &hyper::http::request::Parts) -> Response<GatewayBody> {
    let snapshot: std::sync::Arc<RoutingSnapshot> = match context.llm.snapshot().await {
        Ok(snapshot) => snapshot,
        Err(error) => return error_response(None, 500, error.message(), "api_error"),
    };
    let identity = context.identity();
    let names: Vec<String> = snapshot
        .listed_models()
        .into_iter()
        .filter(|name| snapshot.allowed_by(name, &|name: &str| identity.allows(name)))
        .collect();
    let body = if parts.uri.path().starts_with("/v1beta")
        || parts.headers.contains_key("x-goog-api-key")
    {
        json!({"models": names.iter().map(|name| json!({
            "name": format!("models/{name}"),
            "displayName": name,
            "supportedGenerationMethods": ["generateContent", "streamGenerateContent"],
        })).collect::<Vec<_>>()})
    } else if parts.headers.contains_key("anthropic-version") {
        json!({
            "data": names.iter().map(|name| json!({
                "type": "model", "id": name, "display_name": name, "created_at": "1970-01-01T00:00:00Z",
            })).collect::<Vec<_>>(),
            "has_more": false,
            "first_id": names.first(),
            "last_id": names.last(),
        })
    } else {
        json!({"object": "list", "data": names.iter().map(|name| json!({
            "id": name, "object": "model", "created": 0, "owned_by": "pingdeck",
        })).collect::<Vec<_>>()})
    };
    json_response(200, &body)
}

/// The prompt size by the one estimate (`pingdeck_model::tokens`).
#[must_use]
pub(super) fn estimate_tokens(request: &HubRequest) -> u64 {
    pingdeck_model::tokens::request_tokens(request)
}

/// `response` marked as PingDeck's own estimate.
fn estimated(mut response: Response<GatewayBody>) -> Response<GatewayBody> {
    response
        .headers_mut()
        .insert(ESTIMATE_HEADER, HeaderValue::from_static("1"));
    response
}

/// `POST /v1/messages/count_tokens`: the count of the first Messages target of the model that
/// answers it (exact), else PingDeck's estimate, marked as one: no such target, or none of them
/// counted (a relay without the endpoint, an error, an unreachable address).
async fn count_tokens(context: &Context, inbound: Inbound) -> Response<GatewayBody> {
    if let Some(counted) = calls::count_tokens(context, &inbound).await {
        return counted;
    }
    let Some(codec) = codec(Protocol::AnthropicMessages) else {
        return error_response(None, 500, "the Anthropic codec is missing", "api_error");
    };
    match codec.decode_request(&inbound.json, &InboundHint::default()) {
        Ok(request) => estimated(json_response(
            200,
            &json!({"input_tokens": estimate_tokens(&request)}),
        )),
        Err(error) => error_response(
            Some(Protocol::AnthropicMessages),
            super::server::status_of(&error),
            error.message(),
            "invalid_request_error",
        ),
    }
}

/// Gemini `:countTokens`: PingDeck's estimate of the contents (or of `generateContentRequest`),
/// marked as one.
fn gemini_count_tokens(json: &Value) -> Response<GatewayBody> {
    let Some(codec) = codec(Protocol::Gemini) else {
        return error_response(None, 500, "the Gemini codec is missing", "api_error");
    };
    let body = json.get("generateContentRequest").unwrap_or(json);
    let hint = InboundHint {
        model: Some("count".to_owned()),
        stream: false,
    };
    match codec.decode_request(body, &hint) {
        Ok(request) => estimated(json_response(
            200,
            &json!({"totalTokens": estimate_tokens(&request)}),
        )),
        Err(error) => error_response(
            Some(Protocol::Gemini),
            super::server::status_of(&error),
            error.message(),
            "invalid_request_error",
        ),
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn routes_cover_every_documented_endpoint() {
        let chat = |protocol| Route::Chat {
            protocol,
            hint: InboundHint::default(),
        };
        assert_eq!(route(&Method::GET, "/health"), Route::Health);
        assert_eq!(route(&Method::GET, "/v1/models/"), Route::Models);
        assert_eq!(route(&Method::GET, "/v1beta/models"), Route::Models);
        assert_eq!(
            route(&Method::POST, "/v1/chat/completions"),
            chat(Protocol::OpenaiChat)
        );
        assert_eq!(
            route(&Method::POST, "/v1/responses"),
            chat(Protocol::OpenaiResponses)
        );
        assert_eq!(
            route(&Method::POST, "/v1/messages"),
            chat(Protocol::AnthropicMessages)
        );
        assert_eq!(
            route(&Method::POST, "/v1/messages/count_tokens"),
            Route::CountTokens
        );
        assert_eq!(route(&Method::POST, "/v1/embeddings"), Route::Embeddings);
        assert_eq!(route(&Method::POST, "/v1/completions"), Route::Completions);
        assert_eq!(route(&Method::POST, "/v1/rerank"), Route::Rerank);
        assert_eq!(
            route(&Method::POST, "/v1/systemone"),
            Route::Decision(Protocol::DecisionSystemone)
        );
        assert_eq!(
            route(&Method::POST, "/v1/decisions"),
            Route::Decision(Protocol::DecisionOpenai)
        );
        assert_eq!(route(&Method::GET, "/v1/chat/completions"), Route::NotFound);
        assert_eq!(route(&Method::POST, "/nope"), Route::NotFound);
    }

    #[test]
    fn gemini_routes_carry_the_model_and_stream_flag() {
        assert_eq!(
            route(
                &Method::POST,
                "/v1beta/models/gemini-x:streamGenerateContent"
            ),
            Route::Chat {
                protocol: Protocol::Gemini,
                hint: InboundHint {
                    model: Some("gemini-x".into()),
                    stream: true
                }
            }
        );
        assert_eq!(
            route(
                &Method::POST,
                "/v1beta/models/openai%2Fgpt-x:generateContent"
            ),
            Route::Chat {
                protocol: Protocol::Gemini,
                hint: InboundHint {
                    model: Some("openai/gpt-x".into()),
                    stream: false
                }
            }
        );
        assert_eq!(
            route(&Method::POST, "/v1beta/models/x:countTokens"),
            Route::GeminiCount("x".into())
        );
        assert_eq!(
            route(&Method::POST, "/v1beta/models/e:batchEmbedContents"),
            Route::GeminiEmbed {
                model: "e".into(),
                batch: true
            }
        );
        assert_eq!(
            route(&Method::POST, "/v1beta/models/x:countTokensNow"),
            Route::NotFound
        );
    }

    #[test]
    fn token_estimate_counts_text_tools_and_images() {
        let mut request = HubRequest::user_prompt("m", "x".repeat(400));
        request.system = Some("y".repeat(40));
        assert_eq!(estimate_tokens(&request), 110);
        request.messages.push(crate::llm::hub::HubMessage {
            role: crate::llm::hub::HubRole::User,
            content: vec![crate::llm::hub::HubPart::Image {
                image: crate::llm::hub::HubImage::Url { url: "u".into() },
            }],
        });
        assert_eq!(estimate_tokens(&request), 1610);
    }
}
