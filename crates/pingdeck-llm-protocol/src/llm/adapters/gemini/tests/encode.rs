//! Request and response encoding.

use super::*;

#[test]
fn encode_merges_turns_signs_reasoning_and_names_tool_results() {
    let req = HubRequest {
        model: "ignored".into(),
        system: Some("be brief".into()),
        messages: vec![
            HubMessage::user("weather?"),
            HubMessage {
                role: HubRole::Assistant,
                content: vec![
                    reasoning("hmm", Some("rsig")),
                    reasoning("unsigned", None),
                    call("c1", "get_weather", r#"{"city":"Paris"}"#, Some("sigA")),
                ],
            },
            HubMessage {
                role: HubRole::Tool,
                content: vec![HubPart::ToolResult {
                    call_id: "c1".into(),
                    name: None,
                    content: vec![HubPart::text("sunny")],
                    is_error: false,
                }],
            },
            HubMessage::user("thanks"),
        ],
        stream: true,
        ..HubRequest::default()
    };
    assert_eq!(
        encode(&req),
        json!({
            "systemInstruction": {"parts": [{"text": "be brief"}]},
            "contents": [
                {"role": "user", "parts": [{"text": "weather?"}]},
                {"role": "model", "parts": [
                    {"text": "hmm", "thought": true, "thoughtSignature": "rsig"},
                    {"functionCall": {"name": "get_weather", "args": {"city": "Paris"}, "id": "c1"}, "thoughtSignature": "sigA"},
                ]},
                {"role": "user", "parts": [
                    {"functionResponse": {"id": "c1", "name": "get_weather", "response": {"result": "sunny"}}},
                    {"text": "thanks"},
                ]},
            ],
        })
    );
}

#[test]
fn the_first_unsigned_call_gets_the_dummy_signature_and_bad_arguments_are_wrapped() {
    let unsigned_calls = HubRequest {
        messages: vec![
            HubMessage {
                role: HubRole::Assistant,
                content: vec![
                    call("c1", "f", "not json", None),
                    call("c2", "g", "[1]", None),
                ],
            },
            HubMessage {
                role: HubRole::Tool,
                content: vec![HubPart::ToolResult {
                    call_id: "c1".into(),
                    name: Some("explicit".into()),
                    content: vec![HubPart::text("boom")],
                    is_error: true,
                }],
            },
        ],
        ..HubRequest::default()
    };
    let parts = encode(&unsigned_calls);
    assert_eq!(
        parts["contents"][0]["parts"][0]["functionCall"]["args"],
        json!({"_raw": "not json"})
    );
    assert_eq!(
        parts["contents"][0]["parts"][0]["thoughtSignature"],
        DUMMY_THOUGHT_SIGNATURE
    );
    assert!(
        parts["contents"][0]["parts"][1]
            .get("thoughtSignature")
            .is_none()
    );
}

#[test]
fn json_schema_effort_and_a_forced_tool_map_to_generation_and_tool_config() {
    let schema = json!({"type": "object", "properties": {"a": {"type": "string"}}});
    let cfg_req = HubRequest {
        response_format: Some(HubResponseFormat::JsonSchema {
            name: "out".into(),
            schema: schema.clone(),
            strict: Some(true),
        }),
        reasoning: Some(HubReasoning {
            effort: Some(HubEffort::Minimal),
            ..HubReasoning::default()
        }),
        tools: vec![HubTool {
            name: "f".into(),
            description: Some("does f".into()),
            parameters: schema.clone(),
            strict: None,
        }],
        tool_choice: Some(HubToolChoice::Tool { name: "f".into() }),
        ..HubRequest::user_prompt("m", "hi")
    };
    let body = encode(&cfg_req);
    assert_eq!(
        body["generationConfig"],
        json!({
            "responseMimeType": "application/json",
            "responseJsonSchema": schema,
            "thinkingConfig": {"includeThoughts": true, "thinkingLevel": "minimal"}
        })
    );
    assert_eq!(
        body["toolConfig"],
        json!({"functionCallingConfig": {"mode": "ANY", "allowedFunctionNames": ["f"]}})
    );
}

#[test]
fn a_prefill_request_is_rejected() {
    let prefill = HubRequest {
        messages: vec![HubMessage::user("q"), HubMessage::assistant("Translation:")],
        ..HubRequest::default()
    };
    let err = try_encode(&prefill).unwrap_err();
    assert!(matches!(err, AppError::Validation { .. }));
    assert!(err.message().contains("prefill"));
}

#[test]
fn documents_and_audio_encode_inline_and_web_search_is_sent_once() {
    let doc_req = HubRequest {
        messages: vec![HubMessage {
            role: HubRole::User,
            content: vec![
                HubPart::Document {
                    document: HubDocument {
                        media_type: "application/pdf".into(),
                        name: Some("spec.pdf".into()),
                        source: HubDocumentSource::Base64 {
                            data: "JVBE".into(),
                        },
                    },
                },
                HubPart::Document {
                    document: HubDocument {
                        media_type: "text/markdown".into(),
                        name: Some("notes.md".into()),
                        source: HubDocumentSource::Text {
                            text: "# hi".into(),
                        },
                    },
                },
                HubPart::Audio {
                    media_type: "audio/wav".into(),
                    data: "UklG".into(),
                },
            ],
        }],
        native_tools: vec![HubNativeTool::WebSearch {
            max_uses: Some(3),
            allowed_domains: Vec::new(),
            blocked_domains: Vec::new(),
        }],
        ..HubRequest::default()
    };
    let encoded = encode(&doc_req);
    assert_eq!(encoded["contents"][0]["parts"].as_array().unwrap().len(), 3);
    assert_eq!(
        encoded["contents"][0]["parts"],
        json!([
            {"inlineData": {"mimeType": "application/pdf", "data": "JVBE"}},
            {"text": "<document name=\"notes.md\" type=\"text/markdown\">\n# hi\n</document>"},
            {"inlineData": {"mimeType": "audio/wav", "data": "UklG"}},
        ])
    );
    assert_eq!(encoded["tools"], json!([{"google_search": {}}]));
}

#[test]
fn an_unresolved_attachment_is_a_validation_error() {
    let unresolved = HubRequest {
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
        try_encode(&unresolved),
        Err(AppError::Validation { .. })
    ));
}

