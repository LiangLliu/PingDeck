//! The merged reply per protocol, streaming and not.

use serde_json::json;

use super::bounded;
use crate::llm::capture::{
    CapturedBody, CapturedFrame, CapturedFrames, MergedReply, MergedSource, MergedToolCall,
    merged_reply,
};
use crate::llm::wire::Protocol;

/// The `name(arguments)` form the drawer prints.
fn render(call: &MergedToolCall) -> String {
    format!("{}({})", call.name, call.arguments)
}

fn frames(list: &[(&str, &str)]) -> CapturedFrames {
    let frames: Vec<CapturedFrame> = list
        .iter()
        .enumerate()
        .map(|(i, (event, data))| CapturedFrame {
            index: i as u32 + 1,
            t_ms: i as u64 * 10,
            event: (*event).to_owned(),
            data: (*data).to_owned(),
        })
        .collect();
    CapturedFrames {
        total_frames: frames.len() as u64,
        frames,
        truncated: false,
    }
}

fn from_frames(protocol: Protocol, list: &[(&str, &str)]) -> MergedReply {
    merged_reply(protocol, MergedSource::Frames(&frames(list)))
}

fn from_body(protocol: Protocol, body: serde_json::Value) -> MergedReply {
    let body = CapturedBody::from(bounded(&body.to_string()));
    merged_reply(protocol, MergedSource::Body(&body))
}

#[test]
fn chat_stream_concatenates_deltas_into_pong() {
    let reply = from_frames(
        Protocol::OpenaiChat,
        &[
            (
                "message",
                r#"{"id":"c1","model":"m","choices":[{"index":0,"delta":{"role":"assistant","content":""}}]}"#,
            ),
            (
                "message",
                r#"{"choices":[{"index":0,"delta":{"reasoning_content":"hm"}}]}"#,
            ),
            (
                "message",
                r#"{"choices":[{"index":0,"delta":{"content":"p"}}]}"#,
            ),
            (
                "message",
                r#"{"choices":[{"index":0,"delta":{"content":"ong"}}]}"#,
            ),
            (
                "message",
                r#"{"choices":[{"index":0,"delta":{},"finish_reason":"stop"}],"usage":{"prompt_tokens":3,"completion_tokens":2}}"#,
            ),
            ("message", "[DONE]"),
        ],
    );
    assert_eq!(reply.text, "pong");
    assert_eq!(reply.reasoning, "hm");
    assert_eq!(reply.finish_reason.as_deref(), Some("stop"));
    let usage = reply.usage.unwrap();
    assert_eq!(
        (usage.input_tokens, usage.output_tokens),
        (Some(3), Some(2))
    );
    assert_eq!(reply.error, None);
    assert!(!reply.partial);
}

#[test]
fn chat_stream_assembles_tool_call_arguments_by_index() {
    let reply = from_frames(
        Protocol::OpenaiChat,
        &[
            (
                "message",
                r#"{"id":"c","model":"m","choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"id":"call_1","type":"function","function":{"name":"get_weather","arguments":""}}]}}]}"#,
            ),
            (
                "message",
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"{\"city\":"}}]}}]}"#,
            ),
            (
                "message",
                r#"{"choices":[{"index":0,"delta":{"tool_calls":[{"index":0,"function":{"arguments":"\"Oslo\"}"}}]}}]}"#,
            ),
            (
                "message",
                r#"{"choices":[{"index":0,"delta":{},"finish_reason":"tool_calls"}]}"#,
            ),
        ],
    );
    assert_eq!(reply.tool_calls.len(), 1);
    assert_eq!(
        render(&reply.tool_calls[0]),
        r#"get_weather({"city":"Oslo"})"#
    );
    assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
}

