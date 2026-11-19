//! Stream encoding (toward a gateway client) of the Anthropic Messages codec.

use std::time::{SystemTime, UNIX_EPOCH};

use serde_json::{Map, Value, json};

use super::encode_blocks::{server_tool_result_block, server_tool_use_block};
use super::{OWN, encode_stop_reason, uncached_input_tokens};
use crate::llm::adapters::StreamEncoder;
use crate::llm::hub::{HubEvent, HubFinish, HubUsage, signature_usable, wrap_signature};
use crate::llm::sse::write_json_frame;
use crate::llm::wire::Protocol;

/// The kind of the open text-like block.
#[derive(Clone, Copy, PartialEq, Eq)]
enum BlockKind {
    Text,
    Thinking,
}

/// The one text or thinking block that is open.
#[derive(Clone, Copy)]
struct OpenBlock {
    kind: BlockKind,
    index: u32,
}

/// An open `tool_use` block and the hub tool index it carries.
struct OpenTool {
    tool: u32,
    block: u32,
}

/// Encodes hub events as an Anthropic `/v1/messages` stream.
#[derive(Default)]
pub(super) struct Encoder {
    id: Option<String>,
    model: String,
    /// The protocol that produced the events; foreign signatures get a `pdk1.` envelope.
    origin: Option<Protocol>,
    usage: HubUsage,
    /// `message_start` was written.
    started: bool,
    /// A non-error event arrived; without one, `finish` writes nothing (empty or error-only
    /// stream).
    seen: bool,
    /// An error or `finish` ended the stream; later events write nothing.
    done: bool,
    /// The current text or thinking block, closed when the kind changes.
    open: Option<OpenBlock>,
    /// `tool_use` blocks stay open until `finish`, so deltas of parallel calls can interleave.
    tools: Vec<OpenTool>,
    next_block: u32,
    finish: Option<HubFinish>,
}

impl Encoder {
    fn emit(out: &mut Vec<u8>, kind: &str, mut payload: Value) {
        if let Some(map) = payload.as_object_mut() {
            map.insert("type".into(), json!(kind));
        }
        write_json_frame(out, Some(kind), &payload);
    }

    fn emit_stop(out: &mut Vec<u8>, index: u32) {
        Self::emit(out, "content_block_stop", json!({"index": index}));
    }

    /// The stream's `usage` object. `force_input` writes `input_tokens` even before any usage
    /// arrived (`message_start` must carry it); cache fields appear only once known.
    fn stream_usage(&self, output: u64, force_input: bool) -> Map<String, Value> {
        let mut usage = Map::new();
        if force_input || self.usage.input_tokens.is_some() {
            usage.insert(
                "input_tokens".into(),
                json!(uncached_input_tokens(&self.usage)),
            );
            if let Some(cached) = self.usage.cached_input_tokens {
                usage.insert("cache_read_input_tokens".into(), json!(cached));
            }
            if let Some(written) = self.usage.cache_write_tokens {
                usage.insert("cache_creation_input_tokens".into(), json!(written));
            }
        }
        usage.insert("output_tokens".into(), json!(output));
        usage
    }

    fn ensure_started(&mut self, out: &mut Vec<u8>) {
        if self.started {
            return;
        }
        self.started = true;
        let id = self.id.get_or_insert_with(synthetic_id).clone();
        let usage = self.stream_usage(0, true);
        Self::emit(
            out,
            "message_start",
            json!({"message": {
                "id": id,
                "type": "message",
                "role": "assistant",
                "model": self.model,
                "content": [],
                "stop_reason": null,
                "stop_sequence": null,
                "usage": usage,
            }}),
        );
    }

    fn close_block(&mut self, out: &mut Vec<u8>) {
        if let Some(open) = self.open.take() {
            Self::emit_stop(out, open.index);
        }
    }

    /// Closes every open block in ascending index order. The open text block is always the last
    /// one started, so the indices are already ascending; the sort only guarantees it.
    fn close_all(&mut self, out: &mut Vec<u8>) {
        let mut indices: Vec<u32> = self.tools.drain(..).map(|tool| tool.block).collect();
        indices.extend(self.open.take().map(|open| open.index));
        indices.sort_unstable();
        for index in indices {
            Self::emit_stop(out, index);
        }
    }

    fn start_block(&mut self, start: Value, out: &mut Vec<u8>) -> u32 {
        self.ensure_started(out);
        let index = self.next_block;
        self.next_block += 1;
        Self::emit(
            out,
            "content_block_start",
            json!({"index": index, "content_block": start}),
        );
        index
    }

    /// The index of the open block of `kind`, starting one (and closing the other) when needed.
    fn open_block(&mut self, kind: BlockKind, out: &mut Vec<u8>) -> u32 {
        if let Some(open) = self.open
            && open.kind == kind
        {
            return open.index;
        }
        self.close_block(out);
        let start = match kind {
            BlockKind::Text => json!({"type": "text", "text": ""}),
            BlockKind::Thinking => json!({"type": "thinking", "thinking": ""}),
        };
        let index = self.start_block(start, out);
        self.open = Some(OpenBlock { kind, index });
        index
    }

    /// Writes a block that is complete at once: it closes the open block, starts this one and
    /// returns its index; the caller writes any delta and the stop.
    fn inline_block(&mut self, start: Value, out: &mut Vec<u8>) -> u32 {
        self.close_block(out);
        self.start_block(start, out)
    }

    fn delta(out: &mut Vec<u8>, index: u32, delta: Value) {
        Self::emit(
            out,
            "content_block_delta",
            json!({"index": index, "delta": delta}),
        );
    }

    fn on_text(&mut self, text: &str, out: &mut Vec<u8>) {
        let index = self.open_block(BlockKind::Text, out);
        Self::delta(out, index, json!({"type": "text_delta", "text": text}));
    }

