//! Tests of the gateway lifecycle use cases: status, start/stop/restart, the autostart error,
//! the LAN rule, settings hooks, circuits and the event subscription.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::TcpListener;
use std::sync::Arc;
use std::time::Duration;

use serde_json::json;

use super::uc_support::{Fx, upstream_saying, wait_status, watch_status};
use super::{
    llm_gateway_autostart_impl, llm_gateway_check_settings_impl, llm_gateway_client_create_impl,
    llm_gateway_reset_circuits_impl, llm_gateway_restart_impl, llm_gateway_start_impl,
    llm_gateway_status_impl, llm_gateway_stop_impl, llm_gateway_stop_now_impl,
    llm_health_list_impl, llm_health_reset_impl, llm_subscribe_impl, llm_unsubscribe_impl,
};
use crate::app::settings_update_impl;
use crate::error::AppError;
use crate::events::Sequenced;
use crate::llm::events::LlmEvent;
use crate::llm::gateway::{GatewayState, tests::free_port};
use crate::llm::health::Outcome;
use crate::llm::runtime::circuit_config;
use crate::llm::store::ClientInput;
use crate::settings::SettingsSection;

fn any_client(fx: &Fx) {
    llm_gateway_client_create_impl(
        &fx.state,
        ClientInput {
            name: "Codex".to_owned(),
            allow_all: true,
            allowed_aliases: Vec::new(),
            allowed_models: Vec::new(),
        },
    )
    .unwrap();
}

fn patch_gateway(fx: &Fx, patch: serde_json::Value) -> Result<(), AppError> {
    settings_update_impl(&fx.state, SettingsSection::Ai, &json!({"gateway": patch})).map(|_| ())
}

#[test]
fn status_starts_stopped_and_start_stop_publish_their_states_in_order() {
    let fx = Fx::new();
    let heard = watch_status(&fx);
    let before = llm_gateway_status_impl(&fx.state);
    assert_eq!(before.state, GatewayState::Stopped);
    assert!(!before.running && before.port == 0 && before.base_url.is_empty());
    let started = llm_gateway_start_impl(&fx.state).unwrap();
    assert_eq!(started.state, GatewayState::Running);
    assert_eq!(
        started.base_url,
        format!("http://127.0.0.1:{}", started.port)
    );
    let stopped = llm_gateway_stop_impl(&fx.state);
    assert_eq!(stopped.state, GatewayState::Stopped);
    let states: Vec<GatewayState> = heard.try_iter().map(|status| status.state).collect();
    let running = states
        .iter()
        .position(|s| *s == GatewayState::Running)
        .unwrap();
    let stopping = states
        .iter()
        .position(|s| *s == GatewayState::Stopping)
        .unwrap();
    let stopped = states
        .iter()
        .rposition(|s| *s == GatewayState::Stopped)
        .unwrap();
    assert!(running < stopping && stopping < stopped, "{states:?}");
}

#[test]
fn starting_a_running_gateway_is_a_no_op_and_stop_now_stops_it() {
    let fx = Fx::new();
    let first = llm_gateway_start_impl(&fx.state).unwrap();
    let again = llm_gateway_start_impl(&fx.state).unwrap();
    assert_eq!(
        (first.port, first.started_at_ms),
        (again.port, again.started_at_ms)
    );
    let stopped = llm_gateway_stop_now_impl(&fx.state);
    assert_eq!(stopped.state, GatewayState::Stopped);
    assert_eq!(
        llm_gateway_stop_now_impl(&fx.state).state,
        GatewayState::Stopped
    );
}

#[test]
fn restart_returns_the_number_of_connections_it_dropped() {
    let fx = Fx::new();
    let port = fx.start();
    let http = reqwest::blocking::Client::builder()
        .no_proxy()
        .build()
        .unwrap();
    // The answered request leaves its connection open in the client's pool.
    let health = http
        .get(format!("http://127.0.0.1:{port}/health"))
        .send()
        .unwrap();
    assert_eq!(health.status(), 200);
    health.bytes().unwrap();
    let report = llm_gateway_restart_impl(&fx.state).unwrap();
    assert!(report.dropped_connections >= 1, "{report:?}");
    assert_eq!(report.status.state, GatewayState::Running);
    assert_eq!(report.status.port, port, "the same configured port");
}

#[test]
fn restarting_a_stopped_gateway_starts_it_and_drops_nothing() {
    let fx = Fx::new();
    let report = llm_gateway_restart_impl(&fx.state).unwrap();
    assert_eq!(report.dropped_connections, 0);
    assert!(report.status.running);
}

