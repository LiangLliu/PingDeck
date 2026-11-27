//! Request and response encoding.

use super::*;

#[test]
fn encode_merges_same_role_turns_with_tool_results_first() {
    let req = HubRequest {
        model: "claude".into(),
        system: Some("be brief".into()),
        messages: vec![
            HubMessage::user("hi"),
            HubMessage::user("again"),
            HubMessage {
                role: HubRole::Assistant,
                content: vec![
                    reasoning("deep", Some("sig1"), None),
                    reasoning("", None, Some("blob")),
                    reasoning("unsigned", None, None),
                    HubPart::text("calling"),
                    call("t1", "f", r#"{"a":1}"#),
                    call("t2", "g", "oops"),
                ],
            },
            HubMessage {
                role: HubRole::Tool,
                content: vec![result("t1", "ok", false)],
            },
            HubMessage {
                role: HubRole::Tool,
                content: vec![result("t2", "bad", true)],
            },
            HubMessage::user("thanks"),
        ],
        ..HubRequest::default()
    };
    let body = encode(&req, &Quirks::default());
    assert_eq!(body["system"], "be brief");
    assert_eq!(body["max_tokens"], 8192);
    assert_eq!(
        body["messages"],
        json!([
            {"role": "user", "content": [text_block("hi"), text_block("again")]},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "deep", "signature": "sig1"},
                {"type": "redacted_thinking", "data": "blob"},
                text_block("calling"),
                {"type": "tool_use", "id": "t1", "name": "f", "input": {"a": 1}},
                {"type": "tool_use", "id": "t2", "name": "g", "input": {"_raw": "oops"}},
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "t1", "content": [text_block("ok")]},
                {"type": "tool_result", "tool_use_id": "t2", "content": [text_block("bad")], "is_error": true},
                text_block("thanks"),
            ]},
        ])
    );
}

#[test]
fn forced_tool_choice_disables_parallel_use_and_json_object_adds_system_note() {
    let mut tools_req = HubRequest::user_prompt("claude", "q");
    tools_req.tools = vec![HubTool {
        name: "f".into(),
        description: Some("d".into()),
        parameters: json!({"type": "object"}),
        strict: Some(true),
    }];
    tools_req.tool_choice = Some(HubToolChoice::Required);
    tools_req.parallel_tool_calls = Some(false);
    tools_req.response_format = Some(HubResponseFormat::JsonObject);
    let body = encode(&tools_req, &Quirks::default());
    assert_eq!(
        body["tool_choice"],
        json!({"type": "any", "disable_parallel_tool_use": true})
    );
    assert_eq!(body["system"], JSON_OBJECT_NOTE);
}

#[test]
fn prefill_policy_and_claude_generation() {
    for (model, expected) in [
        ("claude-opus-4-20250514", Some((4, 0))),
        ("claude-sonnet-4.6", Some((4, 6))),
        ("us.anthropic.claude-opus-4-6-v1:0", Some((4, 6))),
        ("glm-5", None),
    ] {
        assert_eq!(claude_generation(model), expected, "{model}");
    }

    let prefill = |model: &str, reasoning: Option<HubReasoning>| HubRequest {
        model: model.into(),
        messages: vec![
            HubMessage::user("Give JSON"),
            HubMessage::assistant("Sure, here it is:  \n"),
        ],
        reasoning,
        ..HubRequest::default()
    };
    let thinking = Some(HubReasoning {
        budget_tokens: Some(2000),
        ..HubReasoning::default()
    });
    for (model, reasoning) in [
        ("claude-sonnet-4-6", None),
        ("claude-mythos-preview", None),
        ("claude-sonnet-4-5-20250929", thinking.clone()),
    ] {
        let err = AnthropicMessages
            .encode_request(
                &prefill(model, reasoning),
                &EncodeCtx {
                    quirks: &Quirks::default(),
                },
            )
            .unwrap_err();
        assert!(
            err.to_string()
                .contains("trailing assistant message (prefill)")
        );
    }

    for (model, reasoning) in [("claude-sonnet-4-5-20250929", None), ("glm-5", thinking)] {
        let body = encode(&prefill(model, reasoning), &Quirks::default());
        assert_eq!(
            body["messages"][1],
            json!({"role": "assistant", "content": [text_block("Sure, here it is:")]})
        );
    }
}

