//! The run orchestrator: executes a [`ProbePlan`] with its concurrency rules.
//!
//! Single responsibility: run planned checks against [`Target`]s through
//! [`super::run_timed`], at most `concurrency` in flight across the run, at most two per provider
//! and exclusive (one) per key once that key answered 429 (restored after 60 s without one);
//! one model's checks run in order, key-scoped checks run once per key before its models; skip
//! rules come from [`super::plan`]; Stop (the cancel token) ends in-flight checks as "Stopped".
//! Every finished cell goes through a [`ProbePersist`] (results and one `probe` request-log row
//! per HTTP request) and then out as a [`ProbeRunEvent`]. Probes never touch the
//! circuit breaker or the health book and never write `source = gateway` rows.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU32, AtomicU64, Ordering};
use std::sync::{Mutex, PoisonError};
use std::time::Instant;

use futures_util::future::{BoxFuture, join_all};
use tokio::sync::{RwLock, RwLockReadGuard, RwLockWriteGuard, Semaphore, SemaphorePermit};

use super::capability::derive_capability;
use super::plan::{
    ChatGate, KeyOutcome, KeyPlan, ProbePlan, RowPlan, SkipReason, chat_gate, key_gate,
};
use super::status::{ReasonContext, reason_line};
pub use super::{CellRecord, RequestRecord};
use super::{
    CheckOutput, CheckTimeouts, ProbeOptions, ProbeRequest, ProbeStatus, ProbeTest, run_timed,
};
use crate::llm::failure::FailureKind;
use crate::llm::router::Target;
use crate::llm::runtime::LlmRuntime;
use pingdeck_base::cancel::{CancelToken, until_cancelled};
use pingdeck_base::error::AppError;
use pingdeck_model::events::EventSink;

/// Most checks in flight against one provider.
pub const MAX_PER_PROVIDER: usize = 2;
/// Fewest and most checks in flight across a run (`ai.providers.probe_concurrency`).
pub const CONCURRENCY_RANGE: (u8, u8) = (1, 16);
/// How long a key stays exclusive after its last 429.
const KEY_LIMIT_MS: u64 = 60_000;

/// A row of the run: the target to call and the options of its model.
#[derive(Debug, Clone)]
pub struct RunItem {
    /// Provider, key, secret and model.
    pub target: Target,
    /// Prompt, protocol and model-list path of the row.
    pub options: ProbeOptions,
}

/// The limits of a run.
#[derive(Debug, Clone, Copy)]
pub struct RunSettings {
    /// Checks in flight across the run.
    pub concurrency: usize,
    /// Per-check timeouts.
    pub timeouts: CheckTimeouts,
}

impl RunSettings {
    /// The settings for `ai.providers.probe_concurrency` and `probe_timeout_s`; the concurrency
    /// is clamped to 1..=16 (the header field may override the stored value per run).
    #[must_use]
    pub fn new(concurrency: u8, timeout_s: u32) -> Self {
        Self {
            concurrency: usize::from(concurrency.clamp(CONCURRENCY_RANGE.0, CONCURRENCY_RANGE.1)),
            timeouts: CheckTimeouts::from_setting(timeout_s),
        }
    }
}

/// Where a run keeps its results. The use-case layer implements it over `llm::store`, inside
/// `with_db_blocking`.
pub trait ProbePersist: Send + Sync {
    /// Stores a cell: its result (latest and history), the run's cell record and the measured
    /// capability.
    fn save_cell<'a>(
        &'a self,
        run_id: &'a str,
        cell: &'a CellRecord,
    ) -> BoxFuture<'a, Result<(), AppError>>;

    /// Writes one `llm_request_log` row with `source = probe`.
    fn record_request<'a>(
        &'a self,
        run_id: &'a str,
        entry: &'a RequestRecord,
    ) -> BoxFuture<'a, Result<(), AppError>>;
}

/// Progress of a run.
#[derive(Debug, Clone, PartialEq)]
pub enum ProbeRunEvent {
    /// A cell finished (or was skipped) and was stored.
    Cell {
        /// The cell.
        cell: CellRecord,
        /// Cells finished so far.
        done: u32,
        /// Cells planned.
        total: u32,
    },
}

