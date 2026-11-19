//! The stored evidence behind the provider status.

use rusqlite::{Connection, params};

use super::{connection, key, model, provider, seeded};
use crate::llm::balance_state::{
    BalanceErrorKind, BalanceErrorRecord, BalanceReport, BalanceScope, BalanceUnit, StoredBalance,
};
use crate::llm::failure::FailureKind;
use crate::llm::probe::run::CellRecord;
use crate::llm::probe::{ProbeStatus, ProbeTest};
use crate::llm::status::{BalanceSignal, GatewayOutcome, ProviderStatus, provider_status};
use crate::llm::store::{
    ProbeResult, persist_cell, provider_status_facts, save_key, save_model, save_probe_result,
    save_provider, set_key_balance,
};
use crate::llm::wire::{AuthStyle, Protocol, ProviderEndpoint};

const NOW: i64 = 1_800_000_000_000;
const MINUTE: i64 = 60_000;
const HOUR: i64 = 60 * MINUTE;

fn setup() -> Connection {
    let c = connection();
    seeded(&c, "p1", "k1");
    save_model(&c, &model("p1", "m1")).unwrap();
    save_model(&c, &model("p1", "m2")).unwrap();
    c
}

fn facts_of(c: &Connection, provider_id: &str) -> crate::llm::status::StatusInputs {
    provider_status_facts(c, NOW)
        .unwrap()
        .into_iter()
        .find(|i| i.provider_id == provider_id)
        .unwrap()
}

fn store_result(
    c: &Connection,
    key_id: &str,
    model_id: &str,
    test: ProbeTest,
    status: ProbeStatus,
    at: i64,
) {
    let ok = status == ProbeStatus::Ready;
    save_probe_result(
        c,
        &ProbeResult {
            key_id: key_id.into(),
            model_id: model_id.into(),
            endpoint: "openai_chat".into(),
            test: test.as_str().into(),
            ok,
            status: Some(if ok { 200 } else { 500 }),
            ttft_ms: ok.then_some(100),
            status_word: status.as_str().into(),
            checked_at_ms: at,
            ..ProbeResult::default()
        },
    )
    .unwrap();
}

fn log_row(
    c: &Connection,
    ts_ms: i64,
    status: i64,
    outcome: &str,
    stream: bool,
    ttft: Option<i64>,
    source: &str,
) {
    c.execute(
        "INSERT INTO llm_request_log (ts_ms, protocol_in, requested_model, provider_id, key_id, status, \
             outcome, stream, latency_ms, ttft_ms, source, failure_kind) \
         VALUES (?1, 'openai_chat', 'm1', 'p1', 'k1', ?2, ?3, ?4, 100, ?5, ?6, NULL)",
        params![ts_ms, status, outcome, i64::from(stream), ttft, source],
    )
    .unwrap();
}

fn used_up(scope: BalanceScope) -> StoredBalance {
    StoredBalance {
        last_good: Some(BalanceReport {
            supported: true,
            unit: BalanceUnit::Usd,
            remaining: Some(0.0),
            exhausted: true,
            scope,
            checked_at_ms: NOW - HOUR,
            ..BalanceReport::default()
        }),
        ..StoredBalance::default()
    }
}

