//! The wire plan: the provider's URL and credential, the user's headers, the masked view.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::time::Duration;

use super::super::types::{AiRunSpec, AiRunStatus, Destination};
use super::super::wire::{masked_view, missing_endpoint, plan, protocol_label};
use crate::llm::router::Target;
use crate::llm::wire::{AuthStyle, EndpointUrlMode, Protocol, ProviderEndpoint, Quirks};

const KEY: &str = "sk-wire-secret-0123456789";

fn target(auth: AuthStyle, endpoints: &[(Protocol, &str)], quirks: Quirks) -> Target {
    Target {
        provider_id: "p".to_owned(),
        provider_name: "DeepSeek".to_owned(),
        key_id: "k".to_owned(),
        secret: KEY.to_owned(),
        base_url: "https://api.deepseek.com".to_owned(),
        endpoints: endpoints
            .iter()
            .map(|(protocol, prefix)| ProviderEndpoint {
                protocol: *protocol,
                path_prefix: (*prefix).to_owned(),
                auth: None,
                url_mode: EndpointUrlMode::Standard,
                enabled: true,
            })
            .collect(),
        auth,
        quirks,
        primary_protocol: None,
        proxy_url: String::new(),
        model: "deepseek-v4-flash".to_owned(),
        key_cooldown: true,
        max_output: None,
    }
}

fn spec(
    protocol: Protocol,
    stream: bool,
    destination: Destination,
    headers: &[(&str, &str)],
) -> AiRunSpec {
    AiRunSpec {
        run_id: "run_1".to_owned(),
        protocol,
        model: "deepseek-v4-flash".to_owned(),
        stream,
        destination,
        headers: headers
            .iter()
            .map(|(name, value)| ((*name).to_owned(), (*value).to_owned()))
            .collect(),
        dropped_headers: Vec::new(),
        body: "{}".to_owned(),
        shown_url: "pingdeck-provider://deepseek/chat/completions".to_owned(),
        timeout: Duration::from_secs(5),
        keep_exchange: true,
        price: crate::llm::usage::PriceResolution::default(),
        secrets: vec!["user-secret-value".to_owned()],
        max_response_bytes: 1024,
        attachments_dir: None,
    }
}

fn provider(
    protocol: Protocol,
    stream: bool,
    auth: AuthStyle,
    prefix: &str,
    headers: &[(&str, &str)],
) -> AiRunSpec {
    spec(
        protocol,
        stream,
        Destination::Provider(Box::new(target(
            auth,
            &[(protocol, prefix)],
            Quirks::default(),
        ))),
        headers,
    )
}

