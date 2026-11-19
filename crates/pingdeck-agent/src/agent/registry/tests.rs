//! Registry tests: one active run per chat, replay with gaps, subscriber counting, slots.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use super::*;
use crate::agent::model::{
    AgentMessage, AgentMessageMeta, AgentRole, AgentStopReason, ApprovalDecision, ApprovalScope,
    LiveToolState, ToolCallStatus,
};
use crate::agent::sandbox::SandboxKind;
use crate::agent::tools::{ToolClass, ToolSource};
use pingdeck_llm::llm::hub::HubUsage;

fn summary(
    run_id: &str,
    session_id: &str,
    kind: AgentRunKind,
    parent: Option<&str>,
) -> AgentRunSummary {
    AgentRunSummary {
        run_id: run_id.into(),
        session_id: session_id.into(),
        kind,
        parent_run_id: parent.map(str::to_owned),
        parent_call_id: None,
        status: AgentRunStatus::Running,
        model_label: "m".into(),
        target: None,
        iterations: 0,
        usage: HubUsage::default(),
        cost_micro_usd: 0,
        started_at_ms: 0,
        finished_at_ms: None,
        last_event_ms: None,
        last_step: None,
        stop_reason: None,
        error_kind: None,
        error: None,
        prompt_version: 1,
        last_seq: 0,
        failure: None,
        role: None,
        cost_currency: None,
        cap_inapplicable: None,
        status_detail: None,
    }
}

fn run(run_id: &str, session_id: &str) -> Arc<ActiveRun> {
    ActiveRun::new(
        summary(run_id, session_id, AgentRunKind::Main, None),
        CancelToken::new(),
    )
}

type Seen = Arc<Mutex<Vec<AgentEnvelope>>>;

fn collector() -> (Seen, Arc<dyn EventSink<AgentEnvelope>>) {
    let seen: Seen = Arc::default();
    let sink = {
        let seen = Arc::clone(&seen);
        move |event: AgentEnvelope| {
            seen.lock().unwrap().push(event);
            true
        }
    };
    (seen, Arc::new(sink))
}

fn notice(n: usize) -> AgentEvent {
    AgentEvent::notice(n.to_string())
}

fn by_run(run_id: &str) -> AgentSubscribeTarget {
    AgentSubscribeTarget::Run {
        run_id: run_id.into(),
    }
}

fn by_session(session_id: &str) -> AgentSubscribeTarget {
    AgentSubscribeTarget::Session {
        session_id: session_id.into(),
    }
}

#[test]
fn a_chat_has_one_active_run_until_it_finishes() {
    let registry = RunRegistry::new();
    let first = run("r1", "s");
    registry.register(&first).unwrap();
    assert!(matches!(
        registry.register(&run("r2", "s")),
        Err(AppError::Conflict { .. })
    ));
    registry.register(&run("r3", "other")).unwrap();
    assert_eq!(registry.active_for_session("s").unwrap().run_id, "r1");
    registry.finish("r1");
    assert!(registry.active_for_session("s").is_none());
    registry.register(&run("r2", "s")).unwrap();
}

#[test]
fn a_discarded_run_leaves_no_trace() {
    let registry = RunRegistry::new();
    registry.register(&run("r1", "s")).unwrap();
    registry.discard("r1");
    assert!(registry.get("r1").is_none());
    registry.register(&run("r2", "s")).unwrap();
}

#[test]
fn a_finished_run_stays_subscribable_for_ten_minutes() {
    let registry = RunRegistry::new();
    registry.register(&run("r1", "s")).unwrap();
    registry.finish("r1");
    let now = Instant::now();
    registry.prune_at(now + RETAIN_FINISHED - Duration::from_secs(1));
    assert!(registry.get("r1").is_some());
    registry.prune_at(now + RETAIN_FINISHED + Duration::from_secs(1));
    assert!(registry.get("r1").is_none());
    assert!(matches!(
        registry.subscribe(&by_run("r1"), None, collector().1),
        Err(AppError::NotFound { .. })
    ));
}

#[test]
fn an_active_run_is_never_pruned() {
    let registry = RunRegistry::new();
    registry.register(&run("r1", "s")).unwrap();
    registry.prune_at(Instant::now() + RETAIN_FINISHED * 10);
    assert!(registry.get("r1").is_some());
}

#[test]
fn a_late_subscriber_replays_the_buffer_then_follows_live() {
    let registry = RunRegistry::new();
    let active = run("r1", "s");
    registry.register(&active).unwrap();
    for n in 0..3 {
        active.emit(notice(n));
    }
    let (seen, sink) = collector();
    registry.subscribe(&by_run("r1"), Some(1), sink).unwrap();
    active.emit(notice(3));
    let seqs: Vec<u64> = seen.lock().unwrap().iter().map(|e| e.seq).collect();
    assert_eq!(seqs, [2, 3, 4]);
}

