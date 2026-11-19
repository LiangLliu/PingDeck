//! Provider rows: server-owned columns, extra-header secrets, custom balance secrets, deletes.

use std::collections::BTreeMap;

use super::{
    connection, fast_model, key, model, provider, raw_text, secret, seeded,
    single_endpoint_provider,
};
use crate::llm::balance_state::{
    BalanceAuth, BalanceScope, CustomBalanceQuery, CustomHeaderRef, ResolvedHeaders, UnitName,
};
use crate::llm::probe::listing::ModelDrift;
use crate::llm::store::provider_state::{clear_model_drift, set_model_drift, set_needs_resniff};
use crate::llm::store::{
    MAX_SNIFF_EVIDENCE, ModelRef, ModelSource, Provider, SniffPersist, delete_provider,
    get_provider, list_fallbacks, list_keys, list_legacy_names, list_models, list_providers,
    provider_header_hints, resolve_provider_headers, resolved_custom_balance, save_key,
    save_legacy_name, save_model, save_provider, set_custom_balance, set_key_management_token,
    set_model_fallbacks, set_provider_sniff,
};
use crate::llm::wire::{AuthStyle, GatewayFamily, MaxTokensField, Protocol, ReasoningStyle};
use pingdeck_base::error::AppError;
use pingdeck_llm_domain::llm::sniff_summary::{EvidenceItem, RunnerUp, SniffSummary};

const HEADER_VALUE: &str = "team-secret-value-1234";
const PROXY_PASSWORD: &str = "pw-proxy-4321";

fn with_header(id: &str, name: &str, value: &str) -> Provider {
    let mut p = provider(id);
    p.quirks
        .extra_headers
        .insert(name.to_owned(), value.to_owned());
    p
}

fn with_proxy(id: &str, url: &str) -> Provider {
    let mut p = provider(id);
    p.proxy_url = url.to_owned();
    p
}

fn summary() -> SniffSummary {
    SniffSummary {
        family: GatewayFamily::NewApi,
        brand_name: Some("Acme Relay".into()),
        version: Some("0.9.7".into()),
        confidence_score: 92,
        confirmed: true,
        runner_up: None,
        requests_used: 6,
        elapsed_ms: 1_200,
        tokens_spent: 0,
        quota_is_remaining: false,
        scanned_at_ms: 77,
    }
}

fn evidence(id: u32) -> EvidenceItem {
    EvidenceItem {
        id,
        phase: "identify".into(),
        method: "GET".into(),
        url: "https://example.test/api/status".into(),
        status: Some(200),
        latency_ms: 12,
        body_sha256_prefix: "0123456789abcdef".into(),
        note: "identified".into(),
        conclusion: "identity".into(),
        error_kind: "none".into(),
    }
}

fn sniff(count: u32) -> SniffPersist {
    SniffPersist {
        family: GatewayFamily::NewApi,
        quota_per_unit: Some(500_000.0),
        display_in_currency: Some(true),
        evidence: (0..count).map(evidence).collect(),
        summary: summary(),
    }
}

fn query(headers: &[&str]) -> CustomBalanceQuery {
    CustomBalanceQuery {
        url: "https://example.test/balance".into(),
        headers: headers
            .iter()
            .map(|name| CustomHeaderRef {
                name: (*name).into(),
                secret_ref: String::new(),
            })
            .collect(),
        auth: BalanceAuth::Bearer,
        remaining_path: "$.data.available".into(),
        used_path: None,
        total_path: None,
        unit: UnitName::Usd,
        scope: Some(BalanceScope::Account),
        unit_label: None,
        divisor: None,
        cross_host: false,
        resolved_headers: ResolvedHeaders::default(),
    }
}

fn values(pairs: &[(&str, &str)]) -> BTreeMap<String, String> {
    pairs
        .iter()
        .map(|(n, v)| ((*n).to_owned(), (*v).to_owned()))
        .collect()
}

