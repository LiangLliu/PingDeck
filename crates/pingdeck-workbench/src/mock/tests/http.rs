//! HTTP framing, header sanitizing and connection timeout tests.

use std::io::{Read, Write};
use std::net::{Shutdown, TcpStream};
use std::time::{Duration, Instant};

use crate::workspace::models::KeyValue;

use super::super::http::MAX_BODY_BYTES;
use super::super::server::MockServerManager;
use super::support::*;

#[test]
fn framing_chunked_keep_alive_idle_timeout_and_header_sanitization_work() {
    let mut injected = response("resp_inject", 200, "ok", vec![]);
    injected.headers = vec![
        KeyValue {
            key: "x-evil\r\nX-Injected".into(),
            value: "1".into(),
            enabled: true,
            description: None,
        },
        KeyValue {
            key: "x-value".into(),
            value: "a\r\nX-Injected-Value: 1".into(),
            enabled: true,
            description: None,
        },
    ];
    let mut framing = response("resp_framing", 200, "ok", vec![]);
    framing.headers = vec![
        KeyValue {
            key: "Content-Length".into(),
            value: "99".into(),
            enabled: true,
            description: None,
        },
        KeyValue {
            key: "Connection".into(),
            value: "keep-alive".into(),
            enabled: true,
            description: None,
        },
        KeyValue {
            key: "Transfer-Encoding".into(),
            value: "chunked".into(),
            enabled: true,
            description: None,
        },
    ];

    let manager = MockServerManager::new();
    let bound = manager
        .start(server(vec![
            route(
                "route_any",
                "ANY",
                "/any",
                vec![response("resp_any", 200, "ok", vec![])],
            ),
            route(
                "route_echo",
                "POST",
                "/echo",
                vec![response("resp_echo", 200, "got:{{bodyRaw}}", vec![])],
            ),
            route("route_inject", "GET", "/inject", vec![injected]),
            route("route_framing", "GET", "/framing", vec![framing]),
        ]))
        .unwrap();

    for method in ["POST", "PUT", "PATCH", "DELETE", "GET"] {
        let (status, _, body) = send(
            bound,
            &format!("{method} /any HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"),
        );
        assert_eq!((status, body.as_str()), (200, "ok"));
    }

    let (status, _, body) = send(
        bound,
        "POST /echo HTTP/1.1\r\nHost: 127.0.0.1\r\nTransfer-Encoding: chunked\r\n\r\n5\r\nhello\r\n6\r\n world\r\n0\r\n\r\n",
    );
    assert_eq!((status, body.as_str()), (200, "got:hello world"));

    let oversized = "a".repeat(MAX_BODY_BYTES + 1);
    let mut raw = format!(
        "POST /echo HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nTransfer-Encoding: chunked\r\n\r\n{:x}\r\n",
        oversized.len()
    )
    .into_bytes();
    raw.extend_from_slice(oversized.as_bytes());
    let mut stream = TcpStream::connect(bound).unwrap();
    stream.write_all(&raw).unwrap();
    let mut resp = String::new();
    stream.read_to_string(&mut resp).unwrap();
    assert_eq!(parse_response(&resp).0, 413);

    let mut ka = TcpStream::connect(bound).unwrap();
    ka.set_read_timeout(Some(Duration::from_secs(5))).unwrap();
    for _ in 0..3 {
        ka.write_all(b"POST /any HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2\r\n\r\nhi")
            .unwrap();
        let mut received = Vec::new();
        let mut byte = [0_u8; 1];
        while !received.ends_with(b"\r\n\r\nok") {
            ka.read_exact(&mut byte).unwrap();
            received.push(byte[0]);
        }
        let text = String::from_utf8_lossy(&received);
        assert!(
            text.starts_with("HTTP/1.1 200")
                && !text.to_ascii_lowercase().contains("connection: close")
        );
    }

    let mut idle = TcpStream::connect(bound).unwrap();
    idle.write_all(b"GET /any HTTP/1.1\r\nHost: 127.0.0.1\r\n")
        .unwrap();
    idle.set_read_timeout(Some(Duration::from_secs(10)))
        .unwrap();
    let started = Instant::now();
    let mut idle_resp = String::new();
    idle.read_to_string(&mut idle_resp).unwrap();
    assert!(started.elapsed() < Duration::from_secs(10));
    assert!(idle_resp.is_empty() || idle_resp.starts_with("HTTP/1.1 408"));

    let (_, headers, _) = get(bound, "/inject");
    assert!(
        !headers
            .iter()
            .any(|(k, _)| k == "x-injected" || k == "x-injected-value")
    );
    assert_eq!(
        (
            header(&headers, "x-evilx-injected"),
            header(&headers, "x-value")
        ),
        ("1", "aX-Injected-Value: 1")
    );

    let (_, headers, body) = get(bound, "/framing");
    assert_eq!(
        (
            body.as_str(),
            header(&headers, "content-length"),
            header(&headers, "connection"),
            header(&headers, "transfer-encoding")
        ),
        ("ok", "2", "close", "")
    );
    assert_eq!(
        headers
            .iter()
            .filter(|(k, _)| k == "content-length")
            .count(),
        1
    );

    for bad_len in [
        "Content-Length: abc\r\n",
        "Content-Length: -1\r\n",
        "Content-Length: 99999999999999999999999\r\n",
        "Content-Length: 1\r\nContent-Length: 2\r\n",
    ] {
        assert_eq!(
            send(
                bound,
                &format!("POST /any HTTP/1.1\r\nHost: 127.0.0.1\r\n{bad_len}\r\n")
            )
            .0,
            400
        );
    }
    assert_eq!(
        send(
            bound,
            "POST /any HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 2000000\r\n\r\n"
        )
        .0,
        413
    );

    let logs_before = manager.logs("mock_flow").len();
    let mut cut = TcpStream::connect(bound).unwrap();
    cut.write_all(b"POST /any HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: 10\r\n\r\nabc")
        .unwrap();
    cut.shutdown(Shutdown::Write).unwrap();
    let mut cut_resp = String::new();
    cut.read_to_string(&mut cut_resp).unwrap();
    assert!(!cut_resp.contains("200 OK") && manager.logs("mock_flow").len() == logs_before);
}
