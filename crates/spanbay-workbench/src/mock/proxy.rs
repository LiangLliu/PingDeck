//! Forwards unmatched requests to the configured upstream (shared async client) and relays its response.
//!
//! Bodies cross byte for byte in both directions. An upstream body larger than
//! [`MAX_BODY_BYTES`] is refused with a `502` that says so, never relayed cut short.

use std::net::SocketAddr;
use std::time::Duration;

use bytes::Bytes;
use reqwest::Method;
use url::{Host, Url};

use crate::workspace::items::{MockLogOutcome, MockServer};

use super::http::{MAX_BODY_BYTES, ParsedRequest, sanitized_header};
use super::reply::MockReply;
use super::routing::header_value;
use super::template::{TemplateContext, render};

/// Route id recorded for proxied transactions.
const PROXY_ROUTE_ID: &str = "proxy";

/// Header counting how many spanbay proxies a request has already crossed.
const HOPS_HEADER: &str = "x-spanbay-hops";

/// A request that arrives having crossed this many proxies is refused, which ends every
/// proxy cycle (a server proxying to itself, or two servers proxying to each other).
const MAX_PROXY_HOPS: u32 = 3;

/// Relays one request to `proxy_base_url` and returns the upstream response.
///
/// A request whose target is this very server (`bound`, or a loopback alias of it) or that has
/// already crossed [`MAX_PROXY_HOPS`] proxies is answered `502` instead of being forwarded.
/// Without `proxy_remove_prefix`, or for a request outside the endpoint prefix, the original
/// path is forwarded; it is never silently rewritten to `/`.
pub(super) async fn proxy_reply(
    config: &MockServer,
    client: &reqwest::Client,
    request: &ParsedRequest,
    relative_path: Option<&str>,
    context: &TemplateContext<'_>,
    bound: SocketAddr,
) -> MockReply {
    let hops = header_value(&request.headers, HOPS_HEADER)
        .and_then(|value| value.trim().parse::<u32>().ok())
        .unwrap_or(0);
    if hops >= MAX_PROXY_HOPS {
        return proxy_failure("proxy loop: the request crossed too many mock proxies");
    }

    let base = config.proxy_base_url.trim().trim_end_matches('/');
    let path = match (config.proxy_remove_prefix, relative_path) {
        (true, Some(relative)) => relative,
        _ => request.path.as_str(),
    };
    let mut url = format!("{base}{path}");
    if !request.query.is_empty() {
        url.push('?');
        url.push_str(&request.query);
    }
    let Ok(url) = Url::parse(&url) else {
        return proxy_failure("the proxy target is not a valid URL");
    };
    if targets_bound_address(&url, bound) {
        return proxy_failure("proxy loop: the proxy target is this mock server");
    }
    if let Err(message) = ensure_target_allowed(&url).await {
        return proxy_failure(&message);
    }

    let method = Method::from_bytes(request.method.as_bytes()).unwrap_or(Method::GET);
    let mut builder = client.request(method, url);
    for (key, value) in &request.headers {
        if is_forwarded_header(key) {
            builder = builder.header(key.as_str(), value.as_str());
        }
    }
    builder = builder.header(HOPS_HEADER, hops + 1);
    for header in config
        .proxy_req_headers
        .iter()
        .filter(|header| header.enabled)
    {
        if let Some((name, value)) = sanitized_header(&header.key, &render(&header.value, context))
        {
            builder = builder.header(name, value);
        }
    }
    if !request.raw_body.is_empty() {
        builder = builder.body(request.raw_body.clone());
    }

    let response = match builder.send().await {
        Ok(response) => response,
        Err(error) => return proxy_failure(&error.without_url().to_string()),
    };
    let status = response.status().as_u16();
    let mut headers: Vec<(String, String)> = response
        .headers()
        .iter()
        .filter(|(name, _)| !is_proxy_reserved_response_header(name.as_str()))
        .map(|(name, value)| {
            (
                name.as_str().to_owned(),
                value.to_str().unwrap_or("<non-utf8>").to_owned(),
            )
        })
        .collect();
    for header in config
        .proxy_res_headers
        .iter()
        .filter(|header| header.enabled)
    {
        headers.push((header.key.clone(), render(&header.value, context)));
    }
    let body = match read_capped_body(response).await {
        Ok(CappedBody::Whole(body)) => body,
        Ok(CappedBody::TooLarge) => {
            return proxy_failure(&format!(
                "the upstream response body exceeds {MAX_BODY_BYTES} bytes and was not relayed"
            ));
        }
        Err(error) => return proxy_failure(&error.to_string()),
    };

    let mut reply = MockReply::json(
        status,
        &serde_json::Value::Null,
        Some(PROXY_ROUTE_ID.to_owned()),
    )
    .with_outcome(MockLogOutcome::Proxy);
    reply.headers = headers;
    reply.body = body;
    reply.proxied = true;
    reply
}

