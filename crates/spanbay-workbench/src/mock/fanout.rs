//! The request log of one running server and the batched fan-out to its live subscribers.
//!
//! Single responsibility: keep the bounded ring of served requests and deliver new rows to every
//! subscriber as [`MockLogEvent::Batch`] events. Each subscriber stream is rate
//! limited on its own by a [`BatchWindow`]: the first row after an idle window is emitted at once
//! (leading edge), rows that arrive inside the 33 ms window that emission opened wait and leave
//! together when it ends, so a stream carries at most ~30 events per second and loses no row.
//!
//! The window logic is a pure state machine over caller-supplied [`Instant`]s, so tests drive it
//! with synthetic time; the runtime only adds a timer task that polls a stream when its window
//! ends (see `logs::push_log`).

use std::collections::VecDeque;
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use crate::events::MockLogEvent;
use crate::workspace::items::{MockCallbackResult, MockTransactionLog};

use super::server::{LogSink, MAX_LOG_SUBSCRIBERS, lock};

/// Minimum spacing of two events on one subscriber stream (≤ 30 events per second).
pub(super) const BATCH_WINDOW: Duration = Duration::from_millis(33);

/// Most bytes (approximate, text of the entries) the ring keeps, whatever its row capacity.
pub(super) const MAX_RING_BYTES: usize = 32 * 1024 * 1024;

/// Approximate heap size of `entry`: the text it carries.
fn entry_bytes(entry: &MockTransactionLog) -> usize {
    let headers = |headers: &[crate::workspace::models::KeyValue]| -> usize {
        headers
            .iter()
            .map(|header| header.key.len() + header.value.len())
            .sum()
    };
    entry.path.len()
        + entry.query.len()
        + entry.request_body.len()
        + entry.response_body.len()
        + headers(&entry.request_headers)
        + headers(&entry.response_headers)
        + entry
            .callbacks
            .iter()
            .map(|callback| callback.name.len() + callback.url.len())
            .sum::<usize>()
}

/// What a stream should do after [`BatchWindow::poll`].
#[derive(Debug, PartialEq)]
pub(super) enum Poll {
    /// Nothing is waiting, or a timer already covers the wait.
    Idle,
    /// Emit these rows, then these callback results, now; a new window has opened.
    Emit(Vec<MockTransactionLog>, Vec<CallbackNote>),
    /// Rows wait for the window to end at this instant and no timer exists yet: start one.
    Arm(Instant),
}

/// A callback result for the row with this id.
pub(super) type CallbackNote = (String, MockCallbackResult);

/// Pending rows and window state of one subscriber stream.
#[derive(Debug, Default)]
pub(super) struct BatchWindow {
    pending: Vec<MockTransactionLog>,
    /// Callback results of rows this stream has already been sent.
    notes: Vec<CallbackNote>,
    /// End of the window the last emission opened; `None` before the first emission.
    window_end: Option<Instant>,
    /// A timer task is alive and will poll this stream with `from_timer`.
    armed: bool,
}

impl BatchWindow {
    /// Queues a row for the next emission.
    pub(super) fn push(&mut self, entry: MockTransactionLog) {
        self.pending.push(entry);
    }

    /// Queues a callback result: folded into its row while the row waits, else sent on its own.
    pub(super) fn note(&mut self, log_id: &str, result: &MockCallbackResult) {
        match self.pending.iter_mut().find(|entry| entry.id == log_id) {
            Some(entry) => entry.callbacks.push(result.clone()),
            None => self.notes.push((log_id.to_owned(), result.clone())),
        }
    }

    /// Drops the rows that were not emitted yet (cleared rows must never reappear).
    pub(super) fn clear(&mut self) {
        self.pending.clear();
        self.notes.clear();
    }

    /// Takes everything pending regardless of the window (final flush when the server stops).
    pub(super) fn take_all(&mut self) -> (Vec<MockTransactionLog>, Vec<CallbackNote>) {
        (
            std::mem::take(&mut self.pending),
            std::mem::take(&mut self.notes),
        )
    }

