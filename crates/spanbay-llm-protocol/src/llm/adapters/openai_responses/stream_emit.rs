//! The frames of a Responses stream toward a client: event numbering and the events of each
//! step of an item's life. `stream_encode` decides when a step happens; this module decides what
//! it writes.

use serde_json::{Map, Value, json};

use crate::llm::adapters::object_map;
use crate::llm::sse::write_json_frame;

/// Numbers the events with `sequence_number` and writes their frames.
pub(super) struct Emitter {
    seq: u64,
}

impl Emitter {
    pub(super) fn new() -> Self {
        Self { seq: 0 }
    }

    /// Writes one event of type `kind`. The keys of `fields` serialize sorted, so the order they
    /// were inserted in does not matter.
    pub(super) fn emit(&mut self, out: &mut Vec<u8>, kind: &str, mut fields: Map<String, Value>) {
        fields.insert("type".into(), kind.into());
        fields.insert("sequence_number".into(), self.seq.into());
        self.seq += 1;
        write_json_frame(out, Some(kind), &Value::Object(fields));
    }

    /// An event about the content of item `item_id`, with `extra` fields.
    fn item_event<const N: usize>(
        &mut self,
        out: &mut Vec<u8>,
        kind: &str,
        item_id: &str,
        output_index: usize,
        extra: [(&str, Value); N],
    ) {
        let mut event = object_map(extra);
        event.insert("item_id".into(), item_id.into());
        event.insert("output_index".into(), output_index.into());
        self.emit(out, kind, event);
    }

    /// `response.created` and `response.in_progress`, both carrying `response`.
    pub(super) fn started(&mut self, out: &mut Vec<u8>, response: Value) {
        let created = object_map([("response", response.clone())]);
        self.emit(out, "response.created", created);
        let in_progress = object_map([("response", response)]);
        self.emit(out, "response.in_progress", in_progress);
    }

    pub(super) fn item_added(&mut self, out: &mut Vec<u8>, output_index: usize, item: Value) {
        let added = object_map([("output_index", output_index.into()), ("item", item)]);
        self.emit(out, "response.output_item.added", added);
    }

    pub(super) fn item_done(&mut self, out: &mut Vec<u8>, output_index: usize, item: Value) {
        let done = object_map([("output_index", output_index.into()), ("item", item)]);
        self.emit(out, "response.output_item.done", done);
    }

    /// A new message item with its empty text part.
    pub(super) fn text_opened(&mut self, out: &mut Vec<u8>, output_index: usize, item_id: &str) {
        let item = json!({
            "id": item_id,
            "type": "message",
            "role": "assistant",
            "status": "in_progress",
            "content": [],
        });
        self.item_added(out, output_index, item);
        let part = json!({"type": "output_text", "text": "", "annotations": []});
        self.item_event(
            out,
            "response.content_part.added",
            item_id,
            output_index,
            [("content_index", 0.into()), ("part", part)],
        );
    }

    /// A new reasoning item with its empty summary part.
    pub(super) fn reasoning_opened(
        &mut self,
        out: &mut Vec<u8>,
        output_index: usize,
        item_id: &str,
    ) {
        let item = json!({"id": item_id, "type": "reasoning", "summary": []});
        self.item_added(out, output_index, item);
        self.item_event(
            out,
            "response.reasoning_summary_part.added",
            item_id,
            output_index,
            [
                ("summary_index", 0.into()),
                ("part", json!({"type": "summary_text", "text": ""})),
            ],
        );
    }

    /// A new function call item with empty arguments.
    pub(super) fn tool_opened(
        &mut self,
        out: &mut Vec<u8>,
        output_index: usize,
        item_id: &str,
        call_id: &str,
        name: &str,
    ) {
        let item = json!({
            "id": item_id,
            "type": "function_call",
            "status": "in_progress",
            "call_id": call_id,
            "name": name,
            "arguments": "",
        });
        self.item_added(out, output_index, item);
    }

    pub(super) fn text_delta(
        &mut self,
        out: &mut Vec<u8>,
        output_index: usize,
        item_id: &str,
        delta: &str,
    ) {
        self.item_event(
            out,
            "response.output_text.delta",
            item_id,
            output_index,
            [
                ("content_index", 0.into()),
                ("delta", delta.into()),
                ("logprobs", json!([])),
            ],
        );
    }

    pub(super) fn reasoning_delta(
        &mut self,
        out: &mut Vec<u8>,
        output_index: usize,
        item_id: &str,
        delta: &str,
    ) {
        self.item_event(
            out,
            "response.reasoning_summary_text.delta",
            item_id,
            output_index,
            [("summary_index", 0.into()), ("delta", delta.into())],
        );
    }

    pub(super) fn arguments_delta(
        &mut self,
        out: &mut Vec<u8>,
        output_index: usize,
        item_id: &str,
        delta: &str,
    ) {
        self.item_event(
            out,
            "response.function_call_arguments.delta",
            item_id,
            output_index,
            [("delta", delta.into())],
        );
    }

    pub(super) fn annotation_added(
        &mut self,
        out: &mut Vec<u8>,
        output_index: usize,
        item_id: &str,
        annotation_index: usize,
        annotation: Value,
    ) {
        self.item_event(
            out,
            "response.output_text.annotation.added",
            item_id,
            output_index,
            [
                ("content_index", 0.into()),
                ("annotation_index", annotation_index.into()),
                ("annotation", annotation),
            ],
        );
    }

    /// The done events of a message's text and text part (the item's own done event follows).
    pub(super) fn text_closed(
        &mut self,
        out: &mut Vec<u8>,
        output_index: usize,
        item_id: &str,
        text: &str,
        annotations: &[Value],
    ) {
        let part = json!({"type": "output_text", "text": text, "annotations": annotations});
        self.item_event(
            out,
            "response.output_text.done",
            item_id,
            output_index,
            [
                ("content_index", 0.into()),
                ("text", text.into()),
                ("logprobs", json!([])),
            ],
        );
        self.item_event(
            out,
            "response.content_part.done",
            item_id,
            output_index,
            [("content_index", 0.into()), ("part", part)],
        );
    }

    /// The done events of a reasoning summary and summary part (the item's own done event
    /// follows).
    pub(super) fn reasoning_closed(
        &mut self,
        out: &mut Vec<u8>,
        output_index: usize,
        item_id: &str,
        text: &str,
    ) {
        let part = json!({"type": "summary_text", "text": text});
        self.item_event(
            out,
            "response.reasoning_summary_text.done",
            item_id,
            output_index,
            [("summary_index", 0.into()), ("text", text.into())],
        );
        self.item_event(
            out,
            "response.reasoning_summary_part.done",
            item_id,
            output_index,
            [("summary_index", 0.into()), ("part", part)],
        );
    }

    /// The done event of a call's arguments (the item's own done event follows).
    pub(super) fn arguments_done(
        &mut self,
        out: &mut Vec<u8>,
        output_index: usize,
        item_id: &str,
        name: &str,
        arguments: &str,
    ) {
        self.item_event(
            out,
            "response.function_call_arguments.done",
            item_id,
            output_index,
            [("name", name.into()), ("arguments", arguments.into())],
        );
    }

    /// An `error` event; `code` is the error kind, when there is one.
    pub(super) fn error(&mut self, out: &mut Vec<u8>, message: &str, code: Option<&str>) {
        let event = object_map([
            ("message", message.into()),
            ("code", code.into()),
            ("param", Value::Null),
        ]);
        self.emit(out, "error", event);
    }
}
