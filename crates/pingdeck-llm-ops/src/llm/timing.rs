//! Per-attempt connection facts of the shared pooled HTTP clients.
//!
//! Single responsibility: tell, for one upstream attempt, whether it reused a pooled connection and
//! how long DNS and connect took, without owning the clients. Three hooks of reqwest 0.13 do it:
//! a custom DNS resolver ([`TimedResolver`]), a connector layer ([`ConnectTimer`]) that times the
//! connect future (which includes TLS for `https`, and the name lookup the connector makes), and
//! the connection facts of the response (`HttpInfo`: local and remote address).
//!
//! * Reuse: a connection is identified by its `(local, remote)` socket addresses; an attempt whose
//!   connection was seen before reused it. Exact, works for IP literals too.
//! * DNS and connect: both hooks record per host. An attempt claims the records of its host made
//!   during it, but only when no other attempt to the same host overlapped (otherwise the records
//!   cannot be told apart, and the phases are `not measured`). The connect layer's time contains
//!   the lookup, so the connect phase is that time minus the measured DNS time; it is measured
//!   only when DNS is known (a time, or not needed for an IP literal). Connect and TLS are one
//!   segment (`connect_includes_tls`); TLS is not split out.

use std::collections::{HashMap, HashSet, VecDeque};
use std::fmt::Debug;
use std::future::Future;
use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use tower::{Layer, Service};

use crate::llm::capture::{AttemptTiming, PhaseValue};
use pingdeck_base::net_guard::{HostClass, classify_ip};

/// DNS lookups and connects remembered per host (each).
const MAX_RECORDS_PER_HOST: usize = 8;
/// Hosts remembered; beyond this the idle ones are dropped.
const MAX_HOSTS: usize = 256;
/// Connections remembered; beyond this the memory restarts (a miss only means "not reused").
const MAX_CONNECTIONS: usize = 4096;

#[derive(Default)]
struct HostState {
    in_flight: u32,
    /// Attempts started so far, to spot an attempt that began while another was running.
    starts: u64,
    /// DNS lookups: when each started and how long it took, oldest first.
    lookups: VecDeque<Record>,
    /// Connect attempts (the connector's time, name lookup included), oldest first.
    connects: VecDeque<Record>,
}

/// When a lookup or connect started and how long it took.
type Record = (Instant, Duration);

/// Appends `record`, dropping the oldest one when [`MAX_RECORDS_PER_HOST`] are kept.
fn push_bounded(records: &mut VecDeque<Record>, record: Record) {
    if records.len() >= MAX_RECORDS_PER_HOST {
        records.pop_front();
    }
    records.push_back(record);
}

#[derive(Default)]
struct Inner {
    hosts: HashMap<String, HostState>,
    connections: HashSet<(SocketAddr, SocketAddr)>,
}

/// What the resolver and the attempts of every shared client tell each other.
#[derive(Default)]
pub struct ConnLedger {
    inner: Mutex<Inner>,
}

impl ConnLedger {
    fn lock(&self) -> std::sync::MutexGuard<'_, Inner> {
        self.inner.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Records one DNS lookup of `host`.
    fn record_dns(&self, host: &str, started: Instant, took: Duration) {
        let mut inner = self.lock();
        let state = inner.hosts.entry(host.to_ascii_lowercase()).or_default();
        push_bounded(&mut state.lookups, (started, took));
    }

    /// Records one connect (or failed connect) of `host`, lookup and TLS included.
    fn record_connect(&self, host: &str, started: Instant, took: Duration) {
        let mut inner = self.lock();
        let state = inner.hosts.entry(host.to_ascii_lowercase()).or_default();
        push_bounded(&mut state.connects, (started, took));
    }

