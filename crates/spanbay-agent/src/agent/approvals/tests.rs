//! Approval tests: delivery, and the timeout rule driven by an injected gate (no sleeping).

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Mutex;
use std::sync::atomic::{AtomicUsize, Ordering};

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use tokio::sync::{Notify, oneshot, watch};

use super::*;

const TIMEOUT: Duration = Duration::from_secs(600);

/// A gate whose subscriber count and timers a test controls.
struct TestGate {
    watchers: AtomicUsize,
    changed: watch::Sender<u64>,
    timers: Mutex<Vec<(Duration, oneshot::Sender<()>)>>,
    steer: Notify,
}

impl TestGate {
    fn new(watchers: usize) -> Self {
        Self {
            watchers: AtomicUsize::new(watchers),
            changed: watch::channel(0).0,
            timers: Mutex::default(),
            steer: Notify::new(),
        }
    }

    fn set_watchers(&self, count: usize) {
        self.watchers.store(count, Ordering::SeqCst);
        self.changed.send_modify(|version| *version += 1);
    }

    /// Timers somebody still waits for.
    fn armed(&self) -> usize {
        self.timers
            .lock()
            .unwrap()
            .iter()
            .filter(|(_, sender)| !sender.is_closed())
            .count()
    }

    fn fire_all(&self) {
        for (_, sender) in std::mem::take(&mut *self.timers.lock().unwrap()) {
            let _ = sender.send(());
        }
    }
}

impl ApprovalGate for TestGate {
    fn watchers(&self) -> usize {
        self.watchers.load(Ordering::SeqCst)
    }

