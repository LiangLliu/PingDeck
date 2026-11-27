//! The reply of an answer: text, reasoning, tool calls, finish reason, usage, vectors.
//!
//! Single responsibility: fold the decoded events of a stream, or the JSON of a plain answer, into
//! the plain values the result shows. It uses the same decoders as the log drawer, so the response
//! tabs and the stored capture say the same.

use serde_json::Value;

use super::read::Reading;
use super::types::{AiToolCallDto, AiVectorDto};
use crate::llm::adapters::{self, embeddings};
use crate::llm::hub::{HubEvent, HubFinish, HubPart, HubResponse, HubUsage, collect_events};
use crate::llm::wire::Protocol;

/// How many values of each embedding vector are shown.
const VECTOR_PREVIEW: usize = 8;

/// What an answer says.
#[derive(Debug, Default)]
pub(super) struct Merged {
    pub reply: String,
    pub reasoning: String,
    pub tool_calls: Vec<AiToolCallDto>,
    pub vectors: Vec<AiVectorDto>,
    pub finish: Option<String>,
    pub usage: HubUsage,
    pub model: Option<String>,
    /// An error event inside the stream.
    pub stream_error: Option<String>,
}

fn finish_word(finish: &HubFinish) -> String {
    match finish {
        HubFinish::Stop => "stop".to_owned(),
        HubFinish::Length => "length".to_owned(),
        HubFinish::Pause => "pause".to_owned(),
        HubFinish::ToolCalls => "tool_calls".to_owned(),
        HubFinish::ContentFilter => "content_filter".to_owned(),
        HubFinish::Other(other) => other.clone(),
    }
}

fn from_response(response: &HubResponse) -> Merged {
    let reasoning: Vec<&str> = response
        .content
        .iter()
        .filter_map(|part| match part {
            HubPart::Reasoning { text, .. } if !text.is_empty() => Some(text.as_str()),
            _ => None,
        })
        .collect();
    Merged {
        reply: response.text(),
        reasoning: reasoning.join("\n\n"),
        tool_calls: response
            .tool_calls()
            .into_iter()
            .map(|(id, name, arguments)| AiToolCallDto {
                id: id.to_owned(),
                name: name.to_owned(),
                arguments: arguments.to_owned(),
            })
            .collect(),
        usage: response.usage.clone(),
        model: Some(response.model.clone()).filter(|model| !model.is_empty()),
        ..Merged::default()
    }
}

/// Folds the events of a (possibly cut) stream. The finish reason is the one the stream sent;
/// a stream that was cut has none.
pub(super) fn from_stream(reading: &Reading) -> Merged {
    let stream_error = reading.events.iter().find_map(|event| match event {
        HubEvent::Error { message, .. } => Some(message.clone()),
        _ => None,
    });
    let events: Vec<HubEvent> = reading
        .events
        .iter()
        .filter(|event| !matches!(event, HubEvent::Error { .. }))
        .cloned()
        .collect();
    let mut merged = match collect_events(&events) {
        Ok(response) => from_response(&response),
        Err(message) => Merged {
            stream_error: Some(message),
            ..Merged::default()
        },
    };
    merged.finish = reading.finish.as_ref().map(finish_word);
    merged.stream_error = merged.stream_error.or(stream_error);
    merged
}

/// Reads a plain JSON answer of `protocol`.
///
/// # Errors
/// The message of an answer that is not valid JSON or not a valid answer of the protocol.
pub(super) fn from_body(protocol: Protocol, raw: &[u8]) -> Result<Merged, String> {
    let json: Value = serde_json::from_slice(raw)
        .map_err(|_| "the response body is not valid JSON".to_owned())?;
    if protocol == Protocol::Embeddings || json.pointer("/embedding/values").is_some() {
        let vectors = vectors_of(&json);
        if vectors.is_empty() {
            return Err("the response holds no embedding vector".to_owned());
        }
        return Ok(Merged {
            vectors,
            usage: embeddings::usage(&json),
            ..Merged::default()
        });
    }
    let codec = adapters::codec(protocol)
        .ok_or_else(|| "this protocol has no reply to merge".to_owned())?;
    let response = codec
        .decode_response(&json)
        .map_err(|error| error.message().to_owned())?;
    let mut merged = from_response(&response);
    merged.finish = Some(finish_word(&response.finish));
    Ok(merged)
}

fn preview(values: &[Value]) -> AiVectorDto {
    AiVectorDto {
        dimensions: values.len(),
        first_values: values
            .iter()
            .take(VECTOR_PREVIEW)
            .filter_map(Value::as_f64)
            .collect(),
    }
}

/// Every vector of an embeddings answer: OpenAI `data[*].embedding`, Gemini `embedding.values`
/// and the batch form `embeddings[*].values`.
fn vectors_of(json: &Value) -> Vec<AiVectorDto> {
    let list = |path: &str, key: &str| -> Vec<AiVectorDto> {
        json.pointer(path)
            .and_then(Value::as_array)
            .map(|items| {
                items
                    .iter()
                    .filter_map(|item| item.get(key).and_then(Value::as_array))
                    .map(|values| preview(values))
                    .collect()
            })
            .unwrap_or_default()
    };
    let mut vectors = list("/data", "embedding");
    vectors.extend(list("/embeddings", "values"));
    if let Some(values) = json.pointer("/embedding/values").and_then(Value::as_array) {
        vectors.push(preview(values));
    }
    vectors
}
