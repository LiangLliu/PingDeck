//! The run registry: every active run and the ones that ended in the last 10 minutes, their
//! replayable event hubs, subscriptions, the top-level concurrency slots and the per-chat queue
//! pause state.
//!
//! Single responsibility: shared bookkeeping of runs. It starts nothing and persists nothing; the
//! run lifecycle (`app::agent_service::run`) creates [`ActiveRun`]s here and drives them. A run's
//! event stream is an [`EventHub`] of 4,096 events or 1 MiB with adjacent text deltas merged (a
//! merged command output keeps its last 16 KiB), so a late subscriber replays what it missed, and one that asks for events already evicted first
//! receives [`AgentEvent::Gap`]. Subscribers are counted per chat ([`RunRegistry::watchers`]):
//! the approval timeout rule (`docs/architecture.md` §11.2, `docs/decisions.md` §5 D-119) asks whether anyone watches the chat.

use std::collections::{HashMap, HashSet, VecDeque};
use std::sync::atomic::{AtomicBool, AtomicU32, Ordering};
use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::{Duration, Instant};

use tokio::sync::{Notify, watch};

use pingdeck_base::cancel::CancelToken;
use pingdeck_base::error::AppError;
use pingdeck_llm::llm::hub::HubPart;
use pingdeck_model::events::{EventHub, EventSink, Sequenced};

use super::attention::{FinishedRun, SessionRunStateDto};
use super::dto::{AgentRunListFilter, AgentSubscribeTarget};
use super::failure::AgentFailure;
use super::model::{
    AgentEnvelope, AgentEvent, AgentRunKind, AgentRunStatus, AgentRunSummary, LiveRunSnapshot,
};
use live::LiveAcc;

pub use super::limits::{REPLAY_BYTES, REPLAY_EVENTS};
/// Ended runs kept for the away-from-app signal.
const FINISHED_KEPT: usize = 8;
/// How long a finished run stays subscribable.
pub const RETAIN_FINISHED: Duration = Duration::from_secs(10 * 60);
/// Subscribers one run's hub serves.
const MAX_SUBSCRIBERS: usize = 64;

/// The replay hub of one run.
pub type RunHub = EventHub<AgentEvent>;

/// Counts the bytes written to it, so an event's JSON size is measured without building the text.
struct ByteCount(usize);

impl std::io::Write for ByteCount {
    fn write(&mut self, bytes: &[u8]) -> std::io::Result<usize> {
        self.0 += bytes.len();
        Ok(bytes.len())
    }

    fn flush(&mut self) -> std::io::Result<()> {
        Ok(())
    }
}

/// A rough size of `event` for the replay byte limit: the text it carries plus a fixed overhead.
/// Cheap on purpose (the hub calls it on every emit, outside its lock); the rare big variants are
/// measured by their JSON length.
fn event_size(event: &AgentEvent) -> usize {
    const OVERHEAD: usize = 64;
    OVERHEAD
        + match event {
            AgentEvent::TextDelta { text }
            | AgentEvent::ReasoningDelta { text }
            | AgentEvent::ToolOutputDelta { text, .. }
            | AgentEvent::Notice { text, .. } => text.len(),
            AgentEvent::ToolCallArgsDelta { arguments, .. } => arguments.len(),
            other => {
                let mut counter = ByteCount(0);
                serde_json::to_writer(&mut counter, other).map_or(OVERHEAD, |()| counter.0)
            }
        }
}

/// Bytes of one call's folded output a replay keeps: its tail. Command output is the one event
/// kind that can fill the ring on its own; keeping only its tail when old events are folded frees
/// the room the structural events (approval cards, usage, tool results) need to stay replayable.
/// The whole output is in the call's result and its spill file.
pub const FOLDED_OUTPUT_TAIL: usize = 16 * 1024;
/// What stands for the output a fold dropped.
const FOLDED_OUTPUT_MARK: &str = "…\n";

/// Cuts a folded output text to its last [`FOLDED_OUTPUT_TAIL`] bytes, at a character boundary,
/// behind [`FOLDED_OUTPUT_MARK`].
fn keep_output_tail(text: &mut String) {
    if text.len() <= FOLDED_OUTPUT_TAIL + FOLDED_OUTPUT_MARK.len() {
        return;
    }
    let mut cut = text.len() - FOLDED_OUTPUT_TAIL;
    while !text.is_char_boundary(cut) {
        cut += 1;
    }
    text.replace_range(..cut, FOLDED_OUTPUT_MARK);
}

