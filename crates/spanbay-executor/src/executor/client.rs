//! Building, caching and configuring the `reqwest` clients: proxy, TLS versions, CA and client certificates.

use crate::executor::probe::{ConnectProbe, ConnectTimer, ProbedResolver};
use crate::executor::tls::{client_identity, trust_roots};
use crate::executor::types::{AuxiliaryNetwork, ExecutionError, ResolveGuard};
use reqwest::Version as HttpVersion;
use reqwest::redirect::Policy;
use spanbay_base::proxy_url::{self, ProxyRoute};
use spanbay_model::Request;
use spanbay_model::models::{
    HttpVersionPreference, NetworkField, TlsCertificate, TlsCertificateKind, TlsVersionPreference,
};
use std::collections::VecDeque;
use std::ops::Deref;
use std::sync::{Arc, LazyLock, Mutex};
use std::time::{Duration, UNIX_EPOCH};
use url::Url;

/// Builds the blocking client used by `pm.sendRequest`: the configured proxy or none (never the
/// system proxy), up to 10 redirects. Link-local, metadata, multicast and reserved addresses are
/// always refused (name lookups through the resolver, IP-literal hosts through
/// [`ensure_script_target_allowed`], also on every redirect hop); loopback and private ones stay
/// reachable because a script is user-written Postman-compatible code. It trusts the roots every
/// client trusts and presents `client_certificate` (the one matching the target host), so a
/// script reaches an mTLS or private-CA service the request itself reaches. Blocking: reads the
/// certificate files.
///
/// # Errors
/// A message naming the file when a certificate or key cannot be read, or the client cannot be
/// built.
pub fn blocking_http_client(
    network: &AuxiliaryNetwork,
    client_certificate: Option<&TlsCertificate>,
) -> Result<reqwest::blocking::Client, String> {
    let resolver = ProbedResolver::guarded(
        Arc::new(ConnectProbe::default()),
        ResolveGuard::new(None),
        0,
    );
    let mut builder = reqwest::blocking::Client::builder();
    for root in trust_roots(&network.ca_bundle_path, &network.tls.certificates)? {
        builder = builder.add_root_certificate(root);
    }
    if let Some(certificate) = client_certificate {
        builder = builder.identity(client_identity(certificate, &network.tls.passphrases)?);
    }
    let builder = builder
        .timeout(network.timeout)
        .danger_accept_invalid_certs(!network.verify_tls)
        .dns_resolver(Arc::new(resolver))
        .redirect(Policy::custom(|attempt| {
            if attempt.previous().len() >= MAX_SCRIPT_REDIRECTS {
                return attempt.error("too many redirects");
            }
            match ensure_script_target_allowed(attempt.url()) {
                Ok(()) => attempt.follow(),
                Err(message) => attempt.error(message),
            }
        }));
    match network.explicit_proxy() {
        Some(proxy_url) => {
            builder.proxy(reqwest::Proxy::all(proxy_url).map_err(|error| error.to_string())?)
        }
        None => builder.no_proxy(),
    }
    .build()
    .map_err(|error| error.to_string())
}

/// Redirects one `pm.sendRequest` call may follow.
const MAX_SCRIPT_REDIRECTS: usize = 10;

/// Refuses a `pm.sendRequest` target that is an IP literal (or `localhost`) of a class the
/// always-refuse guard blocks; names are checked by the client's resolver.
pub fn ensure_script_target_allowed(url: &Url) -> Result<(), String> {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    ResolveGuard::new(None)
        .check_literal(&host, url.port_or_known_default().unwrap_or(0))
        .map_err(|refusal| refusal.to_string())
}

/// A `reqwest` client with the probe that times its connections; dereferences to the client.
#[derive(Clone, Debug)]
pub(super) struct ProbedClient {
    client: reqwest::Client,
    /// Timing of this client's DNS lookups and connects (see [`super::probe`]).
    pub(super) probe: Arc<ConnectProbe>,
}

impl Deref for ProbedClient {
    type Target = reqwest::Client;

    fn deref(&self) -> &reqwest::Client {
        &self.client
    }
}

/// The route of `request` to `host`: a request that follows Settings for its proxy
/// takes the one rule of every outbound client (local and `no_proxy` hosts direct, else the
/// global proxy, else the system proxy); a request with its own proxy uses it, and its own empty
/// value stays "no proxy".
pub(crate) fn proxy_route(request: &Request, network: &AuxiliaryNetwork, host: &str) -> ProxyRoute {
    let options = &request.network_options;
    let own = if options.inherit.contains(&NetworkField::ProxyUrl) {
        ""
    } else {
        options
            .proxy_url
            .as_deref()
            .map(str::trim)
            .filter(|url| !url.is_empty())
            .unwrap_or(proxy_url::DIRECT)
    };
    proxy_url::route(own, &network.proxy_url, &network.no_proxy, host)
}