fn proxy_failure(message: &str) -> MockReply {
    MockReply::json(
        502,
        &serde_json::json!({ "error": "proxy_request_failed", "message": message }),
        Some(PROXY_ROUTE_ID.to_owned()),
    )
    .with_outcome(MockLogOutcome::ProxyFailed)
}

/// Resolver of the shared outbound client: every name a proxy target or callback connects to
/// is resolved through [`crate::net_guard::resolve_with`], and reqwest connects to exactly the
/// addresses it returns. The address that was checked is therefore the address that is used, so
/// a DNS answer that changes between a pre-check and the connect (a rebind) cannot reach a
/// link-local or metadata address. IP-literal hosts skip DNS and are classified by
/// [`ensure_target_allowed`] before the request is built.
#[derive(Debug, Default)]
pub(super) struct GuardedResolver;

impl reqwest::dns::Resolve for GuardedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let host = name.as_str().to_owned();
        Box::pin(async move {
            // Port 0: reqwest substitutes the URL's port.
            let (_, addresses) = crate::net_guard::resolve_with(&host, 0, true, true)
                .await
                .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> {
                    error.to_string().into()
                })?;
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// The shared outbound client of proxies and callbacks: no system proxy, no redirects (a `3xx`
/// is relayed as-is), 10 s per request, and connections pinned to checked addresses
/// ([`GuardedResolver`]).
pub(super) fn outbound_client() -> reqwest::Result<reqwest::Client> {
    reqwest::Client::builder()
        .timeout(Duration::from_secs(10))
        .no_proxy()
        .redirect(reqwest::redirect::Policy::none())
        .dns_resolver(std::sync::Arc::new(GuardedResolver))
        .build()
}

/// Refuses a typed target that is link-local or cloud-metadata (`169.254.0.0/16`, `fe80::/10`,
/// CGNAT, multicast, unspecified), whether it is spelled as an address or resolves to one.
/// Loopback and private networks stay reachable: proxying to local services is the use case.
/// Used for proxy targets and callbacks alike; it gives the early, readable refusal, while
/// [`GuardedResolver`] pins the connection itself.
pub(super) async fn ensure_target_allowed(url: &Url) -> Result<(), String> {
    let host = url.host_str().ok_or("the target URL has no host")?;
    let port = url.port_or_known_default().unwrap_or(80);
    crate::net_guard::resolve_with(host, port, true, true)
        .await
        .map(drop)
        .map_err(|error| error.to_string())
}

/// Request headers worth forwarding: everything except hop-by-hop framing and
/// the headers `reqwest` recomputes (`Host`, `Content-Length`, encodings).
fn is_forwarded_header(name: &str) -> bool {
    ![
        "host",
        "content-length",
        "connection",
        "keep-alive",
        "transfer-encoding",
        "te",
        "trailer",
        "upgrade",
        "proxy-connection",
        "accept-encoding",
        HOPS_HEADER,
    ]
    .iter()
    .any(|skipped| name.eq_ignore_ascii_case(skipped))
}

/// Upstream response headers that would contradict the body we relay.
fn is_proxy_reserved_response_header(name: &str) -> bool {
    ["content-length", "content-encoding", "connection", "date"]
        .iter()
        .any(|reserved| name.eq_ignore_ascii_case(reserved))
}

/// Whether `url` points back at the server listening on `bound`: its port and a loopback host
/// (`127.x`, `::1`, an unspecified address, or `localhost`).
fn targets_bound_address(url: &Url, bound: SocketAddr) -> bool {
    url.port_or_known_default() == Some(bound.port())
        && match url.host() {
            Some(Host::Domain(domain)) => domain
                .trim_end_matches('.')
                .eq_ignore_ascii_case("localhost"),
            Some(Host::Ipv4(ip)) => ip.is_loopback() || ip.is_unspecified(),
            Some(Host::Ipv6(ip)) => ip.is_loopback() || ip.is_unspecified(),
            None => false,
        }
}

/// An upstream body read under the [`MAX_BODY_BYTES`] cap.
pub(super) enum CappedBody {
    /// The whole body, byte for byte.
    Whole(Bytes),
    /// The body is larger than the cap; reading stopped there.
    TooLarge,
}

/// Reads `response` up to [`MAX_BODY_BYTES`], refusing a larger declared length before reading
/// and stopping the pull as soon as one byte past the cap arrives.
pub(super) async fn read_capped_body(
    mut response: reqwest::Response,
) -> reqwest::Result<CappedBody> {
    let limit = u64::try_from(MAX_BODY_BYTES).unwrap_or(u64::MAX);
    if response
        .content_length()
        .is_some_and(|length| length > limit)
    {
        return Ok(CappedBody::TooLarge);
    }
    let mut bytes = Vec::new();
    while let Some(chunk) = response.chunk().await? {
        if chunk.len() > MAX_BODY_BYTES - bytes.len() {
            return Ok(CappedBody::TooLarge);
        }
        bytes.extend_from_slice(&chunk);
    }
    Ok(CappedBody::Whole(Bytes::from(bytes)))
}
