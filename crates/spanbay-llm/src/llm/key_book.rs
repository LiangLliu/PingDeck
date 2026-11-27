//! Runtime state of multi-key rotation: per-key cooldowns, last error, last use, and the
//! round-robin cursor of each provider.
//!
//! Single responsibility: remember, for this session only, what each provider key's calls said
//! about the *key* and about the key's rate limit per model (the circuit of the key+model pair is
//! [`crate::llm::health::HealthBook`]), so routing can rest a rate-limited or rejected key and
//! start each request on the key the provider's [`KeyStrategy`] picks. When the provider's
//! cooldown switch is on:
//! * rate limit: rests the key *for that model only* (another model on the same key is not
//!   held back): the `Retry-After` (at most 10 min), else 30 s doubled per consecutive failure of
//!   that pair, at most 10 min;
//! * exhausted account (quota): the `Retry-After` (at most 10 min), else 30 min, never the 429
//!   doubling;
//! * rejected credential or region: 30 min (cleared when the key is replaced or the user clears
//!   it; a passing key test does not clear it);
//! * anything else (a model-level refusal, 5xx, 408, transport): no key cooldown (the circuit
//!   handles it). The rule is [`crate::llm::failover::Failure::key_rest`].
//! * success: the key's streak and cooldown and the rest of that model are cleared.
//!
//! The streak resets 10 min after the last failure. Last error and last use are recorded whether
//! the switch is on or off. A round robin started for a conversation (an affinity, see
//! [`KeyBook::start`]) keeps that conversation on its key while it is remembered, so a prompt
//! cache built on one account is reused.

use std::collections::HashMap;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use crate::llm::failover::{FailureObservation, FailureScope, KeyRest};
use crate::llm::failure::FailureKind;
use crate::llm::runtime::now_ms;
use crate::llm::store::KeyStrategy;
use crate::llm::upstream::AttemptError;
use spanbay_base::secrets::redact_secrets;

/// First rest after a 429 without `Retry-After`; doubles per consecutive failure.
const RATE_LIMIT_BASE: Duration = Duration::from_secs(30);
/// Longest rest after a 429.
const MAX_RATE_LIMIT_REST: Duration = Duration::from_secs(10 * 60);
/// Rest after a rejected credential or an exhausted account (401, 402, 403).
const REJECTED_REST: Duration = Duration::from_secs(30 * 60);
/// A failure this long after the previous one starts a new streak.
const STREAK_RESET: Duration = Duration::from_secs(10 * 60);
/// Longest error line kept, in characters.
const MAX_ERROR_CHARS: usize = 200;

/// What the UI shows about a key's calls this session.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct KeyState {
    /// Until when (Unix ms) the key rests; only while it does.
    pub cooldown_until_ms: Option<i64>,
    /// The rate-limit rest of one model on the key that ends soonest, while one runs: the model
    /// and until when (Unix ms). The key still serves its other models.
    pub model_rest: Option<(String, i64)>,
    /// HTTP status of the last failure (none for transport errors).
    pub last_error_status: Option<u16>,
    /// One redacted line about the last failure.
    pub last_error: Option<String>,
    /// When the last failure happened (Unix ms).
    pub last_error_ms: Option<i64>,
    /// When the key was last called (Unix ms).
    pub last_used_ms: Option<i64>,
    /// Category of the last failure.
    pub last_error_kind: Option<FailureKind>,
    /// What the last failure was about.
    pub last_error_scope: Option<FailureScope>,
}

/// A rest earned by consecutive failures: until when, how many in a row, the last one's time.
#[derive(Debug, Default, Clone, Copy)]
struct Rest {
    until_ms: Option<i64>,
    streak: u32,
    last_failure_ms: i64,
}

impl Rest {
    /// Counts one more failure at `now` and rests until `now` plus what `rest` says.
    fn extend(&mut self, now: i64, rest: KeyRest) {
        if now.saturating_sub(self.last_failure_ms) >= millis(STREAK_RESET) {
            self.streak = 0;
        }
        self.streak = self.streak.saturating_add(1);
        self.last_failure_ms = now;
        let rest = match rest {
            KeyRest::RetryAfter(ms) => Duration::from_millis(ms).min(MAX_RATE_LIMIT_REST),
            KeyRest::Backoff => {
                let doubling = 1_u32
                    .checked_shl(self.streak.saturating_sub(1))
                    .unwrap_or(u32::MAX);
                RATE_LIMIT_BASE
                    .saturating_mul(doubling)
                    .min(MAX_RATE_LIMIT_REST)
            }
            KeyRest::Rejected => REJECTED_REST,
        };
        self.until_ms = Some(now.saturating_add(millis(rest)));
    }
}

