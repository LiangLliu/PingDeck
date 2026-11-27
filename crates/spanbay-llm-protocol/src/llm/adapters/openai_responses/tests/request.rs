//! Requests: hub requests encoded toward an upstream, and client request bodies decoded into hub
//! requests.

use super::*;

fn document(source: HubDocumentSource, name: Option<&str>, media_type: &str) -> HubPart {
    HubPart::Document {
        document: HubDocument {
            media_type: media_type.into(),
            name: name.map(Into::into),
            source,
        },
    }
}

fn user_prompt() -> HubRequest {
    HubRequest::user_prompt("m", "hi")
}

/// The `input` of [`user_prompt`].
fn hi_input() -> Value {
    json!([{"content": [{"text": "hi", "type": "input_text"}], "role": "user", "type": "message"}])
}

// --- Encoding toward an upstream ---

#[test]
fn encodes_request_regroups_tool_round_trip_options() {
    let req = HubRequest {
        model: "gpt-5".into(),
        system: Some("be brief".into()),
        messages: vec![
            HubMessage {
                role: HubRole::User,
                content: vec![
                    HubPart::text("look"),
                    HubPart::Image {
                        image: HubImage::Base64 {
                            media_type: "image/png".into(),
                            data: "AAA".into(),
                        },
                    },
                ],
            },
            HubMessage {
                role: HubRole::Assistant,
                content: vec![
                    HubPart::Reasoning {
                        text: "hm".into(),
                        signature: None,
                        origin: None,
                        encrypted: Some("enc".into()),
                        id: Some("rs_1".into()),
                    },
                    HubPart::text("calling"),
                    tool_call("c1", "f", "{\"a\":1}"),
                ],
            },
            HubMessage {
                role: HubRole::Tool,
                content: vec![HubPart::ToolResult {
                    call_id: "c1".into(),
                    name: None,
                    content: vec![HubPart::text("ok")],
                    is_error: false,
                }],
            },
        ],
        temperature: Some(0.5),
        max_tokens: Some(64),
        tools: vec![HubTool {
            name: "f".into(),
            description: Some("d".into()),
            parameters: json!({"type": "object"}),
            strict: Some(true),
        }],
        tool_choice: Some(HubToolChoice::Tool { name: "f".into() }),
        response_format: Some(HubResponseFormat::JsonSchema {
            name: "out".into(),
            schema: json!({"type": "object"}),
            strict: Some(true),
        }),
        reasoning: Some(HubReasoning {
            effort: Some(HubEffort::Max),
            ..HubReasoning::default()
        }),
        ..HubRequest::default()
    };
    let body = encode(&req, &Quirks::default());
    assert_eq!(body["store"], false);
    assert_eq!(body["instructions"], "be brief");
    assert_eq!(body["max_output_tokens"], 64);
    assert_eq!(
        body["reasoning"],
        json!({"effort": "max", "summary": "auto"})
    );
    assert_eq!(body["include"], json!(["reasoning.encrypted_content"]));
    assert_eq!(body["input"].as_array().unwrap().len(), 5);
    assert_eq!(
        body,
        json!({
            "include": ["reasoning.encrypted_content"],
            "input": [
                {"content": [
                    {"text": "look", "type": "input_text"},
                    {"image_url": "data:image/png;base64,AAA", "type": "input_image"},
                ], "role": "user", "type": "message"},
                {"encrypted_content": "enc", "id": "rs_1",
                 "summary": [{"text": "hm", "type": "summary_text"}], "type": "reasoning"},
                {"content": [{"text": "calling", "type": "output_text"}],
                 "role": "assistant", "type": "message"},
                {"arguments": "{\"a\":1}", "call_id": "c1", "name": "f", "type": "function_call"},
                {"call_id": "c1", "output": "ok", "type": "function_call_output"},
            ],
            "instructions": "be brief",
            "max_output_tokens": 64,
            "model": "gpt-5",
            "reasoning": {"effort": "max", "summary": "auto"},
            "store": false,
            "temperature": 0.5,
            "text": {"format": {"name": "out", "schema": {"type": "object"}, "strict": true,
                                "type": "json_schema"}},
            "tool_choice": {"name": "f", "type": "function"},
            "tools": [{"description": "d", "name": "f", "parameters": {"type": "object"},
                       "strict": true, "type": "function"}],
        })
    );
}

