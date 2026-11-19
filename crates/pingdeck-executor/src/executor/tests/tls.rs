//! HTTPS end to end through `execute_request`: a workspace CA, mutual TLS and the request timeout,
//! against an in-process rustls server. Certificates are generated per run with a one-week
//! validity (macOS refuses long-lived leaves even under a custom root); the system trust store is
//! never touched.

use super::support::write_temp_file;
use crate::executor::execute_request;
use crate::executor::types::{ExecutionError, ExecutionResult, ExecutorOptions};
use pingdeck_base::error::NetworkCause;
use pingdeck_model::Request;
use pingdeck_model::models::{TlsCertificate, TlsCertificateId, TlsCertificateKind};
use rcgen::{
    BasicConstraints, CertificateParams, DnType, ExtendedKeyUsagePurpose, IsCa, Issuer, KeyPair,
    KeyUsagePurpose,
};
use rustls::pki_types::{CertificateDer, PrivateKeyDer, PrivatePkcs8KeyDer};
use rustls::server::WebPkiClientVerifier;
use rustls::{RootCertStore, ServerConfig, ServerConnection, StreamOwned};
use std::io::{Read, Write};
use std::net::TcpListener;
use std::path::Path;
use std::sync::{Arc, mpsc};
use std::time::{Duration, Instant};

type Outcomes = mpsc::Receiver<Result<String, String>>;

fn short_validity(params: &mut CertificateParams) {
    let now = time::OffsetDateTime::now_utc();
    params.not_before = now - time::Duration::days(1);
    params.not_after = now + time::Duration::days(7);
}

/// A fresh CA: its issuer and its certificate as DER and PEM.
fn new_ca() -> (Issuer<'static, KeyPair>, CertificateDer<'static>, String) {
    let mut params = CertificateParams::new(Vec::<String>::new()).unwrap();
    params
        .distinguished_name
        .push(DnType::CommonName, "PingDeck test CA");
    params.is_ca = IsCa::Ca(BasicConstraints::Unconstrained);
    params.key_usages = vec![
        KeyUsagePurpose::KeyCertSign,
        KeyUsagePurpose::CrlSign,
        KeyUsagePurpose::DigitalSignature,
    ];
    short_validity(&mut params);
    let key = KeyPair::generate().unwrap();
    let cert = params.self_signed(&key).unwrap();
    (Issuer::new(params, key), cert.der().clone(), cert.pem())
}

/// A `localhost` leaf signed by `issuer` for one purpose: its DER, PEM and key.
fn new_leaf(
    issuer: &Issuer<'_, KeyPair>,
    purpose: ExtendedKeyUsagePurpose,
) -> (CertificateDer<'static>, String, KeyPair) {
    let mut params = CertificateParams::new(vec!["localhost".to_owned()]).unwrap();
    params
        .distinguished_name
        .push(DnType::CommonName, "localhost");
    params.key_usages = vec![KeyUsagePurpose::DigitalSignature];
    params.extended_key_usages = vec![purpose];
    short_validity(&mut params);
    let key = KeyPair::generate().unwrap();
    let cert = params.signed_by(&key, issuer).unwrap();
    (cert.der().clone(), cert.pem(), key)
}

fn server_config(
    cert: CertificateDer<'static>,
    key: &KeyPair,
    client_ca: Option<CertificateDer<'static>>,
) -> ServerConfig {
    let provider = Arc::new(rustls::crypto::aws_lc_rs::default_provider());
    let builder = ServerConfig::builder_with_provider(Arc::clone(&provider))
        .with_safe_default_protocol_versions()
        .unwrap();
    let builder = match client_ca {
        Some(ca) => {
            let mut roots = RootCertStore::empty();
            roots.add(ca).unwrap();
            let verifier = WebPkiClientVerifier::builder_with_provider(Arc::new(roots), provider)
                .build()
                .unwrap();
            builder.with_client_cert_verifier(verifier)
        }
        None => builder.with_no_client_auth(),
    };
    let key = PrivateKeyDer::Pkcs8(PrivatePkcs8KeyDer::from(key.serialize_der()));
    builder.with_single_cert(vec![cert], key).unwrap()
}