    /// Decides what happens at `now`. `from_timer` marks the poll of the timer task itself,
    /// which retires that timer; an `Arm` answer then re-arms it.
    pub(super) fn poll(&mut self, now: Instant, from_timer: bool) -> Poll {
        if from_timer {
            self.armed = false;
        }
        if self.pending.is_empty() && self.notes.is_empty() {
            return Poll::Idle;
        }
        match self.window_end {
            Some(end) if now < end => {
                if self.armed {
                    Poll::Idle
                } else {
                    self.armed = true;
                    Poll::Arm(end)
                }
            }
            _ => {
                self.window_end = Some(now + BATCH_WINDOW);
                Poll::Emit(
                    std::mem::take(&mut self.pending),
                    std::mem::take(&mut self.notes),
                )
            }
        }
    }
}

/// One live subscription: its sink and its window.
pub(super) struct Subscriber {
    id: u64,
    sink: LogSink,
    window: Mutex<BatchWindow>,
}

/// The ring buffer of served requests plus the subscribers that follow it.
pub(super) struct LogHub {
    inner: Mutex<HubInner>,
    /// Entries the ring keeps before the oldest is dropped; changed live.
    capacity: AtomicUsize,
}

struct HubInner {
    ring: VecDeque<MockTransactionLog>,
    /// Sum of [`entry_bytes`] over `ring`.
    ring_bytes: usize,
    subscribers: Vec<Arc<Subscriber>>,
}

impl LogHub {
    /// An empty hub whose ring keeps `capacity` entries.
    pub(super) fn new(capacity: usize) -> Self {
        Self {
            inner: Mutex::new(HubInner {
                ring: VecDeque::new(),
                ring_bytes: 0,
                subscribers: Vec::new(),
            }),
            capacity: AtomicUsize::new(capacity),
        }
    }

    /// Sets the ring size; the ring shrinks on its next entry.
    pub(super) fn set_capacity(&self, capacity: usize) {
        self.capacity.store(capacity, Ordering::Relaxed);
    }

    /// The ring, oldest first.
    pub(super) fn snapshot(&self) -> Vec<MockTransactionLog> {
        lock(&self.inner).ring.iter().cloned().collect()
    }

    /// Empties the ring and every subscriber's not-yet-emitted rows in one step, so a row that
    /// is being recorded concurrently ends up either in neither or in both.
    pub(super) fn clear(&self) {
        let mut inner = lock(&self.inner);
        inner.ring.clear();
        inner.ring_bytes = 0;
        for subscriber in &inner.subscribers {
            lock(&subscriber.window).clear();
        }
    }

    /// Registers `sink` under `id`; `false` when [`MAX_LOG_SUBSCRIBERS`] streams are live.
    pub(super) fn subscribe(&self, id: u64, sink: LogSink) -> bool {
        let mut inner = lock(&self.inner);
        if inner.subscribers.len() >= MAX_LOG_SUBSCRIBERS {
            return false;
        }
        inner.subscribers.push(Arc::new(Subscriber {
            id,
            sink,
            window: Mutex::new(BatchWindow::default()),
        }));
        true
    }

    /// Ends subscription `id`; whether it was live.
    pub(super) fn unsubscribe(&self, id: u64) -> bool {
        let mut inner = lock(&self.inner);
        let before = inner.subscribers.len();
        inner.subscribers.retain(|subscriber| subscriber.id != id);
        inner.subscribers.len() != before
    }