    /// Starts watching an attempt toward `host` (a domain name or an IP literal).
    pub fn watch(self: &Arc<Self>, host: Option<url::Host<&str>>) -> AttemptWatch {
        let started = Instant::now();
        let target = host.map(|host| {
            let (name, literal) = match host {
                url::Host::Domain(name) => (name.to_ascii_lowercase(), false),
                url::Host::Ipv4(ip) => (ip.to_string(), true),
                url::Host::Ipv6(ip) => (ip.to_string(), true),
            };
            let mut inner = self.lock();
            if inner.hosts.len() >= MAX_HOSTS {
                inner.hosts.retain(|_, state| state.in_flight > 0);
            }
            let state = inner.hosts.entry(name.clone()).or_default();
            let alone = state.in_flight == 0;
            state.in_flight += 1;
            state.starts += 1;
            Watched {
                name,
                literal,
                alone,
                starts_after_begin: state.starts,
            }
        });
        AttemptWatch {
            ledger: Arc::clone(self),
            started,
            target,
        }
    }

    /// Notes a connection and says whether it was already known (a pooled connection reused).
    fn connection_known(&self, local: SocketAddr, remote: SocketAddr) -> bool {
        let mut inner = self.lock();
        if inner.connections.len() >= MAX_CONNECTIONS {
            inner.connections.clear();
        }
        !inner.connections.insert((local, remote))
    }
}

/// The host an attempt talks to, as the ledger keys it.
struct Watched {
    /// Lower-case name, or the IP literal without brackets.
    name: String,
    literal: bool,
    /// No other attempt to this host was running when this one began.
    alone: bool,
    /// The host's start counter right after this attempt began.
    starts_after_begin: u64,
}

/// One running attempt, as seen by the ledger. Dropping it ends the watch.
pub struct AttemptWatch {
    ledger: Arc<ConnLedger>,
    started: Instant,
    target: Option<Watched>,
}

impl AttemptWatch {
    /// The attempt's `send` instant.
    #[must_use]
    pub fn started(&self) -> Instant {
        self.started
    }

    /// Whether the connection of the response (`info`: its local and remote address) had served
    /// an earlier attempt. `None` when the response carries no connection facts.
    #[must_use]
    fn reused(&self, info: Option<(SocketAddr, SocketAddr)>) -> Option<bool> {
        info.map(|(local, remote)| self.ledger.connection_known(local, remote))
    }

    /// The summed lookup and connect times this attempt's host recorded during it, when they can
    /// be attributed (no other attempt to the host overlapped).
    fn claimed(&self) -> Option<(Duration, Duration)> {
        let target = self.target.as_ref()?;
        let inner = self.ledger.lock();
        let state = inner.hosts.get(&target.name)?;
        if !target.alone || state.starts != target.starts_after_begin {
            return None;
        }
        let sum = |records: &VecDeque<Record>| -> Duration {
            records
                .iter()
                .filter(|(at, _)| *at >= self.started)
                .map(|(_, took)| *took)
                .sum()
        };
        Some((sum(&state.lookups), sum(&state.connects)))
    }

    /// The connect time (TLS inside) of this attempt's own connection when it was alone on its
    /// host. For a client that resolves nothing itself (pinned addresses, a literal host).
    #[must_use]
    pub(crate) fn connect_only(&self) -> Option<Duration> {
        self.claimed()
            .map(|(_, connects)| connects)
            .filter(|connects| !connects.is_zero())
    }

    fn dns_of(&self, lookups: Option<Duration>) -> PhaseValue {
        match (&self.target, lookups) {
            (None, _) => PhaseValue::NotMeasured,
            (Some(target), _) if target.literal => PhaseValue::NotNeeded,
            (Some(_), Some(took)) if !took.is_zero() => PhaseValue::Ms(ceil_ms(took)),
            _ => PhaseValue::NotMeasured,
        }
    }

    /// DNS and connect of this attempt. The connect segment (TLS inside) is the connector's time
    /// minus the name lookup it made within it, measured only when the attempt opened a
    /// connection of its own, alone on its host, and its DNS time is known (a time, or not
    /// needed for a literal). Otherwise `not measured`.
    #[must_use]
    fn dns_and_connect(&self) -> (PhaseValue, PhaseValue) {
        let claimed = self.claimed();
        let dns = self.dns_of(claimed.map(|(lookups, _)| lookups));
        let connect = match claimed {
            Some((lookups, connects)) if !connects.is_zero() && dns != PhaseValue::NotMeasured => {
                let own = connects.saturating_sub(lookups);
                // A connect that took time is never shown as 0.
                PhaseValue::Ms(ceil_ms(own).max(1))
            }
            _ => PhaseValue::NotMeasured,
        };
        (dns, connect)
    }
}

