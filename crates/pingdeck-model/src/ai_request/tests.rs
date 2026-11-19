//! Tests of the AI API request model, destination rules and mode switching.

use serde_json::json;

use super::*;
use crate::models::{AuthConfig, RequestBody};
use crate::variables::{VariableScopes, resolve_template};

fn provider(model: &str) -> AiTarget {
    AiTarget::Provider {
        provider_id: "prov_1".to_owned(),
        provider_name: "DeepSeek".to_owned(),
        base_url_hint: "https://api.deepseek.com".to_owned(),
        model: model.to_owned(),
    }
}

fn config(target: AiTarget) -> AiRequestConfig {
    AiRequestConfig {
        protocol: Protocol::OpenaiChat,
        target,
        builder: None,
        compare_models: Vec::new(),
    }
}

fn header(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_owned(),
        value: value.to_owned(),
        enabled: true,
        description: None,
    }
}

fn provider_request() -> Request {
    let mut request = Request::new(
        "r1",
        "ws1",
        "Ask",
        non_routable_label("DeepSeek", "/chat/completions"),
    )
    .expect("request");
    request.method = crate::models::HttpMethod::Post;
    request.mode = RequestMode::AiApi;
    request.ai = Some(config(provider("deepseek-chat")));
    request.headers = vec![header("X-Trace", "1")];
    request.body = RequestBody::Json {
        content: "{\"model\":\"deepseek-chat\"}".to_owned(),
    };
    request
}

fn reason(result: Result<(), WorkspaceError>) -> String {
    match result.expect_err("must be invalid") {
        WorkspaceError::InvalidAiRequest { reason } => reason,
        other => format!("unexpected error {other}"),
    }
}

#[test]
fn mode_and_config_use_the_documented_wire_names() {
    assert_eq!(
        serde_json::to_value(RequestMode::Http).unwrap(),
        json!("http")
    );
    assert_eq!(
        serde_json::to_value(RequestMode::AiApi).unwrap(),
        json!("ai_api")
    );
    assert_eq!(RequestMode::default(), RequestMode::Http);
    assert_eq!(RequestMode::parse("ai_api"), Some(RequestMode::AiApi));
    assert_eq!(RequestMode::parse("other"), None);

    let mut cfg = config(provider("m"));
    cfg.protocol = Protocol::AnthropicMessages;
    cfg.compare_models = vec![CompareModel {
        provider_id: "p2".to_owned(),
        model: "x".to_owned(),
    }];
    assert_eq!(
        serde_json::to_value(&cfg).unwrap(),
        json!({
            "protocol": "anthropic_messages",
            "target": {"kind": "provider", "provider_id": "prov_1", "provider_name": "DeepSeek",
                       "base_url_hint": "https://api.deepseek.com", "model": "m"},
            "compare_models": [{"provider_id": "p2", "model": "x"}]
        })
    );
    cfg.target = AiTarget::Custom {
        model: "m".to_owned(),
    };
    cfg.builder = Some(json!({"system": "hi"}));
    let value = serde_json::to_value(&cfg).unwrap();
    assert_eq!(value["target"], json!({"kind": "custom", "model": "m"}));
    assert_eq!(value["builder"], json!({"system": "hi"}));
    assert_eq!(
        serde_json::from_value::<AiRequestConfig>(value).unwrap(),
        cfg
    );
}

#[test]
fn validate_checks_protocol_model_target_and_compare_list() {
    assert_eq!(config(provider("m")).validate(), Ok(()));
    assert_eq!(
        config(AiTarget::Custom {
            model: "m".to_owned()
        })
        .validate(),
        Ok(())
    );

    let mut cfg = config(provider("m"));
    cfg.protocol = Protocol::DecisionOpenai;
    assert!(reason(cfg.validate()).contains("decision_openai"));

    assert_eq!(reason(config(provider("  ")).validate()), "choose a model");
    assert_eq!(
        reason(
            config(AiTarget::Custom {
                model: String::new()
            })
            .validate()
        ),
        "choose a model"
    );
    // An unchosen provider may be saved: a synced request whose provider is unknown here keeps
    // its settings and asks for one; only sending is blocked.
    let no_provider = AiTarget::Provider {
        provider_id: " ".to_owned(),
        provider_name: String::new(),
        base_url_hint: String::new(),
        model: "m".to_owned(),
    };
    assert_eq!(config(no_provider).validate(), Ok(()));

    let entry = |index: usize| CompareModel {
        provider_id: format!("p{index}"),
        model: "m".to_owned(),
    };
    let mut cfg = config(provider("m"));
    cfg.compare_models = (0..MAX_COMPARE_MODELS).map(entry).collect();
    assert_eq!(cfg.validate(), Ok(()));
    cfg.compare_models.push(entry(99));
    assert!(reason(cfg.validate()).contains("at most 12"));
    cfg.compare_models = vec![entry(1), entry(1)];
    assert!(reason(cfg.validate()).contains("twice"));
    cfg.compare_models = vec![CompareModel {
        provider_id: "p".to_owned(),
        model: " ".to_owned(),
    }];
    assert!(reason(cfg.validate()).contains("needs a provider and a model"));
}