#[test]
fn thinking_config_follows_the_gemini_generation() {
    // Gemini 2.x takes only a budget, 3 and later a level.
    let request = |model: &str| HubRequest {
        reasoning: Some(HubReasoning {
            effort: Some(HubEffort::Medium),
            ..HubReasoning::default()
        }),
        ..HubRequest::user_prompt(model, "q")
    };
    let thinking =
        |model: &str| encode(&request(model))["generationConfig"]["thinkingConfig"].clone();
    assert_eq!(
        thinking("gemini-2.5-pro"),
        json!({"includeThoughts": true, "thinkingBudget": 8_192})
    );
    assert_eq!(
        thinking("gemini-3-pro-preview"),
        json!({"includeThoughts": true, "thinkingLevel": "medium"})
    );
}

#[test]
fn golden_function_response_refers_to_inline_media_by_display_name() {
    let req = HubRequest {
        messages: vec![
            HubMessage {
                role: HubRole::Assistant,
                content: vec![call("c1", "look", "{}", None)],
            },
            HubMessage {
                role: HubRole::Tool,
                content: vec![
                    HubPart::ToolResult {
                        call_id: "c1".into(),
                        name: None,
                        is_error: false,
                        content: vec![
                            HubPart::text("seen"),
                            HubPart::Image {
                                image: HubImage::Base64 {
                                    media_type: "image/png".into(),
                                    data: "P1".into(),
                                },
                            },
                            HubPart::Document {
                                document: HubDocument {
                                    media_type: "application/pdf".into(),
                                    name: Some("a.pdf".into()),
                                    source: HubDocumentSource::Base64 { data: "D1".into() },
                                },
                            },
                            HubPart::Image {
                                image: HubImage::Base64 {
                                    media_type: "image/jpeg".into(),
                                    data: "P2".into(),
                                },
                            },
                            HubPart::text(" too"),
                        ],
                    },
                    HubPart::ToolResult {
                        call_id: "c2".into(),
                        name: Some("x".into()),
                        is_error: true,
                        content: vec![],
                    },
                ],
            },
        ],
        ..HubRequest::default()
    };
    // Media are numbered per kind; an error result without text is still an `error` key.
    assert_eq!(
        test_support::sort_json_keys(encode(&req)).to_string(),
        r#"{"contents":[{"parts":[{"functionCall":{"args":{},"id":"c1","name":"look"},"thoughtSignature":"context_engineering_is_the_way_to_go"}],"role":"model"},{"parts":[{"functionResponse":{"id":"c1","name":"look","parts":[{"inlineData":{"data":"P1","displayName":"image_1","mimeType":"image/png"}},{"inlineData":{"data":"D1","displayName":"document_1","mimeType":"application/pdf"}},{"inlineData":{"data":"P2","displayName":"image_2","mimeType":"image/jpeg"}}],"response":{"document_1":{"$ref":"document_1"},"image_1":{"$ref":"image_1"},"image_2":{"$ref":"image_2"},"result":"seen too"}}},{"functionResponse":{"id":"c2","name":"x","response":{"error":""}}}],"role":"user"}]}"#
    );
}