impl Drop for AttemptWatch {
    fn drop(&mut self) {
        if let Some(target) = &self.target
            && let Some(state) = self.ledger.lock().hosts.get_mut(&target.name)
        {
            state.in_flight = state.in_flight.saturating_sub(1);
        }
    }
}

/// The DNS resolver of the shared clients: the system's `getaddrinfo` (what reqwest uses by
/// default), with every lookup timed into the [`ConnLedger`].
#[derive(Clone)]
pub struct TimedResolver {
    ledger: Arc<ConnLedger>,
}

impl TimedResolver {
    /// A resolver reporting to `ledger`.
    #[must_use]
    pub fn new(ledger: Arc<ConnLedger>) -> Self {
        Self { ledger }
    }
}

/// Fails when `host` resolved to any address [`HostClass::LinkLocalOrMetadata`] covers (link-local,
/// cloud metadata, multicast, unspecified, reserved): the upstream clients never connect there,
/// whatever name led to it, which also closes DNS rebinding for pooled clients. Loopback,
/// private and CGNAT answers pass (local runtimes and VPN hosts are what users configure).
fn refuse_forbidden_answers(host: &str, addresses: &[SocketAddr]) -> std::io::Result<()> {
    match addresses
        .iter()
        .find(|address| classify_ip(address.ip()) == HostClass::LinkLocalOrMetadata)
    {
        Some(address) => Err(std::io::Error::new(
            std::io::ErrorKind::PermissionDenied,
            format!(
                "refusing to connect to {host} ({}): link-local, cloud metadata, multicast, unspecified and reserved addresses are never allowed",
                address.ip()
            ),
        )),
        None => Ok(()),
    }
}

impl reqwest::dns::Resolve for TimedResolver {
    fn resolve(&self, name: reqwest::dns::Name) -> reqwest::dns::Resolving {
        let ledger = Arc::clone(&self.ledger);
        let host = name.as_str().to_owned();
        Box::pin(async move {
            let started = Instant::now();
            // Port 0: reqwest substitutes the URL's port.
            let addresses: Vec<SocketAddr> = tokio::net::lookup_host((host.as_str(), 0))
                .await
                .map_err(|error| -> Box<dyn std::error::Error + Send + Sync> { Box::new(error) })?
                .collect();
            ledger.record_dns(&host, started, started.elapsed());
            refuse_forbidden_answers(&host, &addresses)?;
            Ok(Box::new(addresses.into_iter()) as reqwest::dns::Addrs)
        })
    }
}

/// The host of a connector request, taken from its `Debug` form (`Unnameable(http://host:port/)`:
/// reqwest does not export the request type). A change of that form only makes the connect phase
/// `not measured`. The `s1_*` tests in `pingdeck-llm`'s `upstream_tests.rs` pin reqwest's real
/// form; `the_host_comes_from_the_connector_requests_debug_form` (`timing/tests.rs`) pins the
/// parsing.
#[must_use]
fn host_of_request(request: &dyn Debug) -> Option<String> {
    let text = format!("{request:?}");
    let uri = text.strip_prefix("Unnameable(")?.strip_suffix(')')?;
    let parsed = url::Url::parse(uri).ok()?;
    Some(
        parsed
            .host_str()?
            .trim_matches(['[', ']'])
            .to_ascii_lowercase(),
    )
}

/// The connector layer of the shared clients: times every connect (name lookup and, for `https`,
/// the TLS handshake included) into the [`ConnLedger`].
#[derive(Clone)]
pub struct ConnectTimer {
    ledger: Arc<ConnLedger>,
}

impl ConnectTimer {
    /// A layer reporting to `ledger`.
    #[must_use]
    pub fn new(ledger: Arc<ConnLedger>) -> Self {
        Self { ledger }
    }
}

impl<S> Layer<S> for ConnectTimer {
    type Service = TimedConnect<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TimedConnect {
            inner,
            ledger: Arc::clone(&self.ledger),
        }
    }
}

