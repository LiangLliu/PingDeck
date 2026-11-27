//! Byte-exact bodies, file and Base64 bodies, `Prefer: code=`, faults, extra allowed hosts,
//! the `[::1]` twin listener and recording of proxied exchanges.

use std::io::{Read, Write};
use std::net::{Ipv6Addr, SocketAddr, TcpStream};
use std::time::Duration;

use crate::workspace::items::{MockBodySource, MockFault, MockSelectionKind};

use super::super::server::{MockServerManager, MockStartError};
use super::support::*;

/// Sends `head` (ending in a blank line) followed by `body` and returns the raw response bytes
/// read until the server closes (a reset ends the read too).
fn exchange_bytes(addr: SocketAddr, head: &str, body: &[u8]) -> Vec<u8> {
    let mut stream = TcpStream::connect(addr).unwrap();
    stream.write_all(head.as_bytes()).unwrap();
    stream.write_all(body).unwrap();
    let mut response = Vec::new();
    let _ = stream.read_to_end(&mut response);
    response
}

/// The body of a raw response (after the first blank line).
fn body_of(raw: &[u8]) -> &[u8] {
    let at = raw
        .windows(4)
        .position(|window| window == b"\r\n\r\n")
        .unwrap();
    &raw[at + 4..]
}

#[test]
fn binary_bodies_cross_the_proxy_byte_for_byte_and_base64_bodies_are_served_decoded() {
    let binary: Vec<u8> = (0_u8..=255).chain([0xff, 0xfe, 0x00, 0xc3]).collect();
    let mut upstream_response = response("resp_bin", 200, "", vec![]);
    upstream_response.body = super::super::reply::encode_base64_body(&binary);
    upstream_response.body_source = Some(MockBodySource::Base64);
    let mut upstream = server(vec![
        route("route_bin", "GET", "/bin", vec![upstream_response]),
        route(
            "route_len",
            "POST",
            "/len",
            vec![response("resp_len", 200, "ok", vec![])],
        ),
    ]);
    upstream.id = "mock_upstream".into();
    let upstream_manager = MockServerManager::new();
    let upstream_addr = upstream_manager.start(upstream).unwrap();

    let mut downstream = server(Vec::new());
    downstream.proxy_enabled = true;
    downstream.proxy_base_url = format!("http://{upstream_addr}");
    let manager = MockServerManager::new();
    let bound = manager.start(downstream).unwrap();

    let relayed = exchange_bytes(
        bound,
        "GET /bin HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        b"",
    );
    assert_eq!(body_of(&relayed), binary.as_slice());

    // The upstream receives the request body unchanged: a lossy UTF-8 round trip would turn the
    // two invalid bytes into three-byte replacement characters and grow the length to 8.
    let sent = [0xff_u8, 0x00, 0xfe, b'a'];
    let raw = exchange_bytes(
        bound,
        &format!(
            "POST /len HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\nContent-Length: {}\r\n\r\n",
            sent.len()
        ),
        &sent,
    );
    assert!(raw.starts_with(b"HTTP/1.1 200"));
    let upstream_row = upstream_manager.logs("mock_upstream").pop().unwrap();
    let length = upstream_row
        .request_headers
        .iter()
        .find(|header| header.key.eq_ignore_ascii_case("content-length"))
        .map(|header| header.value.clone());
    assert_eq!(length.as_deref(), Some("4"));
}