#[derive(Debug, Default)]
struct Entry {
    state: KeyState,
    /// The rest of the whole key (an account-level failure).
    rest: Rest,
    /// The rate-limit rest of each model on this key.
    models: HashMap<String, Rest>,
}

/// Conversations remembered for round-robin affinity; over it the memory starts over.
const MAX_AFFINITIES: usize = 4096;

/// Every key's rotation state, shared by the gateway and the client API.
#[derive(Debug, Default)]
pub struct KeyBook {
    entries: Mutex<HashMap<String, Entry>>,
    cursors: Mutex<HashMap<String, usize>>,
    /// `(cursor, affinity)` → the key index a conversation started on.
    affinities: Mutex<HashMap<(String, u64), usize>>,
    /// Test clock offset: tests move time forward instead of sleeping.
    #[cfg(test)]
    skew_ms: std::sync::atomic::AtomicU64,
    /// Optional frozen base timestamp for deterministic unit tests.
    #[cfg(test)]
    fixed_now_ms: Option<i64>,
}

fn millis(duration: Duration) -> i64 {
    i64::try_from(duration.as_millis()).unwrap_or(i64::MAX)
}

/// The first line of `error` with `secret` and credential shapes masked, at most 200 characters.
#[must_use]
pub fn error_line(error: &AttemptError, secret: &str) -> String {
    let message = error.message();
    let line = message.lines().next().unwrap_or_default().trim();
    redact_secrets(line, &[secret.to_owned()])
        .chars()
        .take(MAX_ERROR_CHARS)
        .collect()
}

impl KeyBook {
    /// An empty book.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// An empty book with a deterministic frozen clock (advances only via [`KeyBook::advance`]).
    #[cfg(test)]
    #[must_use]
    pub(crate) fn with_fixed_clock(base_ms: i64) -> Self {
        Self {
            fixed_now_ms: Some(base_ms),
            ..Self::default()
        }
    }

    /// The current wall clock in Unix ms; tests shift it with [`KeyBook::advance`].
    fn now_ms(&self) -> i64 {
        #[cfg(test)]
        let now = self.fixed_now_ms.unwrap_or_else(now_ms);
        #[cfg(not(test))]
        let now = now_ms();
        #[cfg(test)]
        let now = now.saturating_add(
            i64::try_from(self.skew_ms.load(std::sync::atomic::Ordering::Relaxed)).unwrap_or(0),
        );
        now
    }