/// Upper bound on warm `reqwest::Client`s. Each client owns a connection pool,
/// so keeping a handful alive makes collection and flow runs reuse TLS
/// handshakes instead of rebuilding them per request.
const MAX_CACHED_CLIENTS: usize = 16;

/// Clients in least-recently-used order: the front entry is evicted first.
static CLIENT_CACHE: LazyLock<Mutex<VecDeque<(String, ProbedClient)>>> =
    LazyLock::new(|| Mutex::new(VecDeque::with_capacity(MAX_CACHED_CLIENTS)));

/// Maps the HTTP version preference to a pinned `reqwest` version, if any.
pub(super) fn http_version(preference: HttpVersionPreference) -> Option<HttpVersion> {
    match preference {
        HttpVersionPreference::Auto => None,
        HttpVersionPreference::Http10 => Some(HttpVersion::HTTP_10),
        HttpVersionPreference::Http11 => Some(HttpVersion::HTTP_11),
        HttpVersionPreference::Http2 => Some(HttpVersion::HTTP_2),
    }
}

/// Looks `cache_key` up in the client cache and marks a hit as most recently used.
fn cached_client(cache_key: &str) -> Option<ProbedClient> {
    let mut clients = CLIENT_CACHE.lock().ok()?;
    let index = clients.iter().position(|(key, _)| key == cache_key)?;
    let (_, client) = clients.remove(index)?;
    // Move the hit to the back so eviction follows real usage order.
    clients.push_back((cache_key.to_owned(), client.clone()));
    Some(client)
}

/// Stores `client` under `cache_key` as the most recently used entry. A concurrent build of the
/// same key (both missed the cache) replaces the earlier entry, so a key never appears twice and
/// evicts the warm clients of other hosts.
pub(super) fn insert_client(cache_key: String, client: &ProbedClient) {
    let Ok(mut clients) = CLIENT_CACHE.lock() else {
        return;
    };
    if let Some(index) = clients.iter().position(|(key, _)| *key == cache_key) {
        clients.remove(index);
    } else if clients.len() >= MAX_CACHED_CLIENTS {
        // Evict the least recently used client only; clearing the cache
        // dropped every warm connection pool at once.
        clients.pop_front();
    }
    clients.push_back((cache_key, client.clone()));
}

/// How many cache entries carry `cache_key`.
#[cfg(test)]
pub(super) fn cached_entries(cache_key: &str) -> usize {
    CLIENT_CACHE.lock().map_or(0, |clients| {
        clients.iter().filter(|(key, _)| key == cache_key).count()
    })
}

/// [`build_client`] for async callers: a cache hit is answered in place, and a miss (which reads
/// certificate and key files) is built on the blocking pool so file I/O never stalls a runtime
/// worker.
pub(super) async fn build_client_async(
    request: &Request,
    certificates: &[TlsCertificate],
    url: &Url,
    guard: Option<&ResolveGuard>,
    network: &AuxiliaryNetwork,
) -> Result<ProbedClient, ExecutionError> {
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let port = url.port_or_known_default().unwrap_or(0);
    if let Some(guard) = guard {
        // An IP literal never reaches the resolver: classify it before any connection.
        guard
            .check_literal(&host, port)
            .map_err(|refusal| ExecutionError::GuardRefused { host: refusal.host })?;
    }
    let cache_key = client_cache_key(request, certificates, &host, port, guard, network);
    if let Some(client) = cached_client(&cache_key) {
        return Ok(client);
    }
    let (request, certificates, url, guard, network) = (
        request.clone(),
        certificates.to_vec(),
        url.clone(),
        guard.cloned(),
        network.clone(),
    );
    tokio::task::spawn_blocking(move || {
        build_client(&request, &certificates, &url, guard.as_ref(), &network)
    })
    .await
    .unwrap_or_else(|error| {
        Err(ExecutionError::ClientBuild(format!(
            "client task failed: {error}"
        )))
    })
}

