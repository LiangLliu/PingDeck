//! Plan mode through `HumanGate`: the stored switch, `submit_plan` only in
//! Plan, the plan gate in the live snapshot, approval with the user's text and mode, and the
//! user's additions instead of an approval.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::run::{chat, events, input, wait_terminal, wait_until, watch, world};
use super::upstream::{Recorded, Reply, call_answer, has_tool_result, offers, text_answer};
use super::*;
use crate::agent::model::{AgentEvent, AgentRunStatus, GateOutcomeDto, HumanGateBody};
use crate::agent::tools::submit_plan::{PLAN_APPROVED_RESULT, PLAN_REVISE_RESULT, SUBMIT_PLAN};
use crate::llm::hub::text_of;
use crate::settings::ToolApproval;

use super::super::{
    agent_plan_approve_impl, agent_plan_get_impl, agent_plan_set_impl, agent_run_start_impl,
    agent_run_steer_impl,
};

const PLAN: &str = "1. Read router.rs\n2. Move 429 out of on_failure\n3. Add two tests";

/// Plans when `submit_plan` is offered, answers otherwise.
fn planner(_: usize, request: &Recorded) -> Reply {
    if offers(request, SUBMIT_PLAN) && !has_tool_result(request) {
        call_answer(
            "tc_plan",
            SUBMIT_PLAN,
            &serde_json::json!({"plan": PLAN, "title": "Retry 429"}).to_string(),
        )
    } else {
        text_answer("ok")
    }
}

fn tool_result_text(seen: &super::run::Seen) -> String {
    events(seen)
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolResult { parts, .. } => Some(text_of(parts)),
            _ => None,
        })
        .collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn an_approved_plan_ends_its_run_and_starts_the_next_in_the_chosen_mode() {
    let world = world(planner);
    let session = chat(&world, ToolApproval::AutoAll, None);
    agent_plan_set_impl(&world.state, &session.id, true)
        .await
        .unwrap();
    // The switch is stored: a relaunch reads it back.
    assert_eq!(
        world.state.store().plan_mode_sessions().unwrap(),
        std::slice::from_ref(&session.id)
    );
    let seen = watch(&world, &session);
    let first = agent_run_start_impl(&world.state, input(&session, "plan the retry"))
        .await
        .unwrap();
    wait_until("the plan gate", || {
        events(&seen)
            .iter()
            .any(|e| matches!(e, AgentEvent::GateOpened { .. }))
    })
    .await;
    // A reload still shows the plan, taken from the call's own argument.
    let detail =
        agent_session_get_impl(&world.state, &session.id, AgentPageRequest::default()).unwrap();
    assert_eq!(detail.plan_mode, Some(true));
    let gates = detail.live.unwrap().gates;
    assert!(matches!(
        &gates[..],
        [gate] if matches!(&gate.body, HumanGateBody::Plan { markdown, steps: 3, .. } if markdown == PLAN)
    ));
    let paused = detail
        .runs
        .iter()
        .find(|r| r.run_id == first.run_id)
        .unwrap();
    assert_eq!(paused.status, AgentRunStatus::WaitingApproval);

    let text = "批准计划并执行：\n1. Read router.rs";
    let next = agent_plan_approve_impl(
        &world.state,
        &session.id,
        Some(text.to_owned()),
        Some(ToolApproval::Ask),
    )
    .await
    .unwrap();
    wait_terminal(&seen, 2).await;
    let all = events(&seen);
    assert!(all.iter().any(|e| matches!(
        e,
        AgentEvent::GateResolved {
            outcome: GateOutcomeDto::Answered,
            ..
        }
    )));
    assert!(all.iter().any(|e| matches!(
        e,
        AgentEvent::Done { run, .. } if run.run_id == first.run_id && run.status == AgentRunStatus::Completed
    )));
    let approved = tool_result_text(&seen);
    assert!(approved.contains(PLAN_APPROVED_RESULT));
    assert!(
        approved.starts_with("{\"plan\":{\"outcome\":\"approved\",\"mode\":\"ask\"}}\n"),
        "{approved}"
    );
    assert!(!agent_plan_get_impl(&world.state, &session.id));
    assert!(world.state.store().plan_mode_sessions().unwrap().is_empty());
    let session_now = world
        .state
        .store()
        .get_agent_session(&session.id)
        .unwrap()
        .unwrap();
    assert_eq!(session_now.permission_mode, ToolApproval::Ask);
    // The new run starts from the user's localised text, not the English fallback, without
    // `submit_plan` in its tools.
    let requests = world.upstream.requests();
    let last = requests.last().unwrap();
    assert!(!offers(last, SUBMIT_PLAN));
    assert!(super::upstream::last_user_text(last).contains("批准计划并执行"));
    assert_ne!(next.run_id, first.run_id);
}

