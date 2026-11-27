//! A fake OpenAI-compatible upstream on `127.0.0.1:0` and the provider setup that points a state
//! at it, for the end-to-end run tests. One thread per connection; `Connection: close`.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::{Value, json};

use crate::app::AppState;
use crate::llm::store::{ModelSource, Provider, ProviderKey, ProviderModel};
use crate::llm::wire::{AuthStyle, Protocol, ProviderEndpoint, Quirks};
use spanbay_app_llm::{
    SaveProviderKeyInput, llm_key_save_impl, llm_model_save_impl, llm_provider_save_impl,
};

/// Model reference the chats target (`provider_id/model`).
pub(super) const ALIAS: &str = "fake/fake-model-1";
/// Model id the reference names at the fake upstream.
pub(super) const MODEL: &str = "fake-model-1";

use crate::test_upstream::sse;
pub(super) use crate::test_upstream::{Recorded, Reply, Upstream};

fn chunk(delta: Value, finish: Value) -> String {
    sse(
        &json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1,
        "model": MODEL, "choices": [{"index": 0, "delta": delta, "finish_reason": finish}]}),
    )
}

fn usage_frame() -> String {
    sse(
        &json!({"id": "chatcmpl-1", "object": "chat.completion.chunk", "created": 1,
        "model": MODEL, "choices": [],
        "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18}}),
    )
}

/// A streamed answer: `text` in two fragments, usage (11 in, 7 out).
pub(super) fn text_answer(text: &str) -> Reply {
    let split = text.len() / 2;
    let split = (0..=split)
        .rev()
        .find(|&i| text.is_char_boundary(i))
        .unwrap_or(0);
    let (head, tail) = text.split_at(split);
    Reply::Sse(vec![
        chunk(json!({"role": "assistant", "content": ""}), Value::Null),
        chunk(json!({"content": head}), Value::Null),
        chunk(json!({"content": tail}), Value::Null),
        chunk(json!({}), json!("stop")),
        usage_frame(),
        "data: [DONE]\n\n".to_owned(),
    ])
}

/// A streamed tool call.
pub(super) fn call_answer(call_id: &str, name: &str, arguments: &str) -> Reply {
    let (head, tail) = arguments.split_at(arguments.len() / 2);
    Reply::Sse(vec![
        chunk(json!({"role": "assistant", "content": ""}), Value::Null),
        chunk(
            json!({"tool_calls": [{"index": 0, "id": call_id, "type": "function",
                "function": {"name": name, "arguments": ""}}]}),
            Value::Null,
        ),
        chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": head}}]}),
            Value::Null,
        ),
        chunk(
            json!({"tool_calls": [{"index": 0, "function": {"arguments": tail}}]}),
            Value::Null,
        ),
        chunk(json!({}), json!("tool_calls")),
        usage_frame(),
        "data: [DONE]\n\n".to_owned(),
    ])
}

/// A non-streamed answer (compaction summaries).
pub(super) fn json_answer(text: &str) -> Reply {
    Reply::Json(
        200,
        json!({"id": "chatcmpl-1", "object": "chat.completion", "created": 1, "model": MODEL,
            "choices": [{"index": 0, "message": {"role": "assistant", "content": text},
                "finish_reason": "stop"}],
            "usage": {"prompt_tokens": 11, "completion_tokens": 7, "total_tokens": 18}}),
    )
}

/// The text of the last user message of a request.
pub(super) fn last_user_text(request: &Recorded) -> String {
    request.body["messages"]
        .as_array()
        .and_then(|messages| messages.iter().rfind(|m| m["role"] == "user"))
        .map(|message| match &message["content"] {
            Value::String(text) => text.clone(),
            other => other.to_string(),
        })
        .unwrap_or_default()
}

/// Whether the request already carries a tool result.
pub(super) fn has_tool_result(request: &Recorded) -> bool {
    request.body["messages"]
        .as_array()
        .is_some_and(|messages| messages.iter().any(|m| m["role"] == "tool"))
}

/// How many tool results the request already carries (the step a scripted model is at).
pub(super) fn tool_result_count(request: &Recorded) -> usize {
    request.body["messages"].as_array().map_or(0, |messages| {
        messages.iter().filter(|m| m["role"] == "tool").count()
    })
}

/// Whether the request is a `task` sub-agent's (its system prompt says so).
pub(super) fn is_subagent(request: &Recorded) -> bool {
    request.body["messages"][0]["content"]
        .as_str()
        .is_some_and(|system| system.contains("You are a sub-agent"))
}

/// A model that plays `steps` (tool name, arguments) one tool turn at a time, then answers
/// "done".
pub(super) fn scripted(
    steps: Vec<(&'static str, String)>,
) -> impl Fn(usize, &Recorded) -> Reply + Send + Sync + 'static {
    move |_, request| {
        let step = tool_result_count(request);
        match steps.get(step) {
            Some((name, arguments)) => call_answer(&format!("tc_{step}"), name, arguments),
            None => text_answer("done"),
        }
    }
}

/// Whether the request offers the tool `name`.
pub(super) fn offers(request: &Recorded, name: &str) -> bool {
    request.body["tools"]
        .as_array()
        .is_some_and(|tools| tools.iter().any(|tool| tool["function"]["name"] == name))
}

/// Saves a provider speaking OpenAI chat at the fake upstream, one key and the model [`ALIAS`]
/// names.
pub(super) fn configure_llm(state: &AppState, upstream_port: u16) -> Provider {
    let provider = llm_provider_save_impl(
        state,
        Provider {
            id: "fake".into(),
            name: "Fake upstream".into(),
            preset_id: None,
            category: "custom".into(),
            base_url: format!("http://127.0.0.1:{upstream_port}"),
            auth: AuthStyle::Bearer,
            endpoints: vec![ProviderEndpoint {
                protocol: Protocol::OpenaiChat,
                path_prefix: "/v1".into(),
                auth: None,
                url_mode: Default::default(),
                enabled: true,
            }],
            models_endpoint: None,
            quirks: Quirks::default(),
            proxy_url: String::new(),
            enabled: true,
            position: 0,
            notes: String::new(),
            created_at_ms: 0,
            updated_at_ms: 0,
            ..Default::default()
        },
    )
    .unwrap();
    let key: ProviderKey = llm_key_save_impl(
        state,
        SaveProviderKeyInput {
            id: "fake-key".into(),
            provider_id: provider.id.clone(),
            name: "Fake key".into(),
            secret: Some("sk-fake-upstream-secret".into()),
            enabled: true,
            position: 0,
            api_user_id: None,
            management_token: None,
            notes: None,
            group_name: None,
            protocols: None,
        },
    )
    .unwrap();
    assert_eq!(key.provider_id, provider.id);
    llm_model_save_impl(
        state,
        ProviderModel {
            provider_id: provider.id.clone(),
            model_id: MODEL.into(),
            source: ModelSource::Manual,
            role: None,
            context: None,
            max_output: None,
            vision: false,
            tools: true,
            notes: None,
            enabled: true,
            ..Default::default()
        },
    )
    .unwrap();
    provider
}
