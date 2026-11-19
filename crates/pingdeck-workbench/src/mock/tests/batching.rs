//! Batched log fan-out: window logic on synthetic time, plus the runtime timer end to end.

use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::mpsc;
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::events::{EventSink, MockLogEvent};
use crate::workspace::items::MockTransactionLog;

use super::super::fanout::{BATCH_WINDOW, LogHub, Subscriber};
use super::super::server::{LogSink, MockServerManager};
use super::support::*;

fn entry(number: usize) -> MockTransactionLog {
    MockTransactionLog {
        id: format!("log_{number}"),
        server_id: "mock_flow".into(),
        timestamp_ms: 0,
        method: "GET".into(),
        path: "/".into(),
        query: String::new(),
        request_headers: Vec::new(),
        request_body: String::new(),
        request_body_truncated: false,
        matched_route_id: None,
        matched_response_id: None,
        status_code: 200,
        response_headers: Vec::new(),
        response_body: String::new(),
        response_body_truncated: false,
        proxied: false,
        elapsed_ms: 0,
        ..MockTransactionLog::default()
    }
}

/// A sink recording the ids of every batch it receives.
fn recording_sink() -> (LogSink, Arc<Mutex<Vec<Vec<String>>>>) {
    let batches = Arc::new(Mutex::new(Vec::new()));
    let sink = {
        let batches = Arc::clone(&batches);
        Arc::new(move |event: MockLogEvent| {
            let MockLogEvent::Batch { entries } = event else {
                return true;
            };
            batches
                .lock()
                .expect("sink lock")
                .push(entries.into_iter().map(|log| log.id).collect());
            true
        }) as LogSink
    };
    (sink, batches)
}

fn ids(range: std::ops::RangeInclusive<usize>) -> Vec<String> {
    range.map(|number| format!("log_{number}")).collect()
}

fn ms(base: Instant, millis: u64) -> Instant {
    base + Duration::from_millis(millis)
}

#[test]
fn log_hub_batches_clears_prunes_closes_and_caps_bytes() {
    use super::super::fanout::MAX_RING_BYTES;

    let hub = LogHub::new(200);
    let (sink, batches) = recording_sink();
    assert!(hub.subscribe(1, sink));
    let base = Instant::now();

    assert!(hub.publish(entry(1), base).is_empty());
    assert_eq!(*batches.lock().unwrap(), [ids(1..=1)]);

    let timers = hub.publish(entry(2), ms(base, 1));
    assert_eq!((timers.len(), timers[0].1), (1, base + BATCH_WINDOW));
    assert!(hub.publish(entry(3), ms(base, 2)).is_empty());
    assert_eq!(hub.poll_subscriber(&timers[0].0, timers[0].1, true), None);
    assert_eq!(*batches.lock().unwrap(), [ids(1..=1), ids(2..=3)]);
    assert!(hub.publish(entry(4), ms(base, 500)).is_empty());
    assert_eq!(batches.lock().unwrap().len(), 3);

    let timers = hub.publish(entry(5), ms(base, 501));
    hub.clear();
    assert!(hub.snapshot().is_empty());
    assert_eq!(hub.poll_subscriber(&timers[0].0, timers[0].1, true), None);
    assert_eq!(batches.lock().unwrap().len(), 3);

    hub.publish(entry(6), ms(base, 1000));
    hub.publish(entry(7), ms(base, 1001));
    hub.close();
    assert_eq!(batches.lock().unwrap().last().unwrap(), &ids(7..=7));
    assert!(!hub.unsubscribe(1));

    let hub = LogHub::new(200);
    let (sink, batches) = recording_sink();
    assert!(hub.subscribe(1, sink));
    let mut timer: Option<(Arc<Subscriber>, Instant)> = None;
    for number in 1..=1000_usize {
        let now = ms(base, u64::try_from(number).unwrap());
        if let Some((sub, at)) = timer.take_if(|(_, at)| *at <= now) {
            timer = hub.poll_subscriber(&sub, at, true).map(|next| (sub, next));
        }
        for (sub, at) in hub.publish(entry(number), now) {
            timer = Some((sub, at));
        }
    }
    if let Some((sub, at)) = timer {
        hub.poll_subscriber(&sub, at, true);
    }
    let locked = batches.lock().unwrap();
    assert!(locked.len() <= 1000 / 33 + 2);
    assert_eq!(
        locked.iter().flatten().cloned().collect::<Vec<_>>(),
        ids(1..=1000)
    );
    drop(locked);
    assert!(hub.unsubscribe(1));

    let calls = Arc::new(AtomicUsize::new(0));
    let calls_cl = Arc::clone(&calls);
    assert!(hub.subscribe(
        7,
        Arc::new(move |_: MockLogEvent| {
            calls_cl.fetch_add(1, Ordering::SeqCst);
            false
        }) as LogSink
    ));
    hub.publish(entry(1), base);
    hub.publish(entry(2), ms(base, 100));
    assert_eq!(calls.load(Ordering::SeqCst), 1);
    assert!(!hub.unsubscribe(7));
    for id in 0..16 {
        assert!(hub.subscribe(id, Arc::new(crate::events::NoEvents) as LogSink));
    }
    assert!(!hub.subscribe(99, Arc::new(crate::events::NoEvents) as LogSink));

    let byte_hub = LogHub::new(5_000);
    let body_len = 1024 * 1024;
    let published = MAX_RING_BYTES / body_len * 2;
    for number in 0..published {
        let mut big = entry(number);
        big.request_body = "x".repeat(body_len);
        byte_hub.publish(big, base);
    }
    let kept = byte_hub.snapshot();
    assert!(kept.len() < published && kept.len() * body_len <= MAX_RING_BYTES);
    assert_eq!(
        kept.last().map(|l| l.id.clone()),
        Some(format!("log_{}", published - 1))
    );
    let mut huge = entry(published);
    huge.request_body = "x".repeat(MAX_RING_BYTES + 1);
    byte_hub.publish(huge, base);
    assert_eq!(byte_hub.snapshot().len(), 1);
    byte_hub.clear();
    byte_hub.publish(entry(published + 1), base);
    byte_hub.publish(entry(published + 2), base);
    assert_eq!(byte_hub.snapshot().len(), 2);
}

#[test]
fn runtime_timer_and_stop_flush_pending_batches() {
    let manager = MockServerManager::new();
    let bound = manager
        .start(server(vec![route(
            "route_ok",
            "GET",
            "/ok",
            vec![response("resp_ok", 200, "ok", vec![])],
        )]))
        .unwrap();
    let (sender, receiver) = mpsc::channel();
    let sink = Arc::new(move |event: MockLogEvent| {
        let MockLogEvent::Batch { entries } = event else {
            return true;
        };
        sender.send(entries.len()).is_ok()
    }) as Arc<dyn EventSink<MockLogEvent>>;
    let (stop_sink, batches) = recording_sink();
    manager.subscribe_logs("mock_flow", sink).unwrap();
    manager.subscribe_logs("mock_flow", stop_sink).unwrap();

    for _ in 0..5 {
        get(bound, "/ok");
    }
    let mut received = 0;
    while received < 5 {
        received += receiver.recv_timeout(Duration::from_secs(5)).unwrap();
    }
    assert_eq!(received, 5);

    get(bound, "/ok");
    manager.stop("mock_flow");
    // Live ids are `log_<start ms>_<sequence>`: compare the sequence.
    let delivered: Vec<String> = batches
        .lock()
        .unwrap()
        .iter()
        .flatten()
        .map(|id| format!("log_{}", id.rsplit('_').next().unwrap()))
        .collect();
    assert_eq!(delivered, ids(1..=6));
}