#[test]
fn file_bodies_serve_absolute_regular_files_and_explain_what_they_cannot_serve() {
    let dir = std::env::temp_dir().join(format!("spanbay-mock-file-{}", std::process::id()));
    std::fs::create_dir_all(&dir).unwrap();
    let file = dir.join("payload.bin");
    std::fs::write(&file, [1_u8, 2, 3, 0, 255]).unwrap();
    // A named pipe with no writer: opening it would block the runtime thread.
    let fifo = dir.join("pipe");
    let _ = std::fs::remove_file(&fifo);
    let has_fifo = std::process::Command::new("mkfifo")
        .arg(&fifo)
        .status()
        .is_ok_and(|status| status.success());

    let body = |id: &str, path: &str| {
        let mut response = response(id, 200, path, vec![]);
        response.body_source = Some(MockBodySource::File);
        response
    };
    let config = server(vec![
        route(
            "route_file",
            "GET",
            "/file",
            vec![body("resp_file", file.to_str().unwrap())],
        ),
        route(
            "route_relative",
            "GET",
            "/relative",
            vec![body("resp_relative", "payload.bin")],
        ),
        route(
            "route_dir",
            "GET",
            "/dir",
            vec![body("resp_dir", dir.to_str().unwrap())],
        ),
        route(
            "route_fifo",
            "GET",
            "/fifo",
            vec![body("resp_fifo", fifo.to_str().unwrap())],
        ),
    ]);
    let manager = MockServerManager::new();
    let bound = manager.start(config).unwrap();

    let raw = exchange_bytes(
        bound,
        "GET /file HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        b"",
    );
    assert_eq!(body_of(&raw), &[1_u8, 2, 3, 0, 255]);
    let (status, _, message) = get(bound, "/relative");
    assert!(status == 500 && message.contains("absolute"), "{message}");
    let (status, _, message) = get(bound, "/dir");
    assert!(
        status == 500 && message.contains("regular file"),
        "{message}"
    );
    if has_fifo {
        let (status, _, message) = get(bound, "/fifo");
        assert!(
            status == 500 && message.contains("regular file"),
            "{message}"
        );
        let _ = std::fs::remove_file(&fifo);
    }
    let _ = std::fs::remove_file(&file);
    let _ = std::fs::remove_dir(&dir);
}

#[test]
fn prefer_code_forces_a_documented_response_and_faults_cut_the_connection() {
    let mut ok = response("resp_ok", 200, "ok", vec![]);
    ok.is_default = true;
    let mut missing = response("resp_missing", 404, "missing", vec![]);
    missing.position = 1;
    let mut dropped = response("resp_drop", 200, "never sent", vec![]);
    dropped.is_default = true;
    dropped.fault = Some(MockFault::Drop);
    let config = server(vec![
        route("route_user", "GET", "/user", vec![ok, missing]),
        route("route_drop", "GET", "/drop", vec![dropped]),
    ]);
    let manager = MockServerManager::new();
    let bound = manager.start(config).unwrap();

    assert_eq!(get(bound, "/user").2, "ok");
    let (status, _, body) = send(
        bound,
        "GET /user HTTP/1.1\r\nHost: 127.0.0.1\r\nPrefer: dynamic=true, code=404\r\n\r\n",
    );
    assert_eq!((status, body.as_str()), (404, "missing"));
    let row = manager.logs("mock_flow").pop().unwrap();
    assert_eq!(
        row.selected_by.map(|s| s.kind),
        Some(MockSelectionKind::Prefer)
    );
    // A status no response has falls back to the usual selection.
    let (status, _, _) = send(
        bound,
        "GET /user HTTP/1.1\r\nHost: 127.0.0.1\r\nPrefer: code=500\r\n\r\n",
    );
    assert_eq!(status, 200);

    // Drop: the connection closes without a single response byte.
    let raw = exchange_bytes(
        bound,
        "GET /drop HTTP/1.1\r\nHost: 127.0.0.1\r\nConnection: close\r\n\r\n",
        b"",
    );
    assert!(raw.is_empty(), "{}", String::from_utf8_lossy(&raw));
}

#[test]
fn a_hang_fault_holds_the_connection_without_answering() {
    let mut hanging = response("resp_hang", 200, "never sent", vec![]);
    hanging.is_default = true;
    hanging.fault = Some(MockFault::Hang);
    let manager = MockServerManager::new();
    let bound = manager
        .start(server(vec![route(
            "route_hang",
            "GET",
            "/hang",
            vec![hanging],
        )]))
        .unwrap();
    let mut stream = TcpStream::connect(bound).unwrap();
    stream
        .write_all(b"GET /hang HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n")
        .unwrap();
    stream
        .set_read_timeout(Some(Duration::from_millis(300)))
        .unwrap();
    let mut buffer = [0_u8; 16];
    let error = stream.read(&mut buffer).unwrap_err();
    assert!(matches!(
        error.kind(),
        std::io::ErrorKind::WouldBlock | std::io::ErrorKind::TimedOut
    ));
    // Stopping the server ends the held connection.
    manager.stop("mock_flow");
    stream
        .set_read_timeout(Some(Duration::from_secs(5)))
        .unwrap();
    assert!(matches!(stream.read(&mut buffer), Ok(0) | Err(_)));
}

