//! `todo_write` and the `task` sub-agent through whole runs against the fake upstream.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::sync::Arc;
use std::time::Duration;

use futures_util::FutureExt;
use futures_util::future::BoxFuture;
use serde_json::json;
use tokio::sync::watch as watch_channel;

use super::run::{
    Seen, World, answer, chat, collector, events, input, wait_terminal, wait_until, watch,
    workspace_with, world,
};
use super::upstream::{
    Recorded, Reply, call_answer, has_tool_result, is_subagent, offers, scripted, text_answer,
    tool_result_count,
};
use super::*;
use crate::agent::approvals::{ApprovalGate, ApprovalOutcome, RuleContext};
use crate::agent::attention::PendingApprovalSummary;
use crate::agent::dto::{AgentRunStarted, AgentSubscribeTarget};
use crate::agent::model::{
    AgentEvent, AgentRunKind, AgentRunStatus, AgentSessionFilter, AgentTodoStatus,
    ApprovalDecision, ApprovalScope, ToolCallStatus,
};
use crate::agent::registry::ActiveRun;
use crate::agent::tools::{TestParts, ToolContext, ToolRegistry};
use crate::agent_tools::subagent;
use crate::cancel::CancelToken;
use crate::llm::hub::text_of;
use crate::llm::usage::PriceOverride;
use crate::settings::{SettingsSection, ToolApproval};

use super::super::host::session_mode;
use super::super::run::initial_summary;
use super::super::{
    agent_run_continue_impl, agent_run_start_impl, agent_run_stop_impl, agent_run_subscribe_impl,
    agent_run_unsubscribe_impl, agent_session_list_impl,
};

fn task_args(description: &str, prompt: &str, kind: &str) -> String {
    json!({"description": description, "prompt": prompt, "kind": kind}).to_string()
}

fn delegates(
    kind: &'static str,
    child: impl Fn(&Recorded) -> Reply + Send + Sync + 'static,
) -> impl Fn(usize, &Recorded) -> Reply + Send + Sync + 'static {
    move |_, request| {
        if is_subagent(request) {
            child(request)
        } else if has_tool_result(request) {
            text_answer("parent done")
        } else {
            call_answer(
                "call_task",
                "task",
                &task_args(
                    "Look at the note",
                    "Read note.txt and report what it says",
                    kind,
                ),
            )
        }
    }
}

fn started_child(events: &[AgentEvent]) -> Option<(String, String)> {
    events.iter().find_map(|event| match event {
        AgentEvent::SubagentStarted {
            child_run,
            child_session_id,
            ..
        } => Some((child_run.run_id.clone(), child_session_id.clone())),
        _ => None,
    })
}

fn tool_result(events: &[AgentEvent], call: &str) -> (ToolCallStatus, String) {
    events
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolResult {
                call_id,
                status,
                parts,
                ..
            } if call_id == call => Some((*status, text_of(parts))),
            _ => None,
        })
        .unwrap_or_else(|| panic!("no result for {call}"))
}

fn price_a_call_at_18_dollars(world: &World) {
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
}

async fn run_to_end(world: &World, session: &AgentSession, text: &str) -> (AgentRunStarted, Seen) {
    let seen = watch(world, session);
    let started = agent_run_start_impl(&world.state, input(session, text))
        .await
        .unwrap();
    wait_terminal(&seen, 1).await;
    (started, seen)
}

#[tokio::test(flavor = "multi_thread")]
async fn todo_write_validates_stores_and_publishes_todos() {
    let valid = json!({"todos": [
        {"content": "read the code", "status": "completed"},
        {"id": "keep", "content": "write the fix", "status": "in_progress"},
    ]})
    .to_string();
    let bad = json!({"todos": [
        {"content": "a", "status": "in_progress"},
        {"content": "b", "status": "in_progress"},
    ]})
    .to_string();
    let world = world(scripted(vec![("todo_write", valid), ("todo_write", bad)]));
    let session = chat(&world, ToolApproval::AutoReadOnly, None);
    let (_, seen) = run_to_end(&world, &session, "plan it").await;

    let evs = events(&seen);
    assert_eq!(tool_result(&evs, "tc_0").0, ToolCallStatus::Ok);
    let (status, err_text) = tool_result(&evs, "tc_1");
    assert_eq!(status, ToolCallStatus::Error);
    assert!(err_text.contains("at most one"));
    let stored = world
        .state
        .store()
        .get_agent_session(&session.id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.todos.len(), 2);
    assert_eq!(stored.todos[1].status, AgentTodoStatus::InProgress);
}