#[test]
fn documents_unresolved_attachments_and_server_tools() {
    let doc = |source: HubDocumentSource, media_type: &str, name: Option<&str>| HubPart::Document {
        document: HubDocument {
            media_type: media_type.into(),
            name: name.map(Into::into),
            source,
        },
    };
    let req = HubRequest {
        model: "claude-sonnet-4-5".into(),
        messages: vec![HubMessage {
            role: HubRole::User,
            content: vec![
                HubPart::text("read these"),
                doc(
                    HubDocumentSource::Base64 {
                        data: "JVBER".into(),
                    },
                    "application/pdf",
                    Some("spec.pdf"),
                ),
                doc(
                    HubDocumentSource::Text {
                        text: "# notes".into(),
                    },
                    "text/markdown",
                    None,
                ),
                doc(
                    HubDocumentSource::Url {
                        url: "https://x.test/a.pdf".into(),
                    },
                    "application/pdf",
                    Some(""),
                ),
            ],
        }],
        native_tools: vec![HubNativeTool::WebSearch {
            max_uses: Some(3),
            allowed_domains: vec!["a.test".into()],
            blocked_domains: Vec::new(),
        }],
        ..HubRequest::default()
    };
    let body = encode(&req, &Quirks::default());
    assert_eq!(body["messages"][0]["content"].as_array().unwrap().len(), 4);
    assert_eq!(
        body["tools"],
        json!([{"type": "web_search_20250305", "name": "web_search", "max_uses": 3, "allowed_domains": ["a.test"]}])
    );

    let unresolved_req = HubRequest {
        model: "claude-sonnet-4-5".into(),
        messages: vec![HubMessage {
            role: HubRole::User,
            content: vec![HubPart::Image {
                image: HubImage::AttachmentRef {
                    attachment_id: "att-1".into(),
                    media_type: "image/png".into(),
                },
            }],
        }],
        ..HubRequest::default()
    };
    assert!(matches!(
        AnthropicMessages.encode_request(
            &unresolved_req,
            &EncodeCtx {
                quirks: &Quirks::default()
            }
        ),
        Err(AppError::Validation { .. })
    ));

    let search_use = |origin| HubPart::ServerToolUse {
        id: "srvtoolu_1".into(),
        name: "web_search".into(),
        input: json!({"query": "rust 1.99"}),
        origin,
    };
    let own_replay = encode(
        &HubRequest {
            model: "claude-sonnet-4-5".into(),
            messages: vec![
                HubMessage::user("news?"),
                HubMessage {
                    role: HubRole::Assistant,
                    content: vec![search_use(Some(OWN)), HubPart::text("answer")],
                },
                HubMessage::user("more"),
            ],
            ..HubRequest::default()
        },
        &Quirks::default(),
    );
    assert_eq!(
        own_replay["messages"][1]["content"]
            .as_array()
            .unwrap()
            .len(),
        2
    );

    let foreign_replay = encode(
        &HubRequest {
            model: "claude-sonnet-4-5".into(),
            messages: vec![
                HubMessage::user("news?"),
                HubMessage {
                    role: HubRole::Assistant,
                    content: vec![
                        search_use(Some(Protocol::OpenaiResponses)),
                        HubPart::text("answer"),
                    ],
                },
                HubMessage::user("more"),
            ],
            ..HubRequest::default()
        },
        &Quirks::default(),
    );
    assert_eq!(
        foreign_replay["messages"][1]["content"]
            .as_array()
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn cache_hints_mark_the_tools_the_system_block_and_the_last_message() {
    let mut req = HubRequest::user_prompt("claude-sonnet-4-5", "first");
    req.system = Some("be brief".into());
    req.tools = ["a", "b"]
        .map(|name| HubTool {
            name: name.into(),
            description: None,
            parameters: json!({"type": "object"}),
            strict: None,
        })
        .to_vec();
    req.messages.push(HubMessage {
        role: HubRole::Assistant,
        content: vec![reasoning("hm", Some("sig"), None), call("c1", "a", "{}")],
    });
    req.messages.push(HubMessage {
        role: HubRole::Tool,
        content: vec![result("c1", "ok", false)],
    });
    let plain = encode(&req, &Quirks::default());
    assert!(!plain.to_string().contains("cache_control"));

    req.cache = Some(CacheHint {
        breakpoints: vec![
            CacheBreakpoint::SystemAndTools,
            CacheBreakpoint::LastMessage,
        ],
        key: Some("session-1".into()),
    });
    let body = encode(&req, &Quirks::default());
    let ephemeral = json!({"type": "ephemeral"});
    assert_eq!(body["tools"][1]["cache_control"], ephemeral);
    assert!(body["tools"][0].get("cache_control").is_none());
    assert_eq!(body["system"][0]["text"], "be brief");
    assert_eq!(body["system"][0]["cache_control"], ephemeral);
    let last = body["messages"].as_array().unwrap().last().unwrap().clone();
    assert_eq!(last["content"][0]["type"], "tool_result");
    assert_eq!(last["content"][0]["cache_control"], ephemeral);
    assert_eq!(body.to_string().matches("cache_control").count(), 3);
    assert!(body.get("prompt_cache_key").is_none());
}

fn reasoning_from(
    text: &str,
    signature: Option<&str>,
    encrypted: Option<&str>,
    id: Option<&str>,
    origin: Option<Protocol>,
) -> HubPart {
    HubPart::Reasoning {
        text: text.into(),
        signature: signature.map(Into::into),
        encrypted: encrypted.map(Into::into),
        id: id.map(Into::into),
        origin,
    }
}

#[test]
fn golden_encode_response_wraps_foreign_signatures_and_drops_unreplayable_parts() {
    let response = HubResponse {
        id: "msg_r".into(),
        model: "claude-x".into(),
        content: vec![
            reasoning_from("r1", Some("gsig"), None, None, Some(Protocol::Gemini)),
            reasoning_from("", None, Some("blob"), None, Some(OWN)),
            reasoning_from(
                "",
                None,
                Some("enc"),
                Some("rs_1"),
                Some(Protocol::OpenaiResponses),
            ),
            reasoning_from("plain", None, None, None, None),
            HubPart::ServerToolUse {
                id: "srv".into(),
                name: "web_search".into(),
                input: json!({"query": "q"}),
                origin: Some(Protocol::OpenaiResponses),
            },
            HubPart::text("done"),
        ],
        finish: HubFinish::Stop,
        usage: HubUsage {
            input_tokens: Some(12),
            output_tokens: Some(3),
            cached_input_tokens: Some(2),
            ..HubUsage::default()
        },
    };
    assert_eq!(
        test_support::sort_json_keys(AnthropicMessages.encode_response(&response)).to_string(),
        concat!(
            r#"{"content":[{"signature":"pdk1.gemini.gsig","thinking":"r1","type":"thinking"},"#,
            r#"{"data":"blob","type":"redacted_thinking"},"#,
            r#"{"signature":"","thinking":"plain","type":"thinking"},"#,
            r#"{"text":"done","type":"text"}],"#,
            r#""id":"msg_r","model":"claude-x","role":"assistant","stop_reason":"end_turn","#,
            r#""stop_sequence":null,"type":"message","usage":{"cache_creation_input_tokens":0,"#,
            r#""cache_read_input_tokens":2,"input_tokens":10,"output_tokens":3}}"#,
        )
    );
}

#[test]
fn parallel_tool_use_is_disabled_on_every_choice_but_none() {
    let request = |choice: Option<HubToolChoice>| HubRequest {
        tools: vec![HubTool {
            name: "f".into(),
            description: None,
            parameters: json!({"type": "object"}),
            strict: None,
        }],
        tool_choice: choice,
        parallel_tool_calls: Some(false),
        ..HubRequest::user_prompt("claude-sonnet-4-5", "q")
    };
    for (choice, expected) in [
        (
            None,
            json!({"type": "auto", "disable_parallel_tool_use": true}),
        ),
        (
            Some(HubToolChoice::Tool { name: "f".into() }),
            json!({"type": "tool", "name": "f", "disable_parallel_tool_use": true}),
        ),
        (Some(HubToolChoice::None), json!({"type": "none"})),
        (
            Some(HubToolChoice::Auto),
            json!({"type": "auto", "disable_parallel_tool_use": true}),
        ),
    ] {
        let body = encode(&request(choice.clone()), &Quirks::default());
        assert_eq!(body["tool_choice"], expected, "{choice:?}");
    }
    let mut no_flag = request(None);
    no_flag.parallel_tool_calls = None;
    assert!(
        encode(&no_flag, &Quirks::default())
            .get("tool_choice")
            .is_none()
    );
}

#[test]
fn unsupported_inputs_are_rejected_with_their_messages() {
    let audio = || HubPart::Audio {
        media_type: "audio/wav".into(),
        data: "UklG".into(),
    };
    let user = |content: Vec<HubPart>| HubRequest {
        messages: vec![HubMessage {
            role: HubRole::User,
            content,
        }],
        ..HubRequest::user_prompt("claude-sonnet-4-5", "q")
    };
    let tool_result = |content: Vec<HubPart>| HubPart::ToolResult {
        call_id: "c".into(),
        name: None,
        content,
        is_error: false,
    };
    let mut both_domains = HubRequest::user_prompt("claude-sonnet-4-5", "q");
    both_domains.native_tools = vec![HubNativeTool::WebSearch {
        max_uses: None,
        allowed_domains: vec!["a.test".into()],
        blocked_domains: vec!["b.test".into()],
    }];
    for (req, message) in [
        (
            both_domains,
            "anthropic: web search accepts `allowed_domains` or `blocked_domains`, not both",
        ),
        (
            user(vec![audio()]),
            "anthropic: Anthropic Messages does not accept audio input",
        ),
        (
            user(vec![tool_result(vec![audio()])]),
            "anthropic: Anthropic Messages does not accept audio input",
        ),
        (
            user(vec![tool_result(vec![call("x", "f", "{}")])]),
            "anthropic: a tool call inside a tool result is not supported",
        ),
    ] {
        let err = try_encode(&req, &Quirks::default()).unwrap_err();
        assert!(matches!(err, AppError::Unsupported { .. }), "{message}");
        assert_eq!(err.message(), message);
    }
}

#[test]
fn history_may_call_a_tool_the_current_tool_table_no_longer_has() {
    // A tool dropped from the table (a workspace tool of a chat that is no longer bound) stays
    // in the older turns: the pair is encoded as written and only the table shrinks.
    let mut req = HubRequest::user_prompt("claude-sonnet-4-5", "list my requests");
    req.tools = vec![HubTool {
        name: "run_probe".into(),
        description: None,
        parameters: json!({"type": "object"}),
        strict: None,
    }];
    req.messages.push(HubMessage {
        role: HubRole::Assistant,
        content: vec![call("t1", "list_requests", "{}")],
    });
    req.messages.push(HubMessage {
        role: HubRole::Tool,
        content: vec![result("t1", "not bound", true)],
    });
    let body = encode(&req, &Quirks::default());
    assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    assert_eq!(body["tools"][0]["name"], "run_probe");
    assert_eq!(body["messages"][1]["content"][0]["type"], "tool_use");
    assert_eq!(body["messages"][1]["content"][0]["name"], "list_requests");
    assert_eq!(body["messages"][2]["content"][0]["tool_use_id"], "t1");
}