#[test]
fn a_port_in_use_is_kept_as_the_error_until_the_next_good_start() {
    let fx = Fx::new();
    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let busy = taken.local_addr().unwrap().port();
    patch_gateway(&fx, json!({"port": busy})).unwrap();
    let error = llm_gateway_start_impl(&fx.state).unwrap_err();
    assert!(matches!(error, AppError::Conflict { .. }), "{error:?}");
    let status = llm_gateway_status_impl(&fx.state);
    assert_eq!(status.state, GatewayState::Error);
    assert_eq!(
        status.message.as_deref(),
        Some(&*format!("port {busy} is in use"))
    );
    let last = status.last_error.expect("last error");
    assert_eq!((last.code.as_str(), last.port), ("port_in_use", busy));
    // The error stays on later reads, and the next successful start clears it.
    assert!(llm_gateway_status_impl(&fx.state).last_error.is_some());
    drop(taken);
    assert!(
        llm_gateway_start_impl(&fx.state)
            .unwrap()
            .last_error
            .is_none()
    );
}

#[test]
fn autostart_is_off_by_default_and_reports_a_failed_start_without_failing() {
    let fx = Fx::new();
    assert_eq!(llm_gateway_autostart_impl(&fx.state), None);
    assert!(!llm_gateway_status_impl(&fx.state).running);

    patch_gateway(&fx, json!({"enabled_on_start": true})).unwrap();
    assert_eq!(llm_gateway_autostart_impl(&fx.state), None);
    assert!(llm_gateway_status_impl(&fx.state).running);
    let _ = llm_gateway_stop_impl(&fx.state);

    let taken = TcpListener::bind("127.0.0.1:0").unwrap();
    let busy = taken.local_addr().unwrap().port();
    patch_gateway(&fx, json!({"port": busy})).unwrap();
    let error = llm_gateway_autostart_impl(&fx.state).expect("the failure is returned");
    assert_eq!((error.code.as_str(), error.port), ("port_in_use", busy));
    let status = llm_gateway_status_impl(&fx.state);
    assert_eq!(status.state, GatewayState::Error, "kept in the manager");
    assert_eq!(status.last_error, Some(error));
}

#[test]
fn serving_the_lan_needs_a_token_and_an_active_client() {
    let fx = Fx::new();
    let no_client = patch_gateway(&fx, json!({"serve_lan": true})).unwrap_err();
    assert!(
        matches!(&no_client, AppError::Validation { message, .. } if message.contains("client")),
        "{no_client:?}"
    );
    assert!(
        !fx.state.settings().ai.gateway.serve_lan,
        "nothing was stored"
    );
    any_client(&fx);
    let open = patch_gateway(&fx, json!({"serve_lan": true, "require_token": false})).unwrap_err();
    assert!(
        matches!(&open, AppError::Validation { message, .. } if message.contains("require_token")),
        "{open:?}"
    );
    patch_gateway(&fx, json!({"serve_lan": true})).unwrap();
    assert!(fx.state.settings().ai.gateway.serve_lan);
    // While LAN is on, "Require a token" cannot be turned off.
    assert!(patch_gateway(&fx, json!({"require_token": false})).is_err());
    assert!(fx.state.settings().ai.gateway.require_token);
    // Other changes of the gateway group are not blocked.
    patch_gateway(&fx, json!({"max_retries": 4})).unwrap();
}

#[test]
fn the_settings_check_refuses_lan_without_a_client_and_passes_loopback() {
    let fx = Fx::new();
    let mut next = (*fx.state.settings()).clone();
    assert!(llm_gateway_check_settings_impl(&fx.state, &next).is_ok());
    next.ai.gateway.serve_lan = true;
    assert!(matches!(
        llm_gateway_check_settings_impl(&fx.state, &next),
        Err(AppError::Validation { .. })
    ));
    any_client(&fx);
    assert!(llm_gateway_check_settings_impl(&fx.state, &next).is_ok());
    next.ai.gateway.require_token = false;
    let open = llm_gateway_check_settings_impl(&fx.state, &next).unwrap_err();
    assert!(
        matches!(&open, AppError::Validation { message, .. } if message.contains("Require a token")),
        "{open:?}"
    );
}

#[test]
fn a_port_change_restarts_a_running_gateway() {
    let fx = Fx::new();
    let heard = watch_status(&fx);
    fx.start();
    let next = free_port();
    patch_gateway(&fx, json!({"port": next})).unwrap();
    // The runtime restarts on a helper thread after the settings write.
    let moved = wait_status(&heard, |status| status.running && status.port == next);
    assert_eq!(moved.base_url, format!("http://127.0.0.1:{next}"));
}