#[test]
fn provider_facts_auth_connectivity_and_decisive_tests() {
    let c = setup();
    let facts = facts_of(&c, "p1");
    assert!(facts.enabled && !facts.auth_none);
    assert_eq!(
        (
            facts.key_auth.as_slice(),
            facts.newest_connectivity,
            facts.models.len()
        ),
        (&[None][..], None, 2)
    );
    assert_eq!(
        provider_status(&facts, NOW).status,
        ProviderStatus::Unverified
    );

    let mut first = provider("p0");
    first.position = -1;
    first.auth = AuthStyle::None;
    save_provider(&c, &first, 1).unwrap();
    let all = provider_status_facts(&c, NOW).unwrap();
    assert_eq!(all[0].provider_id, "p0");
    assert!(all[0].auth_none && all[0].key_auth.is_empty());

    let mut p_off = provider("p1");
    for ep in &mut p_off.endpoints {
        ep.enabled = false;
    }
    save_provider(&c, &p_off, 2).unwrap();
    assert!(!facts_of(&c, "p1").enabled);
    save_provider(&c, &provider("p1"), 3).unwrap();

    // Key auth, connectivity, and model evidence timestamps.
    save_key(&c, &key("k2", "p1"), Some("sk-second-key-1"), 1).unwrap();
    let mut disabled = key("k3", "p1");
    disabled.enabled = false;
    save_key(&c, &disabled, Some("sk-third-key-12"), 1).unwrap();

    store_result(
        &c,
        "k1",
        "m1",
        ProbeTest::Connectivity,
        ProbeStatus::Ready,
        NOW - 3 * HOUR,
    );
    store_result(
        &c,
        "k1",
        "m1",
        ProbeTest::Auth,
        ProbeStatus::Rejected,
        NOW - 2 * HOUR,
    );
    store_result(
        &c,
        "k1",
        "m2",
        ProbeTest::Auth,
        ProbeStatus::Ready,
        NOW - HOUR,
    );
    store_result(
        &c,
        "k2",
        "m1",
        ProbeTest::Auth,
        ProbeStatus::Rejected,
        NOW - HOUR,
    );
    store_result(
        &c,
        "k3",
        "m1",
        ProbeTest::Auth,
        ProbeStatus::Ready,
        NOW - 2 * HOUR,
    );

    let facts = facts_of(&c, "p1");
    assert_eq!(
        facts.key_auth,
        vec![Some(ProbeStatus::Ready), Some(ProbeStatus::Rejected)]
    );
    assert_eq!(
        (
            facts.newest_connectivity,
            facts.newest_passing_test_ms,
            facts.newest_ready_model_test_ms
        ),
        (Some(ProbeStatus::Ready), Some(NOW - HOUR), None)
    );

    store_result(
        &c,
        "k1",
        "m1",
        ProbeTest::Stream,
        ProbeStatus::Ready,
        NOW - 30 * MINUTE,
    );
    store_result(
        &c,
        "k1",
        "m2",
        ProbeTest::Tools,
        ProbeStatus::Ready,
        NOW - 10 * MINUTE,
    );
    assert_eq!(
        facts_of(&c, "p1").newest_ready_model_test_ms,
        Some(NOW - 30 * MINUTE)
    );

    // Decisive results follow model kind and classify refusals.
    let mut with_dec = provider("p1");
    with_dec.endpoints.push(ProviderEndpoint {
        protocol: Protocol::DecisionSystemone,
        path_prefix: "/v1".into(),
        auth: None,
        url_mode: Default::default(),
        enabled: true,
    });
    save_provider(&c, &with_dec, 4).unwrap();
    save_model(&c, &model("p1", "text-embedding-3")).unwrap();
    save_model(&c, &model("p1", "jev-latest")).unwrap();

    store_result(
        &c,
        "k1",
        "m1",
        ProbeTest::Chat,
        ProbeStatus::Ready,
        NOW - 3 * HOUR,
    );
    store_result(
        &c,
        "k1",
        "m1",
        ProbeTest::Embeddings,
        ProbeStatus::Rejected,
        NOW - HOUR,
    );
    store_result(
        &c,
        "k1",
        "text-embedding-3",
        ProbeTest::Chat,
        ProbeStatus::Rejected,
        NOW - HOUR,
    );
    store_result(
        &c,
        "k1",
        "text-embedding-3",
        ProbeTest::Embeddings,
        ProbeStatus::Ready,
        NOW - HOUR,
    );
    store_result(
        &c,
        "k1",
        "jev-latest",
        ProbeTest::Chat,
        ProbeStatus::Rejected,
        NOW - HOUR,
    );
    store_result(
        &c,
        "k1",
        "jev-latest",
        ProbeTest::Decision,
        ProbeStatus::Ready,
        NOW - MINUTE,
    );
    save_probe_result(
        &c,
        &ProbeResult {
            key_id: "k1".into(),
            model_id: "m2".into(),
            endpoint: "openai_chat".into(),
            test: "chat".into(),
            ok: false,
            status: Some(422),
            status_word: "ready".into(),
            checked_at_ms: NOW - HOUR,
            ..ProbeResult::default()
        },
    )
    .unwrap();

    let facts = facts_of(&c, "p1");
    assert_eq!(facts.models.len(), 4);
    assert!(
        facts
            .models
            .iter()
            .any(|m| m.newest_test == Some(ProbeStatus::InvalidResponse) && m.request_refused)
    );
    assert_eq!(
        facts
            .models
            .iter()
            .filter(|m| m.newest_test == Some(ProbeStatus::Ready))
            .count(),
        3
    );
}