    /// Moves the book's clock forward, so cooldowns end without sleeping.
    #[cfg(test)]
    pub(crate) fn advance(&self, by: Duration) {
        let millis = u64::try_from(by.as_millis()).unwrap_or(u64::MAX);
        self.skew_ms
            .fetch_add(millis, std::sync::atomic::Ordering::Relaxed);
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HashMap<String, Entry>> {
        self.entries.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Until when the key rests for `model`, while it does: the later of the key's own rest and
    /// the rate-limit rest of that model.
    #[must_use]
    pub fn cooling_until(&self, key_id: &str, model: &str) -> Option<i64> {
        let now = self.now_ms();
        let entries = self.lock();
        let entry = entries.get(key_id)?;
        let model_rest = entry.models.get(model).and_then(|rest| rest.until_ms);
        entry
            .rest
            .until_ms
            .into_iter()
            .chain(model_rest)
            .filter(|until| *until > now)
            .max()
    }

    /// The key's state now (an ended cooldown or model rest is absent).
    #[must_use]
    pub fn state(&self, key_id: &str) -> KeyState {
        let now = self.now_ms();
        let entries = self.lock();
        let Some(entry) = entries.get(key_id) else {
            return KeyState::default();
        };
        let mut state = entry.state.clone();
        state.cooldown_until_ms = state.cooldown_until_ms.filter(|until| *until > now);
        state.model_rest = entry
            .models
            .iter()
            .filter_map(|(model, rest)| Some((model, rest.until_ms.filter(|until| *until > now)?)))
            .min_by_key(|(_, until)| *until)
            .map(|(model, until)| (model.clone(), until));
        state
    }

    /// Records a call of the key for `model` that did not fail on the key; `succeeded` clears the
    /// key's streak and cooldown and that model's rest. Returns whether the key's run state
    /// changed (its cooldown or that model's rest ended).
    pub fn used(&self, key_id: &str, model: &str, succeeded: bool) -> bool {
        let now = self.now_ms();
        let mut entries = self.lock();
        let entry = entries.entry(key_id.to_owned()).or_default();
        entry.state.last_used_ms = Some(now);
        if !succeeded {
            return false;
        }
        let rested = entry
            .models
            .remove(model)
            .and_then(|rest| rest.until_ms)
            .is_some_and(|until| until > now);
        entry.rest = Rest::default();
        entry.state.cooldown_until_ms.take().is_some() || rested
    }

    /// Records a failed call of the key for `model`: the last error always, a rest when
    /// `cooldown` is on and the failure earns one ([`crate::llm::failover::Failure::key_rest`]).
    /// A rate limit rests the key for `model` only; an account-level failure (quota, rejected
    /// credential, region) rests the whole key. Returns whether the key's run state changed (its
    /// own cooldown or that model's rest started or moved).
    pub fn failed(
        &self,
        key_id: &str,
        model: &str,
        observation: &FailureObservation,
        line: String,
        cooldown: bool,
    ) -> bool {
        let now = self.now_ms();
        let mut entries = self.lock();
        let entry = entries.entry(key_id.to_owned()).or_default();
        entry.state.last_used_ms = Some(now);
        entry.state.last_error_status = observation.status;
        entry.state.last_error = Some(line);
        entry.state.last_error_ms = Some(now);
        let failure = observation.assess();
        entry.state.last_error_kind = Some(failure.kind);
        entry.state.last_error_scope = Some(failure.scope);
        let Some(rest) = failure.key_rest().filter(|_| cooldown) else {
            return false;
        };
        if failure.kind == FailureKind::RateLimit {
            entry
                .models
                .entry(model.to_owned())
                .or_default()
                .extend(now, rest);
            return true;
        }
        entry.rest.extend(now, rest);
        entry.state.cooldown_until_ms = entry.rest.until_ms;
        true
    }

    /// Ends the key's cooldown, streak and model rests (key replaced, or the user asked).
    /// Returns whether a cooldown or a model rest ended.
    pub fn clear_cooldown(&self, key_id: &str) -> bool {
        let now = self.now_ms();
        self.lock().get_mut(key_id).is_some_and(|entry| {
            let rested = entry
                .models
                .values()
                .any(|rest| rest.until_ms.is_some_and(|until| until > now));
            entry.rest = Rest::default();
            entry.models.clear();
            entry.state.cooldown_until_ms.take().is_some() || rested
        })
    }

    /// The index [`KeyBook::start`] would answer for a round robin without moving the cursor (the
    /// route preview); a random strategy previews the first key.
    #[must_use]
    pub fn peek_start(
        &self,
        cursor: &str,
        strategy: KeyStrategy,
        len: usize,
        affinity: Option<u64>,
    ) -> usize {
        if len < 2 || strategy != KeyStrategy::RoundRobin {
            return 0;
        }
        if let Some(known) = affinity.and_then(|affinity| self.affinity(cursor, affinity)) {
            return known % len;
        }
        let cursors = self.cursors.lock().unwrap_or_else(PoisonError::into_inner);
        cursors.get(cursor).map_or(0, |position| *position % len)
    }

    fn affinity(&self, cursor: &str, affinity: u64) -> Option<usize> {
        self.affinities
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&(cursor.to_owned(), affinity))
            .copied()
    }

    /// The index (of `len` candidate keys) a request starts with. Round robin moves the cursor
    /// named `cursor` (the provider and its candidate keys, so each relay group has its own) on
    /// by one per call; a request with an `affinity` (its conversation) starts where the first
    /// request of that conversation started, without moving the cursor.
    #[must_use]
    pub fn start(
        &self,
        cursor: &str,
        strategy: KeyStrategy,
        len: usize,
        affinity: Option<u64>,
    ) -> usize {
        if len < 2 {
            return 0;
        }
        match strategy {
            KeyStrategy::Primary => 0,
            KeyStrategy::RoundRobin => {
                if let Some(known) = affinity.and_then(|affinity| self.affinity(cursor, affinity)) {
                    return known % len;
                }
                let start = {
                    let mut cursors = self.cursors.lock().unwrap_or_else(PoisonError::into_inner);
                    let position = cursors.entry(cursor.to_owned()).or_default();
                    let start = *position % len;
                    *position = start + 1;
                    start
                };
                if let Some(affinity) = affinity {
                    let mut known = self
                        .affinities
                        .lock()
                        .unwrap_or_else(PoisonError::into_inner);
                    if known.len() >= MAX_AFFINITIES {
                        known.clear();
                    }
                    known.insert((cursor.to_owned(), affinity), start);
                }
                start
            }
            KeyStrategy::Random => {
                let mut bytes = [0_u8; 8];
                // Without entropy the first key is as good a start as any.
                if getrandom::fill(&mut bytes).is_err() {
                    return 0;
                }
                let len64 = u64::try_from(len).unwrap_or(u64::MAX);
                usize::try_from(u64::from_le_bytes(bytes) % len64).unwrap_or(0)
            }
        }
    }
}

#[cfg(test)]
#[path = "key_book_tests.rs"]
mod tests;
