//! Response callback tests.

use std::sync::atomic::{AtomicUsize, Ordering};

use crate::workspace::items::MockCallback;
use crate::workspace::models::{HttpMethod, KeyValue};

use super::super::callbacks::CallbackSlot;
use super::super::server::MockServerManager;
use super::support::*;

#[test]
fn callbacks_fire_with_sanitized_headers_and_bounded_slots() {
    static COUNTER: AtomicUsize = AtomicUsize::new(0);
    let first = CallbackSlot::acquire(&COUNTER, 2).expect("first slot");
    let second = CallbackSlot::acquire(&COUNTER, 2).expect("second slot");
    assert!(CallbackSlot::acquire(&COUNTER, 2).is_none());
    assert_eq!(COUNTER.load(Ordering::SeqCst), 2);
    drop(first);
    let third = CallbackSlot::acquire(&COUNTER, 2).expect("slot freed by drop");
    drop((second, third));
    assert_eq!(COUNTER.load(Ordering::SeqCst), 0);

    let collector_manager = MockServerManager::new();
    let mut collector = server(vec![route(
        "route_collect",
        "POST",
        "/collect",
        vec![response("resp_collect", 200, "collected", vec![])],
    )]);
    collector.id = "mock_collector".into();
    let collector_addr = collector_manager.start(collector).expect("collector");

    let mut config = server(vec![{
        let mut route = route(
            "route_trigger",
            "GET",
            "/trigger",
            vec![response("resp_trigger", 200, "ok", vec![])],
        );
        route.responses[0].callback_ids = vec!["callback_1".into()];
        route
    }]);
    config.callbacks = vec![MockCallback {
        id: "callback_1".into(),
        name: "collect".into(),
        documentation: String::new(),
        method: HttpMethod::Post,
        url: format!("http://{collector_addr}/collect"),
        delay_ms: 0,
        headers: vec![
            KeyValue {
                key: "x-ok\r\nEvil".into(),
                value: "v\r\nx-injected: 1".into(),
                enabled: true,
                description: None,
            },
            KeyValue {
                key: "Content-Length".into(),
                value: "999".into(),
                enabled: true,
                description: None,
            },
        ],
        body: r#"{"fired":true}"#.into(),
    }];

    let manager = MockServerManager::new();
    let bound = manager.start(config).expect("server should start");
    assert_eq!(get(bound, "/trigger").2, "ok");

    wait_until("the callback to reach the collector", || {
        !collector_manager.logs("mock_collector").is_empty()
    });
    let logs = collector_manager.logs("mock_collector");
    assert_eq!(logs.len(), 1);
    assert_eq!(logs[0].request_body, r#"{"fired":true}"#);
    let value_of = |name: &str| {
        logs[0]
            .request_headers
            .iter()
            .find(|h| h.key == name)
            .map(|h| h.value.as_str())
    };
    assert_eq!(value_of("x-okevil"), Some("vx-injected: 1"));
    assert_eq!(value_of("x-injected"), None);
    assert_eq!(value_of("content-length"), Some("14"));
}

#[test]
fn callback_results_land_on_the_log_row_and_the_live_stream() {
    use std::sync::{Arc, Mutex};

    use crate::events::{EventSink, MockLogEvent};

    let collector_manager = MockServerManager::new();
    let mut collector = server(vec![route(
        "route_collect",
        "POST",
        "/collect",
        vec![response("resp_collect", 204, "", vec![])],
    )]);
    collector.id = "mock_collector".into();
    let collector_addr = collector_manager.start(collector).expect("collector");

    let callback = |id: &str, url: String| MockCallback {
        id: id.into(),
        name: id.into(),
        documentation: String::new(),
        method: HttpMethod::Post,
        url,
        delay_ms: 0,
        headers: Vec::new(),
        body: String::new(),
    };
    let mut config = server(vec![{
        let mut route = route(
            "route_trigger",
            "GET",
            "/trigger",
            vec![response("resp_trigger", 200, "ok", vec![])],
        );
        route.responses[0].callback_ids = vec!["cb_ok".into(), "cb_bad".into()];
        route
    }]);
    config.callbacks = vec![
        callback("cb_ok", format!("http://{collector_addr}/collect")),
        callback("cb_bad", "not a url".into()),
    ];
    let manager = MockServerManager::new();
    let bound = manager.start(config).expect("server should start");
    let streamed = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let streamed = Arc::clone(&streamed);
        Arc::new(move |event: MockLogEvent| {
            if let MockLogEvent::Callback { log_id, result } = event {
                streamed.lock().unwrap().push((log_id, result.name));
            }
            true
        }) as Arc<dyn EventSink<MockLogEvent>>
    };
    manager.subscribe_logs("mock_flow", sink).unwrap();
    assert_eq!(get(bound, "/trigger").2, "ok");

    wait_until("both callback results", || {
        manager.logs("mock_flow")[0].callbacks.len() == 2
    });
    let row = &manager.logs("mock_flow")[0];
    let by_name = |name: &str| row.callbacks.iter().find(|c| c.name == name).unwrap();
    assert_eq!(by_name("cb_ok").status, Some(204));
    assert_eq!(by_name("cb_bad").error.as_deref(), Some("invalid URL"));
    wait_until("both results streamed", || {
        streamed.lock().unwrap().len() == 2
    });
    assert!(streamed.lock().unwrap().iter().all(|(id, _)| id == &row.id));
}
