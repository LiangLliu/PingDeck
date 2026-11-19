//! Tests of the connection facts: the SSRF filter of the resolver, the ledger's attribution of
//! lookups and connects, the connector layer and the timing of an attempt.

use std::net::SocketAddr;
use std::sync::Arc;
use std::time::Duration;

use super::*;

fn addrs(texts: &[&str]) -> Vec<SocketAddr> {
    texts
        .iter()
        .map(|text| SocketAddr::new(text.parse().unwrap(), 0))
        .collect()
}

#[test]
fn a_name_resolving_to_metadata_or_link_local_is_refused() {
    for answer in [
        "169.254.169.254",
        "fe80::1",
        "fd00:ec2::254",
        "100.100.100.200",
    ] {
        let error = refuse_forbidden_answers("evil.example", &addrs(&["93.184.216.34", answer]))
            .expect_err(answer);
        assert!(error.to_string().contains("evil.example"), "{error}");
    }
}

#[test]
fn lan_names_resolving_to_private_loopback_or_cgnat_pass() {
    for answer in [
        "192.168.1.20",
        "10.0.0.5",
        "127.0.0.1",
        "fd12::1",
        "100.64.0.9",
        "8.8.8.8",
    ] {
        refuse_forbidden_answers("ollama.lan", &addrs(&[answer])).expect(answer);
    }
}

#[test]
fn the_ledger_attributes_only_lookups_made_during_a_lone_attempt() {
    let ledger = Arc::new(ConnLedger::default());
    let domain = || Some(url::Host::Domain("A.test"));
    ledger.record_dns(
        "a.test",
        std::time::Instant::now(),
        Duration::from_millis(3),
    );
    let watch = ledger.watch(domain());
    assert_eq!(
        watch.dns_and_connect().0,
        PhaseValue::NotMeasured,
        "an earlier lookup belongs to nobody"
    );
    ledger.record_dns(
        "a.test",
        std::time::Instant::now(),
        Duration::from_millis(4),
    );
    assert_eq!(watch.dns_and_connect().0, PhaseValue::Ms(4));
    ledger.record_dns(
        "A.TEST",
        std::time::Instant::now(),
        Duration::from_micros(1500),
    );
    assert_eq!(
        watch.dns_and_connect().0,
        PhaseValue::Ms(6),
        "lookups add up (4 + 1.5 ms, rounded up), host names are case-blind"
    );
    let quick = ledger.watch(Some(url::Host::Domain("b.test")));
    ledger.record_dns(
        "b.test",
        std::time::Instant::now(),
        Duration::from_micros(40),
    );
    assert_eq!(
        quick.dns_and_connect().0,
        PhaseValue::Ms(1),
        "a measured lookup under a millisecond shows as 1, not 0"
    );
    // A second attempt that overlaps spoils the attribution of both.
    let other = ledger.watch(domain());
    assert_eq!(
        (watch.dns_and_connect().0, other.dns_and_connect().0),
        (PhaseValue::NotMeasured, PhaseValue::NotMeasured)
    );
    drop((watch, other));
    // With nobody running, the next attempt is alone again.
    let lone = ledger.watch(domain());
    ledger.record_dns(
        "a.test",
        std::time::Instant::now(),
        Duration::from_millis(2),
    );
    assert_eq!(lone.dns_and_connect().0, PhaseValue::Ms(2));
    let literal = ledger.watch(Some(url::Host::Ipv4(std::net::Ipv4Addr::LOCALHOST)));
    assert_eq!(literal.dns_and_connect().0, PhaseValue::NotNeeded);
    assert_eq!(
        ledger.watch(None).dns_and_connect().0,
        PhaseValue::NotMeasured
    );
}

#[test]
fn timing_never_turns_unknown_into_zero() {
    let unknown = ConnFacts::UNKNOWN.timing(None, PhaseValue::NotNeeded, 10);
    assert_eq!(unknown.dns, PhaseValue::NotMeasured);
    assert_eq!(unknown.headers_ms, PhaseValue::NotMeasured);
    assert_eq!(unknown.end_ms, PhaseValue::Ms(10));
    let measured = ConnFacts {
        dns: PhaseValue::Ms(7),
        connect: PhaseValue::Ms(12),
        reused: Some(false),
    }
    .timing(Some(40), PhaseValue::Ms(5), 60);
    assert_eq!(
        measured.headers_ms,
        PhaseValue::Ms(21),
        "headers exclude the DNS and connect time"
    );
    assert_eq!(measured.setup_ms(), 19);
    assert_eq!(measured.receive_ms(), Some(60 - 19 - 21 - 5));
    // Setup phases never add up to more than the time to headers.
    let squeezed = ConnFacts {
        dns: PhaseValue::Ms(7),
        connect: PhaseValue::Ms(12),
        reused: Some(false),
    }
    .timing(Some(10), PhaseValue::NotNeeded, 10);
    assert_eq!(
        (squeezed.dns, squeezed.connect, squeezed.headers_ms),
        (PhaseValue::Ms(7), PhaseValue::Ms(3), PhaseValue::Ms(0))
    );
    // A connect without a response head is shown; headers stay unknown.
    let failed = ConnFacts {
        dns: PhaseValue::NotNeeded,
        connect: PhaseValue::Ms(9),
        reused: None,
    }
    .timing(None, PhaseValue::NotNeeded, 12);
    assert_eq!(failed.connect, PhaseValue::Ms(9));
    assert_eq!(failed.headers_ms, PhaseValue::NotMeasured);
}

// ---- the connector layer

