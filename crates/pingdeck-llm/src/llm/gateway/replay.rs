//! Replay of a logged client request through the gateway's own handler.
//!
//! Single responsibility: re-send a stored client request in-process, with no HTTP hop, no token,
//! and no Host or scope check: it tests routing and the upstream, not the client's own path. The
//! request runs through the same pipeline as a wire request (`chat`, `calls`), so it plans,
//! fails over, records health and writes a new log row with `source = gateway`, client "PingDeck
//! replay" and `replay_of` set to the original row. It needs the runtime only: a stopped gateway
//! replays too. The answer streams into an [`EventSink`]; a cancel token ends the upstream request
//! at once and the row is `canceled`.
//!
//! Two modes: [`ReplayMode::Replay`] resolves the requested name again (current routing);
//! [`ReplayMode::ReplayOnServedTarget`] pins the provider and model that served the original.

use std::net::{Ipv4Addr, SocketAddr};
use std::sync::Arc;

use bytes::Bytes;
use http_body_util::BodyExt;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::llm::adapters::InboundHint;
use crate::llm::runtime::LlmRuntime;
use crate::llm::store;
use crate::llm::wire::Protocol;
use pingdeck_base::cancel::{CancelToken, until_cancelled};
use pingdeck_base::error::AppError;
use pingdeck_model::events::EventSink;

use super::Limits;
use super::auth::Identity;
use super::calls;
use super::chat::{self, Inbound};
use super::lan::LanNames;
use super::server::{Context, Shared};

/// Which target a replay calls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReplayMode {
    /// Resolve the requested alias again, with the current routing.
    Replay,
    /// Call the `provider/model` that served the original.
    ReplayOnServedTarget,
}

/// What a replay tells its listener.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ReplayEvent {
    /// A piece of the client's answer, as the client would have received it: a chunk of the SSE
    /// stream, or the whole JSON body.
    Frames {
        /// The text (lossy UTF-8).
        text: String,
    },
    /// The replay ended (finished, failed or stopped). `new_log_id` is its row, once stored.
    Done {
        /// The row the replay wrote; `None` when the log lost it.
        new_log_id: Option<i64>,
    },
}

/// What is replayed.
#[derive(Debug, Clone)]
pub struct ReplayRequest {
    /// The log row being replayed.
    pub of: i64,
    /// The protocol the client spoke.
    pub protocol: Protocol,
    /// The model the client asked for (a Gemini request carries it in its URL, not its body).
    pub requested_model: String,
    /// The client asked for a stream.
    pub stream: bool,
    /// The stored client request body.
    pub body: Bytes,
    /// In [`ReplayMode::ReplayOnServedTarget`]: the serving provider id and upstream model.
    pub pin: Option<(String, String)>,
}

/// The pipeline context of a replay: a gateway-shaped state over the runtime, no listener.
fn context(llm: &Arc<LlmRuntime>, of: i64) -> Result<Context, AppError> {
    let shared = Shared::new(
        Arc::clone(llm),
        0,
        false,
        Limits::default(),
        Arc::new(LanNames::system()),
    )?;
    let context = Context::new(Arc::new(shared), SocketAddr::from((Ipv4Addr::LOCALHOST, 0)));
    context.set_identity(Identity::Replay { of });
    Ok(context)
}

/// Re-sends `request`, streaming the answer into `sink`, and returns the new row's id once it
/// is stored (`None` when the log lost it). `cancel` stops the upstream request.
///
/// # Errors
/// [`AppError::Validation`] when the stored body is not JSON; [`AppError::Internal`] when the
/// log writer cannot start.
pub async fn replay(
    llm: &Arc<LlmRuntime>,
    request: ReplayRequest,
    sink: &dyn EventSink<ReplayEvent>,
    cancel: &CancelToken,
) -> Result<Option<i64>, AppError> {
    let json: Value = serde_json::from_slice(&request.body).map_err(|error| {
        AppError::validation(format!("the stored request is not JSON: {error}"))
    })?;
    let context = context(llm, request.of)?;
    let tagged = super::handlers::carries_signature_tag(&request.body);
    let inbound = Inbound {
        protocol: request.protocol,
        hint: if request.protocol == Protocol::Gemini {
            InboundHint {
                model: Some(request.requested_model.clone()),
                stream: request.stream,
            }
        } else {
            InboundHint::default()
        },
        bytes: request.body,
        json,
        tagged,
        headers: Vec::new(),
        pin: request.pin,
        routed: None,
        cancel: Some(cancel.clone()),
    };
    let response = match request.protocol {
        Protocol::Embeddings => calls::embeddings(&context, inbound).await,
        Protocol::DecisionOpenai | Protocol::DecisionSystemone => {
            calls::decision(&context, inbound).await
        }
        _ => chat::chat(&context, inbound).await,
    };
    let mut body = response.into_body();
    while let Some(frame) = until_cancelled(Some(cancel), body.frame())
        .await
        .flatten()
        .and_then(Result::ok)
    {
        if let Some(data) = frame.data_ref()
            && !data.is_empty()
            && !sink.emit(ReplayEvent::Frames {
                text: String::from_utf8_lossy(data).into_owned(),
            })
        {
            break;
        }
    }
    // Dropping the body ends a stream that is still running: the upstream request closes and the
    // row is written as canceled.
    drop(body);
    let request_id = context.request_id.clone();
    let queue = llm.log()?;
    let llm = Arc::clone(llm);
    let id = tokio::task::spawn_blocking(move || {
        queue.flush();
        let connection = llm.db().open().ok()?;
        store::log_id_for_request_id(&connection, &request_id)
            .ok()
            .flatten()
    })
    .await
    .map_err(|error| AppError::internal(format!("log task failed: {error}")))?;
    sink.emit(ReplayEvent::Done { new_log_id: id });
    Ok(id)
}
