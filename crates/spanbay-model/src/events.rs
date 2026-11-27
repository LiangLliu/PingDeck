//! Push-style events from long-running core operations, independent of any UI framework.
//!
//! Single responsibility: define how the core tells a shell about progress while an operation is
//! still running. A shell passes an [`EventSink`] (a plain trait object or closure, e.g. one that
//! forwards to a Tauri `Channel`, a WebSocket or a terminal) and receives typed event enums; the
//! core never learns what is on the other side.

use std::{
    collections::VecDeque,
    sync::{Arc, Mutex, PoisonError},
    time::{Duration, Instant},
};

use serde::{Deserialize, Serialize};

use spanbay_base::{error::AppError, time::now_ms};

use crate::{
    items::{MockCallbackResult, MockTransactionLog},
    runner_item::RunnerItem,
};

/// Receives events of type `E` from the core.
pub trait EventSink<E>: Send + Sync {
    /// Delivers one event. Returns `false` when the consumer is gone for good (closed channel,
    /// dropped subscriber): the producer then stops sending to this sink.
    fn emit(&self, event: E) -> bool;

    /// Whether the consumer is known to be gone without sending anything to it. Lets a hub free
    /// the slot of a subscriber it has not emitted to since (a closure sink cannot tell).
    fn is_gone(&self) -> bool {
        false
    }

    /// False for sinks that discard every event, so producers can skip building them.
    fn is_active(&self) -> bool {
        true
    }
}

impl<E, F> EventSink<E> for F
where
    F: Fn(E) -> bool + Send + Sync,
{
    fn emit(&self, event: E) -> bool {
        self(event)
    }
}

/// A sink that drops every event, for callers that only want the final result.
#[derive(Debug, Clone, Copy, Default)]
pub struct NoEvents;

impl<E> EventSink<E> for NoEvents {
    fn emit(&self, _event: E) -> bool {
        true
    }

    fn is_active(&self) -> bool {
        false
    }
}

/// Progress of a collection run.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum RunnerEvent {
    /// One request finished (successfully or not); events arrive in execution order.
    ItemCompleted { item: RunnerItem },
}

/// A change in a running mock server's request log.
#[derive(Debug, Clone, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum MockLogEvent {
    /// Every request served and recorded since the previous event of this stream, oldest
    /// first. A stream emits at most one batch per 33 ms window, so a 1,000 req/s
    /// burst reaches the UI as about 30 events carrying all rows.
    Batch { entries: Vec<MockTransactionLog> },
    /// A callback fired by the response of row `log_id` finished (or was dropped); the row in
    /// the server's log carries the same result.
    Callback {
        log_id: String,
        result: MockCallbackResult,
    },
}

/// An event with its position in one stream, so a late or reconnecting subscriber can resume
/// without gaps or duplicates.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Sequenced<E> {
    /// Monotonic position, starting at 1 for the first event of the hub.
    pub seq: u64,
    /// When the event was emitted, in Unix milliseconds.
    pub ts_ms: i64,
    /// The position of the first event folded into this one by the replay ring; `None` for an
    /// event that stands alone (every live delivery). A folded replay slot covers
    /// `first_seq..=seq`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub first_seq: Option<u64>,
    /// The event itself.
    pub event: E,
}

/// A registered subscriber of an [`EventHub`] and its id.
type Subscription<E> = (u64, Arc<dyn EventSink<Sequenced<E>>>);

/// One slot of the replay ring: an event, or adjacent mergeable events folded into one. A folded
/// slot covers sequence numbers `first_seq..=envelope.seq` and carries the last one.
struct RingEntry<E> {
    first_seq: u64,
    bytes: usize,
    envelope: Sequenced<E>,
}

