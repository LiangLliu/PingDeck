//! Stream decoding and encoding.

use super::*;

#[test]
fn stream_round_trip_preserves_events() {
    let events = vec![
        HubEvent::Start {
            id: "msg_b".into(),
            model: "claude-x".into(),
        },
        HubEvent::Origin { protocol: OWN },
        HubEvent::Usage {
            usage: HubUsage {
                input_tokens: Some(30),
                output_tokens: Some(0),
                cached_input_tokens: Some(10),
                ..HubUsage::default()
            },
        },
        HubEvent::ReasoningDelta {
            text: "think hard".into(),
        },
        HubEvent::ReasoningSignature {
            signature: "SIG".into(),
        },
        HubEvent::TextDelta {
            text: "answer".into(),
        },
        HubEvent::ToolCallStart {
            index: 0,
            id: "t1".into(),
            name: "f".into(),
            signature: None,
        },
        HubEvent::ToolCallDelta {
            index: 0,
            arguments: r#"{"city":"Paris"}"#.into(),
        },
        HubEvent::Usage {
            usage: HubUsage {
                input_tokens: Some(30),
                output_tokens: Some(12),
                cached_input_tokens: Some(10),
                ..HubUsage::default()
            },
        },
        HubEvent::Finish {
            reason: HubFinish::ToolCalls,
        },
    ];
    let (text, decoded) = reencode(&events);
    assert!(text.ends_with("event: message_stop\ndata: {\"type\":\"message_stop\"}\n\n"));
    assert_eq!(collect_events(&decoded), collect_events(&events));
}

#[test]
fn stream_duplicate_fields_keep_escaped_fragments_usage_and_errors() {
    let raw = concat!(
        "event: message_start\ndata: ",
        r#"{"type":"message_start","message":{"id":"m1","model":"old","model":"claude","usage":{"input_tokens":10,"cache_read_input_tokens":2,"cache_creation_input_tokens":3,"output_tokens":0}}}"#,
        "\n\nevent: content_block_start\ndata: ",
        r#"{"type":"content_block_start","index":5,"content_block":{"type":"tool_use","id":null,"id":"call","name":false,"name":"read"}}"#,
        "\n\nevent: content_block_delta\ndata: ",
        r#"{"type":"content_block_delta","index":8,"delta":{"type":"text_delta","text":"old","text":"\u4e2d"}}"#,
        "\n\nevent: content_block_delta\ndata: ",
        r#"{"type":"content_block_delta","index":5,"delta":{"type":"input_json_delta","partial_json":null,"partial_json":"{\"city\":\"\u4e2d"}}"#,
        "\n\nevent: content_block_delta\ndata: ",
        r#"{"type":"content_block_delta","index":5,"delta":{"type":"input_json_delta","partial_json":"\"}"}}"#,
        "\n\nevent: message_delta\ndata: ",
        r#"{"type":"message_delta","delta":{"stop_reason":"tool_use"},"usage":[],"usage":{"output_tokens":2}}"#,
        "\n\nevent: message_stop\ndata: {\"type\":\"message_stop\"}\n\n",
    );
    let response = collect_events(&decode_stream(raw)).unwrap();
    assert_eq!(response.text(), "中");
    assert_eq!(
        response.tool_calls(),
        [("call", "read", "{\"city\":\"中\"}")]
    );
    assert_eq!(response.model, "claude");
    assert_eq!(response.usage.input_tokens, Some(15));
    assert_eq!(response.usage.cached_input_tokens, Some(2));
    assert_eq!(response.usage.cache_write_tokens, Some(3));
    assert_eq!(response.usage.output_tokens, Some(2));
    assert_eq!(response.finish, HubFinish::ToolCalls);

    let errors = decode_stream(concat!(
        "event: error\ndata: ",
        r#"{"type":"error","error":{"message":null,"message":"boom","type":7,"type":""}}"#,
        "\n\n",
    ));
    assert_eq!(
        errors,
        [HubEvent::Error {
            message: "boom".into(),
            kind: Some(String::new())
        }]
    );
}

fn encode_all(events: &[HubEvent]) -> String {
    test_support::encode_stream(&AnthropicMessages, events)
}

fn try_decode(raw: &str) -> Result<Vec<HubEvent>, AppError> {
    test_support::try_decode_sse_text(&AnthropicMessages, raw)
}

