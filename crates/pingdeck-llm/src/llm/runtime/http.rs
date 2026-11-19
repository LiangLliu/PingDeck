//! The shared upstream HTTP clients of [`LlmRuntime`]: proxy routing and the CA bundle.
//!
//! Single responsibility: pick (and lazily build) the `reqwest` client for one upstream URL. The
//! routing rule is the one of every outbound client (`proxy_url::route`): a provider's
//! own proxy first (`direct`: none); otherwise a local host or a host on `network.no_proxy` goes
//! direct; every other host uses the global `network.proxy_url` (`direct`: none), else the
//! system proxy. Every
//! client trusts `network.ca_bundle_path` besides the system roots and resolves names through the
//! [`TimedResolver`], which refuses link-local and metadata answers.

use std::collections::HashMap;
use std::sync::{Arc, PoisonError};
use std::time::Duration;

use super::LlmRuntime;
use crate::llm::ca_bundle::load_ca_roots;
use crate::llm::store;
use crate::llm::upstream::{ConnLedger, ConnectTimer, TimedResolver};
use pingdeck_base::error::AppError;
use pingdeck_base::net_guard::{self, HostClass};
use pingdeck_base::proxy_url::{ProxyRoute, route};

/// Builder shared by every upstream client. Redirects are never followed: reqwest would replay
/// the credential headers (`x-api-key`, extra headers) to whatever host the upstream names, so a
/// 3xx surfaces as a plain status error instead. DNS and connect go through the timing resolver
/// and connector layer. A non-empty `ca_bundle` is a PEM file added to the trusted
/// roots.
///
/// # Errors
/// [`AppError::Validation`] when the bundle cannot be read or holds no certificate.
fn http_builder(
    ledger: &Arc<ConnLedger>,
    ca_bundle: &str,
) -> Result<reqwest::ClientBuilder, AppError> {
    let mut builder = reqwest::Client::builder()
        .connect_timeout(Duration::from_secs(15))
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(Arc::new(TimedResolver::new(Arc::clone(ledger))))
        .connector_layer(ConnectTimer::new(Arc::clone(ledger)));
    for root in load_ca_roots(ca_bundle)? {
        builder = builder.add_root_certificate(root);
    }
    Ok(builder)
}

/// The clients built for one CA bundle path; a changed path replaces the whole set.
pub(super) struct SharedClients {
    ca_bundle: String,
    system: reqwest::Client,
    direct: reqwest::Client,
    /// Clients per explicit proxy URL.
    proxied: HashMap<String, reqwest::Client>,
}

impl SharedClients {
    /// The system-proxy and direct clients for `ca_bundle` (blank: system roots only).
    pub(super) fn build(ledger: &Arc<ConnLedger>, ca_bundle: &str) -> Result<Self, AppError> {
        let build = |builder: reqwest::ClientBuilder| {
            builder.build().map_err(|error| {
                AppError::internal(format!("cannot build the HTTP client: {error}"))
            })
        };
        Ok(Self {
            ca_bundle: ca_bundle.to_owned(),
            system: build(http_builder(ledger, ca_bundle)?)?,
            direct: build(http_builder(ledger, ca_bundle)?.no_proxy())?,
            proxied: HashMap::new(),
        })
    }
}

impl LlmRuntime {
    /// Whether a request to `url` dials an explicit proxy (a refused or unresolvable peer
    /// is then the proxy).
    #[must_use]
    pub fn proxied_for(&self, url: &str, provider_proxy: &str) -> bool {
        let host = url::Url::parse(url)
            .ok()
            .and_then(|parsed| parsed.host_str().map(str::to_owned))
            .unwrap_or_default();
        let network = &self.settings().network;
        matches!(
            route(provider_proxy, &network.proxy_url, &network.no_proxy, &host),
            ProxyRoute::Proxy(_)
        )
    }

    /// The HTTP client for `url`.
    ///
    /// Proxy choice: [`route`] (the provider's own `provider_proxy`, `direct` for none; direct for
    /// a local host or one on `network.no_proxy`; the global `network.proxy_url`; the system
    /// proxy). The clients trust
    /// `network.ca_bundle_path`; a change of that setting rebuilds them on the next call (the
    /// file is read again only when the path changes). A link-local or metadata IP literal is
    /// refused here; a name resolving there is refused by the client's resolver (behind a proxy
    /// the proxy resolves the destination, so only literals can be checked).
    ///
    /// # Errors
    /// [`AppError::Validation`] when the URL's host is a link-local or metadata literal, the
    /// chosen proxy URL is not usable, or the CA bundle cannot be loaded.
    pub fn http_for(&self, url: &str, provider_proxy: &str) -> Result<reqwest::Client, AppError> {
        let parsed = url::Url::parse(url).ok();
        let host = parsed
            .as_ref()
            .and_then(url::Url::host_str)
            .unwrap_or_default();
        if net_guard::classify_host_literal(host) == Some(HostClass::LinkLocalOrMetadata) {
            return Err(AppError::validation(
                "refusing to connect to a link-local, cloud metadata or reserved address",
            ));
        }
        let settings = self.settings();
        let ca_bundle = settings.network.ca_bundle_path.trim();
        let mut clients = self
            .http_clients
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if clients.ca_bundle != ca_bundle {
            *clients = SharedClients::build(&self.conn_ledger, ca_bundle)?;
        }
        let network = &settings.network;
        let proxy = match route(provider_proxy, &network.proxy_url, &network.no_proxy, host) {
            ProxyRoute::Direct => return Ok(clients.direct.clone()),
            ProxyRoute::System => return Ok(clients.system.clone()),
            ProxyRoute::Proxy(proxy) => proxy,
        };
        if let Some(client) = clients.proxied.get(&proxy) {
            return Ok(client.clone());
        }
        store::validate_proxy_url(&proxy)?;
        let client = http_builder(&self.conn_ledger, ca_bundle)?
            .proxy(reqwest::Proxy::all(&proxy).map_err(|_| {
                AppError::validation(format!(
                    "invalid proxy URL {}",
                    store::redact_proxy_url(&proxy)
                ))
            })?)
            .build()
            .map_err(|error| {
                AppError::validation(format!(
                    "cannot use proxy {}: {}",
                    store::redact_proxy_url(&proxy),
                    error.without_url()
                ))
            })?;
        clients.proxied.insert(proxy, client.clone());
        Ok(client)
    }
}

#[cfg(test)]
mod tests;