/// The mutable part of an [`EventHub`].
struct HubState<E> {
    next_seq: u64,
    ring: VecDeque<RingEntry<E>>,
    ring_bytes: usize,
    emits_since_compaction: usize,
    max_events: usize,
    max_bytes: usize,
    size_of: fn(&E) -> usize,
    merge: Option<fn(&mut E, &E) -> bool>,
    max_subscribers: usize,
    next_subscription: u64,
    subscribers: Vec<Subscription<E>>,
}

impl<E> HubState<E> {
    fn over_limit(&self) -> bool {
        self.ring.len() > self.max_events || self.ring_bytes > self.max_bytes
    }

    /// Appends `envelope` to the ring, then folds old mergeable events and finally evicts the
    /// oldest until the event and byte limits hold.
    fn remember(&mut self, envelope: Sequenced<E>, bytes: usize) {
        self.ring_bytes += bytes;
        self.ring.push_back(RingEntry {
            first_seq: envelope.seq,
            bytes,
            envelope,
        });
        self.emits_since_compaction += 1;
        // Throttled so a ring of unmergeable events is not rescanned on every emit.
        if self.over_limit() && self.emits_since_compaction > self.ring.len() / 4 {
            self.compact();
        }
        while self.over_limit() {
            let Some(oldest) = self.ring.pop_front() else {
                break;
            };
            self.ring_bytes = self.ring_bytes.saturating_sub(oldest.bytes);
        }
    }

    /// Folds adjacent mergeable events in the older half of the ring; the newest half stays
    /// unfolded so a client that reconnects with a recent `from_seq` can still resume.
    fn compact(&mut self) {
        self.emits_since_compaction = 0;
        let Some(merge) = self.merge else { return };
        let take = self.ring.len() / 2;
        if take < 2 {
            return;
        }
        let mut folded: Vec<RingEntry<E>> = Vec::with_capacity(take);
        for entry in self.ring.drain(..take) {
            if let Some(prev) = folded.last_mut()
                && merge(&mut prev.envelope.event, &entry.envelope.event)
            {
                prev.envelope.first_seq = Some(prev.first_seq);
                prev.envelope.seq = entry.envelope.seq;
                prev.envelope.ts_ms = entry.envelope.ts_ms;
                prev.bytes = (self.size_of)(&prev.envelope.event);
            } else {
                folded.push(entry);
            }
        }
        for entry in folded.into_iter().rev() {
            self.ring.push_front(entry);
        }
        self.ring_bytes = self.ring.iter().map(|entry| entry.bytes).sum();
    }
}

/// A broadcast hub: numbers every event, keeps the newest ones in a bounded ring and delivers to
/// any number of subscribers, replaying the ring to a late one.
///
/// Delivery happens under the hub's lock so order and replay are gap-free: a sink must be
/// non-blocking (a channel send), and must not call back into the hub.
pub struct EventHub<E: Clone + Send + Sync + 'static> {
    state: Mutex<HubState<E>>,
    /// The same estimator as [`HubState::size_of`], callable without the lock.
    size_of: fn(&E) -> usize,
}

