//! Tests of the production transport against raw loopback servers.

use std::sync::Arc;
use std::time::Duration;

use tokio::net::TcpListener;

use super::admission::{GuardedResolver, RefusalSlot, resolved_by_name, validated_addresses};
use super::*;
use crate::llm::safe_http::test_server::{Step, get, post, serve_with};
use pingdeck_base::net_guard::HostClass;

/// Serves `reply` (raw HTTP bytes) once per connection after `delay`, with no chunked terminator.
async fn serve(reply: Vec<u8>, delay: Duration) -> u16 {
    serve_with(vec![Step::Pause(delay), Step::Send(reply)], false)
        .await
        .0
}

#[tokio::test]
async fn returns_a_redirect_without_following_it() {
    let port = serve(
        b"HTTP/1.1 302 Found\r\nlocation: http://169.254.169.254/x\r\ncontent-length: 0\r\n\r\n"
            .to_vec(),
        Duration::ZERO,
    )
    .await;
    let response = ReqwestSafeHttp::direct()
        .send(
            get(format!("http://127.0.0.1:{port}/")),
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert!(response.is_redirect());
    assert_eq!(
        response.header("location"),
        Some("http://169.254.169.254/x")
    );
}

#[tokio::test]
async fn caps_the_body() {
    let mut reply = b"HTTP/1.1 200 OK\r\ncontent-length: 5000\r\n\r\n".to_vec();
    reply.extend(std::iter::repeat_n(b'a', 5000));
    let port = serve(reply, Duration::ZERO).await;
    let response = ReqwestSafeHttp::direct()
        .send(
            get(format!("http://127.0.0.1:{port}/")),
            &CancelToken::new(),
        )
        .await
        .unwrap();
    assert!(response.truncated);
    assert_eq!(response.body.len(), 1024);
}

#[tokio::test]
async fn refused_targets_send_nothing_through_either_entry() {
    let transport = ReqwestSafeHttp::direct();
    let cancel = CancelToken::new();
    let limits = StreamLimits {
        first_byte: Duration::from_millis(500),
        idle: Duration::from_millis(500),
        total: None,
    };
    // Link-local is refused even when private targets are allowed.
    for (url, allow_private, reason) in [
        ("http://169.254.169.254/latest", true, "link_local"),
        ("http://127.0.0.1:9/", false, "private"),
        ("ftp://127.0.0.1/", true, "scheme"),
        ("http://", true, "invalid_url"),
    ] {
        let refused = SafeTransportError::Blocked {
            reason: reason.to_owned(),
        };
        let mut request = get(url.to_owned());
        request.allow_private = allow_private;
        assert_eq!(
            transport.send(request, &cancel).await.unwrap_err(),
            refused,
            "send {url}"
        );
        let mut request = post(url.to_owned());
        request.allow_private = allow_private;
        assert_eq!(
            transport
                .open_stream(request, limits, &cancel)
                .await
                .unwrap_err(),
            refused,
            "open_stream {url}"
        );
    }
}

#[tokio::test]
async fn the_system_route_resolver_validates_the_target_name_only() {
    use reqwest::dns::Resolve;
    use std::str::FromStr;
    // Without a system proxy reqwest resolves the target through this resolver: a refused
    // answer fails the lookup and is reported as a block, not as a DNS error.
    let refusal = RefusalSlot::default();
    let resolver = GuardedResolver {
        target: "localhost".into(),
        allow_private: false,
        refusal: Arc::clone(&refusal),
    };
    let name = reqwest::dns::Name::from_str("localhost").unwrap();
    assert!(resolver.resolve(name).await.is_err());
    assert_eq!(
        refusal.lock().unwrap().clone(),
        Some(SafeTransportError::Blocked {
            reason: "refused_address".into()
        })
    );
    // Another name is the system proxy's host: resolved as is.
    let resolver = GuardedResolver {
        target: "api.example".into(),
        allow_private: false,
        refusal: RefusalSlot::default(),
    };
    let name = reqwest::dns::Name::from_str("localhost").unwrap();
    assert!(resolver.resolve(name).await.is_ok());
}

#[tokio::test]
async fn times_out_and_cancels_promptly() {
    let port = serve(
        b"HTTP/1.1 200 OK\r\ncontent-length: 0\r\n\r\n".to_vec(),
        Duration::from_secs(30),
    )
    .await;
    let mut request = get(format!("http://127.0.0.1:{port}/"));
    request.timeout = Duration::from_millis(150);
    let error = ReqwestSafeHttp::direct()
        .send(request, &CancelToken::new())
        .await
        .unwrap_err();
    assert_eq!(error, SafeTransportError::Timeout);

    let cancel = CancelToken::new();
    let trigger = cancel.clone();
    tokio::spawn(async move {
        tokio::time::sleep(Duration::from_millis(50)).await;
        trigger.cancel();
    });
    // The server holds the answer for 30 s: only the cancel can end the call within the guard.
    let error = tokio::time::timeout(
        Duration::from_secs(10),
        ReqwestSafeHttp::direct().send(get(format!("http://127.0.0.1:{port}/")), &cancel),
    )
    .await
    .expect("the cancel did not end the call")
    .unwrap_err();
    assert_eq!(error, SafeTransportError::Cancelled);
}

#[tokio::test]
async fn connection_refused_is_reported_as_refused() {
    let listener = TcpListener::bind("127.0.0.1:0").await.unwrap();
    let port = listener.local_addr().unwrap().port();
    drop(listener);
    let error = ReqwestSafeHttp::direct()
        .send(
            get(format!("http://127.0.0.1:{port}/")),
            &CancelToken::new(),
        )
        .await
        .unwrap_err();
    assert_eq!(error, SafeTransportError::Refused);
}

#[test]
fn debug_never_prints_the_proxy_password() {
    let printed = format!(
        "{:?}",
        ReqwestSafeHttp::through_proxy("http://user:hunter2@proxy.test:8080")
    );
    assert!(!printed.contains("hunter2"), "{printed}");
    assert!(printed.contains("proxy.test:8080"), "{printed}");
    assert!(printed.contains("ca_roots: 0"), "{printed}");
    let printed = format!("{:?}", ReqwestSafeHttp::system());
    assert!(printed.contains("System"), "{printed}");
}

#[tokio::test(start_paused = true)]
async fn name_resolution_on_the_direct_route_is_bounded_by_the_connect_timeout() {
    let hanging = std::future::pending();
    let outcome = tokio::time::timeout(
        Duration::from_secs(60),
        validated_addresses(hanging, Duration::from_secs(3), None),
    )
    .await
    .expect("a hanging resolver held the call past the connect timeout");
    assert_eq!(outcome, Err(SafeTransportError::Timeout));
    // A refusal keeps its mapping.
    let refused = async { Err(pingdeck_base::error::AppError::validation("refused")) };
    assert_eq!(
        validated_addresses(refused, Duration::from_secs(3), Some(HostClass::Loopback)).await,
        Err(SafeTransportError::Blocked {
            reason: "private".into()
        })
    );
}

#[test]
fn every_dns_name_is_pinned_or_guarded_even_a_localhost_name() {
    // `localhost` and `*.localhost` are names the resolver may
    // send to DNS, so they are pinned (direct) or guarded (system route) like any other name.
    for (url, by_name) in [
        ("http://api.example/v1", true),
        ("http://localhost:11434/", true),
        ("http://foo.localhost/", true),
        ("http://127.0.0.1:8080/", false),
        ("http://[::1]/", false),
    ] {
        let parsed = url::Url::parse(url).unwrap();
        assert_eq!(resolved_by_name(&parsed), by_name, "{url}");
    }
}