#[test]
fn events_older_than_the_buffer_are_announced_by_a_gap_first() {
    let registry = RunRegistry::new();
    let active = run("r1", "s");
    registry.register(&active).unwrap();
    let total = REPLAY_EVENTS + 904;
    for n in 0..total {
        active.emit(notice(n));
    }
    let first = active.hub.first_seq();
    assert_eq!(first, 905, "4,096 events stay");
    let (seen, sink) = collector();
    registry.subscribe(&by_run("r1"), Some(5), sink).unwrap();
    let seen = seen.lock().unwrap();
    assert_eq!(seen[0].event, AgentEvent::Gap { first_seq: first });
    assert_eq!(seen[1].seq, first);
    assert_eq!(seen.len(), 1 + REPLAY_EVENTS);
}

#[test]
fn a_subscriber_that_is_current_gets_no_gap() {
    let registry = RunRegistry::new();
    let active = run("r1", "s");
    registry.register(&active).unwrap();
    active.emit(notice(0));
    let (seen, sink) = collector();
    registry.subscribe(&by_run("r1"), Some(1), sink).unwrap();
    assert!(seen.lock().unwrap().is_empty());
}

#[test]
fn text_deltas_are_merged_to_keep_a_long_answer_replayable() {
    let registry = RunRegistry::new();
    let active = run("r1", "s");
    registry.register(&active).unwrap();
    let total = REPLAY_EVENTS * 3;
    for _ in 0..total {
        active.emit(AgentEvent::TextDelta { text: "x".into() });
    }
    assert_eq!(active.hub.first_seq(), 1, "nothing was evicted");
    let (seen, sink) = collector();
    registry.subscribe(&by_run("r1"), None, sink).unwrap();
    let replayed: usize = seen
        .lock()
        .unwrap()
        .iter()
        .map(|envelope| match &envelope.event {
            AgentEvent::TextDelta { text } => text.len(),
            _ => 0,
        })
        .sum();
    assert_eq!(replayed, total);
    assert!(seen.lock().unwrap().len() <= REPLAY_EVENTS);
}

#[test]
fn a_chat_subscription_follows_each_new_run_of_the_chat() {
    let registry = RunRegistry::new();
    let (seen, sink) = collector();
    registry.subscribe(&by_session("s"), None, sink).unwrap();
    let first = run("r1", "s");
    registry.register(&first).unwrap();
    first.emit(notice(1));
    registry.finish("r1");
    let second = run("r2", "s");
    registry.register(&second).unwrap();
    second.emit(notice(2));
    let texts: Vec<_> = seen
        .lock()
        .unwrap()
        .iter()
        .map(|e| match &e.event {
            AgentEvent::Notice { text, .. } => text.clone(),
            other => format!("{other:?}"),
        })
        .collect();
    assert_eq!(texts, ["1", "2"]);
}

#[test]
fn a_chat_subscription_replays_the_latest_finished_run() {
    let registry = RunRegistry::new();
    let finished = run("r1", "s");
    registry.register(&finished).unwrap();
    finished.emit(notice(7));
    registry.finish("r1");
    let (seen, sink) = collector();
    registry.subscribe(&by_session("s"), None, sink).unwrap();
    assert_eq!(seen.lock().unwrap().len(), 1);
}

#[test]
fn subscribers_are_counted_per_chat_until_they_unsubscribe() {
    let registry = RunRegistry::new();
    registry.register(&run("r1", "s")).unwrap();
    assert_eq!(registry.watchers("s"), 0);
    let id = registry
        .subscribe(&by_run("r1"), None, collector().1)
        .unwrap();
    let other = registry
        .subscribe(&by_session("s"), None, collector().1)
        .unwrap();
    assert_eq!(registry.watchers("s"), 2);
    assert_eq!(registry.watchers("elsewhere"), 0);
    assert!(registry.unsubscribe(id));
    assert!(!registry.unsubscribe(id));
    assert_eq!(registry.watchers("s"), 1);
    registry.unsubscribe(other);
    assert_eq!(registry.watchers("s"), 0);
}

#[test]
fn a_sink_that_disappears_stops_counting_at_its_next_event() {
    let registry = RunRegistry::new();
    let active = run("r1", "s");
    registry.register(&active).unwrap();
    let gone: Arc<dyn EventSink<AgentEnvelope>> = Arc::new(|_: AgentEnvelope| false);
    registry.subscribe(&by_run("r1"), None, gone).unwrap();
    assert_eq!(registry.watchers("s"), 1);
    active.emit(notice(1));
    assert_eq!(registry.watchers("s"), 0);
}

