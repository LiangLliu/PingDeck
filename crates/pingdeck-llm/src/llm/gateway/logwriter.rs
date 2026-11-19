//! The log writer: the one thread that turns finished requests into stored rows.
//!
//! Single responsibility: the log writer thread. The request path only enqueues a
//! [`LogJob`] (bounded queue of 1,024; when it is full the newest job's row is dropped and
//! counted in `log_dropped` while its usage still reaches the rollup; captured bytes waiting in it are bounded too, at 64 MiB: over that a job loses
//! its capture, keeps its row and is counted in `captures_dropped`; nothing here ever blocks the
//! request path or touches a secret). On the writer thread, in
//! this order: the capture decision, redaction of the capture and of error text (needles: every
//! stored secret plus the active client tokens seen on the request path), the cost from the
//! effective price, the daily rollup on the *local* day, one transaction (`store::insert_log`),
//! retention (at start, every 100 inserts and hourly) and the batched `GatewayLogBatch` event
//! (at most one per 100 ms). On shutdown the queue is drained before the thread ends.

use std::collections::HashMap;
use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::mpsc::{self, Receiver, RecvTimeoutError, SyncSender, TrySendError};
use std::sync::{Arc, Mutex, PoisonError, Weak};
use std::thread::JoinHandle;
use std::time::{Duration, Instant};

use crate::llm::capture::{
    BoundedBody, CAPTURE_MAX_BYTES, CaptureSource, LogOutcome, RawExchange, redact_exchange_hiding,
    redact_row_texts, should_capture,
};
use crate::llm::capture_record::CapturedFrames;
use crate::llm::events::LlmEvent;
use crate::llm::runtime::{DbConn, LlmRuntime, now_ms};
use crate::llm::store::{self, LogWrite, PruneLimits, RequestLogEntry};
use crate::llm::usage::{CostUsage, PriceResolution, RollupDelta};
use pingdeck_base::error::AppError;
use pingdeck_store::DbConnection;

use super::auth::SeenTokens;

/// Jobs that may wait for the writer.
pub(crate) const LOG_QUEUE: usize = 1024;
/// Jobs a full queue keeps for their usage only: the row is lost, the tokens and cost
/// still reach the rollup. Beyond this the usage is lost too.
const USAGE_OVERFLOW: usize = 4096;
/// `GatewayLogBatch` events are at least this far apart.
const BATCH_EVERY: Duration = Duration::from_millis(100);
/// `ClientsChanged` events (new `last_used`, today's count, "Verified") are at least this far
/// apart: the clients table refreshes every 5 s at most.
const CLIENTS_EVERY: Duration = Duration::from_secs(5);
/// A client's "Verified" instant is written at most this often.
const VERIFIED_EVERY: Duration = Duration::from_secs(30);
/// Retention (log, captures, usage rollup) runs this often when idle...
const MAINTENANCE_EVERY: Duration = Duration::from_secs(3600);
/// ...and after this many inserts.
const PRUNE_EVERY_INSERTS: u32 = 100;
/// Prices cached before the cache restarts.
const MAX_CACHED_PRICES: usize = 1024;
/// Captured bytes that may wait for the writer; at the bound the capture
/// of the next job is dropped and its row kept.
const MAX_QUEUED_CAPTURE_BYTES: u64 = 64 * 1024 * 1024;
/// How long [`LogQueue::shutdown`] waits to hand the writer its stop message.
const STOP_PATIENCE: Duration = Duration::from_secs(5);

/// One finished request, as the request path hands it over. Public inside the crate: the chat
/// and stream code build it. Everything in it is bounded and **not yet redacted**.
#[derive(Debug)]
pub(crate) struct LogJob {
    /// The row. `id` is assigned on storage; `request_id`, `client_id`, `client_name`, `peer`
    /// and `source` are stamped by `Context::submit`; cost columns by the writer.
    pub entry: RequestLogEntry,
    /// Token counts for the cost and the rollup; `None` when the provider reported none.
    pub usage: Option<CostUsage>,
    /// The bounded raw exchange; stored (redacted) only when the capture policy says so.
    pub raw: Option<RawExchange>,
    /// Some upstream attempt failed (an `errors only` capture is then written).
    pub any_attempt_failed: bool,
    /// The request's "Keep this exchange in the log" option: an `api` row is captured under
    /// every policy while it is on (the default).
    pub keep_exchange: bool,
}