#[test]
fn provider_ui_save_server_columns_and_sniff_healing() {
    let c = connection();
    assert!(matches!(
        set_provider_sniff(&c, "nope", &sniff(1)),
        Err(AppError::NotFound { .. })
    ));
    assert!(matches!(
        set_needs_resniff(&c, "nope", true),
        Err(AppError::NotFound { .. })
    ));

    let mut initial = provider("p1");
    initial.gateway_family = GatewayFamily::Ollama;
    initial.needs_resniff = true;
    initial.quota_per_unit = Some(3.0);
    initial.primary_protocol = Some(Protocol::AnthropicMessages);
    let fresh = save_provider(&c, &initial, 1).unwrap();
    assert_eq!(
        (
            fresh.gateway_family,
            fresh.needs_resniff,
            fresh.quota_per_unit,
            fresh.primary_protocol
        ),
        (
            GatewayFamily::UnknownCompatible,
            false,
            None,
            Some(Protocol::AnthropicMessages)
        )
    );

    let mut dangling = fresh;
    dangling.primary_protocol = Some(Protocol::Gemini);
    assert_eq!(
        save_provider(&c, &dangling, 2).unwrap().primary_protocol,
        None
    );

    set_provider_sniff(&c, "p1", &sniff(35)).unwrap();
    set_model_drift(
        &c,
        "p1",
        &ModelDrift {
            checked_at_ms: 9,
            added: vec!["new".into()],
            removed: vec![],
            key_failures: Vec::new(),
            listing_complete: true,
        },
    )
    .unwrap();
    set_needs_resniff(&c, "p1", true).unwrap();
    set_custom_balance(
        &c,
        "p1",
        Some(&query(&["X-Token"])),
        &values(&[("X-Token", "sec")]),
        1,
    )
    .unwrap();
    let before = get_provider(&c, "p1").unwrap().unwrap();
    assert_eq!(before.sniff_evidence.len(), MAX_SNIFF_EVIDENCE);

    let mut edited = provider("p1");
    edited.name = "Renamed".into();
    edited.gateway_family = GatewayFamily::OneApi;
    edited.quota_per_unit = Some(1.0);
    edited.display_in_currency = Some(false);
    edited.sniff_evidence = vec![evidence(99)];
    let after = save_provider(&c, &edited, 50).unwrap();
    assert_eq!(
        (
            after.name.as_str(),
            after.gateway_family,
            after.quota_per_unit
        ),
        ("Renamed", GatewayFamily::NewApi, Some(500_000.0))
    );
    assert_eq!(
        (
            &after.sniff_evidence,
            &after.model_drift,
            &after.sniff_summary
        ),
        (
            &before.sniff_evidence,
            &before.model_drift,
            &before.sniff_summary
        )
    );
    assert!(after.needs_resniff && after.custom_balance.is_none());

    clear_model_drift(&c, "p1").unwrap();
    assert!(
        get_provider(&c, "p1")
            .unwrap()
            .unwrap()
            .model_drift
            .is_none()
    );

    // Sniff bad quotas, tie healing, and corrupt JSON resilience.
    for bad in [0.0, -5.0, f64::NAN, f64::INFINITY] {
        let mut persist = sniff(1);
        persist.quota_per_unit = Some(bad);
        set_provider_sniff(&c, "p1", &persist).unwrap();
        assert_eq!(
            get_provider(&c, "p1").unwrap().unwrap().quota_per_unit,
            None
        );
    }

    let mut tied = sniff(1);
    tied.family = GatewayFamily::UnknownCompatible;
    tied.summary = SniffSummary {
        family: GatewayFamily::UnknownCompatible,
        brand_name: None,
        confidence_score: 6,
        confirmed: false,
        runner_up: Some(RunnerUp {
            family: GatewayFamily::OneApi,
            score: 6,
        }),
        ..summary()
    };
    tied.evidence[0].note = "200 ok, quota_per_unit 500000, system_name My Hub".into();
    set_provider_sniff(&c, "p1", &tied).unwrap();
    let healed = get_provider(&c, "p1").unwrap().unwrap();
    assert_eq!(healed.gateway_family, GatewayFamily::NewApi);
    assert_eq!(
        healed.sniff_summary.unwrap().brand_name.as_deref(),
        Some("My Hub")
    );

    c.execute(
        "UPDATE llm_providers SET sniff_evidence_json = 'oops', model_drift_json = '{', \
         sniff_summary_json = 'x', custom_balance_json = '[]'",
        [],
    )
    .unwrap();
    let listed = &list_providers(&c).unwrap()[0];
    assert!(
        listed.sniff_evidence.is_empty()
            && listed.model_drift.is_none()
            && listed.sniff_summary.is_none()
            && listed.custom_balance.is_none()
    );
}