/// A sink whose consumer is gone although it took the last event (a reloaded page).
struct Reloaded(std::sync::atomic::AtomicBool);

impl EventSink<AgentEnvelope> for Reloaded {
    fn emit(&self, _: AgentEnvelope) -> bool {
        true
    }

    fn is_gone(&self) -> bool {
        self.0.load(std::sync::atomic::Ordering::SeqCst)
    }
}

#[test]
fn a_consumer_known_to_be_gone_stops_counting_and_is_reaped_without_an_event() {
    let registry = RunRegistry::new();
    registry.register(&run("r1", "s")).unwrap();
    let page = Arc::new(Reloaded(std::sync::atomic::AtomicBool::new(false)));
    let id = registry
        .subscribe(&by_session("s"), None, page.clone())
        .unwrap();
    assert_eq!(registry.watchers("s"), 1);
    let mut changes = registry.watchers_changed();
    changes.borrow_and_update();

    page.0.store(true, std::sync::atomic::Ordering::SeqCst);
    assert_eq!(registry.watchers("s"), 0, "no emit was needed to notice");
    registry.reap_gone();
    assert!(changes.has_changed().unwrap(), "approval timers are woken");
    assert!(!registry.unsubscribe(id), "the entry is gone");
}

#[test]
fn a_sub_agent_counts_as_watched_while_its_parent_chat_is() {
    let registry = RunRegistry::new();
    let parent = run("p", "chat");
    let child = ActiveRun::new(
        summary("c", "child-chat", AgentRunKind::Subagent, Some("p")),
        parent.cancel.child(),
    );
    registry.register(&parent).unwrap();
    registry.register(&child).unwrap();
    assert_eq!(registry.watchers_of_run("c"), 0);
    let id = registry
        .subscribe(&by_session("chat"), None, collector().1)
        .unwrap();
    assert_eq!(registry.watchers_of_run("c"), 1);
    assert_eq!(registry.watchers_of_run("p"), 1);
    registry.unsubscribe(id);
    assert_eq!(registry.watchers_of_run("c"), 0);
    // Cancelling the parent cancels the child.
    parent.cancel.cancel();
    assert!(child.cancel.is_cancelled());
}

#[test]
fn subscribing_and_unsubscribing_wake_the_watchers_changed_receiver() {
    let registry = RunRegistry::new();
    registry.register(&run("r1", "s")).unwrap();
    let mut changes = registry.watchers_changed();
    changes.borrow_and_update();
    let id = registry
        .subscribe(&by_run("r1"), None, collector().1)
        .unwrap();
    assert!(changes.has_changed().unwrap());
    changes.borrow_and_update();
    registry.unsubscribe(id);
    assert!(changes.has_changed().unwrap());
}

#[test]
fn opening_a_chat_or_a_run_clears_what_it_finished_unseen() {
    let registry = RunRegistry::new();
    registry.mark_unseen("r1", "s");
    registry.mark_unseen("r2", "t");
    registry.mark_unseen("r3", "t");
    assert_eq!(registry.unseen_runs(), 3);
    assert_eq!(registry.see_session("t"), 2);
    assert_eq!(registry.unseen_runs(), 1);
    assert!(registry.see_run("r1"));
    assert!(!registry.see_run("r1"));
    assert_eq!(registry.unseen_runs(), 0);
}

#[test]
fn the_queue_pause_is_per_chat_and_sticky_until_resumed() {
    let registry = RunRegistry::new();
    assert!(!registry.queue_paused("s"));
    registry.pause_queue("s");
    assert!(registry.queue_paused("s"));
    assert!(!registry.queue_paused("t"));
    assert!(registry.resume_queue("s"));
    assert!(!registry.resume_queue("s"));
}

#[test]
fn session_states_list_top_level_runs_with_their_pause_flag() {
    let registry = RunRegistry::new();
    let parent = run("p", "chat");
    registry.register(&parent).unwrap();
    let child = ActiveRun::new(
        summary("c", "child", AgentRunKind::Subagent, Some("p")),
        parent.cancel.child(),
    );
    registry.register(&child).unwrap();
    registry.pause_queue("chat");
    let states = registry.session_states();
    assert_eq!(states.len(), 1);
    assert_eq!(states[0].session_id, "chat");
    assert!(states[0].queue_paused);
    assert_eq!(states[0].status, AgentRunStatus::Running);
}

