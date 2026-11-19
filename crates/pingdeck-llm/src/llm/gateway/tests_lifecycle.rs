//! Tests of the gateway life cycle: graceful drain, `stop_now`, LAN dependency rules, last
//! errors and the status DTO.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::{Duration, Instant};

use serde_json::{Value, json};
use tokio::net::TcpStream;

use crate::llm::events::LlmEvent;
use crate::llm::gateway::{GatewayManager, GatewayState, GatewayStatus};
use crate::llm::runtime::{DbHandle, LlmRuntime};
use crate::llm::store::{self, ClientInput};
use crate::llm::wire::Protocol;
use pingdeck_base::error::AppError;
use pingdeck_model::settings::Settings;
use pingdeck_store::secrets_sqlite::SqliteSecretStore;

use super::tests::{Canned, Fake, Harness, Reply, Step, TOKEN, canned_sse, mac_lan};

fn plain_client() -> reqwest::Client {
    reqwest::Client::builder().no_proxy().build().unwrap()
}

fn chat(model: &str, stream: bool) -> Value {
    json!({"model": model, "stream": stream, "messages": [{"role": "user", "content": "hi"}]})
}

fn serving(upstream: &Fake, limits: impl FnOnce(&mut super::Limits)) -> Harness {
    let h = Harness::build(|_| {});
    h.llm.gateway.set_all_limits(limits);
    h.llm.gateway.start(&h.llm).unwrap();
    h.add_provider(
        "a",
        upstream.port,
        &[Protocol::OpenaiChat],
        &["ka"],
        &["ma"],
    );
    h.add_alias("fast", &[("a", None, "ma")]);
    h
}

