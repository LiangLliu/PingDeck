//! Snippet tests: the shared `fixtures/snippet-cases.json` over every language plus targeted rules.

use std::collections::BTreeMap;

use serde::Deserialize;

use super::{
    PROVIDER_KEY_VARIABLE, SnippetLanguage, SnippetMode, SnippetOptions, request_code_snippet,
    request_code_snippet_impl, snippet_languages,
};
use crate::app::AppState;
use crate::error::AppError;
use crate::workspace::ai_request::{AiRequestConfig, AiTarget, RequestMode};
use crate::workspace::{HttpMethod, KeyValue, ProtocolKind, Request, RequestBody};

#[derive(Deserialize)]
struct Case {
    name: String,
    request: Request,
    snippets: BTreeMap<String, String>,
}

fn fixture() -> Vec<Case> {
    let source = include_str!("../../../../../fixtures/snippet-cases.json");
    serde_json::from_str(source).expect("fixture parses")
}

fn snippet(request: &Request, language: SnippetLanguage) -> String {
    request_code_snippet(request, language).expect("snippet renders")
}

#[test]
fn shared_fixture_pins_every_case_in_every_language() {
    let cases = fixture();
    assert!(cases.len() >= 50, "fixture needs at least 50 cases");
    for case in &cases {
        assert_eq!(
            case.snippets.len(),
            SnippetLanguage::ALL.len(),
            "{}: one snippet per language",
            case.name
        );
        for language in SnippetLanguage::ALL {
            let expected = case
                .snippets
                .get(language.id())
                .unwrap_or_else(|| panic!("{}: missing {}", case.name, language.id()));
            assert_eq!(
                &snippet(&case.request, language),
                expected,
                "{} / {}",
                case.name,
                language.id()
            );
        }
    }
}

#[test]
fn language_catalog_matches_the_serde_ids() {
    let catalog = snippet_languages();
    assert_eq!(catalog.len(), SnippetLanguage::ALL.len());
    for (entry, language) in catalog.iter().zip(SnippetLanguage::ALL) {
        let wire = serde_json::to_string(&language).expect("serializes");
        assert_eq!(wire, format!("\"{}\"", entry.id));
        assert_eq!(entry.label, language.label());
        let parsed: SnippetLanguage = serde_json::from_str(&wire).expect("parses back");
        assert_eq!(parsed, language);
    }
}

#[test]
fn websocket_and_grpc_requests_are_unsupported() {
    let mut request = Request::new("r", "w", "n", "ws://localhost").expect("request");
    for protocol in [ProtocolKind::WebSocket, ProtocolKind::Grpc] {
        request.protocol = protocol;
        assert!(matches!(
            request_code_snippet(&request, SnippetLanguage::Curl),
            Err(AppError::Unsupported { .. })
        ));
    }
}

#[test]
fn variables_stay_unexpanded_and_unencoded_in_query_and_form() {
    let mut request = Request::new("r", "w", "n", "https://x.test/p?a={{one}}").expect("request");
    request.query_params.push(crate::workspace::KeyValue {
        key: "{{k}}".into(),
        value: "{{v}} w".into(),
        enabled: true,
        description: None,
    });
    assert_eq!(
        snippet(&request, SnippetLanguage::Curl),
        "curl -L -X GET 'https://x.test/p?a={{one}}&{{k}}={{v}}%20w'"
    );
}

#[test]
fn curl_carries_own_network_options_and_imports_them_back() {
    use crate::importers::import_curl;
    use crate::workspace::models::{HttpVersionPreference, NetworkField};
    let mut request = Request::new("r", "w", "n", "https://x.test/p").expect("request");
    let options = &mut request.network_options;
    options.verify_tls = false;
    options.max_redirects = 3;
    options.total_timeout_ms = Some(2_500);
    options.proxy_url = Some("http://bob:hunter2@proxy.test:8080".into());
    options.http_version = HttpVersionPreference::Http2;
    let curl = snippet(&request, SnippetLanguage::Curl);
    assert!(curl.starts_with("curl -L -k -X GET"), "{curl}");
    assert!(curl.contains("--max-redirs 3"), "{curl}");
    assert!(curl.contains("--max-time 2.5"), "{curl}");
    assert!(curl.contains("--http2"), "{curl}");
    // The proxy password never reaches a snippet.
    assert!(!curl.contains("hunter2"), "{curl}");
    let back = import_curl(&curl, "w").expect("imports").requests.remove(0);
    let imported = &back.network_options;
    assert!(!imported.verify_tls && imported.follow_redirects);
    assert_eq!(imported.max_redirects, 3);
    assert_eq!(imported.total_timeout_ms, Some(2_500));
    assert_eq!(imported.http_version, HttpVersionPreference::Http2);
    assert_eq!(
        imported.proxy_url.as_deref(),
        Some("http://bob@proxy.test:8080")
    );

    // Fields that follow the global settings leave curl's defaults alone.
    request.network_options.inherit = vec![NetworkField::FollowRedirects, NetworkField::VerifyTls];
    let curl = snippet(&request, SnippetLanguage::Curl);
    assert!(curl.starts_with("curl -X GET"), "{curl}");
}