/// Builds (or reuses from the cache) the `reqwest::Client` for `request`: proxy, TLS and
/// certificates. Blocking when it has to read certificate files; async code uses
/// [`build_client_async`].
pub(super) fn build_client(
    request: &Request,
    certificates: &[TlsCertificate],
    url: &Url,
    guard: Option<&ResolveGuard>,
    network: &AuxiliaryNetwork,
) -> Result<ProbedClient, ExecutionError> {
    let options = &request.network_options;
    let timeout_ms = options.timeout_ms;
    let host = url.host_str().unwrap_or_default().to_ascii_lowercase();
    let port = url.port_or_known_default().unwrap_or(0);
    let client_certificate = matching_client_certificate(certificates, &host, port);
    let cache_key = client_cache_key(request, certificates, &host, port, guard, network);

    if let Some(client) = cached_client(&cache_key) {
        return Ok(client);
    }

    let probe = Arc::new(ConnectProbe::default());
    // `timeout_ms` is an idle timeout (docs/architecture.md §6): it bounds the connect and every
    // read, the wait for the headers included, and restarts with each chunk, so a stream that
    // keeps sending is never cut by it. The total limit is the send loop's deadline.
    let idle = Duration::from_millis(timeout_ms);
    let mut builder = reqwest::Client::builder()
        .connect_timeout(idle)
        .read_timeout(idle)
        .redirect(Policy::none())
        .danger_accept_invalid_certs(!options.verify_tls)
        .dns_resolver(Arc::new(match guard {
            Some(guard) => ProbedResolver::guarded(Arc::clone(&probe), guard.clone(), port),
            None => ProbedResolver::new(Arc::clone(&probe)),
        }))
        .connector_layer(ConnectTimer::new(Arc::clone(&probe)));
    builder = match options.http_version {
        HttpVersionPreference::Auto => builder,
        HttpVersionPreference::Http10 | HttpVersionPreference::Http11 => builder.http1_only(),
        HttpVersionPreference::Http2 => builder.http2_prior_knowledge(),
    };

    let allows_tls12 = !options
        .disabled_tls_versions
        .contains(&TlsVersionPreference::Tls12);
    let allows_tls13 = !options
        .disabled_tls_versions
        .contains(&TlsVersionPreference::Tls13);
    match (allows_tls12, allows_tls13) {
        (false, false) => {
            return Err(ExecutionError::ClientBuild(
                "all TLS versions are disabled".to_owned(),
            ));
        }
        (false, true) => {
            builder = builder.min_tls_version(reqwest::tls::Version::TLS_1_3);
        }
        (true, false) => {
            builder = builder.max_tls_version(reqwest::tls::Version::TLS_1_2);
        }
        (true, true) => {}
    }

    // Settings › Network's CA file applies to every outbound client, besides the
    // workspace's own CA certificates.
    for root in
        trust_roots(&network.ca_bundle_path, certificates).map_err(ExecutionError::ClientBuild)?
    {
        builder = builder.add_root_certificate(root);
    }
    if let Some(certificate) = client_certificate {
        builder = builder.identity(
            client_identity(certificate, &network.tls.passphrases)
                .map_err(ExecutionError::ClientBuild)?,
        );
    }

    builder = match proxy_route(request, network, &host) {
        ProxyRoute::Proxy(proxy_url) => {
            builder.proxy(reqwest::Proxy::all(&proxy_url).map_err(|error| {
                ExecutionError::ClientBuild(format!("invalid proxy URL: {error}"))
            })?)
        }
        // reqwest's default: the system proxy settings.
        ProxyRoute::System => builder,
        // A local or `no_proxy` host, or "no proxy": never the system proxy or `*_PROXY`, which
        // would intercept loopback and break every request to a running mock server.
        ProxyRoute::Direct => builder.no_proxy(),
    };

    let client = ProbedClient {
        client: builder
            .build()
            .map_err(|error| ExecutionError::ClientBuild(error.to_string()))?,
        probe,
    };

    insert_client(cache_key, &client);

    Ok(client)
}

