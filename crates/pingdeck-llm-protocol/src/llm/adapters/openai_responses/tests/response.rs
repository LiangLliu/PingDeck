//! Whole responses: hub responses encoded for a client and upstream replies decoded.

use super::*;

fn response(content: Vec<HubPart>, finish: HubFinish) -> HubResponse {
    HubResponse {
        id: "r".into(),
        model: "m".into(),
        content,
        finish,
        usage: HubUsage::default(),
    }
}

/// The encoded response without its clock-dependent `created_at`.
fn encode_response(response: &HubResponse) -> Value {
    let mut wire = OpenaiResponses.encode_response(response);
    wire.as_object_mut().unwrap().remove("created_at");
    wire
}

fn text_with(text: &str, citations: Vec<HubCitation>) -> HubPart {
    HubPart::Text {
        text: text.into(),
        signature: None,
        origin: None,
        citations,
    }
}

// --- Encoding for a client ---

#[test]
fn encode_decode_response_round_trip() {
    let usage = HubUsage {
        input_tokens: Some(12),
        output_tokens: Some(8),
        cached_input_tokens: Some(3),
        cache_write_tokens: None,
        cache_write_long_tokens: None,
        reasoning_tokens: Some(2),
    };
    let response = HubResponse {
        id: "resp_5".into(),
        model: "gpt".into(),
        content: vec![
            HubPart::Reasoning {
                text: "why".into(),
                signature: None,
                origin: Some(Protocol::OpenaiResponses),
                encrypted: Some("enc".into()),
                id: Some("rs_a".into()),
            },
            HubPart::text("answer"),
            tool_call("c1", "f", "{\"a\":1}"),
        ],
        finish: HubFinish::ToolCalls,
        usage,
    };
    let wire = OpenaiResponses.encode_response(&response);
    assert_eq!(OpenaiResponses.decode_response(&wire).unwrap(), response);
}

#[test]
fn golden_encode_response() {
    // Citation offsets count UTF-16 units: "a😀b" shifts the next part by 4 (it was 3, the
    // char count). Foreign reasoning keeps its text
    // under a synthesized id and loses its encrypted payload, or is dropped when it has no text; a
    // foreign web search is dropped.
    let reasoning = |text: &str, id: &str, origin: Protocol| HubPart::Reasoning {
        text: text.into(),
        signature: None,
        encrypted: Some("enc".into()),
        id: Some(id.into()),
        origin: Some(origin),
    };
    let response = HubResponse {
        id: "resp_1".into(),
        model: "gpt".into(),
        content: vec![
            text_with("a😀b", vec![citation("https://a", Some(0), Some(1))]),
            text_with(
                "cd",
                vec![
                    citation("https://b", Some(0), Some(1)),
                    citation("https://c", None, None),
                ],
            ),
            reasoning("own", "rs_up", Protocol::OpenaiResponses),
            reasoning("foreign", "x", Protocol::AnthropicMessages),
            reasoning("", "x", Protocol::AnthropicMessages),
            tool_call("c1", "f", "{}"),
            HubPart::ServerToolUse {
                id: "ws_1".into(),
                name: "web_search".into(),
                input: json!({"query": "q"}),
                origin: Some(Protocol::OpenaiResponses),
            },
            HubPart::ServerToolUse {
                id: "ws_2".into(),
                name: "web_search".into(),
                input: json!({}),
                origin: Some(Protocol::AnthropicMessages),
            },
            HubPart::text("tail"),
        ],
        finish: HubFinish::Stop,
        usage: HubUsage {
            input_tokens: Some(3),
            output_tokens: Some(4),
            cached_input_tokens: Some(1),
            reasoning_tokens: Some(2),
            ..HubUsage::default()
        },
    };
    assert_eq!(
        test_support::sort_json_keys(encode_response(&response)).to_string(),
        r#"{"error":null,"id":"resp_1","incomplete_details":null,"model":"gpt","object":"response","output":[{"content":[{"annotations":[{"end_index":1,"start_index":0,"title":"T","type":"url_citation","url":"https://a"},{"end_index":5,"start_index":4,"title":"T","type":"url_citation","url":"https://b"},{"end_index":6,"start_index":4,"title":"T","type":"url_citation","url":"https://c"}],"text":"a😀bcd","type":"output_text"}],"id":"msg_0","role":"assistant","status":"completed","type":"message"},{"encrypted_content":"enc","id":"rs_up","summary":[{"text":"own","type":"summary_text"}],"type":"reasoning"},{"id":"rs_2","summary":[{"text":"foreign","type":"summary_text"}],"type":"reasoning"},{"arguments":"{}","call_id":"c1","id":"fc_3","name":"f","status":"completed","type":"function_call"},{"action":{"query":"q"},"id":"ws_1","status":"completed","type":"web_search_call"},{"content":[{"annotations":[],"text":"tail","type":"output_text"}],"id":"msg_5","role":"assistant","status":"completed","type":"message"}],"status":"completed","usage":{"input_tokens":3,"input_tokens_details":{"cached_tokens":1},"output_tokens":4,"output_tokens_details":{"reasoning_tokens":2},"total_tokens":7}}"#
    );
}