/// Folds `next` into `prev` when both are the same kind of text fragment of the same stream; a
/// folded command output keeps only its tail ([`FOLDED_OUTPUT_TAIL`]).
fn merge_events(prev: &mut AgentEvent, next: &AgentEvent) -> bool {
    match (prev, next) {
        (AgentEvent::TextDelta { text }, AgentEvent::TextDelta { text: more })
        | (AgentEvent::ReasoningDelta { text }, AgentEvent::ReasoningDelta { text: more }) => {
            text.push_str(more);
            true
        }
        (
            AgentEvent::ToolOutputDelta {
                call_id,
                stream,
                text,
            },
            AgentEvent::ToolOutputDelta {
                call_id: next_call,
                stream: next_stream,
                text: more,
            },
        ) if call_id == next_call && stream == next_stream => {
            text.push_str(more);
            keep_output_tail(text);
            true
        }
        (
            AgentEvent::ToolCallArgsDelta { call_id, arguments },
            AgentEvent::ToolCallArgsDelta {
                call_id: next_call,
                arguments: more,
            },
        ) if call_id == next_call => {
            arguments.push_str(more);
            true
        }
        _ => false,
    }
}

fn lock<T>(mutex: &Mutex<T>) -> MutexGuard<'_, T> {
    mutex.lock().unwrap_or_else(PoisonError::into_inner)
}

/// A message the user sent into a running turn (`agent_run_steer`).
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SteerMessage {
    /// Text and attachment references, as for any user message.
    pub parts: Vec<HubPart>,
    /// Skills attached to it, written to the stored message's `meta.skills`.
    pub skills: Vec<String>,
}

/// What the user decided at the cost cap.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CostDecision {
    /// Run on; the next pause comes one more cap later.
    Continue,
    /// End the run.
    Stop,
}

/// One run, active or recently finished.
pub struct ActiveRun {
    /// Run id.
    pub run_id: String,
    /// The chat the run answers in.
    pub session_id: String,
    /// The run that spawned this one as a sub-agent.
    pub parent_run_id: Option<String>,
    /// What the run is.
    pub kind: AgentRunKind,
    /// The run's replayable event stream.
    pub hub: RunHub,
    /// Cancels the run; a sub-agent's token is a child of its parent's.
    pub cancel: CancelToken,
    summary: Mutex<AgentRunSummary>,
    steer: Mutex<VecDeque<SteerMessage>>,
    steer_notify: Notify,
    /// What the run and its sub-agents have spent, by currency (millionths); never summed across.
    spend: Mutex<Vec<(String, i64)>>,
    /// Spend by part, unpriced models, tokens and cap passes of the run tree.
    ledger: Mutex<RunLedger>,
    /// The top-level slot a top-level run holds; freed while anything in its tree waits for the
    /// user.
    slot: Mutex<Option<SlotGuard>>,
    holds_slot: AtomicBool,
    waits: AtomicU32,
    subagents_started: AtomicU32,
    finished_at: Mutex<Option<Instant>>,
    ended: Notify,
    /// The next run of the chat is started by whoever approved the plan, not by the queue.
    handed_over: AtomicBool,
    /// When the run was created; an `Instant` does not run while the machine sleeps, so this
    /// measures awake time.
    started: Instant,
    approval_timed_out: AtomicBool,
    superseded: Mutex<HashSet<String>>,
    /// The failure class the run loop reported just before it failed.
    failure: Mutex<Option<AgentFailure>>,
    /// The unstored part of the answer; locked around every `hub.emit` (see [`ActiveRun::emit`]).
    live: Mutex<LiveAcc>,
}

impl ActiveRun {
    /// A run that has not started yet; `summary` is its initial row (`Queued`).
    #[must_use]
    pub fn new(summary: AgentRunSummary, cancel: CancelToken) -> Arc<Self> {
        Arc::new(Self {
            run_id: summary.run_id.clone(),
            session_id: summary.session_id.clone(),
            parent_run_id: summary.parent_run_id.clone(),
            kind: summary.kind,
            hub: EventHub::with_limits(
                MAX_SUBSCRIBERS,
                REPLAY_EVENTS,
                REPLAY_BYTES,
                event_size,
                Some(merge_events),
            ),
            cancel,
            summary: Mutex::new(summary),
            steer: Mutex::new(VecDeque::new()),
            steer_notify: Notify::new(),
            spend: Mutex::new(Vec::new()),
            ledger: Mutex::default(),
            slot: Mutex::new(None),
            holds_slot: AtomicBool::new(false),
            waits: AtomicU32::new(0),
            subagents_started: AtomicU32::new(0),
            finished_at: Mutex::new(None),
            ended: Notify::new(),
            handed_over: AtomicBool::new(false),
            started: Instant::now(),
            approval_timed_out: AtomicBool::new(false),
            superseded: Mutex::new(HashSet::new()),
            failure: Mutex::new(None),
            live: Mutex::new(LiveAcc::default()),
        })
    }

    /// Publishes `event` to the run's hub and stamps the run's last-event time. The live view
    /// folds the event under the lock a snapshot takes, so `live_snapshot` never sees one without
    /// the other.
    pub fn emit(&self, event: AgentEvent) -> u64 {
        lock(&self.summary).last_event_ms = Some(pingdeck_base::time::now_ms());
        let mut live = lock(&self.live);
        live.apply(&event);
        self.hub.emit(event)
    }

