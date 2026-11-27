//! Outbound-safety tests: loopback-only listening, no redirect following, refused metadata targets.

use std::net::Ipv4Addr;

use url::Url;

use crate::workspace::models::KeyValue;

use super::super::proxy::ensure_target_allowed;
use super::super::server::MockServerManager;
use super::support::*;

/// An upstream whose `/go` answers `302` to `location`.
fn redirecting_upstream(manager: &MockServerManager, location: &str) -> std::net::SocketAddr {
    let mut redirect = response("resp_redirect", 302, "", vec![]);
    redirect.headers = vec![KeyValue {
        key: "Location".into(),
        value: location.into(),
        enabled: true,
        description: None,
    }];
    let mut upstream = server(vec![route("route_go", "GET", "/go", vec![redirect])]);
    upstream.id = "mock_upstream".into();
    manager.start(upstream).expect("upstream should start")
}

fn proxying_to(base_url: String) -> crate::workspace::items::MockServer {
    let mut downstream = server(Vec::new());
    downstream.proxy_enabled = true;
    downstream.proxy_base_url = base_url;
    downstream
}

#[test]
fn loopback_listener_host_cors_and_proxy_redirect_relay_hold() {
    let trap_manager = MockServerManager::new();
    let mut trap = server(vec![route(
        "route_hit",
        "GET",
        "/hit",
        vec![response("resp_hit", 200, "followed", vec![])],
    )]);
    trap.id = "mock_trap".into();
    let trap_addr = trap_manager.start(trap).unwrap();

    let upstream_manager = MockServerManager::new();
    let upstream_addr = redirecting_upstream(&upstream_manager, &format!("http://{trap_addr}/hit"));

    let mut config = proxying_to(format!("http://{upstream_addr}"));
    config.cors_enabled = true;
    config.routes = vec![route(
        "route_ok",
        "GET",
        "/ok",
        vec![response("resp_ok", 200, "ok", vec![])],
    )];
    let manager = MockServerManager::new();
    let bound = manager.start(config.clone()).unwrap();
    assert_eq!(
        (bound.ip(), manager.bound_addr("mock_flow")),
        (Ipv4Addr::LOCALHOST.into(), Some(bound))
    );

    let (status, headers, _) = get(bound, "/go");
    assert_eq!(
        (
            status,
            header(&headers, "location"),
            trap_manager.logs("mock_trap").is_empty()
        ),
        (302, format!("http://{trap_addr}/hit").as_str(), true)
    );

    let with_host =
        |host: &str| send(bound, &format!("GET /ok HTTP/1.1\r\nHost: {host}\r\n\r\n")).0;
    for bad in ["evil.example", "evil.example:8080", "10.0.0.5"] {
        assert_eq!(with_host(bad), 403, "{bad}");
    }
    for good in [
        "127.0.0.1".into(),
        format!("127.0.0.1:{}", bound.port()),
        format!("localhost:{}", bound.port()),
        format!("app.localhost:{}", bound.port()),
        format!("[::1]:{}", bound.port()),
    ] {
        assert_eq!(with_host(&good), 200, "{good}");
    }

    let with_origin = |method: &str, origin: &str| {
        send(
            bound,
            &format!(
                "{method} /ok HTTP/1.1\r\nHost: 127.0.0.1\r\nOrigin: {origin}\r\nAccess-Control-Request-Method: GET\r\nAccess-Control-Request-Headers: x-token\r\n\r\n"
            ),
        )
    };
    assert_eq!(
        header(
            &with_origin("GET", "http://localhost:5173").1,
            "access-control-allow-origin"
        ),
        "http://localhost:5173"
    );
    for origin in [
        "http://evil.example",
        "null",
        "http://localhost.evil.example",
    ] {
        for method in ["GET", "OPTIONS"] {
            assert!(
                with_origin(method, origin)
                    .1
                    .iter()
                    .all(|(k, _)| !k.to_ascii_lowercase().starts_with("access-control-"))
            );
        }
    }

    config.proxy_base_url = "http://169.254.169.254".into();
    assert!(manager.update(config));
    let (status, _, body) = get(bound, "/latest/meta-data/");
    assert!(status == 502 && body.contains("refusing to connect"));
}

