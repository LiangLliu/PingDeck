//! Response and request decoding.

use super::*;

#[test]
fn decode_response_reads_calls_reasoning_aliases_and_usage_details() {
    let body = json!({"choices": [{"message": {"content": "answer", "thinking": "why", "tool_calls": [
        {"id": "c1", "type": "function", "function": {"name": "a", "arguments": "{\"x\":1}"}},
        {"type": "function", "function": {"name": "b", "arguments": {"y": 2}}},
    ]}, "finish_reason": "tool_calls"}], "usage": {
        "prompt_tokens": 100, "completion_tokens": 40,
        "prompt_tokens_details": {"cached_tokens": 30},
        "completion_tokens_details": {"reasoning_tokens": 25}}});
    let response = OpenaiChat.decode_response(&body).unwrap();
    assert_eq!(
        response.tool_calls(),
        vec![("c1", "a", "{\"x\":1}"), ("call_1", "b", "{\"y\":2}")]
    );
    assert_eq!(response.usage.cached_input_tokens, Some(30));
    assert_eq!(response.usage.reasoning_tokens, Some(25));
}

#[test]
fn an_error_body_is_a_network_error_with_its_message() {
    let error = json!({"error": {"message": "bad key", "type": "invalid_request_error"}});
    let err = OpenaiChat.decode_response(&error).unwrap_err();
    assert!(matches!(err, AppError::Network { .. }));
    assert_eq!(err.message(), "bad key");
}

#[test]
fn decode_response_keeps_only_url_citations() {
    let wire = json!({"id": "r", "model": "m", "choices": [{"message": {
        "content": "see x",
        "annotations": [
            {"type": "url_citation", "url_citation": {"start_index": 4, "end_index": 5, "url": "https://x", "title": "T"}},
            {"type": "file_citation", "file_citation": {"file_id": "f"}}]},
        "finish_reason": "stop"}]});
    let resp = OpenaiChat.decode_response(&wire).unwrap();
    assert!(matches!(&resp.content[0], HubPart::Text { citations, .. } if citations.len() == 1));
}

fn rich_request() -> HubRequest {
    let body = json!({
        "model": "gpt-x",
        "messages": [
            {"role": "system", "content": "sys one"},
            {"role": "developer", "content": [{"type": "text", "text": "dev two"}]},
            {"role": "user", "content": [
                {"type": "text", "text": "look"},
                {"type": "image_url", "image_url": {"url": "data:image/png;base64,QUJD"}},
                {"type": "file", "file": {"filename": "a.pdf", "file_data": "data:application/pdf;base64,QUJD"}},
                {"type": "input_audio", "input_audio": {"data": "AAAA", "format": "wav"}}]},
            {"role": "assistant", "content": null, "reasoning_content": "plan", "tool_calls": [
                {"id": "c1", "type": "function", "function": {"name": "f", "arguments": "{\"a\":1}"}}]},
            {"role": "tool", "tool_call_id": "c1", "content": "42"},
        ],
        "tools": [{"type": "function", "function": {"name": "f", "parameters": {"type": "object"}, "strict": true}}],
        "web_search_options": {"search_context_size": "low"},
        "tool_choice": {"type": "function", "function": {"name": "f"}},
        "response_format": {"type": "json_schema", "json_schema": {"name": "o", "schema": {"type": "object"}, "strict": true}},
        "max_completion_tokens": 256, "stop": "END", "stream": true, "reasoning_effort": "high",
    });
    OpenaiChat
        .decode_request(&body, &InboundHint::default())
        .unwrap()
}

#[test]
fn decode_request_joins_system_messages_and_reads_media_tools_and_effort() {
    let req = rich_request();
    assert_eq!(req.system.as_deref(), Some("sys one\n\ndev two"));
    assert_eq!(req.messages[0].content.len(), 4);
    assert_eq!(req.native_tools.len(), 1);
    assert_eq!(req.reasoning, Some(effort(HubEffort::High)));
}

#[test]
fn decoded_media_and_web_search_encode_again() {
    let req = rich_request();
    let encoded = encode(
        &HubRequest {
            stream: false,
            messages: vec![req.messages[0].clone()],
            native_tools: req.native_tools.clone(),
            ..base()
        },
        &Quirks::default(),
    );
    assert_eq!(encoded["web_search_options"], json!({}));
    assert_eq!(
        encoded["messages"][0]["content"].as_array().unwrap().len(),
        4
    );
}