#[test]
fn summaries_filter_by_chat_and_activity_newest_first() {
    let registry = RunRegistry::new();
    let mut older = summary("r1", "a", AgentRunKind::Main, None);
    older.started_at_ms = 1;
    let mut newer = summary("r2", "b", AgentRunKind::Main, None);
    newer.started_at_ms = 2;
    registry
        .register(&ActiveRun::new(older, CancelToken::new()))
        .unwrap();
    let second = ActiveRun::new(newer, CancelToken::new());
    registry.register(&second).unwrap();
    let all = registry.summaries(&AgentRunListFilter::default());
    assert_eq!(
        all.iter().map(|s| s.run_id.as_str()).collect::<Vec<_>>(),
        ["r2", "r1"]
    );
    let only_a = registry.summaries(&AgentRunListFilter {
        session_id: Some("a".into()),
        ..AgentRunListFilter::default()
    });
    assert_eq!(only_a.len(), 1);
    second.update(|s| s.status = AgentRunStatus::Completed);
    let active = registry.summaries(&AgentRunListFilter {
        active_only: true,
        ..AgentRunListFilter::default()
    });
    assert_eq!(active.len(), 1);
    assert_eq!(active[0].run_id, "r1");
}

#[test]
fn the_summary_reports_the_live_event_position() {
    let active = run("r1", "s");
    active.emit(notice(1));
    active.emit(notice(2));
    let summary = active.summary();
    assert_eq!(summary.last_seq, 2);
    assert!(summary.last_event_ms.is_some());
}

#[tokio::test]
async fn steer_messages_queue_up_and_wake_a_waiter() {
    let active = run("r1", "s");
    assert!(!active.has_steer());
    let waiter = {
        let active = Arc::clone(&active);
        tokio::spawn(async move { active.steered().await })
    };
    tokio::task::yield_now().await;
    assert!(!waiter.is_finished());
    active.push_steer(SteerMessage {
        parts: vec![HubPart::text("a")],
        ..SteerMessage::default()
    });
    active.push_steer(SteerMessage {
        parts: vec![HubPart::text("b")],
        ..SteerMessage::default()
    });
    tokio::time::timeout(Duration::from_secs(5), waiter)
        .await
        .unwrap()
        .unwrap();
    let taken = active.take_steer();
    assert_eq!(taken.len(), 2);
    assert!(!active.has_steer());
    // A steer that was queued before anyone waited resolves at once.
    active.push_steer(SteerMessage::default());
    active.steered().await;
}

#[tokio::test]
async fn the_cost_gate_delivers_one_decision() {
    use crate::agent::gate::{
        GateAnswer, GateKind, GateOutcome, GatePayload, GateRequest, HumanGate,
    };
    let gates = HumanGate::default();
    let stop = || GateAnswer::Cost(CostDecision::Stop);
    assert!(!gates.deliver("r1", "cost:1", stop()), "no gate open");
    let ticket = gates
        .open(GateRequest {
            gate_id: "cost:1".to_owned(),
            run_id: "r1".to_owned(),
            session_id: "s".to_owned(),
            kind: GateKind::Cost,
            payload: GatePayload::Cost,
            opened_ms: 1,
        })
        .unwrap();
    assert!(gates.deliver("r1", "cost:1", stop()));
    assert!(!gates.deliver("r1", "cost:1", stop()), "answered once");
    let watch = crate::agent::runtime::AgentRuntime::for_test();
    let active = run("r1", "s");
    let outcome = ticket
        .wait(
            &watch.gate(&active),
            &CancelToken::new(),
            std::time::Duration::from_secs(600),
        )
        .await;
    assert_eq!(outcome, GateOutcome::Answered(stop()));
}

#[test]
fn a_run_costs_its_tree_spend_per_currency_never_summed_across() {
    let active = run("r1", "s");
    active.add_cost("USD", 100);
    active.add_cost("CNY", 40);
    active.add_cost("USD", 2);
    assert_eq!(active.total_cost_micro("USD"), 102);
    assert_eq!(active.total_cost_micro("CNY"), 40);
    assert_eq!(active.total_cost_micro("EUR"), 0);
}

#[tokio::test]
async fn at_most_the_limit_of_runs_hold_a_slot_and_the_rest_wait() {
    let registry = Arc::new(RunRegistry::new());
    let cancel = CancelToken::new();
    let first = registry.acquire_slot(|| 1, &cancel).await.unwrap();
    assert_eq!(registry.slots_in_use(), 1);
    let waiting = {
        let (registry, cancel) = (Arc::clone(&registry), cancel.clone());
        tokio::spawn(async move { registry.acquire_slot(|| 1, &cancel).await.is_some() })
    };
    for _ in 0..50 {
        tokio::task::yield_now().await;
    }
    assert!(!waiting.is_finished(), "the second run waits queued");
    drop(first);
    assert!(
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap()
    );
}

