//! Tests of one upstream attempt: `Retry-After`, the response facts and the connection timing.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use reqwest::header::{HeaderMap, HeaderName, HeaderValue};
use serde_json::json;
use tokio::io::{AsyncReadExt, AsyncWriteExt};
use tokio::net::TcpListener;

use super::*;
use crate::llm::capture::PhaseValue;
use crate::llm::probe::test_support::{runtime, target};
use crate::llm::runtime::LlmRuntime;

fn headers(pairs: &[(&str, &str)]) -> HeaderMap {
    let mut map = HeaderMap::new();
    for (name, value) in pairs {
        map.append(
            HeaderName::from_bytes(name.as_bytes()).unwrap(),
            HeaderValue::from_str(value).unwrap(),
        );
    }
    map
}

#[test]
fn retry_after_prefers_the_millisecond_header_and_reads_seconds_and_dates() {
    assert_eq!(
        retry_after_ms(&headers(&[("retry-after", "2")])),
        Some(2000)
    );
    assert_eq!(
        retry_after_ms(&headers(&[("retry-after-ms", "350"), ("retry-after", "9")])),
        Some(350)
    );
    assert_eq!(
        retry_after_ms(&headers(&[
            ("retry-after-ms", "junk"),
            ("retry-after", "3")
        ])),
        Some(3000)
    );
    assert_eq!(retry_after_ms(&headers(&[("retry-after", "later")])), None);
    assert_eq!(retry_after_ms(&headers(&[("retry-after", "-1")])), None);
    assert_eq!(retry_after_ms(&headers(&[])), None);
    // An HTTP date in the past waits no time.
    assert_eq!(
        retry_after_ms(&headers(&[(
            "retry-after",
            "Sun, 06 Nov 1994 08:49:37 GMT"
        )])),
        Some(0)
    );
    let future = httpdate::fmt_http_date(std::time::SystemTime::now() + Duration::from_secs(8));
    let wait = retry_after_ms(&headers(&[("retry-after", &future)])).unwrap();
    assert!((6000..=8000).contains(&wait), "{wait}");
}

#[test]
fn a_status_error_reports_its_retry_after() {
    let error = AttemptError::Status {
        status: 429,
        body: String::new(),
        retry_after_ms: Some(1500),
    };
    assert_eq!(error.retry_after(), Some(Duration::from_millis(1500)));
    let none = AttemptError::Status {
        status: 429,
        body: String::new(),
        retry_after_ms: None,
    };
    assert_eq!(none.retry_after(), None);
    assert_eq!(AttemptError::Timeout.retry_after(), None);
}

#[test]
fn an_empty_stream_is_recognised() {
    let empty = AttemptError::Stream {
        message: "the upstream stream ended with no content".into(),
        kind: Some(EMPTY_STREAM.into()),
    };
    assert!(empty.is_empty_stream() && empty.retryable());
    let other = AttemptError::Stream {
        message: "overloaded".into(),
        kind: Some("overloaded_error".into()),
    };
    assert!(!other.is_empty_stream());
}

#[test]
fn the_meta_keeps_the_request_id_and_only_the_headers_worth_capturing() {
    let mut meta = UpstreamMeta::unsent();
    meta.record_headers(&headers(&[
        ("content-type", "application/json"),
        ("request-id", "req_anthropic"),
        ("x-request-id", "req_first"),
        ("set-cookie", "session=secret"),
        ("authorization", "Bearer leak"),
        ("x-ratelimit-remaining-requests", "99"),
        ("anthropic-ratelimit-tokens-remaining", "5"),
        ("retry-after", "1"),
        ("x-unrelated", "nope"),
    ]));
    assert_eq!(meta.upstream_request_id.as_deref(), Some("req_first"));
    let names: Vec<&str> = meta
        .response_headers
        .iter()
        .map(|(n, _)| n.as_str())
        .collect();
    for kept in [
        "content-type",
        "request-id",
        "x-request-id",
        "x-ratelimit-remaining-requests",
        "anthropic-ratelimit-tokens-remaining",
        "retry-after",
    ] {
        assert!(names.contains(&kept), "{kept} in {names:?}");
    }
    for dropped in ["set-cookie", "authorization", "x-unrelated"] {
        assert!(!names.contains(&dropped), "{dropped} in {names:?}");
    }
}