    fn on_reasoning(&mut self, text: &str, out: &mut Vec<u8>) {
        let index = self.open_block(BlockKind::Thinking, out);
        Self::delta(
            out,
            index,
            json!({"type": "thinking_delta", "thinking": text}),
        );
    }

    fn on_redacted(&mut self, data: &str, out: &mut Vec<u8>) {
        let data = wrap_signature(self.origin, data, OWN);
        let index = self.inline_block(json!({"type": "redacted_thinking", "data": data}), out);
        Self::emit_stop(out, index);
    }

    /// A signature ends its thinking block.
    fn on_signature(&mut self, signature: &str, out: &mut Vec<u8>) {
        let signature = wrap_signature(self.origin, signature, OWN);
        let index = self.open_block(BlockKind::Thinking, out);
        Self::delta(
            out,
            index,
            json!({"type": "signature_delta", "signature": signature}),
        );
        self.close_block(out);
    }

    fn on_tool_start(&mut self, tool: u32, id: &str, name: &str, out: &mut Vec<u8>) {
        let block = self.inline_block(
            json!({"type": "tool_use", "id": id, "name": name, "input": {}}),
            out,
        );
        self.tools.push(OpenTool { tool, block });
    }

    fn on_tool_delta(&self, tool: u32, arguments: &str, out: &mut Vec<u8>) {
        if let Some(open) = self.tools.iter().rev().find(|open| open.tool == tool) {
            Self::delta(
                out,
                open.block,
                json!({"type": "input_json_delta", "partial_json": arguments}),
            );
        }
    }

    fn on_error(&mut self, message: &str, kind: Option<&str>, out: &mut Vec<u8>) {
        self.done = true;
        Self::emit(
            out,
            "error",
            json!({"error": {
                "type": kind.unwrap_or("api_error"),
                "message": message,
            }}),
        );
    }

    /// Replays a server tool call as one complete block; only Anthropic's own can be replayed.
    fn on_server_tool_start(&mut self, id: &str, name: &str, input: &Value, out: &mut Vec<u8>) {
        if !signature_usable(self.origin, OWN) {
            return;
        }
        let index = self.inline_block(server_tool_use_block(id, name, &json!({})), out);
        Self::delta(
            out,
            index,
            json!({"type": "input_json_delta", "partial_json": input.to_string()}),
        );
        Self::emit_stop(out, index);
    }

    fn on_server_tool_result(
        &mut self,
        call_id: &str,
        name: &str,
        payload: &Value,
        out: &mut Vec<u8>,
    ) {
        if !signature_usable(self.origin, OWN) {
            return;
        }
        let index = self.inline_block(server_tool_result_block(call_id, name, payload), out);
        Self::emit_stop(out, index);
    }
}

impl StreamEncoder for Encoder {
    fn push(&mut self, event: &HubEvent) -> Vec<u8> {
        let mut out = Vec::new();
        if self.done {
            return out;
        }
        if !matches!(event, HubEvent::Error { .. }) {
            self.seen = true;
        }
        match event {
            HubEvent::Start { id, model } => {
                self.id = Some(id.clone());
                self.model.clone_from(model);
            }
            HubEvent::Origin { protocol } => self.origin = Some(*protocol),
            HubEvent::TextDelta { text } if !text.is_empty() => self.on_text(text, &mut out),
            HubEvent::ReasoningDelta { text } if !text.is_empty() => {
                self.on_reasoning(text, &mut out);
            }
            HubEvent::RedactedReasoning { data } => self.on_redacted(data, &mut out),
            HubEvent::ReasoningSignature { signature } => self.on_signature(signature, &mut out),
            HubEvent::ToolCallStart {
                index, id, name, ..
            } => self.on_tool_start(*index, id, name, &mut out),
            HubEvent::ToolCallDelta { index, arguments } if !arguments.is_empty() => {
                self.on_tool_delta(*index, arguments, &mut out);
            }
            HubEvent::Usage { usage } => self.usage.merge(usage),
            HubEvent::Finish { reason } => self.finish = Some(reason.clone()),
            HubEvent::Error { message, kind } => self.on_error(message, kind.as_deref(), &mut out),
            HubEvent::ServerToolStart { id, name, input } => {
                self.on_server_tool_start(id, name, input, &mut out);
            }
            HubEvent::ServerToolResult {
                call_id,
                name,
                payload,
                ..
            } => self.on_server_tool_result(call_id, name, payload, &mut out),
            HubEvent::Served { .. }
            | HubEvent::ReasoningItem { .. }
            | HubEvent::TextSignature { .. }
            | HubEvent::TextDelta { .. }
            | HubEvent::ReasoningDelta { .. }
            | HubEvent::ToolCallDelta { .. }
            | HubEvent::Citation { .. } => {}
        }
        out
    }

    fn finish(&mut self) -> Vec<u8> {
        let mut out = Vec::new();
        if self.done || !self.seen {
            return out;
        }
        self.done = true;
        self.ensure_started(&mut out);
        self.close_all(&mut out);
        let reason = self.finish.take().unwrap_or(HubFinish::Stop);
        let usage = self.stream_usage(self.usage.output_tokens.unwrap_or(0), false);
        Self::emit(
            &mut out,
            "message_delta",
            json!({
                "delta": {"stop_reason": encode_stop_reason(&reason), "stop_sequence": null},
                "usage": usage,
            }),
        );
        Self::emit(&mut out, "message_stop", json!({}));
        out
    }
}

/// A message id for a stream whose events carried none.
fn synthetic_id() -> String {
    let nanos = SystemTime::now()
        .duration_since(UNIX_EPOCH)
        .map(|d| d.as_nanos())
        .unwrap_or_default();
    format!("msg_{nanos:x}")
}
