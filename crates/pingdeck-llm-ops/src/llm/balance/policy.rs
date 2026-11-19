//! The pure policy around balance readings: refresh schedule, throttle and keep-last-good.
//! Decisions only: no timers, no clock reads (every function takes `now_ms`), no storage; the
//! use-case layer persists [`StoredBalance`] in `llm_keys.balance_json` / `balance_error_json`.

use std::collections::HashMap;

use super::BalanceOutcome;
use super::error::{BalanceError, BalanceErrorKind};
pub(super) use pingdeck_llm_domain::llm::balance_state::{BalanceErrorRecord, StoredBalance};

/// The same key's balance endpoint is called at most once per this long.
pub(super) const THROTTLE_MS: i64 = 30_000;
/// Milliseconds per minute.
const MS_PER_MINUTE: i64 = 60_000;
/// Opening a provider's Overview refreshes when the last attempt is older than this.
pub(super) const OVERVIEW_REFRESH_AFTER_MS: i64 = 5 * MS_PER_MINUTE;
/// The smallest automatic interval in minutes (`ai.providers.balance_refresh_min`).
const MIN_INTERVAL_MIN: u16 = 5;
/// After a failure that may clear by itself (timeout, 5xx, network, 429) the automatic refresh
/// asks again this soon, even when its interval is longer.
const TRANSIENT_RETRY_MS: i64 = 5 * MS_PER_MINUTE;
/// After a failure only the user can fix (rejected key, missing endpoint, unexpected answer,
/// blocked address, console token) the automatic refresh waits at least this long.
const PERMANENT_RETRY_MS: i64 = 24 * 60 * MS_PER_MINUTE;

/// The answer of [`BalanceThrottle::try_acquire`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ThrottleVerdict {
    /// Go ahead; the attempt is recorded.
    Proceed,
    /// The key was called too recently: show the stored report with `throttled: true`.
    Throttled {
        /// When the key was last called, Unix milliseconds.
        last_ms: i64,
        /// Milliseconds until the next call is allowed.
        retry_after_ms: i64,
    },
}

/// Remembers the last call per key (in memory only).
#[derive(Debug, Default)]
pub struct BalanceThrottle {
    last: HashMap<String, i64>,
}

impl BalanceThrottle {
    /// Records an attempt for `key_id` at `now_ms` unless one happened within 30 seconds. A clock
    /// that moved backwards never blocks.
    pub fn try_acquire(&mut self, key_id: &str, now_ms: i64) -> ThrottleVerdict {
        self.last
            .retain(|_, at| now_ms.saturating_sub(*at) < THROTTLE_MS && *at <= now_ms);
        if let Some(&last_ms) = self.last.get(key_id) {
            return ThrottleVerdict::Throttled {
                last_ms,
                retry_after_ms: THROTTLE_MS - (now_ms - last_ms),
            };
        }
        self.last.insert(key_id.to_owned(), now_ms);
        ThrottleVerdict::Proceed
    }
}

fn error_record(error: &BalanceError, now_ms: i64) -> BalanceErrorRecord {
    BalanceErrorRecord {
        at_ms: now_ms,
        kind: error.kind,
        status: error.status,
        message: error.message.clone(),
        retry_at_ms: error
            .retry_after_ms
            .and_then(|wait| i64::try_from(wait).ok())
            .map(|wait| now_ms.saturating_add(wait)),
    }
}

/// The keep-last-good fold of a refresh outcome into the stored balance of a key.
pub trait StoredBalanceMerge {
    /// Folds a refresh `outcome` into the `previous` stored state.
    ///
    /// - A report replaces the last good value and clears the error.
    /// - A failure keeps the last good value, records the error and marks it stale. A cancel is
    ///   not a failure and changes nothing.
    /// - "No adapter" means there is nothing to refresh any more (the custom query was removed
    ///   and no adapter applies): the stored value is dropped.
    #[must_use]
    fn merge(previous: &Self, outcome: &BalanceOutcome, now_ms: i64) -> Self;
}

impl StoredBalanceMerge for StoredBalance {
    fn merge(previous: &Self, outcome: &BalanceOutcome, now_ms: i64) -> Self {
        match outcome {
            BalanceOutcome::Report { report, .. } => Self {
                last_good: Some(report.clone()),
                error: None,
                stale: false,
            }
            .settled(now_ms),
            BalanceOutcome::NoAdapter => Self::default(),
            BalanceOutcome::Failed { error, .. } if error.kind == BalanceErrorKind::Cancelled => {
                previous.clone().settled(now_ms)
            }
            BalanceOutcome::Failed { error, .. } => Self {
                last_good: previous.last_good.clone(),
                error: Some(error_record(error, now_ms)),
                stale: true,
            },
        }
    }
}