#[test]
fn rejects_assistant_prefill() {
    let prefill = HubRequest {
        model: "m".into(),
        messages: vec![
            HubMessage::user("hi"),
            HubMessage::assistant("The answer is"),
        ],
        ..HubRequest::default()
    };
    let err = try_encode(&prefill, &Quirks::default()).unwrap_err();
    assert!(matches!(err, AppError::Validation { .. }), "{err:?}");
    assert_eq!(
        err.message(),
        "the Responses API does not take an assistant prefill; end the conversation with a user \
         or tool message"
    );
}

#[test]
fn a_tool_without_strict_is_sent_as_not_strict() {
    // Responses reads a missing `strict` as true and then rejects loose schemas.
    let req = HubRequest {
        model: "gpt-5".into(),
        messages: vec![HubMessage::user("hi")],
        tools: vec![HubTool {
            name: "read".into(),
            description: None,
            parameters: json!({"type": "object"}),
            strict: None,
        }],
        ..HubRequest::default()
    };
    assert_eq!(
        encode(&req, &Quirks::default())["tools"][0]["strict"],
        false
    );
}

#[test]
fn the_cache_key_is_sent_as_prompt_cache_key() {
    use crate::llm::hub::{CacheBreakpoint, CacheHint};
    let req = HubRequest {
        cache: Some(CacheHint {
            breakpoints: vec![
                CacheBreakpoint::SystemAndTools,
                CacheBreakpoint::LastMessage,
            ],
            key: Some("session-1".into()),
        }),
        ..HubRequest::user_prompt("gpt-x", "hi")
    };
    assert_eq!(
        encode(&req, &Quirks::default())["prompt_cache_key"],
        "session-1"
    );
}

#[test]
fn encodes_documents_and_web_search_tool() {
    let doc_req = HubRequest {
        model: "gpt-5".into(),
        messages: vec![HubMessage {
            role: HubRole::User,
            content: vec![
                document(
                    HubDocumentSource::Base64 { data: "AAA".into() },
                    Some("a.pdf"),
                    "application/pdf",
                ),
                document(
                    HubDocumentSource::Url {
                        url: "https://x/y.pdf".into(),
                    },
                    None,
                    "application/pdf",
                ),
            ],
        }],
        native_tools: vec![HubNativeTool::WebSearch {
            max_uses: Some(3),
            allowed_domains: vec!["a.com".into()],
            blocked_domains: Vec::new(),
        }],
        ..HubRequest::default()
    };
    let body = encode(&doc_req, &Quirks::default());
    assert_eq!(body["input"][0]["content"].as_array().unwrap().len(), 2);
    assert_eq!(
        body["input"][0]["content"],
        json!([
            {"file_data": "data:application/pdf;base64,AAA", "filename": "a.pdf",
             "type": "input_file"},
            {"file_url": "https://x/y.pdf", "type": "input_file"},
        ])
    );
    assert_eq!(
        body["tools"],
        json!([{"type": "web_search", "filters": {"allowed_domains": ["a.com"]}}])
    );
    assert_eq!(body["max_tool_calls"], 3);
}

