//! Target admission of the production transport: the URL checks, the address policy of each
//! route, and the fresh client and call built for one request.
//!
//! Errors keep a fixed order: URL checks, then route admission (with name resolution on the
//! direct route), then building the client, then the request headers. A refused target is
//! reported as `Blocked` even when its headers are invalid too.

use std::net::SocketAddr;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use spanbay_base::error::AppError;
use spanbay_base::net_guard::{self, HostClass};
use spanbay_base::proxy_url::ProxyRoute;

use super::ReqwestSafeHttp;
use crate::llm::safe_http::{SafeMethod, SafeRequest, SafeTransportError};
use crate::llm::timing::{AttemptWatch, ConnLedger, ConnectTimer};

/// A refusal with the short machine-readable `reason`.
fn blocked(reason: &str) -> SafeTransportError {
    SafeTransportError::Blocked {
        reason: reason.to_owned(),
    }
}

/// The `Blocked` reason of a refused address of class `class`.
const fn refusal_reason(class: HostClass) -> &'static str {
    match class {
        HostClass::LinkLocalOrMetadata => "link_local",
        HostClass::Loopback | HostClass::PrivateRfc1918 | HostClass::Cgnat => "private",
        HostClass::Public => "refused_address",
    }
}

/// Maps a refusal of [`net_guard::resolve_and_validate`] to a transport error; `class` is the
/// spelling-based class of the host, when it has one.
fn guard_error(error: &AppError, class: Option<HostClass>) -> SafeTransportError {
    match error {
        AppError::Validation { .. } => blocked(class.map_or("refused_address", refusal_reason)),
        _ => SafeTransportError::Dns,
    }
}

/// The refusal a [`GuardedResolver`] recorded, read back when the send fails.
pub(super) type RefusalSlot = Arc<Mutex<Option<SafeTransportError>>>;

