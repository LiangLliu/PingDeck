//! Unit tests of the Gemini codec, grouped by direction.

mod decode;
mod encode;
mod stream;

use super::*;
use crate::llm::adapters::test_support;
use crate::llm::hub::*;
use crate::llm::wire::Quirks;

pub(super) fn try_encode(req: &HubRequest) -> Result<Value, AppError> {
    test_support::try_encode(&GeminiGenerate, req, &Quirks::default())
}

pub(super) fn encode(req: &HubRequest) -> Value {
    try_encode(req).unwrap()
}

pub(super) fn hint(stream: bool) -> InboundHint {
    InboundHint {
        model: Some("gemini-x".into()),
        stream,
    }
}

pub(super) fn call(id: &str, name: &str, arguments: &str, signature: Option<&str>) -> HubPart {
    HubPart::ToolCall {
        id: id.into(),
        name: name.into(),
        arguments: arguments.into(),
        signature: signature.map(str::to_owned),
        origin: signature.map(|_| Protocol::Gemini),
    }
}

pub(super) fn reasoning(text: &str, signature: Option<&str>) -> HubPart {
    HubPart::Reasoning {
        text: text.into(),
        signature: signature.map(str::to_owned),
        encrypted: None,
        id: None,
        origin: signature.map(|_| Protocol::Gemini),
    }
}