#[test]
fn text_documents_tool_outputs_and_foreign_parts_encode_as_expected() {
    // The text document block escapes only `"` in the name and nothing in the type (a known gap,
    // pinned as it is).
    let req = HubRequest {
        messages: vec![
            HubMessage {
                role: HubRole::User,
                content: vec![
                    document(
                        HubDocumentSource::Text {
                            text: "body".into(),
                        },
                        Some("a\"b&c.txt"),
                        "text/plain",
                    ),
                    document(
                        HubDocumentSource::Text { text: "t".into() },
                        None,
                        "text/<x>",
                    ),
                    document(
                        HubDocumentSource::Base64 { data: "AA".into() },
                        None,
                        "text/csv",
                    ),
                    document(
                        HubDocumentSource::Base64 { data: "AA".into() },
                        None,
                        "image/x",
                    ),
                ],
            },
            HubMessage {
                role: HubRole::Assistant,
                content: vec![
                    HubPart::Reasoning {
                        text: "f".into(),
                        signature: None,
                        encrypted: Some("e".into()),
                        id: Some("x".into()),
                        origin: Some(Protocol::AnthropicMessages),
                    },
                    HubPart::ServerToolUse {
                        id: "s".into(),
                        name: "web_search".into(),
                        input: json!({}),
                        origin: Some(Protocol::AnthropicMessages),
                    },
                    HubPart::text("ok"),
                    tool_call("c1", "f", "{}"),
                    tool_call("c2", "g", "{}"),
                ],
            },
            HubMessage {
                role: HubRole::Tool,
                content: vec![
                    HubPart::ToolResult {
                        call_id: "c1".into(),
                        name: None,
                        content: vec![
                            HubPart::text("one"),
                            document(
                                HubDocumentSource::Url { url: "u".into() },
                                None,
                                "application/pdf",
                            ),
                            HubPart::text("two"),
                        ],
                        is_error: false,
                    },
                    HubPart::ToolResult {
                        call_id: "c2".into(),
                        name: None,
                        content: vec![
                            HubPart::text("see"),
                            HubPart::Image {
                                image: HubImage::Url {
                                    url: "https://i".into(),
                                },
                            },
                        ],
                        is_error: false,
                    },
                ],
            },
        ],
        ..HubRequest::user_prompt("m", "")
    };
    assert_eq!(
        test_support::sort_json_keys(encode(&req, &Quirks::default())).to_string(),
        r#"{"input":[{"content":[{"text":"<document name=\"a&quot;b&c.txt\" type=\"text/plain\">\nbody\n</document>","type":"input_text"},{"text":"<document type=\"text/<x>\">\nt\n</document>","type":"input_text"},{"file_data":"data:text/csv;base64,AA","filename":"document.csv","type":"input_file"},{"file_data":"data:image/x;base64,AA","filename":"document","type":"input_file"}],"role":"user","type":"message"},{"content":[{"text":"ok","type":"output_text"}],"role":"assistant","type":"message"},{"arguments":"{}","call_id":"c1","name":"f","type":"function_call"},{"arguments":"{}","call_id":"c2","name":"g","type":"function_call"},{"call_id":"c1","output":"one\n[document: application/pdf]\ntwo","type":"function_call_output"},{"call_id":"c2","output":[{"text":"see","type":"input_text"},{"image_url":"https://i","type":"input_image"}],"type":"function_call_output"}],"model":"m","store":false}"#
    );
}

#[test]
fn disabled_reasoning_sends_effort_none_without_include() {
    let mut req = user_prompt();
    req.reasoning = Some(HubReasoning {
        enabled: Some(false),
        effort: Some(HubEffort::High),
        ..HubReasoning::default()
    });
    assert_eq!(
        encode(&req, &Quirks::default()),
        json!({"input": hi_input(), "model": "m", "reasoning": {"effort": "none"}, "store": false})
    );
}

#[test]
fn a_provider_without_reasoning_requests_gets_no_reasoning() {
    let mut req = user_prompt();
    req.reasoning = Some(HubReasoning {
        effort: Some(HubEffort::High),
        ..HubReasoning::default()
    });
    let quirks = Quirks {
        reasoning_request: Some(ReasoningStyle::None),
        ..Quirks::default()
    };
    assert_eq!(
        encode(&req, &quirks),
        json!({"input": hi_input(), "model": "m", "store": false})
    );
}

#[test]
fn dropped_sampling_and_json_object_format() {
    let mut req = user_prompt();
    req.temperature = Some(0.5);
    req.top_p = Some(0.9);
    req.response_format = Some(HubResponseFormat::JsonObject);
    let quirks = Quirks {
        drop_sampling_params: Some(true),
        ..Quirks::default()
    };
    assert_eq!(
        encode(&req, &quirks),
        json!({"input": hi_input(), "model": "m", "store": false,
               "text": {"format": {"type": "json_object"}}})
    );
}