    fn changes(&self) -> watch::Receiver<u64> {
        self.changed.subscribe()
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        let (sender, receiver) = oneshot::channel();
        self.timers.lock().unwrap().push((duration, sender));
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

fn summary(run_id: &str, call_id: &str) -> PendingApprovalSummary {
    PendingApprovalSummary {
        run_id: run_id.into(),
        session_id: "s".into(),
        call_id: call_id.into(),
        tool: "shell".into(),
        reason: "asks".into(),
        preview: None,
        requested_ms: 1,
        risk: "low".into(),
        summary: String::new(),
        full_text: None,
        card: None,
    }
}

fn context() -> RuleContext {
    RuleContext {
        session_id: "s".into(),
        workspace_dir: None,
        tool: "shell".into(),
        pattern: Some("cargo test *".into()),
        offered: vec![ApprovalScope::Once, ApprovalScope::Session],
    }
}

fn answer(decision: ApprovalDecision) -> ApprovalAnswer {
    ApprovalAnswer {
        decision,
        scope: ApprovalScope::Once,
        message: None,
        edited_arguments: None,
        allow_network_once: false,
    }
}

async fn settle() {
    for _ in 0..20 {
        tokio::task::yield_now().await;
    }
}

#[tokio::test]
async fn an_answer_reaches_the_waiting_run_and_frees_the_slot() {
    let approvals = Approvals::default();
    let ticket = approvals.register(summary("r", "c"), context()).unwrap();
    assert!(approvals.has_pending("r"));
    assert_eq!(approvals.list().len(), 1);
    let gate = TestGate::new(1);
    let cancel = CancelToken::new();
    let (outcome, delivered) = tokio::join!(ticket.wait(&gate, &cancel, TIMEOUT), async {
        settle().await;
        approvals.deliver("r", "c", answer(ApprovalDecision::Allow))
    });
    assert!(delivered);
    assert_eq!(
        outcome,
        ApprovalOutcome::Answered(answer(ApprovalDecision::Allow))
    );
    assert!(approvals.list().is_empty());
    assert!(!approvals.deliver("r", "c", answer(ApprovalDecision::Allow)));
}

#[test]
fn a_call_has_at_most_one_pending_request_and_dropping_the_ticket_forgets_it() {
    let approvals = Approvals::default();
    let ticket = approvals.register(summary("r", "c"), context()).unwrap();
    assert!(approvals.register(summary("r", "c"), context()).is_none());
    // The same call id of another run is another request.
    let other = approvals.register(summary("r2", "c"), context()).unwrap();
    assert_eq!(approvals.begin_answer("r", "c"), Some(context()));
    drop(ticket);
    assert!(approvals.begin_answer("r", "c").is_none());
    assert!(!approvals.deliver("r", "c", answer(ApprovalDecision::Deny)));
    assert!(approvals.has_pending("r2"));
    drop(other);
    assert!(!approvals.has_pending("r2"));
}

#[test]
fn only_one_answer_at_a_time_may_save_its_rule_and_a_gone_call_refuses_it() {
    let approvals = Approvals::default();
    let ticket = approvals.register(summary("r", "c"), context()).unwrap();
    assert!(approvals.begin_answer("r", "c").is_some());
    assert!(
        approvals.begin_answer("r", "c").is_none(),
        "a concurrent answer must not persist a second rule"
    );
    // The first could not be saved: the call is open again.
    approvals.abort_answer("r", "c");
    assert!(approvals.begin_answer("r", "c").is_some());
    // The call timed out while the rule was being saved: delivery fails, nothing waits.
    drop(ticket);
    assert!(!approvals.deliver("r", "c", answer(ApprovalDecision::Allow)));
    assert!(approvals.begin_answer("r", "c").is_none());
}

#[test]
fn answers_are_keyed_by_run_and_call_and_never_shared() {
    let approvals = Approvals::default();
    let _a = approvals.register(summary("r1", "c"), context()).unwrap();
    let _b = approvals.register(summary("r2", "c"), context()).unwrap();
    assert!(!approvals.deliver("r3", "c", answer(ApprovalDecision::Allow)));
    assert!(approvals.deliver("r1", "c", answer(ApprovalDecision::Allow)));
    assert!(approvals.has_pending("r2"));
}

#[tokio::test]
async fn an_unwatched_run_times_out_after_the_configured_time() {
    let approvals = Approvals::default();
    let ticket = approvals.register(summary("r", "c"), context()).unwrap();
    let gate = TestGate::new(0);
    let cancel = CancelToken::new();
    let (outcome, ()) = tokio::join!(ticket.wait(&gate, &cancel, TIMEOUT), async {
        settle().await;
        assert_eq!(gate.armed(), 1);
        assert_eq!(gate.timers.lock().unwrap()[0].0, TIMEOUT);
        gate.fire_all();
    });
    assert_eq!(outcome, ApprovalOutcome::TimedOut);
    assert!(!approvals.has_pending("r"));
}

#[tokio::test]
async fn a_watched_run_never_arms_a_clock_and_waits_for_the_user() {
    let approvals = Approvals::default();
    let ticket = approvals.register(summary("r", "c"), context()).unwrap();
    let gate = TestGate::new(1);
    let cancel = CancelToken::new();
    let (outcome, ()) = tokio::join!(ticket.wait(&gate, &cancel, TIMEOUT), async {
        settle().await;
        assert_eq!(gate.armed(), 0, "no timer while the chat is open");
        assert!(approvals.deliver("r", "c", answer(ApprovalDecision::Deny)));
    });
    assert_eq!(
        outcome,
        ApprovalOutcome::Answered(answer(ApprovalDecision::Deny))
    );
}

#[tokio::test]
async fn a_subscriber_appearing_cancels_the_running_clock() {
    let approvals = Approvals::default();
    let ticket = approvals.register(summary("r", "c"), context()).unwrap();
    let gate = TestGate::new(0);
    let cancel = CancelToken::new();
    let (outcome, ()) = tokio::join!(ticket.wait(&gate, &cancel, TIMEOUT), async {
        settle().await;
        assert_eq!(gate.armed(), 1);
        gate.set_watchers(1);
        settle().await;
        assert_eq!(gate.armed(), 0, "opening the chat dropped the timer");
        // Even a timer that would have fired now changes nothing.
        gate.fire_all();
        settle().await;
        assert!(approvals.deliver("r", "c", answer(ApprovalDecision::Allow)));
    });
    assert_eq!(
        outcome,
        ApprovalOutcome::Answered(answer(ApprovalDecision::Allow))
    );
}

#[tokio::test]
async fn the_clock_restarts_from_zero_when_the_last_subscriber_leaves() {
    let approvals = Approvals::default();
    let ticket = approvals.register(summary("r", "c"), context()).unwrap();
    let gate = TestGate::new(1);
    let cancel = CancelToken::new();
    let (outcome, ()) = tokio::join!(ticket.wait(&gate, &cancel, TIMEOUT), async {
        settle().await;
        assert_eq!(gate.armed(), 0);
        gate.set_watchers(0);
        settle().await;
        assert_eq!(gate.armed(), 1, "closing the chat starts the clock");
        gate.fire_all();
    });
    assert_eq!(outcome, ApprovalOutcome::TimedOut);
}

#[tokio::test]
async fn an_unrelated_change_does_not_restart_a_running_clock() {
    let approvals = Approvals::default();
    let ticket = approvals.register(summary("r", "c"), context()).unwrap();
    let gate = TestGate::new(0);
    let cancel = CancelToken::new();
    let (outcome, ()) = tokio::join!(ticket.wait(&gate, &cancel, TIMEOUT), async {
        settle().await;
        // A subscriber elsewhere came and went: the count for this run is still zero.
        gate.set_watchers(0);
        settle().await;
        assert_eq!(gate.armed(), 1, "still the one timer");
        gate.fire_all();
    });
    assert_eq!(outcome, ApprovalOutcome::TimedOut);
}

#[tokio::test]
async fn cancelling_the_run_ends_the_wait() {
    let approvals = Approvals::default();
    let ticket = approvals.register(summary("r", "c"), context()).unwrap();
    let gate = TestGate::new(1);
    let cancel = CancelToken::new();
    let (outcome, ()) = tokio::join!(ticket.wait(&gate, &cancel, TIMEOUT), async {
        settle().await;
        cancel.cancel();
    });
    assert_eq!(outcome, ApprovalOutcome::Cancelled);
    assert!(!approvals.has_pending("r"));
}

#[tokio::test]
async fn a_steer_message_supersedes_the_wait() {
    let approvals = Approvals::default();
    let ticket = approvals.register(summary("r", "c"), context()).unwrap();
    let gate = TestGate::new(1);
    let cancel = CancelToken::new();
    let (outcome, ()) = tokio::join!(ticket.wait(&gate, &cancel, TIMEOUT), async {
        settle().await;
        gate.steer.notify_waiters();
    });
    assert_eq!(outcome, ApprovalOutcome::Superseded);
}

#[test]
fn pending_requests_list_oldest_first() {
    let approvals = Approvals::default();
    let mut late = summary("r", "late");
    late.requested_ms = 9;
    let mut early = summary("r", "early");
    early.requested_ms = 3;
    let _a = approvals.register(late, context()).unwrap();
    let _b = approvals.register(early, context()).unwrap();
    let calls: Vec<_> = approvals.list().into_iter().map(|a| a.call_id).collect();
    assert_eq!(calls, ["early", "late"]);
}
