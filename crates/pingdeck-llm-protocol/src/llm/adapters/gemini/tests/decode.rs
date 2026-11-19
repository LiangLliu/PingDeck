//! Response and request decoding, schema conversion and field lookup.

use super::*;
use crate::llm::adapters::gemini::schema::schema_from_gemini;

#[test]
fn decode_response_reports_tool_calls_and_output_with_thoughts() {
    let body = json!({
        "candidates": [{
            "content": {"role": "model", "parts": [
                {"text": "plan", "thought": true, "thoughtSignature": "s1"},
                {"text": "Hello"},
                {"inlineData": {"mimeType": "image/png", "data": "AA"}},
                {"functionCall": {"name": "a", "args": {"x": 1}}, "thoughtSignature": "s2"},
            ]},
            "finishReason": "STOP",
        }],
        "usageMetadata": {
            "promptTokenCount": 20, "cachedContentTokenCount": 5,
            "candidatesTokenCount": 7, "thoughtsTokenCount": 3, "totalTokenCount": 30,
        },
        "modelVersion": "gemini-x", "responseId": "r1",
    });
    let response = GeminiGenerate.decode_response(&body).unwrap();
    assert_eq!(response.finish, HubFinish::ToolCalls);
    assert_eq!(response.usage.output_tokens, Some(10));
}

#[test]
fn decode_request_reads_snake_case_aliases_and_pairs_responses_with_calls() {
    let req_body = json!({
        "system_instruction": {"parts": [{"text": "sys1"}, {"text": "sys2"}]},
        "contents": [
            {"role": "user", "parts": [
                {"text": "hi"},
                {"inline_data": {"mime_type": "image/png", "data": "AAAA"}},
                {"file_data": {"mime_type": "image/png", "file_uri": "https://x.io/a.png"}},
            ]},
            {"role": "model", "parts": [
                {"text": "t", "thought": true, "thought_signature": "sg"},
                {"function_call": {"name": "f", "args": {"a": 1}}},
                {"functionCall": {"name": "f", "args": {"a": 2}}},
            ]},
            {"role": "user", "parts": [
                {"function_response": {"name": "f", "response": {"result": "second"}}},
                {"functionResponse": {"name": "f", "response": {"x": 1}}},
            ]},
        ],
        "generation_config": {"max_output_tokens": 64, "thinking_config": {"thinking_level": "HIGH"}},
        "tools": [
            {"google_search": {}},
            {"function_declarations": [{"name": "f", "parameters": {"type": "OBJECT"}}]},
        ],
        "tool_config": {"function_calling_config": {"mode": "ANY", "allowed_function_names": ["f"]}},
    });
    let req = GeminiGenerate
        .decode_request(&req_body, &hint(true))
        .unwrap();
    assert_eq!(req.system.as_deref(), Some("sys1\n\nsys2"));
    assert_eq!(req.tools[0].parameters, json!({"type": "object"}));
    assert_eq!(req.native_tools.len(), 1);
    // The two unnamed `f` calls become call_0 and call_1; each response answers the most recent
    // open call of its name.
    let tool_result = |message: &HubMessage| match message {
        HubMessage {
            role: HubRole::Tool,
            content,
        } => match content.as_slice() {
            [
                HubPart::ToolResult {
                    call_id, content, ..
                },
            ] => (call_id.clone(), text_of(content)),
            other => panic!("not one tool result: {other:?}"),
        },
        other => panic!("not a tool message: {other:?}"),
    };
    assert_eq!(
        tool_result(&req.messages[2]),
        ("call_1".to_owned(), "second".to_owned())
    );
    assert_eq!(
        tool_result(&req.messages[3]),
        ("call_0".to_owned(), r#"{"x":1}"#.to_owned())
    );
}

#[test]
fn openapi_schemas_become_json_schema() {
    let converted = schema_from_gemini(&json!({
        "type": "OBJECT",
        "propertyOrdering": ["name"],
        "properties": {
            "name": {"type": "STRING", "nullable": true, "example": "x"},
            "tags": {"type": "ARRAY", "minItems": "1", "items": {"type": "INTEGER"}},
        }
    }));
    assert_eq!(
        converted,
        json!({
            "type": "object",
            "properties": {
                "name": {"type": ["string", "null"]},
                "tags": {"type": "array", "minItems": 1, "items": {"type": "integer"}},
            }
        })
    );
}

#[test]
fn a_grounded_response_adds_a_search_call_and_cites_the_text() {
    let grounded = json!({"candidates": [{
        "content": {"role": "model", "parts": [{"text": "Rust 1.0 shipped in 2015."}]},
        "finishReason": "STOP",
        "groundingMetadata": {
            "webSearchQueries": ["rust 1.0 release"],
            "groundingChunks": [{"web": {"uri": "https://a.example/rust", "title": "A"}}],
            "groundingSupports": [{"segment": {"endIndex": 4, "text": "Rust"}, "groundingChunkIndices": [0]}],
        },
    }]});
    let resp = GeminiGenerate.decode_response(&grounded).unwrap();
    assert_eq!(resp.content.len(), 2);
    assert!(matches!(
        &resp.content[0],
        HubPart::ServerToolUse { id, name, .. } if id == "grounding_1" && name == "web_search"
    ));
    let HubPart::Text { citations, .. } = &resp.content[1] else {
        panic!("not a text part: {:?}", resp.content[1]);
    };
    assert_eq!(citations.len(), 1);
    // A missing startIndex means 0 (proto3 leaves out zero values).
    assert_eq!(citations[0].start_index, Some(0));
    assert_eq!(citations[0].end_index, Some(4));
    assert_eq!(citations[0].snippet.as_deref(), Some("Rust"));
}

#[test]
fn field_reads_camel_case_then_the_snake_case_alias() {
    let value = json!({
        "promptTokenCount": 1,
        "cached_content_token_count": 2,
        "allowed_function_names": ["f"],
        "parameters_json_schema": {"type": "object"},
        "both": 3,
        "bothKeys": 4,
        "both_keys": 5,
        "a_very_long_field_name_that_does_not_fit_in_the_stack_buffer_of_sixty_four_bytes": 6,
    });
    assert_eq!(field(&value, "promptTokenCount"), Some(&json!(1)));
    assert_eq!(field(&value, "cachedContentTokenCount"), Some(&json!(2)));
    assert_eq!(field(&value, "allowedFunctionNames"), Some(&json!(["f"])));
    assert_eq!(
        field(&value, "parametersJsonSchema"),
        Some(&json!({"type": "object"}))
    );
    assert_eq!(field(&value, "both"), Some(&json!(3)));
    assert_eq!(field(&value, "bothKeys"), Some(&json!(4)));
    assert_eq!(field(&value, "missingKey"), None);
    assert_eq!(field(&value, "missing"), None);
    assert_eq!(field(&json!([1, 2]), "promptTokenCount"), None);
    assert_eq!(field(&json!("text"), "promptTokenCount"), None);
    let long = "aVeryLongFieldNameThatDoesNotFitInTheStackBufferOfSixtyFourBytes";
    assert!(snake_case(long).len() > SNAKE_KEY_BUFFER);
    assert_eq!(field(&value, long), Some(&json!(6)));
}