#[tokio::test(flavor = "multi_thread")]
async fn writing_instead_of_approving_keeps_the_run_planning() {
    let world = world(planner);
    let session = chat(&world, ToolApproval::AutoReadOnly, None);
    agent_plan_set_impl(&world.state, &session.id, true)
        .await
        .unwrap();
    let seen = watch(&world, &session);
    let started = agent_run_start_impl(&world.state, input(&session, "plan it"))
        .await
        .unwrap();
    wait_until("the plan gate", || {
        events(&seen)
            .iter()
            .any(|e| matches!(e, AgentEvent::GateOpened { .. }))
    })
    .await;
    assert!(
        agent_run_steer_impl(
            &world.state,
            &started.run_id,
            "also cover 503",
            &[],
            Vec::new(),
            &[]
        )
        .unwrap()
    );
    wait_terminal(&seen, 1).await;
    let revised = tool_result_text(&seen);
    assert!(revised.contains(PLAN_REVISE_RESULT));
    assert!(
        revised.starts_with("{\"plan\":{\"outcome\":\"revise\"}}\n"),
        "{revised}"
    );
    // The run went on to answer with the user's message in its history; Plan stays on.
    assert!(
        super::upstream::last_user_text(world.upstream.requests().last().unwrap())
            .contains("also cover 503")
    );
    assert!(agent_plan_get_impl(&world.state, &session.id));
}

#[test]
fn a_plan_nobody_answered_reads_timed_out_on_its_first_line_and_ends_the_run() {
    use super::super::host::plan_answer;
    use crate::agent::gate::GateOutcome;
    let (text, resolved, ends) = plan_answer(GateOutcome::TimedOut);
    assert!(
        text.starts_with("{\"plan\":{\"outcome\":\"timed_out\"}}\n"),
        "{text}"
    );
    assert!(text.contains(crate::agent::tools::submit_plan::PLAN_TIMED_OUT_RESULT));
    assert_eq!(resolved, GateOutcomeDto::TimedOut);
    assert!(ends);
    // A cancellation is an error text, not a plan outcome.
    let (cancelled, _, _) = plan_answer(GateOutcome::Cancelled);
    assert!(!cancelled.starts_with('{'));
}

