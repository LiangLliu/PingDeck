//! Runtime health of upstream keys: a circuit breaker and moving averages.
//!
//! Single responsibility: remember how each `(key, model)` pair has been behaving, so routing can
//! skip a pair that keeps failing and prefer the one that answers fast. Everything is in memory
//! (it describes the current session) and cheap: short mutex sections, never held across an
//! await.
//!
//! The breaker counts *timestamped* failures: it opens after
//! `failure_threshold` consecutive failures inside the rolling `window` (three spread over 40 s do
//! not open a 30 s window; a success clears the count), stays open for the cooldown, then admits
//! exactly one probe request (half-open). A probe failure reopens it with the cooldown doubled,
//! capped at `max(open_max, open_for)` (30, 60, 120, 240, 300 s with the default config); a probe
//! success closes it and resets the cooldown.
//! A client error says nothing about the upstream's health and counts as a success; a rejected
//! credential that triggered key rotation counts as a failure (the caller decides, see
//! [`Outcome`]). The thresholds are passed with every call: the gateway passes the values it
//! started with, the in-process client the live settings. Probes never touch the book.

use std::collections::{HashMap, VecDeque};
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde::{Deserialize, Serialize};

/// Weight of the newest sample in the moving averages.
const EWMA_ALPHA: f64 = 0.3;

/// Failures remembered per pair at most (the window normally keeps far fewer). Far above the
/// largest threshold the settings allow (`ai.gateway.circuit_failure_threshold` is at most 100).
const MAX_REMEMBERED_FAILURES: usize = 1024;

/// Circuit state as shown to the user.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CircuitState {
    /// Requests flow.
    Closed,
    /// Requests are refused until the open time has passed.
    Open,
    /// One probe request is allowed (or in flight).
    HalfOpen,
}

/// Thresholds of the breaker (from the gateway settings; the defaults are theirs).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct CircuitConfig {
    /// Consecutive failures inside `window` that open the circuit.
    pub failure_threshold: u32,
    /// The rolling window the failures are counted over.
    pub window: Duration,
    /// How long it stays open the first time.
    pub open_for: Duration,
    /// The longest an open circuit backs off to (the doubled cooldown never exceeds it, nor
    /// `open_for` when that is larger).
    pub open_max: Duration,
}

impl Default for CircuitConfig {
    fn default() -> Self {
        Self {
            failure_threshold: 3,
            window: Duration::from_secs(30),
            open_for: Duration::from_secs(30),
            open_max: Duration::from_secs(300),
        }
    }
}

/// What a finished attempt means for health.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Outcome {
    /// The upstream answered (a client error counts too).
    Success,
    /// A retryable failure, or a rejected credential that triggered key rotation: counts toward
    /// opening the circuit.
    Failure,
    /// The client went away: neither success nor failure; frees a half-open probe.
    Canceled,
}

/// Permission to attempt a pair, from [`HealthBook::acquire_noting`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Permit {
    /// Go ahead.
    Allowed,
    /// The circuit is open, or a half-open probe is already in flight.
    Blocked,
}

/// Health figures of one pair, for display.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HealthSnapshot {
    /// Key id.
    pub key_id: String,
    /// Model id.
    pub model_id: String,
    /// Circuit state.
    pub state: CircuitState,
    /// Failures counted toward opening (consecutive, inside the window at the last failure).
    pub consecutive_failures: u32,
    /// Moving average of the failure rate, 0 to 1.
    pub failure_rate: f64,
    /// Moving average of latency in milliseconds.
    pub latency_ms: f64,
    /// Requests recorded.
    pub total: u64,
    /// When the circuit may be probed again, Unix ms; `Some` while open.
    pub open_until_ms: Option<i64>,
    /// The cooldown of the current (or next) open period in seconds: doubles from `open_for` up
    /// to `max(open_max, open_for)`; 30, 60, 120, 240, 300 with the defaults.
    pub cooldown_s: u32,
    /// Whole seconds until the probe, rounded up and at least 1; `Some` while open.
    pub retry_in_s: Option<u64>,
    /// Milliseconds since the circuit opened; `Some` while open.
    pub opened_for_ms: Option<u64>,
    /// The words of the UI: "Healthy", "Recovering" or "Circuit open, retry in N s".
    pub label: String,
}

