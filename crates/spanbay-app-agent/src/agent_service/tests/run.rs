//! Whole runs through `AppState` against a fake OpenAI-compatible upstream: start, replay,
//! approvals, stop, steer, queue, concurrency, cost cap, edit/retry and compaction, plus the
//! validations that need no model.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::atomic::{AtomicBool, Ordering};
use std::sync::{Arc, Mutex};
use std::time::Duration;

use serde_json::json;

use super::upstream::{
    ALIAS, Recorded, Reply, Upstream, call_answer, configure_llm, has_tool_result, json_answer,
    last_user_text, offers, text_answer,
};
use super::*;
use crate::agent::attention::AgentAttention;
use crate::agent::dto::{AgentRunInput, AgentRunStarted, AgentSubscribeTarget};
use crate::agent::model::{
    AgentEnvelope, AgentEvent, AgentRunKind, AgentRunStatus, AgentRunSummary, AgentStopReason,
    ApprovalDecision, ApprovalScope,
};
use crate::agent::permissions::PermissionAction;
use crate::events::{EventSink, Sequenced};
use crate::llm::hub::text_of;
use crate::llm::usage::PriceOverride;
use crate::settings::ToolApproval;

use super::super::{
    agent_approval_answer_impl, agent_attention_subscribe_impl, agent_context_compact_impl,
    agent_message_edit_impl, agent_message_retry_impl, agent_queue_add_impl, agent_queue_list_impl,
    agent_queue_resume_impl, agent_queue_steer_impl, agent_run_continue_impl, agent_run_list_impl,
    agent_run_start_impl, agent_run_steer_impl, agent_run_stop_all_impl, agent_run_stop_impl,
    agent_run_subscribe_impl, agent_run_unsubscribe_impl, agent_sandbox_acknowledge_impl,
};

pub(super) type Seen = Arc<Mutex<Vec<AgentEnvelope>>>;

/// The approval card's answer with every field spelled out.
#[allow(clippy::too_many_arguments)]
pub(super) fn answer(
    state: &AppState,
    run_id: &str,
    call_id: &str,
    decision: ApprovalDecision,
    scope: ApprovalScope,
    message: Option<String>,
    edited_arguments: Option<String>,
    allow_network_once: bool,
) -> Result<bool, AppError> {
    agent_approval_answer_impl(
        state,
        run_id,
        call_id,
        crate::agent::approvals::ApprovalAnswer {
            decision,
            scope,
            message,
            edited_arguments,
            allow_network_once,
        },
    )
}