impl LogJob {
    /// A job for `entry` with no capture, usage taken from the entry's own token counts.
    pub(crate) fn from_entry(entry: RequestLogEntry) -> Self {
        let usage =
            (entry.input_tokens.is_some() || entry.output_tokens.is_some()).then(|| CostUsage {
                input_tokens: entry.input_tokens.unwrap_or(0),
                output_tokens: entry.output_tokens.unwrap_or(0),
                cached_tokens: entry.cached_tokens.unwrap_or(0),
                cache_write_tokens: entry.cache_write_tokens.unwrap_or(0),
                reasoning_tokens: entry.reasoning_tokens.unwrap_or(0),
                tokens_estimated: entry.tokens_estimated,
                // The log keeps no one-hour tier; in-app calls only ask for five-minute writes.
                cache_write_long_tokens: 0,
            });
        Self {
            entry,
            usage,
            raw: None,
            any_attempt_failed: false,
            keep_exchange: true,
        }
    }
}

/// Approximate bytes `raw` holds: bodies, frames, headers and the URL (the hub request is
/// counted as the client body it was decoded from).
fn raw_bytes(raw: &RawExchange) -> u64 {
    let body = |body: &Option<BoundedBody>| body.as_ref().map_or(0, |body| body.text.len());
    let frames = |frames: &Option<CapturedFrames>| {
        frames.as_ref().map_or(0, |frames| {
            frames
                .frames
                .iter()
                .map(|frame| frame.event.len() + frame.data.len())
                .sum()
        })
    };
    let headers = |headers: &[(String, String)]| {
        headers
            .iter()
            .map(|(name, value)| name.len() + value.len())
            .sum::<usize>()
    };
    let hub = if raw.hub_request.is_some() {
        body(&raw.client_req_body)
    } else {
        0
    };
    let total = body(&raw.client_req_body)
        + body(&raw.upstream_req_body)
        + body(&raw.upstream_resp_body)
        + body(&raw.client_resp_body)
        + frames(&raw.upstream_frames)
        + frames(&raw.client_frames)
        + headers(&raw.client_req_headers)
        + headers(&raw.upstream_req_headers)
        + headers(&raw.upstream_resp_headers)
        + headers(&raw.client_resp_headers)
        + raw.upstream_url.len()
        + hub;
    u64::try_from(total).unwrap_or(u64::MAX)
}

enum Message {
    /// A job and the captured bytes it holds in the queue.
    Job(Box<LogJob>, u64),
    /// Everything queued before this message is stored and announced; then the sender is told.
    Flush(mpsc::Sender<()>),
    Stop,
}

/// The request path's side of the writer. One per runtime (see `LlmRuntime::log`): the gateway
/// and the in-process chat client share it, so Chat logs its calls with the gateway stopped. The
/// writer thread holds only a weak reference to the runtime and ends when the queue is dropped
/// (after storing what is queued).
pub(crate) struct LogQueue {
    tx: SyncSender<Message>,
    seen: Arc<SeenTokens>,
    dropped: Arc<AtomicU64>,
    captures_dropped: AtomicU64,
    /// Captured bytes waiting in the queue.
    queued_capture_bytes: Arc<AtomicU64>,
    /// Jobs the full queue refused, kept for the rollup.
    overflow: Arc<Mutex<Vec<LogJob>>>,
    writer: Mutex<Option<JoinHandle<()>>>,
}

impl LogQueue {
    /// Starts the writer thread with room for `capacity` waiting jobs. `seen` collects the client
    /// tokens the request path met, which the writer redacts like stored secrets.
    pub(crate) fn spawn(
        llm: Arc<LlmRuntime>,
        seen: Arc<SeenTokens>,
        capacity: usize,
    ) -> Result<Self, AppError> {
        let (tx, rx) = mpsc::sync_channel(capacity);
        let dropped = Arc::new(AtomicU64::new(0));
        let queued_capture_bytes = Arc::new(AtomicU64::new(0));
        let overflow: Arc<Mutex<Vec<LogJob>>> = Arc::default();
        let writer = Writer::new(
            Arc::downgrade(&llm),
            Arc::clone(&seen),
            Arc::clone(&dropped),
            Arc::clone(&queued_capture_bytes),
            Arc::clone(&overflow),
        );
        let handle = std::thread::Builder::new()
            .name("pingdeck-gateway-log".to_owned())
            .spawn(move || writer.run(&rx))
            .map_err(|error| {
                AppError::io(format!("cannot start the gateway log writer: {error}"))
            })?;
        Ok(Self {
            tx,
            seen,
            dropped,
            captures_dropped: AtomicU64::new(0),
            queued_capture_bytes,
            overflow,
            writer: Mutex::new(Some(handle)),
        })
    }

