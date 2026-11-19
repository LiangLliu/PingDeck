//! The one-call checks Settings runs on request: a CA bundle file and a web-search backend.
//!
//! Single responsibility: answer "does this work?" with a result the page can show inline. A
//! check that finds a problem returns it in the result (`ok: false` and a sentence); only a
//! malformed request is an error. Nothing is stored and no secret is echoed.

use std::io::Read;
use std::path::Path;
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

use crate::agent::search::{SearchConfig, http_client, web_search};
use crate::error::AppError;
use crate::secrets::{SEARCH_BRAVE, SEARCH_TAVILY};
use crate::settings::{AiSearchSettings, SearchBackend};

use super::AppState;

/// Largest CA bundle file read, in bytes (a system bundle is about 200 KB).
const MAX_CA_BUNDLE_BYTES: u64 = 8 * 1024 * 1024;

/// Longest a search test waits for the backend.
const SEARCH_TEST_TIMEOUT: Duration = Duration::from_secs(15);

/// The query a search test sends.
const SEARCH_TEST_QUERY: &str = "PingDeck";

/// What a CA bundle file holds.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CaBundleInfoDto {
    /// The file is a usable bundle with at least one certificate.
    pub ok: bool,
    /// Certificates found (0 when the file is unusable).
    pub certificates: u32,
    /// What is wrong with the file; absent when `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

/// How a search backend answered one test query.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SearchTestResultDto {
    /// The backend returned a result list (possibly empty).
    pub ok: bool,
    /// The backend that was called: the chosen one, or what `auto` resolved to.
    pub backend: SearchBackend,
    /// Results returned.
    pub results: u32,
    /// How long the call took.
    pub elapsed_ms: u64,
    /// Why the call failed; absent when `ok`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub error: Option<String>,
}

fn bundle_problem(error: impl Into<String>) -> CaBundleInfoDto {
    CaBundleInfoDto {
        ok: false,
        certificates: 0,
        error: Some(error.into()),
    }
}

/// Reads the PEM bundle at `path` and counts its certificates, checking that each one can serve as
/// a trusted root. Blocking: it reads the file.
///
/// # Errors
/// [`AppError::Validation`] when `path` is blank, not absolute, over 1024 bytes or holds control
/// characters (the rules of the `network.ca_bundle_path` setting); every problem with the file
/// itself is reported in the result.
pub fn network_ca_test_impl(path: &str) -> Result<CaBundleInfoDto, AppError> {
    let path = path.trim();
    if path.is_empty()
        || path.len() > 1024
        || path.chars().any(char::is_control)
        || !Path::new(path).is_absolute()
    {
        return Err(AppError::validation(
            "the CA bundle path must be an absolute path of at most 1024 bytes without control characters",
        ));
    }
    let mut bytes = Vec::new();
    let read = std::fs::File::open(path)
        .and_then(|file| file.take(MAX_CA_BUNDLE_BYTES + 1).read_to_end(&mut bytes));
    if let Err(error) = read {
        return Ok(bundle_problem(format!(
            "The file could not be read: {}",
            error.kind()
        )));
    }
    if u64::try_from(bytes.len()).unwrap_or(u64::MAX) > MAX_CA_BUNDLE_BYTES {
        return Ok(bundle_problem("The file is larger than 8 MiB"));
    }
    let Ok(certificates) = reqwest::Certificate::from_pem_bundle(&bytes) else {
        return Ok(bundle_problem("No certificates found in this file"));
    };
    if certificates.is_empty() {
        return Ok(bundle_problem("No certificates found in this file"));
    }
    let count = u32::try_from(certificates.len()).unwrap_or(u32::MAX);
    let usable = certificates
        .into_iter()
        .try_fold(reqwest::Client::builder(), |builder, certificate| {
            Some(builder.add_root_certificate(certificate))
        })
        .is_some_and(|builder| builder.build().is_ok());
    if !usable {
        return Ok(bundle_problem(
            "The certificates in this file cannot be used as trusted roots",
        ));
    }
    Ok(CaBundleInfoDto {
        ok: true,
        certificates: count,
        error: None,
    })
}