#[test]
fn oauth2_sends_a_bearer_placeholder_in_both_modes() {
    let state = AppState::open_in_memory().expect("state should open");
    let mut request = Request::new("r", "w", "n", "https://x.test/p").expect("request");
    request.auth = serde_json::from_value(serde_json::json!({ "OAuth2": {
        "grant_type": "ClientCredentials", "access_token_url": "https://auth.test/token",
        "client_id": "id", "client_secret_ref": "cs-literal", "username": "",
        "password_ref": "", "scope": "",
    }}))
    .expect("auth");
    let template = snippet(&request, SnippetLanguage::Curl);
    assert!(
        template.contains("-H 'Authorization: Bearer {{access_token}}'"),
        "{template}"
    );
    let values = request_code_snippet_impl(
        &state,
        &request,
        SnippetLanguage::Curl,
        &SnippetOptions {
            mode: SnippetMode::Values,
            environment_id: None,
        },
    )
    .expect("snippet");
    assert!(
        values.contains("'Authorization: Bearer '\"$ACCESS_TOKEN\""),
        "{values}"
    );
    assert!(!values.contains("cs-literal"), "{values}");
}

/// A state with one OpenAI-Chat provider `Relay` at `https://relay.test/v1`.
fn provider_state() -> (AppState, String) {
    let state = AppState::open_in_memory().expect("state should open");
    let provider = state
        .with_llm_connection(|connection| {
            crate::llm::store::save_provider(
                connection,
                &crate::llm::store::Provider {
                    id: "relay".to_owned(),
                    name: "Relay".to_owned(),
                    base_url: "https://relay.test".to_owned(),
                    endpoints: vec![crate::llm::wire::ProviderEndpoint {
                        protocol: crate::llm::wire::Protocol::OpenaiChat,
                        path_prefix: "/v1".to_owned(),
                        auth: None,
                        url_mode: Default::default(),
                        enabled: true,
                    }],
                    ..Default::default()
                },
                1,
            )
        })
        .expect("provider should save");
    (state, provider.id)
}

fn provider_request(provider_id: &str) -> Request {
    let mut request =
        Request::new("r", "w", "Chat", "https://provider.invalid").expect("request valid");
    request.method = HttpMethod::Post;
    request.mode = RequestMode::AiApi;
    request.ai = Some(AiRequestConfig {
        protocol: crate::llm::wire::Protocol::OpenaiChat,
        target: AiTarget::Provider {
            provider_id: provider_id.to_owned(),
            provider_name: "Relay".to_owned(),
            base_url_hint: String::new(),
            model: "gpt-x".to_owned(),
        },
        builder: None,
        compare_models: Vec::new(),
    });
    request.headers.push(KeyValue {
        key: "X-Trace".to_owned(),
        value: "1".to_owned(),
        enabled: true,
        description: None,
    });
    request.headers.push(KeyValue {
        key: "Authorization".to_owned(),
        value: "Bearer sk-live-123".to_owned(),
        enabled: true,
        description: None,
    });
    request.body = RequestBody::Json {
        content: "{\"model\":\"gpt-x\"}".to_owned(),
    };
    request
}

