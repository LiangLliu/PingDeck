//! Client construction and caching: proxy handling, TLS versions, certificates and cache keys.

use super::support::{TestServer, write_temp_file};
use crate::executor::client::{
    build_client, build_client_async, cached_entries, certificate_host_score, client_cache_key,
    http_version, insert_client,
};
use crate::executor::trace::{mask_proxy_credentials, transport_trace};
use crate::executor::types::{AuxiliaryNetwork, ExecutorOptions};
use pingdeck_model::Request;
use pingdeck_model::models::{
    HttpVersionPreference, TlsCertificate, TlsCertificateId, TlsCertificateKind,
    TlsVersionPreference,
};
use reqwest::Version as HttpVersion;
use std::fs;
use url::Url;

#[test]
fn proxy_masking_http_versions_tls_validation_host_scores_and_cache_keys_hold() {
    for (raw, masked) in [
        (
            "http://alice:s3cret@proxy.local:8080",
            "http://alice:****@proxy.local:8080/",
        ),
        ("socks5://:pw@10.0.0.1:1080", "socks5://:****@10.0.0.1:1080"),
        (
            "alice:s3cret@proxy.local:3128",
            "alice:****@proxy.local:3128/",
        ),
        (
            "http://alice@proxy.local:8080",
            "http://alice@proxy.local:8080",
        ),
        ("http://127.0.0.1:8080", "http://127.0.0.1:8080"),
        ("127.0.0.1:8080", "127.0.0.1:8080"),
    ] {
        assert_eq!(mask_proxy_credentials(raw), masked, "{raw}");
    }

    let mut request =
        Request::new("req_proxy", "ws_proxy", "Proxy", "http://example.test/").unwrap();
    let url = Url::parse("http://example.test/").unwrap();
    let direct = transport_trace(
        &request,
        &ExecutorOptions::default(),
        &url,
        "HTTP/1.1",
        None,
        vec![],
    );
    assert!(direct.proxy_url.is_none() && !direct.proxy_bypassed);
    request.network_options.proxy_url = Some(" http://alice:s3cret@proxy.local:8080 ".into());
    let proxied = transport_trace(
        &request,
        &ExecutorOptions::default(),
        &url,
        "HTTP/1.1",
        None,
        vec![],
    );
    assert_eq!(
        (proxied.proxy_url.as_deref(), proxied.proxy_bypassed),
        (Some("http://alice:****@proxy.local:8080/"), false)
    );

    assert_eq!(http_version(HttpVersionPreference::Auto), None);
    assert_eq!(
        http_version(HttpVersionPreference::Http10),
        Some(HttpVersion::HTTP_10)
    );
    assert_eq!(
        http_version(HttpVersionPreference::Http11),
        Some(HttpVersion::HTTP_11)
    );
    assert_eq!(
        http_version(HttpVersionPreference::Http2),
        Some(HttpVersion::HTTP_2)
    );

    let mut tls_req = Request::new("req_tls", "ws_billing", "TLS", "https://example.com").unwrap();
    tls_req.network_options.disabled_tls_versions =
        vec![TlsVersionPreference::Tls12, TlsVersionPreference::Tls13];
    let https_url = Url::parse("https://example.com/").unwrap();
    assert!(
        build_client(
            &tls_req,
            &[],
            &https_url,
            None,
            &AuxiliaryNetwork::default()
        )
        .unwrap_err()
        .to_string()
        .contains("all TLS versions")
    );

    let host = "api.internal.corp";
    assert_eq!(
        certificate_host_score("*.internal.corp", host, 443),
        Some((0, 0, 15))
    );
    assert_eq!(
        certificate_host_score("api.internal.corp:8443", host, 8443),
        Some((1, 1, 17))
    );
    assert_eq!(
        certificate_host_score("api.internal.corp:8443", host, 443),
        None
    );
    assert_eq!(certificate_host_score("*", host, 443), Some((0, 0, 0)));
    assert_eq!(certificate_host_score("other.example.com", host, 443), None);

    let cache_req = Request::new("req_cache", "ws_billing", "Cache", "https://api.test/").unwrap();
    let base = TlsCertificate {
        id: TlsCertificateId::new("cert_client").unwrap(),
        workspace_id: pingdeck_model::WorkspaceId::new("ws_billing").unwrap(),
        kind: TlsCertificateKind::Client,
        host_pattern: "api.test".into(),
        cert_path: "/certs/first.pem".into(),
        key_path: Some("/certs/first.key".into()),
        passphrase_ref: None,
        enabled: true,
    };
    let mut replaced = base.clone();
    replaced.cert_path = "/certs/second.pem".into();
    replaced.key_path = Some("/certs/second.key".into());
    let mut disabled = base.clone();
    disabled.enabled = false;
    let key = |certs: &[TlsCertificate]| {
        client_cache_key(
            &cache_req,
            certs,
            "api.test",
            443,
            None,
            &AuxiliaryNetwork::default(),
        )
    };
    assert_eq!(
        key(std::slice::from_ref(&base)),
        key(std::slice::from_ref(&base))
    );
    assert_ne!(
        key(std::slice::from_ref(&base)),
        key(std::slice::from_ref(&replaced))
    );
    assert_ne!(key(std::slice::from_ref(&base)), key(&[]));
    assert_eq!(key(std::slice::from_ref(&disabled)), key(&[]));

    let pem = write_temp_file("ca.pem", b"first");
    let ca_cert = TlsCertificate {
        id: TlsCertificateId::new("cert_ca").unwrap(),
        workspace_id: pingdeck_model::WorkspaceId::new("ws_billing").unwrap(),
        kind: TlsCertificateKind::Ca,
        host_pattern: String::new(),
        cert_path: pem.path().to_string_lossy().into_owned(),
        key_path: Some("/certs/missing.key".into()),
        passphrase_ref: None,
        enabled: true,
    };
    let before = key(std::slice::from_ref(&ca_cert));
    assert!(before.contains('?'));
    fs::write(pem.path(), b"second, longer contents").unwrap();
    assert_ne!(before, key(std::slice::from_ref(&ca_cert)));
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn direct_and_proxied_builds_async_cert_errors_and_cache_dedup_hold() {
    let server = TestServer::spawn("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
    let target = format!("{}/direct", server.base_url);
    let url = Url::parse(&target).unwrap();
    let mut request = Request::new("req_direct", "ws_billing", "Direct", &target).unwrap();

    let client = build_client(&request, &[], &url, None, &AuxiliaryNetwork::default()).unwrap();
    assert_eq!(client.get(url.clone()).send().await.unwrap().status(), 200);

    request.network_options.proxy_url = Some("http://127.0.0.1:9".into());
    let proxied = build_client(&request, &[], &url, None, &AuxiliaryNetwork::default()).unwrap();
    assert!(proxied.get(url).send().await.is_err());
    assert!(
        server
            .received_request()
            .starts_with("get /direct http/1.1")
    );

    let ca_req = Request::new("req_ca", "ws_billing", "CA", "https://dup-build.test/").unwrap();
    let https_url = Url::parse("https://dup-build.test/").unwrap();
    let missing_ca = TlsCertificate {
        id: TlsCertificateId::new("cert_ca").unwrap(),
        workspace_id: pingdeck_model::WorkspaceId::new("ws_billing").unwrap(),
        kind: TlsCertificateKind::Ca,
        host_pattern: String::new(),
        cert_path: "/definitely/missing/ca.pem".into(),
        key_path: None,
        passphrase_ref: None,
        enabled: true,
    };
    assert!(
        build_client(
            &ca_req,
            std::slice::from_ref(&missing_ca),
            &https_url,
            None,
            &AuxiliaryNetwork::default()
        )
        .unwrap_err()
        .to_string()
        .contains("/definitely/missing/ca.pem")
    );
    assert!(
        build_client_async(
            &ca_req,
            std::slice::from_ref(&missing_ca),
            &https_url,
            None,
            &AuxiliaryNetwork::default()
        )
        .await
        .unwrap_err()
        .to_string()
        .contains("/definitely/missing/ca.pem")
    );

    let key = client_cache_key(
        &ca_req,
        &[],
        "dup-build.test",
        443,
        None,
        &AuxiliaryNetwork::default(),
    );
    insert_client(key.clone(), &client);
    insert_client(key.clone(), &client);
    assert!(cached_entries(&key) <= 1);

    let network = AuxiliaryNetwork::default();
    let (first, second) = tokio::join!(
        build_client_async(&ca_req, &[], &https_url, None, &network),
        build_client_async(&ca_req, &[], &https_url, None, &network),
    );
    first.unwrap();
    second.unwrap();
    assert!(cached_entries(&key) <= 1);
}

/// Acceptance 3: a request that follows Settings reaches a local Mock directly even when
/// the global proxy is set (and dead); the trace says the proxy was bypassed.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_request_following_settings_reaches_a_local_mock_past_the_global_proxy() {
    use pingdeck_model::models::NetworkField;
    let server = TestServer::spawn("HTTP/1.1 200 OK\r\nContent-Length: 2\r\n\r\nok");
    let target = format!("{}/mock", server.base_url);
    let url = Url::parse(&target).unwrap();
    let mut request = Request::new("req_local", "ws_billing", "Local", &target).unwrap();
    request.network_options.inherit = vec![NetworkField::ProxyUrl];
    let network = AuxiliaryNetwork {
        proxy_url: "http://127.0.0.1:9".into(),
        no_proxy: vec!["localhost".into()],
        ..AuxiliaryNetwork::default()
    };
    let client = build_client(&request, &[], &url, None, &network).unwrap();
    assert_eq!(client.get(url.clone()).send().await.unwrap().status(), 200);
    let options = ExecutorOptions {
        auxiliary: network,
        ..ExecutorOptions::default()
    };
    let trace = transport_trace(&request, &options, &url, "HTTP/1.1", None, vec![]);
    assert!(trace.proxy_bypassed && trace.proxy_url.is_none());
}