/// Totals of a run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct RunSummary {
    /// Cells planned.
    pub total: u32,
    /// Cells finished, skipped ones included.
    pub done: u32,
    /// Cells that passed.
    pub ready: u32,
    /// Cells Rejected, Unreachable, Invalid response or Timed out.
    pub failed: u32,
    /// Cells Rate limited (HTTP 429): neither passed nor failed, worth running again.
    pub rate_limited: u32,
    /// Cells Unsupported.
    pub unsupported: u32,
    /// Cells Not observed: the model answered without showing the capability; neither passed
    /// nor failed, worth running again.
    pub not_observed: u32,
    /// Cells skipped.
    pub skipped: u32,
    /// Planned cells that never started (Not tested): after Stop, an interruption, or behind a
    /// rate-limited Chat.
    pub not_tested: u32,
    /// Whether the run was stopped.
    pub cancelled: bool,
    /// Whether the app quit while the run was going ([`mark_interrupted`]).
    pub interrupted: bool,
    /// Cells or request rows that could not be stored (the run went on).
    pub persist_errors: u32,
}

impl RunSummary {
    /// The history note of the run: "Interrupted by quit" for an interrupted run.
    #[must_use]
    pub const fn note(&self) -> Option<&'static str> {
        if self.interrupted {
            Some(INTERRUPTED_NOTE)
        } else {
            None
        }
    }
    fn count(&mut self, status: ProbeStatus) {
        match status {
            ProbeStatus::Ready => self.ready += 1,
            ProbeStatus::Unsupported => self.unsupported += 1,
            ProbeStatus::NotObserved => self.not_observed += 1,
            ProbeStatus::Skipped => self.skipped += 1,
            ProbeStatus::RateLimited => self.rate_limited += 1,
            _ => self.failed += 1,
        }
    }
}

/// The history note of a run the app quit under.
pub const INTERRUPTED_NOTE: &str = "Interrupted by quit";

/// [`INTERRUPTED_NOTE`], for the use-case layer that writes it at the next launch.
#[must_use]
pub const fn interrupted_note() -> &'static str {
    INTERRUPTED_NOTE
}

/// The summary of a run that was cut short by app quit, rebuilt at the next launch from its
/// stored cells: marked Stopped (`cancelled`) with the "Interrupted by quit" note, and every
/// planned cell that has no stored result counts as Not tested, so "Run remaining N"
/// ([`super::plan::plan_remaining`]) offers them again.
#[must_use]
pub fn mark_interrupted(plan: &ProbePlan, recorded: &[CellRecord]) -> RunSummary {
    let total = u32::try_from(plan.total_cells()).unwrap_or(u32::MAX);
    let mut summary = RunSummary {
        total,
        cancelled: true,
        interrupted: true,
        ..RunSummary::default()
    };
    for cell in recorded {
        summary.count(cell.status);
    }
    summary.done = u32::try_from(recorded.len()).unwrap_or(u32::MAX);
    summary.not_tested = total.saturating_sub(summary.done);
    summary
}

/// Everything a run needs.
pub struct ProbeRun<'a> {
    /// Id of the run, passed to the persistence calls.
    pub run_id: &'a str,
    /// Shared HTTP clients.
    pub runtime: &'a LlmRuntime,
    /// Rows, parallel to the [`super::plan::PlanItem`]s the plan was made from.
    pub items: &'a [RunItem],
    /// What to run.
    pub plan: &'a ProbePlan,
    /// Concurrency and timeouts.
    pub settings: RunSettings,
    /// Storage.
    pub persist: &'a dyn ProbePersist,
    /// Receives one event per cell.
    pub sink: &'a dyn EventSink<ProbeRunEvent>,
    /// Stop.
    pub cancel: &'a CancelToken,
}

/// Whether a key is exclusive: it answered 429 less than 60 s ago.
#[derive(Debug)]
struct KeyThrottle {
    last_429_ms: AtomicU64,
}

/// `last_429_ms` while no 429 was seen.
const NO_429: u64 = u64::MAX;

impl KeyThrottle {
    const fn new() -> Self {
        Self {
            last_429_ms: AtomicU64::new(NO_429),
        }
    }

    fn mark(&self, now_ms: u64) {
        self.last_429_ms.store(now_ms, Ordering::SeqCst);
    }

    fn limited(&self, now_ms: u64) -> bool {
        let last = self.last_429_ms.load(Ordering::SeqCst);
        last != NO_429 && now_ms.saturating_sub(last) < KEY_LIMIT_MS
    }
}

/// The locks of one key: shared while the key is free, exclusive after a 429.
struct KeyGate {
    lock: RwLock<()>,
    throttle: KeyThrottle,
}

/// What a run shares between its futures.
struct Shared<'a> {
    run: &'a ProbeRun<'a>,
    started: Instant,
    global: Semaphore,
    providers: HashMap<String, Semaphore>,
    /// Per provider id, per key id.
    keys: HashMap<String, HashMap<String, KeyGate>>,
    done: AtomicU32,
    total: u32,
    tally: Mutex<RunSummary>,
}

