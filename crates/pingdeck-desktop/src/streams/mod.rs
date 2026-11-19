//! Framework-agnostic core event stream adapters and Tokio-timed delta coalescing.
//!
//! Replaces Tauri `Channel` + `PageEpoch` with zero-copy in-process typed Rust channels:
//! dropping the consumer [`StreamReceiver`] closes the channel and causes `EventSink::is_gone()`
//! to immediately return `true`, so core producers reap gone subscribers automatically.

use std::sync::{Arc, Mutex, MutexGuard, PoisonError};
use std::time::Instant;

use pingdeck_core::agent::model::{AgentEnvelope, AgentEvent};
use pingdeck_core::model::events::{EventSink, StreamCoalescer};
use tokio::runtime::Handle;
use tokio::sync::mpsc::{self, UnboundedReceiver, UnboundedSender};
use tokio::task::JoinHandle;
use tokio::time::Instant as TokioInstant;

/// Drop-guard that aborts an in-flight Tokio [`JoinHandle`] if the outer UI task is cancelled,
/// ensuring stream receivers drop immediately and upstream [`EventSink::is_gone`] becomes `true`.
pub struct AbortOnDrop<T>(Option<JoinHandle<T>>);

impl<T> AbortOnDrop<T> {
    #[must_use]
    pub fn new(handle: JoinHandle<T>) -> Self {
        Self(Some(handle))
    }

    /// Awaits the inner Tokio task, returning `None` if cancelled/panicked; dropping this future
    /// mid-await immediately aborts the Tokio task.
    pub async fn wait(mut self) -> Option<T> {
        let out = match self.0.as_mut() {
            Some(join) => join.await.ok(),
            None => None,
        };
        self.0 = None;
        out
    }
}

impl<T> Drop for AbortOnDrop<T> {
    fn drop(&mut self) {
        if let Some(handle) = self.0.take() {
            handle.abort();
        }
    }
}

/// Consumer side of an in-process typed core stream.
pub struct StreamReceiver<E> {
    rx: UnboundedReceiver<E>,
}

impl<E> StreamReceiver<E> {
    /// Awaits the next event, or `None` when the producer finishes or closes.
    pub async fn recv(&mut self) -> Option<E> {
        self.rx.recv().await
    }

    /// Drains all immediately available events without awaiting (for single-frame UI coalescing).
    pub fn drain_ready(&mut self, max_batch: usize) -> Vec<E> {
        let mut batch = Vec::new();
        while batch.len() < max_batch {
            match self.rx.try_recv() {
                Ok(ev) => batch.push(ev),
                Err(_) => break,
            }
        }
        batch
    }

    /// Awaits the first event and greedily drains any immediately queued siblings up to `max_batch`.
    pub async fn recv_batch(&mut self, max_batch: usize) -> Option<Vec<E>> {
        let first = self.rx.recv().await?;
        let mut batch = Vec::with_capacity(max_batch.min(32));
        batch.push(first);
        while batch.len() < max_batch {
            match self.rx.try_recv() {
                Ok(ev) => batch.push(ev),
                Err(_) => break,
            }
        }
        Some(batch)
    }
}

/// Zero-copy [`EventSink`] forwarding typed events directly into a Tokio unbounded channel.
#[derive(Clone)]
pub struct ChannelSink<E> {
    tx: UnboundedSender<E>,
}

impl<E: Send + 'static> ChannelSink<E> {
    /// Creates a connected `(sink, receiver)` pair.
    #[must_use]
    pub fn pair() -> (Self, StreamReceiver<E>) {
        let (tx, rx) = mpsc::unbounded_channel();
        (Self { tx }, StreamReceiver { rx })
    }
}

impl<E: Send + 'static> EventSink<E> for ChannelSink<E> {
    fn emit(&self, event: E) -> bool {
        self.tx.send(event).is_ok()
    }

    fn is_gone(&self) -> bool {
        self.tx.is_closed()
    }
}

/// Creates an `Arc<dyn EventSink<E>>` paired with its typed [`StreamReceiver`].
#[must_use]
pub fn channel_sink<E: Send + 'static>() -> (Arc<dyn EventSink<E>>, StreamReceiver<E>) {
    let (sink, rx) = ChannelSink::pair();
    (Arc::new(sink), rx)
}

