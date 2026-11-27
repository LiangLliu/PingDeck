//! Server lifecycle: start, stop, restart, bounded concurrency and asynchronous latency.

use std::net::{SocketAddr, TcpListener, TcpStream};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use super::super::server::{MAX_CONNECTIONS, MockServerManager};
use super::support::*;

/// Serialises the tests that open ~30+ simultaneous loopback connections so parallel test threads
/// cannot stack their socket pairs above a 256-FD shell soft limit.
static CONNECTION_BURST: Mutex<()> = Mutex::new(());

fn lock_connection_burst() -> MutexGuard<'static, ()> {
    CONNECTION_BURST
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
}

#[test]
fn server_lifecycle_restarts_races_and_callback_cancellation_hold() {
    use crate::workspace::items::MockCallback;
    use crate::workspace::models::HttpMethod;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .build()
        .unwrap();
    let dropped_bound =
        runtime.block_on(async { MockServerManager::new().start(server(Vec::new())).unwrap() });
    assert!(!reachable(dropped_bound));

    let manager = Arc::new(MockServerManager::new());
    let route_ping = || {
        route(
            "r",
            "GET",
            "/ping",
            vec![response("p", 200, "pong", vec![])],
        )
    };
    let first = manager.start(server(vec![route_ping()])).unwrap();
    let mut restart = server(vec![route_ping()]);
    restart.port = first.port();
    let second = manager.start(restart).unwrap();
    assert_eq!((second, get(second, "/ping").2.as_str()), (first, "pong"));
    let third = manager.start(server(vec![route_ping()])).unwrap();
    assert!(third != first && !reachable(first) && reachable(third));
    assert!(manager.stop("mock_flow") && !reachable(third));
    TcpListener::bind(third).expect("port freed immediately on stop");

    let starters: Vec<_> = (0..2)
        .map(|_| {
            let m = Arc::clone(&manager);
            std::thread::spawn(move || m.start(server(Vec::new())).unwrap())
        })
        .collect();
    let mut addrs: Vec<SocketAddr> = starters.into_iter().map(|s| s.join().unwrap()).collect();
    addrs.sort();
    addrs.dedup();
    let live = manager.bound_addr("mock_flow").unwrap();
    assert!(addrs.contains(&live) && addrs.iter().filter(|a| reachable(**a)).count() == 1);

    let fixed_port = free_port();
    let starters: Vec<_> = (0..2)
        .map(|_| {
            let m = Arc::clone(&manager);
            std::thread::spawn(move || {
                let mut cfg = server(Vec::new());
                cfg.port = fixed_port;
                m.start(cfg)
            })
        })
        .collect();
    for s in starters {
        s.join().unwrap().unwrap();
    }
    assert!(reachable(manager.bound_addr("mock_flow").unwrap()));
    manager.stop("mock_flow");

    let collector_manager = MockServerManager::new();
    let mut collector = server(vec![route(
        "route_collect",
        "POST",
        "/collect",
        vec![response("resp_collect", 200, "collected", vec![])],
    )]);
    collector.id = "mock_collector".into();
    let collector_addr = collector_manager.start(collector).unwrap();
    let trigger = |id: &str, label: &str, delay_ms: u64| {
        let mut r = route(
            "route_trigger",
            "GET",
            "/trigger",
            vec![response("resp_trigger", 200, "ok", vec![])],
        );
        r.responses[0].callback_ids = vec!["callback_1".into()];
        let mut cfg = server(vec![r]);
        cfg.id = id.into();
        cfg.callbacks = vec![MockCallback {
            id: "callback_1".into(),
            name: "late".into(),
            documentation: String::new(),
            method: HttpMethod::Post,
            url: format!("http://{collector_addr}/collect"),
            delay_ms,
            headers: Vec::new(),
            body: label.into(),
        }];
        cfg
    };
    let stopped = manager
        .start(trigger("mock_stopped", "stopped", 200))
        .unwrap();
    let control = manager
        .start(trigger("mock_control", "control", 400))
        .unwrap();
    assert_eq!(
        (
            get(stopped, "/trigger").2.as_str(),
            get(control, "/trigger").2.as_str()
        ),
        ("ok", "ok")
    );
    assert!(manager.stop("mock_stopped"));
    wait_until("control callback", || {
        !collector_manager.logs("mock_collector").is_empty()
    });
    let bodies: Vec<String> = collector_manager
        .logs("mock_collector")
        .into_iter()
        .map(|l| l.request_body)
        .collect();
    assert_eq!(bodies, ["control"]);
    assert_eq!(manager.stop_all(), 1);
    assert_eq!(manager.stop_all(), 0);
}