fn value<'a>(headers: &'a [(String, String)], name: &str) -> Option<&'a str> {
    headers
        .iter()
        .find(|(own, _)| own.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

#[test]
fn the_url_is_the_providers_endpoint_for_the_protocol() {
    let url = |protocol, stream, prefix| {
        plan(&provider(protocol, stream, AuthStyle::Bearer, prefix, &[]))
            .unwrap()
            .url
    };
    assert_eq!(
        url(Protocol::OpenaiChat, false, ""),
        "https://api.deepseek.com/chat/completions"
    );
    assert_eq!(
        url(Protocol::OpenaiChat, true, "/v1"),
        "https://api.deepseek.com/v1/chat/completions"
    );
    assert_eq!(
        url(Protocol::OpenaiResponses, false, "/v1"),
        "https://api.deepseek.com/v1/responses"
    );
    assert_eq!(
        url(Protocol::AnthropicMessages, false, "/anthropic/v1"),
        "https://api.deepseek.com/anthropic/v1/messages"
    );
    assert_eq!(
        url(Protocol::Embeddings, false, "/v1"),
        "https://api.deepseek.com/v1/embeddings"
    );
    assert_eq!(
        url(Protocol::Gemini, false, "/v1beta"),
        "https://api.deepseek.com/v1beta/models/deepseek-v4-flash:generateContent"
    );
    assert_eq!(
        url(Protocol::Gemini, true, "/v1beta"),
        "https://api.deepseek.com/v1beta/models/deepseek-v4-flash:streamGenerateContent?alt=sse"
    );
}

#[test]
fn each_auth_style_injects_its_own_header_and_nothing_else_authenticates() {
    let headers = |protocol, auth| {
        plan(&provider(protocol, false, auth, "/v1", &[]))
            .unwrap()
            .headers
    };
    let bearer = headers(Protocol::OpenaiChat, AuthStyle::Bearer);
    assert_eq!(
        value(&bearer, "authorization"),
        Some(&*format!("Bearer {KEY}"))
    );
    assert_eq!(value(&bearer, "x-api-key"), None);
    let messages = headers(Protocol::AnthropicMessages, AuthStyle::XApiKey);
    assert_eq!(value(&messages, "x-api-key"), Some(KEY));
    assert_eq!(value(&messages, "authorization"), None);
    assert_eq!(value(&messages, "anthropic-version"), Some("2023-06-01"));
    let gemini = headers(Protocol::Gemini, AuthStyle::XGoogApiKey);
    assert_eq!(value(&gemini, "x-goog-api-key"), Some(KEY));
    let raw = headers(Protocol::OpenaiChat, AuthStyle::RawAuthorization);
    assert_eq!(value(&raw, "authorization"), Some(KEY));
    let none = headers(Protocol::OpenaiChat, AuthStyle::None);
    assert_eq!(value(&none, "authorization"), None);
}

#[test]
fn defaults_are_added_only_when_the_user_did_not_set_them() {
    let planned = plan(&provider(
        Protocol::OpenaiChat,
        true,
        AuthStyle::Bearer,
        "/v1",
        &[],
    ))
    .unwrap();
    assert_eq!(
        value(&planned.headers, "content-type"),
        Some("application/json")
    );
    assert_eq!(value(&planned.headers, "accept"), Some("text/event-stream"));
    assert_eq!(value(&planned.headers, "connection"), Some("close"));
    let planned = plan(&provider(
        Protocol::OpenaiChat,
        false,
        AuthStyle::Bearer,
        "/v1",
        &[
            ("Accept", "text/plain"),
            ("Content-Type", "application/json; charset=utf-8"),
        ],
    ))
    .unwrap();
    assert_eq!(value(&planned.headers, "accept"), Some("text/plain"));
    assert_eq!(
        value(&planned.headers, "content-type"),
        Some("application/json; charset=utf-8")
    );
    assert_eq!(
        planned
            .headers
            .iter()
            .filter(|(name, _)| name.eq_ignore_ascii_case("accept"))
            .count(),
        1
    );
}

#[test]
fn user_headers_stay_but_an_injected_name_wins_a_collision() {
    let mut quirks = Quirks::default();
    quirks
        .extra_headers
        .insert("x-org".to_owned(), "org-1".to_owned());
    let spec = spec(
        Protocol::AnthropicMessages,
        false,
        Destination::Provider(Box::new(target(
            AuthStyle::XApiKey,
            &[(Protocol::AnthropicMessages, "/v1")],
            quirks,
        ))),
        &[
            ("X-Trace-Id", "t"),
            ("X-Org", "mine"),
            ("anthropic-version", "1999"),
        ],
    );
    let planned = plan(&spec).unwrap();
    assert_eq!(value(&planned.headers, "x-trace-id"), Some("t"));
    assert_eq!(value(&planned.headers, "x-org"), Some("org-1"));
    assert_eq!(
        value(&planned.headers, "anthropic-version"),
        Some("2023-06-01")
    );
}

#[test]
fn a_provider_without_the_endpoint_is_refused_naming_it() {
    let spec = provider(Protocol::OpenaiChat, false, AuthStyle::Bearer, "/v1", &[]);
    let mut other = spec;
    other.protocol = Protocol::OpenaiResponses;
    let refusal = plan(&other).err().unwrap();
    assert_eq!(refusal.status, AiRunStatus::Unsupported);
    assert_eq!(refusal.message, "DeepSeek has no Responses endpoint");
    assert_eq!(
        missing_endpoint("Moonshot", Protocol::Gemini),
        "Moonshot has no Gemini endpoint"
    );
    assert_eq!(protocol_label(Protocol::OpenaiChat), "Chat Completions");
    assert_eq!(protocol_label(Protocol::AnthropicMessages), "Messages");
}

#[test]
fn headers_that_cannot_be_sent_are_refused() {
    let bad_name = plan(&provider(
        Protocol::OpenaiChat,
        false,
        AuthStyle::Bearer,
        "/v1",
        &[("bad name", "x")],
    ));
    assert!(
        bad_name
            .err()
            .unwrap()
            .message
            .contains("not a valid header name")
    );
    let bad_value = plan(&provider(
        Protocol::OpenaiChat,
        false,
        AuthStyle::Bearer,
        "/v1",
        &[("x-a", "line\nbreak")],
    ));
    assert!(bad_value.err().unwrap().message.contains("cannot be sent"));
}

#[test]
fn the_masked_view_hides_credentials_injected_headers_and_secret_variables() {
    let mut quirks = Quirks::default();
    quirks
        .extra_headers
        .insert("x-org".to_owned(), "org-secret".to_owned());
    let spec = spec(
        Protocol::OpenaiChat,
        false,
        Destination::Provider(Box::new(target(
            AuthStyle::Bearer,
            &[(Protocol::OpenaiChat, "/v1")],
            quirks,
        ))),
        &[
            ("X-Note", "has user-secret-value inside"),
            ("X-Trace-Id", "t-1"),
        ],
    );
    let planned = plan(&spec).unwrap();
    let view = masked_view(&planned);
    let get = |name: &str| view.iter().find(|h| h.name == name).unwrap().value.clone();
    assert_eq!(get("authorization"), "Bearer [redacted]");
    assert_eq!(get("x-org"), "[redacted]");
    assert!(!get("X-Note").contains("user-secret-value"));
    assert_eq!(get("X-Trace-Id"), "t-1");
    assert_eq!(get("content-type"), "application/json");
    assert!(
        !serde_json::to_string(&view.iter().map(|h| &h.value).collect::<Vec<_>>())
            .unwrap()
            .contains(KEY)
    );
}

#[test]
fn a_custom_target_sends_exactly_the_users_headers() {
    let spec = spec(
        Protocol::OpenaiChat,
        true,
        Destination::Custom {
            url: "http://127.0.0.1:1/x".to_owned(),
            http: crate::llm::safe_http::client::ReqwestSafeHttp::direct(),
        },
        &[
            ("Authorization", "Bearer typed"),
            ("x-api-key", "also typed"),
        ],
    );
    let planned = plan(&spec).unwrap();
    assert_eq!(planned.url, "http://127.0.0.1:1/x");
    assert_eq!(
        value(&planned.headers, "authorization"),
        Some("Bearer typed")
    );
    assert_eq!(value(&planned.headers, "x-api-key"), Some("also typed"));
    let view = masked_view(&planned);
    let auth = view.iter().find(|h| h.name == "Authorization").unwrap();
    assert_eq!(auth.value, "Bearer [redacted]");
    let api_key = view.iter().find(|h| h.name == "x-api-key").unwrap();
    assert_eq!(api_key.value, "[redacted]");
}

#[test]
fn a_messages_request_without_max_tokens_takes_the_models_output_cap() {
    // A 32k model is not cut at the codec's 8192 default; a caller's own cap still wins.
    let mut target = target(
        AuthStyle::XApiKey,
        &[(Protocol::AnthropicMessages, "/v1")],
        Quirks::default(),
    );
    let mut request = crate::llm::hub::HubRequest::user_prompt("x", "write a long essay");
    let encode = |target: &Target, request: &crate::llm::hub::HubRequest| {
        crate::llm::client::encode_for(target, Protocol::AnthropicMessages, request, false).unwrap()
            ["max_tokens"]
            .clone()
    };
    assert_eq!(encode(&target, &request), 8192);
    target.max_output = Some(32_000);
    assert_eq!(encode(&target, &request), 32_000);
    request.max_tokens = Some(100);
    assert_eq!(encode(&target, &request), 100);
}