    /// Queues `job`. Never blocks; a full queue drops the job's row and counts it but keeps its
    /// usage for the rollup, a queue holding 64 MiB of captures drops the job's capture and
    /// counts that.
    pub(crate) fn submit(&self, mut job: LogJob) {
        let bytes = job.raw.as_ref().map_or(0, raw_bytes);
        if bytes > 0
            && self.queued_capture_bytes.load(Ordering::Relaxed) + bytes > MAX_QUEUED_CAPTURE_BYTES
        {
            job.raw = None;
            self.captures_dropped.fetch_add(1, Ordering::Relaxed);
        }
        let bytes = job.raw.as_ref().map_or(0, raw_bytes);
        self.queued_capture_bytes
            .fetch_add(bytes, Ordering::Relaxed);
        match self.tx.try_send(Message::Job(Box::new(job), bytes)) {
            Ok(()) => {}
            Err(TrySendError::Full(message)) => {
                self.queued_capture_bytes
                    .fetch_sub(bytes, Ordering::Relaxed);
                self.dropped.fetch_add(1, Ordering::Relaxed);
                if let Message::Job(mut job, _) = message {
                    job.raw = None;
                    let mut overflow = self.overflow.lock().unwrap_or_else(PoisonError::into_inner);
                    if overflow.len() < USAGE_OVERFLOW {
                        overflow.push(*job);
                    }
                }
            }
            // The writer is gone (shutdown raced a late request): the row is moot.
            Err(TrySendError::Disconnected(_)) => {
                self.queued_capture_bytes
                    .fetch_sub(bytes, Ordering::Relaxed);
            }
        }
    }

    /// Captures lost so far (their rows were kept).
    pub(crate) fn captures_dropped(&self) -> u64 {
        self.captures_dropped.load(Ordering::Relaxed)
    }

    /// Jobs lost so far: the queue was full or the row could not be stored.
    pub(crate) fn dropped(&self) -> u64 {
        self.dropped.load(Ordering::Relaxed)
    }

    /// The client tokens seen by the request path (redaction needles of the writer).
    pub(crate) fn seen(&self) -> &Arc<SeenTokens> {
        &self.seen
    }

    /// Waits until everything queued before this call is stored and announced (at most
    /// [`STOP_PATIENCE`]); the writer keeps running. The gateway calls it once its last request
    /// is gone, so a stopped gateway has all its rows in the log.
    pub(crate) fn flush(&self) {
        let (done, wait) = mpsc::channel();
        let give_up = Instant::now() + STOP_PATIENCE;
        let mut message = Message::Flush(done);
        loop {
            match self.tx.try_send(message) {
                Ok(()) => break,
                Err(TrySendError::Disconnected(_)) => return,
                Err(TrySendError::Full(back)) if Instant::now() < give_up => {
                    message = back;
                    std::thread::sleep(Duration::from_millis(10));
                }
                Err(TrySendError::Full(_)) => return,
            }
        }
        let _ = wait.recv_timeout(STOP_PATIENCE);
    }

    /// Lets the writer store everything queued, then ends it. Dropping the queue does the same.
    pub(crate) fn shutdown(&self) {
        let Some(handle) = self
            .writer
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()
        else {
            return;
        };
        let give_up = Instant::now() + STOP_PATIENCE;
        loop {
            match self.tx.try_send(Message::Stop) {
                Ok(()) => break,
                Err(TrySendError::Disconnected(_)) => return,
                Err(TrySendError::Full(_)) if Instant::now() < give_up => {
                    std::thread::sleep(Duration::from_millis(10));
                }
                // A writer stuck for seconds: leave it; it ends when the queue is dropped.
                Err(TrySendError::Full(_)) => return,
            }
        }
        // The last runtime reference may be dropped by the writer thread itself: no self-join.
        if handle.thread().id() != std::thread::current().id() {
            let _ = handle.join();
        }
    }
}