#[test]
fn a_request_id_must_be_printable_and_is_cut() {
    let mut meta = UpstreamMeta::unsent();
    meta.record_headers(&headers(&[("x-request-id", &"a".repeat(300))]));
    assert_eq!(meta.upstream_request_id.unwrap().len(), MAX_REQUEST_ID);
    let mut meta = UpstreamMeta::unsent();
    meta.record_headers(&headers(&[("x-request-id", "has space")]));
    assert_eq!(meta.upstream_request_id, None);
}

/// A keep-alive HTTP/1.1 server answering every request after `delay` with a small JSON body, an
/// `x-request-id` and a `retry-after`.
async fn keep_alive_server(delay: Duration) -> SocketAddr {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let addr = listener.local_addr().unwrap();
    tokio::spawn(async move {
        while let Ok((mut socket, _)) = listener.accept().await {
            tokio::spawn(async move {
                let mut buffer: Vec<u8> = Vec::new();
                let mut chunk = [0_u8; 4096];
                loop {
                    let Some(at) = buffer.windows(4).position(|w| w == b"\r\n\r\n") else {
                        match socket.read(&mut chunk).await {
                            Ok(read) if read > 0 => buffer.extend_from_slice(&chunk[..read]),
                            _ => return,
                        }
                        continue;
                    };
                    let head = String::from_utf8_lossy(&buffer[..at]).to_ascii_lowercase();
                    let length = head
                        .lines()
                        .find_map(|line| line.strip_prefix("content-length:"))
                        .and_then(|value| value.trim().parse::<usize>().ok())
                        .unwrap_or(0);
                    while buffer.len() < at + 4 + length {
                        match socket.read(&mut chunk).await {
                            Ok(read) if read > 0 => buffer.extend_from_slice(&chunk[..read]),
                            _ => return,
                        }
                    }
                    buffer.drain(..at + 4 + length);
                    tokio::time::sleep(delay).await;
                    let body = "{}";
                    let reply = format!(
                        "HTTP/1.1 200 OK\r\ncontent-type: application/json\r\ncontent-length: {}\r\nx-request-id: req_abc\r\nretry-after: 1\r\n\r\n{body}",
                        body.len()
                    );
                    if socket.write_all(reply.as_bytes()).await.is_err() {
                        return;
                    }
                }
            });
        }
    });
    addr
}

async fn attempt(runtime: &LlmRuntime, host: &str, addr: SocketAddr) -> UpstreamMeta {
    let mut target = target(addr, &[Protocol::OpenaiChat]);
    target.base_url = format!("http://{host}:{}", addr.port());
    let body = json!({"model": "m"});
    let call = Call {
        runtime,
        target: &target,
        protocol: Protocol::OpenaiChat,
        stream: false,
        url: None,
        body: &body,
        timeout: Duration::from_secs(10),
        cancel: None,
    };
    let (result, meta) = send_measured(&call).await;
    result.unwrap();
    meta
}

