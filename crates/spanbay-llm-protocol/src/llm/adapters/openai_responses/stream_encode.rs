//! Client stream encoding: hub events to a Responses SSE stream. This module keeps the stream's
//! state; `stream_emit` writes the frames.

use serde_json::Value;

use super::DEFAULT_RESPONSE_ID;
use super::encode_items::{
    ResponseHead, annotation_json, client_reasoning_item, function_call_item, is_own_search,
    message_item, reasoning_item, response_object, status_of, web_search_call_item,
};
use super::stream_emit::Emitter;
use crate::llm::adapters::{StreamEncoder, object_map, unix_now, utf16_len};
use crate::llm::hub::{
    HubCitation, HubEvent, HubFinish, HubUsage, MAX_REASONING_BYTES, MAX_TEXT_BYTES,
    MAX_TOOL_ARGUMENT_BYTES, too_large,
};
use crate::llm::wire::Protocol;

/// Appends `delta` to `acc` unless that would take it past `cap` bytes; `false` when refused.
fn append_capped(acc: &mut String, delta: &str, cap: usize) -> bool {
    if acc.len() + delta.len() > cap {
        return false;
    }
    acc.push_str(delta);
    true
}

/// The message or reasoning item being streamed; at most one is open at a time.
enum OpenBlock {
    Text {
        output_index: usize,
        item_id: String,
        text: String,
        annotations: Vec<Value>,
        /// The prefix of `text`, in bytes, that `cited_len` already counts: citations count
        /// the text lazily, so a stream without citations never counts it.
        counted_bytes: usize,
        /// The length of `text[..counted_bytes]` in UTF-16 units, the unit of citation indices.
        cited_len: u32,
    },
    Reasoning {
        output_index: usize,
        item_id: String,
        text: String,
    },
}

/// A function call item; calls stay open until the stream finishes.
struct ToolItem {
    index: u32,
    output_index: usize,
    item_id: String,
    call_id: String,
    name: String,
    arguments: String,
}

/// An item being closed.
enum Closing {
    Block(OpenBlock),
    Tool(ToolItem),
}

impl Closing {
    fn output_index(&self) -> usize {
        match self {
            Self::Block(
                OpenBlock::Text { output_index, .. } | OpenBlock::Reasoning { output_index, .. },
            ) => *output_index,
            Self::Tool(tool) => tool.output_index,
        }
    }
}

pub(super) struct ResponsesEncoder {
    emitter: Emitter,
    created_at: u64,
    started: bool,
    /// An error went out or the stream finished; nothing more is written.
    finished: bool,
    /// The reason of the `Finish` event; `finish` writes the terminal frame.
    finish_reason: Option<HubFinish>,
    /// The upstream id and encrypted payload of the last reasoning item. They are used only when
    /// an open reasoning block closes, so an item with no summary text is lost and the added and
    /// done events of one item can carry different ids (a known defect, kept as it is).
    reasoning_meta: Option<(Option<String>, Option<String>)>,
    origin: Option<Protocol>,
    id: String,
    model: String,
    usage: HubUsage,
    next_output: usize,
    /// Closed items with their output index. Tool items close last, so this order differs from
    /// the output index and the final response sorts it.
    output: Vec<(usize, Value)>,
    open: Option<OpenBlock>,
    tools: Vec<ToolItem>,
}

impl ResponsesEncoder {
    pub(super) fn new() -> Self {
        Self {
            emitter: Emitter::new(),
            created_at: unix_now(),
            started: false,
            finished: false,
            finish_reason: None,
            reasoning_meta: None,
            origin: None,
            id: DEFAULT_RESPONSE_ID.into(),
            model: String::new(),
            usage: HubUsage::default(),
            next_output: 0,
            output: Vec::new(),
            open: None,
            tools: Vec::new(),
        }
    }