/// DNS resolver of the system-route client. Without a system proxy reqwest resolves the target
/// itself; this resolver then applies [`net_guard::resolve_and_validate`] to every lookup of the
/// target name (so a second answer cannot move the connection either) and records why it refused.
/// Other names are the system proxy's host and resolve plainly.
pub(super) struct GuardedResolver {
    pub(super) target: String,
    pub(super) allow_private: bool,
    pub(super) refusal: RefusalSlot,
}

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let name = name.as_str().to_owned();
        let guarded = name.eq_ignore_ascii_case(&self.target);
        let allow_private = self.allow_private;
        let refusal = Arc::clone(&self.refusal);
        Box::pin(async move {
            // Port 0: reqwest substitutes the URL's port.
            let addrs: Vec<std::net::SocketAddr> = if guarded {
                match net_guard::resolve_and_validate(&name, 0, allow_private).await {
                    Ok((_, addrs)) => addrs,
                    Err(error) => {
                        *refusal.lock().unwrap_or_else(PoisonError::into_inner) =
                            Some(guard_error(&error, None));
                        return Err(Box::new(error) as Box<dyn std::error::Error + Send + Sync>);
                    }
                }
            } else {
                tokio::net::lookup_host((name.as_str(), 0))
                    .await
                    .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> {
                        Box::new(error)
                    })?
                    .collect()
            };
            Ok(Box::new(addrs.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// A request ready to send: the fresh client's call and what preparing it measured.
pub(super) struct Prepared {
    pub(super) call: reqwest::RequestBuilder,
    /// Set when the guarded resolver refused the target (system route only).
    pub(super) refusal: RefusalSlot,
    /// Resolver time of a host name (`None`: an IP literal, a spelling-classified name such as
    /// `localhost`, or a proxy resolves it).
    pub(super) dns: Option<Duration>,
    /// Watches this call's connect (the client is fresh, so nothing else shares its ledger).
    pub(super) watch: AttemptWatch,
}

/// Admits the target, builds a fresh client for it and the call; takes the body out of
/// `request`. `whole_timeout` puts `request.timeout` on the whole exchange (a streaming call
/// limits its parts itself).
pub(super) async fn prepare(
    transport: &ReqwestSafeHttp,
    request: &mut SafeRequest,
    whole_timeout: bool,
) -> Result<Prepared, SafeTransportError> {
    let (url, host, port) = parse_target(&request.url)?;
    let refusal = RefusalSlot::default();
    let builder = base_builder(request, whole_timeout, &transport.ca_roots);
    let literal_class = net_guard::classify_host_literal(&host);
    let target = Target {
        host: &host,
        port,
        literal_class,
        by_name: resolved_by_name(&url),
    };
    let (builder, dns) = if let ProxyRoute::Direct = &transport.route {
        admit_direct(builder, &target, request).await?
    } else {
        let builder = admit_via_proxy_or_system(
            builder,
            &transport.route,
            &target,
            request.allow_private,
            &refusal,
        )?;
        (builder, None)
    };
    let ledger = Arc::new(ConnLedger::default());
    let watch = ledger.watch(url.host());
    let client = builder
        .connector_layer(ConnectTimer::new(ledger))
        .build()
        .map_err(|_| SafeTransportError::Other("cannot build the HTTP client".to_owned()))?;
    let call = build_call(&client, url, request)?;
    Ok(Prepared {
        call,
        refusal,
        dns,
        watch,
    })
}

/// The admitted target of one request.
struct Target<'a> {
    host: &'a str,
    port: u16,
    /// The class of the host when its spelling tells (an IP literal, `localhost`, `*.localhost`).
    literal_class: Option<HostClass>,
    /// Whether the host is a name that DNS resolves, so the connection must be pinned or every
    /// lookup guarded.
    by_name: bool,
}

/// Whether the client resolves the host of `url` through DNS: any name, as opposed to an IP
/// literal. Names classified by their spelling (`localhost`, `*.localhost`) count, since the
/// system resolver may still send them to DNS.
pub(super) fn resolved_by_name(url: &url::Url) -> bool {
    matches!(url.host(), Some(url::Host::Domain(_)))
}

/// Parses `url` and checks, in this order, the scheme (`http(s)`), the host and the port.
fn parse_target(url: &str) -> Result<(url::Url, String, u16), SafeTransportError> {
    let url = url::Url::parse(url).map_err(|_| blocked("invalid_url"))?;
    if !matches!(url.scheme(), "http" | "https") {
        return Err(blocked("scheme"));
    }
    let host = url.host_str().ok_or_else(|| blocked("no_host"))?.to_owned();
    let port = url
        .port_or_known_default()
        .ok_or_else(|| blocked("no_port"))?;
    Ok((url, host, port))
}

/// A client builder that never follows redirects, limits connection setup, trusts `ca_roots`
/// and, when `whole_timeout` is set, limits the whole exchange.
fn base_builder(
    request: &SafeRequest,
    whole_timeout: bool,
    ca_roots: &[reqwest::Certificate],
) -> reqwest::ClientBuilder {
    let mut builder = reqwest::Client::builder()
        .redirect(reqwest::redirect::Policy::none())
        .connect_timeout(request.connect_timeout);
    if whole_timeout {
        builder = builder.timeout(request.timeout);
    }
    for root in ca_roots {
        builder = builder.add_root_certificate(root.clone());
    }
    builder
}

/// Admission behind an explicit or the system proxy. The proxy resolves the name, so only an IP
/// literal or a spelling-classified host is checked here. Then the explicit proxy is installed,
/// or, on the system route, a [`GuardedResolver`] for any target name (`localhost` and
/// `*.localhost` included); an IP literal needs none.
fn admit_via_proxy_or_system(
    builder: reqwest::ClientBuilder,
    route: &ProxyRoute,
    target: &Target<'_>,
    allow_private: bool,
    refusal: &RefusalSlot,
) -> Result<reqwest::ClientBuilder, SafeTransportError> {
    if let Some(class) = target.literal_class {
        match class {
            HostClass::Public => {}
            HostClass::Loopback | HostClass::PrivateRfc1918 | HostClass::Cgnat if allow_private => {
            }
            refused => return Err(blocked(refusal_reason(refused))),
        }
    }
    if let ProxyRoute::Proxy(proxy_url) = route {
        let proxy = reqwest::Proxy::all(proxy_url)
            .map_err(|_| SafeTransportError::Other("invalid proxy".to_owned()))?;
        return Ok(builder.proxy(proxy));
    }
    if target.by_name {
        return Ok(builder.dns_resolver(Arc::new(GuardedResolver {
            target: target.host.to_owned(),
            allow_private,
            refusal: Arc::clone(refusal),
        })));
    }
    Ok(builder)
}

/// Admission on the direct route: resolves `host` and validates every address; any name is
/// pinned to the validated addresses, and its resolver time is returned unless the name is
/// classified by its spelling (`localhost`, `*.localhost`).
async fn admit_direct(
    builder: reqwest::ClientBuilder,
    target: &Target<'_>,
    request: &SafeRequest,
) -> Result<(reqwest::ClientBuilder, Option<Duration>), SafeTransportError> {
    let builder = builder.no_proxy();
    let resolving = Instant::now();
    let addrs = validated_addresses(
        net_guard::resolve_and_validate(target.host, target.port, request.allow_private),
        request.connect_timeout,
        target.literal_class,
    )
    .await?;
    if !target.by_name {
        return Ok((builder, None));
    }
    // A name classified by its spelling reports no resolver time, like an IP literal.
    let dns = target.literal_class.is_none().then(|| resolving.elapsed());
    Ok((builder.resolve_to_addrs(target.host, &addrs), dns))
}

/// The addresses `resolving` (a [`net_guard::resolve_and_validate`]) admits within `limit` (the
/// request's connect timeout: a stuck resolver is a [`SafeTransportError::Timeout`]); a refusal
/// is mapped by [`guard_error`] with the host's spelling-based `literal_class`.
pub(super) async fn validated_addresses(
    resolving: impl Future<Output = Result<(HostClass, Vec<SocketAddr>), AppError>>,
    limit: Duration,
    literal_class: Option<HostClass>,
) -> Result<Vec<SocketAddr>, SafeTransportError> {
    let (_, addrs) = tokio::time::timeout(limit, resolving)
        .await
        .map_err(|_| SafeTransportError::Timeout)?
        .map_err(|error| guard_error(&error, literal_class))?;
    Ok(addrs)
}

/// The call of `request` on `client`: method, headers (marked sensitive) and the body taken out
/// of `request`.
fn build_call(
    client: &reqwest::Client,
    url: url::Url,
    request: &mut SafeRequest,
) -> Result<reqwest::RequestBuilder, SafeTransportError> {
    let mut call = match request.method {
        SafeMethod::Get => client.get(url),
        SafeMethod::Post => client.post(url),
    };
    for (name, value) in &request.headers {
        let name = reqwest::header::HeaderName::from_bytes(name.as_bytes())
            .map_err(|_| SafeTransportError::Other("invalid header name".to_owned()))?;
        let mut value = reqwest::header::HeaderValue::from_str(value)
            .map_err(|_| SafeTransportError::Other("invalid header value".to_owned()))?;
        value.set_sensitive(true);
        call = call.header(name, value);
    }
    if let Some(body) = request.body.take() {
        call = call.body(body);
    }
    Ok(call)
}