#[test]
fn extra_header_values_live_in_the_vault() {
    let c = connection();
    let mut p = with_header("p1", "X-Team", HEADER_VALUE);
    p.quirks
        .extra_headers
        .insert("X-Plain".into(), String::new());
    let saved = save_provider(&c, &p, 1).unwrap();
    let placeholder = "{{secret:llm.p1.header.X-Team}}";
    assert_eq!(saved.quirks.extra_headers["X-Team"], placeholder);
    assert_eq!(
        secret(&c, "llm.p1.header.X-Team").as_deref(),
        Some(HEADER_VALUE)
    );
    let stored = raw_text(&c, "SELECT quirks_json FROM llm_providers WHERE id = 'p1'");
    assert!(!stored.contains(HEADER_VALUE) && stored.contains(placeholder));

    let resolved = resolve_provider_headers(&c, &saved).unwrap();
    assert_eq!(
        (resolved["X-Team"].as_str(), resolved["X-Plain"].as_str()),
        (HEADER_VALUE, "")
    );

    // Re-saving placeholder keeps secret; updating replaces; clearing deletes.
    let mut updated = save_provider(&c, &saved, 2).unwrap();
    assert_eq!(
        secret(&c, "llm.p1.header.X-Team").as_deref(),
        Some(HEADER_VALUE)
    );
    updated
        .quirks
        .extra_headers
        .insert("X-Team".into(), "new-secret-val".into());
    save_provider(&c, &updated, 3).unwrap();
    assert_eq!(
        secret(&c, "llm.p1.header.X-Team").as_deref(),
        Some("new-secret-val")
    );
    updated.quirks.extra_headers.clear();
    save_provider(&c, &updated, 4).unwrap();
    assert_eq!(secret(&c, "llm.p1.header.X-Team"), None);
}

#[test]
fn foreign_refs_and_reserved_headers_are_refused_and_a_rollback_keeps_no_secret() {
    let c = connection();
    seeded(&c, "p0", "k0");
    assert!(matches!(
        save_provider(
            &c,
            &with_header("p2", "X-Steal", "{{secret:llm.key.k0}}"),
            1
        ),
        Err(AppError::Validation { .. })
    ));
    assert!(matches!(
        save_provider(&c, &with_header("p2", "Authorization", "Bearer x"), 1),
        Err(AppError::Validation { .. })
    ));
    let err = resolve_provider_headers(&c, &with_header("p2", "X-Steal", "{{secret:llm.key.k0}}"))
        .unwrap_err();
    assert!(err.message().contains("X-Steal") && !err.message().contains("sk-seeded-secret-000"));

    c.execute_batch("BEGIN IMMEDIATE").unwrap();
    save_provider(&c, &with_header("p3", "X-Team", HEADER_VALUE), 1).unwrap();
    c.execute_batch("ROLLBACK").unwrap();
    assert!(
        get_provider(&c, "p3").unwrap().is_none() && secret(&c, "llm.p3.header.X-Team").is_none()
    );
}