/// Renders `(event, data)` pairs as the SSE text the encoder must produce.
fn sse_text(frames: &[(&str, &str)]) -> String {
    frames
        .iter()
        .map(|(event, data)| format!("event: {event}\ndata: {data}\n\n"))
        .collect()
}

fn start(id: &str) -> HubEvent {
    HubEvent::Start {
        id: id.into(),
        model: "claude-x".into(),
    }
}

fn scenario_reasoning_then_text() -> Vec<HubEvent> {
    vec![
        start("msg_a"),
        HubEvent::Origin { protocol: OWN },
        HubEvent::Usage {
            usage: HubUsage {
                input_tokens: Some(30),
                output_tokens: Some(4),
                cached_input_tokens: Some(10),
                cache_write_tokens: Some(5),
                ..HubUsage::default()
            },
        },
        HubEvent::ReasoningDelta {
            text: "think".into(),
        },
        HubEvent::ReasoningSignature {
            signature: "SIG".into(),
        },
        HubEvent::TextDelta { text: "hi".into() },
        HubEvent::Finish {
            reason: HubFinish::Stop,
        },
    ]
}

fn scenario_interleaved_tools() -> Vec<HubEvent> {
    let tool = |index: u32, id: &str, name: &str| HubEvent::ToolCallStart {
        index,
        id: id.into(),
        name: name.into(),
        signature: None,
    };
    let delta = |index: u32, arguments: &str| HubEvent::ToolCallDelta {
        index,
        arguments: arguments.into(),
    };
    vec![
        start("msg_b"),
        tool(0, "t0", "f"),
        tool(1, "t1", "g"),
        delta(0, r#"{"a":1}"#),
        delta(1, "{}"),
        HubEvent::TextDelta { text: "x".into() },
    ]
}

fn scenario_foreign_reasoning() -> Vec<HubEvent> {
    vec![
        start("msg_c"),
        HubEvent::Origin {
            protocol: Protocol::Gemini,
        },
        HubEvent::RedactedReasoning {
            data: "blob".into(),
        },
        HubEvent::ReasoningSignature {
            signature: "gsig".into(),
        },
        HubEvent::Finish {
            reason: HubFinish::Stop,
        },
    ]
}

fn scenario_server_tools(origin: Protocol) -> Vec<HubEvent> {
    vec![
        start("msg_d"),
        HubEvent::Origin { protocol: origin },
        HubEvent::ServerToolStart {
            id: "srv_1".into(),
            name: "web_search".into(),
            input: json!({"query": "q\n1"}),
        },
        HubEvent::ServerToolResult {
            call_id: "srv_1".into(),
            name: "web_search".into(),
            payload: json!([{"type": "web_search_result", "url": "https://a.test", "title": "A"}]),
            citations: Vec::new(),
        },
    ]
}

// Golden byte tests: the expected frames were produced by the unmodified encoder and checked by
// hand (block indices ascend, every payload repeats its `type`, keys are in alphabetical order).

#[test]
fn golden_reasoning_signature_then_text_with_cached_usage() {
    let expected = sse_text(&[
        (
            "message_start",
            r#"{"message":{"content":[],"id":"msg_a","model":"claude-x","role":"assistant","stop_reason":null,"stop_sequence":null,"type":"message","usage":{"cache_creation_input_tokens":5,"cache_read_input_tokens":10,"input_tokens":15,"output_tokens":0}},"type":"message_start"}"#,
        ),
        (
            "content_block_start",
            r#"{"content_block":{"thinking":"","type":"thinking"},"index":0,"type":"content_block_start"}"#,
        ),
        (
            "content_block_delta",
            r#"{"delta":{"thinking":"think","type":"thinking_delta"},"index":0,"type":"content_block_delta"}"#,
        ),
        (
            "content_block_delta",
            r#"{"delta":{"signature":"SIG","type":"signature_delta"},"index":0,"type":"content_block_delta"}"#,
        ),
        (
            "content_block_stop",
            r#"{"index":0,"type":"content_block_stop"}"#,
        ),
        (
            "content_block_start",
            r#"{"content_block":{"text":"","type":"text"},"index":1,"type":"content_block_start"}"#,
        ),
        (
            "content_block_delta",
            r#"{"delta":{"text":"hi","type":"text_delta"},"index":1,"type":"content_block_delta"}"#,
        ),
        (
            "content_block_stop",
            r#"{"index":1,"type":"content_block_stop"}"#,
        ),
        (
            "message_delta",
            r#"{"delta":{"stop_reason":"end_turn","stop_sequence":null},"type":"message_delta","usage":{"cache_creation_input_tokens":5,"cache_read_input_tokens":10,"input_tokens":15,"output_tokens":4}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    assert_eq!(encode_all(&scenario_reasoning_then_text()), expected);
}

#[test]
fn golden_interleaved_tool_calls_stay_open_and_close_in_index_order() {
    let expected = sse_text(&[
        (
            "message_start",
            r#"{"message":{"content":[],"id":"msg_b","model":"claude-x","role":"assistant","stop_reason":null,"stop_sequence":null,"type":"message","usage":{"input_tokens":0,"output_tokens":0}},"type":"message_start"}"#,
        ),
        (
            "content_block_start",
            r#"{"content_block":{"id":"t0","input":{},"name":"f","type":"tool_use"},"index":0,"type":"content_block_start"}"#,
        ),
        (
            "content_block_start",
            r#"{"content_block":{"id":"t1","input":{},"name":"g","type":"tool_use"},"index":1,"type":"content_block_start"}"#,
        ),
        (
            "content_block_delta",
            r#"{"delta":{"partial_json":"{\"a\":1}","type":"input_json_delta"},"index":0,"type":"content_block_delta"}"#,
        ),
        (
            "content_block_delta",
            r#"{"delta":{"partial_json":"{}","type":"input_json_delta"},"index":1,"type":"content_block_delta"}"#,
        ),
        (
            "content_block_start",
            r#"{"content_block":{"text":"","type":"text"},"index":2,"type":"content_block_start"}"#,
        ),
        (
            "content_block_delta",
            r#"{"delta":{"text":"x","type":"text_delta"},"index":2,"type":"content_block_delta"}"#,
        ),
        (
            "content_block_stop",
            r#"{"index":0,"type":"content_block_stop"}"#,
        ),
        (
            "content_block_stop",
            r#"{"index":1,"type":"content_block_stop"}"#,
        ),
        (
            "content_block_stop",
            r#"{"index":2,"type":"content_block_stop"}"#,
        ),
        (
            "message_delta",
            r#"{"delta":{"stop_reason":"end_turn","stop_sequence":null},"type":"message_delta","usage":{"output_tokens":0}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    assert_eq!(encode_all(&scenario_interleaved_tools()), expected);
}

#[test]
fn golden_foreign_reasoning_is_wrapped_in_a_pdk1_envelope() {
    let expected = sse_text(&[
        (
            "message_start",
            r#"{"message":{"content":[],"id":"msg_c","model":"claude-x","role":"assistant","stop_reason":null,"stop_sequence":null,"type":"message","usage":{"input_tokens":0,"output_tokens":0}},"type":"message_start"}"#,
        ),
        (
            "content_block_start",
            r#"{"content_block":{"data":"pdk1.gemini.blob","type":"redacted_thinking"},"index":0,"type":"content_block_start"}"#,
        ),
        (
            "content_block_stop",
            r#"{"index":0,"type":"content_block_stop"}"#,
        ),
        (
            "content_block_start",
            r#"{"content_block":{"thinking":"","type":"thinking"},"index":1,"type":"content_block_start"}"#,
        ),
        (
            "content_block_delta",
            r#"{"delta":{"signature":"pdk1.gemini.gsig","type":"signature_delta"},"index":1,"type":"content_block_delta"}"#,
        ),
        (
            "content_block_stop",
            r#"{"index":1,"type":"content_block_stop"}"#,
        ),
        (
            "message_delta",
            r#"{"delta":{"stop_reason":"end_turn","stop_sequence":null},"type":"message_delta","usage":{"output_tokens":0}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    assert_eq!(encode_all(&scenario_foreign_reasoning()), expected);
}

#[test]
fn golden_server_tools_are_replayed_only_for_an_anthropic_origin() {
    let own = sse_text(&[
        (
            "message_start",
            r#"{"message":{"content":[],"id":"msg_d","model":"claude-x","role":"assistant","stop_reason":null,"stop_sequence":null,"type":"message","usage":{"input_tokens":0,"output_tokens":0}},"type":"message_start"}"#,
        ),
        (
            "content_block_start",
            r#"{"content_block":{"id":"srv_1","input":{},"name":"web_search","type":"server_tool_use"},"index":0,"type":"content_block_start"}"#,
        ),
        (
            "content_block_delta",
            r#"{"delta":{"partial_json":"{\"query\":\"q\\n1\"}","type":"input_json_delta"},"index":0,"type":"content_block_delta"}"#,
        ),
        (
            "content_block_stop",
            r#"{"index":0,"type":"content_block_stop"}"#,
        ),
        (
            "content_block_start",
            r#"{"content_block":{"content":[{"title":"A","type":"web_search_result","url":"https://a.test"}],"tool_use_id":"srv_1","type":"web_search_tool_result"},"index":1,"type":"content_block_start"}"#,
        ),
        (
            "content_block_stop",
            r#"{"index":1,"type":"content_block_stop"}"#,
        ),
        (
            "message_delta",
            r#"{"delta":{"stop_reason":"end_turn","stop_sequence":null},"type":"message_delta","usage":{"output_tokens":0}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    assert_eq!(encode_all(&scenario_server_tools(OWN)), own);
    let foreign = sse_text(&[
        (
            "message_start",
            r#"{"message":{"content":[],"id":"msg_d","model":"claude-x","role":"assistant","stop_reason":null,"stop_sequence":null,"type":"message","usage":{"input_tokens":0,"output_tokens":0}},"type":"message_start"}"#,
        ),
        (
            "message_delta",
            r#"{"delta":{"stop_reason":"end_turn","stop_sequence":null},"type":"message_delta","usage":{"output_tokens":0}}"#,
        ),
        ("message_stop", r#"{"type":"message_stop"}"#),
    ]);
    assert_eq!(
        encode_all(&scenario_server_tools(Protocol::OpenaiResponses)),
        foreign
    );
}

#[test]
fn golden_an_error_ends_the_stream_without_a_message_stop() {
    let mut encoder = AnthropicMessages.stream_encoder();
    let mut bytes = Vec::new();
    for event in [
        start("msg_e"),
        HubEvent::TextDelta { text: "a".into() },
        HubEvent::Error {
            message: "boom".into(),
            kind: Some("overloaded_error".into()),
        },
    ] {
        bytes.extend(encoder.push(&event));
    }
    assert!(
        encoder
            .push(&HubEvent::TextDelta { text: "b".into() })
            .is_empty()
    );
    assert!(encoder.finish().is_empty());
    let expected = sse_text(&[
        (
            "message_start",
            r#"{"message":{"content":[],"id":"msg_e","model":"claude-x","role":"assistant","stop_reason":null,"stop_sequence":null,"type":"message","usage":{"input_tokens":0,"output_tokens":0}},"type":"message_start"}"#,
        ),
        (
            "content_block_start",
            r#"{"content_block":{"text":"","type":"text"},"index":0,"type":"content_block_start"}"#,
        ),
        (
            "content_block_delta",
            r#"{"delta":{"text":"a","type":"text_delta"},"index":0,"type":"content_block_delta"}"#,
        ),
        (
            "error",
            r#"{"error":{"message":"boom","type":"overloaded_error"},"type":"error"}"#,
        ),
    ]);
    assert_eq!(
        test_support::canonicalize_sse(&String::from_utf8(bytes).unwrap()),
        expected
    );
}

#[test]
fn golden_an_empty_stream_writes_nothing_and_a_lone_error_only_its_frame() {
    assert_eq!(encode_all(&[]), "");
    let expected = sse_text(&[(
        "error",
        r#"{"error":{"message":"boom","type":"api_error"},"type":"error"}"#,
    )]);
    assert_eq!(
        encode_all(&[HubEvent::Error {
            message: "boom".into(),
            kind: None,
        }]),
        expected
    );
}

fn upstream(frames: &[Value]) -> String {
    frames
        .iter()
        .map(|data| format!("data: {data}\n\n"))
        .collect()
}

fn message_start() -> Value {
    json!({"type": "message_start", "message": {"id": "m", "model": "claude-x"}})
}

fn server_tool_start(index: u64) -> Value {
    json!({"type": "content_block_start", "index": index,
           "content_block": {"type": "server_tool_use", "id": "srv", "name": "web_search"}})
}

fn input_delta(index: u64, partial: &str) -> Value {
    json!({"type": "content_block_delta", "index": index,
           "delta": {"type": "input_json_delta", "partial_json": partial}})
}

#[test]
fn server_tool_input_beyond_the_argument_cap_is_an_error() {
    let half = "x".repeat(MAX_TOOL_ARGUMENT_BYTES / 2 + 1);
    let raw = upstream(&[
        message_start(),
        server_tool_start(0),
        input_delta(0, &half),
        input_delta(0, &half),
    ]);
    let err = try_decode(&raw).unwrap_err();
    assert_eq!(
        err.message(),
        too_large("server tool input", MAX_TOOL_ARGUMENT_BYTES)
    );
}

#[test]
fn a_stream_ending_inside_a_server_tool_call_is_an_error() {
    let raw = upstream(&[message_start(), server_tool_start(0), input_delta(0, "{")]);
    assert_eq!(
        try_decode(&raw).unwrap_err().message(),
        "anthropic stream ended inside a server tool call"
    );
}

#[test]
fn invalid_server_tool_input_json_is_an_error() {
    let raw = upstream(&[
        message_start(),
        server_tool_start(0),
        input_delta(0, "{not json"),
        json!({"type": "content_block_stop", "index": 0}),
    ]);
    let message = try_decode(&raw).unwrap_err().message().to_owned();
    assert!(
        message.starts_with("invalid anthropic server tool input for `web_search`: "),
        "{message}"
    );
}

#[test]
fn an_unknown_content_block_type_is_an_error() {
    let raw = upstream(&[
        message_start(),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "hologram"}}),
    ]);
    assert_eq!(
        try_decode(&raw).unwrap_err().message(),
        "anthropic stream has an unsupported content block `hologram`"
    );
}

#[test]
fn citations_deltas_become_citation_events() {
    let raw = upstream(
        &[json!({"type": "content_block_delta", "index": 0, "delta": {
            "type": "citations_delta",
            "citation": {"type": "web_search_result_location", "url": "https://a.test",
                         "title": "A", "cited_text": "quote"},
        }})],
    );
    assert_eq!(
        try_decode(&raw).unwrap(),
        [HubEvent::Citation {
            citation: HubCitation {
                url: "https://a.test".into(),
                title: "A".into(),
                snippet: Some("quote".into()),
                start_index: None,
                end_index: None,
            },
        }]
    );
}

#[test]
fn a_missing_message_stop_is_synthesised_with_the_delta_stop_reason() {
    let raw = upstream(&[
        message_start(),
        json!({"type": "message_delta", "delta": {"stop_reason": "max_tokens"}}),
    ]);
    let events = try_decode(&raw).unwrap();
    assert_eq!(
        events.last(),
        Some(&HubEvent::Finish {
            reason: HubFinish::Length
        })
    );
    assert_eq!(
        events
            .iter()
            .filter(|e| matches!(e, HubEvent::Finish { .. }))
            .count(),
        1
    );
}

#[test]
fn chunked_signature_deltas_coalesce_per_thinking_block() {
    let raw = upstream(&[
        message_start(),
        json!({"type": "content_block_start", "index": 0, "content_block": {"type": "thinking", "thinking": ""}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "thinking_delta", "thinking": "step 1"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "sig_part_1"}}),
        json!({"type": "content_block_delta", "index": 0, "delta": {"type": "signature_delta", "signature": "_part_2"}}),
        json!({"type": "content_block_stop", "index": 0}),
        json!({"type": "content_block_start", "index": 1, "content_block": {"type": "text", "text": ""}}),
        json!({"type": "content_block_delta", "index": 1, "delta": {"type": "text_delta", "text": "done"}}),
        json!({"type": "content_block_stop", "index": 1}),
        json!({"type": "message_delta", "delta": {"stop_reason": "end_turn"}}),
        json!({"type": "message_stop"}),
    ]);
    let events = try_decode(&raw).unwrap();
    let signatures: Vec<&str> = events
        .iter()
        .filter_map(|e| match e {
            HubEvent::ReasoningSignature { signature } => Some(signature.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(signatures, ["sig_part_1_part_2"]);
    let response = collect_events(&events).unwrap();
    assert_eq!(
        response.content,
        [
            HubPart::Reasoning {
                text: "step 1".into(),
                signature: Some("sig_part_1_part_2".into()),
                encrypted: None,
                id: None,
                origin: Some(OWN),
            },
            HubPart::text("done"),
        ]
    );
}