/// Creates a coalescing `Arc<dyn EventSink<AgentEnvelope>>` using Tokio timers (zero parked OS
/// threads) paired with its [`StreamReceiver`].
#[must_use]
pub fn coalesced_agent_sink(
    tokio: Handle,
) -> (
    Arc<dyn EventSink<AgentEnvelope>>,
    StreamReceiver<AgentEnvelope>,
) {
    let (raw_sink, rx) = ChannelSink::pair();
    let coalesced = CoalescingSink::new(raw_sink, tokio, is_agent_delta, merge_agent_delta);
    (Arc::new(coalesced), rx)
}

/// Delivers at most one merged delta per core `COALESCE_WINDOW` (~33 ms) to `S`, and any other
/// event immediately after flushing held-back deltas.
pub struct CoalescingSink<E, S> {
    shared: Arc<Shared<E, S>>,
}

struct Shared<E, S> {
    out: S,
    tokio: Handle,
    state: Mutex<State<E>>,
}

struct State<E> {
    coalescer: StreamCoalescer<E>,
    timer_armed: bool,
    closed: bool,
}

impl<E, S> Clone for CoalescingSink<E, S> {
    fn clone(&self) -> Self {
        Self {
            shared: Arc::clone(&self.shared),
        }
    }
}

impl<E: Send + 'static, S: EventSink<E> + 'static> CoalescingSink<E, S> {
    pub fn new(
        out: S,
        tokio: Handle,
        is_delta: fn(&E) -> bool,
        merge: fn(&mut E, E) -> Result<(), E>,
    ) -> Self {
        Self {
            shared: Arc::new(Shared {
                out,
                tokio,
                state: Mutex::new(State {
                    coalescer: StreamCoalescer::new(is_delta, merge),
                    timer_armed: false,
                    closed: false,
                }),
            }),
        }
    }

    fn lock(&self) -> MutexGuard<'_, State<E>> {
        self.shared
            .state
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
    }

    fn deliver(&self, state: &mut State<E>, events: impl IntoIterator<Item = E>) {
        for event in events {
            if !self.shared.out.emit(event) {
                state.closed = true;
                return;
            }
        }
    }

    pub fn push_at(&self, event: E, now: Instant) -> bool {
        let mut state = self.lock();
        if state.closed {
            return false;
        }
        let ready = state.coalescer.push(event, now);
        self.deliver(&mut state, ready);
        !state.closed
    }

    pub fn poll_at(&self, now: Instant) -> Option<Instant> {
        let mut state = self.lock();
        let due = state.coalescer.poll(now);
        self.deliver(&mut state, due);
        let next = if state.closed {
            None
        } else {
            state.coalescer.next_deadline()
        };
        state.timer_armed = next.is_some();
        next
    }

    fn arm_timer(&self) {
        let deadline = {
            let mut state = self.lock();
            if state.timer_armed || state.closed {
                return;
            }
            let Some(deadline) = state.coalescer.next_deadline() else {
                return;
            };
            state.timer_armed = true;
            deadline
        };
        let sink = self.clone();
        self.shared.tokio.spawn(async move {
            let mut next = Some(deadline);
            while let Some(at) = next {
                tokio::time::sleep_until(TokioInstant::from_std(at)).await;
                next = sink.poll_at(Instant::now());
            }
        });
    }
}

impl<E: Send + 'static, S: EventSink<E> + 'static> EventSink<E> for CoalescingSink<E, S> {
    fn emit(&self, event: E) -> bool {
        let alive = self.push_at(event, Instant::now());
        if alive {
            self.arm_timer();
        }
        alive
    }

    fn is_gone(&self) -> bool {
        self.shared.out.is_gone()
    }
}

/// Whether `envelope` carries a mergeable agent fragment (text, reasoning, tool args/output).
#[must_use]
pub fn is_agent_delta(envelope: &AgentEnvelope) -> bool {
    is_delta_event(&envelope.event)
}

fn is_delta_event(event: &AgentEvent) -> bool {
    match event {
        AgentEvent::TextDelta { .. }
        | AgentEvent::ReasoningDelta { .. }
        | AgentEvent::ToolOutputDelta { .. }
        | AgentEvent::ToolCallArgsDelta { .. } => true,
        AgentEvent::SubagentEvent { event, .. } => is_delta_event(event),
        _ => false,
    }
}

