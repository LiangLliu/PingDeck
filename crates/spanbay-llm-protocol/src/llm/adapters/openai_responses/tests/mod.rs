//! Unit tests for the OpenAI Responses codec, grouped by direction: upstream requests and client
//! requests, whole responses, and streams.

mod request;
mod response;
mod stream;
mod stream_frames;

use serde_json::{Value, json};

use super::*;
use crate::llm::adapters::test_support;
use crate::llm::hub::{
    HubCitation, HubDocument, HubDocumentSource, HubEffort, HubEvent, HubFinish, HubImage,
    HubMessage, HubNativeTool, HubPart, HubReasoning, HubRequest, HubResponse, HubResponseFormat,
    HubRole, HubTool, HubToolChoice, HubUsage, collect_events,
};
use crate::llm::wire::{Protocol, Quirks, ReasoningStyle};

pub(super) use test_support::tool_call;

/// Encodes `req` toward an upstream with `quirks`.
pub(super) fn try_encode(req: &HubRequest, quirks: &Quirks) -> Result<Value, AppError> {
    test_support::try_encode(&OpenaiResponses, req, quirks)
}

/// [`try_encode`] for requests that must encode.
pub(super) fn encode(req: &HubRequest, quirks: &Quirks) -> Value {
    test_support::encode_with(&OpenaiResponses, req, quirks)
}

/// Decodes a client request body without an inbound hint.
pub(super) fn decode_request(body: &Value) -> Result<HubRequest, AppError> {
    OpenaiResponses.decode_request(body, &InboundHint::default())
}

/// A citation titled `T`.
pub(super) fn citation(url: &str, start: Option<u32>, end: Option<u32>) -> HubCitation {
    HubCitation {
        url: url.into(),
        title: "T".into(),
        snippet: None,
        start_index: start,
        end_index: end,
    }
}