/// A connector service whose calls are timed; see [`ConnectTimer`].
#[derive(Clone)]
pub struct TimedConnect<S> {
    inner: S,
    ledger: Arc<ConnLedger>,
}

impl<S, R> Service<R> for TimedConnect<S>
where
    S: Service<R>,
    S::Future: Send + 'static,
    S::Response: 'static,
    S::Error: 'static,
    R: Debug,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, S::Error>> + Send>>;

    fn poll_ready(&mut self, context: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(context)
    }

    fn call(&mut self, request: R) -> Self::Future {
        let host = host_of_request(&request);
        let ledger = Arc::clone(&self.ledger);
        let started = Instant::now();
        let connecting = self.inner.call(request);
        Box::pin(async move {
            let result = connecting.await;
            if let Some(host) = host {
                ledger.record_connect(&host, started, started.elapsed());
            }
            result
        })
    }
}

/// Re-exported so `pingdeck_llm::llm::upstream::ceil_ms` keeps its path.
pub use pingdeck_base::time::ceil_ms;

/// What one attempt learned about its connection, before the response body is read.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ConnFacts {
    /// DNS time: measured, not needed (an IP literal) or `not measured`.
    pub dns: PhaseValue,
    /// Connect time with TLS inside, net of the name lookup the connector made; `not measured`
    /// unless the attempt opened its own connection, alone on its host, with a known DNS time.
    pub connect: PhaseValue,
    /// A pooled connection served the attempt (`None`: the stack could not say).
    pub reused: Option<bool>,
}

impl ConnFacts {
    /// Nothing known (a failure before any response, or a transport without facts).
    pub const UNKNOWN: Self = Self {
        dns: PhaseValue::NotMeasured,
        connect: PhaseValue::NotMeasured,
        reused: None,
    };

    /// The facts of an attempt that got a response: DNS and connect from the watch, reuse from the
    /// response's connection (`info`: local and remote address).
    #[must_use]
    pub fn answered(watch: &AttemptWatch, info: Option<(SocketAddr, SocketAddr)>) -> Self {
        let (dns, connect) = watch.dns_and_connect();
        Self {
            dns,
            connect,
            reused: watch.reused(info),
        }
    }

    /// The facts of an attempt that failed before any response head (the connect may have run).
    #[must_use]
    pub fn unanswered(watch: &AttemptWatch) -> Self {
        let (dns, connect) = watch.dns_and_connect();
        Self {
            dns,
            connect,
            reused: None,
        }
    }

    /// The [`AttemptTiming`] of the attempt: `headers_ms` is the time from `send` to the response
    /// headers (`None` when none arrived), `first_token_ms` and `end_ms` as the caller measured
    /// them. A reused connection shows DNS, connect and TLS as reused. Otherwise DNS and connect
    /// (TLS inside, `connect_includes_tls`) are shown as measured or `not measured`, and
    /// `headers_ms` is what remains of the time to headers after them; TLS is not split out.
    #[must_use]
    pub fn timing(
        &self,
        headers_ms: Option<u64>,
        first_token_ms: PhaseValue,
        end_ms: u64,
    ) -> AttemptTiming {
        if self.reused == Some(true)
            && let Some(headers_ms) = headers_ms
        {
            return AttemptTiming::on_reused_connection(headers_ms, first_token_ms, end_ms);
        }
        // The setup phases lie inside the time to headers, so together they never show as more.
        let mut room = headers_ms;
        let mut fit = |phase: PhaseValue| match (phase, room) {
            (PhaseValue::Ms(ms), Some(left)) => {
                let shown = ms.min(left);
                room = Some(left - shown);
                PhaseValue::Ms(shown)
            }
            (other, _) => other,
        };
        let dns = fit(self.dns);
        let connect = fit(self.connect);
        AttemptTiming {
            dns,
            connect,
            tls: PhaseValue::NotMeasured,
            connect_includes_tls: true,
            headers_ms: room.map_or(PhaseValue::NotMeasured, PhaseValue::Ms),
            first_token_ms,
            end_ms: PhaseValue::Ms(end_ms),
            reused: false,
        }
    }
}

#[cfg(test)]
mod tests;
