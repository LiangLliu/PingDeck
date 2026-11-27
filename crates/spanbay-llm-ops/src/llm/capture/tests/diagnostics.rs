//! The diagnostics bundle.

use std::collections::BTreeMap;

use serde_json::Value;

use super::{PD_TOKEN, SK_KEY, captured};
use crate::llm::capture::{
    AppDiag, CaptureMode, ClientDiag, DIAGNOSTICS_MAX_BYTES, DIAGNOSTICS_MAX_ROWS,
    DiagnosticsInput, DiagnosticsRefusal, FallbackDiag, GatewayDiag, LogSummaryDiag, ProviderDiag,
    RowDiag, build_diagnostics,
};
use crate::llm::health::{CircuitState, HealthSnapshot};

fn row(i: usize, error: Option<String>) -> RowDiag {
    RowDiag {
        request_id: format!("rq_{i:08x}"),
        ts_ms: 1_000_000 - i as i64,
        source: "gateway".to_owned(),
        client_name: Some("Claude Code".to_owned()),
        alias: Some("claude-code".to_owned()),
        requested_model: "claude-code".to_owned(),
        upstream_model: Some("deepseek-v4-pro".to_owned()),
        provider_name: Some("DeepSeek".to_owned()),
        protocol_in: "anthropic_messages".to_owned(),
        protocol_out: Some("openai_chat".to_owned()),
        status: if error.is_some() { 429 } else { 200 },
        outcome: "success".to_owned(),
        stream: true,
        translated: true,
        latency_ms: 880,
        ttft_ms: Some(412),
        input_tokens: Some(18_204),
        output_tokens: Some(912),
        cost_micro: Some(23_100),
        cost_currency: Some("USD".to_owned()),
        failure_kind: None,
        error,
    }
}

fn input() -> DiagnosticsInput {
    DiagnosticsInput {
        app: AppDiag {
            version: "1.0.0".to_owned(),
            os: "macos".to_owned(),
            arch: "aarch64".to_owned(),
            language: "en".to_owned(),
        },
        gateway: GatewayDiag {
            state: "running".to_owned(),
            bind_scope: "loopback".to_owned(),
            port: 4141,
            token_required: true,
            lan: false,
            allowed_hosts: 1,
            capture_policy: CaptureMode::ErrorsOnly,
            uptime_s: Some(3600),
            in_flight: 1,
            rows_dropped: 0,
            captures_dropped: 0,
            auth_failures: 2,
            last_start_error: Some("upstream timeout".to_owned()),
        },
        providers: vec![ProviderDiag {
            id: "p1".to_owned(),
            name: "DeepSeek".to_owned(),
            protocols: vec!["openai_chat".to_owned()],
            enabled_keys: 1,
            enabled_models: 4,
            probe_status: Some("ready".to_owned()),
            base_url: Some("https://user:pw@relay.acme.dev/v1?key=abc#x".to_owned()),
        }],
        fallbacks: vec![FallbackDiag {
            model: "p1/deepseek-v4-pro".to_owned(),
            fallbacks: vec!["p2/deepseek-v4-pro".to_owned()],
        }],
        clients: vec![ClientDiag {
            name: "Claude Code".to_owned(),
            allow_summary: "all models".to_owned(),
            created_at_ms: 1,
            last_used_at_ms: Some(2),
            revoked: false,
        }],
        health: vec![HealthSnapshot {
            key_id: "k1".to_owned(),
            model_id: "deepseek-v4-pro".to_owned(),
            state: CircuitState::Closed,
            consecutive_failures: 0,
            failure_rate: 0.0,
            latency_ms: 800.0,
            total: 12,
            open_until_ms: None,
            cooldown_s: 30,
            retry_in_s: None,
            opened_for_ms: None,
            label: "Healthy".to_owned(),
        }],
        log_summary: LogSummaryDiag {
            by_status_class: BTreeMap::from([("2xx".to_owned(), 40), ("4xx".to_owned(), 2)]),
            by_provider: BTreeMap::from([("DeepSeek".to_owned(), 42)]),
            by_source: BTreeMap::from([("gateway".to_owned(), 42)]),
            ttft_p50_ms: Some(400),
            ttft_p95_ms: Some(900),
            latency_p50_ms: Some(800),
            latency_p95_ms: Some(2000),
        },
        recent_rows: (0..3).map(|i| row(i, None)).collect(),
        include_provider_addresses: false,
        selected_capture: None,
        needles: vec!["relay-9f8e7d6c5b4a".to_owned()],
    }
}