/// Sends one query through `config` and reports the outcome.
async fn run_search_test(config: &SearchConfig, client: &reqwest::Client) -> SearchTestResultDto {
    let backend = config.resolved().unwrap_or(config.backend);
    let started = Instant::now();
    let outcome = tokio::time::timeout(
        SEARCH_TEST_TIMEOUT,
        web_search(config, client, SEARCH_TEST_QUERY, 3, None),
    )
    .await;
    let (results, error) = match outcome {
        Ok(Ok(hits)) => (u32::try_from(hits.len()).unwrap_or(u32::MAX), None),
        Ok(Err(error)) => (0, Some(error.message().to_owned())),
        Err(_) => (
            0,
            Some("The search did not answer within 15 seconds".to_owned()),
        ),
    };
    SearchTestResultDto {
        ok: error.is_none(),
        backend,
        results,
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        error,
    }
}

/// Runs one search through `backend` with the stored keys and URL: the "Test search" button.
/// `auto` tests the backend it resolves to. A missing key or URL is reported in the result,
/// naming the setting to fill in.
///
/// # Errors
/// [`AppError::Internal`] when the secret lookup is interrupted.
pub async fn ai_search_test_impl(
    state: &AppState,
    backend: SearchBackend,
) -> Result<SearchTestResultDto, AppError> {
    let settings = state.settings();
    let search = AiSearchSettings {
        backend,
        ..settings.ai.search.clone()
    };
    let secrets = std::sync::Arc::clone(state.secrets());
    let (tavily, brave) = tokio::task::spawn_blocking(move || {
        Ok::<_, AppError>((
            secrets.get(SEARCH_TAVILY)?.unwrap_or_default(),
            secrets.get(SEARCH_BRAVE)?.unwrap_or_default(),
        ))
    })
    .await
    .map_err(|_| AppError::internal("the secret lookup was interrupted"))??;
    let config = SearchConfig::from_settings(&search, tavily, brave);
    let client = match http_client(Some(&settings.network.proxy_url)) {
        Ok(client) => client,
        Err(error) => {
            return Ok(SearchTestResultDto {
                ok: false,
                backend: config.resolved().unwrap_or(config.backend),
                results: 0,
                elapsed_ms: 0,
                error: Some(error.message().to_owned()),
            });
        }
    };
    Ok(run_search_test(&config, &client).await)
}

/// Where a proxy test sends its one request: any HTTP answer means the route works.
const PROXY_TEST_TARGET: &str = "https://api.openai.com/v1/models";

/// How a proxy test ended.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProxyTestOutcome {
    /// The target answered through the route.
    Connected,
    /// The proxy could not be reached.
    ProxyUnreachable,
    /// The proxy asked for credentials (407).
    ProxyAuth,
    /// The proxy refused the tunnel.
    ProxyRefused,
    /// The proxy (or the direct route) worked, the target did not answer in time or at all.
    TargetNoResponse,
}

/// The result of "Test connection" on Settings › Network.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProxyTestDto {
    /// How the test ended.
    pub outcome: ProxyTestOutcome,
    /// The route taken: `system`, `direct` or `custom`.
    pub route: String,
    /// The host the test called.
    pub target: String,
    /// Time until the answer or the failure.
    pub elapsed_ms: u64,
    /// The transport failure's stable name when the target did not answer (`dns`, `tls`...).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// Sends one request through the proxy value `proxy_url` as typed (empty: the system proxy,