/// The slots one running check holds; dropping it frees them.
struct Slots<'s> {
    _read: Option<RwLockReadGuard<'s, ()>>,
    _write: Option<RwLockWriteGuard<'s, ()>>,
    _provider: SemaphorePermit<'s>,
    _global: SemaphorePermit<'s>,
}

impl Shared<'_> {
    fn now_ms(&self) -> u64 {
        u64::try_from(self.started.elapsed().as_millis()).unwrap_or(u64::MAX)
    }

    fn tally<R>(&self, update: impl FnOnce(&mut RunSummary) -> R) -> R {
        update(&mut self.tally.lock().unwrap_or_else(PoisonError::into_inner))
    }

    fn gate(&self, target: &Target) -> Option<&KeyGate> {
        self.keys
            .get(target.provider_id.as_str())?
            .get(target.key_id.as_str())
    }

    /// Waits for the provider, global and key slots in that fixed order (no cycle is possible).
    /// The key comes last so that a 429 answered while this check waited for its turn already
    /// counts. `None` when the run was stopped while waiting.
    async fn acquire<'s>(&'s self, target: &Target) -> Option<Slots<'s>> {
        let cancel = Some(self.run.cancel);
        let gate = self.gate(target)?;
        let provider = self.providers.get(target.provider_id.as_str())?;
        let provider = until_cancelled(cancel, provider.acquire()).await?.ok()?;
        let global = until_cancelled(cancel, self.global.acquire()).await?.ok()?;
        let (read, write) = if gate.throttle.limited(self.now_ms()) {
            (
                None,
                Some(until_cancelled(cancel, gate.lock.write()).await?),
            )
        } else {
            (Some(until_cancelled(cancel, gate.lock.read()).await?), None)
        };
        Some(Slots {
            _read: read,
            _write: write,
            _provider: provider,
            _global: global,
        })
    }

    /// Persists and announces one cell.
    async fn finish(&self, cell: CellRecord, requests: &[ProbeRequest]) {
        let run_id = self.run.run_id;
        let mut errors = 0;
        if self.run.persist.save_cell(run_id, &cell).await.is_err() {
            errors += 1;
        }
        for request in requests {
            let entry = RequestRecord {
                provider_id: cell.provider_id.clone(),
                key_id: cell.key_id.clone(),
                model_id: cell.model_id.clone(),
                request: request.clone(),
            };
            if self
                .run
                .persist
                .record_request(run_id, &entry)
                .await
                .is_err()
            {
                errors += 1;
            }
        }
        let done = self.done.fetch_add(1, Ordering::SeqCst) + 1;
        self.tally(|summary| {
            summary.done = done;
            summary.persist_errors += errors;
            summary.count(cell.status);
        });
        self.run.sink.emit(ProbeRunEvent::Cell {
            cell,
            done,
            total: self.total,
        });
    }

    fn skipped_cell(&self, target: &Target, test: ProbeTest, reason: SkipReason) -> CellRecord {
        CellRecord {
            provider_id: target.provider_id.clone(),
            key_id: target.key_id.clone(),
            model_id: target.model.clone(),
            test,
            status: ProbeStatus::Skipped,
            reason: reason.label().to_owned(),
            skip: Some(reason),
            result: None,
            capability: None,
        }
    }

    /// Runs one check under the limits. `None` when the run was stopped before the check could
    /// start; a check stopped in flight is reported as skipped with the reason "Stopped" and
    /// yields `None` for its status.
    async fn run_check(&self, item: &RunItem, test: ProbeTest) -> Option<Option<ProbeStatus>> {
        let held = self.acquire(&item.target).await?;
        let CheckOutput { result, requests } = run_timed(
            self.run.runtime,
            &item.target,
            test,
            &item.options,
            &self.run.settings.timeouts,
            Some(self.run.cancel),
        )
        .await;
        let status = ProbeStatus::from_result(&result);
        // Mark the key before the slots are released, so the next check already sees it.
        if (result.status == Some(429) || result.failure_kind == Some(FailureKind::RateLimit))
            && let Some(gate) = self.gate(&item.target)
        {
            gate.throttle.mark(self.now_ms());
        }
        drop(held);
        let cell = match status {
            Some(status) => {
                let reason = reason_line(
                    &result,
                    status,
                    &ReasonContext {
                        timeout_s: self.run.settings.timeouts.headline(test).as_secs(),
                        retry_after_s: result.detail["retry_after_s"].as_u64(),
                        ..ReasonContext::default()
                    },
                );
                CellRecord {
                    provider_id: item.target.provider_id.clone(),
                    key_id: item.target.key_id.clone(),
                    model_id: item.target.model.clone(),
                    test,
                    status,
                    reason,
                    skip: None,
                    capability: derive_capability(&result),
                    result: Some(result),
                }
            }
            None => self.skipped_cell(&item.target, test, SkipReason::Stopped),
        };
        self.finish(cell, &requests).await;
        Some(status)
    }

    /// The key-scoped checks of one key, then all of its rows.
    async fn run_key(&self, key: &KeyPlan) {
        let Some(first) = self.run.items.get(key.first_item) else {
            return;
        };
        let mut outcome = KeyOutcome::default();
        for test in &key.checks {
            if self.run.cancel.is_cancelled() {
                return;
            }
            if let Some(reason) = key_gate(*test, &outcome) {
                self.finish(self.skipped_cell(&first.target, *test, reason), &[])
                    .await;
                continue;
            }
            let Some(status) = self.run_check(first, *test).await else {
                return;
            };
            match test {
                ProbeTest::Connectivity => outcome.connectivity = status,
                // Passing the auth check does not end a cooldown: the model list says
                // nothing about the quota or the rate limit that rested the key.
                ProbeTest::Auth => outcome.auth = status,
                _ => {}
            }
        }
        let rows = self.run.plan.rows.iter().filter(|row| {
            self.run.items.get(row.item).is_some_and(|item| {
                item.target.provider_id == key.provider_id && item.target.key_id == key.key_id
            })
        });
        join_all(rows.map(|row| self.run_row(row, outcome))).await;
    }

    /// One model's checks, in order.
    async fn run_row(&self, row: &RowPlan, key: KeyOutcome) {
        let Some(item) = self.run.items.get(row.item) else {
            return;
        };
        let mut chat: Option<ProbeStatus> = None;
        for cell in &row.cells {
            if self.run.cancel.is_cancelled() {
                return;
            }
            let skip = match cell.skip.or_else(|| key_gate(cell.test, &key)) {
                Some(reason) => Some(reason),
                None => match chat_gate(cell.test, chat) {
                    ChatGate::Open => None,
                    ChatGate::Skip(reason) => Some(reason),
                    // Rate limited Chat: the cell stays "Not tested" for a later run.
                    ChatGate::NotTested => continue,
                },
            };
            if let Some(reason) = skip {
                self.finish(self.skipped_cell(&item.target, cell.test, reason), &[])
                    .await;
                continue;
            }
            let Some(status) = self.run_check(item, cell.test).await else {
                return;
            };
            if cell.test == ProbeTest::Chat {
                chat = status;
            }
        }
    }
}