#[tokio::test]
async fn a_larger_limit_lets_that_many_runs_in() {
    let registry = RunRegistry::new();
    let cancel = CancelToken::new();
    let a = registry.acquire_slot(|| 3, &cancel).await.unwrap();
    let b = registry.acquire_slot(|| 3, &cancel).await.unwrap();
    let c = registry.acquire_slot(|| 3, &cancel).await.unwrap();
    assert_eq!(registry.slots_in_use(), 3);
    drop((a, b, c));
    assert_eq!(registry.slots_in_use(), 0);
}

#[tokio::test]
async fn a_run_cancelled_while_it_waits_for_a_slot_gets_none() {
    let registry = Arc::new(RunRegistry::new());
    let holder = registry
        .acquire_slot(|| 1, &CancelToken::new())
        .await
        .unwrap();
    let cancel = CancelToken::new();
    let waiting = {
        let (registry, cancel) = (Arc::clone(&registry), cancel.clone());
        tokio::spawn(async move { registry.acquire_slot(|| 1, &cancel).await.is_none() })
    };
    tokio::task::yield_now().await;
    cancel.cancel();
    assert!(
        tokio::time::timeout(Duration::from_secs(5), waiting)
            .await
            .unwrap()
            .unwrap()
    );
    drop(holder);
    assert_eq!(registry.slots_in_use(), 0);
}

#[test]
fn a_run_may_claim_no_more_sub_agents_than_the_limit_and_a_refusal_counts_nothing() {
    let run = run("r", "s");
    assert!(run.claim_subagent(2));
    assert!(run.claim_subagent(2));
    assert!(!run.claim_subagent(2));
    assert!(!run.claim_subagent(2));
    // A raised limit (the setting changed) lets the run claim again.
    assert!(run.claim_subagent(3));
    // Another run has its own budget.
    assert!(self::run("other", "s2").claim_subagent(1));
}

