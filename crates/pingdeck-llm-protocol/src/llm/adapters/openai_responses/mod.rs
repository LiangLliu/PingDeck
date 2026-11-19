//! OpenAI Responses API codec, both directions.
//!
//! The codec value itself holds no state; the state of one stream lives in its
//! `ResponsesDecoder` or `ResponsesEncoder`. Layout:
//! - `encode`: hub requests toward an upstream, hub responses toward a client;
//! - `encode_items`: the output items and response envelope shared by `encode` and
//!   `stream_encode`;
//! - `decode`: upstream replies and client requests into hub values;
//! - `decode_items`: reading output items, usage and status, shared by `decode` and
//!   `stream_decode`;
//! - `options`: tools, tool choice, text format and reasoning, in both directions;
//! - `stream_decode` / `stream_encode`: the two stream state machines; `stream_emit` writes the
//!   frames of the client stream.

mod decode;
mod decode_items;
mod encode;
mod encode_items;
mod options;
mod stream_decode;
mod stream_emit;
mod stream_encode;
#[cfg(test)]
mod tests;

use serde_json::{Value, json};

use self::stream_decode::ResponsesDecoder;
use self::stream_encode::ResponsesEncoder;
use crate::llm::adapters::{ChatCodec, EncodeCtx, InboundHint, StreamDecoder, StreamEncoder};
use crate::llm::hub::{HubRequest, HubResponse};
use crate::llm::wire::Protocol;
use pingdeck_base::error::AppError;

/// The wire `type` of the web search tool declaration; the hub name is
/// `adapters::WEB_SEARCH_TOOL_NAME`.
const WEB_SEARCH: &str = "web_search";
/// The protocol this codec speaks, as the origin of signatures, item ids and encrypted payloads.
const OWN: Protocol = Protocol::OpenaiResponses;
/// The response id a stream toward a client carries until a `Start` event names one.
const DEFAULT_RESPONSE_ID: &str = "resp_pingdeck";

/// The OpenAI Responses API (`/v1/responses`) codec.
pub(crate) struct OpenaiResponses;

impl ChatCodec for OpenaiResponses {
    fn protocol(&self) -> Protocol {
        OWN
    }

    fn encode_request(&self, req: &HubRequest, ctx: &EncodeCtx<'_>) -> Result<Value, AppError> {
        encode::request(req, ctx)
    }

    fn decode_response(&self, body: &Value) -> Result<HubResponse, AppError> {
        decode::response(body)
    }

    fn stream_decoder(&self) -> Box<dyn StreamDecoder> {
        Box::new(ResponsesDecoder::default())
    }

    fn decode_request(&self, body: &Value, hint: &InboundHint) -> Result<HubRequest, AppError> {
        decode::request(body, hint)
    }

    fn encode_response(&self, response: &HubResponse) -> Value {
        encode::response(response)
    }

    fn stream_encoder(&self) -> Box<dyn StreamEncoder> {
        Box::new(ResponsesEncoder::new())
    }

    fn encode_error(&self, _status: u16, message: &str, kind: &str) -> Value {
        json!({"error": {"message": message, "type": kind, "param": null, "code": null}})
    }
}