    /// What the run has streamed that no stored message holds yet, with the position of the last
    /// event it includes: subscribing from that `seq` continues it without a gap or a duplicate.
    #[must_use]
    pub fn live_snapshot(&self) -> LiveRunSnapshot {
        let live = lock(&self.live);
        live.snapshot(&self.run_id, self.hub.last_seq())
    }

    /// The run as the UI sees it, with the live `last_seq`.
    #[must_use]
    pub fn summary(&self) -> AgentRunSummary {
        let mut summary = lock(&self.summary).clone();
        summary.last_seq = self.hub.last_seq();
        summary
    }

    /// Changes the run's summary and returns the result.
    pub fn update(&self, change: impl FnOnce(&mut AgentRunSummary)) -> AgentRunSummary {
        let mut summary = lock(&self.summary);
        change(&mut summary);
        let mut snapshot = summary.clone();
        drop(summary);
        snapshot.last_seq = self.hub.last_seq();
        snapshot
    }

    /// The run's lifecycle state.
    #[must_use]
    pub fn status(&self) -> AgentRunStatus {
        lock(&self.summary).status
    }

    /// Queues a user message for the next step boundary and wakes a waiting approval.
    pub fn push_steer(&self, message: SteerMessage) {
        lock(&self.steer).push_back(message);
        self.steer_notify.notify_waiters();
    }

    /// Takes every queued steer message, oldest first.
    #[must_use]
    pub fn take_steer(&self) -> Vec<SteerMessage> {
        lock(&self.steer).drain(..).collect()
    }

    /// Whether a steer message is queued.
    #[must_use]
    pub fn has_steer(&self) -> bool {
        !lock(&self.steer).is_empty()
    }

    /// Resolves when a steer message is queued (at once when one already is).
    pub async fn steered(&self) {
        loop {
            let mut notified = std::pin::pin!(self.steer_notify.notified());
            notified.as_mut().enable();
            if self.has_steer() {
                return;
            }
            notified.await;
        }
    }

    /// Resolves once the run is finished in the registry (at once when it already is).
    pub async fn ended(&self) {
        loop {
            let mut notified = std::pin::pin!(self.ended.notified());
            notified.as_mut().enable();
            if lock(&self.finished_at).is_some() {
                return;
            }
            notified.await;
        }
    }

    /// The chat's next run is started by the caller (an approved plan), so the end of this run
    /// does not start the queue.
    pub fn hand_over(&self) {
        self.handed_over.store(true, Ordering::SeqCst);
    }

    /// Whether [`ActiveRun::hand_over`] was called.
    #[must_use]
    pub fn handed_over(&self) -> bool {
        self.handed_over.load(Ordering::SeqCst)
    }

    /// Adds `micro` of `currency` spent by this run or one of its sub-agents (it counts against
    /// the cap in that currency).
    pub fn add_cost(&self, currency: &str, micro: i64) {
        let mut spend = lock(&self.spend);
        match spend.iter_mut().find(|(c, _)| c == currency) {
            Some((_, total)) => *total = total.saturating_add(micro),
            None => spend.push((currency.to_owned(), micro)),
        }
    }

    /// Adds a call of the run tree to the ledger: `micro` of `currency` (`None` when the model has
    /// no price: `model` is then listed as unpriced) spent by `part` (`main`, `subagent` or
    /// `compaction`), and its tokens.
    pub fn note_call(
        &self,
        part: &'static str,
        priced: Option<(&str, i64)>,
        model: &str,
        tokens: u64,
    ) {
        let mut ledger = lock(&self.ledger);
        ledger.tokens = ledger.tokens.saturating_add(tokens);
        match priced {
            Some((currency, micro)) => {
                match ledger
                    .parts
                    .iter_mut()
                    .find(|(p, c, _)| *p == part && c == currency)
                {
                    Some((_, _, total)) => *total = total.saturating_add(micro),
                    None => ledger.parts.push((part, currency.to_owned(), micro)),
                }
            }
            None if !model.is_empty() && !ledger.unpriced.iter().any(|m| m == model) => {
                ledger.unpriced.push(model.to_owned());
            }
            None => {}
        }
    }

    /// What each part of the run tree spent in `currency`, in the order first spent.
    #[must_use]
    pub fn spend_by_part(&self, currency: &str) -> Vec<(&'static str, i64)> {
        lock(&self.ledger)
            .parts
            .iter()
            .filter(|(_, c, _)| c == currency)
            .map(|(part, _, micro)| (*part, *micro))
            .collect()
    }

    /// Models the run tree called that have no price (their spend counts against no cap).
    #[must_use]
    pub fn unpriced_models(&self) -> Vec<String> {
        lock(&self.ledger).unpriced.clone()
    }