#[test]
fn golden_encode_response_places_grounding_supports_by_part_index() {
    let response = HubResponse {
        id: "r9".into(),
        model: "gemini-x".into(),
        content: vec![
            reasoning("plan", Some("RS")),
            reasoning("", None),
            HubPart::ServerToolUse {
                id: "s".into(),
                name: "web_search".into(),
                input: json!({"queries": ["q1", "q2"]}),
                origin: None,
            },
            HubPart::Text {
                text: "Rust".into(),
                signature: None,
                origin: None,
                citations: vec![HubCitation {
                    url: "https://a".into(),
                    title: "A".into(),
                    snippet: Some("Rust".into()),
                    start_index: Some(0),
                    end_index: Some(4),
                }],
            },
            HubPart::ServerToolResult {
                call_id: "s".into(),
                name: "web_search".into(),
                payload: Value::Null,
                citations: vec![HubCitation {
                    url: "https://b".into(),
                    title: "B".into(),
                    snippet: None,
                    start_index: None,
                    end_index: None,
                }],
                origin: None,
            },
            HubPart::Text {
                text: "more".into(),
                signature: Some("TS".into()),
                origin: Some(Protocol::OpenaiResponses),
                citations: vec![HubCitation {
                    url: "https://a".into(),
                    title: "A".into(),
                    snippet: Some("m".into()),
                    start_index: Some(1),
                    end_index: Some(2),
                }],
            },
            call("c1", "f", "{\"a\":1}", Some("CS")),
            HubPart::Image {
                image: HubImage::Base64 {
                    media_type: "image/png".into(),
                    data: "AA".into(),
                },
            },
        ],
        finish: HubFinish::ToolCalls,
        usage: HubUsage {
            input_tokens: Some(5),
            output_tokens: Some(9),
            reasoning_tokens: Some(4),
            ..HubUsage::default()
        },
    };
    assert_eq!(
        test_support::sort_json_keys(GeminiGenerate.encode_response(&response)).to_string(),
        r#"{"candidates":[{"content":{"parts":[{"text":"plan","thought":true,"thoughtSignature":"RS"},{"text":"Rust"},{"text":"more","thoughtSignature":"pdk1.openai_responses.TS"},{"functionCall":{"args":{"a":1},"id":"c1","name":"f"},"thoughtSignature":"CS"},{"inlineData":{"data":"AA","mimeType":"image/png"}}],"role":"model"},"finishReason":"STOP","groundingMetadata":{"groundingChunks":[{"web":{"title":"A","uri":"https://a"}},{"web":{"title":"B","uri":"https://b"}}],"groundingSupports":[{"groundingChunkIndices":[0],"segment":{"endIndex":4,"partIndex":1,"startIndex":0,"text":"Rust"}},{"groundingChunkIndices":[0],"segment":{"endIndex":2,"partIndex":2,"startIndex":1,"text":"m"}}],"webSearchQueries":["q1","q2"]},"index":0}],"modelVersion":"gemini-x","responseId":"r9","usageMetadata":{"candidatesTokenCount":5,"promptTokenCount":5,"thoughtsTokenCount":4,"totalTokenCount":14}}"#
    );
}

