//! Unit tests for the OpenAI Chat Completions codec, grouped by direction.

mod decode;
mod encode;
mod stream;

use serde_json::{Value, json};

use super::*;
use crate::llm::adapters::test_support;
use crate::llm::hub::{
    HubDocument, HubDocumentSource, HubEffort, HubEvent, HubImage, HubMessage, HubReasoning,
    HubResponseFormat, HubRole, HubTool, HubToolChoice, collect_events,
};
use crate::llm::wire::{Quirks, ReasoningStyle};

pub(super) fn try_encode(req: &HubRequest, quirks: &Quirks) -> Result<Value, AppError> {
    test_support::try_encode(&OpenaiChat, req, quirks)
}

pub(super) fn encode(req: &HubRequest, quirks: &Quirks) -> Value {
    test_support::encode_with(&OpenaiChat, req, quirks)
}

pub(super) fn base() -> HubRequest {
    HubRequest::user_prompt("gpt-x", "hi")
}

pub(super) fn effort(effort: HubEffort) -> HubReasoning {
    HubReasoning {
        effort: Some(effort),
        ..HubReasoning::default()
    }
}

pub(super) use test_support::tool_call as call;