#[test]
fn custom_balance_header_values_live_in_the_vault() {
    let c = connection();
    seeded(&c, "p0", "k0");
    save_provider(&c, &provider("p1"), 4).unwrap();
    assert!(resolved_custom_balance(&c, "p1").unwrap().is_none());
    assert!(matches!(
        set_custom_balance(
            &c,
            "p1",
            Some(&query(&["X-One", "X-Two"])),
            &values(&[("X-One", "one-value-1")]),
            1
        ),
        Err(AppError::Validation { .. })
    ));
    assert_eq!(secret(&c, "llm.p1.balance.header.X-One"), None);

    let mut custom = query(&["X-One", "X-Two"]);
    custom.headers[0].secret_ref = "llm.key.k0".into();
    set_custom_balance(
        &c,
        "p1",
        Some(&custom),
        &values(&[("X-One", "one-value-1"), ("X-Two", "two-value-2")]),
        2,
    )
    .unwrap();
    let json = raw_text(
        &c,
        "SELECT custom_balance_json FROM llm_providers WHERE id = 'p1'",
    );
    assert!(!json.contains("one-value-1") && json.contains("llm.p1.balance.header.X-One"));
    let res_bal = resolved_custom_balance(&c, "p1").unwrap().unwrap();
    assert_eq!(res_bal.resolved_headers.0.len(), 2);
    assert!(!format!("{res_bal:?}").contains("one-value-1"));

    let mut narrowed = query(&["X-One"]);
    narrowed.remaining_path = "$.data.other".into();
    set_custom_balance(&c, "p1", Some(&narrowed), &BTreeMap::new(), 3).unwrap();
    assert_eq!(
        secret(&c, "llm.p1.balance.header.X-One").as_deref(),
        Some("one-value-1")
    );
    assert_eq!(secret(&c, "llm.p1.balance.header.X-Two"), None);

    let mut bad = narrowed;
    bad.url = "ftp://example.test/balance".into();
    assert!(matches!(
        set_custom_balance(&c, "p1", Some(&bad), &BTreeMap::new(), 4),
        Err(AppError::Validation { .. })
    ));
    set_custom_balance(&c, "p1", None, &BTreeMap::new(), 5).unwrap();
    assert_eq!(secret(&c, "llm.p1.balance.header.X-One"), None);
}

#[test]
fn a_proxy_password_lives_in_the_vault_and_is_masked_on_the_wire() {
    let c = connection();
    let saved_prx = save_provider(
        &c,
        &with_proxy(
            "p1",
            &format!("http://proxy-user:{PROXY_PASSWORD}@proxy.test:8080"),
        ),
        6,
    )
    .unwrap();
    assert_eq!(
        raw_text(&c, "SELECT proxy_url FROM llm_providers WHERE id = 'p1'"),
        "http://proxy-user:****@proxy.test:8080"
    );
    assert_eq!(
        secret(&c, "llm.provider.p1.proxy_password").as_deref(),
        Some(PROXY_PASSWORD)
    );
    assert!(saved_prx.proxy_url.contains(PROXY_PASSWORD));
    let wire = serde_json::to_string(&saved_prx).unwrap();
    assert!(!wire.contains(PROXY_PASSWORD) && wire.contains("proxy-user:****@proxy.test:8080"));

    save_provider(
        &c,
        &with_proxy("p1", "http://proxy-user:****@proxy.test:8080"),
        7,
    )
    .unwrap();
    assert_eq!(
        secret(&c, "llm.provider.p1.proxy_password").as_deref(),
        Some(PROXY_PASSWORD)
    );
    save_provider(&c, &with_proxy("p1", "http://proxy.test:8080"), 8).unwrap();
    assert_eq!(secret(&c, "llm.provider.p1.proxy_password"), None);
}