impl<E: Clone + Send + Sync + 'static> EventHub<E> {
    /// A hub serving at most `max_subscribers` sinks and remembering the last `ring_capacity`
    /// events (no byte limit, no folding).
    #[must_use]
    pub fn new(max_subscribers: usize, ring_capacity: usize) -> Self {
        Self::with_limits(max_subscribers, ring_capacity, usize::MAX, |_| 0, None)
    }

    /// A hub whose replay ring holds at most `ring_events` events and about `ring_bytes` bytes
    /// (`size_of` estimates one event). When a limit is exceeded, adjacent events that `merge`
    /// accepts in the older half of the ring are folded first (`merge(prev, next)` appends `next`
    /// to `prev` and returns `true`, or returns `false` and leaves `prev` untouched), then the
    /// oldest events are evicted.
    ///
    /// A folded replay event carries the sequence number of the last event it absorbed and, in
    /// [`Sequenced::first_seq`], the first one, so a replay can skip numbers without looking like
    /// a loss. A subscriber resuming from a number *inside* a folded span gets
    /// [`AppError::Conflict`] (it cannot be split) and must reload persisted state.
    #[must_use]
    pub fn with_limits(
        max_subscribers: usize,
        ring_events: usize,
        ring_bytes: usize,
        size_of: fn(&E) -> usize,
        merge: Option<fn(&mut E, &E) -> bool>,
    ) -> Self {
        Self {
            size_of,
            state: Mutex::new(HubState {
                next_seq: 1,
                ring: VecDeque::new(),
                ring_bytes: 0,
                emits_since_compaction: 0,
                max_events: ring_events,
                max_bytes: ring_bytes,
                size_of,
                merge,
                max_subscribers,
                next_subscription: 1,
                subscribers: Vec::new(),
            }),
        }
    }

    fn lock(&self) -> std::sync::MutexGuard<'_, HubState<E>> {
        self.state.lock().unwrap_or_else(PoisonError::into_inner)
    }

    /// Numbers `event`, remembers it and delivers it to every subscriber (dropping those whose
    /// consumer is gone). Returns its sequence number.
    pub fn emit(&self, event: E) -> u64 {
        // Sized before the lock: measuring a big event must not stall every subscriber and
        // emitter of the hub.
        let bytes = (self.size_of)(&event);
        let mut state = self.lock();
        let envelope = Sequenced {
            seq: state.next_seq,
            ts_ms: now_ms(),
            first_seq: None,
            event,
        };
        state.next_seq += 1;
        if state.max_events > 0 {
            state.remember(envelope.clone(), bytes);
        }
        let seq = envelope.seq;
        state
            .subscribers
            .retain(|(_, sink)| sink.emit(envelope.clone()));
        seq
    }

    /// The sequence number of the latest event, or 0 before the first.
    #[must_use]
    pub fn last_seq(&self) -> u64 {
        self.lock().next_seq - 1
    }

    /// The sequence number of the oldest event still replayable; `last_seq() + 1` while the ring
    /// is empty.
    #[must_use]
    pub fn first_seq(&self) -> u64 {
        let state = self.lock();
        state
            .ring
            .front()
            .map_or(state.next_seq, |entry| entry.first_seq)
    }

    /// Registers `sink` after replaying every remembered event with `seq > from_seq`
    /// (`None`: everything remembered). Replay and registration are atomic, so no event is lost
    /// or delivered twice. Returns the id for [`EventHub::unsubscribe`].
    ///
    /// # Errors
    /// [`AppError::Conflict`] when events after `from_seq` were already evicted from the ring or
    /// folded with events the subscriber already saw (the caller must reload persisted state
    /// instead) or when the hub has too many subscribers; the sink then receives nothing.
    pub fn subscribe(
        &self,
        from_seq: Option<u64>,
        sink: Arc<dyn EventSink<Sequenced<E>>>,
    ) -> Result<u64, AppError> {
        self.subscribe_inner(from_seq, sink, None)
    }

    /// Like [`EventHub::subscribe`], except that a `from_seq` whose events were evicted or folded
    /// does not fail: the sink first receives `gap(first_seq)` (numbered `first_seq - 1`), where
    /// `first_seq` is the oldest event still replayed, then the whole ring. The subscriber must
    /// reload persisted state. `None` still fails when the ring no longer starts at 1.
    ///
    /// # Errors
    /// [`AppError::Conflict`] when the hub has too many subscribers (or `from_seq` is `None` and
    /// the start was evicted); [`AppError::Cancelled`] when the sink is gone.
    pub fn subscribe_announcing_gap(
        &self,
        from_seq: Option<u64>,
        sink: Arc<dyn EventSink<Sequenced<E>>>,
        gap: fn(u64) -> E,
    ) -> Result<u64, AppError> {
        self.subscribe_inner(from_seq, sink, Some(gap))
    }

    fn subscribe_inner(
        &self,
        from_seq: Option<u64>,
        sink: Arc<dyn EventSink<Sequenced<E>>>,
        gap: Option<fn(u64) -> E>,
    ) -> Result<u64, AppError> {
        let mut state = self.lock();
        // A consumer that is gone but was never emitted to (a reloaded page) must not hold a
        // slot: probe before counting.
        state.subscribers.retain(|(_, sink)| !sink.is_gone());
        if state.subscribers.len() >= state.max_subscribers {
            return Err(AppError::conflict("too many subscribers"));
        }
        let mut after = from_seq.unwrap_or(0);
        let oldest = state
            .ring
            .front()
            .map_or(state.next_seq, |entry| entry.first_seq);
        let evicted = after.saturating_add(1) < oldest;
        let split = state
            .ring
            .iter()
            .find(|entry| entry.first_seq <= after && after < entry.envelope.seq)
            .map(|entry| (entry.first_seq, entry.envelope.seq));
        match (gap, from_seq, evicted || split.is_some()) {
            (Some(gap), Some(_), true) => {
                after = oldest - 1;
                let announced = sink.emit(Sequenced {
                    seq: after,
                    ts_ms: now_ms(),
                    first_seq: None,
                    event: gap(oldest),
                });
                if !announced {
                    return Err(AppError::cancelled());
                }
            }
            _ if evicted => {
                return Err(AppError::conflict(format!(
                    "events {}..{} are no longer buffered; reload the persisted state",
                    after + 1,
                    oldest - 1
                )));
            }
            _ => {
                if let Some((first, last)) = split {
                    return Err(AppError::conflict(format!(
                        "events {first}..{last} are buffered as one merged event; reload the persisted state"
                    )));
                }
            }
        }
        for entry in state.ring.iter().filter(|entry| entry.envelope.seq > after) {
            if !sink.emit(entry.envelope.clone()) {
                return Err(AppError::cancelled());
            }
        }
        let id = state.next_subscription;
        state.next_subscription += 1;
        state.subscribers.push((id, sink));
        Ok(id)
    }

    /// Registers `sink` for future events only.
    ///
    /// # Errors
    /// [`AppError::Conflict`] when the hub has too many subscribers.
    pub fn subscribe_live(&self, sink: Arc<dyn EventSink<Sequenced<E>>>) -> Result<u64, AppError> {
        let last = self.last_seq();
        self.subscribe(Some(last), sink)
    }

    /// Ends a subscription; returns whether it was still active.
    pub fn unsubscribe(&self, subscription_id: u64) -> bool {
        let mut state = self.lock();
        let before = state.subscribers.len();
        state.subscribers.retain(|(id, _)| *id != subscription_id);
        state.subscribers.len() != before
    }
}