#[tokio::test(flavor = "multi_thread")]
async fn task_runs_hidden_child_enforces_tool_sets_and_caps_output() {
    let over = 5_000;
    let world = world(delegates("explore", move |request| {
        if has_tool_result(request) {
            text_answer(&"y".repeat(16 * 1024 + over))
        } else {
            call_answer("child_read", "read_file", r#"{"path":"note.txt"}"#)
        }
    }));
    settings_update_impl(
        &world.state,
        SettingsSection::Ai,
        &json!({"agent": {"max_concurrent_runs": 1}}),
    )
    .unwrap();
    let workspace = workspace_with("note.txt", "hello file");
    let session = chat(&world, ToolApproval::AutoReadOnly, Some(workspace.path()));
    let (started, seen) = run_to_end(&world, &session, "delegate").await;

    let evs = events(&seen);
    let (status, text) = tool_result(&evs, "call_task");
    assert_eq!(status, ToolCallStatus::Ok);
    // A long report keeps its start and a `read_output` handle to the rest.
    assert!(text.contains("[output truncated: showing "), "{text}");
    assert!(text.contains("read_output"), "{text}");

    let (child_run_id, child_session_id) = started_child(&evs).expect("SubagentStarted");
    let child = world
        .state
        .store()
        .get_agent_session(&child_session_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        child.parent_session_id.as_deref(),
        Some(session.id.as_str())
    );
    assert_eq!(
        child.parent_run_id.as_deref(),
        Some(started.run_id.as_str())
    );
    assert_eq!(
        agent_session_list_impl(&world.state, None, None, None)
            .unwrap()
            .sessions
            .len(),
        1
    );
    let run = world
        .state
        .store()
        .get_agent_run(&child_run_id)
        .unwrap()
        .unwrap();
    assert_eq!(
        (run.kind, run.status),
        (AgentRunKind::Subagent, AgentRunStatus::Completed)
    );

    let first_child = world
        .upstream
        .requests()
        .into_iter()
        .find(is_subagent)
        .unwrap();
    assert!(offers(&first_child, "read_file") && !offers(&first_child, "write_file"));
    assert!(!offers(&first_child, "task") && !offers(&first_child, "shell"));
}

#[tokio::test(flavor = "multi_thread")]
async fn subagent_limits_depth_and_permission_mode_ceiling() {
    assert_eq!(
        crate::settings::Settings::default().ai.agent.max_subagents,
        3
    );
    let world = world(|_, request| {
        if is_subagent(request) {
            return text_answer("child ok");
        }
        match tool_result_count(request) {
            0 => call_answer(
                "call_one",
                "task",
                &task_args("First job", "do one", "general"),
            ),
            1 => call_answer(
                "call_two",
                "task",
                &task_args("Second job", "do two", "general"),
            ),
            _ => text_answer("parent done"),
        }
    });
    settings_update_impl(
        &world.state,
        SettingsSection::Ai,
        &json!({"agent": {"max_subagents": 1}}),
    )
    .unwrap();
    let workspace = workspace_with("a", "b");
    let session = chat(&world, ToolApproval::AutoReadOnly, Some(workspace.path()));
    let (_, seen) = run_to_end(&world, &session, "delegate twice").await;
    let evs = events(&seen);
    assert_eq!(tool_result(&evs, "call_one").0, ToolCallStatus::Ok);
    assert_eq!(tool_result(&evs, "call_two").0, ToolCallStatus::Error);
    let children = agent_session_list_impl(
        &world.state,
        Some(AgentSessionFilter {
            parent_session_id: Some(session.id.clone()),
            ..AgentSessionFilter::default()
        }),
        None,
        None,
    )
    .unwrap();
    assert_eq!(children.sessions.len(), 1);

    let state = AppState::open_in_memory().unwrap();
    let sub_session = super::session(&state, "child");
    let target = AgentTarget::Named {
        name: "x".to_owned(),
    };
    let mut summary = initial_summary(&sub_session.id, AgentRunKind::Subagent, &target);
    summary.parent_run_id = Some("parent".to_owned());
    let run = ActiveRun::new(summary, CancelToken::new());
    state.agent.runs.register(&run).unwrap();
    let mut registry = ToolRegistry::new();
    subagent::register(&mut registry, &[]);
    let tool = registry.get("task").unwrap();
    let settings = state.settings();
    let (cancel, parts) = (CancelToken::new(), TestParts::default());
    let mut ctx = ToolContext::for_test(
        Some(&state as &(dyn std::any::Any + Send + Sync)),
        &settings,
        &cancel,
        &parts,
    );
    ctx.session_id = &sub_session.id;
    ctx.run_id = &run.run_id;
    let out = tool
        .call(&ctx, json!({"description": "nested job", "prompt": "go"}))
        .await;
    assert!(out.is_error && out.text_content() == "A sub-agent cannot start sub-agents.");

    let parent = super::session(&state, "parent");
    let linked = AgentSession {
        id: "child-linked".to_owned(),
        parent_session_id: Some(parent.id.clone()),
        permission_mode: ToolApproval::AutoAll,
        ..sub_session
    };
    state.store().save_agent_session(&linked).unwrap();
    assert_eq!(
        session_mode(&state.store(), "child-linked"),
        Some(ToolApproval::AutoReadOnly)
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn child_approvals_watcher_gate_and_stopping_child_vs_parent() {
    let world = world(delegates("general", |request| {
        if has_tool_result(request) {
            text_answer("wrote it")
        } else {
            call_answer(
                "child_w",
                "write_file",
                r#"{"path":"out.txt","content":"from the child"}"#,
            )
        }
    }));
    let workspace = workspace_with("a", "b");
    let session = chat(&world, ToolApproval::AutoReadOnly, Some(workspace.path()));
    let seen = watch(&world, &session);
    agent_run_start_impl(&world.state, input(&session, "delegate"))
        .await
        .unwrap();

    wait_until("the child's approval request", || {
        !world.state.agent.approvals.list().is_empty()
            && events(&seen).iter().any(|e| {
                matches!(
                    e,
                    AgentEvent::SubagentEvent { event, .. }
                        if matches!(event.as_ref(), AgentEvent::ToolApprovalRequired { .. })
                )
            })
    })
    .await;
    let (child_run, _) = started_child(&events(&seen)).unwrap();
    assert!(
        answer(
            &world.state,
            &child_run,
            "child_w",
            ApprovalDecision::Allow,
            ApprovalScope::Once,
            None,
            None,
            false,
        )
        .unwrap()
    );
    wait_terminal(&seen, 1).await;
    assert_eq!(
        fs::read_to_string(workspace.path().join("out.txt")).unwrap(),
        "from the child"
    );
}

struct ExpiredClock {
    state: AppState,
    run: Arc<ActiveRun>,
}

impl ApprovalGate for ExpiredClock {
    fn watchers(&self) -> usize {
        self.state.agent.runs.watchers_of_run(&self.run.run_id)
    }

    fn changes(&self) -> watch_channel::Receiver<u64> {
        self.state.agent.runs.watchers_changed()
    }

    fn sleep(&self, _: Duration) -> BoxFuture<'static, ()> {
        std::future::ready(()).boxed()
    }

    fn steered(&self) -> BoxFuture<'_, ()> {
        self.run.steered().boxed()
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn subagent_watcher_timeout_stop_isolation_and_shared_cost_cap() {
    let state = AppState::open_in_memory().unwrap();
    let (parent_chat, child_chat) = (super::session(&state, "p"), super::session(&state, "c"));
    let target = AgentTarget::Named {
        name: "x".to_owned(),
    };
    let parent = ActiveRun::new(
        initial_summary(&parent_chat.id, AgentRunKind::Main, &target),
        CancelToken::new(),
    );
    state.agent.runs.register(&parent).unwrap();
    let make_child = |id: &str| {
        let mut s = initial_summary(id, AgentRunKind::Subagent, &target);
        s.parent_run_id = Some(parent.run_id.clone());
        let r = ActiveRun::new(s, parent.cancel.child());
        state.agent.runs.register(&r).unwrap();
        r
    };
    let (child, sibling) = (
        make_child(&child_chat.id),
        make_child(&super::session(&state, "c2").id),
    );
    let gate = ExpiredClock {
        state: state.clone(),
        run: Arc::clone(&child),
    };
    let ticket = |call: &str| {
        state
            .agent
            .approvals
            .register(
                PendingApprovalSummary {
                    run_id: child.run_id.clone(),
                    session_id: child_chat.id.clone(),
                    call_id: call.to_owned(),
                    tool: "write_file".to_owned(),
                    reason: "test".to_owned(),
                    preview: None,
                    requested_ms: 0,
                    risk: "medium".to_owned(),
                    summary: String::new(),
                    full_text: None,
                    card: None,
                },
                RuleContext {
                    session_id: child_chat.id.clone(),
                    workspace_dir: None,
                    tool: "write_file".to_owned(),
                    pattern: None,
                    offered: vec![],
                },
            )
            .unwrap()
    };

    let (_, sink) = collector();
    let sub = agent_run_subscribe_impl(
        &state,
        &AgentSubscribeTarget::Session {
            session_id: parent_chat.id.clone(),
        },
        None,
        sink,
    )
    .unwrap();
    assert!(
        tokio::time::timeout(
            Duration::from_millis(200),
            ticket("watched").wait(&gate, &child.cancel, Duration::from_secs(60))
        )
        .await
        .is_err()
    );
    assert!(agent_run_unsubscribe_impl(&state, sub));
    assert_eq!(
        ticket("unwatched")
            .wait(&gate, &child.cancel, Duration::from_secs(60))
            .await,
        ApprovalOutcome::TimedOut
    );
    assert!(agent_run_stop_impl(&state, &child.run_id));
    assert!(child.cancel.is_cancelled() && !sibling.cancel.is_cancelled());
    assert!(agent_run_stop_impl(&state, &parent.run_id));
    assert!(sibling.cancel.is_cancelled());

    let world = world(delegates("general", |request| {
        if has_tool_result(request) {
            text_answer("never reached")
        } else {
            call_answer("child_read", "read_file", r#"{"path":"note.txt"}"#)
        }
    }));
    price_a_call_at_18_dollars(&world);
    settings_update_impl(
        &world.state,
        SettingsSection::Ai,
        &json!({"agent": {"cost_cap_usd": 30.0}}),
    )
    .unwrap();
    let workspace = workspace_with("note.txt", "hello file");
    let session = chat(&world, ToolApproval::AutoReadOnly, Some(workspace.path()));
    let seen = watch(&world, &session);
    let started = agent_run_start_impl(&world.state, input(&session, "delegate"))
        .await
        .unwrap();

    let paused_child = || {
        events(&seen).into_iter().find_map(|event| match event {
            AgentEvent::SubagentEvent { event, .. } => match *event {
                AgentEvent::CostLimitReached {
                    run_id, cost_micro, ..
                } => Some((run_id, cost_micro)),
                _ => None,
            },
            _ => None,
        })
    };
    wait_until("the child's cost pause", || paused_child().is_some()).await;
    let (child_run_id, cost) = paused_child().unwrap();
    assert_eq!(cost, 36_000_000);
    assert!(agent_run_continue_impl(&world.state, &child_run_id, false));

    wait_until("the parent's cost pause", || {
        events(&seen).iter().any(|e| matches!(e, AgentEvent::CostLimitReached { run_id, .. } if *run_id == started.run_id))
    })
    .await;
    assert!(agent_run_continue_impl(&world.state, &started.run_id, true));
    wait_terminal(&seen, 1).await;
    assert_eq!(
        tool_result(&events(&seen), "call_task").0,
        ToolCallStatus::Error
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn subagent_resume_preserves_child_history_and_rolls_up_parent_usage() {
    let world = world(|_, request| {
        if is_subagent(request) {
            let msgs = request.body["messages"].as_array().map_or(0, Vec::len);
            let saw_first = request.body["messages"]
                .as_array()
                .is_some_and(|arr| arr.iter().any(|m| m["content"] == "first child answer"));
            if msgs > 2 && saw_first {
                return text_answer("resumed saw prior turn");
            }
            return text_answer("first child answer");
        }
        match tool_result_count(request) {
            0 => call_answer(
                "call_first",
                "task",
                &task_args("First pass", "remember token ALPHA", "explore"),
            ),
            1 => {
                let agent_id = request.body["messages"]
                    .as_array()
                    .and_then(|arr| arr.iter().find(|m| m["role"] == "tool"))
                    .and_then(|m| m["content"].as_str())
                    .and_then(|text| {
                        text.lines().find_map(|line| {
                            line.strip_prefix("[agent_id: ")
                                .and_then(|rest| rest.strip_suffix(']'))
                        })
                    })
                    .expect("agent_id header in first task report")
                    .to_owned();
                call_answer(
                    "call_resume",
                    "task",
                    &json!({
                        "description": "Follow up",
                        "prompt": "what was the token?",
                        "kind": "explore",
                        "resume": agent_id
                    })
                    .to_string(),
                )
            }
            _ => text_answer("parent finished"),
        }
    });
    let workspace = workspace_with("note.txt", "hello");
    let session = chat(&world, ToolApproval::AutoReadOnly, Some(workspace.path()));
    let (_, seen) = run_to_end(&world, &session, "delegate and resume").await;

    let evs = events(&seen);
    assert_eq!(tool_result(&evs, "call_first").0, ToolCallStatus::Ok);
    let (resume_status, resume_text) = tool_result(&evs, "call_resume");
    assert_eq!(resume_status, ToolCallStatus::Ok);
    assert!(
        resume_text.contains("resumed saw prior turn"),
        "{resume_text}"
    );

    // Resuming reused the same child session rather than creating a second hidden chat.
    let children = agent_session_list_impl(
        &world.state,
        Some(AgentSessionFilter {
            parent_session_id: Some(session.id.clone()),
            ..AgentSessionFilter::default()
        }),
        None,
        None,
    )
    .unwrap();
    assert_eq!(children.sessions.len(), 1);

    // Parent usage totals include both parent (3 calls) and child (2 calls) = 5 * (11 in, 7 out).
    let stored = world
        .state
        .store()
        .get_agent_session(&session.id)
        .unwrap()
        .unwrap();
    assert_eq!(stored.total_usage.input_tokens, Some(55));
    assert_eq!(stored.total_usage.output_tokens, Some(35));
}

#[tokio::test(flavor = "multi_thread")]
async fn subagent_inherits_parent_block_rules_and_plan_mode_restricts_kind() {
    use crate::agent::permissions::{PermissionAction, PermissionRule};

    let world_blocked = world(delegates("explore", |request| {
        if has_tool_result(request) {
            let saw_secret = request.body["messages"].to_string().contains("top secret");
            text_answer(if saw_secret { "LEAKED" } else { "BLOCKED" })
        } else {
            call_answer("child_read", "read_file", r#"{"path":"secret.txt"}"#)
        }
    }));
    let workspace = workspace_with("secret.txt", "top secret");
    let session = chat(
        &world_blocked,
        ToolApproval::AutoReadOnly,
        Some(workspace.path()),
    );
    world_blocked
        .state
        .store()
        .save_permission_rule(&PermissionRule {
            id: String::new(),
            session_id: Some(session.id.clone()),
            workspace_dir: None,
            tool_pattern: "read_file".to_owned(),
            arg_pattern: None,
            action: PermissionAction::Block,
            created_at_ms: 0,
        })
        .unwrap();
    let (_, seen) = run_to_end(&world_blocked, &session, "try reading").await;
    let evs = events(&seen);
    let (status, text) = tool_result(&evs, "call_task");
    assert_eq!(status, ToolCallStatus::Ok);
    assert!(
        text.contains("BLOCKED") && !text.contains("LEAKED"),
        "{text}"
    );

    // Plan mode allows explore sub-agents and refuses general (write-capable) sub-agents.
    let world_plan = world(|_, request| {
        if is_subagent(request) {
            return text_answer("explore ok");
        }
        match tool_result_count(request) {
            0 => call_answer(
                "call_general",
                "task",
                &task_args("Try write subagent", "edit files", "general"),
            ),
            1 => call_answer(
                "call_explore",
                "task",
                &task_args("Inspect code", "read files", "explore"),
            ),
            _ => text_answer("planned"),
        }
    });
    let plan_session = chat(&world_plan, ToolApproval::AutoAll, Some(workspace.path()));
    super::super::agent_plan_set_impl(&world_plan.state, &plan_session.id, true)
        .await
        .unwrap();
    let (_, plan_seen) = run_to_end(&world_plan, &plan_session, "plan with subagent").await;
    let plan_evs = events(&plan_seen);
    let (gen_status, gen_text) = tool_result(&plan_evs, "call_general");
    assert_eq!(gen_status, ToolCallStatus::Error);
    assert!(gen_text.contains("Plan mode is read-only"), "{gen_text}");
    let (exp_status, exp_text) = tool_result(&plan_evs, "call_explore");
    assert_eq!(exp_status, ToolCallStatus::Ok);
    assert!(exp_text.contains("explore ok"), "{exp_text}");
}
