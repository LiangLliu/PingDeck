//! Server-sent event stream responses.

use std::time::{Duration, Instant};

use crate::workspace::items::{MockStream, MockStreamEnd, MockStreamEvent};

use super::super::reply::{MAX_STREAM_EVENTS, plan_stream};
use super::super::server::MockServerManager;
use super::support::*;

fn event(name: &str, data: &str) -> MockStreamEvent {
    MockStreamEvent {
        event: name.into(),
        data: data.into(),
    }
}

#[test]
fn stream_responses_pace_events_end_as_configured_and_stop_at_the_limits() {
    let stream = MockStream {
        events: vec![
            event("tick", "{{method}} 1"),
            event("", "two\nlines"),
            event("done", "3"),
        ],
        interval_ms: 40,
        end: MockStreamEnd::Close,
    };
    let mut closing = response("resp_close", 200, "unused", vec![]);
    closing.stream = Some(stream.clone());
    let mut dropping = response("resp_drop", 200, "", vec![]);
    dropping.stream = Some(MockStream {
        end: MockStreamEnd::Drop,
        ..stream
    });
    let manager = MockServerManager::new();
    let addr = manager
        .start(server(vec![
            route("route_close", "GET", "/events", vec![closing]),
            route("route_drop", "GET", "/cut", vec![dropping]),
        ]))
        .expect("mock server should start");

    let started = Instant::now();
    let (status, headers, body) = get(addr, "/events");
    assert!(started.elapsed() >= Duration::from_millis(80));
    assert_eq!(status, 200);
    assert_eq!(header(&headers, "content-type"), "text/event-stream");
    assert_eq!(header(&headers, "transfer-encoding"), "chunked");
    assert!(body.contains("event: tick\ndata: GET 1\n\n"), "{body}");
    assert!(body.contains("data: two\ndata: lines\n\n"));
    assert!(!body.contains("unused"));
    assert!(
        body.ends_with("0\r\n\r\n"),
        "a closed stream ends the response"
    );

    let (_, _, cut) = get(addr, "/cut");
    assert!(cut.contains("event: done\ndata: 3\n\n"));
    assert!(
        !cut.ends_with("0\r\n\r\n"),
        "a dropped stream never ends the response"
    );

    let many = MockStream {
        events: vec![event("", "x"); 150],
        interval_ms: 0,
        end: MockStreamEnd::Close,
    };
    assert_eq!(
        plan_stream(&many, str::to_owned).frames.len(),
        MAX_STREAM_EVENTS
    );
    let slow = MockStream {
        interval_ms: 1_000,
        ..many
    };
    // Events that would start after 60 s are left out: 0 s, 1 s, ... 60 s.
    assert_eq!(plan_stream(&slow, str::to_owned).frames.len(), 61);
}