/// Runs `run.plan` to the end or until Stop. Returns the totals; every cell was announced on
/// `run.sink` and handed to `run.persist` before the call returns.
///
/// # Errors
/// [`AppError::Validation`] when the plan refers to an item the run does not have.
pub async fn run_plan(run: &ProbeRun<'_>) -> Result<RunSummary, AppError> {
    let known = |index: usize| index < run.items.len();
    if !run.plan.keys.iter().all(|key| known(key.first_item))
        || !run.plan.rows.iter().all(|row| known(row.item))
    {
        return Err(AppError::validation(
            "the probe plan refers to a row the run does not have",
        ));
    }
    let mut providers: HashMap<String, Semaphore> = HashMap::new();
    let mut keys: HashMap<String, HashMap<String, KeyGate>> = HashMap::new();
    for item in run.items {
        providers
            .entry(item.target.provider_id.clone())
            .or_insert_with(|| Semaphore::new(MAX_PER_PROVIDER));
        keys.entry(item.target.provider_id.clone())
            .or_default()
            .entry(item.target.key_id.clone())
            .or_insert_with(|| KeyGate {
                lock: RwLock::new(()),
                throttle: KeyThrottle::new(),
            });
    }
    let total = u32::try_from(run.plan.total_cells()).unwrap_or(u32::MAX);
    let shared = Shared {
        run,
        started: Instant::now(),
        global: Semaphore::new(run.settings.concurrency.max(1)),
        providers,
        keys,
        done: AtomicU32::new(0),
        total,
        tally: Mutex::new(RunSummary {
            total,
            ..RunSummary::default()
        }),
    };
    // A retry runs the keys of its rate-limited cells one check at a time from the start.
    for (provider, key) in &run.plan.rate_limited_keys {
        if let Some(gate) = shared.keys.get(provider).and_then(|keys| keys.get(key)) {
            gate.throttle.mark(0);
        }
    }
    join_all(run.plan.keys.iter().map(|key| shared.run_key(key))).await;
    let mut summary = shared.tally(|summary| *summary);
    summary.cancelled = run.cancel.is_cancelled();
    summary.not_tested = summary.total.saturating_sub(summary.done);
    Ok(summary)
}

#[cfg(test)]
mod tests;