#[test]
fn request_ai_invariant_is_validated_and_normalised() {
    let mut request = provider_request();
    assert_eq!(request.validate_ai(), Ok(()));
    assert!(request.is_provider_target());

    request.ai = None;
    assert!(reason(request.validate_ai()).contains("needs AI settings"));

    // A request in HTTP mode that still carries AI settings is valid and loses them on save.
    let mut http = provider_request();
    http.mode = RequestMode::Http;
    assert_eq!(http.validate_ai(), Ok(()));
    assert!(!http.is_provider_target());
    http.normalize_ai();
    assert_eq!(http.ai, None);

    let mut ai = provider_request();
    ai.normalize_ai();
    assert!(ai.ai.is_some());
}

#[test]
fn request_json_round_trips_with_and_without_ai_settings() {
    // Sync documents serialise `Request` with serde; the additive fields must survive and an
    // older document without them must load as an HTTP request.
    let ai = provider_request();
    let value = serde_json::to_value(&ai).unwrap();
    assert_eq!(value["mode"], json!("ai_api"));
    assert_eq!(serde_json::from_value::<Request>(value).unwrap(), ai);

    let http = Request::new("r2", "ws1", "Plain", "https://example.com").unwrap();
    let mut value = serde_json::to_value(&http).unwrap();
    assert_eq!(value["mode"], json!("http"));
    assert_eq!(value["ai"], json!(null));
    assert_eq!(
        serde_json::from_value::<Request>(value.clone()).unwrap(),
        http
    );

    let object = value.as_object_mut().unwrap();
    object.remove("mode");
    object.remove("ai");
    let legacy = serde_json::from_value::<Request>(value).unwrap();
    assert_eq!(legacy.mode, RequestMode::Http);
    assert_eq!(legacy.ai, None);
}

#[test]
fn relink_matches_by_id_then_by_base_url() {
    let target = provider("m");
    let providers = [
        ("other", "https://api.deepseek.com/"),
        ("prov_1", "https://x.test"),
    ];
    // Id wins over a base URL match on another provider.
    assert_eq!(
        relink_provider(&target, &providers),
        Relink::Linked("prov_1".to_owned())
    );
    let by_url = [
        ("other", "HTTPS://API.deepseek.com/"),
        ("p3", "https://x.test"),
    ];
    assert_eq!(
        relink_provider(&target, &by_url),
        Relink::Linked("other".to_owned())
    );
    let none = [
        ("p3", "https://x.test"),
        ("p4", "https://api.deepseek.com/v1"),
    ];
    assert_eq!(relink_provider(&target, &none), Relink::NeedsChoice);
    assert_eq!(relink_provider(&target, &[]), Relink::NeedsChoice);

    let blank_hint = AiTarget::Provider {
        provider_id: "gone".to_owned(),
        provider_name: "DeepSeek".to_owned(),
        base_url_hint: " ".to_owned(),
        model: "m".to_owned(),
    };
    assert_eq!(
        relink_provider(&blank_hint, &[("p", "")]),
        Relink::NeedsChoice
    );
    assert_eq!(
        relink_provider(
            &AiTarget::Custom {
                model: "m".to_owned()
            },
            &providers
        ),
        Relink::NotApplicable
    );
}

#[test]
fn provider_label_is_never_a_routable_address() {
    assert_eq!(
        non_routable_label("DeepSeek", "/chat/completions"),
        "pingdeck-provider://deepseek/chat/completions"
    );
    // A provider named after its host does not leave a host-shaped label.
    let label = non_routable_label("api.deepseek.com", "chat/completions");
    assert_eq!(
        label,
        "pingdeck-provider://api-deepseek-com/chat/completions"
    );
    assert_eq!(
        non_routable_label("  ", "/x"),
        "pingdeck-provider://provider/x"
    );
    assert!(is_non_routable_label(&label));
    assert!(!is_non_routable_label("https://api.deepseek.com/v1"));
    assert!(!is_non_routable_label("pingdeck-providerx://a"));
    let parsed = Url::parse(&label).expect("a label is syntactically a URL");
    assert!(!matches!(parsed.scheme(), "http" | "https"));
}

