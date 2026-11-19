//! Protocol adapters: conversion between each wire protocol and the hub format.
//!
//! Each chat protocol has one [`ChatCodec`] (see [`codec`]) that works on both sides of the
//! gateway: toward an upstream it encodes hub requests and decodes responses and streams; toward a
//! gateway client it decodes requests and encodes responses, streams and errors. `decision` and
//! `embeddings` hold the probe helpers of the non-chat protocols; `generation` says which
//! reasoning parameters a model generation takes.

use serde_json::Value;

use crate::llm::hub::{HubEvent, HubRequest, HubResponse};
use crate::llm::sse::SseEvent;
use crate::llm::wire::{Protocol, Quirks};
use pingdeck_base::error::AppError;

pub(crate) mod anthropic;
pub mod decision;
pub mod embeddings;
pub(crate) mod gemini;
pub(crate) mod generation;
pub(crate) mod openai_chat;
pub(crate) mod openai_responses;
mod shared;
#[cfg(test)]
pub(crate) mod test_support;

pub(crate) use shared::{
    arguments_text, as_u32, effort_str, image_data_url, insert_some, non_null, object, object_map,
    parse_args_object, parse_base64_data_url, parse_effort, parse_sse_json, str_of, str_or_empty,
    take_str, to_u32, unix_now, utf16_len,
};

/// The hub name of the provider-run web search tool; Anthropic also uses it as the wire name.
pub(crate) const WEB_SEARCH_TOOL_NAME: &str = "web_search";

/// Per-call context for encoding a request toward an upstream.
#[derive(Debug, Clone, Copy)]
pub struct EncodeCtx<'a> {
    /// The provider's protocol quirks.
    pub quirks: &'a Quirks,
}

/// What the HTTP layer knows about an inbound request beyond its JSON body.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct InboundHint {
    /// The model named outside the body (the Gemini URL path, a replayed request); codecs use it
    /// when the body names none.
    pub model: Option<String>,
    /// A stream was asked for outside the body (Gemini `:streamGenerateContent`, a replay).
    pub stream: bool,
}

/// Turns upstream SSE events of one protocol into hub events.
pub trait StreamDecoder: Send {
    /// Decodes one SSE event into zero or more hub events.
    ///
    /// # Errors
    ///
    /// A network error for an overflow event, malformed data or a limit the stream exceeds.
    fn push(&mut self, event: &SseEvent) -> Result<Vec<HubEvent>, AppError>;
    /// Ends the stream, closing what the upstream left open (for example a missing `Finish`).
    ///
    /// # Errors
    ///
    /// A network error when the stream ended in a state that cannot be completed.
    fn finish(&mut self) -> Result<Vec<HubEvent>, AppError>;
}

/// Turns hub events into the bytes of a protocol's SSE stream.
pub trait StreamEncoder: Send {
    /// The SSE bytes for one hub event; empty when the event writes nothing yet.
    fn push(&mut self, event: &HubEvent) -> Vec<u8>;
    /// The closing frames of the stream; empty after an error or when called again.
    fn finish(&mut self) -> Vec<u8>;
}