impl Drop for LogQueue {
    fn drop(&mut self) {
        self.shutdown();
    }
}

/// Price resolutions by `(provider, model)`, valid for one configuration version.
#[derive(Default)]
struct PriceCache {
    version: u64,
    prices: HashMap<(String, String), PriceResolution>,
}

impl PriceCache {
    fn lookup(
        &mut self,
        connection: &DbConnection,
        version: u64,
        provider: &str,
        model: &str,
    ) -> Option<PriceResolution> {
        if self.version != version || self.prices.len() >= MAX_CACHED_PRICES {
            self.prices.clear();
            self.version = version;
        }
        let key = (provider.to_owned(), model.to_owned());
        if let Some(price) = self.prices.get(&key) {
            return Some(price.clone());
        }
        // A failed read is not cached: the next row tries again.
        let price = store::resolve_price(connection, provider, model).ok()?;
        self.prices.insert(key, price.clone());
        Some(price)
    }
}

/// The names of every provider's extra headers: their values are secrets of any length, so the
/// capture masks them by name (the snapshot is cached until the next `touch`).
fn extra_header_names(llm: &LlmRuntime) -> Vec<String> {
    let Ok(snapshot) = llm.snapshot_blocking() else {
        return Vec::new();
    };
    let mut names: Vec<String> = snapshot
        .providers
        .values()
        .flat_map(|provider| provider.quirks.extra_headers.keys().cloned())
        .collect();
    names.sort_unstable();
    names.dedup();
    names
}

fn capture_source(source: &str) -> CaptureSource {
    match source {
        "api" => CaptureSource::Api,
        "agent" => CaptureSource::Agent,
        "probe" => CaptureSource::Probe,
        _ => CaptureSource::Gateway,
    }
}

fn log_outcome(outcome: &str) -> LogOutcome {
    match outcome {
        "success" => LogOutcome::Success,
        "canceled" => LogOutcome::Canceled,
        _ => LogOutcome::Failure,
    }
}

struct Writer {
    llm: Weak<LlmRuntime>,
    seen: Arc<SeenTokens>,
    dropped: Arc<AtomicU64>,
    queued_capture_bytes: Arc<AtomicU64>,
    overflow: Arc<Mutex<Vec<LogJob>>>,
    connection: Option<DbConn>,
    /// Stored rows not yet announced.
    pending: Vec<RequestLogEntry>,
    last_batch: Option<Instant>,
    inserts_since_prune: u32,
    next_maintenance: Instant,
    /// A stored row named a client since the last `ClientsChanged`.
    clients_dirty: bool,
    last_clients_event: Option<Instant>,
    /// When each client's "Verified" instant was last written.
    verified_marks: HashMap<String, Instant>,
    prices: PriceCache,
}

impl Writer {
    fn new(
        llm: Weak<LlmRuntime>,
        seen: Arc<SeenTokens>,
        dropped: Arc<AtomicU64>,
        queued_capture_bytes: Arc<AtomicU64>,
        overflow: Arc<Mutex<Vec<LogJob>>>,
    ) -> Self {
        Self {
            llm,
            seen,
            dropped,
            queued_capture_bytes,
            overflow,
            connection: None,
            pending: Vec::new(),
            last_batch: None,
            inserts_since_prune: 0,
            next_maintenance: Instant::now() + MAINTENANCE_EVERY,
            clients_dirty: false,
            last_clients_event: None,
            verified_marks: HashMap::new(),
            prices: PriceCache::default(),
        }
    }

    /// The runtime, while it lives (the writer must not keep it alive).
    fn runtime(&self) -> Option<Arc<LlmRuntime>> {
        self.llm.upgrade()
    }

    fn run(mut self, rx: &Receiver<Message>) {
        self.maintain();
        loop {
            match rx.recv_timeout(self.wait()) {
                Ok(Message::Job(job, bytes)) => {
                    self.queued_capture_bytes
                        .fetch_sub(bytes, Ordering::Relaxed);
                    self.write(*job);
                }
                Ok(Message::Flush(done)) => {
                    self.roll_up_overflow();
                    self.flush();
                    let _ = done.send(());
                }
                Ok(Message::Stop) | Err(RecvTimeoutError::Disconnected) => break,
                Err(RecvTimeoutError::Timeout) => {}
            }
            self.roll_up_overflow();
            if self.llm.strong_count() == 0 {
                break;
            }
            self.tick();
        }
        self.roll_up_overflow();
        self.flush();
    }

