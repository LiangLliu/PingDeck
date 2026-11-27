//! Tests of the upstream client routing and the CA bundle of [`LlmRuntime::http_for`].

use std::sync::Arc;

use super::*;
use crate::llm::ca_bundle::TEST_CA_PEM as CA_PEM;
use crate::llm::runtime::DbHandle;
use spanbay_model::settings::Settings;
use spanbay_store::secrets_sqlite::SqliteSecretStore;

const PROXY: &str = "http://proxy.test:3128";

fn settings(proxy: &str, no_proxy: &[&str], ca: &str) -> Settings {
    let mut settings = Settings::default();
    settings.network.proxy_url = proxy.into();
    settings.network.no_proxy = no_proxy.iter().map(|entry| (*entry).to_owned()).collect();
    settings.network.ca_bundle_path = ca.into();
    settings
}

fn runtime(settings: Settings) -> LlmRuntime {
    LlmRuntime::new(
        DbHandle::Unavailable,
        Arc::new(settings),
        Arc::new(SqliteSecretStore::in_memory().unwrap()),
    )
    .unwrap()
}

fn temp_file(name: &str, contents: &[u8]) -> String {
    let path = std::env::temp_dir().join(format!("pd-llm-ca-{name}-{}.pem", std::process::id()));
    std::fs::write(&path, contents).unwrap();
    path.to_string_lossy().into_owned()
}

fn at(settings: &Settings, host: &str, provider_proxy: &str) -> ProxyRoute {
    let network = &settings.network;
    route(provider_proxy, &network.proxy_url, &network.no_proxy, host)
}

fn proxied(runtime: &LlmRuntime) -> usize {
    runtime.http_clients.lock().unwrap().proxied.len()
}

#[test]
fn routing_follows_the_provider_proxy_no_proxy_local_hosts_then_the_global_proxy() {
    let configured = settings(PROXY, &["intranet.corp", "*.lan", "10.9.0.0/16"], "");
    let proxy = || ProxyRoute::Proxy(PROXY.to_owned());
    for (host, provider_proxy, want) in [
        ("api.openai.com", "", proxy()),
        (
            "api.openai.com",
            " socks5://own:1080 ",
            ProxyRoute::Proxy("socks5://own:1080".into()),
        ),
        // The provider's own proxy beats every bypass.
        (
            "localhost",
            "socks5://own:1080",
            ProxyRoute::Proxy("socks5://own:1080".into()),
        ),
        ("intranet.corp", "", ProxyRoute::Direct),
        ("wiki.intranet.corp", "", ProxyRoute::Direct),
        ("ollama.lan", "", ProxyRoute::Direct),
        ("10.9.8.7", "", ProxyRoute::Direct),
        // One local rule for chat and the safe transport: loopback, private, CGNAT, ULA, mDNS.
        ("localhost", "", ProxyRoute::Direct),
        ("127.0.0.1", "", ProxyRoute::Direct),
        ("[::1]", "", ProxyRoute::Direct),
        ("192.168.1.9", "", ProxyRoute::Direct),
        ("100.64.0.7", "", ProxyRoute::Direct),
        ("[fd12:3456::1]", "", ProxyRoute::Direct),
        ("mac-studio.local", "", ProxyRoute::Direct),
        ("notintranet.corp", "", proxy()),
    ] {
        assert_eq!(at(&configured, host, provider_proxy), want, "{host}");
    }
    let unproxied = settings("", &[], "");
    assert_eq!(at(&unproxied, "api.openai.com", ""), ProxyRoute::System);
    assert_eq!(at(&unproxied, "100.64.0.7", ""), ProxyRoute::Direct);
}

#[test]
fn http_for_skips_the_global_proxy_for_no_proxy_hosts() {
    let runtime = runtime(settings(PROXY, &["intranet.corp"], ""));
    runtime.http_for("https://intranet.corp/v1", "").unwrap();
    runtime.http_for("http://100.64.0.7:11434", "").unwrap();
    runtime
        .http_for("http://mac-studio.local:11434", "")
        .unwrap();
    assert_eq!(proxied(&runtime), 0);
    runtime.http_for("https://api.openai.com/v1", "").unwrap();
    runtime
        .http_for("https://api.anthropic.com/v1", "")
        .unwrap();
    assert_eq!(proxied(&runtime), 1, "one client per proxy URL");
}

#[test]
fn http_for_refuses_link_local_and_metadata_literals_even_behind_a_proxy() {
    let runtime = runtime(settings(PROXY, &[], ""));
    for url in [
        "http://169.254.169.254/latest/meta-data",
        "http://[fe80::1]/",
        "http://[fd00:ec2::254]/",
        "http://100.100.100.200/",
        "http://metadata.google.internal/computeMetadata/v1",
        "http://0.0.0.0:8080/",
    ] {
        for provider_proxy in ["", "http://own.test:1"] {
            let error = runtime.http_for(url, provider_proxy).unwrap_err();
            assert!(matches!(error, AppError::Validation { .. }), "{url}");
        }
    }
    runtime.http_for("http://192.168.1.20:11434", "").unwrap();
}

#[test]
fn the_ca_bundle_is_loaded_and_follows_the_setting() {
    let good = temp_file("good", CA_PEM.as_bytes());
    let empty = temp_file("empty", b"not a certificate");
    let runtime = runtime(settings("", &[], &good));
    runtime.http_for("https://api.openai.com", "").unwrap();
    assert_eq!(runtime.http_clients.lock().unwrap().ca_bundle, good);

    // Same path again: the bundle is not read again (the file may be gone).
    std::fs::remove_file(&good).unwrap();
    runtime.http_for("https://api.openai.com", "").unwrap();

    // A new path rebuilds the clients; an unusable bundle is an error, never a silent fallback.
    runtime.set_settings(Arc::new(settings(PROXY, &[], &empty)));
    let error = runtime.http_for("https://api.openai.com", "").unwrap_err();
    assert!(matches!(error, AppError::Validation { .. }), "{error:?}");
    assert!(
        error.message().contains("ca_bundle_path"),
        "{}",
        error.message()
    );
    runtime.set_settings(Arc::new(settings(PROXY, &[], "/nonexistent/ca.pem")));
    assert!(runtime.http_for("https://api.openai.com", "").is_err());

    // Cleared: the system roots again, and the proxy clients were rebuilt with the set.
    runtime.set_settings(Arc::new(settings(PROXY, &[], "")));
    runtime.http_for("https://api.openai.com", "").unwrap();
    assert_eq!(runtime.http_clients.lock().unwrap().ca_bundle, "");
    assert_eq!(proxied(&runtime), 1);
    std::fs::remove_file(empty).unwrap();
}