/// How often a stream may emit a merged delta: `1/30 s` rounded up (the `33 ms` window),
/// so at most 30 coalesced envelopes fit in any one second.
pub const COALESCE_WINDOW: Duration = Duration::from_nanos(33_334_000);

/// Rate-limits and merges a stream of deltas (text or reasoning chunks) so a consumer sees at
/// most 30 envelopes a second. Pure state machine: the caller passes the clock (`now`) and does
/// the timing, so it is deterministic and works under any executor.
///
/// Items for which `is_delta` is false are never delayed: pending deltas are emitted first, then
/// the item itself. The first delta after a quiet period is emitted at once; deltas arriving
/// within [`COALESCE_WINDOW`] of the last emission are folded into one pending item by `merge`
/// and released by [`StreamCoalescer::poll`] (or the next [`StreamCoalescer::push`]) once the
/// window has passed. Call [`StreamCoalescer::flush`] at the end of the stream.
pub struct StreamCoalescer<E> {
    window: Duration,
    is_delta: fn(&E) -> bool,
    merge: fn(&mut E, E) -> Result<(), E>,
    pending: Option<E>,
    last_emit: Option<Instant>,
}

impl<E> StreamCoalescer<E> {
    /// A coalescer with the [`COALESCE_WINDOW`]. `merge(prev, next)` appends `next` to `prev`
    /// when they are the same kind of delta (`Ok`) or hands `next` back (`Err`; e.g. a reasoning
    /// delta after a text delta), which flushes `prev` and starts a new run.
    #[must_use]
    pub fn new(is_delta: fn(&E) -> bool, merge: fn(&mut E, E) -> Result<(), E>) -> Self {
        Self::with_window(COALESCE_WINDOW, is_delta, merge)
    }

