//! Tests of the balance refresh: happy path, throttle, keep-last-good, events, triggers, the
//! automatic refresh tick, secrecy and the shape of the request each key sends.

use serde_json::{Value, json};

use super::super::test_support::{
    MINUTE, NOW, SECRET, add_key, add_provider, deepseek, events_of, header, open, set_family,
    stored_keys,
};
use super::*;
use crate::app::settings_update_impl;
use crate::llm::balance_state::{BalanceScope, BalanceUnit};
use crate::llm::safe_http::SafeMethod;
use crate::llm::safe_http::scripted::{Reply, ScriptedHttp};
use crate::llm::wire::GatewayFamily;
use crate::settings::SettingsSection;
use crate::{SaveProviderKeyInput, llm_key_save_impl, llm_provider_save_impl};

pub(super) fn deepseek_body(total: &str) -> Value {
    json!({"is_available": true, "balance_infos": [
        {"currency": "CNY", "total_balance": total, "granted_balance": "0.00", "topped_up_balance": total}]})
}

pub(super) fn deepseek_http(total: &str) -> ScriptedHttp {
    ScriptedHttp::new().route(
        SafeMethod::Get,
        "/user/balance",
        vec![Reply::json(200, &deepseek_body(total))],
    )
}

pub(super) fn status_http(status: u16) -> ScriptedHttp {
    ScriptedHttp::new().route(
        SafeMethod::Get,
        "/user/balance",
        vec![Reply::json(status, &json!({"error": {"message": "nope"}}))],
    )
}

pub(super) async fn run(
    state: &AppState,
    provider_id: &str,
    key_id: Option<&str>,
    trigger: RefreshTrigger,
    now_ms: i64,
    http: &dyn SafeHttp,
) -> Result<Vec<KeyBalanceResultDto>, AppError> {
    refresh_provider(
        state,
        provider_id,
        key_id,
        trigger,
        &CancelToken::new(),
        now_ms,
        Some(http),
    )
    .await
}

fn set_interval(state: &AppState, minutes: u16) {
    settings_update_impl(
        state,
        SettingsSection::Ai,
        &json!({"providers": {"balance_refresh_min": minutes}}),
    )
    .expect("setting");
}

#[tokio::test]
async fn refresh_reads_stores_throttles_and_keeps_last_good_on_failure() {
    let state = open();
    let (provider, key) = deepseek(&state);
    let seen = events_of(&state);
    let http = deepseek_http("86.40");
    let first = run(
        &state,
        &provider.id,
        None,
        RefreshTrigger::Manual,
        NOW,
        &http,
    )
    .await
    .expect("refresh");
    assert_eq!(first[0].key_id, key.id);
    assert!(first[0].refreshed && !first[0].throttled);
    let report = first[0].report.as_ref().expect("report");
    assert_eq!(
        (report.unit, report.remaining, report.scope),
        (BalanceUnit::Cny, Some(86.4), BalanceScope::Account)
    );
    assert_eq!(
        header(&http.requests()[0], "authorization"),
        Some(format!("Bearer {SECRET}"))
    );
    assert!(
        seen.lock()
            .unwrap()
            .contains(&LlmEvent::ProviderStatusChanged {
                provider_id: provider.id.clone()
            })
    );

    // Throttled within 30s.
    let second = run(
        &state,
        &provider.id,
        None,
        RefreshTrigger::Manual,
        NOW + 10_000,
        &http,
    )
    .await
    .expect("throttled");
    assert!(second[0].throttled && !second[0].refreshed);
    assert_eq!(http.count(), 1);

    // Failure keeps last good value and records the error.
    let failed = run(
        &state,
        &provider.id,
        None,
        RefreshTrigger::Manual,
        NOW + MINUTE,
        &status_http(401),
    )
    .await
    .expect("failed");
    assert_eq!(failed[0].report, first[0].report);
    assert_eq!(
        failed[0].error.as_ref().map(|e| e.kind),
        Some(BalanceErrorKind::Auth)
    );
    assert_eq!(
        stored_keys(&state, &provider.id)[0]
            .balance
            .as_ref()
            .unwrap()
            .last_good,
        first[0].report
    );
}

#[tokio::test]
async fn done_hub_management_token_and_redaction_never_leak_secrets() {
    let state = open();
    let provider = add_provider(&state, "Hub", "https://hub.example.net", None);
    set_family(&state, &provider.id, GatewayFamily::DoneHub, true);
    llm_key_save_impl(
        &state,
        SaveProviderKeyInput {
            provider_id: provider.id.clone(),
            name: "k".to_owned(),
            secret: Some(SECRET.to_owned()),
            api_user_id: Some("5".to_owned()),
            management_token: Some("console-token-AAAA".to_owned()),
            ..Default::default()
        },
    )
    .expect("key");
    let http = ScriptedHttp::new()
        .route(
            SafeMethod::Get,
            "/api/user/self",
            vec![Reply::json(
                200,
                &json!({"success": true, "data": {"id": 5, "quota": 6_200_000, "used_quota": 1_000_000}}),
            )],
        )
        .route(
            SafeMethod::Get,
            "/api/status",
            vec![Reply::json(200, &json!({"data": {"quota_per_unit": 500_000}}))],
        );
    let results = run(
        &state,
        &provider.id,
        None,
        RefreshTrigger::Manual,
        NOW,
        &http,
    )
    .await
    .expect("refresh");
    assert_eq!(
        results[0].report.as_ref().unwrap().remaining,
        Some(6_200_000.0)
    );
    let text = serde_json::to_string(&results).expect("json");
    assert!(!text.contains(SECRET) && !text.contains("console-token-AAAA"));

    // Vendor echoing secret in error message is redacted.
    let (ds, _) = deepseek(&state);
    let echo = ScriptedHttp::new().route(
        SafeMethod::Get,
        "/user/balance",
        vec![Reply::json(
            401,
            &json!({"error": {"message": format!("bad key: {SECRET}")}}),
        )],
    );
    let echoed = run(&state, &ds.id, None, RefreshTrigger::Manual, NOW, &echo)
        .await
        .expect("echo");
    assert!(!serde_json::to_string(&echoed).unwrap().contains(SECRET));
}

#[tokio::test]
async fn scheduler_tick_and_balance_off_respect_settings_and_provider_state() {
    let state = open();
    let (provider, _) = deepseek(&state);
    let plain = add_provider(&state, "Plain", "https://llm.plain.example", None);
    add_key(&state, &plain.id, "k", SECRET, 0, true);

    assert!(
        llm_provider_balance_impl(&state, "ghost", None, true, None)
            .await
            .is_err()
    );
    assert!(
        llm_balance_refresh_if_due_impl(
            &state,
            "ghost",
            RefreshTrigger::AfterAdd,
            &CancelToken::new(),
        )
        .await
        .is_err()
    );

    let http = deepseek_http("1.00");
    let cancel = CancelToken::new();
    assert_eq!(
        tick(&state, NOW, &cancel, Some(&http)).await.expect("off"),
        0
    );
    set_interval(&state, 15);
    assert_eq!(
        tick(&state, NOW, &cancel, Some(&http)).await.expect("due"),
        1
    );
    assert_eq!(
        tick(&state, NOW + MINUTE, &cancel, Some(&http))
            .await
            .expect("not due"),
        0
    );

    let mut off = provider.clone();
    off.balance_off = true;
    llm_provider_save_impl(&state, off).expect("save");
    assert!(
        llm_provider_balance_impl(&state, &provider.id, None, true, None)
            .await
            .unwrap()
            .is_empty()
    );

    start_balance_scheduler(&state).expect("start");
    stop_balance_scheduler(&state);
}
