//! Upstream stream decoding: Responses SSE events to hub events.

use serde_json::Value;

use super::OWN;
use super::decode_items::{
    call_id_of, failure_message, finish_of, parse_usage, reasoning_origin, search_input,
    unsupported_item, url_citation,
};
use crate::llm::adapters::{
    StreamDecoder, WEB_SEARCH_TOOL_NAME, arguments_text, parse_sse_json, str_of, take_str,
};
use crate::llm::hub::{HubEvent, HubFinish};
use crate::llm::sse::SseEvent;
use spanbay_base::error::AppError;

/// The stream events the decoder acts on, named by the SSE `event:` line or, without one, by the
/// data's `type`.
#[derive(Clone, Copy, PartialEq, Eq)]
enum EventKind {
    Created,
    ItemAdded,
    /// `response.output_text.delta` and `response.refusal.delta`.
    TextDelta,
    SummaryDelta,
    ReasoningTextDelta,
    ArgumentsDelta,
    ArgumentsDone,
    ItemDone,
    AnnotationAdded,
    Completed,
    Incomplete,
    Failed,
    Error,
    /// Any other event; ignored.
    Other,
}

impl EventKind {
    fn parse(name: &str) -> Self {
        match name {
            "response.created" => Self::Created,
            "response.output_item.added" => Self::ItemAdded,
            "response.output_text.delta" | "response.refusal.delta" => Self::TextDelta,
            "response.reasoning_summary_text.delta" => Self::SummaryDelta,
            "response.reasoning_text.delta" => Self::ReasoningTextDelta,
            "response.function_call_arguments.delta" => Self::ArgumentsDelta,
            "response.function_call_arguments.done" => Self::ArgumentsDone,
            "response.output_item.done" => Self::ItemDone,
            "response.output_text.annotation.added" => Self::AnnotationAdded,
            "response.completed" => Self::Completed,
            "response.incomplete" => Self::Incomplete,
            "response.failed" => Self::Failed,
            "error" => Self::Error,
            _ => Self::Other,
        }
    }
}

/// A function call the stream opened.
struct CallSlot {
    /// The output item id, which argument events name.
    item_id: String,
    /// The output index, the fallback key when an argument event names no item.
    output_index: Option<u64>,
    /// The hub tool-call index.
    index: u32,
    /// Arguments already went out (with the item or as deltas), so the full text repeated by a
    /// `.done` event is not sent again.
    got_args: bool,
}

/// The reasoning summary part the last summary delta belonged to.
struct SummaryCursor {
    item_id: String,
    index: u64,
}

#[derive(Default)]
pub(super) struct ResponsesDecoder {
    calls: Vec<CallSlot>,
    summary: Option<SummaryCursor>,
    /// A terminal or error event arrived; `finish` adds nothing more.
    ended: bool,
}

/// The non-empty `delta` string of an event, moved out of the event.
fn take_delta(data: &mut Value) -> Option<String> {
    take_str(data, "delta").filter(|delta| !delta.is_empty())
}

fn output_index(data: &Value) -> Option<u64> {
    data.get("output_index").and_then(Value::as_u64)
}

/// The error kind of an error object or event; the generic word `error` is no kind.
fn error_kind(value: &Value) -> Option<String> {
    let error = value
        .get("error")
        .filter(|e| e.is_object())
        .unwrap_or(value);
    str_of(error, "code")
        .or_else(|| str_of(error, "type"))
        .filter(|kind| *kind != "error")
        .map(str::to_owned)
}

/// Fails on an output item the hub cannot carry: dropping it would silently lose content.
fn ensure_known_item(kind: Option<&str>) -> Result<(), AppError> {
    match kind {
        Some("message" | "reasoning" | "function_call" | "web_search_call") => Ok(()),
        other => Err(unsupported_item(other)),
    }
}

fn start_events(data: &Value) -> Vec<HubEvent> {
    let response = data.get("response").unwrap_or(data);
    vec![
        HubEvent::Start {
            id: str_of(response, "id").unwrap_or_default().to_owned(),
            model: str_of(response, "model").unwrap_or_default().to_owned(),
        },
        HubEvent::Origin { protocol: OWN },
    ]
}