    /// [`StreamCoalescer::new`] with an explicit window.
    #[must_use]
    pub fn with_window(
        window: Duration,
        is_delta: fn(&E) -> bool,
        merge: fn(&mut E, E) -> Result<(), E>,
    ) -> Self {
        Self {
            window,
            is_delta,
            merge,
            pending: None,
            last_emit: None,
        }
    }

    fn due(&self, now: Instant) -> bool {
        self.last_emit
            .is_none_or(|last| now.saturating_duration_since(last) >= self.window)
    }

    fn release(&mut self, now: Instant) -> Option<E> {
        let item = self.pending.take()?;
        self.last_emit = Some(now);
        Some(item)
    }

    /// Feeds one item; returns what is ready to send now, in order (possibly nothing).
    pub fn push(&mut self, item: E, now: Instant) -> Vec<E> {
        let mut out = Vec::new();
        if !(self.is_delta)(&item) {
            out.extend(self.release(now));
            out.push(item);
            return out;
        }
        let unmerged = match self.pending.as_mut() {
            Some(prev) => (self.merge)(prev, item).err(),
            None => Some(item),
        };
        if let Some(item) = unmerged {
            out.extend(self.release(now));
            self.pending = Some(item);
        }
        out.extend(self.poll(now));
        out
    }

    /// The pending merged delta once the window since the last emission has passed.
    pub fn poll(&mut self, now: Instant) -> Option<E> {
        if self.due(now) {
            self.release(now)
        } else {
            None
        }
    }

    /// When [`StreamCoalescer::poll`] will next release the pending delta (`None`: nothing pending).
    #[must_use]
    pub fn next_deadline(&self) -> Option<Instant> {
        self.pending.as_ref()?;
        Some(
            self.last_emit
                .map_or_else(Instant::now, |last| last + self.window),
        )
    }

    /// Takes the pending delta regardless of the window (end of stream).
    pub fn flush(&mut self) -> Option<E> {
        self.pending.take()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    type Collected = Arc<Mutex<Vec<u64>>>;

    fn collector() -> (Collected, Arc<dyn EventSink<Sequenced<&'static str>>>) {
        let seen: Collected = Arc::default();
        let sink_seen = Arc::clone(&seen);
        let sink: Arc<dyn EventSink<Sequenced<&'static str>>> =
            Arc::new(move |event: Sequenced<&'static str>| {
                sink_seen.lock().unwrap().push(event.seq);
                true
            });
        (seen, sink)
    }

    #[test]
    fn events_are_numbered_from_one_and_replayed_to_a_late_subscriber_without_gaps() {
        let hub = EventHub::new(4, 16);
        assert_eq!(hub.last_seq(), 0);
        assert_eq!((hub.emit("a"), hub.emit("b"), hub.emit("c")), (1, 2, 3));
        let (seen, sink) = collector();
        hub.subscribe(Some(1), sink).unwrap();
        hub.emit("d");
        assert_eq!(*seen.lock().unwrap(), [2, 3, 4]);
        let (all, sink) = collector();
        hub.subscribe(None, sink).unwrap();
        assert_eq!(*all.lock().unwrap(), [1, 2, 3, 4]);
    }

