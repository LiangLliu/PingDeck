//! Connection-phase timing for the executor's cached `reqwest` clients.
//!
//! `reqwest` reports no DNS or connect times, so every client is built with a resolver wrapper
//! and a connector layer that note how long they took into a shared [`ConnectProbe`]. A send
//! takes an [`Attempt`] before it goes out and asks afterwards whether it opened a connection
//! (and how long DNS and the connect took) or ran on a pooled one.
//!
//! Spans are matched to a send by time, so the answer is only trusted when the send was the one
//! request in flight on its client; with an overlapping send on the same client the connection
//! is [`Connection::Unknown`] and the timing keeps its connection phases absent.

use std::net::SocketAddr;
use std::pin::Pin;
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::task::{Context, Poll};
use std::time::{Duration, Instant};

use reqwest::dns::{Addrs, Name, Resolve, Resolving};
use tower::{Layer, Service};

use crate::executor::ResolveGuard;
use crate::executor::types::GuardRefusal;
use spanbay_base::time::ceil_ms;
use spanbay_model::models::PhaseTiming;
use spanbay_model::phase::PhaseValue;

/// Spans kept per client: a send only looks for spans that started after it began, so a handful
/// of the newest is enough.
const KEPT_SPANS: usize = 16;

/// One timed step: when it started and how long it took.
#[derive(Debug, Clone, Copy)]
struct Span {
    started: Instant,
    took: Duration,
}

#[derive(Debug, Default)]
struct ProbeState {
    /// Sends in flight on the client.
    active: usize,
    /// Sends that began while another was in flight.
    overlaps: u64,
    resolves: Vec<Span>,
    connects: Vec<Span>,
}

impl ProbeState {
    fn keep(spans: &mut Vec<Span>, span: Span) {
        if spans.len() == KEPT_SPANS {
            spans.remove(0);
        }
        spans.push(span);
    }
}

/// What the resolver and connector of one client have seen.
#[derive(Debug, Default)]
pub(super) struct ConnectProbe {
    state: Mutex<ProbeState>,
}

impl ConnectProbe {
    fn state(&self) -> MutexGuard<'_, ProbeState> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Marks the start of a send; hold the [`Attempt`] until its response head arrived.
    pub(super) fn begin(self: &Arc<Self>) -> Attempt {
        let mut state = self.state();
        let solo = state.active == 0;
        if !solo {
            state.overlaps += 1;
        }
        state.active += 1;
        Attempt {
            probe: Arc::clone(self),
            started: Instant::now(),
            solo,
            overlaps: state.overlaps,
        }
    }
}

/// One send on a probed client; leaving scope ends it (also when the send is cancelled).
pub(super) struct Attempt {
    probe: Arc<ConnectProbe>,
    started: Instant,
    /// Nothing else was in flight when this send began.
    solo: bool,
    overlaps: u64,
}

impl Attempt {
    /// What this send learned about its connection: opened one (its DNS and connect spans) or
    /// reused a pooled one. Unknown when another send shared the client meanwhile.
    pub(super) fn connection(&self) -> Connection {
        let state = self.probe.state();
        if !self.solo || state.overlaps != self.overlaps {
            return Connection::Unknown;
        }
        let since = |spans: &[Span], not_before: Instant| {
            spans
                .iter()
                .rev()
                .find(|span| span.started >= not_before)
                .copied()
        };
        match since(&state.connects, self.started) {
            None => Connection::Reused,
            // The resolver runs inside the connector call, so its span starts after it.
            Some(connect) => Connection::Opened {
                dns: since(&state.resolves, connect.started).map(|span| span.took),
                connect: connect.took,
            },
        }
    }
}

impl Drop for Attempt {
    fn drop(&mut self) {
        let mut state = self.probe.state();
        state.active = state.active.saturating_sub(1);
    }
}

/// How a send got its connection.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Connection {
    /// Could not be told apart from a concurrent send on the same client.
    Unknown,
    /// A pooled connection served the request.
    Reused,
    /// The send opened a connection: the connector's whole call, and the part of it spent in the
    /// resolver (`None` for an IP literal, which needs no lookup).
    Opened {
        dns: Option<Duration>,
        connect: Duration,
    },
}