#[tokio::test(flavor = "multi_thread")]
async fn s1_a_new_connection_shows_dns_and_a_pooled_one_shows_reused() {
    let addr = keep_alive_server(Duration::ZERO).await;
    let runtime = runtime();
    let first = attempt(&runtime, "localhost", addr).await;
    assert_eq!(first.conn.reused, Some(false));
    let PhaseValue::Ms(dns) = first.conn.dns else {
        panic!(
            "DNS of a fresh connection to a name is measured: {:?}",
            first.conn
        );
    };
    assert!(dns >= 1, "a measured phase is never 0");
    assert_eq!(first.upstream_request_id.as_deref(), Some("req_abc"));
    assert_eq!(first.status, Some(200));
    let headers_ms = first.headers_ms.unwrap();
    let timing = first.timing(PhaseValue::NotNeeded, headers_ms + 3);
    assert_eq!(timing.dns, PhaseValue::Ms(dns));
    let PhaseValue::Ms(connect) = first.conn.connect else {
        panic!(
            "the connect of a fresh connection is measured: {:?}",
            first.conn
        );
    };
    assert!(connect >= 1, "a measured phase is never 0");
    assert_eq!(
        timing.connect,
        PhaseValue::Ms(connect.min(headers_ms - dns.min(headers_ms)))
    );
    assert_eq!(
        timing.tls,
        PhaseValue::NotMeasured,
        "TLS is inside the connect segment"
    );
    assert!(timing.connect_includes_tls && !timing.reused);
    assert!(timing.setup_ms() + timing.headers_ms.ms().unwrap() <= timing.end_ms.ms().unwrap());
    assert!(timing.receive_ms().is_some());

    let second = attempt(&runtime, "localhost", addr).await;
    assert_eq!(
        second.conn.reused,
        Some(true),
        "the pooled connection is recognised"
    );
    let timing = second.timing(PhaseValue::NotNeeded, second.headers_ms.unwrap() + 1);
    assert_eq!(
        (timing.dns, timing.connect, timing.tls, timing.reused),
        (
            PhaseValue::Reused,
            PhaseValue::Reused,
            PhaseValue::Reused,
            true
        )
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn s1_an_ip_literal_needs_no_dns_but_reuse_is_still_known() {
    let addr = keep_alive_server(Duration::ZERO).await;
    let runtime = runtime();
    let first = attempt(&runtime, "127.0.0.1", addr).await;
    assert_eq!(first.conn.dns, PhaseValue::NotNeeded);
    assert!(
        matches!(first.conn.connect, PhaseValue::Ms(ms) if ms >= 1),
        "a literal host still has a connect: {:?}",
        first.conn
    );
    assert_eq!(first.conn.reused, Some(false));
    let timing = first.timing(PhaseValue::NotNeeded, first.headers_ms.unwrap());
    assert_eq!(timing.dns, PhaseValue::NotNeeded);
    assert_eq!(
        timing.headers_ms.ms().unwrap() + timing.connect.ms().unwrap(),
        first.headers_ms.unwrap(),
        "connect and the rest make up the time to headers"
    );
    let second = attempt(&runtime, "127.0.0.1", addr).await;
    assert_eq!(second.conn.reused, Some(true));
}

#[tokio::test(flavor = "multi_thread")]
async fn s1_overlapping_attempts_to_one_host_do_not_claim_each_others_lookups() {
    let addr = keep_alive_server(Duration::from_millis(300)).await;
    let runtime = Arc::new(runtime());
    let (a, b) = tokio::join!(
        attempt(&runtime, "localhost", addr),
        attempt(&runtime, "localhost", addr)
    );
    assert_eq!(a.conn.dns, PhaseValue::NotMeasured);
    assert_eq!(b.conn.dns, PhaseValue::NotMeasured);
    assert_eq!(a.conn.connect, PhaseValue::NotMeasured);
    assert_eq!(b.conn.connect, PhaseValue::NotMeasured);
    // Both are new connections (the pool had none), and say so.
    assert_eq!((a.conn.reused, b.conn.reused), (Some(false), Some(false)));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_failed_connection_still_reports_what_is_known() {
    let port = std::net::TcpListener::bind("127.0.0.1:0")
        .unwrap()
        .local_addr()
        .unwrap()
        .port();
    let addr: SocketAddr = format!("127.0.0.1:{port}").parse().unwrap();
    let runtime = runtime();
    let target = target(addr, &[Protocol::OpenaiChat]);
    let body = json!({});
    let call = Call {
        runtime: &runtime,
        target: &target,
        protocol: Protocol::OpenaiChat,
        stream: false,
        url: None,
        body: &body,
        timeout: Duration::from_secs(5),
        cancel: None,
    };
    let (result, meta) = send_measured(&call).await;
    assert!(matches!(result, Err(AttemptError::Network { .. })));
    assert_eq!(
        (meta.headers_ms, meta.status, meta.conn.reused),
        (None, None, None)
    );
    let timing = meta.timing(PhaseValue::NotNeeded, 4);
    assert_eq!(timing.headers_ms, PhaseValue::NotMeasured);
    assert_eq!(timing.end_ms, PhaseValue::Ms(4));
    assert!(!timing.reused);
}
