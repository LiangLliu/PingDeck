//! Stream decoding (upstream) of the Anthropic Messages codec.
//!
//! Events are parsed as untyped JSON (a repeated key keeps its last value) and strings are moved
//! out with `take_str`; a field of the wrong JSON type reads as absent. An unknown event type is
//! ignored, while an unknown or missing content block type fails the stream.

use serde_json::{Value, json};

use super::decode_blocks::{citation, search_hits};
use super::{OWN, decode_stop_reason, read_usage};
use crate::llm::adapters::{StreamDecoder, WEB_SEARCH_TOOL_NAME, parse_sse_json, take_str};
use crate::llm::hub::{HubEvent, HubFinish, HubUsage, MAX_TOOL_ARGUMENT_BYTES, too_large};
use crate::llm::sse::SseEvent;
use pingdeck_base::error::AppError;

/// Decodes one upstream `/v1/messages` stream into hub events.
#[derive(Default)]
pub(super) struct Decoder {
    /// A `message_start` arrived, so `finish` must close the message.
    started: bool,
    /// A `message_stop` or `error` already ended the message.
    finished: bool,
    /// The stop reason of the last `message_delta`, used by the closing `Finish`.
    stop: Option<HubFinish>,
    /// Usage reported so far; the base for reading partial `message_delta` usage.
    usage: HubUsage,
    /// Which hub tool index each upstream `tool_use` block maps to.
    tools: Vec<ToolSlot>,
    next_tool: u32,
    /// `server_tool_use` blocks buffered until their `content_block_stop`.
    pending: Vec<PendingServerTool>,
    /// Chunked `signature_delta` fragments buffered per thinking block until `content_block_stop`.
    pending_signatures: Vec<(u64, String)>,
}

/// An upstream `tool_use` block and the hub tool index it was given.
struct ToolSlot {
    block: u64,
    tool: u32,
}

/// A server tool call whose input is still streaming.
struct PendingServerTool {
    /// The upstream block index.
    block: u64,
    id: String,
    name: String,
    /// The input given at `content_block_start`, used when no JSON delta follows.
    input: Value,
    /// The `input_json_delta` fragments so far.
    json: String,
}

impl Decoder {
    fn usage_event(&mut self, usage: &Value, out: &mut Vec<HubEvent>) {
        let read = read_usage(usage, &self.usage);
        if read != HubUsage::default() {
            self.usage.merge(&read);
            out.push(HubEvent::Usage { usage: read });
        }
    }

    fn tool_index(&self, block_index: u64) -> Option<u32> {
        self.tools
            .iter()
            .rev()
            .find(|slot| slot.block == block_index)
            .map(|slot| slot.tool)
    }

    /// The closing `Finish`, carrying the last stop reason (`Stop` when there was none).
    fn finish_event(&mut self) -> HubEvent {
        self.finished = true;
        HubEvent::Finish {
            reason: self.stop.take().unwrap_or(HubFinish::Stop),
        }
    }

    fn on_message_start(&mut self, value: &mut Value, out: &mut Vec<HubEvent>) {
        let mut message = value
            .get_mut("message")
            .map(Value::take)
            .unwrap_or_default();
        self.started = true;
        out.push(HubEvent::Start {
            id: take_str(&mut message, "id").unwrap_or_default(),
            model: take_str(&mut message, "model").unwrap_or_default(),
        });
        out.push(HubEvent::Origin { protocol: OWN });
        if let Some(usage) = message.get("usage") {
            self.usage_event(usage, out);
        }
    }

    fn flush_pending_signatures(&mut self, out: &mut Vec<HubEvent>) {
        for (_, signature) in self.pending_signatures.drain(..) {
            out.push(HubEvent::ReasoningSignature { signature });
        }
    }