impl Connection {
    /// Adds the connection phases to `timing` (whose `request_ms` is already set).
    ///
    /// Through a proxy the connector dials the proxy and tunnels, so a new connection's phases
    /// would describe the wrong peer: they stay absent. TLS is never separable from the
    /// connector call, so HTTPS reports `connect_includes_tls` and plain HTTP `tls_ms` as not
    /// needed.
    pub(super) fn apply(self, https: bool, proxied: bool, mut timing: PhaseTiming) -> PhaseTiming {
        timing.phases_version = Some(1);
        match self {
            Self::Unknown => {}
            Self::Reused => {
                timing.dns_ms = PhaseValue::Reused;
                timing.connect_ms = PhaseValue::Reused;
                timing.tls_ms = PhaseValue::Reused;
                timing.wait_ms = Some(u64::try_from(timing.request_ms).unwrap_or(u64::MAX));
            }
            Self::Opened { .. } if proxied => {}
            Self::Opened { dns, connect } => {
                let dns_ms = dns.map(|took| ceil_ms(took).max(1));
                let connect_ms = ceil_ms(connect.saturating_sub(dns.unwrap_or_default())).max(1);
                timing.dns_ms = dns_ms.map_or(PhaseValue::NotNeeded, PhaseValue::Ms);
                timing.connect_ms = PhaseValue::Ms(connect_ms);
                if https {
                    timing.connect_includes_tls = true;
                } else {
                    timing.tls_ms = PhaseValue::NotNeeded;
                }
                let setup = dns_ms.unwrap_or(0).saturating_add(connect_ms);
                let request_ms = u64::try_from(timing.request_ms).unwrap_or(u64::MAX);
                timing.wait_ms = Some(request_ms.saturating_sub(setup));
            }
        }
        timing
    }
}

/// Resolver that times each lookup into the probe; the lookup itself is `getaddrinfo` on the
/// blocking pool, as in `reqwest`'s default resolver. With a [`ResolveGuard`] every address is
/// classified and only validated ones are returned, so reqwest connects to what was checked.
#[derive(Debug)]
pub(super) struct ProbedResolver {
    probe: Arc<ConnectProbe>,
    /// The guard and the port of the one `host:port` this client is built for.
    guard: Option<(ResolveGuard, u16)>,
}

impl ProbedResolver {
    pub(super) fn new(probe: Arc<ConnectProbe>) -> Self {
        Self { probe, guard: None }
    }

    /// A resolver that also enforces `guard` (SSRF pinning).
    pub(super) fn guarded(probe: Arc<ConnectProbe>, guard: ResolveGuard, port: u16) -> Self {
        Self {
            probe,
            guard: Some((guard, port)),
        }
    }
}

/// The addresses of `host`: the system lookup, or the guard's test override.
async fn lookup(host: &str, guard: Option<&ResolveGuard>) -> std::io::Result<Vec<SocketAddr>> {
    #[cfg(test)]
    if let Some(found) = guard.and_then(|guard| guard.lookup.as_ref()) {
        return Ok(found(host)
            .into_iter()
            .map(|ip| SocketAddr::new(ip, 0))
            .collect());
    }
    let _ = guard;
    Ok(tokio::net::lookup_host((host, 0)).await?.collect())
}

impl Resolve for ProbedResolver {
    fn resolve(&self, name: Name) -> Resolving {
        let probe = Arc::clone(&self.probe);
        let guard = self.guard.clone();
        Box::pin(async move {
            let started = Instant::now();
            let found = lookup(name.as_str(), guard.as_ref().map(|(guard, _)| guard)).await;
            let span = Span {
                started,
                took: started.elapsed(),
            };
            ProbeState::keep(&mut probe.state().resolves, span);
            let mut found = found?;
            if let Some((guard, port)) = &guard {
                for address in &found {
                    let class = spanbay_base::net_guard::classify_ip(address.ip());
                    if !guard.permits(name.as_str(), *port, class) {
                        return Err(GuardRefusal {
                            host: name.as_str().to_owned(),
                            address: Some(address.ip()),
                        }
                        .into());
                    }
                }
                if found.is_empty() {
                    return Err(format!("could not resolve {}", name.as_str()).into());
                }
            }
            let addrs: Addrs = Box::new(std::mem::take(&mut found).into_iter());
            Ok::<Addrs, Box<dyn std::error::Error + Send + Sync>>(addrs)
        })
    }
}