#[derive(Debug)]
struct Entry {
    state: CircuitState,
    /// Failures of the current streak, oldest first.
    failures: VecDeque<Instant>,
    opened_at: Option<Instant>,
    /// Rest of the current open period; doubles on every failed probe.
    cooldown: Duration,
    probe_in_flight: bool,
    failure_ewma: f64,
    latency_ewma: f64,
    total: u64,
}

impl Entry {
    fn new(config: CircuitConfig) -> Self {
        Self {
            state: CircuitState::Closed,
            failures: VecDeque::new(),
            opened_at: None,
            cooldown: config.open_for,
            probe_in_flight: false,
            failure_ewma: 0.0,
            latency_ewma: 0.0,
            total: 0,
        }
    }

    /// Time left of the open period; zero once it has passed (a probe may go through).
    fn remaining(&self, now: Instant) -> Duration {
        self.opened_at.map_or(Duration::ZERO, |at| {
            self.cooldown
                .saturating_sub(now.saturating_duration_since(at))
        })
    }

    /// How long a caller should wait before this pair can serve again; `None` when it can now.
    fn blocked_for(&self, now: Instant) -> Option<Duration> {
        match self.state {
            CircuitState::Closed => None,
            CircuitState::Open => {
                let left = self.remaining(now);
                (!left.is_zero()).then_some(left)
            }
            CircuitState::HalfOpen => self.probe_in_flight.then_some(Duration::from_secs(1)),
        }
    }

    /// Failures counted toward opening, saturated.
    fn streak_len(&self) -> u32 {
        u32::try_from(self.failures.len()).unwrap_or(u32::MAX)
    }

    /// Asks to attempt the pair; also says whether an open circuit just turned half-open.
    fn try_acquire(&mut self, now: Instant) -> (Permit, bool) {
        match self.state {
            CircuitState::Closed => (Permit::Allowed, false),
            CircuitState::Open if self.remaining(now).is_zero() => {
                self.state = CircuitState::HalfOpen;
                self.probe_in_flight = true;
                (Permit::Allowed, true)
            }
            CircuitState::Open => (Permit::Blocked, false),
            CircuitState::HalfOpen if self.probe_in_flight => (Permit::Blocked, false),
            CircuitState::HalfOpen => {
                self.probe_in_flight = true;
                (Permit::Allowed, false)
            }
        }
    }

    fn open(&mut self, now: Instant) {
        self.state = CircuitState::Open;
        self.opened_at = Some(now);
    }