    fn on_block_start(
        &mut self,
        value: &mut Value,
        index: u64,
        out: &mut Vec<HubEvent>,
    ) -> Result<(), AppError> {
        // Flush any unclosed thinking block's signature before opening a later block.
        if !self.pending_signatures.is_empty() {
            let mut i = 0;
            while i < self.pending_signatures.len() {
                if self.pending_signatures[i].0 != index {
                    let (_, signature) = self.pending_signatures.remove(i);
                    out.push(HubEvent::ReasoningSignature { signature });
                } else {
                    i += 1;
                }
            }
        }
        let mut block = value
            .get_mut("content_block")
            .map(Value::take)
            .unwrap_or_default();
        match take_str(&mut block, "type").as_deref() {
            Some("tool_use") => {
                let tool = self.next_tool;
                self.next_tool += 1;
                self.tools.push(ToolSlot { block: index, tool });
                out.push(HubEvent::ToolCallStart {
                    index: tool,
                    id: take_str(&mut block, "id").unwrap_or_default(),
                    name: take_str(&mut block, "name").unwrap_or_default(),
                    signature: None,
                });
            }
            Some("text") => push_text(out, take_str(&mut block, "text"), text_delta),
            Some("thinking") => push_text(out, take_str(&mut block, "thinking"), reasoning_delta),
            Some("redacted_thinking") => {
                if let Some(data) = take_str(&mut block, "data") {
                    out.push(HubEvent::RedactedReasoning { data });
                }
            }
            Some("server_tool_use") => self.pending.push(PendingServerTool {
                block: index,
                id: take_str(&mut block, "id").unwrap_or_default(),
                name: take_str(&mut block, "name").unwrap_or_default(),
                input: block
                    .get_mut("input")
                    .map(Value::take)
                    .unwrap_or_else(|| json!({})),
                json: String::new(),
            }),
            Some("web_search_tool_result") => {
                let payload = block
                    .get_mut("content")
                    .map(Value::take)
                    .unwrap_or_else(|| json!([]));
                let call_id = take_str(&mut block, "tool_use_id").unwrap_or_default();
                let citations = search_hits(&payload);
                out.push(HubEvent::ServerToolResult {
                    call_id,
                    name: WEB_SEARCH_TOOL_NAME.to_owned(),
                    payload,
                    citations,
                });
            }
            other => {
                return Err(AppError::network(format!(
                    "anthropic stream has an unsupported content block `{}`",
                    other.unwrap_or("missing")
                )));
            }
        }
        Ok(())
    }

    fn on_block_delta(
        &mut self,
        value: &mut Value,
        index: u64,
        out: &mut Vec<HubEvent>,
    ) -> Result<(), AppError> {
        let mut delta = value.get_mut("delta").map(Value::take).unwrap_or_default();
        match take_str(&mut delta, "type").as_deref() {
            Some("text_delta") => push_text(out, take_str(&mut delta, "text"), text_delta),
            Some("thinking_delta") => {
                push_text(out, take_str(&mut delta, "thinking"), reasoning_delta);
            }
            Some("signature_delta") => {
                if let Some(signature) = take_str(&mut delta, "signature") {
                    if let Some((_, buf)) = self
                        .pending_signatures
                        .iter_mut()
                        .find(|(block, _)| *block == index)
                    {
                        if buf.len() + signature.len() > MAX_TOOL_ARGUMENT_BYTES {
                            return Err(AppError::network(too_large(
                                "reasoning signature",
                                MAX_TOOL_ARGUMENT_BYTES,
                            )));
                        }
                        buf.push_str(&signature);
                    } else {
                        if signature.len() > MAX_TOOL_ARGUMENT_BYTES {
                            return Err(AppError::network(too_large(
                                "reasoning signature",
                                MAX_TOOL_ARGUMENT_BYTES,
                            )));
                        }
                        self.pending_signatures.push((index, signature));
                    }
                }
            }
            Some("input_json_delta") => {
                if let Some(arguments) =
                    take_str(&mut delta, "partial_json").filter(|a| !a.is_empty())
                {
                    self.on_input_json(index, arguments, out)?;
                }
            }
            Some("citations_delta") => {
                if let Some(citation) = delta.get("citation").and_then(citation) {
                    out.push(HubEvent::Citation { citation });
                }
            }
            _ => {}
        }
        Ok(())
    }

    /// Routes an argument fragment to a pending server tool (capped) or a client tool call.
    fn on_input_json(
        &mut self,
        index: u64,
        arguments: String,
        out: &mut Vec<HubEvent>,
    ) -> Result<(), AppError> {
        if let Some(call) = self.pending.iter_mut().find(|p| p.block == index) {
            if call.json.len() + arguments.len() > MAX_TOOL_ARGUMENT_BYTES {
                return Err(AppError::network(too_large(
                    "server tool input",
                    MAX_TOOL_ARGUMENT_BYTES,
                )));
            }
            call.json.push_str(&arguments);
        } else if let Some(tool) = self.tool_index(index) {
            out.push(HubEvent::ToolCallDelta {
                index: tool,
                arguments,
            });
        }
        Ok(())
    }

