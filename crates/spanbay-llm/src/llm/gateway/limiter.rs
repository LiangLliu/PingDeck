//! Rate limits of the gateway's failed-authentication path.
//!
//! Single responsibility: [`AuthThrottle`] remembers, per key, the failed attempts of the last
//! minute. The caller picks the key: on loopback (peer address, hash of the presented token), in
//! LAN mode the peer address; only a request that presents a token no active client owns is
//! counted, so a client with a valid token is never locked out by a stale one on the same peer.
//! From the 11th counted failure of a key in a minute the answer is 429 with `Retry-After`. Log
//! rows for failed authentication are written at most once per second per peer, so a scan
//! cannot flood the log. Time is passed in, so the rules are testable without sleeping.

use std::collections::{HashMap, VecDeque};
use std::net::IpAddr;
use std::sync::{Mutex, PoisonError};
use std::time::{Duration, Instant};

/// Failed attempts allowed per window before the next one is throttled.
pub(super) const MAX_FAILURES: usize = 10;
/// The failure window.
const WINDOW: Duration = Duration::from_secs(60);
/// Failed-authentication rows per peer: one per this interval.
pub(super) const ROW_INTERVAL: Duration = Duration::from_secs(1);
/// Keys (and peers) remembered; above it expired entries go first, then everything (a flood of
/// distinct addresses or tokens cannot grow the maps without bound).
const MAX_KEYS: usize = 4096;

/// What a failed attempt gets.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub(super) enum Verdict {
    /// Within the limit (or not counted): the normal 401.
    Unauthorized {
        /// A log row may be written for this attempt.
        log: bool,
    },
    /// Over the limit: 429 with this `Retry-After` (seconds).
    Throttled {
        /// Whole seconds until the oldest counted failure leaves the window (at least 1).
        retry_after_s: u64,
        /// A log row may be written for this attempt.
        log: bool,
    },
}

#[derive(Default)]
struct State {
    /// Counted failures per key.
    failures: HashMap<String, VecDeque<Instant>>,
    /// When each peer last got a log row.
    rows: HashMap<IpAddr, Instant>,
}

/// Failed-authentication bookkeeping.
pub(super) struct AuthThrottle {
    max_failures: usize,
    row_interval: Duration,
    state: Mutex<State>,
}

impl AuthThrottle {
    /// A throttle that answers the `max_failures + 1`-th counted failure of a key within a
    /// minute with 429 and allows a log row per peer every `row_interval`.
    pub(super) fn new(max_failures: usize, row_interval: Duration) -> Self {
        Self {
            max_failures,
            row_interval,
            state: Mutex::new(State::default()),
        }
    }