    /// Appends `entry` to the ring and queues it on every subscriber, then emits where a
    /// window allows. Returns the streams that need a timer, with the instant it must fire.
    ///
    /// Ring and queues are updated under one lock (so [`clear`](Self::clear) is atomic with
    /// them); the sinks are called outside it.
    pub(super) fn publish(
        &self,
        entry: MockTransactionLog,
        now: Instant,
    ) -> Vec<(Arc<Subscriber>, Instant)> {
        let subscribers = {
            let mut inner = lock(&self.inner);
            // A lowered capacity drops several entries at once; `max(1)` keeps the loop finite.
            let capacity = self.capacity.load(Ordering::Relaxed).max(1);
            let incoming = entry_bytes(&entry);
            while inner.ring.len() >= capacity
                || (!inner.ring.is_empty() && inner.ring_bytes + incoming > MAX_RING_BYTES)
            {
                if let Some(dropped) = inner.ring.pop_front() {
                    inner.ring_bytes -= entry_bytes(&dropped);
                }
            }
            inner.ring_bytes += incoming;
            for subscriber in &inner.subscribers {
                lock(&subscriber.window).push(entry.clone());
            }
            inner.ring.push_back(entry);
            inner.subscribers.clone()
        };
        subscribers
            .into_iter()
            .filter_map(|subscriber| {
                self.poll_subscriber(&subscriber, now, false)
                    .map(|at| (subscriber, at))
            })
            .collect()
    }

    /// Adds a callback result to row `log_id` of the ring (a row already trimmed is skipped) and
    /// queues it on every subscriber, like [`publish`](Self::publish) does with a row.
    pub(super) fn record_callback(
        &self,
        log_id: &str,
        result: &MockCallbackResult,
        now: Instant,
    ) -> Vec<(Arc<Subscriber>, Instant)> {
        let subscribers = {
            let mut inner = lock(&self.inner);
            let Some(entry) = inner.ring.iter_mut().rev().find(|entry| entry.id == log_id) else {
                return Vec::new();
            };
            entry.callbacks.push(result.clone());
            let added = result.name.len() + result.url.len();
            inner.ring_bytes += added;
            for subscriber in &inner.subscribers {
                lock(&subscriber.window).note(log_id, result);
            }
            inner.subscribers.clone()
        };
        subscribers
            .into_iter()
            .filter_map(|subscriber| {
                self.poll_subscriber(&subscriber, now, false)
                    .map(|at| (subscriber, at))
            })
            .collect()
    }

    /// Polls one stream: emits its batch when its window allows, and prunes it when its sink
    /// reports the consumer gone. Returns when a timer must fire next, if one is needed.
    ///
    /// The sink is called while the stream's window lock is held, so batches of one stream
    /// leave in the order they were taken.
    pub(super) fn poll_subscriber(
        &self,
        subscriber: &Subscriber,
        now: Instant,
        from_timer: bool,
    ) -> Option<Instant> {
        let mut window = lock(&subscriber.window);
        match window.poll(now, from_timer) {
            Poll::Idle => None,
            Poll::Arm(at) => Some(at),
            Poll::Emit(entries, notes) => {
                let delivered = emit_all(&subscriber.sink, entries, notes);
                // Released first: `unsubscribe` takes the hub lock, which `publish` holds
                // while it takes window locks.
                drop(window);
                if !delivered {
                    self.unsubscribe(subscriber.id);
                }
                None
            }
        }
    }

    /// Delivers every pending row regardless of the windows and ends all subscriptions: the
    /// server is stopping, and a timer that fires later finds nothing left to do.
    pub(super) fn close(&self) {
        let subscribers = std::mem::take(&mut lock(&self.inner).subscribers);
        for subscriber in subscribers {
            let (entries, notes) = lock(&subscriber.window).take_all();
            emit_all(&subscriber.sink, entries, notes);
        }
    }
}

/// Emits a batch of rows (when there are any), then one event per callback result; `false`
/// once the sink reports its consumer gone.
fn emit_all(sink: &LogSink, entries: Vec<MockTransactionLog>, notes: Vec<CallbackNote>) -> bool {
    if !entries.is_empty() && !sink.emit(MockLogEvent::Batch { entries }) {
        return false;
    }
    notes
        .into_iter()
        .all(|(log_id, result)| sink.emit(MockLogEvent::Callback { log_id, result }))
}