fn parse(text: &str) -> Value {
    serde_json::from_str(text).unwrap()
}

#[test]
fn the_bundle_has_every_section_and_no_capture_by_default() {
    let mut data = input();
    data.gateway.rows_dropped = 3;
    data.gateway.captures_dropped = 7;
    let value = parse(&build_diagnostics(&data).unwrap());
    for section in [
        "app",
        "gateway",
        "providers",
        "fallbacks",
        "clients",
        "health",
        "log_summary",
        "recent_rows",
    ] {
        assert!(value.get(section).is_some(), "{section}");
    }
    assert!(value.get("capture").is_none());
    assert_eq!(value["gateway"]["capture_policy"], "errors_only");
    assert_eq!(value["gateway"]["port"], 4141);
    assert_eq!(value["providers"][0]["enabled_models"], 4);
    assert_eq!(value["fallbacks"][0]["fallbacks"][0], "p2/deepseek-v4-pro");
    assert_eq!(value["health"][0]["state"], "closed");
    assert_eq!(value["recent_rows"].as_array().unwrap().len(), 3);
    // The gateway counts dropped rows and captures separately.
    assert_eq!(value["gateway"]["rows_dropped"], 3);
    assert_eq!(value["gateway"]["captures_dropped"], 7);
    assert_eq!(value["gateway"]["allowed_hosts"], 1);
    assert!(value["gateway"].get("last_start_error").is_some());
}

#[test]
fn clients_carry_no_token_material() {
    let text = build_diagnostics(&input()).unwrap();
    let clients = &parse(&text)["clients"][0];
    let keys: Vec<&str> = clients
        .as_object()
        .unwrap()
        .keys()
        .map(String::as_str)
        .collect();
    assert_eq!(
        keys,
        [
            "allow_summary",
            "created_at_ms",
            "last_used_at_ms",
            "name",
            "revoked"
        ]
    );
}

#[test]
fn provider_addresses_appear_only_with_the_switch_and_without_user_info_or_query() {
    let text = build_diagnostics(&input()).unwrap();
    assert!(!text.contains("base_url") && !text.contains("relay.acme.dev"));
    let mut with = input();
    with.include_provider_addresses = true;
    let text = build_diagnostics(&with).unwrap();
    assert_eq!(
        parse(&text)["providers"][0]["base_url"],
        "https://relay.acme.dev/v1"
    );
    assert!(!text.contains("user:pw") && !text.contains("key=abc"));
}

#[test]
fn bodies_are_excluded_unless_the_single_row_switch_is_on() {
    let text = build_diagnostics(&input()).unwrap();
    assert!(!text.contains("body"), "a body string exists");
    let mut with = input();
    with.selected_capture = Some(captured());
    let text = build_diagnostics(&with).unwrap();
    let capture = &parse(&text)["capture"];
    assert_eq!(capture["notice"], "This includes prompts and replies.");
    assert_eq!(capture["request_log_id"], 7);
    assert!(
        capture["upstream_response"]["text"]
            .as_str()
            .unwrap()
            .contains("pong")
    );
    assert!(
        !text.contains("Bearer") && !text.contains("session="),
        "headers leaked"
    );
}

#[test]
fn a_provider_key_in_an_error_text_refuses_the_bundle() {
    let mut leaky = input();
    leaky.gateway.last_start_error = Some(format!("401 from upstream for key {SK_KEY}"));
    let refusal = build_diagnostics(&leaky).unwrap_err();
    assert_eq!(refusal, DiagnosticsRefusal::Unsafe);
    assert_eq!(
        refusal.message(),
        "Diagnostics could not be made safe. Nothing was copied."
    );
    assert_eq!(refusal.to_string(), refusal.message());
}