    fn fail(&mut self, now: Instant, config: CircuitConfig) {
        while self
            .failures
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) > config.window)
        {
            self.failures.pop_front();
        }
        if self.failures.len() >= MAX_REMEMBERED_FAILURES {
            self.failures.pop_front();
        }
        self.failures.push_back(now);
        match self.state {
            CircuitState::HalfOpen => {
                let ceiling = config.open_max.max(config.open_for);
                self.cooldown = self.cooldown.saturating_mul(2).min(ceiling);
                self.open(now);
            }
            CircuitState::Closed if self.streak_len() >= config.failure_threshold => {
                self.cooldown = config.open_for;
                self.open(now);
            }
            // Open: a failure of an attempt that started before the circuit opened changes nothing.
            CircuitState::Closed | CircuitState::Open => {}
        }
    }

    fn succeed(&mut self, config: CircuitConfig) {
        self.failures.clear();
        self.state = CircuitState::Closed;
        self.opened_at = None;
        self.cooldown = config.open_for;
    }

    /// Records how an attempt ended; returns whether the circuit state changed.
    fn record(
        &mut self,
        outcome: Outcome,
        latency: Duration,
        now: Instant,
        config: CircuitConfig,
    ) -> bool {
        let before = self.state;
        match outcome {
            Outcome::Canceled => {
                self.probe_in_flight = false;
                return false;
            }
            Outcome::Success => self.succeed(config),
            Outcome::Failure => self.fail(now, config),
        }
        let changed = self.state != before;
        self.note_sample(outcome, latency);
        changed
    }

    /// Frees the probe slot and moves the request count and the averages.
    fn note_sample(&mut self, outcome: Outcome, latency: Duration) {
        self.probe_in_flight = false;
        let first = self.total == 0;
        self.total += 1;
        let failed = if outcome == Outcome::Failure {
            1.0
        } else {
            0.0
        };
        self.failure_ewma = average(self.failure_ewma, failed, first);
        if outcome == Outcome::Success {
            // `0.0` means "no sample yet" on purpose: after a 0 ms success the next sample starts
            // the average afresh. Do not turn it into an `Option<f64>`.
            self.latency_ewma = average(
                self.latency_ewma,
                latency.as_secs_f64() * 1000.0,
                self.latency_ewma == 0.0,
            );
        }
    }

    /// The display row of this pair at `now` (`wall_ms`: the same instant in Unix ms).
    fn snapshot(&self, key_id: &str, model_id: &str, now: Instant, wall_ms: i64) -> HealthSnapshot {
        let open = self.state == CircuitState::Open;
        let left = self.remaining(now);
        let retry_in_s = open.then(|| ceil_secs(left).max(1));
        let left_ms = i64::try_from(left.as_millis()).unwrap_or(i64::MAX);
        HealthSnapshot {
            key_id: key_id.to_owned(),
            model_id: model_id.to_owned(),
            state: self.state,
            consecutive_failures: self.streak_len(),
            failure_rate: self.failure_ewma,
            latency_ms: self.latency_ewma,
            total: self.total,
            open_until_ms: open.then(|| wall_ms.saturating_add(left_ms)),
            cooldown_s: u32::try_from(self.cooldown.as_secs()).unwrap_or(u32::MAX),
            retry_in_s,
            opened_for_ms: self.opened_at.filter(|_| open).map(|at| {
                u64::try_from(now.saturating_duration_since(at).as_millis()).unwrap_or(u64::MAX)
            }),
            label: state_label(self.state, retry_in_s),
        }
    }
}

/// The pairs, by key id then model id (lookups by `&str` without allocating).
type Pairs = HashMap<String, HashMap<String, Entry>>;

/// All pairs' health, shared by the gateway and the client API.
#[derive(Debug, Default)]
pub struct HealthBook {
    entries: Mutex<Pairs>,
    /// Test clock offset: tests move time forward instead of sleeping.
    #[cfg(any(test, feature = "test-support"))]
    skew_ms: std::sync::atomic::AtomicU64,
    /// Optional frozen monotonic base for deterministic unit tests.
    #[cfg(any(test, feature = "test-support"))]
    fixed_now: Option<Instant>,
    /// Optional frozen wall-clock base (Unix ms) paired with `fixed_now`.
    #[cfg(any(test, feature = "test-support"))]
    fixed_wall_ms: Option<i64>,
}

fn average(previous: f64, sample: f64, first: bool) -> f64 {
    if first {
        sample
    } else {
        EWMA_ALPHA * sample + (1.0 - EWMA_ALPHA) * previous
    }
}

/// `duration` in whole seconds, rounded up.
fn ceil_secs(duration: Duration) -> u64 {
    duration.as_secs() + u64::from(duration.subsec_nanos() > 0)
}

fn state_label(state: CircuitState, retry_in_s: Option<u64>) -> String {
    match (state, retry_in_s) {
        (CircuitState::Closed, _) => "Healthy".to_owned(),
        (CircuitState::HalfOpen, _) => "Recovering".to_owned(),
        (CircuitState::Open, Some(seconds)) => format!("Circuit open, retry in {seconds} s"),
        (CircuitState::Open, None) => "Circuit open".to_owned(),
    }
}

