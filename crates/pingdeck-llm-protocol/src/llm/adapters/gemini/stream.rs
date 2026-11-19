//! Gemini SSE streams: decoding an upstream stream and encoding one for a Gemini client.
//!
//! Every Gemini chunk is a full `GenerateContentResponse` holding the next parts of the first
//! candidate; the last chunk carries `finishReason`, grounding and usage.

use serde_json::Value;

use super::decode::model_part;
use super::grounding::{self, GROUNDING_CALL_ID};
use super::{
    GeminiGenerate, blocked, candidate, candidate_parts, candidates_body, decode_finish,
    decode_usage, encode_finish, encode_usage, error_message, field, first_candidate,
    function_call_part, str_field, text_part, thought_part,
};
use crate::llm::adapters::{
    ChatCodec, StreamDecoder, StreamEncoder, WEB_SEARCH_TOOL_NAME, parse_sse_json,
};
use crate::llm::hub::{
    HubCitation, HubEvent, HubFinish, HubPart, HubUsage, MAX_TOOL_ARGUMENT_BYTES, too_large,
    wrap_signature,
};
use crate::llm::sse::{SseEvent, frame_json};
use crate::llm::wire::Protocol;
use pingdeck_base::error::AppError;

/// Decodes an upstream Gemini stream.
#[derive(Default)]
pub(super) struct GeminiStreamDecoder {
    started: bool,
    finished: bool,
    /// Function calls seen so far; also the index of the next call and the source of the
    /// synthetic `call_<n>` ids.
    calls: u32,
    /// Citations already sent: Gemini repeats grounding in later chunks.
    cited: Vec<HubCitation>,
    /// Whether the web search call was sent.
    searched: bool,
}

impl GeminiStreamDecoder {
    fn emit_part(&mut self, part: &Value, out: &mut Vec<HubEvent>) -> Result<(), AppError> {
        let part = model_part(part, &mut self.calls, false)
            .map_err(|message| AppError::network(format!("Gemini stream: {message}")))?;
        match part {
            Some(HubPart::Text {
                text, signature, ..
            }) => {
                if !text.is_empty() {
                    out.push(HubEvent::TextDelta { text });
                }
                if let Some(signature) = signature {
                    out.push(HubEvent::TextSignature { signature });
                }
            }
            Some(HubPart::Reasoning {
                text, signature, ..
            }) => {
                if !text.is_empty() {
                    out.push(HubEvent::ReasoningDelta { text });
                }
                if let Some(signature) = signature {
                    out.push(HubEvent::ReasoningSignature { signature });
                }
            }
            Some(HubPart::ToolCall {
                id,
                name,
                arguments,
                signature,
                ..
            }) => {
                let index = self.calls.saturating_sub(1);
                out.push(HubEvent::ToolCallStart {
                    index,
                    id,
                    name,
                    signature,
                });
                out.push(HubEvent::ToolCallDelta { index, arguments });
            }
            Some(HubPart::Image { .. } | HubPart::Audio { .. } | HubPart::Document { .. }) => {
                return Err(AppError::network(
                    "Gemini stream: an image, audio or document part cannot be streamed through \
                     the hub",
                ));
            }
            Some(_) | None => {}
        }
        Ok(())
    }

    fn emit_grounding(&mut self, candidate: &Value, out: &mut Vec<HubEvent>) {
        let found = grounding::decode(candidate);
        let input = found.search_input();
        for citation in found.citations {
            if !self.cited.contains(&citation) {
                self.cited.push(citation.clone());
                out.push(HubEvent::Citation { citation });
            }
        }
        if let (false, Some(input)) = (self.searched, input) {
            self.searched = true;
            out.push(HubEvent::ServerToolStart {
                id: GROUNDING_CALL_ID.to_owned(),
                name: WEB_SEARCH_TOOL_NAME.to_owned(),
                input,
            });
        }
    }

    /// `Start` and `Origin` before the first chunk's content.
    fn start_events(&mut self, chunk: &Value, out: &mut Vec<HubEvent>) {
        if self.started {
            return;
        }
        self.started = true;
        out.push(HubEvent::Start {
            id: str_field(chunk, "responseId"),
            model: str_field(chunk, "modelVersion"),
        });
        out.push(HubEvent::Origin {
            protocol: Protocol::Gemini,
        });
    }