#[test]
fn user_credentials_are_dropped_in_any_case() {
    let mut disabled = header("Authorization", "Bearer off");
    disabled.enabled = false;
    let headers = vec![
        header("Accept", "text/plain"),
        header("authorization", "Bearer a"),
        header(" X-API-KEY ", "k"),
        header("X-Goog-Api-Key", "g"),
        header("HOST", "evil.example"),
        header("Host", "evil.example"),
        disabled.clone(),
        header("x-trace", "1"),
    ];
    let (kept, dropped) = strip_user_credentials(&headers);
    assert_eq!(
        dropped,
        ["authorization", "X-API-KEY", "X-Goog-Api-Key", "HOST"]
    );
    assert_eq!(
        kept.iter().map(|h| h.key.as_str()).collect::<Vec<_>>(),
        ["Accept", "Authorization", "x-trace"]
    );
    assert!(
        kept.contains(&disabled),
        "a disabled header is not sent, so it is not dropped"
    );
}

#[test]
fn script_policy_locks_the_destination_of_provider_targets_only() {
    let original = provider_request();
    let mut scripted = original.clone();
    scripted.url = "https://evil.example/steal".to_owned();
    scripted.query_params = vec![header("key", "x")];
    scripted.path_variables = vec![header("id", "1")];
    scripted.auth = AuthConfig::Bearer {
        token_ref: "t".to_owned(),
    };
    scripted.headers = vec![header("Authorization", "Bearer s"), header("X-New", "1")];
    scripted.body = RequestBody::Json {
        content: "{\"model\":\"x\"}".to_owned(),
    };

    let policy = ScriptRequestPolicy::for_request(&original);
    assert!(policy.lock_destination);
    let dropped = policy.enforce(&original, &mut scripted);
    assert_eq!(dropped, ["Authorization"]);
    assert_eq!(scripted.url, original.url);
    assert_eq!(scripted.query_params, original.query_params);
    assert_eq!(scripted.path_variables, original.path_variables);
    assert_eq!(scripted.auth, original.auth);
    assert_eq!(scripted.headers, vec![header("X-New", "1")]);
    // The prompt is the script's to change.
    assert_eq!(
        scripted.body,
        RequestBody::Json {
            content: "{\"model\":\"x\"}".to_owned()
        }
    );

    let mut custom = original.clone();
    custom.ai = Some(config(AiTarget::Custom {
        model: "m".to_owned(),
    }));
    let policy = ScriptRequestPolicy::for_request(&custom);
    assert!(!policy.lock_destination);
    let mut scripted = custom.clone();
    scripted.url = "https://mine.example".to_owned();
    scripted.headers = vec![header("Authorization", "Bearer mine")];
    assert!(policy.enforce(&custom, &mut scripted).is_empty());
    assert_eq!(scripted.url, "https://mine.example");
    assert_eq!(
        scripted.headers,
        vec![header("Authorization", "Bearer mine")]
    );
}

#[test]
fn http_to_ai_and_back_keeps_the_http_fields_byte_identical() {
    let mut http = Request::new("r3", "ws1", "Plain", "https://example.com/v1/chat?a=1").unwrap();
    http.headers = vec![header("Authorization", "Bearer mine"), header("X", "y")];
    http.auth = AuthConfig::Bearer {
        token_ref: "tok".to_owned(),
    };
    http.body = RequestBody::RawText {
        language: None,
        content: "  raw ".to_owned(),
    };

    let ai = switch_mode(http.clone(), RequestMode::AiApi, None);
    assert_eq!(ai.mode, RequestMode::AiApi);
    assert_eq!(ai.ai, Some(AiRequestConfig::default()));
    assert_eq!(ai.url, http.url);
    assert_eq!(ai.headers, http.headers);
    assert_eq!(ai.auth, http.auth);
    assert_eq!(ai.body, http.body);
    assert_eq!(ai.query_params, http.query_params);

    // A custom-target request already is plain HTTP: nothing is rewritten.
    let back = switch_mode(ai, RequestMode::Http, None);
    assert_eq!(back, http);

    // Switching to the mode a request is already in changes nothing, existing settings stay.
    let existing = provider_request();
    assert_eq!(
        switch_mode(existing.clone(), RequestMode::AiApi, None),
        existing
    );
    assert_eq!(switch_mode(http.clone(), RequestMode::Http, None), http);

    // Existing AI settings are used when switching to AI mode.
    let mut staged = http;
    staged.ai = Some(config(provider("deepseek-chat")));
    let ai = switch_mode(staged, RequestMode::AiApi, None);
    assert_eq!(ai.ai, Some(config(provider("deepseek-chat"))));
}

