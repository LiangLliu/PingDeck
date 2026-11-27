//! Streams: round trips, and the exact bytes of every frame toward a Gemini client.

use super::*;

fn encode_events(events: &[HubEvent]) -> String {
    test_support::encode_stream(&GeminiGenerate, events)
}

#[test]
fn stream_round_trip_preserves_events() {
    let events = [
        HubEvent::Start {
            id: "r1".into(),
            model: "gemini-x".into(),
        },
        HubEvent::Origin {
            protocol: Protocol::Gemini,
        },
        HubEvent::TextDelta {
            text: "calling".into(),
        },
        HubEvent::ToolCallStart {
            index: 0,
            id: "call_0".into(),
            name: "a".into(),
            signature: Some("s".into()),
        },
        HubEvent::ToolCallDelta {
            index: 0,
            arguments: "{\"x\":1}".into(),
        },
        HubEvent::Finish {
            reason: HubFinish::ToolCalls,
        },
        HubEvent::Usage {
            usage: HubUsage {
                input_tokens: Some(10),
                output_tokens: Some(7),
                cached_input_tokens: Some(2),
                cache_write_tokens: None,
                cache_write_long_tokens: None,
                reasoning_tokens: Some(3),
            },
        },
    ];
    let (_, decoded) = test_support::reencode(&GeminiGenerate, &events);
    assert_eq!(
        collect_events(&decoded).unwrap(),
        collect_events(&events).unwrap()
    );
}

#[test]
fn golden_stream_buffers_calls_wraps_foreign_signatures_and_closes_with_grounding() {
    let events = [
        HubEvent::Start {
            id: "r1".into(),
            model: "gemini-x".into(),
        },
        HubEvent::Origin {
            protocol: Protocol::AnthropicMessages,
        },
        HubEvent::ReasoningDelta {
            text: "think".into(),
        },
        HubEvent::ReasoningSignature {
            signature: "RS".into(),
        },
        HubEvent::TextDelta { text: "Hi".into() },
        HubEvent::ToolCallStart {
            index: 1,
            id: "b".into(),
            name: "g".into(),
            signature: None,
        },
        HubEvent::ToolCallDelta {
            index: 1,
            arguments: "{\"y\":".into(),
        },
        HubEvent::ToolCallStart {
            index: 0,
            id: "a".into(),
            name: "f".into(),
            signature: Some("CS".into()),
        },
        HubEvent::ToolCallDelta {
            index: 0,
            arguments: "{\"x\":1}".into(),
        },
        HubEvent::ToolCallDelta {
            index: 1,
            arguments: "2}".into(),
        },
        HubEvent::Citation {
            citation: HubCitation {
                url: "https://a.example".into(),
                title: "A".into(),
                snippet: Some("Hi".into()),
                start_index: Some(0),
                end_index: Some(2),
            },
        },
        HubEvent::ServerToolStart {
            id: "s1".into(),
            name: "web_search".into(),
            input: json!({"query": "hi"}),
        },
        HubEvent::TextDelta {
            text: "after".into(),
        },
        HubEvent::Finish {
            reason: HubFinish::ToolCalls,
        },
        HubEvent::Usage {
            usage: HubUsage {
                input_tokens: Some(10),
                output_tokens: Some(7),
                cached_input_tokens: Some(2),
                reasoning_tokens: Some(3),
                ..HubUsage::default()
            },
        },
    ];
    // Calls wait for the next event that writes (not the citation or the search), then flush
    // sorted by index; the closing chunk carries grounding and usage.
    let expected = concat!(
        "data: ",
        r#"{"candidates":[{"content":{"parts":[{"text":"think","thought":true}],"role":"model"},"index":0}],"modelVersion":"gemini-x","responseId":"r1"}"#,
        "\n\n",
        "data: ",
        r#"{"candidates":[{"content":{"parts":[{"text":"","thought":true,"thoughtSignature":"pdk1.anthropic_messages.RS"}],"role":"model"},"index":0}],"modelVersion":"gemini-x","responseId":"r1"}"#,
        "\n\n",
        "data: ",
        r#"{"candidates":[{"content":{"parts":[{"text":"Hi"}],"role":"model"},"index":0}],"modelVersion":"gemini-x","responseId":"r1"}"#,
        "\n\n",
        "data: ",
        r#"{"candidates":[{"content":{"parts":[{"functionCall":{"args":{"x":1},"id":"a","name":"f"},"thoughtSignature":"pdk1.anthropic_messages.CS"},{"functionCall":{"args":{"y":2},"id":"b","name":"g"}}],"role":"model"},"index":0}],"modelVersion":"gemini-x","responseId":"r1"}"#,
        "\n\n",
        "data: ",
        r#"{"candidates":[{"content":{"parts":[{"text":"after"}],"role":"model"},"index":0}],"modelVersion":"gemini-x","responseId":"r1"}"#,
        "\n\n",
        "data: ",
        r#"{"candidates":[{"content":{"parts":[{"text":""}],"role":"model"},"finishReason":"STOP","groundingMetadata":{"groundingChunks":[{"web":{"title":"A","uri":"https://a.example"}}],"groundingSupports":[{"groundingChunkIndices":[0],"segment":{"endIndex":2,"startIndex":0,"text":"Hi"}}],"webSearchQueries":["hi"]},"index":0}],"modelVersion":"gemini-x","responseId":"r1","usageMetadata":{"cachedContentTokenCount":2,"candidatesTokenCount":4,"promptTokenCount":10,"thoughtsTokenCount":3,"totalTokenCount":17}}"#,
        "\n\n",
    );
    assert_eq!(encode_events(&events), expected);
}