    /// Tokens (input and output) the run tree used.
    #[must_use]
    pub fn tokens_used(&self) -> u64 {
        lock(&self.ledger).tokens
    }

    /// How many times the user let the run tree go past the cap named `key` (a currency, or
    /// `tokens`).
    #[must_use]
    pub fn cap_passes(&self, key: &str) -> i64 {
        lock(&self.ledger)
            .cap_passes
            .iter()
            .find(|(k, _)| k == key)
            .map_or(0, |(_, passes)| *passes)
    }

    /// Records the passes of the cap named `key`.
    pub fn set_cap_passes(&self, key: &str, passes: i64) {
        let mut ledger = lock(&self.ledger);
        match ledger.cap_passes.iter_mut().find(|(k, _)| k == key) {
            Some((_, stored)) => *stored = passes,
            None => ledger.cap_passes.push((key.to_owned(), passes)),
        }
    }

    /// Counts one more `task` call of this run against `max`; `false` (nothing counted) once
    /// `max` calls were made.
    pub fn claim_subagent(&self, max: u32) -> bool {
        self.subagents_started
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |started| {
                (started < max).then_some(started + 1)
            })
            .is_ok()
    }

    /// Keeps the top-level slot the run took.
    pub fn hold_slot(&self, guard: SlotGuard) {
        *lock(&self.slot) = Some(guard);
        self.holds_slot.store(true, Ordering::SeqCst);
    }

    /// Frees the run's slot for good (the run is over).
    pub fn drop_slot(&self) {
        self.holds_slot.store(false, Ordering::SeqCst);
        lock(&self.slot).take();
    }

    /// One more wait for the user (an approval or a cost pause) in this run's tree; the first one
    /// frees the tree's top-level slot, so a sub-agent waiting for an approval does not hold it.
    pub fn begin_wait(&self) {
        if self.waits.fetch_add(1, Ordering::SeqCst) == 0 {
            lock(&self.slot).take();
        }
    }

    /// One wait ended; `true` when it was the last one and the run must take a slot back.
    #[must_use]
    pub fn end_wait(&self) -> bool {
        let before = self
            .waits
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |waits| {
                waits.checked_sub(1)
            })
            .unwrap_or(0);
        before == 1 && self.holds_slot.load(Ordering::SeqCst) && lock(&self.slot).is_none()
    }

    /// Gives back a `task` call counted by [`ActiveRun::claim_subagent`] whose sub-agent never
    /// started.
    pub fn release_subagent(&self) {
        let _ = self
            .subagents_started
            .try_update(Ordering::SeqCst, Ordering::SeqCst, |started| {
                started.checked_sub(1)
            });
    }

    /// What the run and its sub-agents have spent in `currency`, in millionths.
    #[must_use]
    pub fn total_cost_micro(&self, currency: &str) -> i64 {
        lock(&self.spend)
            .iter()
            .find(|(c, _)| c == currency)
            .map_or(0, |(_, total)| *total)
    }

    /// What the run and its sub-agents have spent, per currency, in the order first spent.
    #[must_use]
    pub fn spend(&self) -> Vec<(String, i64)> {
        lock(&self.spend).clone()
    }

    /// Elapsed run time without machine sleep, in milliseconds (frozen at the end).
    #[must_use]
    pub fn awake_ms(&self) -> u64 {
        let end = lock(&self.finished_at).unwrap_or_else(Instant::now);
        u64::try_from(end.saturating_duration_since(self.started).as_millis()).unwrap_or(u64::MAX)
    }

    /// Remembers that a tool approval of this run ended timed out.
    pub fn mark_approval_timed_out(&self) {
        self.approval_timed_out.store(true, Ordering::SeqCst);
    }

    /// Whether a tool approval of this run ended timed out.
    #[must_use]
    pub fn approval_timed_out(&self) -> bool {
        self.approval_timed_out.load(Ordering::SeqCst)
    }

    /// Marks the `task` call `call_id` as superseded by a steer message: its sub-agent was
    /// cancelled for that reason and the call ends `superseded`, not `error`.
    pub fn mark_superseded(&self, call_id: &str) {
        lock(&self.superseded).insert(call_id.to_owned());
    }

    /// Whether `call_id` was marked superseded (consumes the mark).
    #[must_use]
    pub fn take_superseded(&self, call_id: &str) -> bool {
        lock(&self.superseded).remove(call_id)
    }

    /// Remembers the failure class of the model call that is about to end the run.
    pub fn set_failure(&self, failure: AgentFailure) {
        *lock(&self.failure) = Some(failure);
    }

    /// Takes the failure class [`ActiveRun::set_failure`] left, if any.
    #[must_use]
    pub fn take_failure(&self) -> Option<AgentFailure> {
        lock(&self.failure).take()
    }
}

/// A subscriber of one or more hubs; counts as a watcher of its chat while `alive`.
struct Watch {
    alive: AtomicBool,
    changed: watch::Sender<u64>,
}