    /// Emits any buffered thinking signature or server tool call once its block closes.
    fn on_block_stop(&mut self, index: u64, out: &mut Vec<HubEvent>) -> Result<(), AppError> {
        if let Some(pos) = self
            .pending_signatures
            .iter()
            .position(|(block, _)| *block == index)
        {
            let (_, signature) = self.pending_signatures.remove(pos);
            out.push(HubEvent::ReasoningSignature { signature });
        }
        let Some(at) = self.pending.iter().position(|p| p.block == index) else {
            return Ok(());
        };
        let call = self.pending.remove(at);
        let input = if call.json.trim().is_empty() {
            call.input
        } else {
            serde_json::from_str(&call.json).map_err(|e| {
                AppError::network(format!(
                    "invalid anthropic server tool input for `{}`: {e}",
                    call.name
                ))
            })?
        };
        out.push(HubEvent::ServerToolStart {
            id: call.id,
            name: call.name,
            input,
        });
        Ok(())
    }

    fn on_message_delta(&mut self, value: &Value, out: &mut Vec<HubEvent>) {
        if let Some(reason) = value.pointer("/delta/stop_reason").and_then(Value::as_str) {
            self.stop = Some(decode_stop_reason(Some(reason)));
        }
        if let Some(usage) = value.get("usage") {
            self.usage_event(usage, out);
        }
    }

    fn on_error(&mut self, value: &mut Value, out: &mut Vec<HubEvent>) {
        self.finished = true;
        self.pending.clear();
        self.pending_signatures.clear();
        let mut error = value.get_mut("error").map(Value::take).unwrap_or_default();
        out.push(HubEvent::Error {
            message: take_str(&mut error, "message").unwrap_or_else(|| "upstream error".to_owned()),
            kind: take_str(&mut error, "type"),
        });
    }
}

impl StreamDecoder for Decoder {
    fn push(&mut self, event: &SseEvent) -> Result<Vec<HubEvent>, AppError> {
        let Some(mut value) = parse_sse_json(event, "anthropic stream event")? else {
            return Ok(Vec::new());
        };
        // The payload's `type` wins over the SSE `event:` name.
        let kind = take_str(&mut value, "type");
        let kind = kind
            .as_deref()
            .or(event.event.as_deref())
            .unwrap_or_default();
        let index = value.get("index").and_then(Value::as_u64).unwrap_or(0);
        let mut out = Vec::new();
        match kind {
            "message_start" => self.on_message_start(&mut value, &mut out),
            "content_block_start" => self.on_block_start(&mut value, index, &mut out)?,
            "content_block_delta" => self.on_block_delta(&mut value, index, &mut out)?,
            "content_block_stop" => self.on_block_stop(index, &mut out)?,
            "message_delta" => self.on_message_delta(&value, &mut out),
            "message_stop" => {
                self.flush_pending_signatures(&mut out);
                out.push(self.finish_event());
            }
            "error" => self.on_error(&mut value, &mut out),
            _ => {}
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<HubEvent>, AppError> {
        if !self.pending.is_empty() {
            self.pending.clear();
            self.pending_signatures.clear();
            return Err(AppError::network(
                "anthropic stream ended inside a server tool call",
            ));
        }
        let mut out = Vec::new();
        self.flush_pending_signatures(&mut out);
        if self.started && !self.finished {
            out.push(self.finish_event());
        }
        Ok(out)
    }
}

fn text_delta(text: String) -> HubEvent {
    HubEvent::TextDelta { text }
}

fn reasoning_delta(text: String) -> HubEvent {
    HubEvent::ReasoningDelta { text }
}

/// Pushes `make(text)` unless the text is missing or empty.
fn push_text(out: &mut Vec<HubEvent>, text: Option<String>, make: fn(String) -> HubEvent) {
    if let Some(text) = text.filter(|t| !t.is_empty()) {
        out.push(make(text));
    }
}