/// Layer that times every call of the connector (DNS, TCP and TLS together) into the probe.
#[derive(Debug, Clone)]
pub(super) struct ConnectTimer {
    probe: Arc<ConnectProbe>,
}

impl ConnectTimer {
    pub(super) fn new(probe: Arc<ConnectProbe>) -> Self {
        Self { probe }
    }
}

impl<S> Layer<S> for ConnectTimer {
    type Service = TimedConnector<S>;

    fn layer(&self, inner: S) -> Self::Service {
        TimedConnector {
            inner,
            probe: Arc::clone(&self.probe),
        }
    }
}

/// The connector wrapped by [`ConnectTimer`].
#[derive(Debug, Clone)]
pub(super) struct TimedConnector<S> {
    inner: S,
    probe: Arc<ConnectProbe>,
}

impl<S, Request> Service<Request> for TimedConnector<S>
where
    S: Service<Request>,
    S::Future: Send + 'static,
    S::Response: 'static,
    S::Error: 'static,
{
    type Response = S::Response;
    type Error = S::Error;
    type Future = Pin<Box<dyn Future<Output = Result<S::Response, S::Error>> + Send>>;

    fn poll_ready(&mut self, cx: &mut Context<'_>) -> Poll<Result<(), Self::Error>> {
        self.inner.poll_ready(cx)
    }

    fn call(&mut self, request: Request) -> Self::Future {
        let probe = Arc::clone(&self.probe);
        let started = Instant::now();
        let future = self.inner.call(request);
        Box::pin(async move {
            let connected = future.await;
            // A failed connect is reported through the send's error, not as a phase.
            if connected.is_ok() {
                let span = Span {
                    started,
                    took: started.elapsed(),
                };
                ProbeState::keep(&mut probe.state().connects, span);
            }
            connected
        })
    }
}

#[cfg(test)]
mod tests {
    use std::future::{Ready, ready};
    use std::str::FromStr;
    use std::thread::sleep;

    use super::*;

    fn ms(value: u64) -> Duration {
        Duration::from_millis(value)
    }

    /// Lets the monotonic clock move past an earlier reading.
    fn tick() {
        sleep(ms(5));
    }

    fn span(started: Instant, took_ms: u64) -> Span {
        Span {
            started,
            took: ms(took_ms),
        }
    }

    #[test]
    fn a_send_without_a_connector_call_ran_on_a_pooled_connection() {
        let probe = Arc::new(ConnectProbe::default());
        // A connect of an earlier send does not count for this one.
        ProbeState::keep(&mut probe.state().connects, span(Instant::now(), 30));
        tick();
        let attempt = probe.begin();
        assert_eq!(attempt.connection(), Connection::Reused);
    }

    #[test]
    fn a_send_that_opened_a_connection_reports_its_connect_and_the_dns_inside_it() {
        let probe = Arc::new(ConnectProbe::default());
        ProbeState::keep(&mut probe.state().resolves, span(Instant::now(), 99));
        tick();
        let attempt = probe.begin();
        tick();
        let connect_started = Instant::now();
        tick();
        {
            let mut state = probe.state();
            ProbeState::keep(&mut state.resolves, span(Instant::now(), 8));
            ProbeState::keep(&mut state.connects, span(connect_started, 30));
        }
        assert_eq!(
            attempt.connection(),
            Connection::Opened {
                dns: Some(ms(8)),
                connect: ms(30)
            }
        );
    }

    #[test]
    fn an_ip_literal_connect_has_no_dns_span() {
        let probe = Arc::new(ConnectProbe::default());
        let attempt = probe.begin();
        tick();
        ProbeState::keep(&mut probe.state().connects, span(Instant::now(), 4));
        assert_eq!(
            attempt.connection(),
            Connection::Opened {
                dns: None,
                connect: ms(4)
            }
        );
    }

    #[test]
    fn overlapping_sends_are_unknown_and_a_finished_send_frees_the_client() {
        let probe = Arc::new(ConnectProbe::default());
        let first = probe.begin();
        assert_eq!(first.connection(), Connection::Reused);
        let second = probe.begin();
        assert_eq!(first.connection(), Connection::Unknown, "it was shared");
        assert_eq!(second.connection(), Connection::Unknown);
        drop((first, second));

        let alone = probe.begin();
        assert_eq!(alone.connection(), Connection::Reused);
        drop(alone);
        assert_eq!(probe.state().active, 0);
    }