#[tokio::test(flavor = "multi_thread")]
async fn reset_circuits_clears_the_breakers_and_health_calls_pass_through() {
    let fx = Fx::new();
    let upstream = upstream_saying("pong").await;
    let wired = fx.wire("smart", &upstream, &["m"]);
    let config = circuit_config(&fx.state.llm().settings().ai.gateway);
    for _ in 0..3 {
        fx.state.llm().health.record(
            &wired.key_id,
            "m",
            Outcome::Failure,
            Duration::from_millis(3),
            config,
        );
    }
    fx.state.llm().health.record(
        "other-key",
        "m",
        Outcome::Success,
        Duration::from_millis(3),
        config,
    );
    assert_eq!(llm_health_list_impl(&fx.state).len(), 2);
    llm_health_reset_impl(&fx.state, Some("other-key"));
    assert_eq!(llm_health_list_impl(&fx.state).len(), 1);
    llm_gateway_reset_circuits_impl(&fx.state);
    assert!(llm_health_list_impl(&fx.state).is_empty());
}

#[test]
fn a_subscriber_gets_the_events_after_from_seq_and_a_live_one_only_new_ones() {
    let fx = Fx::new();
    let hub = fx.state.llm().events();
    let first = hub.last_seq();
    fx.state.llm().emit(LlmEvent::AliasesChanged);
    fx.state.llm().emit(LlmEvent::ProvidersChanged);
    let (replayed, replay_rx) = std::sync::mpsc::channel();
    llm_subscribe_impl(
        &fx.state,
        Arc::new(move |event: Sequenced<LlmEvent>| replayed.send(event).is_ok()),
        Some(first),
    )
    .unwrap();
    let kinds: Vec<LlmEvent> = replay_rx.try_iter().map(|event| event.event).collect();
    assert_eq!(
        kinds,
        [LlmEvent::AliasesChanged, LlmEvent::ProvidersChanged]
    );
    let (live, live_rx) = std::sync::mpsc::channel();
    llm_subscribe_impl(
        &fx.state,
        Arc::new(move |event: Sequenced<LlmEvent>| live.send(event).is_ok()),
        None,
    )
    .unwrap();
    assert!(live_rx.try_recv().is_err(), "nothing from the past");
    fx.state.llm().emit(LlmEvent::AliasesChanged);
    assert_eq!(live_rx.try_recv().unwrap().event, LlmEvent::AliasesChanged);
    // Sequence numbers are strictly increasing without gaps.
    let seqs: Vec<u64> = replay_rx.try_iter().map(|event| event.seq).collect();
    assert_eq!(seqs.len(), 1);
    assert_eq!(seqs[0], first + 3);
}

#[test]
fn resuming_from_a_sequence_the_hub_no_longer_holds_announces_a_gap_first() {
    let fx = Fx::new();
    for _ in 0..600 {
        fx.state.llm().emit(LlmEvent::AliasesChanged);
    }
    let first = fx.state.llm().events().first_seq();
    let (sender, receiver) = std::sync::mpsc::channel();
    llm_subscribe_impl(
        &fx.state,
        Arc::new(move |event: Sequenced<LlmEvent>| sender.send(event).is_ok()),
        Some(1),
    )
    .unwrap();
    let seen: Vec<Sequenced<LlmEvent>> = receiver.try_iter().collect();
    assert_eq!(seen[0].event, LlmEvent::Gap { first_seq: first });
    assert_eq!(seen[0].seq, first - 1);
    assert_eq!(seen[1].seq, first, "the ring follows the gap");
    assert_eq!(
        seen.len() as u64,
        1 + fx.state.llm().events().last_seq() - first + 1
    );
}

#[test]
fn unsubscribing_ends_one_subscription_and_reports_a_stale_id() {
    let fx = Fx::new();
    let (sender, receiver) = std::sync::mpsc::channel();
    let id = llm_subscribe_impl(
        &fx.state,
        Arc::new(move |event: Sequenced<LlmEvent>| sender.send(event).is_ok()),
        None,
    )
    .unwrap();
    assert!(llm_unsubscribe_impl(&fx.state, id));
    assert!(!llm_unsubscribe_impl(&fx.state, id));
    fx.state.llm().emit(LlmEvent::AliasesChanged);
    assert!(receiver.try_recv().is_err());
}