#[test]
fn an_exact_needle_or_a_token_in_a_row_error_refuses_the_bundle() {
    let mut leaky = input();
    leaky.recent_rows = vec![row(0, Some("bad key relay-9f8e7d6c5b4a".to_owned()))];
    assert_eq!(build_diagnostics(&leaky), Err(DiagnosticsRefusal::Unsafe));
    leaky.recent_rows = vec![row(0, Some(format!("client {PD_TOKEN} rejected")))];
    assert_eq!(build_diagnostics(&leaky), Err(DiagnosticsRefusal::Unsafe));
    leaky.recent_rows = vec![row(
        0,
        Some("Authorization: Bearer abcdefghijklmnopqrstuv".to_owned()),
    )];
    assert_eq!(build_diagnostics(&leaky), Err(DiagnosticsRefusal::Unsafe));
}

#[test]
fn an_already_redacted_error_text_passes_the_check() {
    let mut clean = input();
    clean.gateway.last_start_error = Some("401 for key ••••••".to_owned());
    clean.recent_rows = vec![row(0, Some("Authorization: ••••••".to_owned()))];
    assert!(build_diagnostics(&clean).is_ok());
}

#[test]
fn five_thousand_rows_give_the_fifty_newest_within_the_size_cap() {
    let mut big = input();
    big.recent_rows = (0..5_000).map(|i| row(i, None)).collect();
    let text = build_diagnostics(&big).unwrap();
    assert!(text.len() <= DIAGNOSTICS_MAX_BYTES);
    let rows = parse(&text)["recent_rows"].as_array().unwrap().clone();
    assert_eq!(rows.len(), DIAGNOSTICS_MAX_ROWS);
    assert_eq!(rows[0]["request_id"], "rq_00000000");
    assert_eq!(rows[49]["request_id"], "rq_00000031");
}

#[test]
fn oversized_rows_are_trimmed_from_the_oldest_end() {
    let mut big = input();
    big.recent_rows = (0..50).map(|i| row(i, Some("x".repeat(20_000)))).collect();
    let text = build_diagnostics(&big).unwrap();
    assert!(text.len() <= DIAGNOSTICS_MAX_BYTES, "{}", text.len());
    let rows = parse(&text)["recent_rows"].as_array().unwrap().clone();
    assert!(!rows.is_empty() && rows.len() < 50, "{}", rows.len());
    for (i, row) in rows.iter().enumerate() {
        assert_eq!(row["request_id"], format!("rq_{i:08x}"));
    }
}

#[test]
fn a_bundle_too_large_even_without_rows_is_refused() {
    let mut huge = input();
    huge.gateway.last_start_error = Some("x".repeat(DIAGNOSTICS_MAX_BYTES));
    let refusal = build_diagnostics(&huge).unwrap_err();
    assert_eq!(refusal, DiagnosticsRefusal::TooLarge);
    assert!(refusal.message().contains("256 KB"));
}

#[test]
fn the_bundle_is_deterministic_json() {
    let first = build_diagnostics(&input()).unwrap();
    assert_eq!(first, build_diagnostics(&input()).unwrap());
    assert!(serde_json::from_str::<Value>(&first).is_ok());
}

#[test]
fn provider_addresses_elide_token_like_path_segments() {
    let mut with = input();
    with.include_provider_addresses = true;
    with.providers[0].base_url =
        Some("https://relay.acme.dev/t/AbCdEfGhIjKlMnOpQrStUvWx/v1?key=1".to_owned());
    let text = build_diagnostics(&with).unwrap();
    assert_eq!(
        parse(&text)["providers"][0]["base_url"],
        "https://relay.acme.dev/t/…/v1"
    );
    assert!(!text.contains("AbCdEfGhIjKl"));
}

#[test]
fn a_url_with_a_token_path_in_an_error_text_refuses_the_bundle() {
    let mut leaky = input();
    leaky.recent_rows = vec![row(
        0,
        Some("GET https://relay.acme.dev/t/AbCdEfGhIjKlMnOpQrStUvWx/v1/models failed".to_owned()),
    )];
    assert_eq!(build_diagnostics(&leaky), Err(DiagnosticsRefusal::Unsafe));
}

#[test]
fn the_bundle_with_a_capture_matches_the_golden_text() {
    let mut with = input();
    with.selected_capture = Some(captured());
    assert_eq!(
        build_diagnostics(&with).unwrap(),
        include_str!("golden/diagnostics.json")
    );
}
