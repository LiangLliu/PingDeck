//! Production [`SafeHttp`]: reqwest with no redirects, validated targets and hard caps; the only
//! place the sniffer, the balance adapters and custom-target AI requests build an HTTP client.
//!
//! Every call builds a fresh client after admitting its target (see `admission`):
//! - Direct route: the host is resolved here, within the request's connect timeout, and every
//!   address is validated by [`spanbay_base::net_guard`]; a name (`localhost` included) is
//!   pinned to the validated addresses (`resolve_to_addrs`), so a second DNS answer cannot move
//!   the connection (rebinding).
//! - Explicit proxy: the proxy resolves the destination, so only a literal target is classified.
//! - System route: a literal target is classified; a target name is validated on every lookup by
//!   `GuardedResolver`, and the system proxy's own host resolves plainly.

use std::sync::PoisonError;
use std::time::{Duration, Instant};

use futures_util::StreamExt;
use futures_util::future::BoxFuture;

use super::stream::{SafeStream, StreamLimits};
use super::{SafeHttp, SafeRequest, SafeResponse, SafeTransportError};
use admission::{Prepared, RefusalSlot, prepare};
use spanbay_base::{
    cancel::{CancelToken, until_cancelled},
    proxy_url::ProxyRoute,
};

mod admission;

/// The reqwest-backed transport; one value per proxy choice and CA bundle.
#[derive(Clone)]
pub struct ReqwestSafeHttp {
    route: ProxyRoute,
    /// Extra trusted roots (`network.ca_bundle_path`).
    ca_roots: Vec<reqwest::Certificate>,
}

/// An explicit proxy URL may carry a password: `Debug` prints it masked, and only the number of
/// extra roots.
impl std::fmt::Debug for ReqwestSafeHttp {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let route = match &self.route {
            ProxyRoute::Proxy(url) => ProxyRoute::Proxy(spanbay_base::proxy_url::mask(url)),
            other => other.clone(),
        };
        f.debug_struct("ReqwestSafeHttp")
            .field("route", &route)
            .field("ca_roots", &self.ca_roots.len())
            .finish()
    }
}

impl Default for ReqwestSafeHttp {
    fn default() -> Self {
        Self::direct()
    }
}

impl ReqwestSafeHttp {
    /// A direct (no proxy) transport with every address validated and pinned. System proxy
    /// settings are ignored.
    #[must_use]
    pub const fn direct() -> Self {
        Self {
            route: ProxyRoute::Direct,
            ca_roots: Vec::new(),
        }
    }

    /// A transport that goes through `proxy_url` (a validated `http(s)`/`socks5(h)` URL). The
    /// proxy resolves the destination, so only literal targets can be classified.
    #[must_use]
    pub(crate) fn through_proxy(proxy_url: &str) -> Self {
        Self {
            route: ProxyRoute::Proxy(proxy_url.to_owned()),
            ca_roots: Vec::new(),
        }
    }

    /// A transport that uses the system proxy settings, so management requests reach the network
    /// for a user who only turned on Clash or Surge. Addresses are not pinned, as behind an
    /// explicit proxy.
    #[must_use]
    pub const fn system() -> Self {
        Self {
            route: ProxyRoute::System,
            ca_roots: Vec::new(),
        }
    }

    /// Trusts `roots` besides the system roots (see [`crate::llm::ca_bundle::load_ca_roots`]).
    #[must_use]
    pub(crate) fn with_ca_roots(mut self, roots: Vec<reqwest::Certificate>) -> Self {
        self.ca_roots = roots;
        self
    }

    /// Whether requests may leave through a proxy (the sniffer reports `proxy_dns` then).
    #[must_use]
    pub const fn uses_proxy(&self) -> bool {
        !matches!(self.route, ProxyRoute::Direct)
    }

    /// The route this transport takes (for failure cards: which way the request went).
    #[must_use]
    pub const fn route(&self) -> &ProxyRoute {
        &self.route
    }

    /// Builds the safe transport for requests to `origin` by the one proxy rule
    /// ([`spanbay_base::proxy_url::route`]); trusts `network.ca_bundle_path`.
    ///
    /// # Errors
    /// [`spanbay_base::error::AppError::Validation`] when the chosen proxy URL or CA bundle is
    /// unusable.
    pub fn for_network(
        network: &spanbay_model::settings::NetworkSettings,
        origin: &str,
        provider_proxy: &str,
    ) -> Result<Self, spanbay_base::error::AppError> {
        let host = url::Url::parse(origin)
            .ok()
            .and_then(|url| url.host_str().map(str::to_owned))
            .unwrap_or_default();
        let route = spanbay_base::proxy_url::route(
            provider_proxy,
            &network.proxy_url,
            &network.no_proxy,
            &host,
        );
        let transport = match route {
            ProxyRoute::Direct => Self::direct(),
            ProxyRoute::System => Self::system(),
            ProxyRoute::Proxy(url) => {
                crate::llm::providers::validate_proxy_url(&url)?;
                Self::through_proxy(&url)
            }
        };
        Ok(
            transport.with_ca_roots(crate::llm::ca_bundle::load_ca_roots(
                &network.ca_bundle_path,
            )?),
        )
    }

