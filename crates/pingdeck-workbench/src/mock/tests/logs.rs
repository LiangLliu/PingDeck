//! Request log truncation tests.

use crate::workspace::items::MOCK_LOG_BODY_LIMIT_BYTES;

use super::super::server::MockServerManager;
use super::support::*;

#[test]
fn log_truncation_header_budget_capacity_shrink_and_subscriptions_work() {
    use super::super::MockSubscribeError;
    use super::super::server::MAX_LOG_SUBSCRIBERS;
    use crate::events::{EventSink, MockLogEvent};
    use crate::workspace::models::HISTORY_HEADERS_LIMIT_BYTES;
    use std::sync::Arc;
    use std::sync::atomic::{AtomicUsize, Ordering};

    let huge_response = "€".repeat(30_000);
    let config = server(vec![
        route(
            "route_huge",
            "POST",
            "/huge",
            vec![response("resp_huge", 200, &huge_response, vec![])],
        ),
        route(
            "route_ok",
            "GET",
            "/ok",
            vec![response("resp_ok", 200, "tiny", vec![])],
        ),
    ]);
    let manager = MockServerManager::new();
    let bound = manager.start(config).unwrap();

    let received = Arc::new(std::sync::Mutex::new(Vec::new()));
    let rec_clone = Arc::clone(&received);
    let sub = manager
        .subscribe_logs(
            "mock_flow",
            Arc::new(move |event: MockLogEvent| {
                let MockLogEvent::Batch { entries } = event else {
                    return true;
                };
                rec_clone
                    .lock()
                    .unwrap()
                    .extend(entries.into_iter().map(|l| l.id));
                true
            }),
        )
        .unwrap();
    assert!(matches!(
        manager.subscribe_logs("unknown", Arc::new(crate::events::NoEvents)),
        Err(MockSubscribeError::NotRunning)
    ));

    let huge_request = "a".repeat(MOCK_LOG_BODY_LIMIT_BYTES + 1_000);
    assert_eq!(
        send_with_body(bound, "POST", "/huge", &huge_request).2,
        huge_response
    );
    assert!(manager.unsubscribe_logs(sub) && !manager.unsubscribe_logs(sub));

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_cl = Arc::clone(&calls);
    manager
        .subscribe_logs(
            "mock_flow",
            Arc::new(move |_: MockLogEvent| {
                calls_cl.fetch_add(1, Ordering::SeqCst);
                false
            }),
        )
        .unwrap();
    let padding = "p".repeat(8 * 1024);
    let raw = format!(
        "GET /ok HTTP/1.1\r\nHost: 127.0.0.1\r\nx-a: {padding}\r\nx-b: {padding}\r\nx-c: {padding}\r\nx-d: {padding}\r\n\r\n"
    );
    assert_eq!(send(bound, &raw).0, 200);
    get(bound, "/ok");
    assert_eq!(calls.load(Ordering::SeqCst), 1);

    wait_until("three rows", || manager.logs("mock_flow").len() == 3);
    let logs = manager.logs("mock_flow");
    assert_eq!(
        (
            logs[0].request_body.len(),
            logs[0].request_body_truncated,
            logs[0].response_body.len(),
            logs[0].response_body_truncated
        ),
        (
            MOCK_LOG_BODY_LIMIT_BYTES,
            true,
            MOCK_LOG_BODY_LIMIT_BYTES - 1,
            true
        )
    );
    let kept_hdr: usize = logs[1]
        .request_headers
        .iter()
        .map(|h| h.key.len() + h.value.len())
        .sum();
    assert!(
        kept_hdr <= HISTORY_HEADERS_LIMIT_BYTES
            && logs[1].request_headers.iter().any(|h| h.key == "host")
    );

    manager.set_log_capacity(2);
    get(bound, "/ok");
    wait_until("shrunk to two rows", || {
        manager.logs("mock_flow").len() == 2
    });

    let noop = || Arc::new(crate::events::NoEvents) as Arc<dyn EventSink<MockLogEvent>>;
    let subs: Vec<u64> = (0..MAX_LOG_SUBSCRIBERS)
        .map(|_| manager.subscribe_logs("mock_flow", noop()).unwrap())
        .collect();
    assert!(matches!(
        manager.subscribe_logs("mock_flow", noop()),
        Err(MockSubscribeError::TooMany)
    ));
    assert!(manager.unsubscribe_logs(subs[0]));
    assert!(manager.subscribe_logs("mock_flow", noop()).is_ok());
}

#[test]
fn timing_phases_and_serde_backward_compatibility_hold() {
    let upstream_manager = MockServerManager::new();
    let mut upstream = server(vec![route(
        "route_echo",
        "GET",
        "/echo",
        vec![response("resp_echo", 200, "up", vec![])],
    )]);
    upstream.id = "mock_upstream".into();
    let upstream_addr = upstream_manager.start(upstream).unwrap();

    let mut config = server(vec![route(
        "route_ok",
        "GET",
        "/ok",
        vec![response("resp_ok", 200, "ok", vec![])],
    )]);
    config.latency_ms = 50;
    config.proxy_enabled = true;
    config.proxy_base_url = format!("http://{upstream_addr}");
    let manager = MockServerManager::new();
    let bound = manager.start(config).unwrap();
    get(bound, "/ok");
    assert_eq!(get(bound, "/echo").2, "up");

    wait_until("two rows", || manager.logs("mock_flow").len() == 2);
    let logs = manager.logs("mock_flow");
    let direct = logs[0].timing.as_ref().unwrap();
    assert!(direct.latency_ms.is_some_and(|ms| ms >= 50) && direct.proxy_ms.is_none());
    let proxied = logs[1].timing.as_ref().unwrap();
    assert!(logs[1].proxied && proxied.proxy_ms.is_some() && proxied.latency_ms.is_none());

    let old = r#"{"id":"log_1","server_id":"m","timestamp_ms":1,"method":"GET","path":"/","query":"","request_body":"","matched_route_id":null,"matched_response_id":null,"status_code":200,"response_body":"","elapsed_ms":3}"#;
    let row: crate::workspace::items::MockTransactionLog = serde_json::from_str(old).unwrap();
    assert!(row.timing.is_none() && !serde_json::to_string(&row).unwrap().contains("timing"));
}
