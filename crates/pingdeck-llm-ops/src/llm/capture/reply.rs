//! The merged reply of the debug drawer: the assistant output reassembled from a stored response
//! body or stored frames with the hub decoders of `pingdeck-llm-protocol`, so the webview never
//! decodes a protocol itself. Pure; works on redacted, possibly cut or elided captures.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::model::{CapturedBody, CapturedFrames};
use crate::llm::adapters::{self, embeddings};
use crate::llm::hub::{HubEvent, HubFinish, HubPart, HubResponse, HubUsage, collect_events};
use crate::llm::sse::SseEvent;
use crate::llm::wire::Protocol;

/// How many embedding values the drawer shows.
const EMBEDDING_PREVIEW: usize = 8;

/// What to merge.
#[derive(Debug, Clone, Copy)]
pub enum MergedSource<'a> {
    /// A non-streaming response body.
    Body(&'a CapturedBody),
    /// The frames of a streamed response.
    Frames(&'a CapturedFrames),
}

/// One tool call of the reply.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MergedToolCall {
    /// Function name.
    pub name: String,
    /// Arguments as the JSON text the model produced.
    pub arguments: String,
}

/// An embeddings reply: dimension count and the first values (there is no text).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EmbeddingSummary {
    /// Length of the vector.
    pub dimensions: usize,
    /// The first 8 values.
    pub first_values: Vec<f64>,
}

/// The reassembled reply, plain text only.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct MergedReply {
    /// Reasoning text (the collapsed "Reasoning" block).
    pub reasoning: String,
    /// The answer text.
    pub text: String,
    /// Tool calls in order.
    pub tool_calls: Vec<MergedToolCall>,
    /// Why generation stopped: `stop`, `length`, `tool_calls`, `content_filter` or the provider's word.
    pub finish_reason: Option<String>,
    /// Token accounting, when the response carried any.
    pub usage: Option<HubUsage>,
    /// Embeddings replies only.
    pub embedding: Option<EmbeddingSummary>,
    /// The stored data is not the whole response (frames or body cut); the reply is what fits.
    pub partial: bool,
    /// Why the reply could not be (fully) merged: an error body, a decode failure, a provider
    /// error event inside the stream.
    pub error: Option<String>,
}

impl MergedReply {
    fn failed(error: impl Into<String>) -> Self {
        Self {
            error: Some(error.into()),
            ..Self::default()
        }
    }

    fn from_response(response: &HubResponse) -> Self {
        let reasoning: Vec<&str> = response
            .content
            .iter()
            .filter_map(|part| match part {
                HubPart::Reasoning { text, .. } if !text.is_empty() => Some(text.as_str()),
                _ => None,
            })
            .collect();
        Self {
            reasoning: reasoning.join("\n\n"),
            text: response.text(),
            tool_calls: response
                .tool_calls()
                .into_iter()
                .map(|(_, name, arguments)| MergedToolCall {
                    name: name.to_owned(),
                    arguments: arguments.to_owned(),
                })
                .collect(),
            finish_reason: Some(finish_word(&response.finish)),
            usage: Some(response.usage.clone()),
            ..Self::default()
        }
    }
}

fn finish_word(finish: &HubFinish) -> String {
    match finish {
        HubFinish::Stop => "stop".to_owned(),
        HubFinish::Length => "length".to_owned(),
        HubFinish::Pause => "pause".to_owned(),
        HubFinish::ToolCalls => "tool_calls".to_owned(),
        HubFinish::ContentFilter => "content_filter".to_owned(),
        HubFinish::Other(other) => other.clone(),
    }
}

/// Reassembles the reply `source` holds, decoding it as `protocol` (the protocol of the stored
/// side: the client's for client frames and bodies, the provider's for upstream ones).
#[must_use]
pub fn merged_reply(protocol: Protocol, source: MergedSource<'_>) -> MergedReply {
    match (protocol, source) {
        (Protocol::Embeddings, MergedSource::Body(body)) => embedding_reply(body),
        (Protocol::Embeddings, MergedSource::Frames(_)) => {
            MergedReply::failed("Embeddings are not streamed")
        }
        (_, MergedSource::Body(body)) => body_reply(protocol, body),
        (_, MergedSource::Frames(frames)) => frames_reply(protocol, frames),
    }
}

fn stored_json(body: &CapturedBody) -> Result<Value, Box<MergedReply>> {
    let Some(text) = body.text.as_deref() else {
        return Err(Box::new(MergedReply::failed("No response body was stored")));
    };
    serde_json::from_str(text).map_err(|_| {
        let mut reply = MergedReply::failed(if body.truncated {
            "The stored body was cut at 256 KiB and cannot be merged"
        } else {
            "The response body is not valid JSON"
        });
        reply.partial = body.truncated;
        Box::new(reply)
    })
}

fn body_reply(protocol: Protocol, body: &CapturedBody) -> MergedReply {
    let Some(codec) = adapters::codec(protocol) else {
        return MergedReply::failed("This protocol has no merged reply");
    };
    let json = match stored_json(body) {
        Ok(json) => json,
        Err(reply) => return *reply,
    };
    match codec.decode_response(&json) {
        Ok(response) => MergedReply::from_response(&response),
        Err(error) => MergedReply::failed(error.to_string()),
    }
}

fn frames_reply(protocol: Protocol, frames: &CapturedFrames) -> MergedReply {
    let Some(codec) = adapters::codec(protocol) else {
        return MergedReply::failed("This protocol has no merged reply");
    };
    let mut decoder = codec.stream_decoder();
    let mut events: Vec<HubEvent> = Vec::new();
    let mut error = None;
    for frame in &frames.frames {
        let event = SseEvent {
            event: (frame.event != "message").then(|| frame.event.clone()),
            data: frame.data.clone(),
            id: None,
        };
        match decoder.push(&event) {
            Ok(decoded) => events.extend(decoded),
            Err(failure) => {
                error = Some(failure.to_string());
                break;
            }
        }
    }
    if error.is_none() {
        match decoder.finish() {
            Ok(decoded) => events.extend(decoded),
            Err(failure) => error = Some(failure.to_string()),
        }
    }
    let stream_error = events.iter().find_map(|event| match event {
        HubEvent::Error { message, .. } => Some(message.clone()),
        _ => None,
    });
    events.retain(|event| !matches!(event, HubEvent::Error { .. }));
    let mut reply = match collect_events(&events) {
        Ok(response) => MergedReply::from_response(&response),
        Err(message) => MergedReply::failed(message),
    };
    reply.partial = frames.truncated;
    reply.error = reply.error.or(stream_error).or(error);
    reply
}

fn embedding_reply(body: &CapturedBody) -> MergedReply {
    let json = match stored_json(body) {
        Ok(json) => json,
        Err(reply) => return *reply,
    };
    let values = json
        .pointer("/data/0/embedding")
        .or_else(|| json.pointer("/embedding/values"))
        .and_then(Value::as_array);
    let Some(values) = values else {
        return MergedReply::failed("The response holds no embedding vector");
    };
    MergedReply {
        embedding: Some(EmbeddingSummary {
            dimensions: values.len(),
            first_values: values
                .iter()
                .take(EMBEDDING_PREVIEW)
                .filter_map(Value::as_f64)
                .collect(),
        }),
        usage: Some(embeddings::usage(&json)),
        ..MergedReply::default()
    }
}
