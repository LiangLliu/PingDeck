//! Streams: decoding upstream chunks, round trips, and the exact bytes of every frame toward a
//! Chat Completions client.

use super::*;
use crate::llm::hub::HubCitation;

/// Decodes `data:` chunks as one upstream stream.
fn decode_chunks(chunks: &[&str]) -> Vec<HubEvent> {
    let text: String = chunks.iter().map(|c| format!("data: {c}\n\n")).collect();
    test_support::decode_sse_text(&OpenaiChat, &text)
}

#[test]
fn stream_decoding_assembles_interleaved_tool_calls() {
    let out = decode_chunks(&[
        r#"{"id":"c","model":"m","choices":[{"delta":{"role":"assistant","reasoning_content":"hm","tool_calls":[{"index":0,"id":"a1","type":"function","function":{"name":"f","arguments":"{\"a\""}}]}}]}"#,
        r#"{"id":"c","model":"m","choices":[{"delta":{"content":"Hi","tool_calls":[{"index":1,"id":"b1","type":"function","function":{"name":"g","arguments":""}}]}}]}"#,
        r#"{"id":"c","model":"m","choices":[{"delta":{"tool_calls":[{"index":0,"function":{"arguments":":1}"}},{"index":1,"function":{"arguments":"{\"b\":2}"}}]}}]}"#,
        r#"{"id":"c","model":"m","choices":[{"delta":{},"finish_reason":"tool_calls"}],"usage":{"prompt_tokens":4,"completion_tokens":3}}"#,
        "[DONE]",
    ]);
    let collected = collect_events(&out).unwrap();
    assert_eq!(collected.text(), "Hi");
    assert_eq!(
        collected.tool_calls(),
        vec![("a1", "f", "{\"a\":1}"), ("b1", "g", "{\"b\":2}")]
    );
}

#[test]
fn stream_round_trip_preserves_events() {
    let round_trip = [
        HubEvent::Start {
            id: "chatcmpl-9".into(),
            model: "m".into(),
        },
        HubEvent::ReasoningDelta {
            text: "think".into(),
        },
        HubEvent::TextDelta { text: "42".into() },
        HubEvent::Usage {
            usage: HubUsage {
                input_tokens: Some(10),
                output_tokens: Some(5),
                cached_input_tokens: Some(3),
                reasoning_tokens: Some(1),
                ..HubUsage::default()
            },
        },
        HubEvent::Finish {
            reason: HubFinish::Stop,
        },
    ];
    let (_, decoded) = test_support::reencode(&OpenaiChat, &round_trip);
    assert_eq!(collect_events(&decoded), collect_events(&round_trip));
}