#[test]
fn extra_allowed_hosts_open_the_host_guard_only_for_the_listed_names() {
    let mut ok = response("resp_ok", 200, "ok", vec![]);
    ok.is_default = true;
    let manager = MockServerManager::new();
    let bound = manager
        .start(server(vec![route("route_ok", "GET", "/ok", vec![ok])]))
        .unwrap();
    let port = bound.port();
    let with_host =
        |host: &str| send(bound, &format!("GET /ok HTTP/1.1\r\nHost: {host}\r\n\r\n")).0;
    assert_eq!(with_host(&format!("10.0.2.2:{port}")), 403);

    manager.set_extra_allowed_hosts(vec!["10.0.2.2".into(), "Host.Docker.Internal:8080".into()]);
    assert_eq!(with_host(&format!("10.0.2.2:{port}")), 200);
    assert_eq!(with_host("10.0.2.2"), 200);
    assert_eq!(with_host("host.docker.internal:8080"), 200);
    // A listed name with another port, or a name that only ends like one, stays refused.
    assert_eq!(with_host("10.0.2.2:1"), 403);
    assert_eq!(with_host(&format!("host.docker.internal:{port}")), 403);
    assert_eq!(with_host(&format!("evil10.0.2.2:{port}")), 403);

    manager.set_extra_allowed_hosts(Vec::new());
    assert_eq!(with_host(&format!("10.0.2.2:{port}")), 403);
}

#[test]
fn the_server_also_answers_on_ipv6_loopback_and_reports_a_foreign_ipv6_listener() {
    let Ok(probe) = std::net::TcpListener::bind((Ipv6Addr::LOCALHOST, 0)) else {
        // No IPv6 loopback on this machine: nothing to check.
        return;
    };
    let mut ok = response("resp_ok", 200, "ok", vec![]);
    ok.is_default = true;
    let manager = MockServerManager::new();
    let bound = manager
        .start(server(vec![route(
            "route_ok",
            "GET",
            "/ok",
            vec![ok.clone()],
        )]))
        .unwrap();
    let ipv6 = SocketAddr::from((Ipv6Addr::LOCALHOST, bound.port()));
    assert_eq!(get(ipv6, "/ok").2, "ok");

    // Another process holding [::1]:port would answer `localhost` clients instead of the mock.
    let taken = probe.local_addr().unwrap().port();
    let mut config = server(vec![route("route_ok", "GET", "/ok", vec![ok])]);
    config.id = "mock_conflict".into();
    config.port = taken;
    let error = MockServerManager::new().start(config).unwrap_err();
    assert!(
        matches!(error, MockStartError::Ipv6PortInUse { port } if port == taken),
        "{error}"
    );
}

#[test]
fn recording_keeps_whole_proxied_responses_once_per_method_and_path() {
    let big = "x".repeat(200 * 1024);
    let mut upstream = server(vec![
        route(
            "route_big",
            "GET",
            "/big",
            vec![response("resp_big", 200, &big, vec![])],
        ),
        route(
            "route_n",
            "GET",
            "/n",
            vec![response("resp_n", 200, "{{queryParam 'v'}}", vec![])],
        ),
    ]);
    upstream.id = "mock_upstream".into();
    let upstream_manager = MockServerManager::new();
    let upstream_addr = upstream_manager.start(upstream).unwrap();

    let mut downstream = server(Vec::new());
    downstream.proxy_enabled = true;
    downstream.proxy_base_url = format!("http://{upstream_addr}");
    let manager = MockServerManager::new();
    let bound = manager.start(downstream).unwrap();

    get(bound, "/n?v=before");
    assert_eq!(manager.recording("mock_flow"), Some((false, 0)));
    assert_eq!(manager.set_recording("mock_flow", true), Some(0));
    get(bound, "/big");
    get(bound, "/n?v=1");
    get(bound, "/n?v=2");

    let recordings = manager.recordings("mock_flow");
    assert_eq!(recordings.len(), 2);
    assert_eq!(recordings[0].body.len(), big.len());
    assert_eq!(
        (recordings[1].query.as_str(), &recordings[1].body[..]),
        ("v=2", &b"2"[..])
    );
    // The log row keeps a 64 KiB preview; the recording behind it has the whole body.
    let big_row = manager
        .logs("mock_flow")
        .into_iter()
        .find(|row| row.path == "/big")
        .unwrap();
    assert!(big_row.response_body_truncated);
    let recorded = manager.recorded_exchange("mock_flow", &big_row.id).unwrap();
    assert_eq!(recorded.body.len(), big.len());

    assert_eq!(manager.set_recording("mock_flow", false), Some(2));
    get(bound, "/other");
    assert_eq!(manager.recordings("mock_flow").len(), 2);
    manager.stop("mock_flow");
    assert_eq!(manager.set_recording("mock_flow", true), None);
}