/// The value under `key`, inserted with `make` first when missing. Allocates the key only on a
/// miss. `None` never happens; it spares an `expect`.
fn get_or_insert<'a, V>(
    map: &'a mut HashMap<String, V>,
    key: &str,
    make: impl FnOnce() -> V,
) -> Option<&'a mut V> {
    if !map.contains_key(key) {
        map.insert(key.to_owned(), make());
    }
    map.get_mut(key)
}

impl HealthBook {
    /// An empty book.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty book with a deterministic frozen clock (advances only via [`HealthBook::advance`]).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_fixed_clock() -> Self {
        Self {
            fixed_now: Some(Instant::now()),
            fixed_wall_ms: Some(1_700_000_000_000),
            ..Self::default()
        }
    }

    #[cfg(any(test, feature = "test-support"))]
    fn skew(&self) -> Duration {
        Duration::from_millis(self.skew_ms.load(std::sync::atomic::Ordering::Relaxed))
    }

    /// The current instant; tests shift it with [`HealthBook::advance`].
    fn now(&self) -> Instant {
        #[cfg(any(test, feature = "test-support"))]
        let now = self.fixed_now.unwrap_or_else(Instant::now) + self.skew();
        #[cfg(not(any(test, feature = "test-support")))]
        let now = Instant::now();
        now
    }

    /// The current wall clock in Unix ms, shifted like [`HealthBook::now`].
    fn wall_ms(&self) -> i64 {
        #[cfg(any(test, feature = "test-support"))]
        let now = self
            .fixed_wall_ms
            .unwrap_or_else(pingdeck_base::time::now_ms)
            .saturating_add(
                i64::try_from(self.skew_ms.load(std::sync::atomic::Ordering::Relaxed))
                    .unwrap_or(i64::MAX),
            );
        #[cfg(not(any(test, feature = "test-support")))]
        let now = pingdeck_base::time::now_ms();
        now
    }

    /// Moves the book's clock forward, so open circuits time out without sleeping.
    #[cfg(any(test, feature = "test-support"))]
    pub fn advance(&self, by: Duration) {
        let millis = u64::try_from(by.as_millis()).unwrap_or(u64::MAX);
        self.skew_ms
            .fetch_add(millis, std::sync::atomic::Ordering::Relaxed);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, Pairs> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// [`HealthBook::acquire_noting`] without the flag: an open circuit whose cooldown has passed
    /// still turns half-open and the probe slot is still taken.
    #[cfg(any(test, feature = "test-support"))]
    pub fn acquire(&self, key_id: &str, model_id: &str) -> Permit {
        self.acquire_noting(key_id, model_id).0
    }

    /// Asks to attempt `(key, model)`. An open circuit whose cooldown has passed turns half-open
    /// and admits exactly one probe; a caller that gets [`Permit::Allowed`] must report an
    /// [`Outcome`]. Also says whether the call turned an open circuit half-open (a state change
    /// the caller announces, see `LlmEvent::HealthChanged`).
    pub fn acquire_noting(&self, key_id: &str, model_id: &str) -> (Permit, bool) {
        let now = self.now();
        self.lock()
            .get_mut(key_id)
            .and_then(|models| models.get_mut(model_id))
            .map_or((Permit::Allowed, false), |entry| entry.try_acquire(now))
    }

    /// How long until `(key, model)` can serve again, without taking a probe slot; `None` when
    /// it can now (closed, or open with its cooldown over).
    #[must_use]
    pub fn blocked_for(&self, key_id: &str, model_id: &str) -> Option<Duration> {
        let now = self.now();
        self.lock()
            .get(key_id)
            .and_then(|models| models.get(model_id))
            .and_then(|entry| entry.blocked_for(now))
    }

    /// Whether `(key, model)` would currently be refused, without taking a probe slot.
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn is_blocked(&self, key_id: &str, model_id: &str) -> bool {
        self.blocked_for(key_id, model_id).is_some()
    }

    /// The smallest remaining cooldown, in whole seconds and at least 1, among the blocked pairs
    /// of `pairs` (`(key id, model id)`); `None` when none of them is blocked.
    #[must_use]
    pub fn earliest_retry_s(&self, pairs: &[(&str, &str)]) -> Option<u64> {
        pairs
            .iter()
            .filter_map(|(key, model)| self.blocked_for(key, model))
            .map(|left| ceil_secs(left).max(1))
            .min()
    }

    /// Records how an attempt ended. Returns whether the pair's circuit state changed (opened,
    /// reopened after a failed probe, or closed), which the caller announces (see
    /// `LlmEvent::HealthChanged`).
    pub fn record(
        &self,
        key_id: &str,
        model_id: &str,
        outcome: Outcome,
        latency: Duration,
        config: CircuitConfig,
    ) -> bool {
        let now = self.now();
        let mut entries = self.lock();
        if outcome == Outcome::Canceled {
            // A cancelled attempt only frees a probe slot: it never creates a pair.
            return entries
                .get_mut(key_id)
                .and_then(|models| models.get_mut(model_id))
                .is_some_and(|entry| entry.record(outcome, latency, now, config));
        }
        get_or_insert(&mut entries, key_id, HashMap::new)
            .and_then(|models| get_or_insert(models, model_id, || Entry::new(config)))
            .is_some_and(|entry| entry.record(outcome, latency, now, config))
    }

    /// Moving-average failure rate of a pair (0 when unknown), for tests only (test-support);
    /// production reads [`HealthSnapshot::failure_rate`].
    #[cfg(any(test, feature = "test-support"))]
    #[must_use]
    pub fn failure_rate(&self, key_id: &str, model_id: &str) -> f64 {
        self.lock()
            .get(key_id)
            .and_then(|models| models.get(model_id))
            .map_or(0.0, |entry| entry.failure_ewma)
    }

    /// Resets the pairs of one key, or every pair when `key_id` is `None`.
    pub fn reset(&self, key_id: Option<&str>) {
        let mut entries = self.lock();
        match key_id {
            Some(key_id) => {
                entries.remove(key_id);
            }
            None => entries.clear(),
        }
    }

    /// The rows of the pairs `keep` accepts, ordered by key then model.
    fn rows(&self, keep: impl Fn(&Entry) -> bool) -> Vec<HealthSnapshot> {
        let (now, wall) = (self.now(), self.wall_ms());
        let mut rows: Vec<HealthSnapshot> = self
            .lock()
            .iter()
            .flat_map(|(key_id, models)| {
                models
                    .iter()
                    .filter(|(_, entry)| keep(entry))
                    .map(|(model_id, entry)| entry.snapshot(key_id, model_id, now, wall))
            })
            .collect();
        rows.sort_by(|a, b| (&a.key_id, &a.model_id).cmp(&(&b.key_id, &b.model_id)));
        rows
    }

    /// Every pair seen so far, ordered by key then model.
    #[must_use]
    pub fn snapshot(&self) -> Vec<HealthSnapshot> {
        self.rows(|_| true)
    }

    /// The pairs whose circuit is open, optionally only those of keys `key_filter` accepts (the
    /// caller maps a provider to its key ids), ordered by key then model. The filter runs after
    /// the book's lock is released.
    #[must_use]
    pub fn open_circuits(&self, key_filter: Option<&dyn Fn(&str) -> bool>) -> Vec<HealthSnapshot> {
        let mut rows = self.rows(|entry| entry.state == CircuitState::Open);
        if let Some(accepts) = key_filter {
            rows.retain(|row| accepts(&row.key_id));
        }
        rows
    }
}

#[cfg(test)]
mod tests;