#[test]
fn chat_body_is_decoded_directly() {
    let reply = from_body(
        Protocol::OpenaiChat,
        json!({"id":"c1","model":"m","choices":[{"index":0,"message":{"role":"assistant","content":"pong","reasoning_content":"hm","tool_calls":[{"id":"c","type":"function","function":{"name":"get_weather","arguments":"{\"city\":\"Oslo\"}"}}]},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":3,"completion_tokens":1}}),
    );
    assert_eq!(reply.text, "pong");
    assert_eq!(reply.reasoning, "hm");
    assert_eq!(
        render(&reply.tool_calls[0]),
        r#"get_weather({"city":"Oslo"})"#
    );
    assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
}

#[test]
fn responses_stream_concatenates_text_reasoning_and_arguments() {
    let reply = from_frames(
        Protocol::OpenaiResponses,
        &[
            (
                "response.created",
                r#"{"type":"response.created","response":{"id":"r1","model":"m"}}"#,
            ),
            (
                "response.reasoning_summary_text.delta",
                r#"{"type":"response.reasoning_summary_text.delta","item_id":"rs1","summary_index":0,"delta":"think"}"#,
            ),
            (
                "response.output_item.added",
                r#"{"type":"response.output_item.added","output_index":2,"item":{"type":"function_call","id":"fc1","call_id":"call_1","name":"get_weather","arguments":""}}"#,
            ),
            (
                "response.function_call_arguments.delta",
                r#"{"type":"response.function_call_arguments.delta","item_id":"fc1","output_index":2,"delta":"{\"city\":"}"#,
            ),
            (
                "response.function_call_arguments.delta",
                r#"{"type":"response.function_call_arguments.delta","item_id":"fc1","output_index":2,"delta":"\"Oslo\"}"}"#,
            ),
            (
                "response.output_text.delta",
                r#"{"type":"response.output_text.delta","item_id":"m1","output_index":1,"delta":"po"}"#,
            ),
            (
                "response.output_text.delta",
                r#"{"type":"response.output_text.delta","item_id":"m1","output_index":1,"delta":"ng"}"#,
            ),
            (
                "response.completed",
                r#"{"type":"response.completed","response":{"id":"r1","status":"completed","usage":{"input_tokens":4,"output_tokens":6}}}"#,
            ),
        ],
    );
    assert_eq!(reply.text, "pong");
    assert_eq!(reply.reasoning, "think");
    assert_eq!(
        render(&reply.tool_calls[0]),
        r#"get_weather({"city":"Oslo"})"#
    );
    assert_eq!(reply.usage.unwrap().output_tokens, Some(6));
}

#[test]
fn responses_body_is_decoded_directly() {
    let reply = from_body(
        Protocol::OpenaiResponses,
        json!({"id":"r1","model":"m","status":"completed","output":[
            {"type":"reasoning","id":"rs1","summary":[{"type":"summary_text","text":"think"}]},
            {"type":"message","role":"assistant","content":[{"type":"output_text","text":"pong"}]},
            {"type":"function_call","call_id":"c1","name":"get_weather","arguments":"{\"city\":\"Oslo\"}"}
        ],"usage":{"input_tokens":4,"output_tokens":6}}),
    );
    assert_eq!(reply.text, "pong");
    assert_eq!(reply.reasoning, "think");
    assert_eq!(reply.tool_calls[0].name, "get_weather");
}

#[test]
fn messages_stream_merges_blocks_by_index() {
    let reply = from_frames(
        Protocol::AnthropicMessages,
        &[
            (
                "message_start",
                r#"{"type":"message_start","message":{"id":"m","model":"c","usage":{"input_tokens":5,"output_tokens":1}}}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"thinking","thinking":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"Let me "}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"thinking_delta","thinking":"think."}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":0}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":1,"content_block":{"type":"text","text":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"po"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":1,"delta":{"type":"text_delta","text":"ng"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":1}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":2,"content_block":{"type":"tool_use","id":"tu1","name":"get_weather","input":{}}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"{\"city\":"}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":2,"delta":{"type":"input_json_delta","partial_json":"\"Oslo\"}"}}"#,
            ),
            (
                "content_block_stop",
                r#"{"type":"content_block_stop","index":2}"#,
            ),
            (
                "message_delta",
                r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":{"output_tokens":9}}"#,
            ),
            ("message_stop", r#"{"type":"message_stop"}"#),
        ],
    );
    assert_eq!(reply.reasoning, "Let me think.");
    assert_eq!(reply.text, "pong");
    assert_eq!(
        render(&reply.tool_calls[0]),
        r#"get_weather({"city":"Oslo"})"#
    );
    assert_eq!(reply.finish_reason.as_deref(), Some("tool_calls"));
    assert_eq!(reply.usage.unwrap().output_tokens, Some(9));
}

#[test]
fn messages_body_is_decoded_directly() {
    let reply = from_body(
        Protocol::AnthropicMessages,
        json!({"id":"m","type":"message","role":"assistant","model":"c","content":[
            {"type":"thinking","thinking":"hm","signature":"s"},
            {"type":"text","text":"pong"},
            {"type":"tool_use","id":"tu","name":"get_weather","input":{"city":"Oslo"}}
        ],"stop_reason":"tool_use","usage":{"input_tokens":3,"output_tokens":4}}),
    );
    assert_eq!(reply.text, "pong");
    assert_eq!(reply.reasoning, "hm");
    assert_eq!(
        render(&reply.tool_calls[0]),
        r#"get_weather({"city":"Oslo"})"#
    );
}

#[test]
fn gemini_stream_concatenates_parts_and_collects_function_calls() {
    let reply = from_frames(
        Protocol::Gemini,
        &[
            (
                "message",
                r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"po"}]}}],"modelVersion":"g"}"#,
            ),
            (
                "message",
                r#"{"candidates":[{"content":{"role":"model","parts":[{"text":"ng"},{"functionCall":{"name":"get_weather","args":{"city":"Oslo"}}}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":5}}"#,
            ),
        ],
    );
    assert_eq!(reply.text, "pong");
    assert_eq!(
        render(&reply.tool_calls[0]),
        r#"get_weather({"city":"Oslo"})"#
    );
    assert_eq!(reply.usage.unwrap().input_tokens, Some(3));
}