#[test]
fn a_loose_schema_an_allow_list_and_the_flags_encode_as_expected() {
    // `Allowed` maps to "required"; the tools are already filtered to the allow list.
    let mut req = user_prompt();
    req.temperature = Some(0.5);
    req.top_p = Some(0.9);
    req.response_format = Some(HubResponseFormat::JsonSchema {
        name: "n".into(),
        schema: json!({}),
        strict: None,
    });
    req.tools = vec![HubTool {
        name: "f".into(),
        description: None,
        parameters: json!({}),
        strict: None,
    }];
    req.tool_choice = Some(HubToolChoice::Allowed {
        names: vec!["f".into()],
    });
    req.parallel_tool_calls = Some(false);
    req.user = Some("u".into());
    req.stream = true;
    assert_eq!(
        encode(&req, &Quirks::default()),
        json!({
            "input": hi_input(), "model": "m", "parallel_tool_calls": false, "store": false,
            "stream": true, "temperature": 0.5,
            "text": {"format": {"name": "n", "schema": {}, "type": "json_schema"}},
            "tool_choice": "required",
            "tools": [{"name": "f", "parameters": {}, "strict": false, "type": "function"}],
            "top_p": 0.9, "user": "u",
        })
    );
}

#[test]
fn unsupported_request_content_keeps_its_messages() {
    let mut blocked = user_prompt();
    blocked.native_tools = vec![HubNativeTool::WebSearch {
        max_uses: None,
        allowed_domains: Vec::new(),
        blocked_domains: vec!["b.com".into()],
    }];
    let err = try_encode(&blocked, &Quirks::default()).unwrap_err();
    assert!(matches!(err, AppError::Unsupported { .. }), "{err:?}");
    assert_eq!(
        err.message(),
        "OpenAI Responses: web search `blocked_domains` is not supported, only `allowed_domains`"
    );

    let assistant_turn = |part: HubPart| HubRequest {
        messages: vec![
            HubMessage {
                role: HubRole::Assistant,
                content: vec![part],
            },
            HubMessage::user("q"),
        ],
        ..HubRequest::user_prompt("m", "")
    };
    let media = "OpenAI Responses: images and documents in assistant messages are not supported";
    for (part, message) in [
        (
            HubPart::Image {
                image: HubImage::Url { url: "u".into() },
            },
            media,
        ),
        (
            document(
                HubDocumentSource::Url { url: "u".into() },
                None,
                "application/pdf",
            ),
            media,
        ),
        (
            HubPart::Audio {
                media_type: "audio/wav".into(),
                data: "AA".into(),
            },
            "OpenAI Responses does not accept audio input here",
        ),
        (
            HubPart::ServerToolUse {
                id: "s".into(),
                name: "code".into(),
                input: json!({}),
                origin: Some(Protocol::OpenaiResponses),
            },
            "OpenAI Responses: server tool `code` cannot be replayed",
        ),
    ] {
        let err = try_encode(&assistant_turn(part), &Quirks::default()).unwrap_err();
        assert!(matches!(err, AppError::Unsupported { .. }), "{err:?}");
        assert_eq!(err.message(), message);
    }
}

#[test]
fn encoding_errors_keep_their_order() {
    // Prefill first, then message content, then native tools.
    let blocked = vec![HubNativeTool::WebSearch {
        max_uses: None,
        allowed_domains: Vec::new(),
        blocked_domains: vec!["b.com".into()],
    }];
    let prefill = HubRequest {
        messages: vec![
            HubMessage::user("hi"),
            HubMessage {
                role: HubRole::Assistant,
                content: vec![HubPart::Audio {
                    media_type: "audio/wav".into(),
                    data: "AA".into(),
                }],
            },
        ],
        native_tools: blocked.clone(),
        ..HubRequest::user_prompt("m", "")
    };
    assert!(
        try_encode(&prefill, &Quirks::default())
            .unwrap_err()
            .message()
            .contains("prefill")
    );
    let audio = HubRequest {
        messages: vec![HubMessage {
            role: HubRole::User,
            content: vec![HubPart::Audio {
                media_type: "audio/wav".into(),
                data: "AA".into(),
            }],
        }],
        native_tools: blocked,
        ..HubRequest::user_prompt("m", "")
    };
    assert_eq!(
        try_encode(&audio, &Quirks::default())
            .unwrap_err()
            .message(),
        "OpenAI Responses does not accept audio input here"
    );
}