impl ResponsesDecoder {
    fn find_call(
        &mut self,
        item_id: Option<&str>,
        output_index: Option<u64>,
    ) -> Option<&mut CallSlot> {
        let by_id = item_id.and_then(|id| self.calls.iter().position(|call| call.item_id == id));
        let position = by_id.or_else(|| {
            output_index.and_then(|index| {
                self.calls
                    .iter()
                    .position(|call| call.output_index == Some(index))
            })
        })?;
        self.calls.get_mut(position)
    }

    /// The tool-call index that late (`.done`) arguments belong to, when the call has sent none
    /// yet; marks it as having arguments.
    fn late_arguments(
        &mut self,
        item_id: Option<&str>,
        output_index: Option<u64>,
        has_arguments: bool,
    ) -> Option<u32> {
        if !has_arguments {
            return None;
        }
        let call = self
            .find_call(item_id, output_index)
            .filter(|call| !call.got_args)?;
        call.got_args = true;
        Some(call.index)
    }

    fn on_item_added(&mut self, data: &Value) -> Result<Vec<HubEvent>, AppError> {
        let item = data.get("item").unwrap_or(&Value::Null);
        ensure_known_item(str_of(item, "type"))?;
        if str_of(item, "type") != Some("function_call") {
            return Ok(Vec::new());
        }
        let index = u32::try_from(self.calls.len()).unwrap_or(u32::MAX);
        let arguments = arguments_text(item.get("arguments"));
        self.calls.push(CallSlot {
            item_id: str_of(item, "id").unwrap_or_default().to_owned(),
            output_index: output_index(data),
            index,
            got_args: !arguments.is_empty(),
        });
        let mut events = vec![HubEvent::ToolCallStart {
            index,
            id: call_id_of(item).to_owned(),
            name: str_of(item, "name").unwrap_or_default().to_owned(),
            signature: None,
        }];
        if !arguments.is_empty() {
            events.push(HubEvent::ToolCallDelta { index, arguments });
        }
        Ok(events)
    }

    /// A summary delta; a new summary part of the same reasoning item starts a new paragraph.
    fn on_summary_delta(&mut self, data: &mut Value) -> Vec<HubEvent> {
        let Some(text) = take_delta(data) else {
            return Vec::new();
        };
        let item_id = str_of(data, "item_id").unwrap_or_default();
        let index = data
            .get("summary_index")
            .and_then(Value::as_u64)
            .unwrap_or(0);
        let mut events = Vec::new();
        match &mut self.summary {
            Some(cursor) if cursor.item_id == item_id => {
                if cursor.index != index {
                    events.push(HubEvent::ReasoningDelta {
                        text: "\n\n".into(),
                    });
                    cursor.index = index;
                }
            }
            _ => {
                self.summary = Some(SummaryCursor {
                    item_id: item_id.to_owned(),
                    index,
                });
            }
        }
        events.push(HubEvent::ReasoningDelta { text });
        events
    }

    fn on_arguments_delta(&mut self, data: &mut Value) -> Vec<HubEvent> {
        let Some(arguments) = take_delta(data) else {
            return Vec::new();
        };
        match self.find_call(str_of(data, "item_id"), output_index(data)) {
            Some(call) => {
                call.got_args = true;
                vec![HubEvent::ToolCallDelta {
                    index: call.index,
                    arguments,
                }]
            }
            None => Vec::new(),
        }
    }

    /// The full arguments of a call, sent only when no delta carried them.
    fn on_arguments_done(&mut self, data: &mut Value) -> Vec<HubEvent> {
        let has_arguments = str_of(data, "arguments").is_some_and(|a| !a.is_empty());
        let index = self.late_arguments(str_of(data, "item_id"), output_index(data), has_arguments);
        late_delta(index, data)
    }