/// Cache key for the shared `reqwest::Client`. Certificate records contribute
/// their id, kind, PEM paths and each PEM file's size and mtime, so two requests can never
/// share a client built from a different (or since-rewritten) client certificate or trust store.
pub(super) fn client_cache_key(
    request: &Request,
    certificates: &[TlsCertificate],
    host: &str,
    port: u16,
    guard: Option<&ResolveGuard>,
    network: &AuxiliaryNetwork,
) -> String {
    let options = &request.network_options;
    let tls_key = options
        .disabled_tls_versions
        .iter()
        .map(disabled_tls_version_key)
        .collect::<Vec<_>>()
        .join(",");
    let certificate_key = certificates
        .iter()
        .filter(|certificate| certificate.enabled)
        .map(|certificate| {
            // The host pattern decides whether a client certificate is presented at all.
            format!(
                "{}:{}:{}:{}:{}:{}:{}:{}",
                certificate.id.as_str(),
                // A passphrase stored or removed rebuilds the identity.
                certificate.passphrase_ref.is_some(),
                match certificate.kind {
                    TlsCertificateKind::Ca => "ca",
                    TlsCertificateKind::Client => "client",
                },
                certificate.host_pattern,
                certificate.cert_path,
                file_stamp(&certificate.cert_path),
                certificate.key_path.as_deref().unwrap_or_default(),
                certificate
                    .key_path
                    .as_deref()
                    .map(file_stamp)
                    .unwrap_or_default(),
            )
        })
        .collect::<Vec<_>>()
        .join(";");

    let route = match proxy_route(request, network, host) {
        ProxyRoute::Proxy(url) => url,
        other => other.name().to_owned(),
    };
    let ca_bundle = if network.ca_bundle_path.is_empty() {
        String::new()
    } else {
        format!(
            "{}:{}",
            network.ca_bundle_path,
            file_stamp(&network.ca_bundle_path)
        )
    };
    format!(
        "{}|{}|{route}|{}|{tls_key}|{host}|{port}|{certificate_key}|{}|{ca_bundle}",
        options.timeout_ms,
        options.verify_tls,
        http_version_key(options.http_version),
        guard.map(ResolveGuard::cache_key).unwrap_or_default(),
    )
}

/// `size-mtime_nanos` of a PEM file, or `?` when it cannot be inspected.
fn file_stamp(path: &str) -> String {
    let stamp = || {
        let metadata = std::fs::metadata(path).ok()?;
        let modified = metadata.modified().ok()?.duration_since(UNIX_EPOCH).ok()?;
        Some(format!("{}-{}", metadata.len(), modified.as_nanos()))
    };
    stamp().unwrap_or_else(|| "?".to_owned())
}

fn http_version_key(preference: HttpVersionPreference) -> &'static str {
    match preference {
        HttpVersionPreference::Auto => "auto",
        HttpVersionPreference::Http10 => "http1.0",
        HttpVersionPreference::Http11 => "http1.1",
        HttpVersionPreference::Http2 => "http2",
    }
}

/// Stable cache-key fragment for a disabled TLS version.
pub(super) fn disabled_tls_version_key(version: &TlsVersionPreference) -> &'static str {
    match version {
        TlsVersionPreference::Tls12 => "1.2",
        TlsVersionPreference::Tls13 => "1.3",
    }
}

/// Picks the most specific enabled client certificate for `host:port`.
///
/// Specificity ranks an explicit port above any port, an exact host above a
/// `*.` wildcard, then the longest pattern.
pub fn matching_client_certificate<'a>(
    certificates: &'a [TlsCertificate],
    host: &str,
    port: u16,
) -> Option<&'a TlsCertificate> {
    certificates
        .iter()
        .filter(|certificate| certificate.enabled && certificate.kind == TlsCertificateKind::Client)
        .filter_map(|certificate| {
            certificate_host_score(&certificate.host_pattern, host, port)
                .map(|score| (score, certificate))
        })
        .max_by_key(|(score, _)| *score)
        .map(|(_, certificate)| certificate)
}

/// Specificity score of a certificate host pattern for `host:port`, or `None` if it does not match.
pub(super) fn certificate_host_score(
    pattern: &str,
    host: &str,
    port: u16,
) -> Option<(u8, u8, usize)> {
    let pattern = pattern.trim().to_ascii_lowercase();
    if pattern.is_empty() || pattern == "*" {
        return Some((0, 0, 0));
    }
    let (pattern_host, pattern_port) = match pattern.rsplit_once(':') {
        Some((pattern_host, pattern_port)) if !pattern_host.contains(':') => {
            match pattern_port.parse::<u16>() {
                Ok(pattern_port) => (pattern_host.to_owned(), Some(pattern_port)),
                Err(_) => (pattern.clone(), None),
            }
        }
        _ => (pattern.clone(), None),
    };
    if let Some(pattern_port) = pattern_port
        && pattern_port != port
    {
        return None;
    }

    let exact = !pattern_host.starts_with("*.");
    let host_matches = match pattern_host.strip_prefix("*.") {
        Some(suffix) => host == suffix || host.ends_with(&format!(".{suffix}")),
        None => host == pattern_host,
    };
    host_matches.then_some((
        u8::from(pattern_port.is_some()),
        u8::from(exact),
        pattern_host.len(),
    ))
}