#[tokio::test(flavor = "multi_thread")]
async fn structured_task_graph_plan_persists_on_approval_and_supports_node_recovery() {
    use super::super::{agent_run_topology_impl, agent_task_node_recover_impl};
    use crate::agent::model::{AgentTodoStatus, NodeRecoveryAction};

    fn graph_planner(_: usize, request: &Recorded) -> Reply {
        if offers(request, SUBMIT_PLAN) && !has_tool_result(request) {
            call_answer(
                "tc_plan_dag",
                SUBMIT_PLAN,
                &serde_json::json!({
                    "title": "Auth Task Graph",
                    "plan": "1. Audit router\n2. Audit store\n3. Patch auth\n4. Run tests",
                    "tasks": [
                        {"id": "n1", "content": "Audit router", "kind": "explore", "role": "fast", "group": "Audit"},
                        {"id": "n2", "content": "Audit store", "kind": "explore", "role": "fast", "group": "Audit"},
                        {"id": "n3", "content": "Patch auth", "depends_on": ["n1", "n2"], "kind": "implement"},
                        {"id": "n4", "content": "Run tests", "depends_on": ["n3"], "kind": "verify"}
                    ]
                })
                .to_string(),
            )
        } else {
            text_answer("done")
        }
    }

    let world = world(graph_planner);
    let session = chat(&world, ToolApproval::AutoReadOnly, None);
    agent_plan_set_impl(&world.state, &session.id, true)
        .await
        .unwrap();
    let seen = watch(&world, &session);
    agent_run_start_impl(&world.state, input(&session, "plan the task graph"))
        .await
        .unwrap();
    wait_until("structured plan gate", || {
        events(&seen)
            .iter()
            .any(|e| matches!(e, AgentEvent::GateOpened { .. }))
    })
    .await;

    let detail =
        agent_session_get_impl(&world.state, &session.id, AgentPageRequest::default()).unwrap();
    let gates = detail.live.unwrap().gates;
    assert!(matches!(
        &gates[..],
        [gate] if matches!(&gate.body, HumanGateBody::Plan { steps: 4, tasks, .. } if tasks.len() == 4)
    ));

    let exec_run = agent_plan_approve_impl(
        &world.state,
        &session.id,
        Some("批准并执行".to_owned()),
        Some(ToolApproval::AutoReadOnly),
    )
    .await
    .unwrap();
    wait_terminal(&seen, 2).await;

    // Approved structured tasks are persisted into session.todos and exposed via topology.
    let topo = agent_run_topology_impl(&world.state, &exec_run.run_id).unwrap();
    assert_eq!(topo.task_graph.len(), 4);
    assert_eq!(topo.task_graph[2].depends_on, vec!["n1", "n2"]);
    let last_prompt = super::upstream::last_user_text(world.upstream.requests().last().unwrap());
    assert!(last_prompt.contains("[Approved Task Graph]"));

    // Simulate n1/n2 completed, n3 failed, n4 blocked -> Retry n3 resets n3+n4 while preserving n1+n2.
    let mut todos = topo.task_graph;
    todos[0].status = AgentTodoStatus::Completed;
    todos[0].summary = Some("router ok".to_owned());
    todos[1].status = AgentTodoStatus::Completed;
    todos[2].status = AgentTodoStatus::Failed;
    todos[2].error = Some("borrow checker".to_owned());
    todos[3].status = AgentTodoStatus::Blocked;
    world
        .state
        .store()
        .save_session_todos(&session.id, &todos)
        .unwrap();

    agent_task_node_recover_impl(
        &world.state,
        &session.id,
        "n3",
        NodeRecoveryAction::Retry,
        Some("clone the Arc first".to_owned()),
    )
    .await
    .unwrap();
    wait_terminal(&seen, 3).await;

    let after_retry = world
        .state
        .store()
        .get_agent_session(&session.id)
        .unwrap()
        .unwrap();
    assert_eq!(after_retry.todos[0].status, AgentTodoStatus::Completed);
    assert_eq!(after_retry.todos[1].status, AgentTodoStatus::Completed);
    assert_eq!(after_retry.todos[2].status, AgentTodoStatus::Pending);
    assert_eq!(after_retry.todos[2].attempt, 1);
    assert!(after_retry.todos[2].error.is_none());
    assert_eq!(after_retry.todos[3].status, AgentTodoStatus::Pending);
    let retry_prompt = super::upstream::last_user_text(world.upstream.requests().last().unwrap());
    assert!(retry_prompt.contains("clone the Arc first"));
    assert!(retry_prompt.contains("router ok"));

    // Skip n3 marks it Cancelled and launches continuation.
    agent_task_node_recover_impl(
        &world.state,
        &session.id,
        "n3",
        NodeRecoveryAction::Skip,
        None,
    )
    .await
    .unwrap();
    wait_terminal(&seen, 4).await;
    let after_skip = world
        .state
        .store()
        .get_agent_session(&session.id)
        .unwrap()
        .unwrap();
    assert_eq!(after_skip.todos[2].status, AgentTodoStatus::Cancelled);
}