#[test]
fn tool_result_errors_keep_their_kind_and_order() {
    let result = |content: Vec<HubPart>| HubRequest {
        messages: vec![HubMessage {
            role: HubRole::Tool,
            content: vec![HubPart::ToolResult {
                call_id: "c".into(),
                name: None,
                content,
                is_error: false,
            }],
        }],
        ..HubRequest::user_prompt("m", "")
    };
    let image = HubPart::Image {
        image: HubImage::AttachmentRef {
            attachment_id: "a".into(),
            media_type: "image/png".into(),
        },
    };
    let unresolved_document = document(
        HubDocumentSource::AttachmentRef {
            attachment_id: "d".into(),
        },
        None,
        "application/pdf",
    );
    let not_content = tool_call("c0", "f", "{}");
    let unsupported = "OpenAI Responses: only text, image and document parts can be a tool result";
    for (content, message, validation) in [
        (
            vec![image.clone()],
            "image attachment must be resolved before the request is encoded",
            true,
        ),
        (
            vec![HubPart::text("t"), unresolved_document, not_content.clone()],
            "document attachment must be resolved before the request is encoded",
            true,
        ),
        (vec![not_content, image], unsupported, false),
    ] {
        let err = try_encode(&result(content), &Quirks::default()).unwrap_err();
        assert_eq!(err.message(), message);
        assert_eq!(
            matches!(err, AppError::Validation { .. }),
            validation,
            "{err:?}"
        );
    }
}

// --- Decoding a client request ---

#[test]
fn decodes_request_with_developer_reasoning_and_tool_round_trip() {
    let req_body = json!({
        "model": "m", "instructions": "base",
        "input": [
            {"type": "message", "role": "developer", "content": "dev rules"},
            {"type": "message", "role": "user", "content": [
                {"type": "input_text", "text": "see"},
                {"type": "input_image", "image_url": "data:image/png;base64,AAA"},
            ]},
            {"type": "reasoning", "id": "rs_1", "encrypted_content": "enc", "summary": [{"type": "summary_text", "text": "why"}]},
            {"type": "function_call", "call_id": "c1", "name": "f", "arguments": "{}"},
            {"type": "function_call_output", "call_id": "c1", "output": "42"},
        ],
    });
    let req = decode_request(&req_body).unwrap();
    assert_eq!(req.system.as_deref(), Some("base\n\ndev rules"));
    assert_eq!(req.messages.len(), 3);
    assert_eq!(
        req.messages,
        vec![
            HubMessage {
                role: HubRole::User,
                content: vec![
                    HubPart::text("see"),
                    HubPart::Image {
                        image: HubImage::Base64 {
                            media_type: "image/png".into(),
                            data: "AAA".into(),
                        },
                    },
                ],
            },
            HubMessage {
                role: HubRole::Assistant,
                content: vec![
                    HubPart::Reasoning {
                        text: "why".into(),
                        signature: None,
                        origin: Some(Protocol::OpenaiResponses),
                        encrypted: Some("enc".into()),
                        id: Some("rs_1".into()),
                    },
                    tool_call("c1", "f", "{}"),
                ],
            },
            HubMessage {
                role: HubRole::Tool,
                content: vec![HubPart::ToolResult {
                    call_id: "c1".into(),
                    name: Some("f".into()),
                    content: vec![HubPart::text("42")],
                    is_error: false,
                }],
            },
        ]
    );
}

#[test]
fn string_input_and_role_only_items_are_user_messages() {
    for input in [json!("hi"), json!([{"role": "user", "content": "hi"}])] {
        let req = decode_request(&json!({"model": "m", "input": input})).unwrap();
        assert_eq!(req, HubRequest::user_prompt("m", "hi"));
    }
}

