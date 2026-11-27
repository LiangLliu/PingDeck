//! Identity scoring tests.

use serde_json::json;

use super::*;

fn status(data: &Value) -> Facts {
    Facts {
        status_body: Some(json!({"success": true, "data": data})),
        ..Facts::default()
    }
}

fn headers(facts: &mut Facts, list: &[(&str, &str)]) {
    let owned: Vec<(String, String)> = list
        .iter()
        .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
        .collect();
    facts.observe_headers(&owned);
}

#[test]
fn new_api_scores_eight_against_one_api_three() {
    let mut facts = status(
        &json!({"quota_per_unit": 500000, "system_name": "Acme Relay",
        "version": "0.9.7", "display_in_currency": true, "usd_exchange_rate": 7.2}),
    );
    headers(&mut facts, &[("x-new-api-version", "0.9.7")]);
    let identity = score(&facts);
    assert_eq!(identity.family, GatewayFamily::NewApi);
    assert_eq!(identity.score, 8);
    assert_eq!(
        identity.runner_up,
        Some(RunnerUp {
            family: GatewayFamily::OneApi,
            score: 3
        })
    );
    assert!(identity.confirmed && identity.label == IdentityLabel::Confirmed);
    assert_eq!(identity.brand.as_deref(), Some("Acme Relay"));
    assert_eq!(identity.version.as_deref(), Some("0.9.7"));
    assert_eq!(identity.quota_per_unit, Some(500_000.0));
    assert_eq!(identity.display_in_currency, Some(true));
}

#[test]
fn new_api_keeps_its_lead_when_it_also_sends_the_one_api_request_id() {
    let mut facts = status(&json!({"quota_per_unit": 500000, "system_name": "My Hub",
        "quota_display_type": "USD", "usd_exchange_rate": 7.2,
        "custom_currency_symbol": "¥"}));
    headers(
        &mut facts,
        &[
            ("x-new-api-version", "v1.0.0-rc.48"),
            ("x-oneapi-request-id", "20261108090000123456"),
        ],
    );
    let identity = score(&facts);
    assert_eq!(identity.family, GatewayFamily::NewApi);
    assert_eq!(identity.score, 8);
    assert_eq!(
        identity.runner_up,
        Some(RunnerUp {
            family: GatewayFamily::OneApi,
            score: 3
        })
    );
    assert!(identity.confirmed && identity.label == IdentityLabel::Confirmed);
    assert_eq!(identity.brand.as_deref(), Some("My Hub"));
    assert_eq!(identity.version.as_deref(), Some("v1.0.0-rc.48"));
}

#[test]
fn shared_structure_without_an_exclusive_signal_is_unidentified() {
    let identity = score(&status(
        &json!({"quota_per_unit": 500000, "system_name": ""}),
    ));
    assert_eq!(identity.family, GatewayFamily::UnknownCompatible);
    assert_eq!(identity.label, IdentityLabel::CompatibleUnidentified);
    assert_eq!(identity.score, 4, "one-api gets +1 for the empty name");
    let tie = score(&status(&json!({"quota_per_unit": 500000})));
    assert_eq!(tie.family, GatewayFamily::UnknownCompatible);
    assert!(!tie.confirmed);
    assert_eq!(tie.quota_per_unit, Some(500_000.0));
}

#[test]
fn one_api_by_its_request_id_header() {
    let mut facts = status(
        &json!({"quota_per_unit": 500000, "system_name": "", "version": "v0.6.10", "display_in_currency": false}),
    );
    headers(&mut facts, &[("X-Oneapi-Request-Id", "abc")]);
    let identity = score(&facts);
    assert_eq!(identity.family, GatewayFamily::OneApi);
    assert_eq!(identity.score, 7);
    assert_eq!(identity.display_in_currency, Some(false));
    assert_eq!(identity.version.as_deref(), Some("v0.6.10"));
}

