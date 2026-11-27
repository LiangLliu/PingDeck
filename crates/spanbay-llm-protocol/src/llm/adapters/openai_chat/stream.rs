//! SSE stream decoding (upstream chunks -> hub events) and encoding (hub events -> client chunks).

use serde_json::{Value, json};

use super::{
    DEFAULT_ID, REASONING_FIELDS, annotation_json, decode_finish, decode_usage, encode_finish,
    encode_usage, error_kind, error_message, non_empty_or, url_citations,
};
use crate::llm::adapters::{
    StreamDecoder, StreamEncoder, arguments_text, non_null, object, parse_sse_json, take_str,
    to_u32, unix_now, utf16_len,
};
use crate::llm::hub::{HubEvent, HubFinish, HubUsage};
use crate::llm::sse::{SseEvent, frame, frame_json};
use crate::llm::wire::Protocol;
use spanbay_base::error::AppError;

/// Decodes an upstream Chat Completions stream (its first choice).
#[derive(Default)]
pub(super) struct ChatStreamDecoder {
    started: bool,
    finished: bool,
    /// Tool call indices already started, in arrival order.
    tool_indices: Vec<u32>,
}

impl ChatStreamDecoder {
    fn tool_call_events(&mut self, calls: &mut [Value], out: &mut Vec<HubEvent>) {
        for (position, call) in calls.iter_mut().enumerate() {
            let index = call["index"]
                .as_u64()
                .map_or_else(|| u32::try_from(position).unwrap_or(u32::MAX), to_u32);
            if !self.tool_indices.contains(&index) {
                self.tool_indices.push(index);
                let id = take_str(call, "id")
                    .filter(|id| !id.is_empty())
                    .unwrap_or_else(|| format!("call_{index}"));
                let name = call
                    .get_mut("function")
                    .and_then(|function| take_str(function, "name"))
                    .unwrap_or_default();
                out.push(HubEvent::ToolCallStart {
                    index,
                    id,
                    name,
                    signature: None,
                });
            }
            let arguments = call
                .get_mut("function")
                .and_then(|function| function.get_mut("arguments"));
            let arguments = match arguments {
                Some(Value::String(text)) => std::mem::take(text),
                other => arguments_text(other.as_deref()),
            };
            if !arguments.is_empty() {
                out.push(HubEvent::ToolCallDelta { index, arguments });
            }
        }
    }

    /// The events of one choice's delta, and its finish while none was seen yet. Strings are
    /// moved out of the chunk, not copied.
    fn choice_events(&mut self, choice: &mut Value, out: &mut Vec<HubEvent>) -> Option<HubFinish> {
        if let Some(delta) = choice.get_mut("delta") {
            if let Some(text) = REASONING_FIELDS
                .iter()
                .find_map(|field| take_str(delta, field))
                .filter(|text| !text.is_empty())
            {
                out.push(HubEvent::ReasoningDelta { text });
            }
            if let Some(text) = take_str(delta, "content").filter(|text| !text.is_empty()) {
                out.push(HubEvent::TextDelta { text });
            }
            out.extend(
                url_citations(&delta["annotations"])
                    .into_iter()
                    .map(|citation| HubEvent::Citation { citation }),
            );
            if let Some(calls) = delta.get_mut("tool_calls").and_then(Value::as_array_mut) {
                self.tool_call_events(calls, out);
            }
        }
        if self.finished {
            return None;
        }
        choice["finish_reason"]
            .as_str()
            .map(|reason| decode_finish(Some(reason)))
    }
}