#[test]
fn gateway_traffic_feeds_the_status() {
    let c = setup();
    store_result(
        &c,
        "k1",
        "m1",
        ProbeTest::Auth,
        ProbeStatus::Ready,
        NOW - HOUR,
    );
    let cell = CellRecord {
        provider_id: "p1".into(),
        key_id: "k1".into(),
        model_id: "m1".into(),
        test: ProbeTest::Stream,
        status: ProbeStatus::Ready,
        reason: String::new(),
        skip: None,
        result: Some(ProbeResult {
            key_id: "k1".into(),
            model_id: "m1".into(),
            endpoint: "openai_chat".into(),
            test: "stream".into(),
            ok: true,
            ttft_ms: Some(300),
            checked_at_ms: NOW - MINUTE,
            ..ProbeResult::default()
        }),
        capability: None,
    };
    persist_cell(&c, "run-1", &cell, NOW).unwrap();
    assert_eq!(facts_of(&c, "p1").test_ttft_ms, vec![300]);

    log_row(&c, NOW - 25 * HOUR, 200, "success", false, None, "gateway");
    log_row(&c, NOW - MINUTE, 200, "success", true, Some(50), "probe");
    assert_eq!(facts_of(&c, "p1").newest_gateway_success_ms, None);
    // The agent's and the workbench's forwarded requests count as the health card counts them.
    log_row(
        &c,
        NOW - 10 * MINUTE,
        200,
        "success",
        true,
        Some(50),
        "agent",
    );
    assert_eq!(
        facts_of(&c, "p1").newest_gateway_success_ms,
        Some(NOW - 10 * MINUTE + 100)
    );

    for (n, kind) in [
        (5, Some(FailureKind::Network)),
        (4, Some(FailureKind::Timeout)),
        (3, None),
    ] {
        c.execute(
            "INSERT INTO llm_request_log (ts_ms, protocol_in, requested_model, provider_id, status, \
                 outcome, failure_kind, source) VALUES (?1, 'openai_chat', 'm1', 'p1', 502, 'failure', ?2, 'gateway')",
            params![NOW - n * MINUTE, kind.map(FailureKind::as_str)],
        )
        .unwrap();
    }
    let facts = facts_of(&c, "p1");
    assert!(
        facts
            .recent_gateway
            .iter()
            .all(|a| a.outcome == GatewayOutcome::TransportFailure)
    );
    assert_eq!(
        provider_status(&facts, NOW).status,
        ProviderStatus::Unreachable
    );

    for n in 0..22 {
        log_row(
            &c,
            NOW - (n + 1) * 1_000,
            200,
            "success",
            true,
            Some(400 + n),
            "gateway",
        );
    }
    let facts = facts_of(&c, "p1");
    assert_eq!(facts.gateway_ttft_ms.len(), 20);
    assert_eq!(provider_status(&facts, NOW).status, ProviderStatus::Healthy);
}

#[test]
fn balance_signals_count_exhausted_keys_unless_balance_is_off() {
    let c = setup();
    save_key(&c, &key("k2", "p1"), Some("sk-second-key-1"), 1).unwrap();
    set_key_balance(&c, "k1", &used_up(BalanceScope::Key), 1).unwrap();
    set_key_balance(&c, "k2", &used_up(BalanceScope::Key), 1).unwrap();
    assert_eq!(facts_of(&c, "p1").balance.exhausted_keys, 2);
    // A reading older than a day says nothing about the balance now.
    let mut old = used_up(BalanceScope::Key);
    if let Some(report) = old.last_good.as_mut() {
        report.checked_at_ms = NOW - 2 * 24 * HOUR;
    }
    set_key_balance(&c, "k2", &old, 1).unwrap();
    assert_eq!(facts_of(&c, "p1").balance.exhausted_keys, 1);

    set_key_balance(
        &c,
        "k1",
        &StoredBalance {
            last_good: used_up(BalanceScope::Account).last_good,
            error: Some(BalanceErrorRecord {
                at_ms: 1,
                kind: BalanceErrorKind::Auth,
                status: Some(401),
                message: "no".into(),
                retry_at_ms: None,
            }),
            stale: true,
        },
        2,
    )
    .unwrap();
    let bal = facts_of(&c, "p1").balance;
    assert!(bal.account_exhausted && bal.auth_failed);

    let mut off = provider("p1");
    off.balance_off = true;
    save_provider(&c, &off, 3).unwrap();
    assert_eq!(facts_of(&c, "p1").balance, BalanceSignal::default());
}

#[test]
fn gateway_traffic_queries_use_the_provider_index() {
    let c = setup();
    for sql in [
        crate::llm::store::facts::GATEWAY_TTFT_SQL,
        crate::llm::store::facts::GATEWAY_RECENT_SQL,
        crate::llm::store::facts::GATEWAY_SUCCESS_SQL,
    ] {
        let mut stmt = c.prepare(&format!("EXPLAIN QUERY PLAN {sql}")).unwrap();
        let text = stmt
            .query_map(params!["p1", 0_i64], |r| r.get::<_, String>(3))
            .unwrap()
            .map(Result::unwrap)
            .collect::<Vec<_>>()
            .join(" | ");
        assert!(
            text.contains("idx_llm_request_log_provider") && !text.contains("SCAN llm_request_log")
        );
    }
}