#[test]
fn gemini_body_is_decoded_directly() {
    let reply = from_body(
        Protocol::Gemini,
        json!({"candidates":[{"content":{"role":"model","parts":[{"text":"pong"}]},"finishReason":"STOP"}],"usageMetadata":{"promptTokenCount":3,"candidatesTokenCount":1}}),
    );
    assert_eq!(reply.text, "pong");
    assert_eq!(reply.finish_reason.as_deref(), Some("stop"));
}

#[test]
fn embeddings_show_the_dimension_and_the_first_eight_values() {
    let vector: Vec<f64> = (0..1536).map(|i| f64::from(i) / 10.0).collect();
    let reply = from_body(
        Protocol::Embeddings,
        json!({"data":[{"embedding":vector,"index":0}],"usage":{"prompt_tokens":5,"total_tokens":5}}),
    );
    let embedding = reply.embedding.unwrap();
    assert_eq!(embedding.dimensions, 1536);
    assert_eq!(
        embedding.first_values,
        [0.0, 0.1, 0.2, 0.3, 0.4, 0.5, 0.6, 0.7]
    );
    assert!(reply.text.is_empty());
    assert_eq!(reply.usage.unwrap().input_tokens, Some(5));
}

#[test]
fn gemini_embeddings_are_read_too() {
    let reply = from_body(
        Protocol::Embeddings,
        json!({"embedding":{"values":[1.0,2.0,3.0]}}),
    );
    assert_eq!(reply.embedding.unwrap().dimensions, 3);
}

#[test]
fn a_provider_error_event_keeps_the_partial_text_and_reports_the_error() {
    let reply = from_frames(
        Protocol::AnthropicMessages,
        &[
            (
                "message_start",
                r#"{"type":"message_start","message":{"id":"m","model":"c"}}"#,
            ),
            (
                "content_block_start",
                r#"{"type":"content_block_start","index":0,"content_block":{"type":"text","text":""}}"#,
            ),
            (
                "content_block_delta",
                r#"{"type":"content_block_delta","index":0,"delta":{"type":"text_delta","text":"half"}}"#,
            ),
            (
                "error",
                r#"{"type":"error","error":{"type":"overloaded_error","message":"Overloaded"}}"#,
            ),
        ],
    );
    assert_eq!(reply.text, "half");
    assert_eq!(reply.error.as_deref(), Some("Overloaded"));
}

#[test]
fn truncated_frames_mark_the_reply_partial() {
    let mut cut = frames(&[(
        "message",
        r#"{"id":"c","model":"m","choices":[{"index":0,"delta":{"content":"he"}}]}"#,
    )]);
    cut.total_frames = 6_413;
    cut.truncated = true;
    let reply = merged_reply(Protocol::OpenaiChat, MergedSource::Frames(&cut));
    assert_eq!(reply.text, "he");
    assert!(reply.partial);
}

#[test]
fn an_undecodable_body_is_reported_not_guessed() {
    let body = CapturedBody::from(bounded("upstream exploded"));
    let reply = merged_reply(Protocol::OpenaiChat, MergedSource::Body(&body));
    assert!(reply.error.is_some() && reply.text.is_empty());
    let cut = CapturedBody {
        text: Some(r#"{"choices":[{"mes"#.to_owned()),
        truncated: true,
        elided: false,
        original_bytes: 900_000,
    };
    let reply = merged_reply(Protocol::OpenaiChat, MergedSource::Body(&cut));
    assert!(reply.partial && reply.error.unwrap().contains("256 KiB"));
}

#[test]
fn a_missing_body_and_decision_protocols_have_no_reply() {
    let reply = merged_reply(
        Protocol::OpenaiChat,
        MergedSource::Body(&CapturedBody::default()),
    );
    assert_eq!(reply.error.as_deref(), Some("No response body was stored"));
    let reply = from_body(Protocol::DecisionOpenai, json!({}));
    assert!(reply.error.is_some());
}

#[test]
fn embeddings_frames_have_no_reply() {
    let reply = from_frames(Protocol::Embeddings, &[("message", "{}")]);
    assert_eq!(reply.error.as_deref(), Some("Embeddings are not streamed"));
    assert_eq!(
        reply,
        MergedReply {
            error: Some("Embeddings are not streamed".to_owned()),
            ..MergedReply::default()
        }
    );
}

#[test]
fn several_reasoning_parts_are_joined_by_a_blank_line() {
    let reply = from_body(
        Protocol::AnthropicMessages,
        json!({"id": "m", "type": "message", "role": "assistant", "model": "c", "content": [
            {"type": "thinking", "thinking": "first", "signature": "s1"},
            {"type": "thinking", "thinking": "second", "signature": "s2"},
            {"type": "text", "text": "ok"}
        ], "stop_reason": "end_turn", "usage": {"input_tokens": 1, "output_tokens": 2}}),
    );
    assert_eq!(reply.reasoning, "first\n\nsecond");
    assert_eq!(reply.text, "ok");
    assert_eq!(reply.error, None);
}