    #[test]
    fn a_live_subscription_skips_history_and_unsubscribe_stops_delivery() {
        let hub = EventHub::new(4, 16);
        hub.emit("old");
        let (seen, sink) = collector();
        let id = hub.subscribe_live(sink).unwrap();
        hub.emit("new");
        assert!(hub.unsubscribe(id) && !hub.unsubscribe(id));
        hub.emit("later");
        assert_eq!(*seen.lock().unwrap(), [2]);
    }

    #[test]
    fn a_gone_consumer_is_dropped_on_the_next_emit() {
        let hub = EventHub::new(4, 16);
        let sink: Arc<dyn EventSink<Sequenced<&'static str>>> =
            Arc::new(|_: Sequenced<&'static str>| false);
        hub.subscribe_live(sink).unwrap();
        hub.emit("x");
        assert_eq!(hub.lock().subscribers.len(), 0);
    }

    #[test]
    fn evicted_history_is_reported_instead_of_leaving_a_silent_gap() {
        let hub = EventHub::new(4, 3);
        for text in ["a", "b", "c", "d", "e"] {
            hub.emit(text);
        }
        let (seen, sink) = collector();
        let error = hub.subscribe(Some(1), sink).unwrap_err();
        assert!(matches!(error, AppError::Conflict { .. }), "{error:?}");
        assert!(seen.lock().unwrap().is_empty());
        let (seen, sink) = collector();
        hub.subscribe(Some(2), sink).unwrap();
        assert_eq!(*seen.lock().unwrap(), [3, 4, 5]);
    }

    /// A sink whose consumer has left but that nobody emitted to since (a reloaded page).
    struct Reloaded(std::sync::atomic::AtomicBool);

    impl EventSink<Sequenced<&'static str>> for Reloaded {
        fn emit(&self, _: Sequenced<&'static str>) -> bool {
            true
        }

        fn is_gone(&self) -> bool {
            self.0.load(std::sync::atomic::Ordering::SeqCst)
        }
    }

    #[test]
    fn a_gone_subscriber_that_was_never_emitted_to_frees_its_slot() {
        let hub = EventHub::<&'static str>::new(1, 4);
        let page = Arc::new(Reloaded(std::sync::atomic::AtomicBool::new(false)));
        hub.subscribe_live(page.clone()).unwrap();
        assert!(
            hub.subscribe_live(collector().1).is_err(),
            "the slot is taken"
        );
        page.0.store(true, std::sync::atomic::Ordering::SeqCst);
        hub.subscribe_live(collector().1)
            .expect("the reloaded page's slot is reclaimed without any emit");
    }

    #[derive(Debug, Clone, PartialEq)]
    enum Ev {
        Text(String),
        Reason(String),
        Mark(u32),
    }

    fn is_delta(event: &Ev) -> bool {
        matches!(event, Ev::Text(_) | Ev::Reason(_))
    }

    fn merge_owned(prev: &mut Ev, next: Ev) -> Result<(), Ev> {
        match (prev, next) {
            (Ev::Text(a), Ev::Text(b)) | (Ev::Reason(a), Ev::Reason(b)) => {
                a.push_str(&b);
                Ok(())
            }
            (_, next) => Err(next),
        }
    }

    fn merge_ref(prev: &mut Ev, next: &Ev) -> bool {
        match (prev, next) {
            (Ev::Text(a), Ev::Text(b)) | (Ev::Reason(a), Ev::Reason(b)) => {
                a.push_str(b);
                true
            }
            _ => false,
        }
    }

    fn size_of(event: &Ev) -> usize {
        match event {
            Ev::Text(text) | Ev::Reason(text) => text.len(),
            Ev::Mark(_) => 8,
        }
    }

    fn text(chunk: &str) -> Ev {
        Ev::Text(chunk.to_owned())
    }

    type Seen = Arc<Mutex<Vec<(u64, Ev)>>>;