/// Both directions of one chat protocol.
///
/// Upstream side: [`encode_request`](Self::encode_request),
/// [`decode_response`](Self::decode_response), [`stream_decoder`](Self::stream_decoder).
/// Gateway-client side: [`decode_request`](Self::decode_request),
/// [`encode_response`](Self::encode_response), [`stream_encoder`](Self::stream_encoder),
/// [`encode_error`](Self::encode_error).
pub trait ChatCodec: Sync {
    /// The protocol this codec speaks.
    fn protocol(&self) -> Protocol;
    /// Encodes a hub request as the upstream request body.
    ///
    /// # Errors
    ///
    /// A validation or unsupported error for content the protocol cannot carry.
    fn encode_request(&self, req: &HubRequest, ctx: &EncodeCtx<'_>) -> Result<Value, AppError>;
    /// Decodes an upstream response body.
    ///
    /// # Errors
    ///
    /// A network error for an error body or a body of the wrong shape.
    fn decode_response(&self, body: &Value) -> Result<HubResponse, AppError>;
    /// A fresh decoder for one upstream stream.
    fn stream_decoder(&self) -> Box<dyn StreamDecoder>;
    /// Decodes a gateway client's request body.
    ///
    /// # Errors
    ///
    /// A validation or unsupported error for a request the hub cannot represent.
    fn decode_request(&self, body: &Value, hint: &InboundHint) -> Result<HubRequest, AppError>;
    /// Encodes a hub response as the body a gateway client expects.
    fn encode_response(&self, response: &HubResponse) -> Value;
    /// A fresh encoder for one stream toward a gateway client.
    fn stream_encoder(&self) -> Box<dyn StreamEncoder>;
    /// The protocol's error body: `status` is the HTTP status (a protocol may ignore it) and
    /// `kind` the error type word.
    fn encode_error(&self, status: u16, message: &str, kind: &str) -> Value;
}

/// The codec for a chat protocol; `None` for embeddings and decision protocols.
#[must_use]
pub fn codec(protocol: Protocol) -> Option<&'static dyn ChatCodec> {
    match protocol {
        Protocol::OpenaiChat => Some(&openai_chat::OpenaiChat),
        Protocol::OpenaiResponses => Some(&openai_responses::OpenaiResponses),
        Protocol::AnthropicMessages => Some(&anthropic::AnthropicMessages),
        Protocol::Gemini => Some(&gemini::GeminiGenerate),
        Protocol::Embeddings | Protocol::DecisionSystemone | Protocol::DecisionOpenai => None,
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::llm::hub::{HubImage, HubTool, HubToolChoice};
    use crate::llm::sse::OVERFLOW_EVENT;

    const CHAT: [Protocol; 4] = [
        Protocol::OpenaiChat,
        Protocol::OpenaiResponses,
        Protocol::AnthropicMessages,
        Protocol::Gemini,
    ];

    /// The protocols that list tools directly under `tools`; Gemini nests them under
    /// `functionDeclarations`, so it is not among them.
    const FLAT_TOOL_PROTOCOLS: [Protocol; 3] = [
        Protocol::OpenaiChat,
        Protocol::OpenaiResponses,
        Protocol::AnthropicMessages,
    ];

    #[test]
    fn unresolved_image_errors_keep_each_protocols_message() {
        let mut request = HubRequest::user_prompt("m", "look");
        request.messages[0]
            .content
            .push(crate::llm::hub::HubPart::Image {
                image: HubImage::AttachmentRef {
                    attachment_id: "a".into(),
                    media_type: "image/png".into(),
                },
            });
        let ctx = EncodeCtx {
            quirks: &Quirks::default(),
        };
        for (protocol, message) in [
            (
                Protocol::OpenaiChat,
                "attachment image must be resolved before the request is encoded",
            ),
            (
                Protocol::OpenaiResponses,
                "image attachment must be resolved before the request is encoded",
            ),
        ] {
            assert_eq!(
                codec(protocol)
                    .unwrap()
                    .encode_request(&request, &ctx)
                    .unwrap_err()
                    .message(),
                message
            );
        }
    }

    #[test]
    fn every_stream_decoder_rejects_an_sse_overflow() {
        let overflow = SseEvent {
            event: Some(OVERFLOW_EVENT.to_owned()),
            data: "too big".to_owned(),
            id: None,
        };
        for protocol in CHAT {
            let err = codec(protocol)
                .unwrap()
                .stream_decoder()
                .push(&overflow)
                .unwrap_err();
            assert!(err.to_string().contains("too big"), "{protocol:?}");
        }
    }

    #[test]
    fn an_allow_list_tool_choice_filters_the_tools() {
        let tool = |name: &str| HubTool {
            name: name.to_owned(),
            description: None,
            parameters: json!({"type": "object"}),
            strict: None,
        };
        let req = HubRequest {
            tools: vec![tool("f"), tool("g"), tool("h")],
            tool_choice: Some(HubToolChoice::Allowed {
                names: vec!["f".to_owned(), "h".to_owned()],
            }),
            ..HubRequest::user_prompt("m", "hi")
        };
        let ctx = EncodeCtx {
            quirks: &Quirks::default(),
        };
        for protocol in FLAT_TOOL_PROTOCOLS {
            let body = codec(protocol).unwrap().encode_request(&req, &ctx).unwrap();
            let names: Vec<&str> = body["tools"]
                .as_array()
                .unwrap()
                .iter()
                .map(|t| {
                    t["name"]
                        .as_str()
                        .or_else(|| t["function"]["name"].as_str())
                        .unwrap()
                })
                .collect();
            assert_eq!(names, ["f", "h"], "{protocol:?}");
        }
    }
}