    /// The usage of the jobs a full queue refused goes into the rollup; their rows stay lost.
    fn roll_up_overflow(&mut self) {
        let jobs =
            std::mem::take(&mut *self.overflow.lock().unwrap_or_else(PoisonError::into_inner));
        if jobs.is_empty() {
            return;
        }
        let Some(llm) = self.runtime() else {
            return;
        };
        let (version, tz_offset_minutes) = (llm.version(), llm.tz_offset_minutes());
        if self.connection().is_none() {
            return;
        }
        let Some(connection) = self.connection.as_ref() else {
            return;
        };
        for LogJob {
            mut entry, usage, ..
        } in jobs
        {
            if !counts_in_usage(&entry) {
                continue;
            }
            let delta = priced_delta(
                &mut self.prices,
                connection,
                version,
                tz_offset_minutes,
                &mut entry,
                usage.as_ref(),
            );
            // Best effort, like the row it stands for.
            let _ = store::upsert_rollup(connection, &delta);
        }
    }

    /// How long to wait for the next job before something else is due.
    fn wait(&self) -> Duration {
        let now = Instant::now();
        let mut due = self.next_maintenance;
        if !self.pending.is_empty() {
            due = due.min(self.batch_due(now));
        }
        if self.clients_dirty {
            due = due.min(self.clients_due(now));
        }
        due.saturating_duration_since(now)
    }

    /// When the next batch may go out: `now` when none went yet (a later clock reading would
    /// never compare as due against the `now` the caller holds).
    fn batch_due(&self, now: Instant) -> Instant {
        self.last_batch.map_or(now, |at| at + BATCH_EVERY)
    }

    /// When the next `ClientsChanged` may go out (see [`Writer::batch_due`]).
    fn clients_due(&self, now: Instant) -> Instant {
        self.last_clients_event.map_or(now, |at| at + CLIENTS_EVERY)
    }

    fn tick(&mut self) {
        let now = Instant::now();
        if !self.pending.is_empty() && now >= self.batch_due(now) {
            self.flush();
        }
        if self.clients_dirty && now >= self.clients_due(now) {
            self.clients_dirty = false;
            self.last_clients_event = Some(now);
            if let Some(llm) = self.runtime() {
                llm.emit(LlmEvent::ClientsChanged);
            }
        }
        if now >= self.next_maintenance {
            self.maintain();
        }
    }

    /// A stored row names a client: its `last_used` and today's count moved, and a passed gateway
    /// request is the client's "Verified" instant (written at most every
    /// [`VERIFIED_EVERY`] per client, so a busy client costs no write per request).
    fn note_client(&mut self, entry: &RequestLogEntry) {
        let Some(client) = entry.client_id.as_deref().filter(|id| !id.is_empty()) else {
            return;
        };
        self.clients_dirty = true;
        if entry.source != "gateway" {
            return;
        }
        // The first request counts once it finished (model allowed, upstream answered).
        if let (Some(llm), Some(request_id)) = (self.runtime(), entry.request_id.as_deref()) {
            llm.services
                .client_watch
                .first_requests
                .mark(client, request_id, entry.status);
        }
        if !(200..300).contains(&entry.status) {
            return;
        }
        let now = Instant::now();
        if self
            .verified_marks
            .get(client)
            .is_some_and(|at| now.duration_since(*at) < VERIFIED_EVERY)
        {
            return;
        }
        self.verified_marks.insert(client.to_owned(), now);
        if let Some(connection) = self.connection.as_ref() {
            // A failed write only leaves the client unverified until the next passed request.
            let _ = store::mark_client_verified(connection, client, entry.ts_ms);
        }
    }

    /// Announces the stored rows as one batch.
    fn flush(&mut self) {
        if self.pending.is_empty() {
            return;
        }
        self.last_batch = Some(Instant::now());
        let entries = std::mem::take(&mut self.pending);
        if let Some(llm) = self.runtime() {
            llm.emit(LlmEvent::GatewayLogBatch { entries });
        }
    }