    #[test]
    fn the_connection_phases_fill_the_timing_without_inventing_any() {
        let timing = PhaseTiming {
            request_ms: 100,
            response_ms: 7,
            total_ms: 107,
            ..PhaseTiming::default()
        };
        let opened = Connection::Opened {
            dns: Some(Duration::from_micros(2_300)),
            connect: ms(30),
        };

        let https = opened.apply(true, false, timing);
        assert_eq!(https.dns_ms, PhaseValue::Ms(3), "rounded up");
        assert_eq!(
            https.connect_ms,
            PhaseValue::Ms(28),
            "the connector call minus DNS"
        );
        assert!(https.connect_includes_tls);
        assert_eq!(
            https.tls_ms,
            PhaseValue::NotMeasured,
            "never split from connect"
        );
        assert_eq!(https.wait_ms, Some(100 - 3 - 28));

        let plain = Connection::Opened {
            dns: None,
            connect: Duration::from_micros(120),
        }
        .apply(false, false, timing);
        assert_eq!(plain.dns_ms, PhaseValue::NotNeeded);
        assert_eq!(
            plain.connect_ms,
            PhaseValue::Ms(1),
            "a measured phase is never 0"
        );
        assert_eq!(plain.tls_ms, PhaseValue::NotNeeded);
        assert!(!plain.connect_includes_tls);
        assert_eq!(plain.wait_ms, Some(99));

        let reused = Connection::Reused.apply(true, false, timing);
        assert_eq!(
            (reused.dns_ms, reused.connect_ms, reused.tls_ms),
            (PhaseValue::Reused, PhaseValue::Reused, PhaseValue::Reused)
        );
        assert_eq!(reused.wait_ms, Some(100));

        // Through a proxy a new connection describes the proxy, so nothing is reported; so
        // does an unknown connection.
        let marked = PhaseTiming {
            phases_version: Some(1),
            ..timing
        };
        assert_eq!(opened.apply(true, true, timing), marked);
        assert_eq!(Connection::Unknown.apply(true, false, timing), marked);
        let json = serde_json::to_value(timing).unwrap();
        assert_eq!(
            json,
            serde_json::json!({ "request_ms": 100, "response_ms": 7, "total_ms": 107 }),
            "unmeasured phases are absent from the wire form"
        );
    }

    /// A connector that answers at once, successfully or not.
    #[derive(Clone)]
    struct ImmediateConnector {
        fail: bool,
    }

    impl Service<()> for ImmediateConnector {
        type Response = u8;
        type Error = String;
        type Future = Ready<Result<u8, String>>;

        fn poll_ready(&mut self, _: &mut Context<'_>) -> Poll<Result<(), String>> {
            Poll::Ready(Ok(()))
        }

        fn call(&mut self, (): ()) -> Self::Future {
            ready(if self.fail {
                Err("refused".into())
            } else {
                Ok(7)
            })
        }
    }

    #[tokio::test]
    async fn the_connector_layer_times_successful_connects_only() {
        let probe = Arc::new(ConnectProbe::default());
        let layer = ConnectTimer::new(Arc::clone(&probe));

        let mut refused = layer.layer(ImmediateConnector { fail: true });
        assert_eq!(refused.call(()).await, Err("refused".to_owned()));
        assert!(probe.state().connects.is_empty());

        let mut open = layer.layer(ImmediateConnector { fail: false });
        assert_eq!(open.call(()).await, Ok(7));
        assert_eq!(probe.state().connects.len(), 1);
    }

    #[tokio::test]
    async fn the_resolver_times_each_lookup_and_still_resolves() {
        let probe = Arc::new(ConnectProbe::default());
        let resolver = ProbedResolver::new(Arc::clone(&probe));

        let addrs: Vec<_> = resolver
            .resolve(Name::from_str("localhost").unwrap())
            .await
            .unwrap()
            .collect();

        assert!(addrs.iter().all(|address| address.ip().is_loopback()));
        assert!(!addrs.is_empty());
        assert_eq!(probe.state().resolves.len(), 1);
    }

    #[test]
    fn only_the_newest_spans_are_kept() {
        let probe = ConnectProbe::default();
        for _ in 0..KEPT_SPANS + 5 {
            ProbeState::keep(&mut probe.state().connects, span(Instant::now(), 1));
        }
        assert_eq!(probe.state().connects.len(), KEPT_SPANS);
    }
}
