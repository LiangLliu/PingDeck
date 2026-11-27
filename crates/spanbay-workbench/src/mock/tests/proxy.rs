//! Upstream proxying tests.

use std::net::SocketAddr;

use crate::workspace::items::MockServer;
use crate::workspace::models::KeyValue;

use super::super::http::MAX_BODY_BYTES;
use super::super::server::MockServerManager;
use super::support::*;

#[test]
fn proxies_unmatched_requests_refuses_oversized_bodies_and_handles_headers_hops_and_prefixes() {
    let upstream_manager = MockServerManager::new();
    let big = format!("a{}", "é".repeat(MAX_BODY_BYTES / 2 + 10));
    let port = free_port();
    let mut redirect = route(
        "route_redirect",
        "GET",
        "/redir",
        vec![response("resp_redirect", 302, "", vec![])],
    );
    redirect.responses[0].headers = vec![KeyValue {
        key: "Location".into(),
        value: format!("http://127.0.0.1:{port}/again"),
        enabled: true,
        description: None,
    }];
    let mut upstream = server(vec![
        route(
            "route_echo",
            "GET",
            "/echo",
            vec![response(
                "resp_echo",
                203,
                "{{header 'xbadname'}}|{{header 'x-injected'}}",
                vec![],
            )],
        ),
        route(
            "route_big",
            "GET",
            "/big",
            vec![response("resp_big", 200, &big, vec![])],
        ),
        route(
            "route_hops",
            "GET",
            "/hops",
            vec![response(
                "resp_hops",
                200,
                "{{header 'x-spanbay-hops'}}",
                vec![],
            )],
        ),
        route(
            "route_other",
            "GET",
            "/other/x",
            vec![response("resp_other", 200, "other", vec![])],
        ),
        redirect,
    ]);
    upstream.id = "mock_upstream".into();
    let upstream_addr = upstream_manager.start(upstream).unwrap();

    let mut downstream = server(vec![route(
        "route_local",
        "GET",
        "/local",
        vec![response("resp_local", 200, "local", vec![])],
    )]);
    downstream.port = port;
    downstream.endpoint_prefix = "/api".into();
    downstream.proxy_enabled = true;
    downstream.proxy_remove_prefix = true;
    downstream.proxy_base_url = format!("http://{upstream_addr}");
    downstream.proxy_req_headers = vec![
        KeyValue {
            key: "x bad:name".into(),
            value: "a\r\nx-injected: 1".into(),
            enabled: true,
            description: None,
        },
        KeyValue {
            key: "Content-Length".into(),
            value: "999".into(),
            enabled: true,
            description: None,
        },
    ];
    downstream.proxy_res_headers = vec![KeyValue {
        key: "x-proxied".into(),
        value: "yes".into(),
        enabled: true,
        description: None,
    }];

    let manager = MockServerManager::new();
    let bound = manager.start(downstream).unwrap();

    assert_eq!(get(bound, "/api/local").2, "local");
    let (status, headers, body) = get(bound, "/api/echo");
    assert_eq!(
        (status, body.as_str(), header(&headers, "x-proxied")),
        (203, "ax-injected: 1|", "yes")
    );
    let logs = manager.logs("mock_flow");
    let proxied = logs.last().unwrap();
    assert!(proxied.proxied && proxied.matched_route_id.as_deref() == Some("proxy"));

    assert_eq!(get(bound, "/other/x").2, "other");
    assert_eq!(get(bound, "/api/hops").2, "1");
    assert_eq!(
        send(
            bound,
            "GET /api/hops HTTP/1.1\r\nHost: 127.0.0.1\r\nx-spanbay-hops: 3\r\n\r\n"
        )
        .0,
        502
    );

    let (status, headers, _) = get(bound, "/api/redir");
    assert_eq!(
        (status, header(&headers, "location")),
        (302, format!("http://127.0.0.1:{port}/again").as_str())
    );

    // An upstream body over the cap is refused, never relayed cut short.
    let (status, _, relayed) = get(bound, "/api/big");
    assert_eq!(status, 502);
    assert!(
        relayed.contains("exceeds") && relayed.contains("not relayed"),
        "{relayed}"
    );

    upstream_manager.stop("mock_upstream");
    assert_eq!(get(bound, "/api/echo").0, 502);
}

#[test]
fn a_server_proxying_to_itself_answers_502_without_recursing() {
    for host in ["127.0.0.1", "localhost", "[::1]"] {
        let manager = MockServerManager::new();
        let port = free_port();
        let mut config: MockServer = server(Vec::new());
        config.port = port;
        config.proxy_enabled = true;
        config.proxy_base_url = format!("http://{host}:{port}");
        let bound: SocketAddr = manager.start(config).unwrap();

        let (status, _, body) = get(bound, "/nowhere");
        assert_eq!(status, 502, "{host}");
        assert!(body.contains("proxy loop") && manager.logs("mock_flow").len() == 1);
    }
}

#[test]
fn an_unreachable_proxy_target_is_logged_as_proxy_failed() {
    use crate::workspace::items::MockLogOutcome;

    let mut config = server(Vec::new());
    config.proxy_enabled = true;
    config.proxy_base_url = format!("http://127.0.0.1:{}", free_port());
    let manager = MockServerManager::new();
    let bound = manager.start(config).unwrap();
    assert_eq!(get(bound, "/missing").0, 502);

    let row = &manager.logs("mock_flow")[0];
    assert_eq!(row.outcome, Some(MockLogOutcome::ProxyFailed));
    assert_eq!(row.matched_route_id.as_deref(), Some("proxy"));
    assert!(!row.proxied);
}
