//! Tests of the provider status and health use-cases: the evidence rows, balance
//! scope rules, circuits, the change event and the Overview health card.

use std::time::Duration;

use super::super::test_support::{
    SECRET, add_key, add_model, add_provider, deepseek, events_of, insert_request, open,
    provider_template,
};
use super::*;
use crate::llm::balance_state::{
    BalanceErrorKind, BalanceErrorRecord, BalanceReport, BalanceScope, BalanceUnit, StoredBalance,
};
use crate::llm::capture::PhaseValue;
use crate::llm::capture_record::AttemptTiming;
use crate::llm::health::{CircuitConfig, Outcome};
use crate::llm::status::{ProviderStatus, ReasonCode};
use crate::llm::store::set_key_balance;
use crate::llm::wire::AuthStyle;
use crate::{llm_provider_delete_impl, llm_provider_save_impl};

const HOUR: i64 = 60 * 60 * 1000;

async fn status_of(state: &AppState, provider_id: &str) -> ProviderStatusDto {
    llm_provider_status_list_impl(state)
        .await
        .expect("list")
        .into_iter()
        .find(|status| status.provider_id == provider_id)
        .expect("status")
}

fn codes(status: &ProviderStatusDto) -> Vec<ReasonCode> {
    status.reasons.iter().map(|reason| reason.code).collect()
}

fn balance(
    state: &AppState,
    key_id: &str,
    report: Option<BalanceReport>,
    error: Option<BalanceErrorKind>,
) {
    let stored = StoredBalance {
        last_good: report,
        error: error.map(|kind| BalanceErrorRecord {
            at_ms: 1,
            kind,
            status: None,
            message: "x".to_owned(),
            retry_at_ms: None,
        }),
        stale: false,
    };
    set_key_balance(state.store().connection(), key_id, &stored, 1).expect("balance");
}

fn exhausted(scope: BalanceScope) -> BalanceReport {
    BalanceReport {
        supported: true,
        scope,
        unit: BalanceUnit::Usd,
        remaining: Some(0.0),
        exhausted: true,
        checked_at_ms: 1,
        ..BalanceReport::default()
    }
}

#[tokio::test]
async fn provider_status_tracks_keys_evidence_balance_circuits_and_never_leaks_secrets() {
    let state = open();
    let (provider, key) = deepseek(&state);
    add_model(&state, &provider.id, "m1", true);

    let initial = status_of(&state, &provider.id).await;
    assert_eq!(initial.status, ProviderStatus::Unverified);
    assert_eq!(codes(&initial), [ReasonCode::NoModelAnswered]);

    let mut local = provider_template("Ollama", "http://127.0.0.1:11434", None);
    local.auth = AuthStyle::None;
    let local = llm_provider_save_impl(&state, local).expect("local");
    assert_eq!(
        codes(&status_of(&state, &local.id).await),
        [ReasonCode::NoModelAnswered]
    );

    for _ in 0..5 {
        insert_request(&state, &provider.id, |entry| {
            entry.stream = true;
            entry.latency_ms = 400;
            entry.ttft_ms = Some(400);
        });
    }
    let healthy = status_of(&state, &provider.id).await;
    assert_eq!(healthy.status, ProviderStatus::Healthy);
    assert_eq!(healthy.latency_ms, Some(400));

    let circuit = CircuitConfig {
        failure_threshold: 1,
        ..CircuitConfig::default()
    };
    state.llm.health.record(
        &key.id,
        "m1",
        Outcome::Failure,
        Duration::from_millis(5),
        circuit,
    );
    assert_eq!(
        codes(&status_of(&state, &provider.id).await),
        [ReasonCode::CircuitOpen]
    );

    balance(
        &state,
        &key.id,
        Some(exhausted(BalanceScope::Account)),
        Some(BalanceErrorKind::Auth),
    );
    let list = llm_provider_status_list_impl(&state).await.expect("list");
    assert!(!serde_json::to_string(&list).unwrap().contains(SECRET));
}

#[tokio::test]
async fn status_recompute_emits_only_on_changes_and_deletion() {
    let state = open();
    let (provider, _) = deepseek(&state);
    let seen = events_of(&state);
    llm_status_recompute_impl(&state, &provider.id)
        .await
        .expect("first");
    llm_status_recompute_impl(&state, &provider.id)
        .await
        .expect("same");
    assert_eq!(seen.lock().unwrap().len(), 1);

    llm_provider_delete_impl(&state, &provider.id).expect("delete");
    llm_status_recompute_impl(&state, &provider.id)
        .await
        .expect("gone");
    assert_eq!(
        seen.lock().unwrap().last(),
        Some(&LlmEvent::ProviderStatusChanged {
            provider_id: provider.id
        })
    );
}

#[tokio::test]
async fn health_card_aggregates_24h_metrics_timings_and_open_circuits() {
    let state = open();
    let (provider, key) = deepseek(&state);
    let other = add_provider(&state, "Other", "https://o.example.net", None);
    add_key(&state, &other.id, "k", "sk-other-0123456789abcdef", 0, true);
    let now = crate::app::current_time_ms();
    for ttft in (1..=10).map(|n| n * 100) {
        insert_request(&state, &provider.id, |entry| {
            entry.stream = true;
            entry.ttft_ms = Some(ttft);
            entry.timing = Some(TimingJson {
                queue_ms: 4,
                attempts: vec![AttemptTiming {
                    dns: PhaseValue::NotMeasured,
                    connect: PhaseValue::Ms(50),
                    tls: PhaseValue::NotMeasured,
                    connect_includes_tls: true,
                    headers_ms: PhaseValue::Ms(200),
                    first_token_ms: PhaseValue::Ms(40),
                    end_ms: PhaseValue::Ms(500),
                    reused: false,
                }],
            });
        });
    }
    insert_request(&state, &provider.id, |entry| {
        entry.status = 429;
        entry.outcome = "failure".to_owned();
        entry.ts_ms = now - HOUR / 2;
    });
    state.llm.health.record(
        &key.id,
        "deepseek-v4-pro",
        Outcome::Failure,
        Duration::from_millis(5),
        CircuitConfig {
            failure_threshold: 1,
            ..CircuitConfig::default()
        },
    );

    let health = llm_provider_health_impl(&state, &provider.id)
        .await
        .expect("health");
    assert!(health.enough_requests);
    assert_eq!(health.requests, 11);
    assert_eq!(health.rate_limited_by_hour[23], 1);
    assert_eq!(health.typical_request.connect_ms, Some(50));
    assert_eq!(health.open_circuits.len(), 1);
    assert_eq!(median(vec![4, 1, 3, 2]), Some(3));
    assert_eq!(percentile((1..=20).collect(), 95), Some(19));
    assert_eq!(typical_request(&[]), TypicalRequestDto::default());
}