    fn head(&self) -> ResponseHead<'_> {
        ResponseHead {
            id: &self.id,
            model: &self.model,
            created_at: self.created_at,
        }
    }

    /// The final response: every closed item in output order, with usage. Only `finish` calls
    /// it, so the items move out.
    fn final_response(&mut self, status: &str, reason: Option<&str>) -> Value {
        let mut items = std::mem::take(&mut self.output);
        items.sort_by_key(|(output_index, _)| *output_index);
        let output = items.into_iter().map(|(_, item)| item).collect();
        response_object(&self.head(), status, output, Some(&self.usage), reason)
    }

    fn start(&mut self, out: &mut Vec<u8>) {
        if self.started {
            return;
        }
        self.started = true;
        let response = response_object(&self.head(), "in_progress", Vec::new(), None, None);
        self.emitter.started(out, response);
    }

    /// `Start`: the first one names the response; every one opens the stream if it is not open.
    fn on_start(&mut self, out: &mut Vec<u8>, id: &str, model: &str) {
        if !self.started {
            id.clone_into(&mut self.id);
            model.clone_into(&mut self.model);
        }
        self.start(out);
    }

    /// `Finish`: closes every open item and keeps the reason for the terminal frame.
    fn on_finish(&mut self, out: &mut Vec<u8>, reason: &HubFinish) {
        self.start(out);
        self.close_all(out);
        self.finish_reason = Some(reason.clone());
    }

    fn alloc(&mut self) -> usize {
        let output_index = self.next_output;
        self.next_output += 1;
        output_index
    }

    /// Writes the closing events of an item and records it for the final response.
    fn finish_item(&mut self, out: &mut Vec<u8>, closing: Closing) {
        let output_index = closing.output_index();
        let item = match closing {
            Closing::Block(OpenBlock::Text {
                item_id,
                text,
                annotations,
                ..
            }) => {
                self.emitter
                    .text_closed(out, output_index, &item_id, &text, &annotations);
                message_item(&item_id, &text, annotations)
            }
            Closing::Block(OpenBlock::Reasoning { item_id, text, .. }) => {
                self.emitter
                    .reasoning_closed(out, output_index, &item_id, &text);
                let (id, encrypted) = self.reasoning_meta.take().unwrap_or_default();
                // An open reasoning block always has text, so `client_reasoning_item` never
                // drops it; the fallback only avoids an unwrap.
                client_reasoning_item(
                    &text,
                    encrypted.as_deref(),
                    id.as_deref(),
                    self.origin,
                    &item_id,
                )
                .unwrap_or_else(|| reasoning_item(&item_id, &text, None))
            }
            Closing::Tool(tool) => {
                self.emitter.arguments_done(
                    out,
                    output_index,
                    &tool.item_id,
                    &tool.name,
                    &tool.arguments,
                );
                function_call_item(&tool.item_id, &tool.call_id, &tool.name, &tool.arguments)
            }
        };
        self.emitter.item_done(out, output_index, item.clone());
        self.output.push((output_index, item));
    }

    fn close_block(&mut self, out: &mut Vec<u8>) {
        if let Some(open) = self.open.take() {
            self.finish_item(out, Closing::Block(open));
        }
    }

    /// Closes the open block and every tool call, in output order.
    fn close_all(&mut self, out: &mut Vec<u8>) {
        let mut closing: Vec<Closing> = self.open.take().map(Closing::Block).into_iter().collect();
        closing.extend(
            std::mem::take(&mut self.tools)
                .into_iter()
                .map(Closing::Tool),
        );
        closing.sort_by_key(Closing::output_index);
        for item in closing {
            self.finish_item(out, item);
        }
    }

    fn ensure_text(&mut self, out: &mut Vec<u8>) {
        if matches!(self.open, Some(OpenBlock::Text { .. })) {
            return;
        }
        self.close_block(out);
        let output_index = self.alloc();
        let item_id = format!("msg_{output_index}");
        self.emitter.text_opened(out, output_index, &item_id);
        self.open = Some(OpenBlock::Text {
            output_index,
            item_id,
            text: String::new(),
            annotations: Vec::new(),
            counted_bytes: 0,
            cited_len: 0,
        });
    }

    fn ensure_reasoning(&mut self, out: &mut Vec<u8>) {
        if matches!(self.open, Some(OpenBlock::Reasoning { .. })) {
            return;
        }
        self.close_block(out);
        let output_index = self.alloc();
        let item_id = format!("rs_{output_index}");
        self.emitter.reasoning_opened(out, output_index, &item_id);
        self.open = Some(OpenBlock::Reasoning {
            output_index,
            item_id,
            text: String::new(),
        });
    }

    fn open_tool(&mut self, out: &mut Vec<u8>, index: u32, call_id: &str, name: &str) {
        self.close_block(out);
        let output_index = self.alloc();
        let item_id = format!("fc_{output_index}");
        self.emitter
            .tool_opened(out, output_index, &item_id, call_id, name);
        self.tools.push(ToolItem {
            index,
            output_index,
            item_id,
            call_id: call_id.to_owned(),
            name: name.to_owned(),
            arguments: String::new(),
        });
    }

    fn on_text_delta(&mut self, out: &mut Vec<u8>, delta: &str) {
        self.start(out);
        self.ensure_text(out);
        let Some(OpenBlock::Text {
            output_index,
            item_id,
            text,
            ..
        }) = &mut self.open
        else {
            return;
        };
        if !append_capped(text, delta, MAX_TEXT_BYTES) {
            return self.fail(out, &too_large("answer text", MAX_TEXT_BYTES));
        }
        self.emitter.text_delta(out, *output_index, item_id, delta);
    }

    fn on_reasoning_delta(&mut self, out: &mut Vec<u8>, delta: &str) {
        if delta.is_empty() {
            return;
        }
        self.start(out);
        self.ensure_reasoning(out);
        let Some(OpenBlock::Reasoning {
            output_index,
            item_id,
            text,
        }) = &mut self.open
        else {
            return;
        };
        if !append_capped(text, delta, MAX_REASONING_BYTES) {
            return self.fail(out, &too_large("reasoning text", MAX_REASONING_BYTES));
        }
        self.emitter
            .reasoning_delta(out, *output_index, item_id, delta);
    }

    fn on_tool_delta(&mut self, out: &mut Vec<u8>, index: u32, delta: &str) {
        let Some(tool) = self.tools.iter_mut().rev().find(|tool| tool.index == index) else {
            return;
        };
        if !append_capped(&mut tool.arguments, delta, MAX_TOOL_ARGUMENT_BYTES) {
            return self.fail(
                out,
                &too_large("tool-call arguments", MAX_TOOL_ARGUMENT_BYTES),
            );
        }
        self.emitter
            .arguments_delta(out, tool.output_index, &tool.item_id, delta);
    }

    /// Adds a citation to the open message (opening one when needed); without a range it covers
    /// the message text so far.
    fn cite(&mut self, out: &mut Vec<u8>, citation: &HubCitation) {
        self.ensure_text(out);
        let Some(OpenBlock::Text {
            output_index,
            item_id,
            text,
            annotations,
            counted_bytes,
            cited_len,
        }) = &mut self.open
        else {
            return;
        };
        // `text` only grows by whole strings, so `counted_bytes` is always a char boundary.
        let uncounted = text.get(*counted_bytes..).unwrap_or_default();
        *cited_len = cited_len.saturating_add(utf16_len(uncounted));
        *counted_bytes = text.len();
        let annotation = annotation_json(citation, 0, *cited_len);
        let annotation_index = annotations.len();
        annotations.push(annotation.clone());
        self.emitter
            .annotation_added(out, *output_index, item_id, annotation_index, annotation);
    }

    /// An own web search: added and done at once, since the search already ran upstream.
    fn search_call(&mut self, out: &mut Vec<u8>, id: &str, input: &Value) {
        self.close_block(out);
        let output_index = self.alloc();
        let added = web_search_call_item(id, "in_progress", input);
        self.emitter.item_added(out, output_index, added);
        let item = web_search_call_item(id, "completed", input);
        self.emitter.item_done(out, output_index, item.clone());
        self.output.push((output_index, item));
    }

    /// Writes an `error` event and ends the stream.
    fn emit_error(&mut self, out: &mut Vec<u8>, message: &str, code: Option<&str>) {
        self.emitter.error(out, message, code);
        self.finished = true;
    }

    fn fail(&mut self, out: &mut Vec<u8>, message: &str) {
        self.emit_error(out, message, None);
    }
}