impl StreamDecoder for ChatStreamDecoder {
    fn push(&mut self, event: &SseEvent) -> Result<Vec<HubEvent>, AppError> {
        let Some(mut chunk) = parse_sse_json(event, "stream chunk")? else {
            return Ok(Vec::new());
        };
        if let Some(error) = non_null(&chunk, "error") {
            self.finished = true;
            return Ok(vec![HubEvent::Error {
                message: error_message(error),
                kind: error_kind(error),
            }]);
        }
        let mut out = Vec::new();
        if !self.started {
            self.started = true;
            out.push(HubEvent::Start {
                id: take_str(&mut chunk, "id").unwrap_or_default(),
                model: take_str(&mut chunk, "model").unwrap_or_default(),
            });
            out.push(HubEvent::Origin {
                protocol: Protocol::OpenaiChat,
            });
        }
        let finish = chunk
            .get_mut("choices")
            .and_then(|choices| choices.get_mut(0))
            .and_then(|choice| self.choice_events(choice, &mut out));
        if chunk["usage"].is_object() {
            let usage = decode_usage(&chunk["usage"]);
            if usage != HubUsage::default() {
                out.push(HubEvent::Usage { usage });
            }
        }
        if let Some(reason) = finish {
            self.finished = true;
            out.push(HubEvent::Finish { reason });
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<HubEvent>, AppError> {
        if !self.started || self.finished {
            return Ok(Vec::new());
        }
        self.finished = true;
        let reason = if self.tool_indices.is_empty() {
            HubFinish::Stop
        } else {
            HubFinish::ToolCalls
        };
        Ok(vec![HubEvent::Finish { reason }])
    }
}

/// Encodes a stream for a Chat Completions client.
pub(super) struct ChatStreamEncoder {
    id: String,
    model: String,
    created: u64,
    /// Whether the opening `role: assistant` chunk was sent.
    started: bool,
    /// Set by `finish` or an error; nothing is written afterwards, so `[DONE]` comes at most
    /// once and never after an error.
    done: bool,
    /// Usage merged from every `Usage` event, sent in its own chunk before `[DONE]`.
    usage: Option<HubUsage>,
    /// UTF-16 length of all text sent so far.
    text_units: u32,
    /// UTF-16 offset where the current text part began: hub citation indices are relative to
    /// their part, Chat annotation indices to the whole content. A tool call or server tool
    /// starts a new part.
    part_start: u32,
}

impl ChatStreamEncoder {
    pub(super) fn new() -> Self {
        Self {
            id: DEFAULT_ID.to_owned(),
            model: String::new(),
            created: unix_now(),
            started: false,
            done: false,
            usage: None,
            text_units: 0,
            part_start: 0,
        }
    }

    fn chunk(&self, choices: Value, usage: Option<Value>) -> Vec<u8> {
        let mut chunk = serde_json::Map::new();
        chunk.insert("id".into(), self.id.as_str().into());
        chunk.insert("object".into(), "chat.completion.chunk".into());
        chunk.insert("created".into(), self.created.into());
        chunk.insert("model".into(), self.model.as_str().into());
        chunk.insert("choices".into(), choices);
        if let Some(usage) = usage {
            chunk.insert("usage".into(), usage);
        }
        frame_json(None, &Value::Object(chunk))
    }

    fn delta(&self, delta: Value, finish: Option<&str>) -> Vec<u8> {
        let choice = object([
            ("index", 0.into()),
            ("delta", delta),
            ("finish_reason", finish.into()),
        ]);
        self.chunk(Value::Array(vec![choice]), None)
    }

    fn ensure_started(&mut self) -> Vec<u8> {
        if self.started {
            return Vec::new();
        }
        self.started = true;
        self.delta(json!({"role": "assistant", "content": ""}), None)
    }

    /// The opening chunk when not yet sent, then one chunk of `delta`.
    fn started_delta(&mut self, delta: Value, finish: Option<&str>) -> Vec<u8> {
        let mut out = self.ensure_started();
        out.extend(self.delta(delta, finish));
        out
    }
}

/// A delta of one `tool_calls` entry.
fn tool_call_delta(entry: Value) -> Value {
    object([("tool_calls", Value::Array(vec![entry]))])
}

impl StreamEncoder for ChatStreamEncoder {
    fn push(&mut self, event: &HubEvent) -> Vec<u8> {
        if self.done {
            return Vec::new();
        }
        match event {
            HubEvent::Start { id, model } => {
                if !self.started {
                    self.id = non_empty_or(id, DEFAULT_ID).to_owned();
                    self.model.clone_from(model);
                }
                self.ensure_started()
            }
            HubEvent::Usage { usage } => {
                self.usage
                    .get_or_insert_with(HubUsage::default)
                    .merge(usage);
                Vec::new()
            }
            HubEvent::Citation { citation } => {
                let annotation = annotation_json(citation, self.part_start);
                self.started_delta(
                    object([("annotations", Value::Array(vec![annotation]))]),
                    None,
                )
            }
            HubEvent::ServerToolStart { .. } | HubEvent::ServerToolResult { .. } => {
                self.part_start = self.text_units;
                Vec::new()
            }
            HubEvent::Error { message, kind } => {
                self.done = true;
                let kind = kind.as_deref().unwrap_or("upstream_error");
                frame_json(None, &json!({"error": {"message": message, "type": kind}}))
            }
            HubEvent::ReasoningSignature { .. }
            | HubEvent::TextSignature { .. }
            | HubEvent::Origin { .. }
            | HubEvent::Served { .. }
            | HubEvent::ReasoningItem { .. }
            | HubEvent::RedactedReasoning { .. } => Vec::new(),
            HubEvent::TextDelta { text } => {
                self.text_units = self.text_units.saturating_add(utf16_len(text));
                self.started_delta(json!({"content": text}), None)
            }
            HubEvent::ReasoningDelta { text } => {
                self.started_delta(json!({"reasoning_content": text}), None)
            }
            HubEvent::ToolCallStart {
                index, id, name, ..
            } => {
                self.part_start = self.text_units;
                let call = json!({
                    "index": index,
                    "id": id,
                    "type": "function",
                    "function": {"name": name, "arguments": ""},
                });
                self.started_delta(tool_call_delta(call), None)
            }
            HubEvent::ToolCallDelta { index, arguments } => {
                let call = json!({"index": index, "function": {"arguments": arguments}});
                self.started_delta(tool_call_delta(call), None)
            }
            HubEvent::Finish { reason } => {
                self.started_delta(json!({}), Some(encode_finish(reason)))
            }
        }
    }

    fn finish(&mut self) -> Vec<u8> {
        if self.done {
            return Vec::new();
        }
        self.done = true;
        let mut out = Vec::new();
        if let Some(usage) = self.usage.take() {
            out.extend(self.chunk(Value::Array(Vec::new()), Some(encode_usage(&usage))));
        }
        out.extend(frame(None, "[DONE]"));
        out
    }
}