/// Answers `ok` over TLS on every connection. Each connection's outcome (the request head, or the
/// handshake error) goes to the returned channel.
fn serve_tls(config: ServerConfig) -> (String, Outcomes) {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let url = format!(
        "https://localhost:{}/tls",
        listener.local_addr().unwrap().port()
    );
    let config = Arc::new(config);
    let (tx, rx) = mpsc::channel();
    std::thread::spawn(move || {
        for tcp in listener.incoming().flatten() {
            let _ = tcp.set_read_timeout(Some(Duration::from_secs(10)));
            let connection = ServerConnection::new(Arc::clone(&config)).unwrap();
            let mut tls = StreamOwned::new(connection, tcp);
            let (mut head, mut chunk) = (Vec::new(), [0_u8; 1024]);
            let outcome = loop {
                match tls.read(&mut chunk) {
                    Ok(0) => break Err("closed before a request".to_owned()),
                    Ok(read) => {
                        head.extend_from_slice(&chunk[..read]);
                        if head.windows(4).any(|window| window == b"\r\n\r\n") {
                            break Ok(String::from_utf8_lossy(&head).into_owned());
                        }
                    }
                    Err(error) => break Err(error.to_string()),
                }
            };
            if outcome.is_ok() {
                let _ = tls.write_all(
                    b"HTTP/1.1 200 OK\r\nContent-Length: 2\r\nConnection: close\r\n\r\nok",
                );
                tls.conn.send_close_notify();
                let _ = tls.flush();
            }
            if tx.send(outcome).is_err() {
                break;
            }
        }
    });
    (url, rx)
}

fn next(outcomes: &Outcomes) -> Result<String, String> {
    outcomes.recv_timeout(Duration::from_secs(10)).unwrap()
}

fn entry(kind: TlsCertificateKind, host: &str, cert: &Path, key: Option<&Path>) -> TlsCertificate {
    TlsCertificate {
        id: TlsCertificateId::new("cert_tls").unwrap(),
        workspace_id: pingdeck_model::WorkspaceId::new("ws_tls").unwrap(),
        kind,
        host_pattern: host.to_owned(),
        cert_path: cert.to_string_lossy().into_owned(),
        key_path: key.map(|path| path.to_string_lossy().into_owned()),
        passphrase_ref: None,
        enabled: true,
    }
}

async fn send(
    request: &Request,
    certificates: Vec<TlsCertificate>,
) -> Result<ExecutionResult, ExecutionError> {
    let options = ExecutorOptions {
        certificates,
        ..ExecutorOptions::default()
    };
    execute_request(request, None, options).await
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn https_trusts_a_workspace_ca_and_refuses_an_unknown_issuer() {
    let (issuer, _, ca_pem) = new_ca();
    let (cert, _, key) = new_leaf(&issuer, ExtendedKeyUsagePurpose::ServerAuth);
    let (url, outcomes) = serve_tls(server_config(cert, &key, None));
    let ca_file = write_temp_file("ca.pem", ca_pem.as_bytes());
    let ca = entry(TlsCertificateKind::Ca, "", ca_file.path(), None);
    let mut disabled = ca.clone();
    disabled.enabled = false;
    let mut request = Request::new("req_tls", "ws_tls", "TLS", &url).unwrap();

    for certificates in [Vec::new(), vec![disabled]] {
        let error = send(&request, certificates).await.unwrap_err();
        assert!(
            matches!(
                error,
                ExecutionError::Network {
                    cause: Some(NetworkCause::Tls),
                    ..
                }
            ),
            "{error:?}"
        );
        assert!(next(&outcomes).is_err());
    }

    let trusted = send(&request, vec![ca]).await.unwrap();
    assert_eq!(
        (trusted.status_code, trusted.body_text.as_str()),
        (200, "ok")
    );
    assert!(next(&outcomes).unwrap().starts_with("GET /tls HTTP/1.1"));

    request.network_options.verify_tls = false;
    assert_eq!(send(&request, Vec::new()).await.unwrap().status_code, 200);
    assert!(next(&outcomes).is_ok());
}

#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mtls_presents_only_the_client_certificate_matching_the_host() {
    let (issuer, ca_der, ca_pem) = new_ca();
    let (cert, _, key) = new_leaf(&issuer, ExtendedKeyUsagePurpose::ServerAuth);
    let (url, outcomes) = serve_tls(server_config(cert, &key, Some(ca_der)));
    let (_, client_pem, client_key) = new_leaf(&issuer, ExtendedKeyUsagePurpose::ClientAuth);
    let ca_file = write_temp_file("ca.pem", ca_pem.as_bytes());
    let cert_file = write_temp_file("client.pem", client_pem.as_bytes());
    let key_file = write_temp_file("client.key", client_key.serialize_pem().as_bytes());
    let ca = entry(TlsCertificateKind::Ca, "", ca_file.path(), None);
    let client = |host: &str| {
        let kind = TlsCertificateKind::Client;
        entry(kind, host, cert_file.path(), Some(key_file.path()))
    };
    let request = Request::new("req_mtls", "ws_tls", "mTLS", &url).unwrap();

    for certificates in [
        vec![ca.clone()],
        vec![ca.clone(), client("api.example.test")],
    ] {
        let error = send(&request, certificates).await.unwrap_err();
        assert!(matches!(error, ExecutionError::Network { .. }), "{error}");
        let refused = next(&outcomes).unwrap_err();
        assert!(refused.contains("no certificates"), "{refused}");
    }

    let mutual = send(&request, vec![ca, client("localhost")]).await.unwrap();
    assert_eq!((mutual.status_code, mutual.body_text.as_str()), (200, "ok"));
    assert!(next(&outcomes).is_ok());
}