    /// The finish a chunk reports: its candidate's `finishReason`, or a blocked prompt.
    fn chunk_finish(&self, candidate: Option<&Value>, chunk: &Value) -> Option<HubFinish> {
        match candidate {
            Some(candidate) => field(candidate, "finishReason")
                .and_then(Value::as_str)
                .map(|reason| decode_finish(Some(reason), self.calls > 0)),
            None => blocked(chunk).then_some(HubFinish::ContentFilter),
        }
    }
}

/// The hub error of an in-stream `error` object.
fn error_event(error: &Value) -> HubEvent {
    HubEvent::Error {
        message: error_message(error),
        kind: error
            .get("status")
            .and_then(Value::as_str)
            .map(str::to_owned),
    }
}

impl StreamDecoder for GeminiStreamDecoder {
    fn push(&mut self, event: &SseEvent) -> Result<Vec<HubEvent>, AppError> {
        let Some(chunk) = parse_sse_json(event, "Gemini stream chunk")? else {
            return Ok(Vec::new());
        };
        if let Some(error) = chunk.get("error").filter(|e| e.is_object()) {
            self.finished = true;
            return Ok(vec![error_event(error)]);
        }
        let mut out = Vec::new();
        self.start_events(&chunk, &mut out);
        let candidate = first_candidate(&chunk);
        for part in candidate_parts(candidate) {
            self.emit_part(part, &mut out)?;
        }
        if let Some(candidate) = candidate {
            self.emit_grounding(candidate, &mut out);
        }
        if let Some(meta) = field(&chunk, "usageMetadata").filter(|m| m.is_object()) {
            out.push(HubEvent::Usage {
                usage: decode_usage(meta),
            });
        }
        if let Some(reason) = self
            .chunk_finish(candidate, &chunk)
            .filter(|_| !self.finished)
        {
            self.finished = true;
            out.push(HubEvent::Finish { reason });
        }
        Ok(out)
    }

    fn finish(&mut self) -> Result<Vec<HubEvent>, AppError> {
        if self.finished || !self.started {
            return Ok(Vec::new());
        }
        self.finished = true;
        Ok(vec![HubEvent::Finish {
            reason: decode_finish(None, self.calls > 0),
        }])
    }
}

/// A function call whose arguments are still arriving.
struct PendingCall {
    index: u32,
    id: String,
    name: String,
    signature: Option<String>,
    arguments: String,
}

/// Encodes a stream for a Gemini client.
///
/// Gemini sends each function call as one complete part, so calls are buffered in `pending`
/// until the next event that writes a part (or the finish) and then sent together, sorted by
/// index. Citations, queries, usage and the finish reason are held for the closing chunk.
#[derive(Default)]
pub(super) struct GeminiStreamEncoder {
    id: String,
    model: String,
    pending: Vec<PendingCall>,
    origin: Option<Protocol>,
    usage: HubUsage,
    finish: Option<HubFinish>,
    /// Whether any event arrived; an untouched stream writes no closing chunk.
    touched: bool,
    /// Set by the closing chunk or an error; nothing is written afterwards.
    done: bool,
    citations: Vec<HubCitation>,
    queries: Vec<String>,
}

impl GeminiStreamEncoder {
    fn chunk(&self, candidate: Value, usage: Option<Value>) -> Vec<u8> {
        frame_json(
            None,
            &candidates_body(candidate, usage, &self.model, &self.id),
        )
    }

    fn parts_chunk(&self, parts: Vec<Value>) -> Vec<u8> {
        self.chunk(candidate(parts, None, None), None)
    }

    fn client_signature(&self, signature: &str) -> String {
        wrap_signature(self.origin, signature, Protocol::Gemini)
    }

    fn cite(&mut self, citation: &HubCitation) {
        if !self.citations.contains(citation) {
            self.citations.push(citation.clone());
        }
    }

    /// The buffered calls as one chunk, sorted by index; empty when none are buffered.
    fn flush(&mut self) -> Vec<u8> {
        if self.pending.is_empty() {
            return Vec::new();
        }
        let mut calls = std::mem::take(&mut self.pending);
        calls.sort_by_key(|call| call.index);
        let parts: Vec<Value> = calls
            .iter()
            .map(|call| {
                let signature = call.signature.as_deref().map(|s| self.client_signature(s));
                function_call_part(&call.id, &call.name, &call.arguments, signature.as_deref())
            })
            .collect();
        self.parts_chunk(parts)
    }

    /// Flushes the buffered calls, then sends `part` in its own chunk.
    fn flush_then(&mut self, part: Value) -> Vec<u8> {
        let mut out = self.flush();
        out.extend(self.parts_chunk(vec![part]));
        out
    }

