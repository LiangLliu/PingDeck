//! Request and response encoding.

use super::*;
use crate::llm::hub::{CacheBreakpoint, CacheHint, HubCitation};

fn with_reasoning(style: ReasoningStyle, reasoning: HubReasoning) -> Value {
    let quirks = Quirks {
        reasoning_request: Some(style),
        ..Quirks::default()
    };
    encode(
        &HubRequest {
            reasoning: Some(reasoning),
            ..base()
        },
        &quirks,
    )
}

fn off() -> HubReasoning {
    HubReasoning {
        enabled: Some(false),
        ..HubReasoning::default()
    }
}

#[test]
fn encode_writes_messages_sampling_and_options() {
    let req = HubRequest {
        system: Some("be brief".into()),
        messages: vec![
            HubMessage::user("hi"),
            HubMessage::assistant("hello"),
            HubMessage::user("again"),
        ],
        temperature: Some(0.5),
        top_p: Some(0.9),
        top_k: Some(40),
        presence_penalty: Some(0.1),
        frequency_penalty: Some(0.2),
        max_tokens: Some(100),
        stop: vec!["END".into()],
        seed: Some(7),
        user: Some("u1".into()),
        parallel_tool_calls: Some(false),
        ..base()
    };
    assert_eq!(
        encode(&req, &Quirks::default()),
        json!({
            "model": "gpt-x",
            "messages": [
                {"role": "system", "content": "be brief"},
                {"role": "user", "content": "hi"},
                {"role": "assistant", "content": "hello"},
                {"role": "user", "content": "again"},
            ],
            "temperature": 0.5, "top_p": 0.9, "top_k": 40,
            "presence_penalty": 0.1, "frequency_penalty": 0.2,
            "max_tokens": 100, "stop": ["END"], "seed": 7, "user": "u1",
            "parallel_tool_calls": false,
        })
    );
}

#[test]
fn a_bare_prompt_encodes_only_model_and_messages() {
    assert_eq!(
        encode(&base(), &Quirks::default()),
        json!({"model": "gpt-x", "messages": [{"role": "user", "content": "hi"}]})
    );
}

#[test]
fn tool_requests_encode_choice_format_text_results_and_blank_arguments() {
    let tool_req = HubRequest {
        messages: vec![
            HubMessage::user("weather?"),
            HubMessage {
                role: HubRole::Assistant,
                content: vec![
                    call("c1", "weather", "{\"city\":\"Paris\"}"),
                    call("c2", "time", ""),
                ],
            },
            HubMessage {
                role: HubRole::Tool,
                content: vec![
                    HubPart::ToolResult {
                        call_id: "c1".into(),
                        name: Some("weather".into()),
                        content: vec![
                            HubPart::text("sunny"),
                            HubPart::Image {
                                image: HubImage::Url {
                                    url: "https://x/y.png".into(),
                                },
                            },
                        ],
                        is_error: true,
                    },
                    HubPart::ToolResult {
                        call_id: "c2".into(),
                        name: None,
                        content: vec![HubPart::text("noon")],
                        is_error: false,
                    },
                ],
            },
        ],
        tools: vec![HubTool {
            name: "weather".into(),
            description: Some("get weather".into()),
            parameters: json!({"type": "object"}),
            strict: Some(true),
        }],
        tool_choice: Some(HubToolChoice::Tool {
            name: "weather".into(),
        }),
        response_format: Some(HubResponseFormat::JsonSchema {
            name: "out".into(),
            schema: json!({"type": "object"}),
            strict: Some(true),
        }),
        ..base()
    };
    let body = encode(&tool_req, &Quirks::default());
    assert_eq!(
        body["tool_choice"],
        json!({"type": "function", "function": {"name": "weather"}})
    );
    assert_eq!(body["response_format"]["type"], "json_schema");
    assert_eq!(body["messages"][2]["content"], "sunny\n[image]");
    assert_eq!(
        body["messages"][1]["tool_calls"][1]["function"]["arguments"],
        "{}"
    );
}

#[test]
fn reasoning_styles_write_their_dialect() {
    use ReasoningStyle::*;
    let budget = HubReasoning {
        budget_tokens: Some(2048),
        ..HubReasoning::default()
    };
    assert_eq!(
        with_reasoning(ReasoningEffort, effort(HubEffort::Max))["reasoning_effort"],
        "max"
    );
    assert_eq!(
        with_reasoning(ReasoningEffort, off())["reasoning_effort"],
        "none"
    );
    assert_eq!(
        with_reasoning(ReasoningObject, effort(HubEffort::Low))["reasoning"],
        json!({"effort": "low"})
    );
    assert_eq!(
        with_reasoning(ReasoningObject, budget.clone())["reasoning"],
        json!({"max_tokens": 2048})
    );
    assert_eq!(
        with_reasoning(ThinkingType, effort(HubEffort::High))["thinking"],
        json!({"type": "enabled"})
    );
    assert_eq!(
        with_reasoning(EnableThinking, budget)["thinking_budget"],
        2048
    );
    assert_eq!(
        with_reasoning(ChatTemplateKwargs, off())["chat_template_kwargs"],
        json!({"enable_thinking": false})
    );
}