#[test]
fn a_provider_target_snippet_is_the_plain_export_with_the_key_variable() {
    let (state, provider_id) = provider_state();
    let request = provider_request(&provider_id);
    let curl = request_code_snippet_impl(
        &state,
        &request,
        SnippetLanguage::Curl,
        &SnippetOptions::default(),
    )
    .expect("snippet");
    assert!(
        curl.contains("https://relay.test/v1/chat/completions"),
        "{curl}"
    );
    assert!(curl.contains("-H 'X-Trace: 1'"), "{curl}");
    // The key's place is a shell variable, so this one header is double-quoted.
    assert!(
        curl.contains(&format!(
            "-H \"Authorization: Bearer {PROVIDER_KEY_VARIABLE}\""
        )),
        "{curl}"
    );
    assert!(curl.contains("--data '{\"model\":\"gpt-x\"}'"), "{curl}");
    assert!(!curl.contains("sk-live-123"), "{curl}");
    assert!(!curl.contains("provider.invalid"), "{curl}");

    let python = request_code_snippet_impl(
        &state,
        &request,
        SnippetLanguage::Python,
        &SnippetOptions::default(),
    )
    .expect("snippet");
    assert!(
        python.contains("https://relay.test/v1/chat/completions"),
        "{python}"
    );
    assert!(python.contains(PROVIDER_KEY_VARIABLE), "{python}");
    assert!(!python.contains("sk-live-123"), "{python}");
}

#[test]
fn a_provider_target_without_its_provider_or_endpoint_fails() {
    let (state, provider_id) = provider_state();
    let mut request = provider_request(&provider_id);
    if let Some(ai) = request.ai.as_mut() {
        ai.target = AiTarget::Provider {
            provider_id: "gone".to_owned(),
            provider_name: "Gone".to_owned(),
            base_url_hint: String::new(),
            model: "gpt-x".to_owned(),
        };
    }
    assert!(matches!(
        request_code_snippet_impl(
            &state,
            &request,
            SnippetLanguage::Curl,
            &SnippetOptions::default()
        ),
        Err(AppError::NotFound { .. })
    ));

    if let Some(ai) = request.ai.as_mut() {
        ai.target = AiTarget::Provider {
            provider_id,
            provider_name: "Relay".to_owned(),
            base_url_hint: String::new(),
            model: "gpt-x".to_owned(),
        };
        ai.protocol = crate::llm::wire::Protocol::AnthropicMessages;
    }
    let error = request_code_snippet_impl(
        &state,
        &request,
        SnippetLanguage::Curl,
        &SnippetOptions::default(),
    )
    .unwrap_err();
    assert!(
        error.to_string().contains("Relay has no Messages endpoint"),
        "{error}"
    );
}

#[test]
fn an_ai_custom_target_still_renders_its_own_request() {
    let state = AppState::open_in_memory().expect("state should open");
    let mut request =
        Request::new("r", "w", "Custom", "https://api.example.com/v1/chat").expect("request valid");
    request.mode = RequestMode::AiApi;
    request.ai = Some(AiRequestConfig {
        protocol: crate::llm::wire::Protocol::OpenaiChat,
        target: AiTarget::Custom {
            model: "llama".to_owned(),
        },
        builder: None,
        compare_models: Vec::new(),
    });
    let curl = request_code_snippet_impl(
        &state,
        &request,
        SnippetLanguage::Curl,
        &SnippetOptions::default(),
    )
    .expect("snippet");
    assert!(curl.contains("https://api.example.com/v1/chat"), "{curl}");
    assert!(!curl.contains(PROVIDER_KEY_VARIABLE), "{curl}");
}