    fn connection(&mut self) -> Option<&DbConnection> {
        if self.connection.is_none() {
            self.connection = self.runtime().and_then(|llm| llm.db().open().ok());
        }
        self.connection.as_deref()
    }

    /// Retention of the log, the captures and the usage rollup.
    fn maintain(&mut self) {
        self.next_maintenance = Instant::now() + MAINTENANCE_EVERY;
        self.prune_log();
        let Some(llm) = self.runtime() else {
            return;
        };
        let retention = llm.settings().ai.providers.usage_retention_days;
        if let Some(connection) = self.connection() {
            // Best effort: a failed prune is retried at the next trigger.
            let _ = store::prune_usage(connection, retention, now_ms());
        }
    }

    /// The stored captures passed the byte cap (a prune brings them back under it).
    fn captures_over_cap(&mut self) -> bool {
        self.connection()
            .and_then(|connection| store::capture_bytes_total(connection).ok())
            .is_some_and(|bytes| bytes > CAPTURE_MAX_BYTES)
    }

    fn prune_log(&mut self) {
        self.inserts_since_prune = 0;
        let limits = PruneLimits::default();
        if let Some(connection) = self.connection() {
            let _ = store::prune_log(connection, &limits, now_ms());
        }
    }

    /// Redacts, prices, rolls up and stores one job.
    fn write(&mut self, job: LogJob) {
        let Some(llm) = self.runtime() else {
            return;
        };
        let wanted = should_capture(
            llm.settings().ai.gateway.capture_mode,
            capture_source(&job.entry.source),
            job.entry.status,
            log_outcome(&job.entry.outcome),
            job.any_attempt_failed,
            job.keep_exchange,
        );
        let LogJob {
            mut entry,
            usage,
            raw,
            ..
        } = job;
        let capture = raw.filter(|_| wanted);
        let has_attempts = entry.attempts.as_array().is_some_and(|a| !a.is_empty());
        // Read per row, never cached: a secret stored or rotated since the last row (a key, a
        // token, a settings secret) is redacted from the very next one.
        let needles = if capture.is_some() || entry.error.is_some() || has_attempts {
            let mut needles = llm.secrets().active_redaction_needles();
            needles.extend(self.seen.snapshot());
            needles
        } else {
            Vec::new()
        };
        redact_row_texts(entry.error.as_mut(), &mut entry.attempts, &needles);
        let captured = capture.map(|raw| {
            let hidden = extra_header_names(&llm);
            redact_exchange_hiding(raw, &needles, &hidden).0
        });

        if self.connection().is_none() {
            self.dropped.fetch_add(1, Ordering::Relaxed);
            return;
        }
        let version = llm.version();
        let tz_offset_minutes = llm.tz_offset_minutes();
        let stored = {
            let Some(connection) = self.connection.as_ref() else {
                return;
            };
            let delta = counts_in_usage(&entry).then(|| {
                priced_delta(
                    &mut self.prices,
                    connection,
                    version,
                    tz_offset_minutes,
                    &mut entry,
                    usage.as_ref(),
                )
            });
            let insert = |entry: &RequestLogEntry| {
                store::insert_log(
                    connection,
                    &LogWrite {
                        entry,
                        capture: captured.as_ref(),
                        rollup: delta.as_ref(),
                    },
                )
            };
            let mut stored = insert(&entry);
            // Defence against an id that already names a stored row (64 random bits make it
            // vanishingly rare): the row and its rollup are kept under a fresh id. The client was
            // already sent the first id, so only this path lets the two differ.
            let taken = stored.is_err()
                && entry.request_id.as_deref().is_some_and(|id| {
                    matches!(store::log_id_for_request_id(connection, id), Ok(Some(_)))
                });
            if taken {
                entry.request_id = Some(store::new_request_id());
                stored = insert(&entry);
            }
            stored
        };
        match stored {
            Ok(id) => {
                entry.id = id;
                self.note_client(&entry);
                let entry_had_capture = captured.is_some();
                entry.has_capture = entry_had_capture;
                // The announced row equals the one a read returns, derived words included.
                store::derive_log_reason(&mut entry);
                self.pending.push(entry);
                self.inserts_since_prune += 1;
                if self.inserts_since_prune >= PRUNE_EVERY_INSERTS
                    || (entry_had_capture && self.captures_over_cap())
                {
                    self.prune_log();
                }
            }
            Err(_) => {
                self.dropped.fetch_add(1, Ordering::Relaxed);
            }
        }
    }
}

