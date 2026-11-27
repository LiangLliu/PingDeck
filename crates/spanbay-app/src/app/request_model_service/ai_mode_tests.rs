//! `mode` and `ai` survive the draft and URL-sync functions; the mode switch is reachable here.

use super::{
    UrlSyncEdit, UrlSyncInput, draft_normalize, request_ensure_sendable, request_switch_mode,
    sync_request_url,
};
use crate::llm::wire::Protocol;
use crate::workspace::KeyValue;
use crate::workspace::ai_request::{AiRequestConfig, AiTarget, RequestMode, non_routable_label};
use crate::workspace::models::Request;

fn provider_request() -> Request {
    let mut request = Request::new(
        "r1",
        "ws1",
        "Ask",
        non_routable_label("DeepSeek", "/chat/completions"),
    )
    .expect("request");
    request.mode = RequestMode::AiApi;
    request.ai = Some(AiRequestConfig {
        protocol: Protocol::OpenaiChat,
        target: AiTarget::Provider {
            provider_id: "p1".to_owned(),
            provider_name: "DeepSeek".to_owned(),
            base_url_hint: "https://api.deepseek.com".to_owned(),
            model: "deepseek-chat".to_owned(),
        },
        builder: Some(serde_json::json!({"system": "be brief"})),
        compare_models: Vec::new(),
    });
    request
}

#[test]
fn normalising_a_draft_keeps_mode_and_ai_settings() {
    let request = provider_request();
    let normalised = draft_normalize(request.clone(), "ws2").expect("normalises");
    assert_eq!(normalised.mode, RequestMode::AiApi);
    assert_eq!(normalised.ai, request.ai);

    // A tab in HTTP mode may still hold the settings it will restore; a draft never drops them.
    let mut http = request;
    http.mode = RequestMode::Http;
    assert!(
        draft_normalize(http, "ws2")
            .expect("normalises")
            .ai
            .is_some()
    );
}

#[test]
fn url_sync_leaves_the_provider_label_alone() {
    let request = provider_request();
    let sync = |edited| {
        sync_request_url(UrlSyncInput {
            url: request.url.clone(),
            query_params: Vec::new(),
            path_variables: Vec::new(),
            edited,
        })
    };
    for edited in [UrlSyncEdit::Url, UrlSyncEdit::Query] {
        let out = sync(edited);
        assert_eq!(out.url, request.url);
        assert!(out.query_params.is_empty() && out.path_variables.is_empty());
    }
    // Gemini's `:generateContent` suffix is not a path variable.
    let gemini = non_routable_label("Google", "/models/gemini-2.0-flash:generateContent");
    let out = sync_request_url(UrlSyncInput {
        url: gemini.clone(),
        query_params: vec![KeyValue {
            key: "alt".to_owned(),
            value: "sse".to_owned(),
            enabled: true,
            description: None,
        }],
        path_variables: Vec::new(),
        edited: UrlSyncEdit::Query,
    });
    assert_eq!(out.url, format!("{gemini}?alt=sse"));
    assert!(
        sync_request_url(UrlSyncInput {
            url: out.url,
            query_params: Vec::new(),
            path_variables: Vec::new(),
            edited: UrlSyncEdit::Url,
        })
        .path_variables
        .is_empty()
    );
}

#[test]
fn switching_to_http_is_refused_until_authentication_is_added() {
    let http = request_switch_mode(
        provider_request(),
        RequestMode::Http,
        Some("https://api.deepseek.com/chat/completions"),
    );
    assert_eq!(http.mode, RequestMode::Http);
    assert!(request_ensure_sendable(&http).is_err());
    let back = request_switch_mode(http, RequestMode::AiApi, None);
    assert_eq!(back.mode, RequestMode::AiApi);
    assert_eq!(back.ai, Some(AiRequestConfig::default()));
}