#[test]
fn function_call_outputs_take_arrays_objects_and_null() {
    let req = decode_request(&json!({"model": "m", "input": [
        {"type": "function_call", "call_id": "c1", "name": "f", "arguments": {"a": 1}},
        {"type": "function_call_output", "call_id": "c1", "output": [{"type": "input_text", "text": "t"}]},
        {"type": "function_call_output", "call_id": "c2", "output": {"k": 1}},
        {"type": "function_call_output", "call_id": "c3", "output": null},
    ]}))
    .unwrap();
    let result = |call_id: &str, name: Option<&str>, content: Vec<HubPart>| HubPart::ToolResult {
        call_id: call_id.into(),
        name: name.map(Into::into),
        content,
        is_error: false,
    };
    assert_eq!(
        req.messages,
        vec![
            HubMessage {
                role: HubRole::Assistant,
                content: vec![tool_call("c1", "f", "{\"a\":1}")],
            },
            HubMessage {
                role: HubRole::Tool,
                content: vec![
                    result("c1", Some("f"), vec![HubPart::text("t")]),
                    result("c2", None, vec![HubPart::text("{\"k\":1}")]),
                    result("c3", None, Vec::new()),
                ],
            },
        ]
    );
}

#[test]
fn input_files_map_to_documents() {
    let req = decode_request(&json!({"model": "m", "input": [{"role": "user", "content": [
        {"type": "input_file", "filename": "a.pdf", "file_data": "data:application/pdf;base64,AA"},
        {"type": "input_file", "file_url": "https://x/r.CSV?sig=1"},
        {"type": "input_file", "filename": "n.md", "file_url": "https://x/r"},
    ]}]}))
    .unwrap();
    assert_eq!(
        req.messages[0].content,
        vec![
            document(
                HubDocumentSource::Base64 { data: "AA".into() },
                Some("a.pdf"),
                "application/pdf"
            ),
            document(
                HubDocumentSource::Url {
                    url: "https://x/r.CSV?sig=1".into()
                },
                None,
                "text/csv"
            ),
            document(
                HubDocumentSource::Url {
                    url: "https://x/r".into()
                },
                Some("n.md"),
                "text/markdown"
            ),
        ]
    );
}

#[test]
fn web_search_tools_take_the_global_call_limit() {
    let req = decode_request(
        &json!({"model": "m", "input": "x", "max_tool_calls": 2, "tools": [
            {"type": "web_search_preview_2025_03_11"},
            {"type": "web_search", "filters": {"allowed_domains": ["a.com"]}},
            {"name": "f"},
        ]}),
    )
    .unwrap();
    assert_eq!(
        req.tools,
        vec![HubTool {
            name: "f".into(),
            description: None,
            parameters: json!({"type": "object"}),
            strict: None,
        }]
    );
    assert_eq!(
        req.native_tools,
        vec![
            HubNativeTool::WebSearch {
                max_uses: Some(2),
                allowed_domains: Vec::new(),
                blocked_domains: Vec::new(),
            },
            HubNativeTool::WebSearch {
                max_uses: Some(2),
                allowed_domains: vec!["a.com".into()],
                blocked_domains: Vec::new(),
            },
        ]
    );
}

#[test]
fn tool_choice_format_and_effort_decode() {
    let with = |key: &str, value: Value| {
        let mut body = json!({"model": "m", "input": "x"});
        body[key] = value;
        decode_request(&body)
    };
    assert_eq!(
        with("tool_choice", json!({"type": "function", "name": "f"}))
            .unwrap()
            .tool_choice,
        Some(HubToolChoice::Tool { name: "f".into() })
    );
    assert_eq!(
        with("tool_choice", json!("required")).unwrap().tool_choice,
        Some(HubToolChoice::Required)
    );
    assert_eq!(
        with("text", json!({"format": {"type": "json_schema"}}))
            .unwrap()
            .response_format,
        Some(HubResponseFormat::JsonSchema {
            name: "response".into(),
            schema: json!({}),
            strict: None,
        })
    );
    assert_eq!(
        with("reasoning", json!({"effort": "none"}))
            .unwrap()
            .reasoning,
        Some(HubReasoning {
            enabled: Some(false),
            ..HubReasoning::default()
        })
    );
    assert_eq!(
        with("reasoning", json!({"effort": "high"}))
            .unwrap()
            .reasoning,
        Some(HubReasoning {
            effort: Some(HubEffort::High),
            ..HubReasoning::default()
        })
    );
    assert_eq!(
        with("reasoning", json!({"effort": "bogus"}))
            .unwrap()
            .reasoning,
        None
    );
}