#[test]
fn provider_delete_secret_cascade_and_quirks_round_trip() {
    let c = connection();
    let mut p1 = with_header("p1", "X-Team", HEADER_VALUE);
    p1.quirks
        .extra_headers
        .insert("X-None".into(), String::new());
    p1.proxy_url = format!("http://u:{PROXY_PASSWORD}@proxy.test:8080");
    p1.balance_off = true;
    p1.endpoints[1].enabled = false;
    p1.endpoints[1].auth = Some(AuthStyle::XApiKey);
    p1.quirks.developer_role_as_system = Some(true);
    p1.quirks.stream_usage = Some(false);
    p1.quirks.max_tokens_field = Some(MaxTokensField::MaxCompletionTokens);
    p1.quirks.reasoning_request = Some(ReasoningStyle::None);
    let saved = save_provider(&c, &p1, 1).unwrap();

    set_provider_sniff(&c, "p1", &sniff(0)).unwrap();
    let reread = get_provider(&c, "p1").unwrap().unwrap();
    assert!(reread.balance_off && !reread.endpoints[1].enabled);
    assert_eq!(
        (reread.endpoints[1].auth, &reread.quirks),
        (Some(AuthStyle::XApiKey), &saved.quirks)
    );

    save_provider(&c, &with_header("p10", "X-Team", "other-value-1234"), 1).unwrap();
    save_key(&c, &key("k1", "p1"), Some("sk-first-123456"), 1).unwrap();
    set_key_management_token(&c, "k1", Some("console-token-1"), 1).unwrap();
    set_custom_balance(
        &c,
        "p1",
        Some(&query(&["X-Token"])),
        &values(&[("X-Token", HEADER_VALUE)]),
        2,
    )
    .unwrap();
    save_model(&c, &model("p1", "m")).unwrap();

    let with_bal = get_provider(&c, "p1").unwrap().unwrap();
    let hints = provider_header_hints(&c, &with_bal).unwrap();
    assert_eq!(hints.extra["X-None"], "");
    assert!(!hints.extra["X-Team"].is_empty() && !hints.balance["X-Token"].is_empty());
    assert!(!format!("{hints:?}").contains(HEADER_VALUE));

    assert!(matches!(
        delete_provider(&c, "nope"),
        Err(AppError::NotFound { .. })
    ));
    delete_provider(&c, "p1").unwrap();
    for secret_ref in [
        "llm.p1.header.X-Team",
        "llm.provider.p1.proxy_password",
        "llm.key.k1",
        "llm.key.k1.management_token",
        "llm.p1.balance.header.X-Token",
    ] {
        assert_eq!(secret(&c, secret_ref), None, "{secret_ref}");
    }
    assert_eq!(
        secret(&c, "llm.p10.header.X-Team").as_deref(),
        Some("other-value-1234")
    );
}

#[test]
fn providers_round_trip_and_keep_creation_time() {
    let c = connection();
    let saved = save_provider(&c, &single_endpoint_provider("p1"), 10).unwrap();
    assert_eq!(saved.created_at_ms, 10);
    let mut changed = saved.clone();
    changed.name = "Renamed".into();
    changed.auth = AuthStyle::XApiKey;
    let again = save_provider(&c, &changed, 20).unwrap();
    assert_eq!((again.created_at_ms, again.updated_at_ms), (10, 20));
    assert_eq!(again.auth, AuthStyle::XApiKey);
    assert_eq!(list_providers(&c).unwrap().len(), 1);
    assert!(
        save_provider(
            &c,
            &Provider {
                name: " ".into(),
                ..single_endpoint_provider("p2")
            },
            1
        )
        .is_err()
    );
    delete_provider(&c, "p1").unwrap();
    assert!(delete_provider(&c, "p1").is_err());
}

#[test]
fn deleting_a_provider_cascades_to_keys_models_and_targets() {
    let c = connection();
    save_provider(&c, &single_endpoint_provider("p1"), 1).unwrap();
    save_key(&c, &key("k1", "p1"), Some("sk-test"), 1).unwrap();
    save_model(&c, &fast_model("p1", "m", ModelSource::Manual)).unwrap();
    let m = ModelRef {
        provider_id: "p1".into(),
        model: "m".into(),
    };
    let other = ModelRef {
        provider_id: "p1".into(),
        model: "n".into(),
    };
    set_model_fallbacks(&c, &m, std::slice::from_ref(&other)).unwrap();
    save_legacy_name(&c, "fast", &m).unwrap();
    delete_provider(&c, "p1").unwrap();
    assert!(list_keys(&c, None).unwrap().is_empty());
    assert!(list_models(&c, None).unwrap().is_empty());
    assert!(list_fallbacks(&c).unwrap().is_empty());
    assert!(list_legacy_names(&c).unwrap().is_empty());
    assert!(save_key(&c, &key("k2", "p1"), Some("sk-test"), 1).is_err());
}