#[test]
fn stream_duplicate_fields_and_aliases_keep_content_tools_and_metadata() {
    let out = decode_chunks(&[
        r#"{"id":"old","id":"c","model":"old","model":"m","choices":[{"delta":{"reasoning_content":null,"reasoning":"plan","thinking":"ignored","content":"old","content":"\u4e2d","annotations":[{"type":"url_citation","url_citation":{"url":"https://x","title":"source","start_index":0,"end_index":1}}],"tool_calls":[{"index":3,"id":"old","id":"call","function":{"name":"old","name":"read","arguments":"old","arguments":"{\"city\":\"\u4e2d"}}]},"finish_reason":null}],"usage":{"prompt_tokens":1}}"#,
        r#"{"choices":[null,{"delta":{"content":"not the first choice"}}]}"#,
        r#"{"choices":[{"delta":{"reasoning_content":"","reasoning":"hidden","content":null,"tool_calls":[{"index":3,"function":{"arguments":"\"}"}},{"index":4,"function":{"name":"object_arg","arguments":{"n":7}}}]},"finish_reason":"tool_calls"}],"usage":null,"usage":{"prompt_tokens":8,"completion_tokens":2}}"#,
    ]);
    assert_eq!(
        out[0],
        HubEvent::Start {
            id: "c".into(),
            model: "m".into()
        }
    );
    let reasoning: Vec<_> = out
        .iter()
        .filter_map(|event| match event {
            HubEvent::ReasoningDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(reasoning, ["plan"]);
    assert!(out.iter().any(|event| matches!(
        event, HubEvent::Citation { citation }
            if citation.url == "https://x" && citation.start_index == Some(0)
    )));
    let response = collect_events(&out).unwrap();
    assert_eq!(response.text(), "中");
    assert_eq!(
        response.tool_calls(),
        [
            ("call", "read", "{\"city\":\"中\"}"),
            ("call_4", "object_arg", "{\"n\":7}"),
        ]
    );
    assert_eq!(response.usage.input_tokens, Some(8));
    assert_eq!(response.usage.output_tokens, Some(2));
    assert_eq!(response.finish, HubFinish::ToolCalls);
}

/// The encoded stream with every `"created":<seconds>` replaced by `"created":0`.
fn encode_events(events: &[HubEvent]) -> String {
    let text = test_support::encode_stream(&OpenaiChat, events);
    let mut out = String::with_capacity(text.len());
    let mut rest = text.as_str();
    while let Some(at) = rest.find("\"created\":") {
        let (head, tail) = rest.split_at(at + "\"created\":".len());
        out.push_str(head);
        out.push('0');
        rest = tail.trim_start_matches(|c: char| c.is_ascii_digit());
    }
    out.push_str(rest);
    out
}

fn cite(url: &str, title: &str, start: Option<u32>, end: u32) -> HubCitation {
    HubCitation {
        url: url.into(),
        title: title.into(),
        snippet: None,
        start_index: start,
        end_index: Some(end),
    }
}

#[test]
fn golden_stream_offsets_annotations_in_utf16_units_per_part() {
    let events = [
        HubEvent::Start {
            id: String::new(),
            model: "m".into(),
        },
        HubEvent::Origin {
            protocol: Protocol::Gemini,
        },
        HubEvent::ReasoningDelta { text: "hm".into() },
        HubEvent::TextDelta {
            text: "中😀".into(),
        },
        HubEvent::Citation {
            citation: cite("https://a", "T", Some(0), 1),
        },
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
        HubEvent::TextDelta { text: "ab".into() },
        HubEvent::Citation {
            citation: cite("https://b", "B", None, 2),
        },
        HubEvent::ServerToolStart {
            id: "s".into(),
            name: "web_search".into(),
            input: json!({}),
        },
        HubEvent::Citation {
            citation: cite("https://c", "T", Some(0), 0),
        },
        HubEvent::Usage {
            usage: HubUsage {
                input_tokens: Some(3),
                output_tokens: Some(2),
                reasoning_tokens: Some(1),
                cached_input_tokens: Some(1),
                ..HubUsage::default()
            },
        },
        HubEvent::Finish {
            reason: HubFinish::ToolCalls,
        },
    ];
    // "中😀" is three UTF-16 units: the part after the tool call starts at 3, the part after the
    // server tool at 5. Usage comes last in a chunk with no choices, then `[DONE]`.
    let expected = concat!(
        "data: ",
        r#"{"choices":[{"delta":{"content":"","role":"assistant"},"finish_reason":null,"index":0}],"created":0,"id":"chatcmpl-spanbay","model":"m","object":"chat.completion.chunk"}"#,
        "\n\n",
        "data: ",
        r#"{"choices":[{"delta":{"reasoning_content":"hm"},"finish_reason":null,"index":0}],"created":0,"id":"chatcmpl-spanbay","model":"m","object":"chat.completion.chunk"}"#,
        "\n\n",
        "data: ",
        r#"{"choices":[{"delta":{"content":"中😀"},"finish_reason":null,"index":0}],"created":0,"id":"chatcmpl-spanbay","model":"m","object":"chat.completion.chunk"}"#,
        "\n\n",
        "data: ",
        r#"{"choices":[{"delta":{"annotations":[{"type":"url_citation","url_citation":{"end_index":1,"start_index":0,"title":"T","url":"https://a"}}]},"finish_reason":null,"index":0}],"created":0,"id":"chatcmpl-spanbay","model":"m","object":"chat.completion.chunk"}"#,
        "\n\n",
        "data: ",
        r#"{"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"","name":"f"},"id":"c1","index":0,"type":"function"}]},"finish_reason":null,"index":0}],"created":0,"id":"chatcmpl-spanbay","model":"m","object":"chat.completion.chunk"}"#,
        "\n\n",
        "data: ",
        r#"{"choices":[{"delta":{"tool_calls":[{"function":{"arguments":"{}"},"index":0}]},"finish_reason":null,"index":0}],"created":0,"id":"chatcmpl-spanbay","model":"m","object":"chat.completion.chunk"}"#,
        "\n\n",
        "data: ",
        r#"{"choices":[{"delta":{"content":"ab"},"finish_reason":null,"index":0}],"created":0,"id":"chatcmpl-spanbay","model":"m","object":"chat.completion.chunk"}"#,
        "\n\n",
        "data: ",
        r#"{"choices":[{"delta":{"annotations":[{"type":"url_citation","url_citation":{"end_index":5,"start_index":3,"title":"B","url":"https://b"}}]},"finish_reason":null,"index":0}],"created":0,"id":"chatcmpl-spanbay","model":"m","object":"chat.completion.chunk"}"#,
        "\n\n",
        "data: ",
        r#"{"choices":[{"delta":{"annotations":[{"type":"url_citation","url_citation":{"end_index":5,"start_index":5,"title":"T","url":"https://c"}}]},"finish_reason":null,"index":0}],"created":0,"id":"chatcmpl-spanbay","model":"m","object":"chat.completion.chunk"}"#,
        "\n\n",
        "data: ",
        r#"{"choices":[{"delta":{},"finish_reason":"tool_calls","index":0}],"created":0,"id":"chatcmpl-spanbay","model":"m","object":"chat.completion.chunk"}"#,
        "\n\n",
        "data: ",
        r#"{"choices":[],"created":0,"id":"chatcmpl-spanbay","model":"m","object":"chat.completion.chunk","usage":{"completion_tokens":2,"completion_tokens_details":{"reasoning_tokens":1},"prompt_tokens":3,"prompt_tokens_details":{"cached_tokens":1},"total_tokens":5}}"#,
        "\n\n",
        "data: [DONE]\n\n",
    );
    assert_eq!(encode_events(&events), expected);
}

#[test]
fn golden_stream_opens_with_a_role_chunk_even_without_start() {
    let expected = concat!(
        "data: ",
        r#"{"choices":[{"delta":{"content":"","role":"assistant"},"finish_reason":null,"index":0}],"created":0,"id":"chatcmpl-spanbay","model":"","object":"chat.completion.chunk"}"#,
        "\n\n",
        "data: ",
        r#"{"choices":[{"delta":{"content":"x"},"finish_reason":null,"index":0}],"created":0,"id":"chatcmpl-spanbay","model":"","object":"chat.completion.chunk"}"#,
        "\n\n",
        "data: [DONE]\n\n",
    );
    assert_eq!(
        encode_events(&[HubEvent::TextDelta { text: "x".into() }]),
        expected
    );
}

#[test]
fn an_empty_stream_still_ends_with_done() {
    assert_eq!(encode_events(&[]), "data: [DONE]\n\n");
}

#[test]
fn golden_stream_error_frame() {
    let events = [
        HubEvent::Start {
            id: "i".into(),
            model: "m".into(),
        },
        HubEvent::Usage {
            usage: HubUsage {
                input_tokens: Some(3),
                ..HubUsage::default()
            },
        },
        HubEvent::Error {
            message: "boom".into(),
            kind: None,
        },
        HubEvent::TextDelta {
            text: "late".into(),
        },
        HubEvent::Finish {
            reason: HubFinish::Stop,
        },
    ];
    // The error frame ends the stream; the late delta,
    // the finish, the usage chunk and `[DONE]` are no longer written.
    let expected = concat!(
        "data: ",
        r#"{"choices":[{"delta":{"content":"","role":"assistant"},"finish_reason":null,"index":0}],"created":0,"id":"i","model":"m","object":"chat.completion.chunk"}"#,
        "\n\n",
        "data: ",
        r#"{"error":{"message":"boom","type":"upstream_error"}}"#,
        "\n\n",
    );
    assert_eq!(encode_events(&events), expected);
}

#[test]
fn an_error_ends_the_stream_without_usage_or_done() {
    // Like the Anthropic, Responses and Gemini encoders,
    // nothing follows the error frame, not even the closing usage chunk or `[DONE]`.
    let mut encoder = OpenaiChat.stream_encoder();
    encoder.push(&HubEvent::Usage {
        usage: HubUsage {
            input_tokens: Some(3),
            ..HubUsage::default()
        },
    });
    let error = encoder.push(&HubEvent::Error {
        message: "boom".into(),
        kind: Some("overloaded".into()),
    });
    assert_eq!(
        String::from_utf8(error).unwrap(),
        "data: {\"error\":{\"message\":\"boom\",\"type\":\"overloaded\"}}\n\n"
    );
    assert!(
        encoder
            .push(&HubEvent::TextDelta {
                text: "late".into()
            })
            .is_empty()
    );
    assert!(
        encoder
            .push(&HubEvent::Finish {
                reason: HubFinish::Stop
            })
            .is_empty()
    );
    assert!(encoder.finish().is_empty());
}