/// Why a refresh is being considered.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum RefreshTrigger {
    /// The provider's Overview opened.
    OverviewOpened,
    /// The user clicked "Refresh balance".
    Manual,
    /// The automatic interval elapsed.
    Interval,
    /// The provider was just added (from the sniff).
    AfterAdd,
}

/// The facts [`refresh_due`] decides on.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct RefreshInput {
    /// Why the refresh is considered.
    pub trigger: RefreshTrigger,
    /// Now, Unix milliseconds.
    pub now_ms: i64,
    /// When the balance was last attempted (success or failure).
    pub last_attempt_ms: Option<i64>,
    /// `ai.providers.balance_refresh_min`: `0` is off, otherwise 5 to 1440.
    pub interval_min: u16,
    /// Whether the provider has an adapter or a custom query.
    pub has_adapter: bool,
    /// Whether the provider is enabled.
    pub enabled: bool,
    /// The class of the key's last failure; `None` after a success or before any attempt.
    pub last_error: Option<BalanceErrorKind>,
    /// The service asked not to be called again before this instant (`Retry-After`).
    pub retry_at_ms: Option<i64>,
}

/// Whether a balance request should be sent. Opening the module alone is not a trigger,
/// so nothing is sent for it. A manual click is further limited by [`BalanceThrottle`].
///
/// The automatic interval backs off by the last failure: a failure that may clear by itself is
/// asked again after five minutes, one only the user can fix after a day at the soonest, and a
/// `Retry-After` is honoured by the interval and the Overview rule alike.
#[must_use]
pub fn refresh_due(input: &RefreshInput) -> bool {
    if !input.has_adapter {
        return false;
    }
    let age = input
        .last_attempt_ms
        .map(|last| input.now_ms.saturating_sub(last));
    let allowed = input.retry_at_ms.is_none_or(|at| input.now_ms >= at);
    match input.trigger {
        RefreshTrigger::Manual | RefreshTrigger::AfterAdd => true,
        RefreshTrigger::OverviewOpened => {
            allowed && age.is_none_or(|age| age > OVERVIEW_REFRESH_AFTER_MS)
        }
        RefreshTrigger::Interval => {
            if !input.enabled || input.interval_min == 0 || !allowed {
                return false;
            }
            let interval = i64::from(input.interval_min.max(MIN_INTERVAL_MIN)) * MS_PER_MINUTE;
            let wait = match input.last_error {
                None => interval,
                Some(kind) if kind.is_transient() => interval.min(TRANSIENT_RETRY_MS),
                Some(_) => interval.max(PERMANENT_RETRY_MS),
            };
            age.is_none_or(|age| age >= wait)
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::balance::{BalanceReport, BalanceUnit};

    const MIN: i64 = 60_000;

    fn report(checked_at_ms: i64) -> BalanceReport {
        BalanceReport {
            supported: true,
            unit: BalanceUnit::Cny,
            remaining: Some(86.4),
            checked_at_ms,
            ..BalanceReport::default()
        }
    }

    fn good(checked_at_ms: i64) -> BalanceOutcome {
        BalanceOutcome::Report {
            report: report(checked_at_ms),
            requests_used: 1,
        }
    }

    fn failed(kind: BalanceErrorKind, status: Option<u16>) -> BalanceOutcome {
        BalanceOutcome::Failed {
            error: BalanceError {
                kind,
                status,
                message: "boom".to_owned(),
                endpoint: Some("/user/balance".to_owned()),
                retry_after_ms: None,
            },
            requests_used: 1,
        }
    }

    #[test]
    fn two_clicks_ten_seconds_apart_produce_one_request() {
        let mut throttle = BalanceThrottle::default();
        assert_eq!(throttle.try_acquire("k1", 1_000), ThrottleVerdict::Proceed);
        assert_eq!(
            throttle.try_acquire("k1", 11_000),
            ThrottleVerdict::Throttled {
                last_ms: 1_000,
                retry_after_ms: 20_000
            }
        );
        assert_eq!(
            throttle.try_acquire("k2", 11_000),
            ThrottleVerdict::Proceed,
            "keys are independent"
        );
        assert_eq!(
            throttle.try_acquire("k1", 30_999),
            ThrottleVerdict::Throttled {
                last_ms: 1_000,
                retry_after_ms: 1
            }
        );
        assert_eq!(throttle.try_acquire("k1", 31_000), ThrottleVerdict::Proceed);
    }

    #[test]
    fn a_clock_that_moves_backwards_never_blocks() {
        let mut throttle = BalanceThrottle::default();
        assert_eq!(throttle.try_acquire("k", 100_000), ThrottleVerdict::Proceed);
        assert_eq!(throttle.try_acquire("k", 50_000), ThrottleVerdict::Proceed);
    }

    #[test]
    fn a_success_replaces_the_value_and_clears_the_error() {
        let failing = StoredBalance::merge(
            &StoredBalance::default(),
            &failed(BalanceErrorKind::Transient, None),
            10,
        );
        assert!(failing.error.is_some() && failing.stale && failing.last_good.is_none());
        let stored = StoredBalance::merge(&failing, &good(1_000), 1_000);
        assert_eq!(stored.last_good, Some(report(1_000)));
        assert_eq!(stored.error, None);
        assert!(!stored.stale);
    }

    #[test]
    fn no_error_class_clears_the_last_good_value() {
        let base = StoredBalance::merge(&StoredBalance::default(), &good(1_000), 1_000);
        for (kind, status) in [
            (BalanceErrorKind::Transient, Some(502)),
            (BalanceErrorKind::Network, None),
            (BalanceErrorKind::RateLimited, Some(429)),
            (BalanceErrorKind::Auth, Some(401)),
            (BalanceErrorKind::Auth, Some(403)),
            (BalanceErrorKind::NotFound, Some(404)),
            (BalanceErrorKind::SchemaMismatch, Some(200)),
            (BalanceErrorKind::Blocked, None),
            (BalanceErrorKind::NeedsConsoleToken, None),
            (BalanceErrorKind::ConsoleTokenExpired, Some(401)),
        ] {
            let merged = StoredBalance::merge(&base, &failed(kind, status), 2_000);
            assert_eq!(merged.last_good, base.last_good, "{kind:?}");
            assert!(merged.stale, "{kind:?}");
            let record = merged
                .error
                .unwrap_or_else(|| panic!("{kind:?} has no record"));
            assert_eq!(
                (record.at_ms, record.kind, record.status),
                (2_000, kind, status)
            );
        }
    }

    #[test]
    fn a_cancel_changes_nothing() {
        let base = StoredBalance::merge(&StoredBalance::default(), &good(1_000), 1_000);
        let merged = StoredBalance::merge(&base, &failed(BalanceErrorKind::Cancelled, None), 5_000);
        assert_eq!(merged, base);
    }

    #[test]
    fn no_adapter_drops_the_stored_value() {
        let base = StoredBalance::merge(&StoredBalance::default(), &good(1_000), 1_000);
        assert_eq!(
            StoredBalance::merge(&base, &BalanceOutcome::NoAdapter, 2_000),
            StoredBalance::default()
        );
    }

    #[test]
    fn sixty_one_minutes_is_stale_and_fifty_nine_is_not() {
        let stored = StoredBalance::merge(&StoredBalance::default(), &good(0), 0);
        // The merge snapshot follows the stale rule for an old report.
        assert!(StoredBalance::merge(&stored, &good(0), 61 * MIN).stale);
        assert!(!StoredBalance::merge(&stored, &good(0), 59 * MIN).stale);
    }

    #[test]
    fn the_stored_form_round_trips_through_json() {
        let stored = StoredBalance::merge(
            &StoredBalance::merge(&StoredBalance::default(), &good(1), 1),
            &failed(BalanceErrorKind::NotFound, Some(404)),
            2,
        );
        let json = serde_json::to_string(&stored).unwrap_or_default();
        assert_eq!(
            serde_json::from_str::<StoredBalance>(&json).ok(),
            Some(stored)
        );
    }

    fn due(trigger: RefreshTrigger, last_attempt_ms: Option<i64>, interval_min: u16) -> bool {
        refresh_due(&RefreshInput {
            trigger,
            now_ms: 100 * MIN,
            last_attempt_ms,
            interval_min,
            has_adapter: true,
            enabled: true,
            last_error: None,
            retry_at_ms: None,
        })
    }

    fn due_after(last_error: BalanceErrorKind, minutes_ago: i64, retry_at_ms: Option<i64>) -> bool {
        refresh_due(&RefreshInput {
            trigger: RefreshTrigger::Interval,
            now_ms: 100 * MIN,
            last_attempt_ms: Some(100 * MIN - minutes_ago * MIN),
            interval_min: 60,
            has_adapter: true,
            enabled: true,
            last_error: Some(last_error),
            retry_at_ms,
        })
    }

    #[test]
    fn the_interval_backs_off_by_the_kind_of_failure() {
        // A transient failure is asked again after five minutes, not a full hour.
        assert!(!due_after(BalanceErrorKind::Transient, 4, None));
        assert!(due_after(BalanceErrorKind::Network, 5, None));
        // A failure only the user can fix waits a day, not the hourly interval.
        assert!(!due_after(BalanceErrorKind::Auth, 90, None));
        assert!(due_after(BalanceErrorKind::NotFound, 24 * 60, None));
        // A Retry-After holds back even a transient retry.
        assert!(!due_after(
            BalanceErrorKind::RateLimited,
            10,
            Some(100 * MIN + 1)
        ));
        assert!(due_after(
            BalanceErrorKind::RateLimited,
            10,
            Some(100 * MIN)
        ));
    }

    #[test]
    fn a_retry_after_is_stored_as_an_instant() {
        let mut outcome = failed(BalanceErrorKind::RateLimited, Some(429));
        if let BalanceOutcome::Failed { error, .. } = &mut outcome {
            error.retry_after_ms = Some(30_000);
        }
        let stored = StoredBalance::merge(&StoredBalance::default(), &outcome, 1_000);
        assert_eq!(stored.error.and_then(|e| e.retry_at_ms), Some(31_000));
    }

    #[test]
    fn overview_refreshes_after_five_minutes_only() {
        assert!(due(RefreshTrigger::OverviewOpened, None, 0));
        assert!(!due(
            RefreshTrigger::OverviewOpened,
            Some(100 * MIN - 5 * MIN),
            0
        ));
        assert!(due(
            RefreshTrigger::OverviewOpened,
            Some(100 * MIN - 5 * MIN - 1),
            0
        ));
        assert!(
            !due(RefreshTrigger::OverviewOpened, Some(100 * MIN - MIN), 15),
            "switching providers within 5 minutes sends nothing"
        );
    }

    #[test]
    fn interval_follows_the_setting_and_off_sends_nothing() {
        assert!(!due(RefreshTrigger::Interval, None, 0), "Off");
        assert!(!due(
            RefreshTrigger::Interval,
            Some(100 * MIN - 14 * MIN),
            15
        ));
        assert!(due(
            RefreshTrigger::Interval,
            Some(100 * MIN - 15 * MIN),
            15
        ));
        assert!(due(RefreshTrigger::Interval, None, 15));
        assert!(
            !due(RefreshTrigger::Interval, Some(0), 1440),
            "100 minutes is less than a day"
        );
        assert!(
            !due(RefreshTrigger::Interval, Some(100 * MIN - 3 * MIN), 1),
            "the floor is 5 minutes"
        );
    }

    #[test]
    fn manual_and_after_add_always_go_and_nothing_goes_without_an_adapter() {
        assert!(due(RefreshTrigger::Manual, Some(100 * MIN), 0));
        assert!(due(RefreshTrigger::AfterAdd, None, 0));
        for trigger in [
            RefreshTrigger::Manual,
            RefreshTrigger::AfterAdd,
            RefreshTrigger::Interval,
            RefreshTrigger::OverviewOpened,
        ] {
            let input = RefreshInput {
                trigger,
                now_ms: 0,
                last_attempt_ms: None,
                interval_min: 15,
                has_adapter: false,
                enabled: true,
                last_error: None,
                retry_at_ms: None,
            };
            assert!(!refresh_due(&input), "{trigger:?}");
        }
        let disabled = RefreshInput {
            trigger: RefreshTrigger::Interval,
            now_ms: 0,
            last_attempt_ms: None,
            interval_min: 15,
            has_adapter: true,
            enabled: false,
            last_error: None,
            retry_at_ms: None,
        };
        assert!(!refresh_due(&disabled));
    }

    #[test]
    fn interval_off_sends_nothing_while_other_triggers_still_go() {
        assert!(!due(RefreshTrigger::Interval, None, 0));
        assert!(!due(RefreshTrigger::Interval, Some(0), 0));
        assert!(due(RefreshTrigger::OverviewOpened, None, 0));
        assert!(due(RefreshTrigger::Manual, None, 0));
        assert!(due(RefreshTrigger::AfterAdd, None, 0));
    }
}
