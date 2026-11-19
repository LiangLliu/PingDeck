//! Gate tests: the focus half of the timeout rule and the cost pause, with an injected clock.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicBool, AtomicUsize, Ordering};

use futures_util::FutureExt;
use tokio::sync::Notify;

use super::*;

const TIMEOUT: Duration = Duration::from_secs(600);

/// Subscribers, focus and timers a test controls.
struct World {
    watchers: AtomicUsize,
    focused: AtomicBool,
    changed: watch::Sender<u64>,
    timers: Mutex<Vec<oneshot::Sender<()>>>,
    steer: Notify,
}

impl World {
    fn new(watchers: usize, focused: bool) -> Self {
        Self {
            watchers: AtomicUsize::new(watchers),
            focused: AtomicBool::new(focused),
            changed: watch::channel(0).0,
            timers: Mutex::default(),
            steer: Notify::new(),
        }
    }

    fn focus(&self, on: bool) {
        self.focused.store(on, Ordering::SeqCst);
        self.changed.send_modify(|version| *version += 1);
    }

    fn armed(&self) -> usize {
        self.timers
            .lock()
            .unwrap()
            .iter()
            .filter(|timer| !timer.is_closed())
            .count()
    }

    fn fire_all(&self) {
        for timer in std::mem::take(&mut *self.timers.lock().unwrap()) {
            let _ = timer.send(());
        }
    }
}

impl GateWatch for World {
    fn watchers(&self) -> usize {
        self.watchers.load(Ordering::SeqCst)
    }

    fn focused(&self) -> bool {
        self.focused.load(Ordering::SeqCst)
    }

    fn changes(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn sleep(&self, _duration: Duration) -> BoxFuture<'static, ()> {
        let (sender, receiver) = oneshot::channel();
        self.timers.lock().unwrap().push(sender);
        async move {
            if receiver.await.is_err() {
                std::future::pending::<()>().await;
            }
        }
        .boxed()
    }

    fn steered(&self) -> BoxFuture<'_, ()> {
        self.steer.notified().boxed()
    }
}

fn request(kind: GateKind, gate_id: &str) -> GateRequest {
    GateRequest {
        gate_id: gate_id.to_owned(),
        run_id: "r".to_owned(),
        session_id: "s".to_owned(),
        kind,
        payload: GatePayload::Cost,
        opened_ms: 1,
    }
}

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn a_watched_chat_in_an_unfocused_window_times_out_and_focus_clears_the_clock() {
    let gates = HumanGate::default();
    let ticket = gates.open(request(GateKind::Plan, "c1")).unwrap();
    let world = World::new(1, true);
    let cancel = CancelToken::new();
    let (outcome, ()) = tokio::join!(ticket.wait(&world, &cancel, TIMEOUT), async {
        settle().await;
        assert_eq!(world.armed(), 0, "watched and focused: no clock");
        world.focus(false);
        settle().await;
        assert_eq!(world.armed(), 1, "the window lost focus: the clock runs");
        world.focus(true);
        settle().await;
        assert_eq!(world.armed(), 0, "focus again: the clock is gone");
        world.focus(false);
        settle().await;
        world.fire_all();
    });
    assert_eq!(outcome, GateOutcome::TimedOut);
    assert_eq!(gates.count(GateKind::Plan), 0, "a closed gate is forgotten");
}

#[tokio::test]
async fn a_cost_pause_ignores_a_steer_and_takes_one_decision() {
    let gates = HumanGate::default();
    let ticket = gates.open(request(GateKind::Cost, "cost:1")).unwrap();
    assert!(gates.open(request(GateKind::Cost, "cost:1")).is_none());
    assert_eq!(
        gates.open_gate("r", GateKind::Cost).as_deref(),
        Some("cost:1")
    );
    let world = World::new(1, true);
    let cancel = CancelToken::new();
    let (outcome, delivered) = tokio::join!(ticket.wait(&world, &cancel, TIMEOUT), async {
        settle().await;
        world.steer.notify_waiters();
        settle().await;
        gates.deliver("r", "cost:1", GateAnswer::Cost(CostDecision::Stop))
    });
    assert!(delivered);
    assert_eq!(
        outcome,
        GateOutcome::Answered(GateAnswer::Cost(CostDecision::Stop))
    );
    assert!(!gates.deliver("r", "cost:1", GateAnswer::Cost(CostDecision::Stop)));
}