#[test]
fn every_finish_maps_to_a_status() {
    for (finish, status, details) in [
        (HubFinish::Stop, "completed", Value::Null),
        (HubFinish::ToolCalls, "completed", Value::Null),
        (HubFinish::Other("x".into()), "completed", Value::Null),
        (
            HubFinish::Length,
            "incomplete",
            json!({"reason": "max_output_tokens"}),
        ),
        (
            HubFinish::Pause,
            "incomplete",
            json!({"reason": "max_output_tokens"}),
        ),
        (
            HubFinish::ContentFilter,
            "incomplete",
            json!({"reason": "content_filter"}),
        ),
    ] {
        let wire = encode_response(&response(Vec::new(), finish.clone()));
        assert_eq!(wire["status"], status, "{finish:?}");
        assert_eq!(wire["incomplete_details"], details, "{finish:?}");
    }
}

#[test]
fn a_dropped_foreign_part_still_splits_the_message() {
    let wire = encode_response(&response(
        vec![
            HubPart::text("a"),
            HubPart::ServerToolUse {
                id: "ws".into(),
                name: "web_search".into(),
                input: json!({}),
                origin: Some(Protocol::Gemini),
            },
            HubPart::text("b"),
        ],
        HubFinish::Stop,
    ));
    let message = |id: &str, text: &str| {
        json!({"content": [{"annotations": [], "text": text, "type": "output_text"}],
               "id": id, "role": "assistant", "status": "completed", "type": "message"})
    };
    assert_eq!(
        wire["output"],
        json!([message("msg_0", "a"), message("msg_1", "b")])
    );
}

// --- Decoding an upstream reply ---

#[test]
fn incomplete_max_output_tokens_is_length() {
    let length = json!({"id": "r", "status": "incomplete", "incomplete_details": {"reason": "max_output_tokens"}, "output": []});
    assert_eq!(
        OpenaiResponses.decode_response(&length).unwrap().finish,
        HubFinish::Length
    );
}

#[test]
fn failed_status_surfaces_error_message() {
    let failed = json!({"id": "r", "status": "failed", "error": {"code": "server_error", "message": "boom"}, "output": []});
    assert_eq!(
        OpenaiResponses
            .decode_response(&failed)
            .unwrap_err()
            .message(),
        "boom"
    );
    for (body, message) in [
        (json!({"error": "plain", "output": []}), "plain"),
        (
            json!({"status": "failed", "output": []}),
            "the response failed",
        ),
        (json!({"error": {"code": "x"}}), "the response failed"),
    ] {
        let err = OpenaiResponses.decode_response(&body).unwrap_err();
        assert!(matches!(err, AppError::Network { .. }), "{err:?}");
        assert_eq!(err.message(), message);
    }
}

#[test]
fn decodes_search_response() {
    let search_resp = json!({
        "id": "resp_1", "model": "gpt-5", "status": "completed",
        "output": [
            {"type": "web_search_call", "id": "ws_1", "status": "completed", "action": {"type": "search", "query": "rust"}},
            {"type": "message", "id": "msg_1", "role": "assistant", "content": [
                {"type": "output_text", "text": "Rust 1.99 is out.", "annotations": [
                    {"type": "url_citation", "start_index": 0, "end_index": 8, "url": "https://r.org", "title": "Rust"}
                ]}
            ]}
        ],
    });
    let decoded = OpenaiResponses.decode_response(&search_resp).unwrap();
    assert_eq!(decoded.content.len(), 2);
    assert_eq!(
        decoded.content,
        vec![
            HubPart::ServerToolUse {
                id: "ws_1".into(),
                name: "web_search".into(),
                input: json!({"type": "search", "query": "rust"}),
                origin: Some(Protocol::OpenaiResponses),
            },
            text_with(
                "Rust 1.99 is out.",
                vec![HubCitation {
                    url: "https://r.org".into(),
                    title: "Rust".into(),
                    snippet: None,
                    start_index: Some(0),
                    end_index: Some(8),
                }],
            ),
        ]
    );
    assert_eq!(decoded.finish, HubFinish::Stop);
}