/// Whether `entry` was routed to a configured provider and belongs in usage statistics.
/// Pre-routing refusals (bad token, unknown model, quota, body too large) and ad-hoc custom URLs
/// remain in `llm_request_log` for debugging, never as a synthetic "Unrouted" provider in rollups.
fn counts_in_usage(entry: &RequestLogEntry) -> bool {
    entry
        .provider_id
        .as_deref()
        .is_some_and(|id| !id.is_empty())
}

/// Prices `entry` (its cost columns and `price_source`, C-$5) and returns its rollup delta.
fn priced_delta(
    prices: &mut PriceCache,
    connection: &DbConnection,
    version: u64,
    tz_offset_minutes: i16,
    entry: &mut RequestLogEntry,
    usage: Option<&CostUsage>,
) -> RollupDelta {
    let resolution = entry
        .provider_id
        .as_deref()
        .zip(entry.upstream_model.as_deref())
        .and_then(|(provider, model)| prices.lookup(connection, version, provider, model))
        .unwrap_or_default();
    store::book_entry(entry, &resolution, usage, tz_offset_minutes)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used)]

    use std::sync::Arc;

    use super::{LogJob, LogQueue};
    use crate::llm::runtime::now_ms;
    use crate::llm::store::{self, RequestLogEntry};

    // A full queue costs log rows, never the usage behind them.
    #[test]
    fn a_full_queue_loses_rows_but_never_their_usage() {
        let h = super::super::tests::Harness::build(|_| {});
        let queue = LogQueue::spawn(Arc::clone(&h.llm), Arc::default(), 1).unwrap();
        for _ in 0..200 {
            queue.submit(LogJob::from_entry(RequestLogEntry {
                ts_ms: now_ms(),
                source: "gateway".into(),
                provider_id: Some("p".into()),
                status: 200,
                outcome: "success".into(),
                requested_model: "m".into(),
                input_tokens: Some(10),
                output_tokens: Some(1),
                ..RequestLogEntry::default()
            }));
        }
        queue.flush();
        assert!(queue.dropped() > 0, "the queue never filled");
        let rows = store::today_gateway_rows(&h.db, now_ms(), h.llm.tz_offset_minutes()).unwrap();
        let requests: u64 = rows.iter().map(|row| row.requests).sum();
        let input: u64 = rows.iter().map(|row| row.input_tokens).sum();
        assert_eq!((requests, input), (200, 2000));
    }

    #[test]
    fn unrouted_rows_are_logged_but_excluded_from_usage_rollups() {
        let h = super::super::tests::Harness::build(|_| {});
        let queue = LogQueue::spawn(Arc::clone(&h.llm), Arc::default(), 16).unwrap();
        queue.submit(LogJob::from_entry(RequestLogEntry {
            ts_ms: now_ms(),
            source: "gateway".into(),
            provider_id: None,
            status: 401,
            outcome: "failure".into(),
            requested_model: String::new(),
            error: Some("bad token".into()),
            ..RequestLogEntry::default()
        }));
        queue.submit(LogJob::from_entry(RequestLogEntry {
            ts_ms: now_ms(),
            source: "agent".into(),
            provider_id: Some("p".into()),
            key_id: Some("k".into()),
            status: 200,
            outcome: "success".into(),
            requested_model: "p/m".into(),
            upstream_model: Some("m".into()),
            input_tokens: Some(12),
            output_tokens: Some(4),
            ..RequestLogEntry::default()
        }));
        queue.flush();
        let logged = store::list_log(&h.db, &store::LogPage::default()).unwrap();
        assert_eq!(logged.len(), 2, "both rows stay in the call log");
        let daily: i64 =
            h.db.query_row("SELECT COUNT(*) FROM llm_usage_daily", [], |r| r.get(0))
                .unwrap();
        let minute: i64 =
            h.db.query_row("SELECT COUNT(*) FROM llm_usage_minute", [], |r| r.get(0))
                .unwrap();
        let failures: i64 =
            h.db.query_row("SELECT COUNT(*) FROM llm_usage_failures", [], |r| r.get(0))
                .unwrap();
        assert_eq!((daily, minute, failures), (1, 1, 0));
    }
}