/// `direct`: none; the stored masked form stands for the stored URL), with the rest of Settings ›
/// Network (no-proxy hosts, CA file). Nothing is stored.
///
/// # Errors
/// [`AppError::Validation`] when `proxy_url` is not a usable proxy value.
pub async fn network_proxy_test_impl(
    state: &AppState,
    proxy_url: &str,
) -> Result<ProxyTestDto, AppError> {
    use crate::llm::safe_http::client::ReqwestSafeHttp;
    use crate::llm::safe_http::{SafeHttp, SafeMethod, SafeRequest, SafeTransportError as E};
    let mut network = state.settings().network.clone();
    let typed = proxy_url.trim();
    if typed != pingdeck_base::proxy_url::mask(&network.proxy_url) {
        crate::llm::providers::validate_proxy_url(typed)?;
        typed.clone_into(&mut network.proxy_url);
    }
    let http = ReqwestSafeHttp::for_network(&network, PROXY_TEST_TARGET, "")?;
    let route = http.route().name().to_owned();
    let started = Instant::now();
    let request = SafeRequest {
        method: SafeMethod::Get,
        url: PROXY_TEST_TARGET.to_owned(),
        headers: Vec::new(),
        body: None,
        timeout: Duration::from_secs(10),
        connect_timeout: Duration::from_secs(5),
        max_body: 1024,
        allow_private: false,
    };
    let result = http
        .send(request, &pingdeck_base::cancel::CancelToken::new())
        .await;
    let (outcome, detail) = match result {
        Ok(_) => (ProxyTestOutcome::Connected, None),
        Err(E::ProxyUnreachable) => (ProxyTestOutcome::ProxyUnreachable, None),
        Err(E::ProxyAuth) => (ProxyTestOutcome::ProxyAuth, None),
        Err(E::ProxyRefused) => (ProxyTestOutcome::ProxyRefused, None),
        Err(other) => (
            ProxyTestOutcome::TargetNoResponse,
            Some(other.kind().to_owned()),
        ),
    };
    Ok(ProxyTestDto {
        outcome,
        route,
        target: "api.openai.com".to_owned(),
        elapsed_ms: u64::try_from(started.elapsed().as_millis()).unwrap_or(u64::MAX),
        detail,
    })
}

#[cfg(test)]
mod tests {
    use std::io::Write;
    use std::net::TcpListener;

    use serde_json::json;

    use super::*;
    use crate::app::settings_service::{SettingsSecret, settings_secret_set_impl};
    use crate::app::settings_update_impl;
    use crate::settings::SettingsSection;

    const CA_PEM: &str = "-----BEGIN CERTIFICATE-----\n\
        MIIBbDCCARKgAwIBAgIUbULgTdgKYCXtb5wwubnW/Qe8m/QwCgYIKoZIzj0EAwIw\n\
        GzEZMBcGA1UEAwwQUGluZ0RlY2sgVGVzdCBDQTAgFw0yNjExMDcwMzE5NDBaGA8y\n\
        MTI2MTAxNDAzMTk0MFowGzEZMBcGA1UEAwwQUGluZ0RlY2sgVGVzdCBDQTBZMBMG\n\
        ByqGSM49AgEGCCqGSM49AwEHA0IABLh3RvPPORoiGPEZxBstSzPPUuK6EQ/j9SED\n\
        aJfhq1ECR8wMiMOfJRZdk9GyCSOO1ZiskXPrNUjRmJAfIcPZUN6jMjAwMB0GA1Ud\n\
        DgQWBBS4NkpL3tMKMOR7CVQENr9wYVIYlDAPBgNVHRMBAf8EBTADAQH/MAoGCCqG\n\
        SM49BAMCA0gAMEUCIHE5xb5q/IDl2D3H0VEiuGjCT2MdDNHzGS+hpB67pYZHAiEA\n\
        5dUWvwluv2JTKEbWs+grsNRfav5Pv6D2zUlgafX7Ih8=\n\
        -----END CERTIFICATE-----\n";

    fn temp_file(name: &str, contents: &[u8]) -> String {
        let path = std::env::temp_dir().join(format!("pd-ca-{name}-{}.pem", std::process::id()));
        std::fs::write(&path, contents).unwrap();
        path.to_string_lossy().into_owned()
    }