    /// Starts buffering a call; a repeated index replaces the earlier call.
    fn open_call(&mut self, index: u32, id: &str, name: &str, signature: Option<&String>) {
        let call = PendingCall {
            index,
            id: id.to_owned(),
            name: name.to_owned(),
            signature: signature.cloned(),
            arguments: String::new(),
        };
        match self.pending.iter_mut().find(|open| open.index == index) {
            Some(open) => *open = call,
            None => self.pending.push(call),
        }
    }

    /// Adds argument text to a buffered call; past [`MAX_TOOL_ARGUMENT_BYTES`] the stream fails.
    fn append_arguments(&mut self, index: u32, arguments: &str) -> Vec<u8> {
        let Some(call) = self.pending.iter_mut().find(|open| open.index == index) else {
            return Vec::new();
        };
        if call.arguments.len() + arguments.len() > MAX_TOOL_ARGUMENT_BYTES {
            return self.fail(&too_large("tool-call arguments", MAX_TOOL_ARGUMENT_BYTES));
        }
        call.arguments.push_str(arguments);
        Vec::new()
    }

    /// Keeps a web search call's queries for the closing chunk's grounding.
    fn record_queries(&mut self, name: &str, input: &Value) {
        if name != WEB_SEARCH_TOOL_NAME {
            return;
        }
        for query in grounding::search_queries(input) {
            if !self.queries.contains(&query) {
                self.queries.push(query);
            }
        }
    }

    /// Ends the stream with a Gemini error chunk, dropping the buffered calls.
    fn fail(&mut self, message: &str) -> Vec<u8> {
        self.pending.clear();
        self.done = true;
        frame_json(None, &GeminiGenerate.encode_error(500, message, ""))
    }
}

impl StreamEncoder for GeminiStreamEncoder {
    fn push(&mut self, event: &HubEvent) -> Vec<u8> {
        if self.done {
            return Vec::new();
        }
        self.touched = true;
        match event {
            HubEvent::Start { id, model } => {
                self.id.clone_from(id);
                self.model.clone_from(model);
                Vec::new()
            }
            HubEvent::Origin { protocol } => {
                self.origin = Some(*protocol);
                Vec::new()
            }
            HubEvent::ToolCallStart {
                index,
                id,
                name,
                signature,
            } => {
                self.open_call(*index, id, name, signature.as_ref());
                Vec::new()
            }
            HubEvent::ToolCallDelta { index, arguments } => {
                self.append_arguments(*index, arguments)
            }
            HubEvent::Usage { usage } => {
                self.usage.merge(usage);
                Vec::new()
            }
            HubEvent::TextDelta { text } => self.flush_then(text_part(text, None)),
            HubEvent::ReasoningDelta { text } => self.flush_then(thought_part(text, None)),
            HubEvent::ReasoningSignature { signature } => {
                let signature = self.client_signature(signature);
                self.flush_then(thought_part("", Some(&signature)))
            }
            HubEvent::TextSignature { signature } => {
                let signature = self.client_signature(signature);
                self.flush_then(text_part("", Some(&signature)))
            }
            HubEvent::Citation { citation } => {
                self.cite(citation);
                Vec::new()
            }
            HubEvent::ServerToolStart { name, input, .. } => {
                self.record_queries(name, input);
                Vec::new()
            }
            HubEvent::ServerToolResult { citations, .. } => {
                for citation in citations {
                    self.cite(citation);
                }
                Vec::new()
            }
            HubEvent::Finish { reason } => {
                self.finish = Some(reason.clone());
                self.flush()
            }
            HubEvent::Error { message, .. } => self.fail(message),
            HubEvent::Served { .. }
            | HubEvent::RedactedReasoning { .. }
            | HubEvent::ReasoningItem { .. } => Vec::new(),
        }
    }

    fn finish(&mut self) -> Vec<u8> {
        if self.done || !self.touched {
            return Vec::new();
        }
        self.done = true;
        let mut out = self.flush();
        let reason = self.finish.take().unwrap_or(HubFinish::Stop);
        let sources = self.citations.iter().map(|citation| (None, citation));
        let grounding = grounding::encode(sources, &self.queries);
        let closing = candidate(
            vec![text_part("", None)],
            Some(encode_finish(&reason)),
            grounding,
        );
        out.extend(self.chunk(closing, encode_usage(&self.usage)));
        out
    }
}