#[test]
fn connection_cap_flood_recovery_and_async_latency_hold() {
    use std::io::Read;

    let _burst = lock_connection_burst();
    let mut slow = response("resp_slow", 200, "slow", vec![]);
    slow.latency_ms = 300;
    let manager = MockServerManager::new();
    let bound = manager
        .start(server(vec![
            route(
                "route_ok",
                "GET",
                "/ok",
                vec![response("resp_ok", 200, "ok", vec![])],
            ),
            route("route_slow", "GET", "/slow", vec![slow]),
        ]))
        .unwrap();

    let started = Instant::now();
    let clients: Vec<_> = (0..30)
        .map(|_| std::thread::spawn(move || get(bound, "/slow").0))
        .collect();
    for c in clients {
        assert_eq!(c.join().unwrap(), 200);
    }
    assert!(started.elapsed() < Duration::from_millis(1500));

    let base_accepted = accepted_connections(&manager, "mock_flow");
    let holders: Vec<_> = (0..MAX_CONNECTIONS)
        .map(|_| TcpStream::connect(bound).unwrap())
        .collect();
    wait_until("all holders accepted", || {
        accepted_connections(&manager, "mock_flow") >= base_accepted + MAX_CONNECTIONS as u64
    });
    let (status, _, body) = get(bound, "/ok");
    assert!(status == 503 && body.contains("too_many_connections"));

    let flood_start = Instant::now();
    for _ in 0..5 {
        let flood: Vec<_> = (0..20)
            .map(|_| TcpStream::connect_timeout(&bound, Duration::from_secs(2)).unwrap())
            .collect();
        for mut socket in flood {
            socket
                .set_read_timeout(Some(Duration::from_secs(2)))
                .unwrap();
            let mut sink = Vec::new();
            if let Err(err) = socket.read_to_end(&mut sink) {
                assert!(!matches!(
                    err.kind(),
                    std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
                ));
            }
        }
    }
    assert!(flood_start.elapsed() < Duration::from_secs(5));

    drop(holders);
    wait_until("capacity to recover", || get(bound, "/ok").0 == 200);
}

#[test]
fn foreign_hosts_and_busy_connections_are_logged_without_hits() {
    use crate::workspace::items::MockLogOutcome;

    let _burst = lock_connection_burst();
    let manager = MockServerManager::new();
    let bound = manager
        .start(server(vec![route(
            "route_ok",
            "GET",
            "/ok",
            vec![response("resp_ok", 200, "ok", vec![])],
        )]))
        .unwrap();
    let status = send(bound, "GET /ok?x=1 HTTP/1.1\r\nHost: evil.example\r\n\r\n").0;
    assert_eq!(status, 403);

    let base_accepted = accepted_connections(&manager, "mock_flow");
    let holders: Vec<_> = (0..MAX_CONNECTIONS)
        .map(|_| TcpStream::connect(bound).unwrap())
        .collect();
    wait_until("all holders accepted", || {
        accepted_connections(&manager, "mock_flow") >= base_accepted + MAX_CONNECTIONS as u64
    });
    assert_eq!(get(bound, "/ok").0, 503);
    drop(holders);

    let logs = manager.logs("mock_flow");
    let outcomes: Vec<_> = logs.iter().map(|row| row.outcome).collect();
    assert_eq!(
        outcomes,
        [
            Some(MockLogOutcome::RejectedHost),
            Some(MockLogOutcome::Busy)
        ]
    );
    assert_eq!(
        (logs[0].path.as_str(), logs[0].query.as_str()),
        ("/ok", "x=1")
    );
    assert_eq!(logs[1].status_code, 503);
    assert_eq!(manager.stats("mock_flow").unwrap().hit_count, 0);
}

#[test]
fn reset_state_rewinds_cursors_and_request_numbers_but_keeps_live_data() {
    use crate::workspace::items::{
        MockDataBucket, MockResponseMode, MockRuleOperator, MockRuleTarget, MockSelectionKind,
    };

    let mut sequence = route(
        "route_seq",
        "GET",
        "/seq",
        vec![response("resp_a", 200, "a", vec![]), {
            let mut b = response("resp_b", 200, "b", vec![]);
            b.position = 1;
            b
        }],
    );
    sequence.response_mode = MockResponseMode::Sequential;
    let first_call = rule(
        MockRuleTarget::RequestNumber,
        "",
        MockRuleOperator::Equals,
        "1",
    );
    let numbered = route(
        "route_nth",
        "GET",
        "/nth",
        vec![response("resp_first", 201, "first", vec![first_call]), {
            let mut later = response("resp_later", 200, "later", vec![]);
            later.is_default = true;
            later.position = 1;
            later
        }],
    );
    let mut config = server(vec![
        sequence,
        numbered,
        crud_route("route_users", "/users", "b_users"),
    ]);
    config.data_buckets = vec![MockDataBucket {
        id: "b_users".into(),
        name: "users".into(),
        documentation: String::new(),
        value_json: "[]".into(),
    }];
    let manager = MockServerManager::new();
    let bound = manager.start(config).unwrap();

    assert_eq!(get(bound, "/seq").2, "a");
    assert_eq!(get(bound, "/nth").0, 201);
    assert_eq!(get(bound, "/nth").0, 200);
    let created = send_with_body(bound, "POST", "/users", r#"{"id":"u1"}"#).0;
    assert_eq!(created, 201);
    let rows = manager.logs("mock_flow");
    let chosen = rows[1].selected_by.as_ref().unwrap();
    assert_eq!(chosen.kind, MockSelectionKind::Rule);
    assert_eq!(
        (chosen.rule_number, chosen.received.as_deref()),
        (Some(1), Some("1"))
    );
    assert_eq!(
        rows[2].selected_by.as_ref().unwrap().kind,
        MockSelectionKind::Default
    );

    assert!(manager.reset_state("mock_flow"));
    assert!(!manager.reset_state("mock_missing"));
    assert_eq!(manager.stats("mock_flow").unwrap().hit_count, 0);
    assert_eq!(get(bound, "/seq").2, "a");
    assert_eq!(get(bound, "/nth").0, 201);
    let users = &manager.live_data("mock_flow").unwrap()[0];
    assert!(users.changed && users.value_json.contains("u1"));
}