pub(super) fn collector() -> (Seen, Arc<dyn EventSink<AgentEnvelope>>) {
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

pub(super) fn events(seen: &Seen) -> Vec<AgentEvent> {
    seen.lock()
        .unwrap()
        .iter()
        .map(|envelope| envelope.event.clone())
        .collect()
}

const GATE_POLLS: u32 = 6_000;

/// Polls `condition` for up to 20 s; a condition that never holds fails with `label`.
pub(super) async fn wait_until(label: &str, mut condition: impl FnMut() -> bool) {
    for _ in 0..2_000 {
        if condition() {
            return;
        }
        tokio::time::sleep(Duration::from_millis(10)).await;
    }
    panic!("timed out waiting for: {label}");
}

fn is_terminal(event: &AgentEvent) -> bool {
    matches!(event, AgentEvent::Done { .. } | AgentEvent::Error { .. })
}

pub(super) async fn wait_terminal(seen: &Seen, count: usize) {
    wait_until("the run's final event", || {
        events(seen).iter().filter(|e| is_terminal(e)).count() >= count
    })
    .await;
}

pub(super) struct World {
    _root: TempDir,
    pub(super) state: AppState,
    pub(super) upstream: Upstream,
}

pub(super) fn world(handler: impl Fn(usize, &Recorded) -> Reply + Send + Sync + 'static) -> World {
    let root = TempDir::new("run");
    let upstream = Upstream::start(handler);
    let state = AppState::open(root.path().join("spanbay.sqlite3")).expect("state opens");
    state.on_start().expect("data dir");
    configure_llm(&state, upstream.port);
    World {
        _root: root,
        state,
        upstream,
    }
}

fn reads_then_answers(_: usize, request: &Recorded) -> Reply {
    if has_tool_result(request) {
        text_answer("it says hello")
    } else if offers(request, "read_file") {
        call_answer("tc_1", "read_file", r#"{"path":"note.txt"}"#)
    } else {
        text_answer("plain answer")
    }
}

pub(super) fn chat(world: &World, mode: ToolApproval, workspace: Option<&Path>) -> AgentSession {
    agent_session_create_impl(
        &world.state,
        AgentSessionInput {
            target: Some(AgentTarget::Named {
                name: ALIAS.to_owned(),
            }),
            permission_mode: Some(mode),
            workspace_dir: Some(workspace.map_or(String::new(), |p| p.display().to_string())),
            ..AgentSessionInput::default()
        },
    )
    .unwrap()
}

pub(super) fn input(session: &AgentSession, text: &str) -> AgentRunInput {
    AgentRunInput {
        session_id: session.id.clone(),
        text: text.to_owned(),
        ..AgentRunInput::default()
    }
}

pub(super) fn watch(world: &World, session: &AgentSession) -> Seen {
    let (seen, sink) = collector();
    agent_run_subscribe_impl(
        &world.state,
        &AgentSubscribeTarget::Session {
            session_id: session.id.clone(),
        },
        None,
        sink,
    )
    .unwrap();
    seen
}

pub(super) fn messages_of(world: &World, session: &AgentSession) -> Vec<AgentMessage> {
    agent_session_get_impl(
        &world.state,
        &session.id,
        AgentPageRequest {
            before_seq: None,
            limit: None,
            ..AgentPageRequest::default()
        },
    )
    .unwrap()
    .messages
}

pub(super) fn workspace_with(file: &str, content: &str) -> TempDir {
    let dir = TempDir::new("ws");
    fs::write(dir.path().join(file), content).unwrap();
    dir
}

#[tokio::test(flavor = "multi_thread")]
async fn text_and_tool_runs_stream_title_audit_instructions_and_redact_stored_secrets() {
    let world = world(reads_then_answers);
    settings_update_impl(
        &world.state,
        SettingsSection::Ai,
        &json!({"agent": {"global_instructions": "Be brief."}}),
    )
    .unwrap();
    let secret = "tvly-this-is-a-stored-secret-value";
    world
        .state
        .secrets()
        .put(crate::secrets::SEARCH_TAVILY, secret)
        .unwrap();
    let workspace = workspace_with("note.txt", &format!("hello file {secret}"));
    let session = chat(&world, ToolApproval::AutoReadOnly, Some(workspace.path()));
    let session = agent_session_update_impl(
        &world.state,
        &session.id,
        AgentSessionPatch {
            instructions: Some("Answer in French.".to_owned()),
            ..AgentSessionPatch::default()
        },
    )
    .unwrap();
    let seen = watch(&world, &session);

    let started = agent_run_start_impl(&world.state, input(&session, "Please read the note"))
        .await
        .unwrap();
    wait_terminal(&seen, 1).await;

    let evs = events(&seen);
    let AgentEvent::Done { run, stop_reason } = evs.last().unwrap() else {
        panic!("expected Done");
    };
    assert_eq!(*stop_reason, AgentStopReason::Completed);
    assert_eq!(run.run_id, started.run_id);

    let stored =
        agent_session_get_impl(&world.state, &session.id, AgentPageRequest::default()).unwrap();
    assert_eq!(stored.session.title, "Please read the note");
    let serialized = serde_json::to_string(&stored.messages).unwrap();
    assert!(!serialized.contains(secret) && serialized.contains("hello file"));

    let reqs = world.upstream.requests();
    let system = reqs[0].body["messages"][0]["content"].as_str().unwrap();
    assert!(system.contains("Be brief.") && system.contains("Answer in French."));
    assert!(!reqs[1].body.to_string().contains(secret));

    let audit = world.state.store().list_tool_calls(&session.id).unwrap();
    assert_eq!(audit.len(), 1);
    assert_eq!(
        (audit[0].name.as_str(), audit[0].state.as_str()),
        ("read_file", "output-available")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn approvals_support_allow_with_edited_args_scoped_rules_and_deny() {
    let world = world(|_, request| {
        if has_tool_result(request) {
            text_answer("done")
        } else {
            call_answer(
                "call_w",
                "write_file",
                r#"{"path":"out.txt","content":"hi"}"#,
            )
        }
    });
    let workspace = TempDir::new("ws");
    let session = chat(&world, ToolApproval::Ask, Some(workspace.path()));
    let seen = watch(&world, &session);
    let started = agent_run_start_impl(&world.state, input(&session, "write it"))
        .await
        .unwrap();

    wait_until("the approval request", || {
        events(&seen)
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolApprovalRequired { .. }))
    })
    .await;
    assert_eq!(
        agent_run_list_impl(&world.state, None).unwrap()[0].status,
        AgentRunStatus::WaitingApproval
    );

    // Invalid edited arguments keep the approval pending.
    let bad = answer(
        &world.state,
        &started.run_id,
        "call_w",
        ApprovalDecision::Allow,
        ApprovalScope::Once,
        None,
        Some("{not json".into()),
        false,
    )
    .unwrap_err();
    assert!(matches!(bad, AppError::Validation { .. }));

    // Remember the edited call in this folder; the deny case uses a separate folder.
    assert!(
        answer(
            &world.state,
            &started.run_id,
            "call_w",
            ApprovalDecision::Allow,
            ApprovalScope::WorkspacePattern,
            None,
            Some(r#"{"path":"edited.txt","content":"edited"}"#.into()),
            false,
        )
        .unwrap()
    );
    wait_terminal(&seen, 1).await;
    assert_eq!(
        fs::read_to_string(workspace.path().join("edited.txt")).unwrap(),
        "edited"
    );
    let rules = world
        .state
        .store()
        .list_permission_rules(Some(&session.id))
        .unwrap();
    assert_eq!((rules.len(), rules[0].action), (1, PermissionAction::Allow));

    // Deny branch: does not write and sends denial reason to model.
    let deny_workspace = TempDir::new("deny-ws");
    let deny_session = chat(&world, ToolApproval::Ask, Some(deny_workspace.path()));
    let deny_seen = watch(&world, &deny_session);
    let deny_run = agent_run_start_impl(&world.state, input(&deny_session, "write"))
        .await
        .unwrap();
    wait_until("deny approval request", || {
        events(&deny_seen)
            .iter()
            .any(|e| matches!(e, AgentEvent::ToolApprovalRequired { .. }))
    })
    .await;
    answer(
        &world.state,
        &deny_run.run_id,
        "call_w",
        ApprovalDecision::Deny,
        ApprovalScope::Once,
        Some("not that file".into()),
        None,
        false,
    )
    .unwrap();
    wait_terminal(&deny_seen, 1).await;
    assert!(!deny_workspace.path().join("out.txt").exists());
    assert!(
        world
            .upstream
            .requests()
            .last()
            .unwrap()
            .body
            .to_string()
            .contains("not that file")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_stop_all_steer_and_queue_pause_resume_lifecycle() {
    let release = Arc::new(AtomicBool::new(false));
    let fail = Arc::new(AtomicBool::new(false));
    let (gate, failing) = (Arc::clone(&release), Arc::clone(&fail));
    let world = world(move |number, _| {
        if number == 0 || failing.load(Ordering::SeqCst) {
            for _ in 0..GATE_POLLS {
                if gate.load(Ordering::SeqCst) {
                    break;
                }
                std::thread::sleep(Duration::from_millis(10));
            }
        }
        if failing.load(Ordering::SeqCst) {
            Reply::Json(400, json!({"error": {"message": "bad request"}}))
        } else {
            text_answer("answer")
        }
    });
    let session = chat(&world, ToolApproval::AutoReadOnly, None);
    let seen = watch(&world, &session);
    let started = agent_run_start_impl(&world.state, input(&session, "start"))
        .await
        .unwrap();
    wait_until("the first call", || !world.upstream.requests().is_empty()).await;

    // A second run in a busy chat is a conflict; steer joins the active turn.
    assert!(matches!(
        agent_run_start_impl(&world.state, input(&session, "second")).await,
        Err(AppError::Conflict { .. })
    ));
    assert!(
        agent_run_steer_impl(
            &world.state,
            &started.run_id,
            "also this",
            &[],
            Vec::new(),
            &[]
        )
        .unwrap()
    );
    release.store(true, Ordering::SeqCst);
    wait_terminal(&seen, 1).await;
    assert_eq!(last_user_text(&world.upstream.requests()[1]), "also this");

    // Failed run pauses the queue until resumed; stop_all cancels active runs.
    release.store(false, Ordering::SeqCst);
    fail.store(true, Ordering::SeqCst);
    agent_run_start_impl(&world.state, input(&session, "fail me"))
        .await
        .unwrap();
    wait_until("the failing call", || world.upstream.requests().len() == 3).await;
    agent_queue_add_impl(
        &world.state,
        &session.id,
        vec![HubPart::text("queued")],
        Vec::new(),
        &[],
    )
    .await
    .unwrap();
    release.store(true, Ordering::SeqCst);
    wait_terminal(&seen, 2).await;
    assert!(world.state.agent.runs.queue_paused(&session.id));
    assert_eq!(
        agent_queue_list_impl(&world.state, &session.id)
            .unwrap()
            .len(),
        1
    );

    fail.store(false, Ordering::SeqCst);
    assert!(
        agent_queue_resume_impl(&world.state, &session.id)
            .await
            .unwrap()
            .is_empty()
    );
    wait_terminal(&seen, 3).await;

    // Stop and stop_all.
    release.store(false, Ordering::SeqCst);
    fail.store(true, Ordering::SeqCst);
    let other = chat(&world, ToolApproval::AutoReadOnly, None);
    let seen_other = watch(&world, &other);
    let s1 = agent_run_start_impl(&world.state, input(&other, "hold"))
        .await
        .unwrap();
    wait_until("the hold call", || world.upstream.requests().len() == 5).await;
    assert!(agent_run_stop_impl(&world.state, &s1.run_id));
    wait_terminal(&seen_other, 1).await;
    assert_eq!(agent_run_stop_all_impl(&world.state), 0);
    release.store(true, Ordering::SeqCst);
}

#[tokio::test(flavor = "multi_thread")]
async fn queue_steer_joins_the_run_or_goes_back_to_the_front() {
    let release = Arc::new(AtomicBool::new(false));
    let gate = Arc::clone(&release);
    let world = world(move |_, _| {
        for _ in 0..GATE_POLLS {
            if gate.load(Ordering::SeqCst) {
                break;
            }
            std::thread::sleep(Duration::from_millis(10));
        }
        text_answer("answer")
    });
    let session = chat(&world, ToolApproval::AutoReadOnly, None);
    let seen = watch(&world, &session);
    let queue = |text: &'static str| {
        agent_queue_add_impl(
            &world.state,
            &session.id,
            vec![HubPart::text(text)],
            Vec::new(),
            &[],
        )
    };
    let texts = || {
        agent_queue_list_impl(&world.state, &session.id)
            .unwrap()
            .iter()
            .map(|turn| text_of(&turn.parts))
            .collect::<Vec<_>>()
    };

    // Joined: the steered message leaves the queue and reaches the model in the same run.
    agent_run_start_impl(&world.state, input(&session, "start"))
        .await
        .unwrap();
    wait_until("the first call", || !world.upstream.requests().is_empty()).await;
    let first = queue("steer me").await.unwrap();
    assert!(
        agent_queue_steer_impl(&world.state, &session.id, &first[0].id)
            .await
            .unwrap()
    );
    assert!(texts().is_empty());
    assert!(
        !agent_queue_steer_impl(&world.state, &session.id, "ghost")
            .await
            .unwrap()
    );
    release.store(true, Ordering::SeqCst);
    wait_terminal(&seen, 1).await;
    assert_eq!(last_user_text(&world.upstream.requests()[1]), "steer me");

    // Not joined: the run is stopped first, so the steer returns to the front of a paused queue.
    release.store(false, Ordering::SeqCst);
    let started = agent_run_start_impl(&world.state, input(&session, "hold"))
        .await
        .unwrap();
    wait_until("the hold call", || world.upstream.requests().len() == 3).await;
    queue("first").await.unwrap();
    let both = queue("second").await.unwrap();
    assert!(
        agent_queue_steer_impl(&world.state, &session.id, &both[1].id)
            .await
            .unwrap()
    );
    assert!(agent_run_stop_impl(&world.state, &started.run_id));
    wait_terminal(&seen, 2).await;
    release.store(true, Ordering::SeqCst);
    assert_eq!(texts(), ["second", "first"]);
    assert!(world.state.agent.runs.queue_paused(&session.id));
    // No active run: nothing to steer into.
    assert!(
        !agent_queue_steer_impl(&world.state, &session.id, &both[0].id)
            .await
            .unwrap()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn late_subscribers_replay_finished_runs_and_attention_tracks_active_work() {
    let world = world(|_, _| text_answer("hi"));
    let session = chat(&world, ToolApproval::AutoReadOnly, None);
    let attention: Arc<Mutex<Vec<Sequenced<AgentAttention>>>> = Arc::default();
    let sink = {
        let attention = Arc::clone(&attention);
        move |event: Sequenced<AgentAttention>| {
            attention.lock().unwrap().push(event);
            true
        }
    };
    agent_attention_subscribe_impl(&world.state, None, Arc::new(sink)).unwrap();
    let seen = watch(&world, &session);
    let started = agent_run_start_impl(&world.state, input(&session, "hello"))
        .await
        .unwrap();
    wait_terminal(&seen, 1).await;

    let (late, sink) = collector();
    let id = agent_run_subscribe_impl(
        &world.state,
        &AgentSubscribeTarget::Run {
            run_id: started.run_id.clone(),
        },
        None,
        sink,
    )
    .unwrap();
    assert_eq!(events(&late), events(&seen));
    assert!(agent_run_unsubscribe_impl(&world.state, id));
    assert!(!agent_run_unsubscribe_impl(&world.state, id));
    assert!(
        attention
            .lock()
            .unwrap()
            .iter()
            .any(|s| s.event.active_runs == 1)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn cost_cap_pauses_and_continues_or_cancels() {
    let world = world(reads_then_answers);
    spanbay_app_llm::llm_price_override_save_impl(
        &world.state,
        PriceOverride {
            provider_id: "fake".into(),
            model_id: super::upstream::MODEL.into(),
            input_per_mtok: 1_000_000.0,
            output_per_mtok: 1_000_000.0,
            cache_read_per_mtok: None,
            cache_write_per_mtok: None,
            reasoning_per_mtok: None,
            currency: "USD".into(),
            updated_at_ms: 0,
        },
    )
    .unwrap();
    settings_update_impl(
        &world.state,
        SettingsSection::Ai,
        &json!({"agent": {"cost_cap_usd": 5.0}}),
    )
    .unwrap();
    let workspace = workspace_with("note.txt", "hello file");
    let session = chat(&world, ToolApproval::AutoReadOnly, Some(workspace.path()));
    let seen = watch(&world, &session);
    let started = agent_run_start_impl(&world.state, input(&session, "read"))
        .await
        .unwrap();
    wait_until("the cost pause", || {
        events(&seen)
            .iter()
            .any(|e| matches!(e, AgentEvent::CostLimitReached { .. }))
    })
    .await;
    // The pause is a gate: the run says what it waits for and the attention feed counts it.
    let paused = world
        .state
        .agent
        .runs
        .get(&started.run_id)
        .unwrap()
        .summary();
    assert_eq!(
        (paused.status, paused.status_detail.as_deref()),
        (AgentRunStatus::WaitingApproval, Some("cost limit"))
    );
    assert_eq!(
        world
            .state
            .agent
            .gates
            .count(crate::agent::gate::GateKind::Cost),
        1
    );
    assert!(agent_run_continue_impl(&world.state, &started.run_id, true));
    assert!(!agent_run_continue_impl(
        &world.state,
        &started.run_id,
        true
    ));
    wait_terminal(&seen, 1).await;
    assert!(matches!(
        events(&seen).last(),
        Some(AgentEvent::Done { .. })
    ));

    // Stopping at cost cap marks the run Cancelled with cost_limit stop_reason.
    let stop_session = chat(&world, ToolApproval::AutoReadOnly, Some(workspace.path()));
    let stop_seen = watch(&world, &stop_session);
    let second = agent_run_start_impl(&world.state, input(&stop_session, "again"))
        .await
        .unwrap();
    wait_until("second cost pause", || {
        events(&stop_seen)
            .iter()
            .any(|e| matches!(e, AgentEvent::CostLimitReached { .. }))
    })
    .await;
    assert!(agent_run_continue_impl(&world.state, &second.run_id, false));
    wait_terminal(&stop_seen, 1).await;
    let run_row = agent_run_list_impl(&world.state, None)
        .unwrap()
        .into_iter()
        .find(|r| r.run_id == second.run_id)
        .unwrap();
    assert_eq!(
        (run_row.status, run_row.stop_reason.as_deref()),
        (AgentRunStatus::Cancelled, Some("cost_limit"))
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_edit_and_manual_compaction_update_history() {
    let world = world(|number, request| {
        if request.body["stream"] == true {
            text_answer(&format!("answer {number}"))
        } else {
            json_answer("they discussed earlier turns")
        }
    });
    let session = chat(&world, ToolApproval::AutoReadOnly, None);
    let seen = watch(&world, &session);
    let first = agent_run_start_impl(&world.state, input(&session, "question"))
        .await
        .unwrap();
    wait_terminal(&seen, 1).await;
    wait_until("the initial run releases the chat", || {
        world
            .state
            .agent
            .runs
            .active_for_session(&session.id)
            .is_none()
    })
    .await;

    // Short chat compaction is refused.
    assert!(matches!(
        agent_context_compact_impl(&world.state, &session.id).await,
        Err(AppError::Validation { .. })
    ));

    let assistant = messages_of(&world, &session)
        .into_iter()
        .find(|m| m.role == AgentRole::Assistant)
        .unwrap();
    agent_message_retry_impl(&world.state, &session.id, &assistant.id, false)
        .await
        .unwrap();
    wait_terminal(&seen, 2).await;
    wait_until("the retry releases the chat", || {
        world
            .state
            .agent
            .runs
            .active_for_session(&session.id)
            .is_none()
    })
    .await;

    agent_message_edit_impl(
        &world.state,
        &session.id,
        &first.user_message_id,
        "better question".into(),
        Vec::new(),
        false,
    )
    .await
    .unwrap();
    wait_terminal(&seen, 3).await;
    wait_until("the edit releases the chat", || {
        world
            .state
            .agent
            .runs
            .active_for_session(&session.id)
            .is_none()
    })
    .await;
    assert_eq!(
        text_of(&messages_of(&world, &session)[0].parts),
        "better question"
    );

    for turn in 0..3 {
        append(
            &world.state,
            &message(
                &session.id,
                &format!("u{turn}"),
                AgentRole::User,
                // Longer than the summary, which is stored only when it is shorter.
                &format!("q {turn} {}", "x".repeat(200)),
                10 + turn,
            ),
        );
        append(
            &world.state,
            &message(
                &session.id,
                &format!("a{turn}"),
                AgentRole::Assistant,
                &format!("a {turn}"),
                20 + turn,
            ),
        );
    }
    let summary = agent_context_compact_impl(&world.state, &session.id)
        .await
        .unwrap();
    assert_eq!(
        (summary.status, summary.kind),
        (AgentRunStatus::Completed, AgentRunKind::Background)
    );
    assert_eq!(
        world.state.store().list_model_history(&session.id).unwrap()[0].role,
        AgentRole::Summary
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn starting_validates_inputs_and_startup_interrupts_stale_runs() {
    fn assert_send<T: Send>(_: &T) {}
    let state = state();
    let future = agent_run_start_impl(&state, AgentRunInput::default());
    assert_send(&future);
    let started: Option<AgentRunStarted> = None;
    assert!(started.is_none());

    let blank = session(&state, "No model");
    for (session_id, text) in [(blank.id.as_str(), "hello"), ("missing", "hi")] {
        assert!(
            agent_run_start_impl(
                &state,
                AgentRunInput {
                    session_id: session_id.into(),
                    text: text.into(),
                    ..AgentRunInput::default()
                }
            )
            .await
            .is_err()
        );
    }

    let dir = TempDir::new("start-unknown-attachment");
    let disk = super::attachments::disk_state(&dir);
    let chat = session(&disk, "Disk chat");
    assert!(
        agent_run_start_impl(
            &disk,
            AgentRunInput {
                session_id: chat.id.clone(),
                text: "hi".into(),
                attachment_ids: vec!["att_missing".into()],
                ..AgentRunInput::default()
            }
        )
        .await
        .is_err()
    );

    let stale = AgentRunSummary {
        run_id: "old".into(),
        session_id: chat.id,
        kind: AgentRunKind::Main,
        parent_run_id: None,
        parent_call_id: None,
        status: AgentRunStatus::Running,
        model_label: "m".into(),
        target: None,
        iterations: 1,
        usage: crate::llm::hub::HubUsage::default(),
        cost_micro_usd: 0,
        started_at_ms: 1,
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
    };
    disk.store().insert_agent_run(&stale).unwrap();
    disk.on_start().unwrap();
    assert_eq!(
        agent_run_list_impl(&disk, None).unwrap()[0].status,
        AgentRunStatus::Interrupted
    );
    assert_eq!(
        super::super::run::title_from("  hello \n  world  "),
        "hello world"
    );
}

#[test]
fn the_automatic_title_leaves_out_context_parts_and_names_a_context_only_message_by_its_label() {
    use super::super::run::title_of;
    let probe = "<context kind=\"probe\" label=\"Probe: chat ready\">\nprovider: a\nstatus: ready\n</context>";
    let request =
        "<context kind=\"request\" id=\"rq_1\" label=\"POST /invoices\">\nbody\n</context>";
    assert_eq!(
        title_of(&[HubPart::text("Why did this fail?"), HubPart::text(probe)]),
        "Why did this fail?"
    );
    assert_eq!(
        title_of(&[HubPart::text(request), HubPart::text(probe)]),
        "POST /invoices"
    );
    // Text that only looks like a tag at its start is the user's own words.
    assert_eq!(
        title_of(&[HubPart::text("<context kind=\"x\"> is a tag")]),
        "<context kind=\"x\"> is a tag"
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn an_acknowledged_chat_shells_outside_the_sandbox_and_the_step_records_it() {
    let world = world(|_, request| {
        if has_tool_result(request) {
            text_answer("ran")
        } else {
            call_answer("call_sh", "shell", r#"{"command":"echo hi"}"#)
        }
    });
    let workspace = workspace_with("note.txt", "hello");
    let session = chat(&world, ToolApproval::AutoReadOnly, Some(workspace.path()));
    agent_sandbox_acknowledge_impl(&world.state, &session.id, true);
    let seen = watch(&world, &session);
    let started = agent_run_start_impl(&world.state, input(&session, "say hi"))
        .await
        .unwrap();
    wait_until("the shell card", || {
        events(&seen)
            .iter()
            .any(|event| matches!(event, AgentEvent::ToolApprovalRequired { .. }))
    })
    .await;
    let (call_id, sandbox) = events(&seen)
        .into_iter()
        .find_map(|event| match event {
            AgentEvent::ToolApprovalRequired {
                call_id, sandbox, ..
            } => Some((call_id, sandbox)),
            _ => None,
        })
        .unwrap();
    assert_eq!(sandbox, crate::agent::sandbox::SandboxKind::None);
    answer(
        &world.state,
        &started.run_id,
        &call_id,
        ApprovalDecision::Allow,
        ApprovalScope::Once,
        None,
        None,
        false,
    )
    .unwrap();
    wait_terminal(&seen, 1).await;
    let audit = world.state.store().list_tool_calls(&session.id).unwrap();
    assert_eq!(audit[0].sandbox.as_deref(), Some("skipped_by_choice"));
    // The command's exit status reaches the stored row, where the UI reads it.
    assert_eq!(audit[0].exit_code, Some(0));
}

#[tokio::test(flavor = "multi_thread")]
async fn model_title_generation_follows_six_rule_lifecycle() {
    use super::super::agent_session_generate_title_impl;
    use super::super::run::{PROVISIONAL_TITLE_MAX_CHARS, title_from};
    use super::super::title::GENERATED_TITLE_MAX_CHARS;

    // Rule 2: overly long first question is truncated to PROVISIONAL_TITLE_MAX_CHARS.
    let long_question = "为什么网关遇到上游返回 429 限流状态码时就直接熔断 60 秒而不先遵循 Retry-After 响应头进行退避重试？";
    let expected_provisional = title_from(long_question);
    assert_eq!(
        expected_provisional.chars().count(),
        PROVISIONAL_TITLE_MAX_CHARS
    );
    assert!(expected_provisional.ends_with('…'));

    // --- Scenario A: Rules 1, 2, 3, 4, 6 (first turn -> truncated -> AI summary -> locked -> Turn 2 unchanged) ---
    let w = world(|_, req| {
        let is_title_call = req.body["messages"][0]["content"]
            .as_str()
            .is_some_and(|s| s.contains("concise, clear title"));
        if is_title_call {
            json_answer("修复网关 429 熔断与退避")
        } else {
            text_answer("好的，我来检查网关熔断逻辑。")
        }
    });
    let session = chat(&w, ToolApproval::Ask, None);
    let seen = watch(&w, &session);

    // Rule 1 & 2: first question sets truncated provisional title and emits SessionTitleUpdated.
    agent_run_start_impl(&w.state, input(&session, long_question))
        .await
        .unwrap();
    wait_terminal(&seen, 1).await;
    let after_turn1 = agent_session_get_impl(&w.state, &session.id, AgentPageRequest::default())
        .unwrap()
        .session;
    assert_eq!(after_turn1.title, expected_provisional);
    assert!(!after_turn1.title_locked);

    // Mismatched expected_current_title skips overwrite (protects manual rename race).
    let skipped = agent_session_generate_title_impl(&w.state, &session.id, Some("other"))
        .await
        .unwrap();
    assert_eq!(skipped, None);

    // Rule 3 & 4: AI request summarizes within GENERATED_TITLE_MAX_CHARS, updates title, and locks it.
    let updated =
        agent_session_generate_title_impl(&w.state, &session.id, Some(&expected_provisional))
            .await
            .unwrap();
    assert_eq!(updated.as_deref(), Some("修复网关 429 熔断与退避"));
    assert!(
        updated
            .as_deref()
            .is_some_and(|t| t.chars().count() <= GENERATED_TITLE_MAX_CHARS)
    );
    let after_ai = agent_session_get_impl(&w.state, &session.id, AgentPageRequest::default())
        .unwrap()
        .session;
    assert_eq!(after_ai.title, "修复网关 429 熔断与退避");
    assert!(after_ai.title_locked);

    // Rule 6: subsequent turns never alter the title and emit no further SessionTitleUpdated.
    let title_events_before = events(&seen)
        .into_iter()
        .filter(|e| matches!(e, AgentEvent::SessionTitleUpdated { .. }))
        .count();
    agent_run_start_impl(&w.state, input(&session, "接下来帮我顺便优化日志导出功能"))
        .await
        .unwrap();
    wait_terminal(&seen, 2).await;
    let after_turn2 = agent_session_get_impl(&w.state, &session.id, AgentPageRequest::default())
        .unwrap()
        .session;
    assert_eq!(after_turn2.title, "修复网关 429 熔断与退避");
    assert!(after_turn2.title_locked);
    let title_events_after = events(&seen)
        .into_iter()
        .filter(|e| matches!(e, AgentEvent::SessionTitleUpdated { .. }))
        .count();
    assert_eq!(title_events_before, title_events_after);

    // A second automatic generation call is permanently blocked by title_locked.
    let repeat_skip =
        agent_session_generate_title_impl(&w.state, &session.id, Some("修复网关 429 熔断与退避"))
            .await
            .unwrap();
    assert_eq!(repeat_skip, None);

    // --- Scenario B: Rule 5 & 6 (AI title fails -> keep truncated title, lock forever, no retries) ---
    let title_attempts = Arc::new(AtomicUsize::new(0));
    let attempts_clone = Arc::clone(&title_attempts);
    let wf = world(move |_, req| {
        let is_title_call = req.body["messages"][0]["content"]
            .as_str()
            .is_some_and(|s| s.contains("concise, clear title"));
        if is_title_call {
            attempts_clone.fetch_add(1, Ordering::SeqCst);
            Reply::Json(400, json!({"error": {"message": "bad title request"}}))
        } else {
            text_answer("收到。")
        }
    });
    let sf = chat(&wf, ToolApproval::Ask, None);
    let seen_f = watch(&wf, &sf);
    agent_run_start_impl(&wf.state, input(&sf, long_question))
        .await
        .unwrap();
    wait_terminal(&seen_f, 1).await;

    // AI title call fails: returns Ok(None), keeps provisional title untouched, locks permanently.
    let failed_res =
        agent_session_generate_title_impl(&wf.state, &sf.id, Some(&expected_provisional))
            .await
            .unwrap();
    assert_eq!(failed_res, None);
    assert_eq!(title_attempts.load(Ordering::SeqCst), 1);
    let locked_after_fail = agent_session_get_impl(&wf.state, &sf.id, AgentPageRequest::default())
        .unwrap()
        .session;
    assert_eq!(locked_after_fail.title, expected_provisional);
    assert!(locked_after_fail.title_locked);

    // Retrying automatic generation does NOT hit the model again and never alters the title.
    let no_retry =
        agent_session_generate_title_impl(&wf.state, &sf.id, Some(&expected_provisional))
            .await
            .unwrap();
    assert_eq!(no_retry, None);
    assert_eq!(title_attempts.load(Ordering::SeqCst), 1);

    // Subsequent Turn 2 also leaves the failed-to-upgrade title untouched.
    agent_run_start_impl(&wf.state, input(&sf, "第二个完全不同的问题"))
        .await
        .unwrap();
    wait_terminal(&seen_f, 2).await;
    let after_fail_turn2 = agent_session_get_impl(&wf.state, &sf.id, AgentPageRequest::default())
        .unwrap()
        .session;
    assert_eq!(after_fail_turn2.title, expected_provisional);
    assert!(after_fail_turn2.title_locked);

    // --- Scenario C: auto_title = false skips automatic title entirely ---
    settings_update_impl(
        &w.state,
        SettingsSection::Ai,
        &json!({"agent": {"auto_title": false}}),
    )
    .unwrap();
    let s2 = chat(&w, ToolApproval::Ask, None);
    let disabled_skip = agent_session_generate_title_impl(&w.state, &s2.id, Some(&s2.title))
        .await
        .unwrap();
    assert_eq!(disabled_skip, None);
}