#[test]
fn provider_request_in_http_mode_holds_a_placeholder_and_cannot_be_sent() {
    let mut request = provider_request();
    request.headers = vec![
        header("Authorization", "Bearer user-typed"),
        header("Host", "evil.example"),
        header("X-Trace", "1"),
    ];
    request.auth = AuthConfig::None;
    let body = request.body.clone();

    let http = switch_mode(
        request,
        RequestMode::Http,
        Some("https://api.deepseek.com/chat/completions"),
    );
    assert_eq!(http.mode, RequestMode::Http);
    assert_eq!(http.ai, None);
    assert_eq!(http.url, "https://api.deepseek.com/chat/completions");
    assert_eq!(http.body, body);
    assert_eq!(
        http.headers,
        vec![
            header("X-Trace", "1"),
            header("Authorization", "Bearer {{provider key}}"),
            header("Content-Type", "application/json"),
        ]
    );
    let error = ensure_sendable(&http).expect_err("refused");
    assert!(
        matches!(&error, AppError::Validation { message, .. } if message.contains("{{provider key}}")),
        "{error:?}"
    );

    // Replacing the credential with real authentication makes it sendable.
    let mut authed = http.clone();
    authed.headers.retain(|h| h.key != "Authorization");
    authed
        .headers
        .push(header("Authorization", "Bearer sk-mine"));
    assert_eq!(ensure_sendable(&authed), Ok(()));

    // The placeholder is found wherever it is typed.
    for edit in [
        |r: &mut Request| r.url = "https://x.test/?k={{provider key}}".to_owned(),
        |r: &mut Request| r.query_params = vec![header("key", "{{provider_key}}")],
        |r: &mut Request| {
            r.auth = AuthConfig::Bearer {
                token_ref: "{{provider key}}".to_owned(),
            }
        },
        |r: &mut Request| {
            r.body = RequestBody::Json {
                content: "{\"k\":\"{{provider key}}\"}".to_owned(),
            }
        },
    ] {
        let mut request = authed.clone();
        edit(&mut request);
        assert!(ensure_sendable(&request).is_err());
    }
}

#[test]
fn placeholder_headers_follow_the_protocol() {
    let names = |protocol| {
        plain_http_headers(&[], protocol, EXPORT_KEY_PLACEHOLDER)
            .into_iter()
            .map(|h| format!("{}: {}", h.key, h.value))
            .collect::<Vec<_>>()
    };
    assert_eq!(
        names(Protocol::OpenaiChat),
        [
            "Authorization: Bearer {{api_key}}",
            "Content-Type: application/json"
        ]
    );
    assert_eq!(
        names(Protocol::Embeddings),
        names(Protocol::OpenaiResponses)
    );
    assert_eq!(
        names(Protocol::AnthropicMessages),
        [
            "x-api-key: {{api_key}}".to_owned(),
            format!("anthropic-version: {DEFAULT_ANTHROPIC_VERSION}"),
            "Content-Type: application/json".to_owned(),
        ]
    );
    assert_eq!(
        names(Protocol::Gemini),
        [
            "x-goog-api-key: {{api_key}}",
            "Content-Type: application/json"
        ]
    );
    let kept = plain_http_headers(
        &[header("content-type", "application/json; charset=utf-8")],
        Protocol::OpenaiChat,
        "k",
    );
    assert_eq!(
        kept.iter()
            .filter(|h| h.key.eq_ignore_ascii_case("content-type"))
            .count(),
        1
    );
}

#[test]
fn provider_keys_are_not_variables() {
    let scopes = VariableScopes::new();
    for text in ["Bearer {{provider key}}", "{{provider_key}}"] {
        assert!(
            resolve_template(text, &scopes).is_err(),
            "{text} must not resolve"
        );
    }
}

#[test]
fn script_result_serialises_in_the_script_shape() {
    let result = AiScriptResult {
        reply: "pong".to_owned(),
        reasoning: String::new(),
        tool_calls: vec![AiScriptToolCall {
            id: "c1".to_owned(),
            name: "f".to_owned(),
            arguments: "{}".to_owned(),
        }],
        finish_reason: Some("stop".to_owned()),
        model: Some("m".to_owned()),
        usage: AiScriptUsage {
            input_tokens: Some(9),
            output_tokens: Some(2),
            ..AiScriptUsage::default()
        },
        ttft_ms: Some(120),
        total_ms: 233,
        tokens_per_second: None,
        event_count: 4,
    };
    let value = serde_json::to_value(&result).unwrap();
    assert_eq!(
        value["toolCalls"][0],
        json!({"id": "c1", "name": "f", "arguments": "{}"})
    );
    assert_eq!(value["finishReason"], json!("stop"));
    assert_eq!(
        value["usage"],
        json!({"inputTokens": 9, "outputTokens": 2, "cachedTokens": null, "reasoningTokens": null})
    );
    assert_eq!(value["tokensPerSecond"], json!(null));
    assert_eq!(value["ttftMs"], json!(120));
    assert_eq!(value["eventCount"], json!(4));
    assert!(value.get("provider").is_none() && value.get("apiKey").is_none());
}