#[test]
fn reasoning_models_drop_sampling_and_take_max_completion_tokens() {
    let req = HubRequest {
        temperature: Some(1.0),
        top_p: Some(1.0),
        max_tokens: Some(64),
        ..base()
    };
    let body = encode(
        &req,
        &Quirks {
            drop_sampling_params: Some(true),
            reasoning_request: Some(ReasoningStyle::ReasoningEffort),
            ..Quirks::default()
        },
    );
    assert!(body.get("temperature").is_none());
    assert_eq!(body["max_completion_tokens"], 64);
}

#[test]
fn url_documents_are_unsupported_and_unresolved_images_invalid() {
    let url_doc = HubPart::Document {
        document: HubDocument {
            media_type: "application/pdf".into(),
            name: None,
            source: HubDocumentSource::Url {
                url: "https://x/a.pdf".into(),
            },
        },
    };
    let unresolved_img = HubPart::Image {
        image: HubImage::AttachmentRef {
            attachment_id: "a1".into(),
            media_type: "image/png".into(),
        },
    };
    let error_of = |part: HubPart| {
        try_encode(
            &HubRequest {
                messages: vec![HubMessage {
                    role: HubRole::User,
                    content: vec![part],
                }],
                ..base()
            },
            &Quirks::default(),
        )
        .unwrap_err()
    };
    assert!(matches!(error_of(url_doc), AppError::Unsupported { .. }));
    assert!(matches!(
        error_of(unresolved_img),
        AppError::Validation { .. }
    ));
}

#[test]
fn the_cache_key_is_sent_as_prompt_cache_key_and_nothing_else_is_marked() {
    let mut req = base();
    assert!(
        encode(&req, &Quirks::default())
            .get("prompt_cache_key")
            .is_none()
    );
    req.cache = Some(CacheHint {
        breakpoints: vec![
            CacheBreakpoint::SystemAndTools,
            CacheBreakpoint::LastMessage,
        ],
        key: Some("session-1".into()),
    });
    let chat = encode(&req, &Quirks::default());
    assert_eq!(chat["prompt_cache_key"], "session-1");
    assert!(!chat.to_string().contains("cache_control"));
}

fn user_turn(parts: Vec<HubPart>) -> Value {
    let req = HubRequest {
        messages: vec![HubMessage {
            role: HubRole::User,
            content: parts,
        }],
        ..base()
    };
    encode(&req, &Quirks::default())["messages"].clone()
}

fn tool_result(call_id: &str) -> HubPart {
    HubPart::ToolResult {
        call_id: call_id.into(),
        name: None,
        content: vec![HubPart::text("r")],
        is_error: false,
    }
}

#[test]
fn a_text_only_user_turn_folds_into_one_string() {
    assert_eq!(
        user_turn(vec![HubPart::text("a"), HubPart::text("b")]),
        json!([{"role": "user", "content": "ab"}])
    );
}

#[test]
fn a_user_turn_with_an_image_keeps_the_part_array() {
    let image = HubPart::Image {
        image: HubImage::Url {
            url: "https://x/i.png".into(),
        },
    };
    assert_eq!(
        user_turn(vec![HubPart::text("a"), image, HubPart::text("b")]),
        json!([{"role": "user", "content": [
            {"type": "text", "text": "a"},
            {"type": "image_url", "image_url": {"url": "https://x/i.png"}},
            {"type": "text", "text": "b"},
        ]}])
    );
}

#[test]
fn tool_results_come_before_the_user_message_of_the_same_turn() {
    assert_eq!(
        user_turn(vec![
            HubPart::text("q"),
            tool_result("c1"),
            tool_result("c2")
        ]),
        json!([
            {"role": "tool", "tool_call_id": "c1", "content": "r"},
            {"role": "tool", "tool_call_id": "c2", "content": "r"},
            {"role": "user", "content": "q"},
        ])
    );
}

#[test]
fn a_turn_of_only_tool_results_writes_no_user_message() {
    assert_eq!(
        user_turn(vec![tool_result("c1")]),
        json!([{"role": "tool", "tool_call_id": "c1", "content": "r"}])
    );
    assert_eq!(user_turn(vec![]), json!([]));
}

#[test]
fn a_lone_empty_text_still_writes_a_user_message() {
    assert_eq!(
        user_turn(vec![HubPart::text("")]),
        json!([{"role": "user", "content": ""}])
    );
}

