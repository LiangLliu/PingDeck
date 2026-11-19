//! Tests of the start routine against temp states: what it does, what it reports, and that the
//! delay hook never holds the call back.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::TcpListener;
use std::time::{Duration, Instant};

use serde_json::json;

use super::uc_support::{Fx, wait_status, watch_status};
use super::{
    GatewayStart, StartOptions, llm_gateway_status_impl, llm_gateway_stop_impl, llm_log_count_impl,
    llm_start_routine_impl, stop_balance_scheduler,
};
use crate::app::{AppState, current_time_ms, settings_update_impl};
use crate::llm::gateway::GatewayState;
use crate::llm::store::{self, LogWrite, RequestLogEntry};
use crate::settings::SettingsSection;

const OFFLINE: StartOptions = StartOptions {
    gateway_start_delay: None,
    skip_catalog_refresh: true,
};

fn autostart(fx: &Fx) {
    settings_update_impl(
        &fx.state,
        SettingsSection::Ai,
        &json!({"gateway": {"enabled_on_start": true}}),
    )
    .unwrap();
}

fn row(ts_ms: i64) -> RequestLogEntry {
    RequestLogEntry {
        ts_ms,
        protocol_in: "openai_chat".to_owned(),
        requested_model: "smart".to_owned(),
        status: 200,
        outcome: "success".to_owned(),
        request_id: Some(format!("rq_{:08x}", ts_ms.unsigned_abs() % 0xffff_ffff)),
        ..RequestLogEntry::default()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn the_routine_prepares_the_ai_layer_and_reports_it() {
    let fx = Fx::new();
    let report = llm_start_routine_impl(&fx.state, OFFLINE).await;
    assert_eq!(report.gateway, GatewayStart::Off);
    assert!(report.catalog_ready);
    assert!(report.balance_scheduler_started);
    assert_eq!(report.tz_offset_minutes, fx.state.llm().tz_offset_minutes());
    assert_eq!(report.recovered_probe_runs, 0);
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    assert!(!report.catalog_refresh_started);
    assert!(!llm_gateway_status_impl(&fx.state).running);
    stop_balance_scheduler(&fx.state);
}

#[tokio::test(flavor = "multi_thread")]
async fn autostart_starts_the_gateway_when_the_setting_is_on() {
    let fx = Fx::new();
    autostart(&fx);
    let report = llm_start_routine_impl(&fx.state, OFFLINE).await;
    assert_eq!(report.gateway, GatewayStart::Started);
    assert!(llm_gateway_status_impl(&fx.state).running);
    stop_balance_scheduler(&fx.state);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_gateway_that_cannot_start_is_reported_and_kept_in_its_status_without_a_warning() {
    let fx = Fx::new();
    autostart(&fx);
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let busy = taken.local_addr().unwrap().port();
    settings_update_impl(
        &fx.state,
        SettingsSection::Ai,
        &json!({"gateway": {"port": busy}}),
    )
    .unwrap();
    let report = llm_start_routine_impl(&fx.state, OFFLINE).await;
    let GatewayStart::Failed { error } = report.gateway else {
        panic!("{:?}", report.gateway);
    };
    assert_eq!((error.code.as_str(), error.port), ("port_in_use", busy));
    let status = llm_gateway_status_impl(&fx.state);
    assert_eq!(status.state, GatewayState::Error);
    assert_eq!(status.last_error, Some(error));
    assert!(report.warnings.is_empty());
    assert!(report.balance_scheduler_started, "the rest still ran");
    stop_balance_scheduler(&fx.state);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_five_second_delay_hook_does_not_delay_the_call() {
    let fx = Fx::new();
    autostart(&fx);
    let started = Instant::now();
    let report = llm_start_routine_impl(
        &fx.state,
        StartOptions {
            gateway_start_delay: Some(Duration::from_secs(5)),
            skip_catalog_refresh: true,
        },
    )
    .await;
    assert!(
        started.elapsed() < Duration::from_secs(4),
        "{:?}",
        started.elapsed()
    );
    assert_eq!(report.gateway, GatewayStart::Scheduled { delay_ms: 5_000 });
    assert!(!llm_gateway_status_impl(&fx.state).running, "not yet");
    assert!(
        report.catalog_ready && report.balance_scheduler_started,
        "everything else ran"
    );
    stop_balance_scheduler(&fx.state);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_delayed_start_happens_after_the_delay_and_announces_itself() {
    let fx = Fx::new();
    autostart(&fx);
    let heard = watch_status(&fx);
    let report = llm_start_routine_impl(
        &fx.state,
        StartOptions {
            gateway_start_delay: Some(Duration::from_millis(150)),
            skip_catalog_refresh: true,
        },
    )
    .await;
    assert!(matches!(report.gateway, GatewayStart::Scheduled { .. }));
    let running = wait_status(&heard, |status| status.running);
    assert!(running.port > 0);
    stop_balance_scheduler(&fx.state);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_delay_hook_with_autostart_off_schedules_nothing() {
    let fx = Fx::new();
    let report = llm_start_routine_impl(
        &fx.state,
        StartOptions {
            gateway_start_delay: Some(Duration::from_millis(50)),
            skip_catalog_refresh: true,
        },
    )
    .await;
    assert_eq!(report.gateway, GatewayStart::Off);
    stop_balance_scheduler(&fx.state);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_first_retention_pass_prunes_rows_older_than_thirty_days() {
    let fx = Fx::new();
    let now = current_time_ms();
    let day = 86_400_000;
    {
        let store = fx.state.store();
        for ts in [now - 31 * day, now - 40 * day, now - 90 * day, now - 1_000] {
            store::insert_log(
                store.connection(),
                &LogWrite {
                    entry: &row(ts),
                    capture: None,
                    rollup: None,
                },
            )
            .unwrap();
        }
    }
    let report = llm_start_routine_impl(&fx.state, OFFLINE).await;
    assert_eq!(report.log_rows_pruned, 3);
    assert_eq!(llm_log_count_impl(&fx.state, None).unwrap(), 1);
    stop_balance_scheduler(&fx.state);
}

#[tokio::test(flavor = "multi_thread")]
async fn running_the_routine_twice_keeps_one_balance_scheduler() {
    let fx = Fx::new();
    let first = llm_start_routine_impl(&fx.state, OFFLINE).await;
    let second = llm_start_routine_impl(&fx.state, OFFLINE).await;
    assert!(first.balance_scheduler_started && second.balance_scheduler_started);
    assert!(second.warnings.is_empty(), "{:?}", second.warnings);
    stop_balance_scheduler(&fx.state);
}

#[tokio::test(flavor = "multi_thread")]
async fn an_in_memory_state_runs_the_routine_and_reports_the_gateway_it_cannot_start() {
    let state = AppState::open_in_memory().unwrap();
    let off = llm_start_routine_impl(&state, OFFLINE).await;
    assert_eq!(off.gateway, GatewayStart::Off);
    assert!(off.catalog_ready);
    settings_update_impl(
        &state,
        SettingsSection::Ai,
        &json!({"gateway": {"enabled_on_start": true}}),
    )
    .unwrap();
    let on = llm_start_routine_impl(&state, OFFLINE).await;
    let GatewayStart::Failed { error } = on.gateway else {
        panic!("{:?}", on.gateway);
    };
    assert!(!error.message.is_empty());
    stop_balance_scheduler(&state);
    let _ = llm_gateway_stop_impl(&state);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_routine_leaves_provider_rows_alone_because_migration_v36_owns_the_proxy_split() {
    let fx = Fx::new();
    let plain = "http://user:secret@proxy.test:8080";
    fx.state
        .store()
        .connection()
        .execute(
            "INSERT INTO llm_providers (id, name, base_url, proxy_url, created_at_ms, updated_at_ms)
             VALUES ('p', 'P', 'https://x.test', ?1, 1, 1)",
            [plain],
        )
        .unwrap();

    let report = llm_start_routine_impl(&fx.state, OFFLINE).await;

    let stored: String = fx
        .state
        .store()
        .connection()
        .query_row(
            "SELECT proxy_url FROM llm_providers WHERE id = 'p'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(stored, plain, "no start-time scrub");
    assert!(report.warnings.is_empty(), "{:?}", report.warnings);
    stop_balance_scheduler(&fx.state);
}
