//! Shared fixtures and socket helpers for the mock tests.

use std::io::{Read, Write};
use std::net::{SocketAddr, TcpStream};
use std::time::{Duration, Instant};

use crate::workspace::items::{
    MockResponse, MockResponseMode, MockRoute, MockRouteType, MockRule, MockRuleOperator,
    MockRuleTarget, MockRulesOperator, MockServer,
};
use crate::workspace::models::WorkspaceId;

/// Builds a mock server config with the given routes (also used by the flow runner tests).
pub(crate) fn server(routes: Vec<MockRoute>) -> MockServer {
    MockServer {
        id: "mock_flow".into(),
        workspace_id: WorkspaceId::new("ws_mock").expect("workspace id should be valid"),
        name: "Mock".into(),
        port: 0,
        endpoint_prefix: String::new(),
        latency_ms: 0,
        cors_enabled: false,
        headers: Vec::new(),
        routes,
        data_buckets: Vec::new(),
        callbacks: Vec::new(),
        proxy_enabled: false,
        proxy_base_url: String::new(),
        proxy_remove_prefix: false,
        proxy_req_headers: Vec::new(),
        proxy_res_headers: Vec::new(),
        updated_at_ms: 0,
        environment_id: None,
    }
}

/// Builds an enabled HTTP route.
pub(crate) fn route(id: &str, method: &str, path: &str, responses: Vec<MockResponse>) -> MockRoute {
    MockRoute {
        id: id.into(),
        method: method.into(),
        path: path.into(),
        enabled: true,
        description: String::new(),
        route_type: MockRouteType::Http,
        data_bucket_id: String::new(),
        crud_id_key: "id".into(),
        response_mode: MockResponseMode::FirstMatch,
        responses,
        position: 0,
    }
}

/// Builds a response with the given status, body and rules.
pub(crate) fn response(
    id: &str,
    status_code: u16,
    body: &str,
    rules: Vec<MockRule>,
) -> MockResponse {
    MockResponse {
        id: id.into(),
        label: id.into(),
        status_code,
        headers: Vec::new(),
        body: body.into(),
        body_source: None,
        fault: None,
        latency_ms: 0,
        is_default: false,
        rules_operator: MockRulesOperator::And,
        rules,
        callback_ids: Vec::new(),
        position: 0,
        stream: None,
    }
}

/// Builds a rule on `target` with the given modifier, operator and value.
pub(super) fn rule(
    target: MockRuleTarget,
    modifier: &str,
    operator: MockRuleOperator,
    value: &str,
) -> MockRule {
    MockRule {
        id: format!("rule_{modifier}_{value}"),
        target,
        modifier: modifier.into(),
        operator,
        value: value.into(),
        invert: false,
        position: 0,
    }
}

/// Sends raw request bytes and returns the parsed `(status, headers, body)`.
///
/// `Connection: close` is injected after the request line so the server ends the response
/// with EOF (the server itself keeps connections alive by default).
pub(super) fn send(addr: SocketAddr, raw: &str) -> (u16, Vec<(String, String)>, String) {
    let mut stream = TcpStream::connect(addr).expect("mock server should accept connections");
    let raw = raw.replacen("\r\n", "\r\nConnection: close\r\n", 1);
    stream
        .write_all(raw.as_bytes())
        .expect("request should be written");
    stream.flush().expect("request should flush");

    let mut response = String::new();
    stream
        .read_to_string(&mut response)
        .expect("response should be readable");
    parse_response(&response)
}

/// Sends a bare `GET` for `path`.
pub(super) fn get(addr: SocketAddr, path: &str) -> (u16, Vec<(String, String)>, String) {
    send(
        addr,
        &format!("GET {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n\r\n"),
    )
}

/// Parses a raw HTTP response into `(status, lowercase headers, body)`.
pub(super) fn parse_response(raw: &str) -> (u16, Vec<(String, String)>, String) {
    let (head, body) = raw
        .split_once("\r\n\r\n")
        .expect("response should have a body");
    let mut lines = head.lines();
    let status = lines
        .next()
        .and_then(|line| line.split_whitespace().nth(1))
        .and_then(|status| status.parse::<u16>().ok())
        .expect("response should have a status line");
    let headers = lines
        .filter_map(|line| line.split_once(':'))
        .map(|(key, value)| (key.trim().to_ascii_lowercase(), value.trim().to_owned()))
        .collect();

    (status, headers, body.to_owned())
}

/// Value of the named lowercase header, or an empty string.
pub(super) fn header<'a>(headers: &'a [(String, String)], name: &str) -> &'a str {
    headers
        .iter()
        .find(|(key, _)| key == name)
        .map(|(_, value)| value.as_str())
        .unwrap_or_default()
}

/// Sends a request with a JSON body and returns the parsed response.
pub(super) fn send_with_body(
    addr: SocketAddr,
    method: &str,
    path: &str,
    body: &str,
) -> (u16, Vec<(String, String)>, String) {
    send(
        addr,
        &format!(
            "{method} {path} HTTP/1.1\r\nHost: 127.0.0.1\r\nContent-Length: {}\r\n\r\n{body}",
            body.len()
        ),
    )
}

/// Builds a `Crud` route over the given data bucket.
pub(super) fn crud_route(id: &str, path: &str, bucket_id: &str) -> MockRoute {
    let mut crud = route(id, "ANY", path, Vec::new());
    crud.route_type = MockRouteType::Crud;
    crud.data_bucket_id = bucket_id.into();
    crud
}

/// Whether a TCP connection to `addr` succeeds.
pub(super) fn reachable(addr: SocketAddr) -> bool {
    TcpStream::connect_timeout(&addr, Duration::from_millis(200)).is_ok()
}

/// A loopback port nothing listens on right now (bound and released again).
pub(super) fn free_port() -> u16 {
    std::net::TcpListener::bind(("127.0.0.1", 0))
        .expect("bind a probe port")
        .local_addr()
        .expect("probe address")
        .port()
}

/// Polls `condition` until it holds; fails with `what` after a 10 s deadline. For positive
/// conditions only: a negative claim needs a synchronisation point, not a wait.
pub(super) fn wait_until(what: &str, mut condition: impl FnMut() -> bool) {
    let deadline = Instant::now() + Duration::from_secs(10);
    while !condition() {
        assert!(Instant::now() < deadline, "timed out waiting for {what}");
        std::thread::sleep(Duration::from_millis(5));
    }
}

/// Connections the accept loop of server `id` has dispatched so far.
pub(super) fn accepted_connections(
    manager: &super::super::server::MockServerManager,
    id: &str,
) -> u64 {
    super::super::server::lock(&manager.running)
        .get(id)
        .map_or(0, |running| {
            running
                .state
                .accepted
                .load(std::sync::atomic::Ordering::SeqCst)
        })
}