#[test]
fn target_guard_and_outbound_client_refuse_link_local_and_metadata() {
    use super::super::proxy::outbound_client;

    let runtime = tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .unwrap();
    let check = |url: &str| runtime.block_on(ensure_target_allowed(&Url::parse(url).unwrap()));
    for refused in [
        "http://169.254.169.254/latest",
        "http://[fe80::1]:8080/",
        "http://0.0.0.0:80/",
    ] {
        assert!(check(refused).is_err(), "{refused}");
    }
    for allowed in ["http://127.0.0.1:9/", "http://192.168.1.10/"] {
        assert!(check(allowed).is_ok(), "{allowed}");
    }

    let client = outbound_client().unwrap();
    let refused = runtime
        .block_on(async {
            client
                .get("http://metadata.google.internal/computeMetadata/v1/")
                .send()
                .await
        })
        .unwrap_err();
    assert!(format!("{refused:?}").contains("refusing to connect"));

    let manager = MockServerManager::new();
    let bound = manager
        .start(server(vec![route(
            "route_ok",
            "GET",
            "/ok",
            vec![response("resp_ok", 200, "reached", vec![])],
        )]))
        .unwrap();
    let body = runtime
        .block_on(async {
            client
                .get(format!("http://localhost:{}/ok", bound.port()))
                .send()
                .await?
                .text()
                .await
        })
        .unwrap();
    assert_eq!(body, "reached");
}

#[test]
fn cross_site_writes_are_refused_before_buckets_callbacks_and_proxy() {
    use crate::workspace::items::{MockCallback, MockDataBucket};
    use crate::workspace::models::HttpMethod;

    let sink_manager = MockServerManager::new();
    let mut sink = server(vec![route(
        "route_sink",
        "ANY",
        "/*",
        vec![response("resp_sink", 200, "sunk", vec![])],
    )]);
    sink.id = "mock_sink".into();
    let sink_addr = sink_manager.start(sink).unwrap();

    let mut hook = route(
        "route_hook",
        "POST",
        "/hook",
        vec![response("resp_hook", 200, "hooked", vec![])],
    );
    hook.responses[0].callback_ids = vec!["callback_sink".into()];
    let mut config = proxying_to(format!("http://{sink_addr}"));
    config.routes = vec![crud_route("route_users", "/users", "bucket_users"), hook];
    config.data_buckets = vec![MockDataBucket {
        id: "bucket_users".into(),
        name: "Users".into(),
        documentation: String::new(),
        value_json: r#"[{"id":1,"name":"Ada"}]"#.into(),
    }];
    config.callbacks = vec![MockCallback {
        id: "callback_sink".into(),
        name: "sink".into(),
        documentation: String::new(),
        method: HttpMethod::Post,
        url: format!("http://{sink_addr}/callback"),
        delay_ms: 0,
        headers: Vec::new(),
        body: "{{headers.x-mark}}".into(),
    }];
    let manager = MockServerManager::new();
    let bound = manager.start(config).unwrap();

    let body = r#"{"name":"Mallory"}"#;
    let post = |path: &str, extra: &str, mark: &str| {
        send(
            bound,
            &format!(
                "POST {path} HTTP/1.1\r\nHost: 127.0.0.1\r\n{extra}x-mark: {mark}\r\nContent-Type: text/plain\r\nContent-Length: {}\r\n\r\n{body}",
                body.len()
            ),
        )
        .0
    };
    for extra in [
        "Origin: http://evil.example\r\n",
        "Origin: null\r\n",
        "Sec-Fetch-Site: cross-site\r\n",
    ] {
        for path in ["/users", "/hook", "/elsewhere"] {
            assert_eq!(post(path, extra, "evil"), 403, "{extra} {path}");
        }
    }
    // Refused writes are traffic rows, never hits.
    assert_eq!(manager.stats("mock_flow").unwrap().hit_count, 0);
    assert_eq!(get(bound, "/users").2, r#"[{"id":1,"name":"Ada"}]"#);
    assert!(sink_manager.logs("mock_sink").is_empty());

    // A loopback page still writes, fires the callback and reaches the proxy.
    let local = "Origin: http://localhost:5173\r\nSec-Fetch-Site: cross-site\r\n";
    assert_eq!(post("/users", local, "good"), 201);
    assert_eq!(post("/hook", local, "good"), 200);
    wait_until("the loopback callback to arrive", || {
        sink_manager
            .logs("mock_sink")
            .iter()
            .any(|log| log.request_body == "good")
    });
    assert!(
        sink_manager
            .logs("mock_sink")
            .iter()
            .all(|log| log.request_body != "evil")
    );
    assert_eq!(post("/elsewhere", local, "good"), 200);
}