    #[test]
    fn a_bundle_reports_how_many_certificates_it_holds() {
        let one = temp_file("one", CA_PEM.as_bytes());
        let two = temp_file("two", format!("{CA_PEM}\n{CA_PEM}").as_bytes());
        let first = network_ca_test_impl(&one).unwrap();
        assert_eq!(
            first,
            CaBundleInfoDto {
                ok: true,
                certificates: 1,
                error: None
            }
        );
        assert_eq!(network_ca_test_impl(&two).unwrap().certificates, 2);
        for file in [one, two] {
            std::fs::remove_file(file).unwrap();
        }
    }

    #[test]
    fn a_file_without_certificates_or_unreadable_is_reported_not_raised() {
        let text = temp_file("text", b"not a certificate");
        let result = network_ca_test_impl(&text).unwrap();
        assert!(!result.ok && result.certificates == 0);
        assert_eq!(
            result.error.as_deref(),
            Some("No certificates found in this file")
        );
        std::fs::remove_file(text).unwrap();

        let missing = network_ca_test_impl("/no/such/bundle.pem").unwrap();
        assert!(!missing.ok);
        assert!(
            missing
                .error
                .unwrap()
                .starts_with("The file could not be read")
        );
    }

    #[test]
    fn a_malformed_path_is_a_validation_error() {
        for bad in ["", "  ", "relative.pem", "/tmp/a\u{0}b.pem"] {
            assert!(
                matches!(network_ca_test_impl(bad), Err(AppError::Validation { .. })),
                "{bad:?}"
            );
        }
    }

    /// Serves one canned JSON answer on a local port and returns its base URL.
    fn serve_once(body: &'static str) -> String {
        let listener = TcpListener::bind("127.0.0.1:0").unwrap();
        let port = listener.local_addr().unwrap().port();
        std::thread::spawn(move || {
            let Ok((mut stream, _)) = listener.accept() else {
                return;
            };
            let mut request = [0_u8; 4096];
            let _ = std::io::Read::read(&mut stream, &mut request);
            let _ = write!(
                stream,
                "HTTP/1.1 200 OK\r\nContent-Type: application/json\r\nContent-Length: {}\r\nConnection: close\r\n\r\n{body}",
                body.len()
            );
        });
        format!("http://127.0.0.1:{port}")
    }

    #[tokio::test]
    async fn a_search_test_counts_the_results_of_the_backend() {
        let state = AppState::open_in_memory().unwrap();
        let url = serve_once(
            r#"{"results":[{"title":"One","url":"https://a.example/1","content":"x"},{"title":"Two","url":"https://a.example/2","content":"y"}]}"#,
        );
        settings_update_impl(
            &state,
            SettingsSection::Ai,
            &json!({ "search": { "searxng_base_url": url } }),
        )
        .unwrap();
        let result = ai_search_test_impl(&state, SearchBackend::Searxng)
            .await
            .unwrap();
        assert!(result.ok, "{result:?}");
        assert_eq!(
            (result.backend, result.results),
            (SearchBackend::Searxng, 2)
        );
        assert!(result.error.is_none());
    }

    #[tokio::test]
    async fn a_missing_key_is_reported_in_the_result_and_never_echoed() {
        let state = AppState::open_in_memory().unwrap();
        let missing = ai_search_test_impl(&state, SearchBackend::Tavily)
            .await
            .unwrap();
        assert!(!missing.ok && missing.results == 0);
        assert!(missing.error.unwrap().contains("Tavily API key"));

        // With a key but no reachable endpoint the failure text carries neither key nor URL.
        settings_secret_set_impl(&state, SettingsSecret::SearchBrave, "brv-secret-key-123456")
            .unwrap();
        settings_update_impl(
            &state,
            SettingsSection::Ai,
            &json!({ "search": { "searxng_base_url": "http://127.0.0.1:9" } }),
        )
        .unwrap();
        let refused = ai_search_test_impl(&state, SearchBackend::Searxng)
            .await
            .unwrap();
        assert!(!refused.ok);
        let text = serde_json::to_string(&refused).unwrap();
        assert!(
            !text.contains("brv-secret") && !text.contains("127.0.0.1"),
            "{text}"
        );
    }
}