#[test]
fn done_hub_veloera_and_rix_win_by_their_brand_signal() {
    let done = score(&status(
        &json!({"quota_per_unit": 500000, "system_name": "My DoneHub"}),
    ));
    assert_eq!((done.family, done.score), (GatewayFamily::DoneHub, 5));
    assert!(done.quota_is_remaining);
    let veloera = score(&status(
        &json!({"quota_per_unit": 500000, "system_name": "x", "version": "veloera-1.2"}),
    ));
    assert_eq!((veloera.family, veloera.score), (GatewayFamily::Veloera, 5));
    assert!(veloera.third_party);
    assert_eq!(veloera.brand.as_deref(), Some("Veloera"));
    let rix = score(&status(
        &json!({"quota_per_unit": 500000, "system_name": "x", "rix_license_enabled": true}),
    ));
    assert_eq!((rix.family, rix.score), (GatewayFamily::RixApi, 6));
}

#[test]
fn sub2api_needs_exactly_the_two_key_401() {
    let exact = Facts {
        sub2api_me: Some((
            401,
            Some(json!({"code": "UNAUTHORIZED", "message": "Authorization header is required"})),
        )),
        ..Facts::default()
    };
    assert_eq!(score(&exact).family, GatewayFamily::Sub2Api);
    for body in [
        json!({"code": "UNAUTHORIZED", "message": "x", "extra": 1}),
        json!({"code": "UNAUTHORIZED"}),
        json!({"code": 401, "message": "x"}),
    ] {
        let facts = Facts {
            sub2api_me: Some((401, Some(body))),
            ..Facts::default()
        };
        assert_eq!(score(&facts).family, GatewayFamily::UnknownCompatible);
    }
    let wrong_status = Facts {
        sub2api_me: Some((403, Some(json!({"code": "UNAUTHORIZED", "message": "x"})))),
        ..Facts::default()
    };
    assert_eq!(
        score(&wrong_status).family,
        GatewayFamily::UnknownCompatible
    );
}

#[test]
fn litellm_needs_a_header_or_model_info_never_the_envelope_alone() {
    let envelope = Facts {
        litellm_envelope: true,
        ..Facts::default()
    };
    assert_eq!(score(&envelope).family, GatewayFamily::UnknownCompatible);
    assert_eq!(score(&envelope).score, 0);
    let mut by_header = Facts::default();
    headers(
        &mut by_header,
        &[("x-litellm-call-id", "1"), ("x-litellm-version", "1.80.0")],
    );
    let identity = score(&by_header);
    assert_eq!(
        (identity.family, identity.score),
        (GatewayFamily::LiteLlm, 3)
    );
    assert_eq!(identity.version.as_deref(), Some("1.80.0"));
    let mut full = Facts {
        model_info: Some((
            200,
            Some(json!({"data": [{"model_name": "m", "litellm_params": {}}]})),
        )),
        litellm_envelope: true,
        ..Facts::default()
    };
    headers(&mut full, &[("x-litellm-call-id", "1")]);
    assert_eq!(score(&full).score, 7);
}

#[test]
fn cliproxyapi_by_header_never_by_port() {
    let port_only = Facts {
        cpa_default_port: true,
        ..Facts::default()
    };
    assert_eq!(score(&port_only).family, GatewayFamily::UnknownCompatible);
    let mut facts = Facts {
        cpa_default_port: true,
        ..Facts::default()
    };
    headers(&mut facts, &[("x-cpa-version", "6.1.0")]);
    let identity = score(&facts);
    assert_eq!(
        (identity.family, identity.score),
        (GatewayFamily::CliProxyApi, 4)
    );
    assert_eq!(identity.version.as_deref(), Some("6.1.0"));
}