#[test]
fn golden_encode_response_without_usage_model_or_id() {
    let response = HubResponse {
        id: String::new(),
        model: String::new(),
        content: vec![HubPart::text("x")],
        finish: HubFinish::Stop,
        usage: HubUsage::default(),
    };
    assert_eq!(
        test_support::sort_json_keys(GeminiGenerate.encode_response(&response)).to_string(),
        r#"{"candidates":[{"content":{"parts":[{"text":"x"}],"role":"model"},"finishReason":"STOP","index":0}]}"#
    );
}

#[test]
fn a_budget_generation_keeps_an_explicit_budget_next_to_an_effort() {
    // Gemini 2.x takes only `thinkingBudget`, so an
    // explicit budget wins over the effort and no `thinkingLevel` is sent.
    let reasoning = |effort: Option<HubEffort>, budget: Option<u32>| HubRequest {
        reasoning: Some(HubReasoning {
            effort,
            budget_tokens: budget,
            ..HubReasoning::default()
        }),
        ..HubRequest::user_prompt("gemini-2.5-pro", "q")
    };
    let thinking = |req: &HubRequest| encode(req)["generationConfig"]["thinkingConfig"].clone();
    assert_eq!(
        thinking(&reasoning(Some(HubEffort::High), Some(4_096))),
        json!({"includeThoughts": true, "thinkingBudget": 4_096})
    );
    assert_eq!(
        thinking(&reasoning(None, Some(4_096))),
        json!({"includeThoughts": true, "thinkingBudget": 4_096})
    );
    assert_eq!(
        thinking(&reasoning(Some(HubEffort::High), None)),
        json!({"includeThoughts": true, "thinkingBudget": 24_576})
    );
    // A level generation still prefers the effort.
    let level = HubRequest {
        model: "gemini-3-pro-preview".into(),
        ..reasoning(Some(HubEffort::High), Some(4_096))
    };
    assert_eq!(
        thinking(&level),
        json!({"includeThoughts": true, "thinkingLevel": "high"})
    );
}

#[test]
fn history_may_call_a_tool_the_current_tool_table_no_longer_has() {
    // A tool dropped from the table (a workspace tool of a chat that is no longer bound) stays
    // in the older turns: the pair is encoded as written and only the declarations shrink.
    let mut req = HubRequest::user_prompt("gemini-2.5-pro", "list my requests");
    req.tools = vec![HubTool {
        name: "run_probe".into(),
        description: None,
        parameters: json!({"type": "object"}),
        strict: None,
    }];
    req.messages.push(HubMessage {
        role: HubRole::Assistant,
        content: vec![call("t1", "list_requests", "{}", None)],
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
    let body = encode(&req);
    let declarations = body["tools"][0]["functionDeclarations"].as_array().unwrap();
    assert_eq!(declarations.len(), 1);
    assert_eq!(declarations[0]["name"], "run_probe");
    assert_eq!(
        body["contents"][1]["parts"][0]["functionCall"]["name"],
        "list_requests"
    );
    // The result still finds its call's name although no declaration has it.
    assert_eq!(
        body["contents"][2]["parts"][0]["functionResponse"]["name"],
        "list_requests"
    );
}