impl Watch {
    fn die(&self) {
        if self.alive.swap(false, Ordering::SeqCst) {
            self.changed.send_modify(|version| *version += 1);
        }
    }
}

/// The consumer's sink, plus the bookkeeping that notices when the consumer is gone.
struct WatchedSink {
    inner: Arc<dyn EventSink<AgentEnvelope>>,
    watch: Arc<Watch>,
}

impl EventSink<AgentEnvelope> for WatchedSink {
    fn emit(&self, event: AgentEnvelope) -> bool {
        let delivered = self.inner.emit(event);
        if !delivered {
            self.watch.die();
        }
        delivered
    }

    fn is_gone(&self) -> bool {
        !self.watch.alive.load(Ordering::SeqCst) || self.inner.is_gone()
    }
}

/// Names the run on every event a hub delivers to a chat or run subscription.
struct RunSink {
    run_id: String,
    sink: Arc<WatchedSink>,
}

impl RunSink {
    fn tagged(
        run: &ActiveRun,
        sink: &Arc<WatchedSink>,
    ) -> Arc<dyn EventSink<Sequenced<AgentEvent>>> {
        Arc::new(Self {
            run_id: run.run_id.clone(),
            sink: Arc::clone(sink),
        })
    }
}

impl EventSink<Sequenced<AgentEvent>> for RunSink {
    fn emit(&self, event: Sequenced<AgentEvent>) -> bool {
        self.sink.emit(AgentEnvelope::of_run(&self.run_id, event))
    }

    fn is_gone(&self) -> bool {
        self.sink.is_gone()
    }
}

/// One registered subscription.
struct Subscription {
    /// The chat that counts as watched.
    session_id: String,
    /// A chat subscription follows every run of the chat; a run subscription only one.
    follows_session: bool,
    watch: Arc<Watch>,
    sink: Arc<WatchedSink>,
    attached: Vec<(Arc<ActiveRun>, u64)>,
}

impl Subscription {
    /// Whether the consumer is still there: it took the last event and is not known to be gone.
    fn watching(&self) -> bool {
        self.watch.alive.load(Ordering::SeqCst) && !self.sink.is_gone()
    }
}

#[derive(Default)]
struct State {
    runs: HashMap<String, Arc<ActiveRun>>,
    /// The active run of each chat.
    active: HashMap<String, String>,
    /// The latest run of each chat, active or not.
    latest: HashMap<String, String>,
    paused: HashSet<String>,
    /// Ended top-level runs the user has not opened: run id → chat.
    unseen: HashMap<String, String>,
    /// The latest ended top-level runs, newest last (the away-from-app signal reads them).
    finished: VecDeque<FinishedRun>,
    subs: HashMap<u64, Subscription>,
    next_sub: u64,
    /// Each chat's pruning watermark and learned window (in memory only).
    context: HashMap<String, crate::agent::context::ContextState>,
}