    fn on_item_done(&mut self, data: &mut Value) -> Result<Vec<HubEvent>, AppError> {
        let output_index = output_index(data);
        let mut item = data.get_mut("item").map(Value::take).unwrap_or_default();
        ensure_known_item(str_of(&item, "type"))?;
        Ok(match str_of(&item, "type") {
            Some("web_search_call") => vec![HubEvent::ServerToolStart {
                id: str_of(&item, "id").unwrap_or_default().to_owned(),
                name: WEB_SEARCH_TOOL_NAME.into(),
                input: search_input(&item),
            }],
            Some("function_call") => {
                let has_arguments = str_of(&item, "arguments").is_some_and(|a| !a.is_empty());
                let index = self.late_arguments(str_of(&item, "id"), output_index, has_arguments);
                late_delta(index, &mut item)
            }
            Some("reasoning") => {
                let id = take_str(&mut item, "id");
                let encrypted = take_str(&mut item, "encrypted_content");
                if reasoning_origin(id.as_deref(), encrypted.as_deref()).is_some() {
                    vec![HubEvent::ReasoningItem { id, encrypted }]
                } else {
                    Vec::new()
                }
            }
            _ => Vec::new(),
        })
    }

    fn on_error(&mut self, data: &Value) -> Vec<HubEvent> {
        self.ended = true;
        let source = data.get("error").filter(|e| e.is_object()).unwrap_or(data);
        vec![HubEvent::Error {
            message: str_of(source, "message")
                .unwrap_or("upstream stream error")
                .to_owned(),
            kind: error_kind(data),
        }]
    }

    fn terminal(&mut self, kind: EventKind, data: &Value) -> Vec<HubEvent> {
        self.ended = true;
        let response = data.get("response").unwrap_or(data);
        if kind == EventKind::Failed {
            return vec![HubEvent::Error {
                message: failure_message(response).unwrap_or_else(|| "the response failed".into()),
                kind: error_kind(response),
            }];
        }
        let mut events = Vec::new();
        if let Some(usage) = response.get("usage").filter(|u| u.is_object()) {
            events.push(HubEvent::Usage {
                usage: parse_usage(usage),
            });
        }
        let status = str_of(response, "status").or(if kind == EventKind::Incomplete {
            Some("incomplete")
        } else {
            None
        });
        events.push(HubEvent::Finish {
            reason: finish_of(status, response, !self.calls.is_empty()),
        });
        events
    }
}

/// The `arguments` of `source` as a delta of call `index`, moved out of `source`.
fn late_delta(index: Option<u32>, source: &mut Value) -> Vec<HubEvent> {
    index
        .and_then(|index| {
            let arguments = take_str(source, "arguments")?;
            Some(vec![HubEvent::ToolCallDelta { index, arguments }])
        })
        .unwrap_or_default()
}

impl StreamDecoder for ResponsesDecoder {
    fn push(&mut self, event: &SseEvent) -> Result<Vec<HubEvent>, AppError> {
        let Some(mut data) = parse_sse_json(event, "Responses stream event")? else {
            return Ok(Vec::new());
        };
        let kind = EventKind::parse(
            event
                .event
                .as_deref()
                .or_else(|| str_of(&data, "type"))
                .unwrap_or_default(),
        );
        Ok(match kind {
            EventKind::Created => start_events(&data),
            EventKind::ItemAdded => self.on_item_added(&data)?,
            EventKind::TextDelta => take_delta(&mut data)
                .map(|text| vec![HubEvent::TextDelta { text }])
                .unwrap_or_default(),
            EventKind::SummaryDelta => self.on_summary_delta(&mut data),
            EventKind::ReasoningTextDelta => take_delta(&mut data)
                .map(|text| vec![HubEvent::ReasoningDelta { text }])
                .unwrap_or_default(),
            EventKind::ArgumentsDelta => self.on_arguments_delta(&mut data),
            EventKind::ArgumentsDone => self.on_arguments_done(&mut data),
            EventKind::ItemDone => self.on_item_done(&mut data)?,
            EventKind::AnnotationAdded => data
                .get("annotation")
                .and_then(url_citation)
                .map(|citation| vec![HubEvent::Citation { citation }])
                .unwrap_or_default(),
            EventKind::Completed | EventKind::Incomplete | EventKind::Failed => {
                self.terminal(kind, &data)
            }
            EventKind::Error => self.on_error(&data),
            EventKind::Other => Vec::new(),
        })
    }

    fn finish(&mut self) -> Result<Vec<HubEvent>, AppError> {
        if self.ended {
            return Ok(Vec::new());
        }
        self.ended = true;
        let reason = if self.calls.is_empty() {
            HubFinish::Stop
        } else {
            HubFinish::ToolCalls
        };
        Ok(vec![HubEvent::Finish { reason }])
    }
}
