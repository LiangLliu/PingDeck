//! Unit tests for the Anthropic Messages codec, grouped by direction.

mod decode;
mod encode;
mod stream;
mod thinking;

use serde_json::{Value, json};

use super::*;
use crate::llm::adapters::generation::claude_prefill_generation as claude_generation;
use crate::llm::adapters::test_support::{self, tool_call as call};
use crate::llm::adapters::{ChatCodec, EncodeCtx, InboundHint};
use crate::llm::hub::*;
use crate::llm::wire::{Protocol, Quirks, ReasoningStyle};

pub(super) fn encode(req: &HubRequest, quirks: &Quirks) -> Value {
    test_support::encode_with(&AnthropicMessages, req, quirks)
}

pub(super) fn try_encode(req: &HubRequest, quirks: &Quirks) -> Result<Value, AppError> {
    test_support::try_encode(&AnthropicMessages, req, quirks)
}

pub(super) fn reasoning(text: &str, signature: Option<&str>, encrypted: Option<&str>) -> HubPart {
    HubPart::Reasoning {
        text: text.into(),
        signature: signature.map(Into::into),
        encrypted: encrypted.map(Into::into),
        id: None,
        origin: None,
    }
}

pub(super) fn result(id: &str, text: &str, is_error: bool) -> HubPart {
    HubPart::ToolResult {
        call_id: id.into(),
        name: None,
        content: vec![HubPart::text(text)],
        is_error,
    }
}

pub(super) fn decode_stream(raw: &str) -> Vec<HubEvent> {
    test_support::decode_sse_text(&AnthropicMessages, raw)
}

pub(super) fn reencode(events: &[HubEvent]) -> (String, Vec<HubEvent>) {
    test_support::reencode(&AnthropicMessages, events)
}