#[test]
fn curl_snippets_import_back_unchanged() {
    use crate::importers::import_curl;
    use crate::workspace::{AuthConfig, FormPart};
    let base = import_curl(
        "curl -H \"X-Note: it's\" 'https://api.example.com/v1/users?page=2'",
        "ws_1",
    )
    .unwrap()
    .requests
    .remove(0);
    let field = |key: &str, value: &str| KeyValue {
        key: key.into(),
        value: value.into(),
        enabled: true,
        description: None,
    };
    let cases = [
        (
            RequestBody::Json {
                content: "{\"a\":\"x\\\"y 'z' C:\\\\tmp\\n\"}".into(),
            },
            AuthConfig::None,
        ),
        (
            RequestBody::FormUrlEncoded {
                fields: vec![field("q", "a b&c")],
            },
            AuthConfig::None,
        ),
        (
            RequestBody::RawText {
                language: None,
                content: "line1\nline2 $HOME `x`".into(),
            },
            AuthConfig::Basic {
                username: "u".into(),
                password_ref: "p:w".into(),
            },
        ),
        (
            RequestBody::Multipart {
                parts: vec![FormPart {
                    key: "k".into(),
                    value: "@not-a-file".into(),
                    file_path: None,
                    enabled: true,
                }],
            },
            AuthConfig::Digest {
                username: "alice".into(),
                password_ref: Some("s3cret".into()),
            },
        ),
    ];
    for (body, auth) in cases {
        let mut original = base.clone();
        original.body = body;
        original.auth = auth;
        let snippet = request_code_snippet(&original, SnippetLanguage::Curl).unwrap();
        let back = import_curl(&snippet, "ws_1").unwrap().requests.remove(0);
        let lines = |text: &str| {
            let mut lines: Vec<String> = text
                .lines()
                .map(|line| line.trim_end_matches(" \\").to_owned())
                .collect();
            lines.sort();
            lines
        };
        assert_eq!(
            lines(&request_code_snippet(&back, SnippetLanguage::Curl).unwrap()),
            lines(&snippet)
        );
        assert_eq!(
            (&back.auth, &back.query_params),
            (&original.auth, &original.query_params),
            "{snippet}"
        );
        if matches!(
            original.body,
            RequestBody::Json { .. } | RequestBody::Multipart { .. }
        ) {
            assert_eq!(back.body, original.body, "{snippet}");
        }
    }
}

const TOKEN: &str = "lit-collection-token-123";
const SECRET: &str = "env-secret-value-456";

/// A workspace whose collection holds a literal Bearer token, an environment with a plain
/// `base_url` and a secret `api_secret`, and a request inheriting the collection's auth.
fn inherited_auth_request() -> (AppState, Request) {
    let state = AppState::open_in_memory().expect("state should open");
    crate::app::workspace_create_impl(&state, "ws".into(), "W".into()).expect("workspace");
    let collection = serde_json::from_value(serde_json::json!({
        "id": "col", "workspace_id": "ws", "name": "Billing",
        "auth": { "Bearer": { "token_ref": TOKEN } },
    }))
    .expect("collection input");
    crate::app::collection_save_impl(&state, collection).expect("collection saves");
    let environment = serde_json::from_value(serde_json::json!({
        "id": "env", "workspace_id": "ws", "name": "Staging",
        "variables": [
            { "key": "base_url", "value": "https://api.test", "is_secret": false, "enabled": true },
            { "key": "api_secret", "value": SECRET, "is_secret": true, "enabled": true },
        ],
    }))
    .expect("environment input");
    crate::app::environment_save_impl(&state, environment).expect("environment saves");
    let mut request = Request::new("req", "ws", "List", "{{base_url}}/x?key=literal-key-789")
        .expect("request")
        .with_collection("col")
        .expect("collection");
    request.headers.push(KeyValue {
        key: "X-Trace".to_owned(),
        value: "{{api_secret}}".to_owned(),
        enabled: true,
        description: None,
    });
    (state, request)
}

#[test]
fn values_mode_expands_plain_variables_and_never_writes_a_secret_value() {
    let (state, request) = inherited_auth_request();
    let options = SnippetOptions {
        mode: SnippetMode::Values,
        environment_id: Some("env".into()),
    };
    let curl = request_code_snippet_impl(&state, &request, SnippetLanguage::Curl, &options)
        .expect("snippet");
    for value in [TOKEN, SECRET, "literal-key-789"] {
        assert!(!curl.contains(value), "{value} leaked into:\n{curl}");
    }
    assert!(curl.contains("'https://api.test/x?key='\"$KEY\""), "{curl}");
    assert!(
        curl.contains("'Authorization: Bearer '\"$TOKEN\""),
        "{curl}"
    );
    assert!(curl.contains("'X-Trace: '\"$API_SECRET\""), "{curl}");
}

#[test]
fn template_mode_resolves_inherited_auth_into_a_placeholder_header() {
    let (state, request) = inherited_auth_request();
    let curl = request_code_snippet_impl(
        &state,
        &request,
        SnippetLanguage::Curl,
        &SnippetOptions::default(),
    )
    .expect("snippet");
    assert!(!curl.contains(TOKEN), "{curl}");
    assert!(curl.contains("Authorization: Bearer {{token}}"), "{curl}");
    assert!(curl.contains("{{base_url}}/x?key={{key}}"), "{curl}");
    assert!(curl.contains("X-Trace: {{api_secret}}"), "{curl}");
}