/// The secret store of one certificate: every lookup answers its passphrase.
struct OnePassphrase(&'static str);

impl pingdeck_base::secrets::SecretStore for OnePassphrase {
    fn get(&self, _: &str) -> Result<Option<String>, pingdeck_base::error::AppError> {
        Ok(Some(self.0.to_owned()))
    }
    fn put(
        &self,
        _: &str,
        _: &str,
    ) -> Result<pingdeck_base::secrets::SecretHint, pingdeck_base::error::AppError> {
        Err(pingdeck_base::error::AppError::internal("read-only"))
    }
    fn delete(&self, _: &str) -> Result<bool, pingdeck_base::error::AppError> {
        Ok(false)
    }
    fn hint(
        &self,
        _: &str,
    ) -> Result<pingdeck_base::secrets::SecretHint, pingdeck_base::error::AppError> {
        Err(pingdeck_base::error::AppError::internal("read-only"))
    }
    fn active_redaction_needles(&self) -> Vec<String> {
        Vec::new()
    }
}

async fn send_with_passphrase(
    request: &Request,
    certificates: Vec<TlsCertificate>,
    passphrase: Option<&'static str>,
) -> Result<ExecutionResult, ExecutionError> {
    let mut options = ExecutorOptions {
        certificates,
        ..ExecutorOptions::default()
    };
    if let Some(passphrase) = passphrase {
        options.auxiliary.tls.passphrases =
            crate::executor::CertificatePassphrases::new(Arc::new(OnePassphrase(passphrase)));
    }
    execute_request(request, None, options).await
}

/// The stored passphrase opens a PKCS#8 `ENCRYPTED PRIVATE KEY` and a PKCS#12 bundle (no key
/// file); a missing or wrong passphrase is a client error naming the gap, never a silent send
/// without the certificate.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn mtls_opens_a_passphrase_protected_key_and_a_pkcs12_bundle() {
    use pkcs8::der::pem::LineEnding;
    let (issuer, ca_der, ca_pem) = new_ca();
    let (cert, _, key) = new_leaf(&issuer, ExtendedKeyUsagePurpose::ServerAuth);
    let (url, outcomes) = serve_tls(server_config(cert, &key, Some(ca_der)));
    let (client_der, client_pem, client_key) =
        new_leaf(&issuer, ExtendedKeyUsagePurpose::ClientAuth);
    let key_der = client_key.serialize_der();
    let params =
        pkcs8::pkcs5::pbes2::Parameters::pbkdf2_sha256_aes256cbc(2048, &[7; 16], &[9; 16]).unwrap();
    let encrypted = pkcs8::PrivateKeyInfo::try_from(key_der.as_slice())
        .unwrap()
        .encrypt_with_params(params, "s3cret")
        .unwrap()
        .to_pem("ENCRYPTED PRIVATE KEY", LineEnding::LF)
        .unwrap();
    let ca_file = write_temp_file("ca.pem", ca_pem.as_bytes());
    let cert_file = write_temp_file("client.pem", client_pem.as_bytes());
    let key_file = write_temp_file("client.key", encrypted.as_bytes());
    let ca = entry(TlsCertificateKind::Ca, "", ca_file.path(), None);
    let mut protected = entry(
        TlsCertificateKind::Client,
        "localhost",
        cert_file.path(),
        Some(key_file.path()),
    );
    protected.passphrase_ref = Some("tls.ws_tls.cert_tls.passphrase".to_owned());
    let request = Request::new("req_mtls_key", "ws_tls", "mTLS", &url).unwrap();

    for (passphrase, says) in [
        (None, "protected by a passphrase"),
        (Some("wrong"), "does not open the key"),
    ] {
        let error = send_with_passphrase(&request, vec![ca.clone(), protected.clone()], passphrase)
            .await
            .unwrap_err();
        assert!(
            matches!(&error, ExecutionError::ClientBuild(message) if message.contains(says)),
            "{error}"
        );
    }
    let opened = send_with_passphrase(&request, vec![ca.clone(), protected], Some("s3cret"))
        .await
        .unwrap();
    assert_eq!(opened.status_code, 200);
    assert!(next(&outcomes).is_ok());

    let mut keystore = p12_keystore::KeyStore::new();
    keystore.add_entry(
        "client",
        p12_keystore::KeyStoreEntry::PrivateKeyChain(p12_keystore::PrivateKeyChain::new(
            &key_der,
            b"client",
            vec![p12_keystore::Certificate::from_der(&client_der).unwrap()],
        )),
    );
    let bundle_file = write_temp_file("client.p12", &keystore.writer("s3cret").write().unwrap());
    let mut bundle = entry(
        TlsCertificateKind::Client,
        "localhost",
        bundle_file.path(),
        None,
    );
    bundle.id = TlsCertificateId::new("cert_bundle").unwrap();
    bundle.passphrase_ref = Some("tls.ws_tls.cert_bundle.passphrase".to_owned());
    let opened = send_with_passphrase(&request, vec![ca, bundle], Some("s3cret"))
        .await
        .unwrap();
    assert_eq!(opened.status_code, 200);
    assert!(next(&outcomes).is_ok());
}