    /// Records a failed attempt of `peer` at `now` and says how to answer it. `key` is the
    /// counter the attempt belongs to, `None` for an attempt that is not counted (no token was
    /// presented). A throttled attempt is not counted again (the window does not extend while
    /// the caller keeps hammering).
    pub(super) fn failed(&self, peer: IpAddr, key: Option<&str>, now: Instant) -> Verdict {
        let mut state = self.state.lock().unwrap_or_else(PoisonError::into_inner);
        if state.rows.len() >= MAX_KEYS && !state.rows.contains_key(&peer) {
            state
                .rows
                .retain(|_, at| now.saturating_duration_since(*at) < self.row_interval);
            if state.rows.len() >= MAX_KEYS {
                state.rows.clear();
            }
        }
        let log = state
            .rows
            .get(&peer)
            .is_none_or(|at| now.saturating_duration_since(*at) >= self.row_interval);
        if log {
            state.rows.insert(peer, now);
        }
        let Some(key) = key else {
            return Verdict::Unauthorized { log };
        };
        if state.failures.len() >= MAX_KEYS && !state.failures.contains_key(key) {
            state.failures.retain(|_, failures| {
                failures
                    .back()
                    .is_some_and(|at| now.saturating_duration_since(*at) < WINDOW)
            });
            if state.failures.len() >= MAX_KEYS {
                state.failures.clear();
            }
        }
        let failures = state.failures.entry(key.to_owned()).or_default();
        while failures
            .front()
            .is_some_and(|at| now.saturating_duration_since(*at) >= WINDOW)
        {
            failures.pop_front();
        }
        if failures.len() >= self.max_failures {
            let oldest = failures.front().copied().unwrap_or(now);
            let left = WINDOW.saturating_sub(now.saturating_duration_since(oldest));
            return Verdict::Throttled {
                retry_after_s: (left.as_secs() + u64::from(left.subsec_nanos() > 0)).max(1),
                log,
            };
        }
        failures.push_back(now);
        Verdict::Unauthorized { log }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn peer(last: u8) -> IpAddr {
        IpAddr::from([10, 0, 0, last])
    }

    fn throttle() -> AuthThrottle {
        AuthThrottle::new(MAX_FAILURES, ROW_INTERVAL)
    }

    #[test]
    fn the_eleventh_counted_failure_in_a_minute_is_throttled() {
        let throttle = throttle();
        let start = Instant::now();
        for second in 0..10 {
            assert!(
                matches!(
                    throttle.failed(peer(1), Some("1#a"), start + Duration::from_secs(second)),
                    Verdict::Unauthorized { .. }
                ),
                "failure {} is a plain 401",
                second + 1
            );
        }
        let verdict = throttle.failed(peer(1), Some("1#a"), start + Duration::from_secs(10));
        assert!(
            matches!(
                verdict,
                Verdict::Throttled {
                    retry_after_s: 50,
                    ..
                }
            ),
            "{verdict:?}"
        );
        // Another key (another stale token, another peer) has its own counter.
        assert!(matches!(
            throttle.failed(peer(1), Some("1#b"), start),
            Verdict::Unauthorized { .. }
        ));
        assert!(matches!(
            throttle.failed(peer(2), Some("2#a"), start),
            Verdict::Unauthorized { .. }
        ));
    }

    #[test]
    fn attempts_without_a_key_are_never_counted() {
        let throttle = throttle();
        let start = Instant::now();
        for _ in 0..100 {
            assert!(matches!(
                throttle.failed(peer(1), None, start),
                Verdict::Unauthorized { .. }
            ));
        }
        assert!(matches!(
            throttle.failed(peer(1), Some("a"), start),
            Verdict::Unauthorized { .. }
        ));
    }

    #[test]
    fn the_window_slides_and_a_throttled_attempt_is_not_counted() {
        let throttle = throttle();
        let start = Instant::now();
        for _ in 0..10 {
            throttle.failed(peer(1), Some("a"), start);
        }
        for second in 1..30 {
            assert!(matches!(
                throttle.failed(peer(1), Some("a"), start + Duration::from_secs(second)),
                Verdict::Throttled { .. }
            ));
        }
        // 60 s after the first ten, none of them counts any more.
        assert!(matches!(
            throttle.failed(peer(1), Some("a"), start + WINDOW),
            Verdict::Unauthorized { .. }
        ));
    }

    #[test]
    fn rows_are_allowed_once_per_second_per_peer_whatever_the_key() {
        let throttle = throttle();
        let start = Instant::now();
        let logged = |verdict: Verdict| match verdict {
            Verdict::Unauthorized { log } | Verdict::Throttled { log, .. } => log,
        };
        assert!(logged(throttle.failed(peer(1), Some("a"), start)));
        assert!(!logged(throttle.failed(
            peer(1),
            Some("b"),
            start + Duration::from_millis(400)
        )));
        assert!(!logged(throttle.failed(
            peer(1),
            None,
            start + Duration::from_millis(900)
        )));
        assert!(logged(throttle.failed(
            peer(1),
            Some("a"),
            start + Duration::from_millis(1000)
        )));
        assert!(
            logged(throttle.failed(peer(2), Some("a"), start)),
            "per peer"
        );
    }

    #[test]
    fn the_tables_stay_bounded() {
        let throttle = throttle();
        let now = Instant::now();
        for index in 0..=u32::try_from(MAX_KEYS).unwrap() + 10 {
            throttle.failed(
                IpAddr::from(index.to_be_bytes()),
                Some(&index.to_string()),
                now,
            );
        }
        let state = throttle.state.lock().unwrap();
        assert!(state.failures.len() <= MAX_KEYS && state.rows.len() <= MAX_KEYS);
    }
}