#[test]
fn user_turn_errors_follow_part_order() {
    let image = HubPart::Image {
        image: HubImage::AttachmentRef {
            attachment_id: "i".into(),
            media_type: "image/png".into(),
        },
    };
    let document = HubPart::Document {
        document: HubDocument {
            media_type: "text/plain".into(),
            name: None,
            source: HubDocumentSource::AttachmentRef {
                attachment_id: "d".into(),
            },
        },
    };
    let req = HubRequest {
        messages: vec![HubMessage {
            role: HubRole::User,
            content: vec![
                image,
                HubPart::ToolResult {
                    call_id: "c".into(),
                    name: None,
                    content: vec![document],
                    is_error: false,
                },
            ],
        }],
        ..base()
    };
    let err = try_encode(&req, &Quirks::default()).unwrap_err();
    assert!(matches!(err, AppError::Validation { .. }));
    assert_eq!(
        err.message(),
        "attachment image must be resolved before the request is encoded"
    );
}

/// `encode_response` without its clock-dependent `created` field.
fn encode_response(response: &HubResponse) -> String {
    let mut body = OpenaiChat.encode_response(response);
    body.as_object_mut().unwrap().remove("created");
    body.to_string()
}

fn reasoning(text: &str) -> HubPart {
    HubPart::Reasoning {
        text: text.into(),
        signature: None,
        encrypted: None,
        id: None,
        origin: None,
    }
}

fn cited_text(text: &str, url: &str, start: u32, end: u32) -> HubPart {
    HubPart::Text {
        text: text.into(),
        signature: None,
        origin: None,
        citations: vec![HubCitation {
            url: url.into(),
            title: "T".into(),
            snippet: None,
            start_index: Some(start),
            end_index: Some(end),
        }],
    }
}

#[test]
fn golden_encode_response_joins_parts_and_offsets_annotations() {
    let response = HubResponse {
        id: String::new(),
        model: "m".into(),
        content: vec![
            reasoning("r1"),
            cited_text("中😀", "https://a", 0, 1),
            reasoning("r2"),
            cited_text("ab", "https://b", 1, 2),
            call("c1", "f", ""),
            call("c2", "g", "{\"a\":1}"),
        ],
        finish: HubFinish::Pause,
        usage: HubUsage {
            input_tokens: Some(3),
            output_tokens: Some(2),
            reasoning_tokens: Some(1),
            cached_input_tokens: Some(1),
            ..HubUsage::default()
        },
    };
    assert_eq!(
        test_support::canonical_json_str(&encode_response(&response)),
        r#"{"choices":[{"finish_reason":"length","index":0,"message":{"annotations":[{"type":"url_citation","url_citation":{"end_index":1,"start_index":0,"title":"T","url":"https://a"}},{"type":"url_citation","url_citation":{"end_index":5,"start_index":4,"title":"T","url":"https://b"}}],"content":"中😀ab","reasoning_content":"r1r2","role":"assistant","tool_calls":[{"function":{"arguments":"{}","name":"f"},"id":"c1","type":"function"},{"function":{"arguments":"{\"a\":1}","name":"g"},"id":"c2","type":"function"}]}}],"id":"chatcmpl-pingdeck","model":"m","object":"chat.completion","usage":{"completion_tokens":2,"completion_tokens_details":{"reasoning_tokens":1},"prompt_tokens":3,"prompt_tokens_details":{"cached_tokens":1},"total_tokens":5}}"#
    );
}

#[test]
fn golden_encode_response_of_an_empty_answer() {
    let response = HubResponse {
        id: "x".into(),
        model: String::new(),
        content: vec![],
        finish: HubFinish::Other("weird".into()),
        usage: HubUsage::default(),
    };
    assert_eq!(
        test_support::canonical_json_str(&encode_response(&response)),
        r#"{"choices":[{"finish_reason":"weird","index":0,"message":{"content":null,"role":"assistant"}}],"id":"x","model":"","object":"chat.completion","usage":{"completion_tokens":0,"prompt_tokens":0,"total_tokens":0}}"#
    );
}

#[test]
fn history_may_call_a_tool_the_current_tool_table_no_longer_has() {
    // A tool dropped from the table (a workspace tool of a chat that is no longer bound) stays
    // in the older turns: the pair is encoded as written and only the table shrinks.
    let mut req = base();
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
        content: vec![crate::llm::hub::HubPart::ToolResult {
            call_id: "t1".into(),
            name: None,
            content: vec![crate::llm::hub::HubPart::text("not bound")],
            is_error: true,
        }],
    });
    let body = encode(&req, &Quirks::default());
    assert_eq!(body["tools"].as_array().unwrap().len(), 1);
    assert_eq!(body["tools"][0]["function"]["name"], "run_probe");
    let messages = body["messages"].as_array().unwrap();
    assert_eq!(
        messages[1]["tool_calls"][0]["function"]["name"],
        "list_requests"
    );
    assert_eq!(messages[2]["role"], "tool");
    assert_eq!(messages[2]["tool_call_id"], "t1");
}