#[test]
fn domain_rules_give_preset_and_a_response_signal_confirms() {
    let preset = Facts {
        domain: Some(GatewayFamily::Official),
        ..Facts::default()
    };
    let identity = score(&preset);
    assert_eq!(
        (identity.family, identity.score, identity.label),
        (GatewayFamily::Official, 4, IdentityLabel::Preset)
    );
    assert!(!identity.confirmed);
    let openrouter = Facts {
        domain: Some(GatewayFamily::OpenRouter),
        openrouter_anonymous_models: true,
        ..Facts::default()
    };
    let identity = score(&openrouter);
    assert_eq!(
        (identity.family, identity.score, identity.confirmed),
        (GatewayFamily::OpenRouter, 7, true)
    );
    assert_eq!(identity.brand.as_deref(), Some("OpenRouter"));
    // The anonymous list alone (a relay mimicking OpenRouter) is accepted at 3 but not by domain.
    let relay = Facts {
        openrouter_anonymous_models: true,
        ..Facts::default()
    };
    assert_eq!(score(&relay).label, IdentityLabel::Confirmed);
}

#[test]
fn voapi_is_identified_only_with_the_console_answer() {
    assert_eq!(
        score(&Facts::default()).family,
        GatewayFamily::UnknownCompatible
    );
    let with_token = Facts {
        voapi_user_info: true,
        ..Facts::default()
    };
    assert_eq!(score(&with_token).family, GatewayFamily::VoApi);
}

#[test]
fn domain_family_table() {
    let known = |id: &str| KnownHost {
        preset_id: id.into(),
        region_id: "global".into(),
        balance_adapter: None,
    };
    assert_eq!(
        domain_family(Some(&known("deepseek")), "api.deepseek.com"),
        Some(GatewayFamily::Official)
    );
    assert_eq!(
        domain_family(Some(&known("openrouter")), "openrouter.ai"),
        Some(GatewayFamily::OpenRouter)
    );
    assert_eq!(
        domain_family(Some(&known("ollama")), "localhost"),
        Some(GatewayFamily::Ollama)
    );
    assert_eq!(
        domain_family(None, "ai-gateway.vercel.sh"),
        Some(GatewayFamily::VercelAiGateway)
    );
    assert_eq!(domain_family(None, "relay.acme.dev"), None);
}

#[test]
fn brand_and_version_are_cleaned_and_cut() {
    let long = "x".repeat(200);
    let identity = score(&{
        let mut facts = status(
            &json!({"quota_per_unit": 1, "system_name": format!("A\n{long}"), "usd_exchange_rate": 1}),
        );
        headers(&mut facts, &[("x-new-api-version", "1.0")]);
        facts
    });
    assert_eq!(identity.brand.map(|b| b.chars().count()), Some(64));
}

#[test]
fn equal_scores_keep_the_order_the_signals_were_added_in() {
    // Domain (+4) is added before a local hit (+4): the domain family wins the tie.
    let tie = score(&Facts {
        domain: Some(GatewayFamily::Official),
        local: Some(LocalHit {
            family: GatewayFamily::Ollama,
            version: None,
            context: None,
            port_hint: true,
        }),
        ..Facts::default()
    });
    assert_eq!(
        (tie.family, tie.score, tie.accepted),
        (GatewayFamily::UnknownCompatible, 4, false)
    );
    assert_eq!(
        tie.runner_up,
        Some(RunnerUp {
            family: GatewayFamily::Ollama,
            score: 4
        })
    );
    // Sub2API's 401 is scored before VoAPI's console answer.
    let probes = score(&Facts {
        sub2api_me: Some((401, Some(json!({"code": "UNAUTHORIZED", "message": "x"})))),
        voapi_user_info: true,
        ..Facts::default()
    });
    assert_eq!(probes.family, GatewayFamily::UnknownCompatible);
    assert_eq!(
        probes.runner_up,
        Some(RunnerUp {
            family: GatewayFamily::VoApi,
            score: 3
        })
    );
}

#[test]
fn an_unparsable_status_alone_scores_zero() {
    // Regression guard (the score used to be -2).
    let identity = score(&Facts {
        status_unparsable: true,
        ..Facts::default()
    });
    assert_eq!(
        (identity.family, identity.score, identity.accepted),
        (GatewayFamily::UnknownCompatible, 0, false)
    );
    assert_eq!(identity.runner_up, None);
}