    /// Sends `request` and returns as soon as the response head arrives: the body is read from
    /// the returned stream under `limits` (`request.timeout` and `request.max_body` do not
    /// apply; the reader caps what it keeps). Every call uses a fresh connection (no pooling),
    /// so `dns` and `connect` (TLS inside) are measured and reuse never happens.
    ///
    /// # Errors
    /// A [`SafeTransportError`] when the target is refused, the head does not arrive within
    /// `limits.first_byte`, the connection fails, or `cancel` fires.
    pub async fn open_stream(
        &self,
        mut request: SafeRequest,
        limits: StreamLimits,
        cancel: &CancelToken,
    ) -> Result<SafeStream, SafeTransportError> {
        let started = Instant::now();
        let opening = async {
            let Prepared {
                call,
                refusal,
                dns,
                watch,
            } = prepare(self, &mut request, false).await?;
            let proxied = self.dials_proxy();
            let response = tokio::time::timeout(limits.first_byte, call.send())
                .await
                .map_err(|_| SafeTransportError::Timeout)?
                .map_err(|error| send_error(&error, proxied, &refusal))?;
            let connect = watch.connect_only();
            Ok(stream_of(response, dns, limits, started, cancel.clone()).with_connect(connect))
        };
        until_cancelled(Some(cancel), opening)
            .await
            .unwrap_or(Err(SafeTransportError::Cancelled))
    }

    /// Whether requests dial an explicit proxy (proxy failures are told apart then).
    const fn dials_proxy(&self) -> bool {
        matches!(self.route, ProxyRoute::Proxy(_))
    }
}

/// Classifies a reqwest failure: the timeout flag, else the shared reading of the error
/// chain ([`spanbay_base::error::transport_cause`]); `proxied` says a proxy was dialled.
pub fn map_error(error: &reqwest::Error, proxied: bool) -> SafeTransportError {
    use spanbay_base::error::NetworkCause as C;
    if error.is_timeout() {
        return SafeTransportError::Timeout;
    }
    match spanbay_base::error::transport_cause(error, proxied) {
        Some(C::Timeout) => SafeTransportError::Timeout,
        Some(C::Dns) => SafeTransportError::Dns,
        Some(C::Refused) => SafeTransportError::Refused,
        Some(C::Tls) => SafeTransportError::Tls,
        Some(C::Reset) => SafeTransportError::Reset,
        Some(C::ProxyAuth) => SafeTransportError::ProxyAuth,
        Some(C::ProxyRefused) => SafeTransportError::ProxyRefused,
        Some(C::ProxyUnreachable) => SafeTransportError::ProxyUnreachable,
        None if error.is_connect() => SafeTransportError::Refused,
        None => SafeTransportError::Other("request failed".to_owned()),
    }
}

/// Maps a send failure, preferring the refusal the guarded resolver recorded.
fn send_error(error: &reqwest::Error, proxied: bool, refusal: &RefusalSlot) -> SafeTransportError {
    refusal
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .unwrap_or_else(|| map_error(error, proxied))
}

async fn exchange(
    transport: &ReqwestSafeHttp,
    mut request: SafeRequest,
) -> Result<SafeResponse, SafeTransportError> {
    let Prepared { call, refusal, .. } = prepare(transport, &mut request, true).await?;
    let started = Instant::now();
    let proxied = transport.dials_proxy();
    let mut response = call
        .send()
        .await
        .map_err(|error| send_error(&error, proxied, &refusal))?;
    let status = response.status().as_u16();
    let headers = lowercase_headers(response.headers());
    // Content-Length is untrusted, but the reservation never exceeds `max_body`.
    let mut body = Vec::with_capacity(response.content_length().map_or(0, |length| {
        usize::try_from(length)
            .unwrap_or(usize::MAX)
            .min(request.max_body)
    }));
    let mut truncated = false;
    while let Some(chunk) = response
        .chunk()
        .await
        .map_err(|error| map_error(&error, false))?
    {
        let room = request.max_body.saturating_sub(body.len());
        if chunk.len() > room {
            body.extend_from_slice(&chunk[..room]);
            truncated = true;
            break;
        }
        body.extend_from_slice(&chunk);
    }
    Ok(SafeResponse {
        status,
        headers,
        body,
        truncated,
        elapsed: started.elapsed(),
    })
}

impl SafeHttp for ReqwestSafeHttp {
    fn send<'a>(
        &'a self,
        request: SafeRequest,
        cancel: &'a CancelToken,
    ) -> BoxFuture<'a, Result<SafeResponse, SafeTransportError>> {
        Box::pin(async move {
            until_cancelled(Some(cancel), exchange(self, request))
                .await
                .unwrap_or(Err(SafeTransportError::Cancelled))
        })
    }
}

/// The headers whose values are valid text, names lower-cased, in arrival order.
fn lowercase_headers(headers: &reqwest::header::HeaderMap) -> Vec<(String, String)> {
    headers
        .iter()
        .filter_map(|(name, value)| {
            value
                .to_str()
                .ok()
                .map(|text| (name.as_str().to_ascii_lowercase(), text.to_owned()))
        })
        .collect()
}

/// Wraps a reqwest response as a [`SafeStream`]; `started` is when the call began.
pub fn stream_of(
    response: reqwest::Response,
    dns: Option<Duration>,
    limits: StreamLimits,
    started: Instant,
    cancel: CancelToken,
) -> SafeStream {
    let status = response.status().as_u16();
    let headers = lowercase_headers(response.headers());
    let chunks = response
        .bytes_stream()
        .map(|chunk| chunk.map_err(|error| map_error(&error, false)))
        .boxed();
    SafeStream::new(status, headers, dns, chunks, limits, started, cancel)
}

#[cfg(test)]
mod tests;