/// The request type of reqwest's connector: it prints as `Unnameable(<uri>)`.
struct Unnameable(&'static str);

impl std::fmt::Debug for Unnameable {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        write!(f, "Unnameable({})", self.0)
    }
}

/// A connector that takes `delay` to connect, or fails after it.
#[derive(Clone)]
struct Slow {
    delay: Duration,
    fail: bool,
}

impl tower::Service<Unnameable> for Slow {
    type Response = u8;
    type Error = &'static str;
    type Future =
        std::pin::Pin<Box<dyn std::future::Future<Output = Result<u8, &'static str>> + Send>>;

    fn poll_ready(
        &mut self,
        _: &mut std::task::Context<'_>,
    ) -> std::task::Poll<Result<(), Self::Error>> {
        std::task::Poll::Ready(Ok(()))
    }

    fn call(&mut self, _: Unnameable) -> Self::Future {
        let (delay, fail) = (self.delay, self.fail);
        Box::pin(async move {
            tokio::time::sleep(delay).await;
            if fail { Err("refused") } else { Ok(7) }
        })
    }
}

fn timed(delay: Duration, fail: bool, ledger: &Arc<ConnLedger>) -> TimedConnect<Slow> {
    use tower::Layer;
    ConnectTimer::new(Arc::clone(ledger)).layer(Slow { delay, fail })
}

#[tokio::test]
async fn the_layer_times_a_delayed_connect_net_of_the_name_lookup() {
    use tower::Service;
    let ledger = Arc::new(ConnLedger::default());
    let watch = ledger.watch(Some(url::Host::Domain("slow.test")));
    // The connector's own lookup happens inside the connect: 10 ms of the 80.
    ledger.record_dns(
        "slow.test",
        std::time::Instant::now(),
        Duration::from_millis(10),
    );
    let mut service = timed(Duration::from_millis(80), false, &ledger);
    assert_eq!(
        service.call(Unnameable("http://slow.test:8080/")).await,
        Ok(7)
    );
    let (dns, connect) = watch.dns_and_connect();
    assert_eq!(dns, PhaseValue::Ms(10));
    let PhaseValue::Ms(connect) = connect else {
        panic!("{connect:?}");
    };
    assert!(
        connect >= 70,
        "connect {connect} ms is at least the delay without the lookup"
    );
    assert!(connect < 1000);
}

#[tokio::test]
async fn the_layer_times_a_failed_connect_too_and_never_a_foreign_host() {
    use tower::Service;
    let ledger = Arc::new(ConnLedger::default());
    let literal = ledger.watch(Some(url::Host::Ipv4(std::net::Ipv4Addr::LOCALHOST)));
    let other = ledger.watch(Some(url::Host::Domain("other.test")));
    let mut service = timed(Duration::from_millis(30), true, &ledger);
    assert_eq!(
        service.call(Unnameable("https://127.0.0.1:443/")).await,
        Err("refused")
    );
    let (dns, connect) = literal.dns_and_connect();
    assert_eq!(dns, PhaseValue::NotNeeded);
    assert!(
        matches!(connect, PhaseValue::Ms(ms) if ms >= 30),
        "{connect:?}"
    );
    assert_eq!(other.dns_and_connect().1, PhaseValue::NotMeasured);
    assert_eq!(
        literal
            .connect_only()
            .map(|d| d >= Duration::from_millis(30)),
        Some(true)
    );
}

#[tokio::test]
async fn overlapping_attempts_to_one_host_do_not_claim_each_others_connects() {
    use tower::Service;
    let ledger = Arc::new(ConnLedger::default());
    let first = ledger.watch(Some(url::Host::Domain("busy.test")));
    let second = ledger.watch(Some(url::Host::Domain("busy.test")));
    ledger.record_dns(
        "busy.test",
        std::time::Instant::now(),
        Duration::from_millis(1),
    );
    let mut service = timed(Duration::from_millis(5), false, &ledger);
    service.call(Unnameable("http://busy.test/")).await.unwrap();
    assert_eq!(
        first.dns_and_connect(),
        (PhaseValue::NotMeasured, PhaseValue::NotMeasured)
    );
    assert_eq!(
        second.dns_and_connect(),
        (PhaseValue::NotMeasured, PhaseValue::NotMeasured)
    );
    assert_eq!(first.connect_only(), None);
}

#[tokio::test]
async fn a_connect_before_the_attempt_or_without_a_known_lookup_is_not_attributed() {
    use tower::Service;
    let ledger = Arc::new(ConnLedger::default());
    let mut service = timed(Duration::from_millis(5), false, &ledger);
    service.call(Unnameable("http://old.test/")).await.unwrap();
    let watch = ledger.watch(Some(url::Host::Domain("old.test")));
    assert_eq!(
        watch.dns_and_connect(),
        (PhaseValue::NotMeasured, PhaseValue::NotMeasured)
    );
    // A connect whose lookup was not seen is not measured: its time would contain an unknown lookup.
    service.call(Unnameable("http://old.test/")).await.unwrap();
    assert_eq!(watch.dns_and_connect().1, PhaseValue::NotMeasured);
    // A request the layer cannot name records nothing.
    service.call(Unnameable("not a uri")).await.unwrap();
}

#[test]
fn the_host_comes_from_the_connector_requests_debug_form() {
    for (text, host) in [
        ("http://Example.test:8080/v1", Some("example.test")),
        ("https://[::1]:443/", Some("::1")),
        ("http://127.0.0.1:9/", Some("127.0.0.1")),
        ("garbage", None),
    ] {
        assert_eq!(
            host_of_request(&Unnameable(text)).as_deref(),
            host,
            "{text}"
        );
    }
    assert_eq!(host_of_request(&"Unnameable-less"), None);
}