fn live_text(snapshot: &LiveRunSnapshot) -> String {
    snapshot
        .parts
        .iter()
        .filter_map(|part| match part {
            HubPart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect()
}

fn stored(role: AgentRole) -> Box<AgentMessage> {
    Box::new(AgentMessage {
        id: "m".into(),
        session_id: "s".into(),
        seq: 1,
        role,
        parts: Vec::new(),
        usage: None,
        model: None,
        error: None,
        run_id: Some("r".into()),
        parent_id: None,
        branch_index: 0,
        partial: false,
        cost_micro_usd: 0,
        cost_currency: None,
        meta: AgentMessageMeta::default(),
        branch_siblings: Vec::new(),
        attachments: Vec::new(),
        created_at_ms: 0,
    })
}

#[test]
fn a_snapshot_taken_after_the_ring_evicted_the_text_continues_without_gap_or_duplicate() {
    let registry = RunRegistry::new();
    let active = run("r1", "s");
    registry.register(&active).unwrap();
    let words =
        |range: std::ops::Range<usize>| -> String { range.map(|n| format!("<{n}>")).collect() };
    let say = |range: std::ops::Range<usize>| {
        for n in range {
            active.emit(AgentEvent::TextDelta {
                text: format!("<{n}>"),
            });
        }
    };
    say(0..5);
    for n in 0..REPLAY_EVENTS + 10 {
        active.emit(notice(n));
    }
    assert!(active.hub.first_seq() > 6, "the early deltas left the ring");
    say(5..10);
    let said_before = words(0..10);

    let snapshot = active.live_snapshot();
    assert_eq!(snapshot.run_id, "r1");
    assert_eq!(snapshot.seq, active.hub.last_seq());
    assert_eq!(live_text(&snapshot), said_before);
    assert!(!snapshot.truncated);

    say(10..12);
    let (seen, sink) = collector();
    registry
        .subscribe(&by_run("r1"), Some(snapshot.seq), sink)
        .unwrap();
    say(12..15);
    let final_answer = words(0..15);
    active.emit(AgentEvent::Done {
        run: Box::new(summary("r1", "s", AgentRunKind::Main, None)),
        stop_reason: AgentStopReason::Completed,
    });

    let seen = seen.lock().unwrap();
    assert!(
        seen.iter()
            .all(|envelope| !matches!(envelope.event, AgentEvent::Gap { .. })),
        "no gap"
    );
    assert_eq!(seen[0].seq, snapshot.seq + 1, "no event skipped");
    let seqs: Vec<u64> = seen.iter().map(|envelope| envelope.seq).collect();
    assert!(
        seqs.windows(2).all(|pair| pair[0] < pair[1]),
        "no duplicate"
    );
    let replayed: String = seen
        .iter()
        .filter_map(|envelope| match &envelope.event {
            AgentEvent::TextDelta { text } => Some(text.as_str()),
            _ => None,
        })
        .collect();
    assert_eq!(live_text(&snapshot) + &replayed, final_answer);
    assert_eq!(
        active.live_snapshot().parts,
        Vec::new(),
        "an ended run holds nothing"
    );
}

#[test]
fn a_tool_call_shows_its_arguments_approval_wait_and_result() {
    let active = run("r1", "s");
    let call = |active: &ActiveRun| {
        let snapshot = active.live_snapshot();
        assert_eq!(snapshot.tools.len(), 1);
        (snapshot.tools[0].clone(), snapshot.parts)
    };
    active.emit(AgentEvent::ReasoningDelta { text: "hm".into() });
    active.emit(AgentEvent::ToolCallStarted {
        call_id: "c1".into(),
        name: "write_file".into(),
        source: ToolSource::Builtin,
    });
    for fragment in ["{\"path\":", "\"a.txt\"}"] {
        active.emit(AgentEvent::ToolCallArgsDelta {
            call_id: "c1".into(),
            arguments: fragment.into(),
        });
    }
    let (tool, parts) = call(&active);
    assert_eq!(tool.state, LiveToolState::StreamingArgs);
    assert!(matches!(&parts[0], HubPart::Reasoning { text, .. } if text == "hm"));
    assert!(matches!(
        &parts[1],
        HubPart::ToolCall { id, name, arguments, .. }
            if id == "c1" && name == "write_file" && arguments == "{\"path\":\"a.txt\"}"
    ));

    // The step's message is stored: its calls now run; the text moved into the stored message.
    active.emit(AgentEvent::MessageSaved {
        message: stored(AgentRole::Assistant),
    });
    let (tool, parts) = call(&active);
    assert_eq!(tool.state, LiveToolState::Running);
    assert_eq!(parts.len(), 1, "only the call remains");

    active.emit(AgentEvent::ToolApprovalRequired {
        run_id: "r1".into(),
        call_id: "c1".into(),
        name: "write_file".into(),
        arguments: "{\"path\":\"a.txt\",\"content\":\"x\"}".into(),
        class: ToolClass::Edit,
        read_only: false,
        destructive: false,
        sandbox: SandboxKind::None,
        reason: "edits".into(),
        preview: None,
        network: false,
        risk: "low".into(),
        summary: "Write a.txt".into(),
        full_text: None,
        card: None,
    });
    let (tool, parts) = call(&active);
    assert_eq!(tool.state, LiveToolState::WaitingApproval);
    assert!(matches!(
        &parts[0],
        HubPart::ToolCall { arguments, .. } if arguments == "{\"path\":\"a.txt\",\"content\":\"x\"}"
    ));

    active.emit(AgentEvent::ToolApprovalResolved {
        call_id: "c1".into(),
        decision: ApprovalDecision::Allow,
        scope: ApprovalScope::Once,
        timed_out: false,
    });
    assert_eq!(call(&active).0.state, LiveToolState::Running);

    active.emit(AgentEvent::ToolResult {
        call_id: "c1".into(),
        name: "write_file".into(),
        status: ToolCallStatus::Ok,
        parts: vec![HubPart::text("é".repeat(300))],
        duration_ms: 7,
        meta: None,
        exit_code: None,
    });
    let (tool, _) = call(&active);
    assert_eq!(tool.state, LiveToolState::Done);
    let result = tool.result.unwrap();
    assert_eq!(result.status, ToolCallStatus::Ok);
    assert_eq!(result.summary, "é".repeat(200));
    assert_eq!(result.duration_ms, 7);

    // The next model step starts clean.
    active.emit(AgentEvent::TextDelta {
        text: "next".into(),
    });
    let snapshot = active.live_snapshot();
    assert!(snapshot.tools.is_empty());
    assert_eq!(live_text(&snapshot), "next");
}

#[test]
fn the_live_view_keeps_the_waiting_calls_card_and_the_runs_last_usage_across_steps() {
    use crate::agent::model::{LiveToolCall, ToolCallStatus};
    use crate::agent::permissions::{ApprovalCard, RiskLevel};
    use crate::agent::sandbox::{SandboxState, SandboxStatus};

    let active = run("r1", "s");
    let usage = |input| AgentEvent::Usage {
        iteration_usage: HubUsage::default(),
        run_usage: HubUsage {
            input_tokens: Some(input),
            ..HubUsage::default()
        },
        cost_micro_usd: 5,
        context_used_tokens: 9,
        context_limit_tokens: Some(100),
        currency: Some("USD".into()),
        ttft_ms: None,
        stream_ms: None,
    };
    assert!(active.live_snapshot().usage.is_none());
    active.emit(usage(10));
    active.emit(AgentEvent::ToolCallStarted {
        call_id: "c1".into(),
        name: "write_file".into(),
        source: ToolSource::Builtin,
    });
    let card = ApprovalCard {
        risk: RiskLevel::Low,
        risk_reason: None,
        tags: Vec::new(),
        operators: Vec::new(),
        why: None,
        rule_not_used: None,
        sandbox: SandboxStatus {
            state: SandboxState::Available,
            tier: SandboxKind::None,
        },
        summary: "Write a.txt".into(),
        offered_scopes: vec![ApprovalScope::Once],
        rule_label: None,
        scope_disabled_reason: None,
        layer: None,
        why_code: None,
    };
    active.emit(AgentEvent::ToolApprovalRequired {
        run_id: "r1".into(),
        call_id: "c1".into(),
        name: "write_file".into(),
        arguments: "{}".into(),
        class: ToolClass::Edit,
        read_only: false,
        destructive: false,
        sandbox: SandboxKind::None,
        reason: "edits".into(),
        preview: None,
        network: false,
        risk: "low".into(),
        summary: "Write a.txt".into(),
        full_text: None,
        card: Some(Box::new(card.clone())),
    });
    let waiting = |active: &ActiveRun| -> LiveToolCall { active.live_snapshot().tools.remove(0) };
    assert_eq!(waiting(&active).approval.as_deref(), Some(&card));

    active.emit(AgentEvent::ToolApprovalResolved {
        call_id: "c1".into(),
        decision: ApprovalDecision::Allow,
        scope: ApprovalScope::Once,
        timed_out: false,
    });
    assert_eq!(waiting(&active).approval, None);

    // A later usage replaces the earlier one, and a new model step keeps it.
    active.emit(AgentEvent::ToolResult {
        call_id: "c1".into(),
        name: "write_file".into(),
        status: ToolCallStatus::Ok,
        parts: Vec::new(),
        duration_ms: 1,
        meta: None,
        exit_code: None,
    });
    active.emit(AgentEvent::MessageSaved {
        message: stored(AgentRole::Assistant),
    });
    active.emit(usage(20));
    active.emit(AgentEvent::TextDelta {
        text: "next".into(),
    });
    let snapshot = active.live_snapshot();
    assert!(snapshot.tools.is_empty());
    let live = snapshot.usage.unwrap();
    assert_eq!(live.run_usage.input_tokens, Some(20));
    assert_eq!(
        (live.cost_micro_usd, live.context_limit_tokens),
        (5, Some(100))
    );
}

#[test]
fn the_live_view_keeps_a_waiting_cost_pause_until_the_run_goes_on() {
    let active = run("r1", "s");
    active.emit(AgentEvent::TextDelta {
        text: "step".into(),
    });
    active.emit(AgentEvent::MessageSaved {
        message: stored(AgentRole::Assistant),
    });
    active.emit(AgentEvent::CostLimitReached {
        run_id: "r1".into(),
        cost_micro: 520_000,
        cap_micro: 500_000,
        currency: "USD".into(),
        breakdown: None,
        next_cap_micro: Some(1_000_000),
        unpriced_models: None,
        tokens: None,
    });
    // A sub-agent's events while the run waits do not answer the pause.
    active.emit(AgentEvent::SubagentEvent {
        parent_call_id: "c1".into(),
        child_run_id: "child".into(),
        event: Box::new(AgentEvent::TextDelta {
            text: "child".into(),
        }),
    });
    let pause = active.live_snapshot().cost_limit.unwrap();
    assert_eq!(
        (pause.cost_micro, pause.cap_micro, pause.next_cap_micro),
        (520_000, 500_000, Some(1_000_000))
    );
    // Continuing: the next model call streams again and the pause is gone.
    active.emit(AgentEvent::TextDelta {
        text: "more".into(),
    });
    assert!(active.live_snapshot().cost_limit.is_none());
}

#[test]
fn a_task_call_names_the_child_run_it_started() {
    let active = run("r1", "s");
    active.emit(AgentEvent::ToolCallStarted {
        call_id: "c1".into(),
        name: "task".into(),
        source: ToolSource::Builtin,
    });
    active.emit(AgentEvent::SubagentStarted {
        parent_call_id: "c1".into(),
        child_run: Box::new(summary("child", "cs", AgentRunKind::Subagent, Some("r1"))),
        child_session_id: "cs".into(),
    });
    active.emit(AgentEvent::SubagentEvent {
        parent_call_id: "c1".into(),
        child_run_id: "child".into(),
        event: Box::new(AgentEvent::TextDelta {
            text: "child".into(),
        }),
    });
    let snapshot = active.live_snapshot();
    assert_eq!(snapshot.tools[0].child_run_id.as_deref(), Some("child"));
    assert_eq!(
        live_text(&snapshot),
        "",
        "the child's text is not the parent's"
    );
}

#[test]
fn the_live_text_is_capped_and_says_so() {
    let active = run("r1", "s");
    let chunk = "x".repeat(64 * 1024);
    for _ in 0..20 {
        active.emit(AgentEvent::TextDelta {
            text: chunk.clone(),
        });
    }
    let snapshot = active.live_snapshot();
    assert!(snapshot.truncated);
    assert!(live_text(&snapshot).len() <= live::MAX_LIVE_BYTES);
    assert!(live_text(&snapshot).len() > live::MAX_LIVE_BYTES / 2);
}

#[test]
fn a_snapshot_is_never_torn_by_a_concurrent_emit() {
    use std::sync::Barrier;
    use std::sync::atomic::{AtomicBool, Ordering};

    const EMITS: usize = 20_000;
    let active = run("r1", "s");
    let start = Arc::new(Barrier::new(2));
    let done = Arc::new(AtomicBool::new(false));
    let emitter = {
        let (active, start, done) = (Arc::clone(&active), Arc::clone(&start), Arc::clone(&done));
        std::thread::spawn(move || {
            start.wait();
            for _ in 0..EMITS {
                active.emit(AgentEvent::TextDelta { text: "x".into() });
            }
            done.store(true, Ordering::Release);
        })
    };
    start.wait();
    loop {
        let finished = done.load(Ordering::Acquire);
        let snapshot = active.live_snapshot();
        // Every emitted event adds one character, so text and position agree in any consistent view.
        assert_eq!(live_text(&snapshot).len() as u64, snapshot.seq);
        if finished {
            break;
        }
    }
    emitter.join().unwrap();
    let last = active.live_snapshot();
    assert_eq!((last.seq, live_text(&last).len()), (EMITS as u64, EMITS));
}

// While anything in a run's tree waits for the user, its top-level slot is free; the last
// wait to end asks for it back, and a run that never held one asks for nothing.
#[tokio::test]
async fn a_waiting_run_tree_frees_its_slot_until_the_last_wait_ends() {
    let registry = RunRegistry::default();
    let cancel = CancelToken::new();
    let root = run("r1", "s1");
    root.hold_slot(registry.acquire_slot(|| 1, &cancel).await.unwrap());
    assert_eq!(registry.slots_in_use(), 1);
    root.begin_wait();
    root.begin_wait();
    assert_eq!(registry.slots_in_use(), 0, "a waiting tree holds no slot");
    assert!(!root.end_wait(), "another wait is still open");
    assert!(root.end_wait(), "the last wait takes the slot back");
    root.hold_slot(registry.acquire_slot(|| 1, &cancel).await.unwrap());
    root.drop_slot();
    assert_eq!(registry.slots_in_use(), 0);
    let child = run("r2", "s2");
    child.begin_wait();
    assert!(!child.end_wait(), "a run without a slot takes none back");
}

#[test]
fn folding_command_output_keeps_only_its_tail_so_the_ring_frees_room() {
    let delta = |text: String| AgentEvent::ToolOutputDelta {
        call_id: "c1".into(),
        stream: crate::agent::tools::OutputStream::Stdout,
        text,
    };
    let mut folded = delta("a".repeat(FOLDED_OUTPUT_TAIL));
    assert!(merge_events(
        &mut folded,
        &delta("b".repeat(FOLDED_OUTPUT_TAIL))
    ));
    let AgentEvent::ToolOutputDelta { text, .. } = &folded else {
        panic!("still an output delta");
    };
    assert!(text.starts_with('…'), "the dropped head is marked");
    assert!(text.ends_with(&"b".repeat(FOLDED_OUTPUT_TAIL)));
    assert!(!text.contains('a'));
    assert!(event_size(&folded) < 2 * FOLDED_OUTPUT_TAIL);

    // Another call's output, or the model's text, is never folded into it.
    let other = AgentEvent::ToolOutputDelta {
        call_id: "c2".into(),
        stream: crate::agent::tools::OutputStream::Stdout,
        text: "x".into(),
    };
    assert!(!merge_events(&mut folded, &other));
}
