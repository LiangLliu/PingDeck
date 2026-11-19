//! Response and request decoding.

use super::*;

fn sample_request_body() -> Value {
    json!({
        "model": "claude-x",
        "max_tokens": 1000,
        "system": [{"type": "text", "text": "one"}, {"type": "text", "text": "two"}],
        "stream": true,
        "thinking": {"type": "enabled", "budget_tokens": 2048},
        "output_config": {"effort": "high", "format": {"type": "json_schema", "schema": {"type": "object"}}},
        "tools": [
            {"name": "f", "description": "d", "input_schema": {"type": "object", "properties": {}}},
            {"type": "web_search_20250305", "name": "web_search"},
        ],
        "tool_choice": {"type": "any", "disable_parallel_tool_use": true},
        "messages": [
            {"role": "user", "content": "hi"},
            {"role": "assistant", "content": [
                {"type": "thinking", "thinking": "hm", "signature": "s"},
                {"type": "tool_use", "id": "tu1", "name": "f", "input": {"a": 1}},
            ]},
            {"role": "user", "content": [
                {"type": "tool_result", "tool_use_id": "tu1", "content": "done", "is_error": true},
                {"type": "text", "text": "and then?"},
            ]},
        ],
    })
}

#[test]
fn decode_response_totals_cached_input() {
    let body = json!({
        "id": "msg_1", "type": "message", "role": "assistant", "model": "claude-x",
        "content": [
            {"type": "thinking", "thinking": "hm", "signature": "sg"},
            {"type": "redacted_thinking", "data": "zzz"},
            {"type": "text", "text": "hello"},
            {"type": "tool_use", "id": "tu", "name": "f", "input": {"a": 1}},
        ],
        "stop_reason": "tool_use",
        "usage": {"input_tokens": 10, "cache_read_input_tokens": 100, "cache_creation_input_tokens": 5, "output_tokens": 7},
    });
    let response = AnthropicMessages.decode_response(&body).unwrap();
    assert_eq!(response.finish, HubFinish::ToolCalls);
    assert_eq!(response.usage.input_tokens, Some(115));
}

#[test]
fn decode_request_reads_system_tools_and_messages() {
    let req_body = sample_request_body();
    let req = AnthropicMessages
        .decode_request(&req_body, &InboundHint::default())
        .unwrap();
    assert_eq!(req.system.as_deref(), Some("one\n\ntwo"));
    assert_eq!(req.tools.len(), 1);
    assert_eq!(req.native_tools.len(), 1);
    assert_eq!(req.messages.len(), 4);
}

#[test]
fn pause_turn_asks_to_continue_and_thinking_tokens_are_reasoning() {
    // `pause_turn` is no normal end; `thinking_tokens` count as reasoning.
    let body = json!({
        "id": "m", "type": "message", "role": "assistant", "model": "claude-opus-4-7",
        "content": [{"type": "text", "text": "part"}],
        "stop_reason": "pause_turn",
        "usage": {"input_tokens": 5, "output_tokens": 40, "thinking_tokens": 30}
    });
    let response = AnthropicMessages.decode_response(&body).unwrap();
    assert_eq!(response.finish, HubFinish::Pause);
    assert_eq!(response.usage.reasoning_tokens, Some(30));
    assert_eq!(
        AnthropicMessages.encode_response(&response)["stop_reason"],
        "pause_turn"
    );
}

#[test]
fn decode_request_reads_roles_tool_names_reasoning_format_and_flags() {
    let req = AnthropicMessages
        .decode_request(&sample_request_body(), &InboundHint::default())
        .unwrap();
    let roles: Vec<HubRole> = req.messages.iter().map(|m| m.role).collect();
    assert_eq!(
        roles,
        [
            HubRole::User,
            HubRole::Assistant,
            HubRole::Tool,
            HubRole::User
        ]
    );
    // The tool result names its call through the earlier tool_use id.
    assert!(matches!(
        &req.messages[2].content[..],
        [HubPart::ToolResult { call_id, name: Some(name), is_error: true, .. }]
            if call_id == "tu1" && name == "f"
    ));
    assert_eq!(
        req.reasoning,
        Some(HubReasoning {
            enabled: Some(true),
            budget_tokens: Some(2048),
            effort: Some(HubEffort::High),
        })
    );
    assert_eq!(
        req.response_format,
        Some(HubResponseFormat::JsonSchema {
            name: "response".into(),
            schema: json!({"type": "object"}),
            strict: None,
        })
    );
    assert_eq!(req.parallel_tool_calls, Some(false));
    assert_eq!(req.tool_choice, Some(HubToolChoice::Required));
    assert!(req.stream);
    assert_eq!(req.max_tokens, Some(1000));
}