/// Appends `next` to `prev` when both continue the same fragment stream of the same run.
#[allow(clippy::result_large_err)]
pub fn merge_agent_delta(
    prev: &mut AgentEnvelope,
    next: AgentEnvelope,
) -> Result<(), AgentEnvelope> {
    if prev.run_id != next.run_id {
        return Err(next);
    }
    let AgentEnvelope {
        seq,
        first_seq,
        ts_ms,
        run_id,
        event,
    } = next;
    match merge_event(&mut prev.event, event) {
        Ok(()) => {
            prev.seq = seq;
            prev.ts_ms = ts_ms;
            Ok(())
        }
        Err(event) => Err(AgentEnvelope {
            seq,
            first_seq,
            ts_ms,
            run_id,
            event,
        }),
    }
}

#[allow(clippy::result_large_err)]
fn merge_event(prev: &mut AgentEvent, next: AgentEvent) -> Result<(), AgentEvent> {
    match (prev, next) {
        (AgentEvent::TextDelta { text: a }, AgentEvent::TextDelta { text: b })
        | (AgentEvent::ReasoningDelta { text: a }, AgentEvent::ReasoningDelta { text: b }) => {
            a.push_str(&b);
            Ok(())
        }
        (
            AgentEvent::ToolCallArgsDelta {
                call_id: a_id,
                arguments: a,
            },
            AgentEvent::ToolCallArgsDelta {
                call_id: b_id,
                arguments: b,
            },
        ) if *a_id == b_id => {
            a.push_str(&b);
            Ok(())
        }
        (
            AgentEvent::ToolOutputDelta {
                call_id: a_id,
                stream: a_stream,
                text: a,
            },
            AgentEvent::ToolOutputDelta {
                call_id: b_id,
                stream: b_stream,
                text: b,
            },
        ) if *a_id == b_id && *a_stream == b_stream => {
            a.push_str(&b);
            Ok(())
        }
        (
            AgentEvent::SubagentEvent {
                parent_call_id: a_parent,
                child_run_id: a_child,
                event: a,
            },
            AgentEvent::SubagentEvent {
                parent_call_id: b_parent,
                child_run_id: b_child,
                event: b,
            },
        ) if *a_parent == b_parent && *a_child == b_child => {
            merge_event(a, *b).map_err(|b| AgentEvent::SubagentEvent {
                parent_call_id: b_parent,
                child_run_id: b_child,
                event: Box::new(b),
            })
        }
        (_, next) => Err(next),
    }
}

#[cfg(test)]
mod tests {
    use std::time::Duration;

    use pingdeck_core::model::events::COALESCE_WINDOW;

    use super::*;

    fn text(seq: u64, value: &str) -> AgentEnvelope {
        AgentEnvelope {
            seq,
            first_seq: seq,
            ts_ms: i64::try_from(seq).unwrap() * 10,
            run_id: "run_1".into(),
            event: AgentEvent::TextDelta {
                text: value.to_owned(),
            },
        }
    }

    #[tokio::test]
    async fn channel_sink_reports_is_gone_immediately_on_receiver_drop() {
        let (sink, rx) = channel_sink::<u32>();
        assert!(!sink.is_gone());
        assert!(sink.emit(1));
        drop(rx);
        assert!(sink.is_gone());
        assert!(!sink.emit(2));
    }

    #[tokio::test]
    async fn coalescing_sink_merges_burst_and_preserves_order() {
        let (raw, mut rx) = ChannelSink::pair();
        let sink = CoalescingSink::new(raw, Handle::current(), is_agent_delta, merge_agent_delta);
        let start = Instant::now();
        assert!(sink.push_at(text(1, "a"), start));
        assert!(sink.push_at(text(2, "b"), start + Duration::from_millis(1)));
        assert!(sink.push_at(text(3, "c"), start + Duration::from_millis(2)));
        assert_eq!(sink.poll_at(start + COALESCE_WINDOW), None);

        let batch = rx.drain_ready(16);
        assert_eq!(batch.len(), 2);
        match &batch[0].event {
            AgentEvent::TextDelta { text } => assert_eq!(text, "a"),
            other => panic!("unexpected: {other:?}"),
        }
        match &batch[1].event {
            AgentEvent::TextDelta { text } => assert_eq!(text, "bc"),
            other => panic!("unexpected: {other:?}"),
        }
        assert_eq!((batch[1].first_seq, batch[1].seq), (2, 3));
    }
}