/// The run tree's spend by part, its unpriced models, tokens and cap passes.
#[derive(Default)]
struct RunLedger {
    parts: Vec<(&'static str, String, i64)>,
    unpriced: Vec<String>,
    tokens: u64,
    cap_passes: Vec<(String, i64)>,
}

/// Counts running top-level runs against `ai.agent.max_concurrent_runs`.
#[derive(Default)]
struct Slots {
    used: Mutex<usize>,
    freed: Notify,
}

/// A held concurrency slot; dropping it frees the slot and wakes a waiting run.
pub struct SlotGuard {
    slots: Arc<Slots>,
}

impl Drop for SlotGuard {
    fn drop(&mut self) {
        {
            let mut used = lock(&self.slots.used);
            *used = used.saturating_sub(1);
        }
        self.slots.freed.notify_waiters();
    }
}

/// Every run of the app.
pub struct RunRegistry {
    state: Mutex<State>,
    watchers_changed: watch::Sender<u64>,
    slots: Arc<Slots>,
}

impl Default for RunRegistry {
    fn default() -> Self {
        Self {
            state: Mutex::default(),
            watchers_changed: watch::channel(0).0,
            slots: Arc::default(),
        }
    }
}

impl RunRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    fn state(&self) -> MutexGuard<'_, State> {
        lock(&self.state)
    }

    /// Adds `run` as its chat's active run and attaches the chat's subscriptions to its hub.
    ///
    /// # Errors
    /// [`AppError::Conflict`] when the chat already has an active run.
    pub fn register(&self, run: &Arc<ActiveRun>) -> Result<(), AppError> {
        let mut state = self.state();
        Self::prune(&mut state, Instant::now());
        if state.active.contains_key(&run.session_id) {
            return Err(AppError::conflict("this chat is already answering"));
        }
        state.runs.insert(run.run_id.clone(), Arc::clone(run));
        state
            .active
            .insert(run.session_id.clone(), run.run_id.clone());
        state
            .latest
            .insert(run.session_id.clone(), run.run_id.clone());
        for subscription in state
            .subs
            .values_mut()
            .filter(|sub| sub.follows_session && sub.session_id == run.session_id)
        {
            if subscription.watching()
                && let Ok(id) = run
                    .hub
                    .subscribe(None, RunSink::tagged(run, &subscription.sink))
            {
                subscription.attached.push((Arc::clone(run), id));
            }
        }
        Ok(())
    }

    /// Takes a run that never started out of the registry (its start failed).
    pub fn discard(&self, run_id: &str) {
        let mut state = self.state();
        if let Some(run) = state.runs.remove(run_id) {
            if state.active.get(&run.session_id).map(String::as_str) == Some(run_id) {
                state.active.remove(&run.session_id);
            }
            if state.latest.get(&run.session_id).map(String::as_str) == Some(run_id) {
                state.latest.remove(&run.session_id);
            }
        }
    }

    /// Marks `run_id` finished: it frees its chat and stays subscribable for 10 minutes.
    pub fn finish(&self, run_id: &str) {
        let mut state = self.state();
        let Some(run) = state.runs.get(run_id).cloned() else {
            return;
        };
        *lock(&run.finished_at) = Some(Instant::now());
        if state.active.get(&run.session_id).map(String::as_str) == Some(run_id) {
            state.active.remove(&run.session_id);
        }
        run.ended.notify_waiters();
    }

    /// Forgets finished runs older than [`RETAIN_FINISHED`] at `now`, and subscriptions whose
    /// consumer is gone.
    fn prune(state: &mut State, now: Instant) {
        // A subscription whose consumer is gone (a reloaded webview never unsubscribes) would
        // otherwise keep its run Arcs for as long as the app lives; each hub drops its own dead
        // sink on its next emit.
        state.subs.retain(|_, sub| {
            if sub.sink.is_gone() {
                sub.watch.die();
            }
            sub.watch.alive.load(Ordering::SeqCst)
        });
        let expired: Vec<String> = state
            .runs
            .iter()
            .filter(|(_, run)| {
                lock(&run.finished_at)
                    .is_some_and(|at| now.saturating_duration_since(at) >= RETAIN_FINISHED)
            })
            .map(|(id, _)| id.clone())
            .collect();
        for id in expired {
            if let Some(run) = state.runs.remove(&id) {
                if state.latest.get(&run.session_id) == Some(&id) {
                    state.latest.remove(&run.session_id);
                }
                for subscription in state.subs.values_mut() {
                    subscription
                        .attached
                        .retain(|(attached, _)| attached.run_id != id);
                }
            }
        }
    }

    /// Forgets finished runs that ended at least [`RETAIN_FINISHED`] before `now`; tests pass a
    /// later `now` instead of waiting.
    pub fn prune_at(&self, now: Instant) {
        Self::prune(&mut self.state(), now);
    }

    /// The run `run_id` while it is active or retained.
    #[must_use]
    pub fn get(&self, run_id: &str) -> Option<Arc<ActiveRun>> {
        let mut state = self.state();
        Self::prune(&mut state, Instant::now());
        state.runs.get(run_id).cloned()
    }

    /// The active run of `session_id`.
    #[must_use]
    pub fn active_for_session(&self, session_id: &str) -> Option<Arc<ActiveRun>> {
        let state = self.state();
        state
            .active
            .get(session_id)
            .and_then(|id| state.runs.get(id))
            .cloned()
    }

    /// Every run that has not finished.
    #[must_use]
    pub fn active_runs(&self) -> Vec<Arc<ActiveRun>> {
        let state = self.state();
        state
            .runs
            .values()
            .filter(|run| lock(&run.finished_at).is_none())
            .cloned()
            .collect()
    }

    /// The in-memory runs matching `filter` as summaries, newest first (not limited: the caller
    /// merges them with stored rows).
    #[must_use]
    pub fn summaries(&self, filter: &AgentRunListFilter) -> Vec<AgentRunSummary> {
        let mut state = self.state();
        Self::prune(&mut state, Instant::now());
        let mut rows: Vec<AgentRunSummary> = state
            .runs
            .values()
            .filter(|run| {
                filter
                    .session_id
                    .as_deref()
                    .is_none_or(|session| run.session_id == session)
                    && (!filter.active_only || run.status().is_active())
            })
            .map(|run| run.summary())
            .collect();
        rows.sort_by(|a, b| (b.started_at_ms, &b.run_id).cmp(&(a.started_at_ms, &a.run_id)));
        rows
    }

    /// Subscribes `sink` to `target` and returns the subscription id.
    ///
    /// A run target replays that run's buffer after `from_seq` (everything buffered for `None`);
    /// events already evicted are announced by [`AgentEvent::Gap`] before the replay. A chat target
    /// follows the chat's current run, or its latest one while it is retained, and every run
    /// that starts later.
    ///
    /// # Errors
    /// [`AppError::NotFound`] for an unknown or expired run, [`AppError::Cancelled`] when the sink
    /// is gone before the replay ends, [`AppError::Conflict`] when the run has too many
    /// subscribers.
    pub fn subscribe(
        &self,
        target: &AgentSubscribeTarget,
        from_seq: Option<u64>,
        sink: Arc<dyn EventSink<AgentEnvelope>>,
    ) -> Result<u64, AppError> {
        let watch = Arc::new(Watch {
            alive: AtomicBool::new(true),
            changed: self.watchers_changed.clone(),
        });
        let sink = Arc::new(WatchedSink {
            inner: sink,
            watch: Arc::clone(&watch),
        });
        let (session_id, follows_session, run) = {
            let mut state = self.state();
            Self::prune(&mut state, Instant::now());
            match target {
                AgentSubscribeTarget::Run { run_id } => {
                    let run = state.runs.get(run_id).cloned().ok_or_else(|| {
                        AppError::not_found(format!(
                            "run {run_id} is unknown or ended more than 10 minutes ago; reload the chat"
                        ))
                    })?;
                    (run.session_id.clone(), false, Some(run))
                }
                AgentSubscribeTarget::Session { session_id } => {
                    let run = state
                        .active
                        .get(session_id)
                        .or_else(|| state.latest.get(session_id))
                        .and_then(|id| state.runs.get(id))
                        .cloned();
                    (session_id.clone(), true, run)
                }
            }
        };
        let attached = match &run {
            Some(run) => {
                let id = Self::attach(run, from_seq, &sink)?;
                vec![(Arc::clone(run), id)]
            }
            None => Vec::new(),
        };
        let (id, missed) = {
            let mut state = self.state();
            // A run of the chat that registered between the two lock sections never saw this
            // subscription: attach it below.
            let missed = follows_session
                .then(|| {
                    state
                        .active
                        .get(&session_id)
                        .and_then(|id| state.runs.get(id))
                        .filter(|active| {
                            !attached.iter().any(|(run, _)| run.run_id == active.run_id)
                        })
                        .cloned()
                })
                .flatten();
            state.next_sub += 1;
            let id = state.next_sub;
            state.subs.insert(
                id,
                Subscription {
                    session_id,
                    follows_session,
                    watch,
                    sink: Arc::clone(&sink),
                    attached,
                },
            );
            (id, missed)
        };
        if let Some(run) = missed
            && let Ok(hub_id) = Self::attach(&run, None, &sink)
            && let Some(subscription) = self.state().subs.get_mut(&id)
        {
            subscription.attached.push((run, hub_id));
        }
        self.watchers_changed.send_modify(|version| *version += 1);
        Ok(id)
    }

    /// Subscribes `sink` to `run`'s hub, announcing evicted events with a `Gap` first.
    fn attach(
        run: &Arc<ActiveRun>,
        from_seq: Option<u64>,
        sink: &Arc<WatchedSink>,
    ) -> Result<u64, AppError> {
        let tagged = RunSink::tagged(run, sink);
        run.hub
            .subscribe_announcing_gap(from_seq, tagged, |first_seq| AgentEvent::Gap { first_seq })
    }

    /// Ends a subscription; returns whether it existed.
    pub fn unsubscribe(&self, subscription_id: u64) -> bool {
        let Some(subscription) = self.state().subs.remove(&subscription_id) else {
            return false;
        };
        for (run, id) in &subscription.attached {
            run.hub.unsubscribe(*id);
        }
        subscription.watch.die();
        self.watchers_changed.send_modify(|version| *version += 1);
        true
    }

    /// Drops the subscriptions whose consumer is known to be gone and wakes whoever counts
    /// watchers: the shell calls this when a page was reloaded or closed, since the old page's
    /// sinks have not been emitted to yet and would otherwise still count as watching.
    pub fn reap_gone(&self) {
        Self::prune(&mut self.state(), Instant::now());
    }

    /// How many live subscriptions watch `session_id`.
    #[must_use]
    pub fn watchers(&self, session_id: &str) -> usize {
        self.state()
            .subs
            .values()
            .filter(|sub| sub.session_id == session_id && sub.watching())
            .count()
    }

    /// Live subscriptions watching the chat of `run_id` or of any run above it (a sub-agent counts
    /// as watched while the user has its parent chat open).
    #[must_use]
    pub fn watchers_of_run(&self, run_id: &str) -> usize {
        let state = self.state();
        let mut sessions: HashSet<&str> = HashSet::new();
        let mut next = Some(run_id.to_owned());
        while let Some(id) = next {
            let Some(run) = state.runs.get(&id) else {
                break;
            };
            sessions.insert(run.session_id.as_str());
            next = run.parent_run_id.clone();
        }
        state
            .subs
            .values()
            .filter(|sub| sessions.contains(sub.session_id.as_str()) && sub.watching())
            .count()
    }

    /// A receiver that changes whenever a subscription starts or ends.
    #[must_use]
    pub fn watchers_changed(&self) -> watch::Receiver<u64> {
        self.watchers_changed.subscribe()
    }

    /// Wakes every gate wait to read its watch state again (the window focus changed).
    pub fn notify_watch_change(&self) {
        self.watchers_changed
            .send_modify(|version| *version = version.wrapping_add(1));
    }

    /// What `session_id` remembers about its context; empty after a restart.
    #[must_use]
    pub fn context_state(&self, session_id: &str) -> crate::agent::context::ContextState {
        self.state()
            .context
            .get(session_id)
            .cloned()
            .unwrap_or_default()
    }

    /// Keeps `state` for `session_id`'s next step and run.
    pub fn set_context_state(&self, session_id: &str, state: &crate::agent::context::ContextState) {
        self.state()
            .context
            .insert(session_id.to_owned(), state.clone());
    }

    /// Stops `session_id`'s queue from running on until [`RunRegistry::resume_queue`].
    pub fn pause_queue(&self, session_id: &str) {
        self.state().paused.insert(session_id.to_owned());
    }

    /// Lets the queue run on; returns whether it was paused.
    pub fn resume_queue(&self, session_id: &str) -> bool {
        self.state().paused.remove(session_id)
    }

    /// Whether `session_id`'s queue is paused.
    #[must_use]
    pub fn queue_paused(&self, session_id: &str) -> bool {
        self.state().paused.contains(session_id)
    }

    /// Remembers that the ended top-level run `run_id` of `session_id` waits for the user to
    /// open it.
    pub fn mark_unseen(&self, run_id: &str, session_id: &str) {
        self.state()
            .unseen
            .insert(run_id.to_owned(), session_id.to_owned());
    }

    /// Adds runs read from the store (ended while the app was closed) to the unseen set.
    pub fn seed_unseen(&self, runs: impl IntoIterator<Item = (String, String)>) {
        self.state().unseen.extend(runs);
    }

    /// The user opened `session_id`: its ended runs are seen. Returns how many were unseen.
    pub fn see_session(&self, session_id: &str) -> usize {
        let mut state = self.state();
        let before = state.unseen.len();
        state.unseen.retain(|_, session| session != session_id);
        before - state.unseen.len()
    }

    /// The user opened one run; returns whether it was unseen.
    pub fn see_run(&self, run_id: &str) -> bool {
        self.state().unseen.remove(run_id).is_some()
    }

    /// Ended top-level runs nobody has opened.
    #[must_use]
    pub fn unseen_runs(&self) -> usize {
        self.state().unseen.len()
    }

    /// Records an ended top-level run for the away-from-app signal (the latest
    /// [`FINISHED_KEPT`] are kept).
    pub fn push_finished(&self, run: FinishedRun) {
        let mut state = self.state();
        state.finished.push_back(run);
        while state.finished.len() > FINISHED_KEPT {
            state.finished.pop_front();
        }
    }

    /// The latest ended top-level runs, newest first.
    #[must_use]
    pub fn finished_runs(&self) -> Vec<FinishedRun> {
        self.state().finished.iter().rev().cloned().collect()
    }

    /// The state of every chat with an active top-level run.
    #[must_use]
    pub fn session_states(&self) -> Vec<SessionRunStateDto> {
        let state = self.state();
        state
            .active
            .iter()
            .filter_map(|(session_id, run_id)| {
                let run = state.runs.get(run_id)?;
                if run.kind == AgentRunKind::Subagent {
                    return None;
                }
                let summary = lock(&run.summary);
                Some(SessionRunStateDto {
                    session_id: session_id.clone(),
                    run_id: run_id.clone(),
                    status: summary.status,
                    last_step: summary.last_step.clone(),
                    queue_paused: state.paused.contains(session_id),
                    status_detail: summary.status_detail.clone(),
                })
            })
            .collect()
    }

    /// Waits for a top-level slot: at most `limit()` runs hold one at a time (`limit` is read
    /// again each time a slot frees, so a settings change applies). `None` when `cancel` fires
    /// first.
    pub async fn acquire_slot(
        &self,
        limit: impl Fn() -> usize,
        cancel: &CancelToken,
    ) -> Option<SlotGuard> {
        loop {
            let mut freed = std::pin::pin!(self.slots.freed.notified());
            freed.as_mut().enable();
            {
                let mut used = lock(&self.slots.used);
                if *used < limit().max(1) {
                    *used += 1;
                    return Some(SlotGuard {
                        slots: Arc::clone(&self.slots),
                    });
                }
            }
            tokio::select! {
                () = freed => {}
                () = cancel.cancelled() => return None,
            }
        }
    }

    /// Slots in use.
    #[must_use]
    pub fn slots_in_use(&self) -> usize {
        *lock(&self.slots.used)
    }
}

mod live;

#[cfg(test)]
mod tests;