    fn ev_collector() -> (Seen, Arc<dyn EventSink<Sequenced<Ev>>>) {
        let seen: Arc<Mutex<Vec<(u64, Ev)>>> = Arc::default();
        let sink_seen = Arc::clone(&seen);
        let sink: Arc<dyn EventSink<Sequenced<Ev>>> = Arc::new(move |event: Sequenced<Ev>| {
            sink_seen.lock().unwrap().push((event.seq, event.event));
            true
        });
        (seen, sink)
    }

    #[test]
    fn the_ring_is_bounded_by_bytes_as_well_as_events() {
        let hub = EventHub::<Ev>::with_limits(4, 100, 20, size_of, None);
        for n in 0..5 {
            hub.emit(Ev::Mark(n));
        }
        let (seen, sink) = ev_collector();
        hub.subscribe(Some(3), sink).unwrap();
        assert_eq!(*seen.lock().unwrap(), [(4, Ev::Mark(3)), (5, Ev::Mark(4))]);
        let error = hub.subscribe(Some(2), ev_collector().1).unwrap_err();
        assert!(matches!(error, AppError::Conflict { .. }), "{error:?}");
        assert!(hub.lock().ring_bytes <= 20);
    }

    #[test]
    fn old_adjacent_deltas_are_folded_instead_of_evicted() {
        let hub = EventHub::<Ev>::with_limits(4, 6, usize::MAX, size_of, Some(merge_ref));
        for chunk in ["a", "b", "c", "d", "e", "f"] {
            hub.emit(text(chunk));
        }
        assert_eq!(hub.lock().ring.len(), 6);
        hub.emit(text("g")); // over the limit: the oldest half is folded, nothing is evicted
        let (seen, sink) = ev_collector();
        hub.subscribe(None, sink).unwrap();
        assert_eq!(
            *seen.lock().unwrap(),
            [
                (3, text("abc")),
                (4, text("d")),
                (5, text("e")),
                (6, text("f")),
                (7, text("g"))
            ]
        );
        // Resuming at a span boundary works; inside the folded span it cannot.
        let (seen, sink) = ev_collector();
        hub.subscribe(Some(3), sink).unwrap();
        assert_eq!(seen.lock().unwrap().len(), 4);
        let error = hub.subscribe(Some(1), ev_collector().1).unwrap_err();
        assert!(matches!(error, AppError::Conflict { .. }), "{error:?}");
        assert_eq!(hub.last_seq(), 7);
    }

    #[test]
    fn a_folded_replay_slot_carries_the_first_seq_it_absorbed() {
        type Span = (Option<u64>, u64);
        let hub = EventHub::<Ev>::with_limits(4, 6, usize::MAX, size_of, Some(merge_ref));
        for chunk in ["a", "b", "c", "d", "e", "f", "g"] {
            hub.emit(text(chunk));
        }
        let spans: Arc<Mutex<Vec<Span>>> = Arc::default();
        let sink_spans = Arc::clone(&spans);
        hub.subscribe(
            None,
            Arc::new(move |event: Sequenced<Ev>| {
                sink_spans
                    .lock()
                    .unwrap()
                    .push((event.first_seq, event.seq));
                true
            }),
        )
        .unwrap();
        // "abc" is the folded slot 1..=3; every other slot stands alone.
        assert_eq!(
            *spans.lock().unwrap(),
            [(Some(1), 3), (None, 4), (None, 5), (None, 6), (None, 7)]
        );
    }

    #[test]
    fn folding_stops_at_events_that_do_not_merge() {
        let hub = EventHub::<Ev>::with_limits(4, 8, usize::MAX, size_of, Some(merge_ref));
        for event in [
            text("a"),
            text("b"),
            Ev::Reason("x".into()),
            Ev::Reason("y".into()),
            Ev::Mark(1),
            text("c"),
            text("d"),
            text("e"),
            text("f"),
        ] {
            hub.emit(event);
        }
        let (seen, sink) = ev_collector();
        hub.subscribe(None, sink).unwrap();
        let events: Vec<Ev> = seen
            .lock()
            .unwrap()
            .iter()
            .map(|(_, e)| e.clone())
            .collect();
        assert_eq!(
            events,
            [
                text("ab"),
                Ev::Reason("xy".into()),
                Ev::Mark(1),
                text("c"),
                text("d"),
                text("e"),
                text("f")
            ]
        );
    }