#[test]
fn message_parts_join_and_shift_their_citations() {
    // The shift counts UTF-16 units: "a😀" moves the citation by 3 (it was 2, the
    // char count).
    let body = json!({"output": [
        {"type": "message", "content": [
            {"type": "output_text", "text": "a😀"},
            {"type": "input_text", "text": "skipped"},
            {"type": "refusal", "refusal": "no", "annotations": [
                {"type": "url_citation", "url": "u", "title": "t", "start_index": 0, "end_index": 1},
                {"type": "file_citation"},
            ]},
        ]},
        {"type": "message", "content": "plain"},
        {"type": "message", "content": ""},
    ]});
    let decoded = OpenaiResponses.decode_response(&body).unwrap();
    assert_eq!(
        decoded.content,
        vec![
            text_with(
                "a😀no",
                vec![HubCitation {
                    url: "u".into(),
                    title: "t".into(),
                    snippet: None,
                    start_index: Some(3),
                    end_index: Some(4),
                }],
            ),
            HubPart::text("plain"),
        ]
    );
    assert_eq!(decoded.finish, HubFinish::Stop);
}

#[test]
fn reasoning_text_prefers_the_summary() {
    let body = json!({"output": [
        {"type": "reasoning", "summary": [{"text": "s1"}, {"text": "s2"}], "content": [{"text": "c"}]},
        {"type": "reasoning", "summary": [], "content": [{"text": "c1"}, {"text": "c2"}]},
    ]});
    let texts: Vec<String> = OpenaiResponses
        .decode_response(&body)
        .unwrap()
        .content
        .into_iter()
        .map(|part| match part {
            HubPart::Reasoning {
                text, origin, id, ..
            } => {
                assert_eq!((origin, id), (None, None));
                text
            }
            other => panic!("{other:?}"),
        })
        .collect();
    assert_eq!(texts, ["s1\n\ns2", "c1\n\nc2"]);
}

#[test]
fn unknown_output_items_fail_the_whole_reply() {
    for (item, kind) in [
        (json!({"type": "mcp_call"}), "mcp_call"),
        (json!({}), "<missing>"),
    ] {
        let body = json!({"output": [{"type": "message", "content": "kept?"}, item]});
        let err = OpenaiResponses.decode_response(&body).unwrap_err();
        assert!(matches!(err, AppError::Network { .. }), "{err:?}");
        assert_eq!(
            err.message(),
            format!("Responses output item type `{kind}` is not supported")
        );
    }
    let err = OpenaiResponses.decode_response(&json!({})).unwrap_err();
    assert_eq!(err.message(), "Responses reply has no `output` array");
}

#[test]
fn citation_offsets_count_utf16_units() {
    // Offsets are UTF-16 code units, as the hub documents and
    // as the Chat Completions codec counts them. "a😀b" is 4 units (3 chars).
    let wire = encode_response(&response(
        vec![
            HubPart::text("a😀b"),
            text_with(
                "cd",
                vec![
                    citation("https://c", Some(0), Some(1)),
                    citation("https://d", None, None),
                ],
            ),
        ],
        HubFinish::Stop,
    ));
    let annotations = &wire["output"][0]["content"][0]["annotations"];
    assert_eq!(annotations[0]["start_index"], 4);
    assert_eq!(annotations[0]["end_index"], 5);
    assert_eq!(annotations[1]["start_index"], 4);
    assert_eq!(annotations[1]["end_index"], 6);

    let body = json!({"output": [{"type": "message", "content": [
        {"type": "output_text", "text": "a😀b"},
        {"type": "output_text", "text": "cd", "annotations": [
            {"type": "url_citation", "url": "u", "title": "t", "start_index": 0, "end_index": 1},
        ]},
    ]}]});
    let decoded = OpenaiResponses.decode_response(&body).unwrap();
    let HubPart::Text { citations, .. } = &decoded.content[0] else {
        panic!("{:?}", decoded.content);
    };
    assert_eq!(
        (citations[0].start_index, citations[0].end_index),
        (Some(4), Some(5))
    );

    let stream = test_support::encode_stream(
        &OpenaiResponses,
        &[
            HubEvent::TextDelta {
                text: "a😀".into()
            },
            HubEvent::Citation {
                citation: citation("https://a", None, None),
            },
            HubEvent::TextDelta {
                text: "😀".into()
            },
            HubEvent::Citation {
                citation: citation("https://b", None, None),
            },
        ],
    );
    assert!(
        stream.contains(r#""annotation":{"end_index":3,"start_index":0"#),
        "{stream}"
    );
    assert!(
        stream.contains(r#""annotation":{"end_index":5,"start_index":0"#),
        "{stream}"
    );
}
