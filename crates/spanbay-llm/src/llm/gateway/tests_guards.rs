//! Tests of the Host and Origin guards over real HTTP: the loopback and LAN host matrices
//! (DNS rebinding, an unassigned LAN address), extra allowed hosts in both modes, and the
//! refusal of every `Origin` (preflights included) without any CORS header.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use serde_json::json;

use super::tests::{Harness, TOKEN, mac_lan};

fn refusal_body(message: &str, code: &str) -> serde_json::Value {
    json!({"error": {"message": message, "type": "permission_error", "param": null, "code": code}})
}

/// The 403 text, which names the rejected Host.
fn host_message(host: &str) -> String {
    format!("Host {host} is not allowed. Add it under Extra allowed hosts in Settings.")
}

#[tokio::test(flavor = "multi_thread")]
async fn loopback_mode_accepts_only_loopback_names_with_no_port_or_the_bound_port() {
    let h = Harness::start(|_| {});
    let port = h.port();
    let accepted = [
        "127.0.0.1".to_owned(),
        format!("127.0.0.1:{port}"),
        "localhost".to_owned(),
        format!("LocalHost:{port}"),
        "[::1]".to_owned(),
        format!("[::1]:{port}"),
    ];
    for host in &accepted {
        let r = h.send("GET", "/health", &[("host", host)], None).await;
        assert_eq!(r.status, 200, "{host}: {}", r.text());
    }
    let refused = [
        "evil.example".to_owned(),
        format!("evil.example:{port}"),
        // A DNS-rebinding page: its own name, the gateway's port.
        format!("rebind.test:{port}"),
        format!("127.0.0.1:{}", port.wrapping_add(1)),
        format!("192.168.1.20:{port}"),
        format!("mac:{port}"),
        format!("0.0.0.0:{port}"),
        "localhost.evil.example".to_owned(),
    ];
    for host in &refused {
        let r = h.send("GET", "/health", &[("host", host)], None).await;
        assert_eq!(r.status, 403, "{host}");
        assert_eq!(
            r.json(),
            refusal_body(&host_message(host), "host_not_allowed"),
            "{host}"
        );
    }
    // Every refusal is a row: 403, the reason, no client, no peer (loopback), the response's id.
    let rows = h.logs(refused.len()).await;
    for (row, host) in rows.iter().zip(&refused) {
        assert_eq!(
            (row.status, row.error.as_deref(), row.outcome.as_str()),
            (
                403,
                Some(format!("Host {host} is not allowed").as_str()),
                "failure"
            ),
            "the row names the rejected Host"
        );
        assert_eq!(
            (row.client_id.as_deref(), row.peer.as_deref()),
            (None, None)
        );
        assert!(
            row.request_id
                .as_deref()
                .is_some_and(|id| id.starts_with("rq_"))
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn lan_mode_adds_exactly_the_assigned_address_the_machine_names_and_the_extra_hosts() {
    let h = Harness::build(|gateway| {
        gateway.serve_lan = true;
        gateway.extra_allowed_hosts = vec!["Relay.Example".to_owned()];
    });
    h.llm.gateway.pin_lan(mac_lan());
    h.llm.gateway.start(&h.llm).unwrap();
    let port = h.port();
    let bearer = format!("Bearer {TOKEN}");
    let accepted = [
        "127.0.0.1".to_owned(),
        format!("localhost:{port}"),
        format!("192.168.1.20:{port}"),
        "192.168.1.20".to_owned(),
        format!("MAC:{port}"),
        "mac.local".to_owned(),
        format!("Mac.Local:{port}"),
        format!("relay.example:{port}"),
        "RELAY.EXAMPLE".to_owned(),
    ];
    for host in &accepted {
        let r = h
            .send(
                "GET",
                "/v1/models",
                &[("host", host), ("authorization", &bearer)],
                None,
            )
            .await;
        assert_eq!(r.status, 200, "{host}: {}", r.text());
    }
    let refused = [
        // Not assigned to any interface of this machine.
        format!("192.168.1.21:{port}"),
        "10.0.0.5".to_owned(),
        "evil.example".to_owned(),
        format!("rebind.test:{port}"),
        format!("mac.local:{}", port.wrapping_add(1)),
        format!("relay.example:{}", port.wrapping_add(1)),
        "mac.example".to_owned(),
    ];
    for host in &refused {
        let r = h
            .send(
                "GET",
                "/v1/models",
                &[("host", host), ("authorization", &bearer)],
                None,
            )
            .await;
        assert_eq!(r.status, 403, "{host}");
        assert_eq!(
            r.json(),
            refusal_body(&host_message(host), "host_not_allowed"),
            "{host}"
        );
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn extra_allowed_hosts_work_with_lan_off_and_name_the_host_until_added() {
    let h = Harness::start(|_| {});
    let port = h.port();
    let host = format!("abc.ngrok.app:{port}");
    let bearer = format!("Bearer {TOKEN}");
    // Before adding it: 403 naming the host, token or not.
    let r = h
        .send(
            "GET",
            "/v1/models",
            &[("host", &host), ("authorization", &bearer)],
            None,
        )
        .await;
    assert_eq!(r.status, 403);
    assert!(
        r.json()["error"]["message"]
            .as_str()
            .unwrap()
            .contains("abc.ngrok.app")
    );
    // After adding it (case-insensitive, with LAN still off): 401 without a token, 200 with.
    h.set_gateway(|gateway| gateway.extra_allowed_hosts = vec!["ABC.ngrok.app".to_owned()]);
    let r = h.send("GET", "/v1/models", &[("host", &host)], None).await;
    assert_eq!(r.status, 401);
    let r = h
        .send(
            "GET",
            "/v1/models",
            &[("host", &host), ("authorization", &bearer)],
            None,
        )
        .await;
    assert_eq!(r.status, 200);
    // An entry with a port needs exactly that port.
    h.set_gateway(|gateway| {
        gateway.extra_allowed_hosts = vec!["host.docker.internal:9999".to_owned()]
    });
    for (value, status) in [
        ("host.docker.internal:9999".to_owned(), 200),
        (format!("host.docker.internal:{port}"), 403),
        ("host.docker.internal".to_owned(), 403),
    ] {
        let r = h.send("GET", "/health", &[("host", &value)], None).await;
        assert_eq!(r.status, status, "{value}");
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn any_origin_is_403_without_cors_headers_preflights_included() {
    let h = Harness::start(|_| {});
    let bearer = format!("Bearer {TOKEN}");
    for origin in [
        "http://app.example",
        "https://other.example",
        "https://app.example.evil.test",
        "null",
    ] {
        for path in ["/v1/models", "/health"] {
            let r = h
                .send(
                    "GET",
                    path,
                    &[("origin", origin), ("authorization", &bearer)],
                    None,
                )
                .await;
            assert_eq!(r.status, 403, "{origin} {path}");
            assert_eq!(
                r.json(),
                refusal_body(
                    "Browser requests (with an Origin header) are not allowed.",
                    "origin_not_allowed"
                )
            );
            assert!(
                !r.headers
                    .keys()
                    .any(|name| name.starts_with("access-control-")),
                "{origin}: {:?}",
                r.headers
            );
        }
    }
    // A preflight is refused like any other request with an Origin.
    let r = h
        .send(
            "OPTIONS",
            "/v1/chat/completions",
            &[
                ("origin", "https://app.example"),
                ("access-control-request-method", "POST"),
            ],
            None,
        )
        .await;
    assert_eq!(r.status, 403);
    assert!(
        !r.headers
            .keys()
            .any(|name| name.starts_with("access-control-"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn gwy3_extra_hosts_require_a_token_even_with_tokens_off() {
    let h = Harness::start(|gateway| gateway.require_token = false);
    // Loopback only: tokens off lets an anonymous call through.
    assert_eq!(h.send("GET", "/v1/models", &[], None).await.status, 200);
    // A tunnel may rewrite Host to a loopback name, so every call needs a token now.
    h.set_gateway(|gateway| gateway.extra_allowed_hosts = vec!["abc.ngrok.app".to_owned()]);
    let tunnel = format!("abc.ngrok.app:{}", h.port());
    for host in [tunnel.as_str(), "127.0.0.1"] {
        let r = h.send("GET", "/v1/models", &[("host", host)], None).await;
        assert_eq!(r.status, 401, "{host}: {}", r.text());
    }
    let bearer = format!("Bearer {TOKEN}");
    let r = h
        .send(
            "GET",
            "/v1/models",
            &[("host", &tunnel), ("authorization", &bearer)],
            None,
        )
        .await;
    assert_eq!(r.status, 200);
    assert!(h.llm.gateway.status(&h.llm).token_required);
}