    fn coalescer() -> (StreamCoalescer<Ev>, Instant) {
        (StreamCoalescer::new(is_delta, merge_owned), Instant::now())
    }

    fn ms(base: Instant, millis: u64) -> Instant {
        base + Duration::from_millis(millis)
    }

    #[test]
    fn the_first_delta_is_immediate_and_later_ones_wait_for_the_window() {
        let (mut c, t0) = coalescer();
        assert_eq!(c.push(text("a"), t0), [text("a")]);
        assert!(c.push(text("b"), ms(t0, 5)).is_empty());
        assert!(c.push(text("c"), ms(t0, 20)).is_empty());
        assert_eq!(c.next_deadline(), Some(t0 + COALESCE_WINDOW));
        assert_eq!(c.poll(ms(t0, 33)), None);
        assert_eq!(c.poll(ms(t0, 34)), Some(text("bc")));
        assert_eq!(c.poll(ms(t0, 100)), None);
        assert_eq!(c.next_deadline(), None);
        // A quiet period makes the next delta immediate again.
        assert_eq!(c.push(text("d"), ms(t0, 500)), [text("d")]);
    }

    #[test]
    fn a_push_after_the_window_releases_the_merged_delta() {
        let (mut c, t0) = coalescer();
        c.push(text("a"), t0);
        c.push(text("b"), ms(t0, 10));
        assert_eq!(c.push(text("c"), ms(t0, 40)), [text("bc")]);
    }

    #[test]
    fn a_non_delta_flushes_pending_deltas_first_and_is_never_delayed() {
        let (mut c, t0) = coalescer();
        c.push(text("a"), t0);
        c.push(text("b"), ms(t0, 1));
        c.push(text("c"), ms(t0, 2));
        assert_eq!(c.push(Ev::Mark(1), ms(t0, 3)), [text("bc"), Ev::Mark(1)]);
        assert_eq!(c.push(Ev::Mark(2), ms(t0, 4)), [Ev::Mark(2)]);
        assert_eq!(c.flush(), None);
    }

    #[test]
    fn different_kinds_of_delta_do_not_merge() {
        let (mut c, t0) = coalescer();
        c.push(text("a"), t0);
        c.push(text("b"), ms(t0, 1));
        // The text run is flushed, the reasoning run starts a new window.
        assert_eq!(c.push(Ev::Reason("r".into()), ms(t0, 2)), [text("b")]);
        assert!(c.push(Ev::Reason("s".into()), ms(t0, 3)).is_empty());
        assert_eq!(c.poll(ms(t0, 40)), Some(Ev::Reason("rs".into())));
    }

    #[test]
    fn a_second_of_one_millisecond_deltas_makes_at_most_30_envelopes_without_losing_text() {
        let (mut c, t0) = coalescer();
        let mut envelopes = Vec::new();
        let mut expected = String::new();
        for tick in 0..=1000_u64 {
            let chunk = format!("{tick},");
            expected.push_str(&chunk);
            envelopes.extend(c.push(Ev::Text(chunk), ms(t0, tick)));
        }
        let during_the_second = envelopes.len();
        assert!(
            during_the_second <= 30,
            "{during_the_second} envelopes in 1 s"
        );
        envelopes.extend(c.flush());
        let joined: String = envelopes
            .iter()
            .map(|e| match e {
                Ev::Text(t) => t.as_str(),
                _ => "",
            })
            .collect();
        assert_eq!(joined, expected);
    }
}