impl StreamEncoder for ResponsesEncoder {
    fn push(&mut self, event: &HubEvent) -> Vec<u8> {
        let mut out = Vec::new();
        if self.finished {
            return out;
        }
        match event {
            HubEvent::Start { id, model } => self.on_start(&mut out, id, model),
            HubEvent::TextDelta { text } => self.on_text_delta(&mut out, text),
            HubEvent::ReasoningDelta { text } => self.on_reasoning_delta(&mut out, text),
            HubEvent::ReasoningItem { id, encrypted } => {
                self.reasoning_meta = Some((id.clone(), encrypted.clone()));
            }
            HubEvent::ReasoningSignature { .. }
            | HubEvent::RedactedReasoning { .. }
            | HubEvent::TextSignature { .. }
            | HubEvent::Served { .. }
            | HubEvent::ServerToolResult { .. } => {}
            HubEvent::Origin { protocol } => self.origin = Some(*protocol),
            HubEvent::ToolCallStart {
                index, id, name, ..
            } => {
                self.start(&mut out);
                self.open_tool(&mut out, *index, id, name);
            }
            HubEvent::ToolCallDelta { index, arguments } => {
                self.on_tool_delta(&mut out, *index, arguments);
            }
            HubEvent::Usage { usage } => self.usage.merge(usage),
            HubEvent::Citation { citation } => {
                self.start(&mut out);
                self.cite(&mut out, citation);
            }
            HubEvent::ServerToolStart { id, name, input } => {
                if is_own_search(self.origin, name) {
                    self.start(&mut out);
                    self.search_call(&mut out, id, input);
                }
            }
            HubEvent::Finish { reason } => self.on_finish(&mut out, reason),
            HubEvent::Error { message, kind } => {
                self.emit_error(&mut out, message, kind.as_deref());
            }
        }
        out
    }

    fn finish(&mut self) -> Vec<u8> {
        if self.finished || !self.started {
            return Vec::new();
        }
        let reason = self.finish_reason.take().unwrap_or(HubFinish::Stop);
        let mut out = Vec::new();
        self.close_all(&mut out);
        let (status, incomplete) = status_of(&reason);
        let kind = if status == "completed" {
            "response.completed"
        } else {
            "response.incomplete"
        };
        let response = self.final_response(status, incomplete);
        self.emitter
            .emit(&mut out, kind, object_map([("response", response)]));
        self.finished = true;
        out
    }
}