#[test]
fn request_decoding_errors_keep_their_kind_and_message() {
    let validation = |body: Value, message: &str| {
        let err = decode_request(&body).unwrap_err();
        assert!(matches!(err, AppError::Validation { .. }), "{err:?}");
        assert_eq!(err.message(), message);
    };
    let unsupported = |body: Value, message: &str| {
        let err = decode_request(&body).unwrap_err();
        assert!(matches!(err, AppError::Unsupported { .. }), "{err:?}");
        assert_eq!(err.message(), message);
    };
    validation(json!([]), "request body must be a JSON object");
    validation(json!({"input": "x"}), "`model` is required");
    validation(
        json!({"model": "m", "input": 3}),
        "`input` must be a string or an array of items",
    );
    validation(
        json!({"model": "m", "input": [{"type": "message", "role": "bot", "content": "x"}]}),
        "unknown message role `bot`",
    );
    // An explicit empty `type` reads as missing.
    for item in [json!({"type": ""}), json!({"x": 1})] {
        unsupported(
            json!({"model": "m", "input": [item]}),
            "Responses input item type `<missing>` is not supported",
        );
    }
    unsupported(
        json!({"model": "m", "input": [{"role": "user", "content": [{"type": "input_file", "file_id": "f"}]}]}),
        "`input_file` with a `file_id` is not supported, send `file_data` or `file_url`",
    );
    unsupported(
        json!({"model": "m", "input": [{"role": "user", "content": [{"type": "input_file", "file_data": "AA"}]}]}),
        "`input_file.file_data` must be a base64 `data:` URL",
    );
    validation(
        json!({"model": "m", "input": "x", "tool_choice": "sometimes"}),
        "unknown tool_choice `sometimes`",
    );
    unsupported(
        json!({"model": "m", "input": "x", "tool_choice": {"type": "mcp"}}),
        "only `auto`, `none`, `required` and function tool_choice are supported",
    );
    unsupported(
        json!({"model": "m", "input": "x", "text": {"format": {"type": "yaml"}}}),
        "text format `yaml` is not supported",
    );
}

#[test]
fn request_decoding_errors_keep_their_order() {
    // input items, then tools, then tool_choice, then the text format.
    let bad_tool = json!([{"type": "mcp"}]);
    let tool_error =
        "Responses tool type `mcp` is not supported, only `function` and web search tools";
    let cases = [
        (
            json!({"model": "m", "input": 3, "tools": bad_tool}),
            "`input` must be a string or an array of items",
        ),
        (
            json!({"model": "m", "input": "x", "tools": bad_tool, "tool_choice": "sometimes"}),
            tool_error,
        ),
        (
            json!({"model": "m", "input": "x", "tool_choice": "sometimes",
                   "text": {"format": {"type": "yaml"}}}),
            "unknown tool_choice `sometimes`",
        ),
    ];
    for (body, message) in cases {
        assert_eq!(decode_request(&body).unwrap_err().message(), message);
    }
}

#[test]
fn history_may_call_a_tool_the_current_tool_table_no_longer_has() {
    // A tool dropped from the table (a workspace tool of a chat that is no longer bound) stays
    // in the older turns: the pair is encoded as written and only the table shrinks.
    let mut req = user_prompt();
    req.tools = vec![HubTool {
        name: "run_probe".into(),
        description: None,
        parameters: json!({"type": "object"}),
        strict: None,
    }];
    req.messages.push(HubMessage {
        role: HubRole::Assistant,
        content: vec![tool_call("t1", "list_requests", "{}")],
    });
    req.messages.push(HubMessage {
        role: HubRole::Tool,
        content: vec![HubPart::ToolResult {
            call_id: "t1".into(),
            name: None,
            content: vec![HubPart::text("not bound")],
            is_error: true,
        }],
    });
    let body = encode(&req, &Quirks::default());
    assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    assert_eq!(body["tools"][0]["name"], "run_probe");
    let input = body["input"].as_array().unwrap();
    let call = input.iter().find(|item| item["type"] == "function_call");
    assert_eq!(call.unwrap()["name"], "list_requests");
    let output = input
        .iter()
        .find(|item| item["type"] == "function_call_output");
    assert_eq!(output.unwrap()["call_id"], "t1");
}