#[test]
fn golden_stream_flushes_calls_before_signatures_and_at_finish() {
    let events = [
        HubEvent::Origin {
            protocol: Protocol::Gemini,
        },
        HubEvent::ToolCallStart {
            index: 0,
            id: String::new(),
            name: "f".into(),
            signature: Some("G".into()),
        },
        HubEvent::ToolCallDelta {
            index: 0,
            arguments: String::new(),
        },
        HubEvent::TextSignature {
            signature: "TS".into(),
        },
        HubEvent::ToolCallStart {
            index: 2,
            id: "c".into(),
            name: "h".into(),
            signature: None,
        },
        HubEvent::ToolCallDelta {
            index: 2,
            arguments: "nope".into(),
        },
        HubEvent::Finish {
            reason: HubFinish::Length,
        },
    ];
    // Gemini's own signatures pass unwrapped; an empty id is left out; no start means no
    // modelVersion or responseId.
    let expected = concat!(
        "data: ",
        r#"{"candidates":[{"content":{"parts":[{"functionCall":{"args":{},"name":"f"},"thoughtSignature":"G"}],"role":"model"},"index":0}]}"#,
        "\n\n",
        "data: ",
        r#"{"candidates":[{"content":{"parts":[{"text":"","thoughtSignature":"TS"}],"role":"model"},"index":0}]}"#,
        "\n\n",
        "data: ",
        r#"{"candidates":[{"content":{"parts":[{"functionCall":{"args":{"_raw":"nope"},"id":"c","name":"h"}}],"role":"model"},"index":0}]}"#,
        "\n\n",
        "data: ",
        r#"{"candidates":[{"content":{"parts":[{"text":""}],"role":"model"},"finishReason":"MAX_TOKENS","index":0}]}"#,
        "\n\n",
    );
    assert_eq!(encode_events(&events), expected);
}

#[test]
fn oversized_call_arguments_end_the_stream_with_one_error_frame() {
    let mut encoder = GeminiGenerate.stream_encoder();
    let mut bytes = Vec::new();
    for event in [
        HubEvent::Start {
            id: "r".into(),
            model: "m".into(),
        },
        HubEvent::ToolCallStart {
            index: 0,
            id: "a".into(),
            name: "f".into(),
            signature: None,
        },
        HubEvent::ToolCallDelta {
            index: 0,
            arguments: "{".into(),
        },
        HubEvent::ToolCallDelta {
            index: 0,
            arguments: "x".repeat(MAX_TOOL_ARGUMENT_BYTES),
        },
    ] {
        bytes.extend(encoder.push(&event));
    }
    let expected = concat!(
        "data: ",
        r#"{"error":{"code":500,"message":"the upstream stream's tool-call arguments exceed the 4 MiB limit","status":"INTERNAL"}}"#,
        "\n\n",
    );
    assert_eq!(String::from_utf8(bytes).unwrap(), expected);
    assert!(
        encoder
            .push(&HubEvent::TextDelta {
                text: "late".into()
            })
            .is_empty()
    );
    assert!(encoder.finish().is_empty());
}

#[test]
fn an_error_ends_the_stream_and_finish_adds_nothing() {
    let events = [
        HubEvent::TextDelta { text: "a".into() },
        HubEvent::Error {
            message: "boom".into(),
            kind: Some("overloaded".into()),
        },
        HubEvent::TextDelta {
            text: "late".into(),
        },
    ];
    let expected = concat!(
        "data: ",
        r#"{"candidates":[{"content":{"parts":[{"text":"a"}],"role":"model"},"index":0}]}"#,
        "\n\n",
        "data: ",
        r#"{"error":{"code":500,"message":"boom","status":"INTERNAL"}}"#,
        "\n\n",
    );
    assert_eq!(encode_events(&events), expected);
}

#[test]
fn an_untouched_encoder_writes_nothing() {
    assert_eq!(encode_events(&[]), "");
}
