//! Streams: decoding upstream SSE events, round trips, and the exact bytes of every frame toward
//! a Responses client.

use super::stream_frames::{
    AFTER_FINISH, CITATION_FIRST, CONTENT_FILTER, EMPTY_TEXT_DELTA, ERROR_WITH_KIND,
    ERROR_WITHOUT_KIND, FOREIGN_SEARCH, LENGTH, MAIN, TEXT_OVER,
};
use super::*;
use crate::llm::hub::{MAX_REASONING_BYTES, MAX_TEXT_BYTES, MAX_TOOL_ARGUMENT_BYTES};

fn start() -> HubEvent {
    HubEvent::Start {
        id: "resp_9".into(),
        model: "gpt".into(),
    }
}

/// `created_at` is the clock; the goldens pin it to 0.
fn normalise(text: &str) -> String {
    regex::Regex::new(r#""created_at":\d+"#)
        .unwrap()
        .replace_all(text, r#""created_at":0"#)
        .into_owned()
}

/// The SSE text of `(event, data)` frames.
fn frames(expected: &[(&str, &str)]) -> String {
    expected
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect()
}

/// The normalised stream text of `events`, `finish` included.
fn encode_stream(events: &[HubEvent]) -> String {
    normalise(&test_support::encode_stream(&OpenaiResponses, events))
}

fn decode(raw: &str) -> Vec<HubEvent> {
    test_support::decode_sse_text(&OpenaiResponses, raw)
}

fn try_decode(raw: &str) -> Result<Vec<HubEvent>, AppError> {
    test_support::try_decode_sse_text(&OpenaiResponses, raw)
}

/// One `data:` frame per JSON value, without `event:` lines.
fn data_frames(values: &[Value]) -> String {
    values.iter().map(|v| format!("data: {v}\n\n")).collect()
}

// --- Encoding toward a client ---

#[test]
fn stream_round_trip_matches_collect_events() {
    let events = vec![
        start(),
        HubEvent::Origin {
            protocol: Protocol::OpenaiResponses,
        },
        HubEvent::ReasoningDelta {
            text: "thinking".into(),
        },
        HubEvent::ReasoningItem {
            id: Some("rs_x".into()),
            encrypted: Some("enc".into()),
        },
        HubEvent::TextDelta {
            text: "calling".into(),
        },
        HubEvent::ToolCallStart {
            index: 0,
            id: "c1".into(),
            name: "f".into(),
            signature: None,
        },
        HubEvent::ToolCallDelta {
            index: 0,
            arguments: "{\"a\":1}".into(),
        },
        HubEvent::Usage {
            usage: HubUsage {
                input_tokens: Some(12),
                output_tokens: Some(8),
                cached_input_tokens: Some(3),
                cache_write_tokens: None,
                cache_write_long_tokens: None,
                reasoning_tokens: Some(2),
            },
        },
        HubEvent::Finish {
            reason: HubFinish::ToolCalls,
        },
    ];
    let (_, decoded) = test_support::reencode(&OpenaiResponses, &events);
    assert_eq!(
        collect_events(&decoded).unwrap(),
        collect_events(&events).unwrap()
    );
}

#[test]
fn stream_encoder_caps_text_and_reasoning() {
    let text: fn(String) -> HubEvent = |s| HubEvent::TextDelta { text: s };
    let reasoning: fn(String) -> HubEvent = |s| HubEvent::ReasoningDelta { text: s };
    for (limit, make) in [(MAX_TEXT_BYTES, text), (MAX_REASONING_BYTES, reasoning)] {
        let mut encoder = OpenaiResponses.stream_encoder();
        encoder.push(&start());
        encoder.push(&make("x".repeat(limit)));
        let out = String::from_utf8(encoder.push(&make("y".into()))).unwrap();
        assert!(out.starts_with("event: error\n"));
        assert!(encoder.finish().is_empty());
    }
}

#[test]
fn stream_encoder_caps_tool_arguments() {
    let mut encoder = OpenaiResponses.stream_encoder();
    encoder.push(&start());
    encoder.push(&HubEvent::ToolCallStart {
        index: 0,
        id: "c".into(),
        name: "f".into(),
        signature: None,
    });
    encoder.push(&HubEvent::ToolCallDelta {
        index: 0,
        arguments: "x".repeat(MAX_TOOL_ARGUMENT_BYTES),
    });
    let out = String::from_utf8(encoder.push(&HubEvent::ToolCallDelta {
        index: 0,
        arguments: "y".into(),
    }))
    .unwrap();
    assert!(out.starts_with("event: error\n") && out.contains("tool-call arguments"));
}

#[test]
fn golden_stream_frames() {
    // Reasoning, text with a ranged and an unranged citation, two tool calls, text reopened after
    // them, an own web search, usage and the finish. The citation without a range covers the text
    // so far in UTF-16 units (5 for "Hi 😀"; it was 4, the char
    // count). The reasoning item is added as `rs_0` but done under the upstream id `rs_up` (a known
    // defect, pinned as it is). Tool items close last, so `output` is sorted by output index.
    let events = [
        start(),
        HubEvent::Origin {
            protocol: Protocol::OpenaiResponses,
        },
        HubEvent::ReasoningDelta {
            text: "think".into(),
        },
        HubEvent::ReasoningItem {
            id: Some("rs_up".into()),
            encrypted: Some("enc".into()),
        },
        HubEvent::TextDelta {
            text: "Hi 😀".into(),
        },
        HubEvent::Citation {
            citation: citation("https://a", Some(0), Some(2)),
        },
        HubEvent::Citation {
            citation: citation("https://b", None, None),
        },
        HubEvent::ToolCallStart {
            index: 0,
            id: "c1".into(),
            name: "f".into(),
            signature: None,
        },
        HubEvent::ToolCallDelta {
            index: 0,
            arguments: "{\"a\"".into(),
        },
        HubEvent::ToolCallDelta {
            index: 0,
            arguments: ":1}".into(),
        },
        HubEvent::ToolCallStart {
            index: 1,
            id: "c2".into(),
            name: "g".into(),
            signature: None,
        },
        HubEvent::TextDelta {
            text: "after".into(),
        },
        HubEvent::ServerToolStart {
            id: "ws_1".into(),
            name: "web_search".into(),
            input: json!({"query": "q"}),
        },
        HubEvent::Usage {
            usage: HubUsage {
                input_tokens: Some(5),
                output_tokens: Some(7),
                cached_input_tokens: Some(1),
                reasoning_tokens: Some(2),
                ..HubUsage::default()
            },
        },
        HubEvent::Finish {
            reason: HubFinish::ToolCalls,
        },
    ];
    assert_eq!(encode_stream(&events), frames(MAIN));
}

#[test]
fn golden_a_foreign_web_search_is_not_replayed() {
    let events = [
        start(),
        HubEvent::Origin {
            protocol: Protocol::AnthropicMessages,
        },
        HubEvent::ServerToolStart {
            id: "ws_1".into(),
            name: "web_search".into(),
            input: json!({}),
        },
        HubEvent::TextDelta { text: "x".into() },
    ];
    assert_eq!(encode_stream(&events), frames(FOREIGN_SEARCH));
}

#[test]
fn golden_an_over_limit_delta_is_one_error_frame() {
    let mut encoder = OpenaiResponses.stream_encoder();
    encoder.push(&start());
    encoder.push(&HubEvent::TextDelta {
        text: "x".repeat(MAX_TEXT_BYTES),
    });
    let out = encoder.push(&HubEvent::TextDelta { text: "y".into() });
    assert_eq!(
        test_support::canonicalize_sse(&String::from_utf8(out).unwrap()),
        frames(TEXT_OVER)
    );
    assert!(
        encoder
            .push(&HubEvent::TextDelta { text: "z".into() })
            .is_empty()
    );
    assert!(encoder.finish().is_empty());
}

#[test]
fn golden_an_error_before_the_start_is_the_only_frame() {
    for (kind, expected) in [
        (Some("rate_limit".to_owned()), ERROR_WITH_KIND),
        (None, ERROR_WITHOUT_KIND),
    ] {
        let events = [
            HubEvent::Error {
                message: "boom".into(),
                kind,
            },
            HubEvent::TextDelta {
                text: "ignored".into(),
            },
        ];
        assert_eq!(encode_stream(&events), frames(expected));
    }
}

#[test]
fn golden_length_and_content_filter_end_incomplete() {
    let length = [
        start(),
        HubEvent::TextDelta { text: "a".into() },
        HubEvent::Finish {
            reason: HubFinish::Length,
        },
    ];
    assert_eq!(encode_stream(&length), frames(LENGTH));
    let filtered = [
        start(),
        HubEvent::Finish {
            reason: HubFinish::ContentFilter,
        },
    ];
    assert_eq!(encode_stream(&filtered), frames(CONTENT_FILTER));
}

#[test]
fn golden_events_after_finish_still_encode_before_the_last_frame() {
    // Pins current behaviour: `Finish` only records the reason; `finish()` writes the terminal
    // frame, so a late delta still opens a message.
    let events = [
        start(),
        HubEvent::Finish {
            reason: HubFinish::Stop,
        },
        HubEvent::TextDelta {
            text: "late".into(),
        },
    ];
    assert_eq!(encode_stream(&events), frames(AFTER_FINISH));
}

#[test]
fn golden_an_empty_text_delta_opens_a_message() {
    let events = [
        start(),
        HubEvent::TextDelta {
            text: String::new(),
        },
    ];
    assert_eq!(encode_stream(&events), frames(EMPTY_TEXT_DELTA));
}

#[test]
fn golden_a_citation_before_any_text_opens_an_empty_message() {
    let events = [
        start(),
        HubEvent::Citation {
            citation: citation("https://a", None, None),
        },
    ];
    assert_eq!(encode_stream(&events), frames(CITATION_FIRST));
}

#[test]
fn an_empty_stream_writes_nothing() {
    assert_eq!(encode_stream(&[]), "");
}

// --- Decoding an upstream stream ---

#[test]
fn decodes_reasoning_tool_calls_items_and_citations() {
    // A new summary index of the same item inserts a paragraph break; a new item does not. Late
    // arguments, from `function_call_arguments.done` or a `function_call` `output_item.done`,
    // count only when the call has sent none yet, and only once.
    let raw = data_frames(&[
        json!({"type": "response.created", "response": {"id": "r", "model": "m"}}),
        json!({"type": "response.reasoning_summary_text.delta", "item_id": "rs_1", "summary_index": 0, "delta": "a"}),
        json!({"type": "response.reasoning_summary_text.delta", "item_id": "rs_1", "summary_index": 1, "delta": "b"}),
        json!({"type": "response.reasoning_summary_text.delta", "item_id": "rs_2", "summary_index": 0, "delta": "c"}),
        json!({"type": "response.reasoning_text.delta", "item_id": "rs_2", "delta": "d"}),
        json!({"type": "response.output_item.added", "output_index": 3, "item": {"type": "function_call", "id": "fc_1", "call_id": "c1", "name": "f"}}),
        json!({"type": "response.function_call_arguments.delta", "item_id": "fc_1", "delta": "{}"}),
        json!({"type": "response.function_call_arguments.done", "item_id": "fc_1", "arguments": "{\"late\":1}"}),
        json!({"type": "response.output_item.added", "output_index": 4, "item": {"type": "function_call", "id": "fc_2", "call_id": "c2", "name": "g"}}),
        json!({"type": "response.function_call_arguments.done", "output_index": 4, "arguments": "{\"x\":2}"}),
        json!({"type": "response.output_item.added", "output_index": 5, "item": {"type": "function_call", "id": "fc_3", "call_id": "c3", "name": "h"}}),
        json!({"type": "response.output_item.done", "output_index": 5, "item": {"type": "function_call", "id": "fc_3", "arguments": "{\"y\":3}"}}),
        json!({"type": "response.output_item.done", "output_index": 5, "item": {"type": "function_call", "id": "fc_3", "arguments": "{\"y\":3}"}}),
        json!({"type": "response.output_item.done", "item": {"type": "function_call", "id": "fc_1", "arguments": "{\"z\":1}"}}),
        json!({"type": "response.output_item.done", "item": {"type": "reasoning", "id": "rs_2", "encrypted_content": "E"}}),
        json!({"type": "response.output_item.done", "item": {"type": "reasoning"}}),
        json!({"type": "response.output_item.done", "item": {"type": "web_search_call", "id": "ws", "action": null}}),
        json!({"type": "response.output_text.annotation.added", "annotation": {"type": "url_citation", "url": "u", "title": "t", "start_index": 1, "end_index": 2}}),
        json!({"type": "response.incomplete", "response": {"incomplete_details": {"reason": "max_output_tokens"}}}),
    ]);
    let reasoning = |text: &str| HubEvent::ReasoningDelta { text: text.into() };
    assert_eq!(
        decode(&raw),
        vec![
            HubEvent::Start {
                id: "r".into(),
                model: "m".into(),
            },
            HubEvent::Origin {
                protocol: Protocol::OpenaiResponses,
            },
            reasoning("a"),
            reasoning("\n\n"),
            reasoning("b"),
            reasoning("c"),
            reasoning("d"),
            HubEvent::ToolCallStart {
                index: 0,
                id: "c1".into(),
                name: "f".into(),
                signature: None,
            },
            HubEvent::ToolCallDelta {
                index: 0,
                arguments: "{}".into(),
            },
            HubEvent::ToolCallStart {
                index: 1,
                id: "c2".into(),
                name: "g".into(),
                signature: None,
            },
            HubEvent::ToolCallDelta {
                index: 1,
                arguments: "{\"x\":2}".into(),
            },
            HubEvent::ToolCallStart {
                index: 2,
                id: "c3".into(),
                name: "h".into(),
                signature: None,
            },
            HubEvent::ToolCallDelta {
                index: 2,
                arguments: "{\"y\":3}".into(),
            },
            HubEvent::ReasoningItem {
                id: Some("rs_2".into()),
                encrypted: Some("E".into()),
            },
            HubEvent::ServerToolStart {
                id: "ws".into(),
                name: "web_search".into(),
                input: json!({}),
            },
            HubEvent::Citation {
                citation: HubCitation {
                    url: "u".into(),
                    title: "t".into(),
                    snippet: None,
                    start_index: Some(1),
                    end_index: Some(2),
                },
            },
            HubEvent::Finish {
                reason: HubFinish::Length,
            },
        ]
    );
}

#[test]
fn the_event_line_names_the_event_and_data_type_is_the_fallback() {
    assert_eq!(
        decode("event: response.output_text.delta\ndata: {\"delta\":\"hi\"}\n\n"),
        vec![
            HubEvent::TextDelta { text: "hi".into() },
            HubEvent::Finish {
                reason: HubFinish::Stop,
            },
        ]
    );
}

#[test]
fn an_unknown_added_item_fails_the_stream() {
    let err = try_decode(&data_frames(&[json!({
        "type": "response.output_item.added", "item": {"type": "mcp_call"}
    })]))
    .unwrap_err();
    assert!(matches!(err, AppError::Network { .. }), "{err:?}");
    assert_eq!(
        err.message(),
        "Responses output item type `mcp_call` is not supported"
    );
}

#[test]
fn failures_and_error_events_end_the_stream_with_their_kind() {
    let error = |message: &str, kind: Option<&str>| {
        vec![HubEvent::Error {
            message: message.into(),
            kind: kind.map(Into::into),
        }]
    };
    for (frame, expected) in [
        (
            json!({"type": "response.failed", "response": {"status": "failed", "error": {"code": "server_error", "message": "boom"}}}),
            error("boom", Some("server_error")),
        ),
        (
            json!({"type": "error", "error": {"type": "invalid_request_error", "message": "bad"}}),
            error("bad", Some("invalid_request_error")),
        ),
        (
            json!({"type": "error", "code": "rate_limit", "message": "slow"}),
            error("slow", Some("rate_limit")),
        ),
        (
            json!({"type": "error"}),
            error("upstream stream error", None),
        ),
    ] {
        assert_eq!(decode(&data_frames(&[frame])), expected);
    }
}

#[test]
fn a_stream_without_a_terminal_event_finishes_from_its_calls() {
    assert_eq!(
        decode(&data_frames(&[
            json!({"type": "response.output_text.delta", "delta": "x"})
        ])),
        vec![
            HubEvent::TextDelta { text: "x".into() },
            HubEvent::Finish {
                reason: HubFinish::Stop,
            },
        ]
    );
    assert_eq!(
        decode(&data_frames(&[json!({
            "type": "response.output_item.added",
            "item": {"type": "function_call", "id": "i", "name": "f", "arguments": "{}"}
        })])),
        vec![
            HubEvent::ToolCallStart {
                index: 0,
                id: "i".into(),
                name: "f".into(),
                signature: None,
            },
            HubEvent::ToolCallDelta {
                index: 0,
                arguments: "{}".into(),
            },
            HubEvent::Finish {
                reason: HubFinish::ToolCalls,
            },
        ]
    );
}

#[test]
fn the_completed_event_carries_usage() {
    assert_eq!(
        decode(&data_frames(&[json!({
            "type": "response.completed",
            "response": {"status": "completed", "usage": {
                "input_tokens": 2, "output_tokens": 3,
                "input_tokens_details": {"cached_tokens": 1, "cache_write_tokens": 4},
                "output_tokens_details": {"reasoning_tokens": 1},
            }}
        })])),
        vec![
            HubEvent::Usage {
                usage: HubUsage {
                    input_tokens: Some(2),
                    output_tokens: Some(3),
                    cached_input_tokens: Some(1),
                    cache_write_tokens: Some(4),
                    cache_write_long_tokens: None,
                    reasoning_tokens: Some(1),
                },
            },
            HubEvent::Finish {
                reason: HubFinish::Stop,
            },
        ]
    );
}