async fn until(h: &Harness, what: &str, done: impl Fn(&GatewayStatus) -> bool) -> GatewayStatus {
    let deadline = Instant::now() + Duration::from_secs(10);
    loop {
        let status = h.llm.gateway.status(&h.llm);
        if done(&status) {
            return status;
        }
        assert!(
            Instant::now() < deadline,
            "never saw: {what}; last {status:?}"
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

pub(super) async fn until_requests(upstream: &Fake, count: usize) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while upstream.count() < count {
        assert!(
            Instant::now() < deadline,
            "upstream got {} requests",
            upstream.count()
        );
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_drains_running_streams_and_stop_now_aborts_immediately() {
    let pause = Duration::from_millis(1500);
    let upstream = Fake::start(move |_, _| {
        let mut steps = canned_sse(Protocol::OpenaiChat, "up", &Canned::text(&["a", "b"]));
        steps.insert(1, Step::Pause(pause));
        Reply::Sse(steps)
    })
    .await;
    let h = serving(&upstream, |_| {});
    let port = h.port();
    let auth = format!("Bearer {TOKEN}");
    let mut clients = tokio::task::JoinSet::new();
    for _ in 0..3 {
        let url = h.url("/v1/chat/completions");
        let auth = auth.clone();
        clients.spawn(async move {
            let resp = plain_client()
                .post(url)
                .header("authorization", auth)
                .header("content-type", "application/json")
                .body(serde_json::to_vec(&chat("fast", true)).unwrap())
                .send()
                .await
                .unwrap();
            (resp.status().as_u16(), resp.text().await.unwrap())
        });
    }
    until_requests(&upstream, 3).await;
    let llm = Arc::clone(&h.llm);
    let started = Instant::now();
    let stopper = tokio::task::spawn_blocking(move || llm.gateway.stop(&llm));

    let stopping = until(&h, "stopping", |s| s.state == GatewayState::Stopping).await;
    assert_eq!((stopping.in_flight, stopping.port), (3, port));
    assert!(!stopping.running && stopping.base_url.is_empty());

    let deadline = Instant::now() + Duration::from_secs(5);
    while TcpStream::connect(("127.0.0.1", port)).await.is_ok() {
        assert!(Instant::now() < deadline);
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    while let Some(done) = clients.join_next().await {
        let (status, body) = done.unwrap();
        assert!(status == 200 && body.contains("[DONE]"));
    }
    assert_eq!(stopper.await.unwrap().state, GatewayState::Stopped);
    assert!(started.elapsed() >= Duration::from_millis(1400));
    for row in &h.logs(3).await {
        assert_eq!(
            (row.outcome.as_str(), row.status, row.error.as_deref()),
            ("success", 200, None)
        );
    }

    // stop_now aborts an in-progress drain at once.
    let hung = Fake::always(Reply::Hang).await;
    let h2 = serving(&hung, |limits| limits.drain = Duration::from_secs(60));
    let url = h2.url("/v1/chat/completions");
    let client = tokio::spawn(async move {
        plain_client()
            .post(url)
            .header("authorization", auth)
            .header("content-type", "application/json")
            .body(serde_json::to_vec(&chat("fast", false)).unwrap())
            .send()
            .await
            .is_err()
    });
    until_requests(&hung, 1).await;
    let llm = Arc::clone(&h2.llm);
    let draining = tokio::task::spawn_blocking(move || llm.gateway.stop(&llm));
    until(&h2, "stopping", |s| s.state == GatewayState::Stopping).await;
    let llm = Arc::clone(&h2.llm);
    let stopping = tokio::task::spawn_blocking(move || llm.gateway.stop_now(&llm));
    until(&h2, "stopped", |s| s.state == GatewayState::Stopped).await;
    assert_eq!(stopping.await.unwrap().state, GatewayState::Stopped);
    assert_eq!(draining.await.unwrap().state, GatewayState::Stopped);
    assert!(client.await.unwrap());
    let rows = h2.logs(1).await;
    assert_eq!(
        (
            rows[0].outcome.as_str(),
            rows[0].error.as_deref(),
            rows[0].provider_id.as_deref(),
            rows[0].key_id.as_deref(),
        ),
        ("canceled", Some("gateway stopped"), Some("a"), Some("ka"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn lan_requirements_auto_fallback_and_unavailable_db() {
    let h = Harness::build(|g| g.serve_lan = true);
    store::revoke_client(&h.db, "gc_base", 1).unwrap();
    h.llm.touch();
    let err = h.llm.gateway.start(&h.llm).unwrap_err();
    assert_eq!(
        err.message(),
        "Local network access needs at least one active client: connect a client first"
    );
    assert_eq!(
        h.llm.gateway.status(&h.llm).last_error.unwrap().code,
        "invalid_settings"
    );
    drop(h);

    let h = Harness::build(|g| {
        g.serve_lan = true;
        g.require_token = false;
    });
    let err = h.llm.gateway.start(&h.llm).unwrap_err();
    assert_eq!(
        err.message(),
        "Local network access needs tokens: turn on Require a token first"
    );
    let mut wanted = h.llm.settings().ai.gateway.clone();
    assert!(GatewayManager::validate_lan(&h.llm, &wanted).is_err());
    wanted.require_token = true;
    assert!(GatewayManager::validate_lan(&h.llm, &wanted).is_ok());
    wanted.serve_lan = false;
    wanted.require_token = false;
    assert!(GatewayManager::validate_lan(&h.llm, &wanted).is_ok());
    drop(h);

    // Revoking the last active client turns LAN off and restarts on loopback.
    let h = Harness::build(|g| g.serve_lan = true);
    h.llm.gateway.pin_lan(mac_lan());
    h.llm.gateway.start(&h.llm).unwrap();
    let before = h.llm.gateway.status(&h.llm);
    assert_eq!(
        (before.lan, before.state, before.clients),
        (true, GatewayState::Running, 1)
    );
    assert_eq!(
        before
            .addresses
            .iter()
            .map(|r| r.host.as_str())
            .collect::<Vec<_>>(),
        ["192.168.1.20", "mac.local"]
    );
    let second = store::create_client(
        &h.db,
        &ClientInput {
            name: "second".into(),
            allow_all: true,
            allowed_aliases: Vec::new(),
            allowed_models: Vec::new(),
        },
        0,
    )
    .unwrap();
    store::revoke_client(&h.db, "gc_base", 1).unwrap();
    assert!(!h.llm.gateway.on_clients_changed(&h.llm) && h.llm.gateway.status(&h.llm).lan);
    store::revoke_client(&h.db, &second.client.id, 2).unwrap();
    assert!(h.llm.gateway.on_clients_changed(&h.llm));
    let after = h.llm.gateway.status(&h.llm);
    assert_eq!(
        (after.lan, after.state, after.clients),
        (false, GatewayState::Running, 0)
    );
    assert!(after.addresses.is_empty() && !h.llm.settings().ai.gateway.serve_lan);
    let announced = h
        .events
        .lock()
        .unwrap()
        .iter()
        .rev()
        .find_map(|e| match e {
            LlmEvent::GatewayStatus { status } => Some(status.clone()),
            _ => None,
        })
        .unwrap();
    assert_eq!(
        announced.notice.as_deref(),
        Some("Local network turned off: no active clients")
    );
    // The same words ride a notice event the UI keeps on screen.
    assert!(h.events.lock().unwrap().iter().any(|e| matches!(
        e,
        LlmEvent::GatewayNotice {
            code: super::GatewayNoticeCode::LanOff,
            ..
        }
    )));

    // Unreachable database -> database_unavailable.
    let mut settings = Settings::default();
    settings.ai.gateway.port = 0;
    let llm = Arc::new(
        LlmRuntime::new(
            DbHandle::Unavailable,
            Arc::new(settings),
            Arc::new(SqliteSecretStore::in_memory().unwrap()),
        )
        .unwrap(),
    );
    assert!(matches!(
        llm.gateway.start(&llm),
        Err(AppError::Unsupported { .. })
    ));
    assert_eq!(
        llm.gateway.status(&llm).last_error.unwrap().code,
        "database_unavailable"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn gwy3_a_stored_open_tunnel_starts_with_tokens_required_and_says_why() {
    let h = Harness::build(|_| {});
    let stored =
        json!({"gateway": {"require_token": false, "extra_allowed_hosts": ["abc.ngrok.app"]}});
    let row = pingdeck_model::stored_setting::StoredSetting {
        section: "ai".into(),
        version: pingdeck_model::settings::SETTINGS_VERSION,
        value_json: stored.to_string(),
    };
    {
        let store = pingdeck_store::storage::WorkspaceStore::open(&h.path).unwrap();
        store.save_app_setting(&row, 0).unwrap();
    }
    let mut loaded = Settings::from_rows(&[row]);
    assert!(loaded.ai.gateway.require_token && loaded.ai.gateway.extra_allowed_hosts.len() == 1);
    loaded.ai.gateway.port = 0;
    h.llm.set_settings(Arc::new(loaded));

    let status = h.llm.gateway.start(&h.llm).unwrap();
    assert_eq!(
        (
            status.running,
            status.token_required,
            status.notice.as_deref()
        ),
        (true, true, Some(super::startup::FORCED_TOKEN_NOTICE))
    );
    // A view opened later still reads why.
    assert_eq!(
        h.llm.gateway.status(&h.llm).notice.as_deref(),
        Some(super::startup::FORCED_TOKEN_NOTICE)
    );
    assert_eq!(h.send("GET", "/v1/models", &[], None).await.status, 401);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_port_change_restarts_the_running_gateway_once_even_when_two_writes_race() {
    let h = Harness::start(|_| {});
    let free = std::net::TcpListener::bind("127.0.0.1:0").unwrap();
    let port = free.local_addr().unwrap().port();
    drop(free);
    h.events.lock().unwrap().clear();
    h.set_gateway(|g| g.port = port);
    // Two settings writes in a row each spawn one apply (`LlmRuntime::attach`).
    let applies: Vec<_> = (0..2)
        .map(|_| {
            let llm = Arc::clone(&h.llm);
            std::thread::spawn(move || llm.gateway.apply_settings(&llm))
        })
        .collect();
    for apply in applies {
        apply.join().unwrap().unwrap();
    }
    let status = h.llm.gateway.status(&h.llm);
    assert_eq!((status.state, status.port), (GatewayState::Running, port));
    let starts = h
        .events
        .lock()
        .unwrap()
        .iter()
        .filter(|e| matches!(e, LlmEvent::GatewayStatus { status } if status.state == GatewayState::Starting))
        .count();
    assert_eq!(starts, 1);
}