/// A peer that accepts the connection (the kernel backlog does) but never answers: the plain
/// request and the TLS handshake both end at `timeout_ms` as a network error. Today neither counts
/// as a connect-phase failure, so a stalled handshake is not retried for a non-idempotent request.
#[tokio::test(flavor = "multi_thread", worker_threads = 2)]
async fn a_silent_server_times_out_within_the_request_timeout() {
    let listener = TcpListener::bind("127.0.0.1:0").unwrap();
    let port = listener.local_addr().unwrap().port();
    for scheme in ["http", "https"] {
        let url = format!("{scheme}://localhost:{port}/slow");
        let mut request = Request::new("req_slow", "ws_tls", "Slow", &url).unwrap();
        request.network_options.timeout_ms = 300;
        let started = Instant::now();
        let error = send(&request, Vec::new()).await.unwrap_err();
        let elapsed = started.elapsed();
        assert!(
            matches!(
                error,
                ExecutionError::Network {
                    connect_phase: false,
                    cause: Some(NetworkCause::Timeout),
                    ..
                }
            ),
            "{scheme}: {error:?}"
        );
        assert!(
            (Duration::from_millis(300)..Duration::from_secs(10)).contains(&elapsed),
            "{scheme}: {elapsed:?}"
        );
    }
    drop(listener);
}

/// A node of a client error's source chain, standing in for reqwest's and hyper-util's.
#[derive(Debug)]
struct Node(
    &'static str,
    Option<Box<dyn std::error::Error + Send + Sync>>,
);

impl std::fmt::Display for Node {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.write_str(self.0)
    }
}

impl std::error::Error for Node {
    fn source(&self) -> Option<&(dyn std::error::Error + 'static)> {
        self.1.as_deref().map(|inner| inner as _)
    }
}

#[test]
fn a_network_cause_is_read_from_the_error_types_in_the_chain() {
    use pingdeck_base::error::transport_cause;
    use std::io::{Error, ErrorKind};
    let connect = |inner: Error| Node("client error (Connect)", Some(Box::new(inner)));
    let tunnel = |text: &'static str, inner: Option<Box<dyn std::error::Error + Send + Sync>>| {
        Node("client error (Connect)", Some(Box::new(Node(text, inner))))
    };
    let cases: [(Node, bool, Option<NetworkCause>); 10] = [
        (
            connect(Error::from(ErrorKind::ConnectionRefused)),
            false,
            Some(NetworkCause::Refused),
        ),
        (
            connect(Error::from(ErrorKind::ConnectionReset)),
            false,
            Some(NetworkCause::Reset),
        ),
        (
            connect(Error::from(ErrorKind::TimedOut)),
            false,
            Some(NetworkCause::Timeout),
        ),
        // The connector wraps the TLS stream's InvalidData error in one of kind Other.
        (
            connect(Error::other(Error::new(
                ErrorKind::InvalidData,
                "invalid peer certificate",
            ))),
            false,
            Some(NetworkCause::Tls),
        ),
        (
            Node(
                "client error (Connect)",
                Some(Box::new(Node(
                    "dns error",
                    Some(Box::new(Error::other("failed to lookup address"))),
                ))),
            ),
            false,
            Some(NetworkCause::Dns),
        ),
        // The proxy's own failures, each from the connector's typed node.
        (
            tunnel("tunnel error: proxy authorization required", None),
            true,
            Some(NetworkCause::ProxyAuth),
        ),
        (
            tunnel("tunnel error: unsuccessful", None),
            true,
            Some(NetworkCause::ProxyRefused),
        ),
        (
            tunnel(
                "tunnel error: failed to create underlying connection",
                Some(Box::new(Error::from(ErrorKind::ConnectionRefused))),
            ),
            true,
            Some(NetworkCause::ProxyUnreachable),
        ),
        // Behind a proxy the refusing peer is the proxy (a plain-HTTP target is not tunnelled).
        (
            connect(Error::from(ErrorKind::ConnectionRefused)),
            true,
            Some(NetworkCause::ProxyUnreachable),
        ),
        (Node("error sending request", None), false, None),
    ];
    for (error, proxied, expected) in cases {
        assert_eq!(transport_cause(&error, proxied), expected, "{error}");
    }
}
