//! `ask_user` through whole runs: the question gate in the live snapshot, the
//! answer as information that approves nothing, the composer message as the answer, and no
//! question from a sub-agent.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use super::run::{chat, events, input, wait_terminal, wait_until, watch, workspace_with, world};
use super::upstream::{
    Recorded, Reply, call_answer, has_tool_result, is_subagent, offers, text_answer,
    tool_result_count,
};
use super::*;
use crate::agent::model::{AgentEvent, AgentQuestionAnswer, GateOutcomeDto, HumanGateBody};
use crate::agent::tools::ask_user::{ASK_USER, NOT_AN_APPROVAL};
use crate::agent_tools::subagent::ASK_REFUSAL;
use crate::llm::hub::text_of;
use crate::settings::ToolApproval;

use super::super::{
    agent_question_answer_impl, agent_run_start_impl, agent_run_steer_impl, agent_run_stop_impl,
};

fn question() -> String {
    serde_json::json!({
        "question": "Which layer handles 429?",
        "options": [{"label": "Router"}, {"label": "Gateway"}, {"label": "Both"}]
    })
    .to_string()
}

/// Asks first, then writes a file once answered, then answers.
fn asker(_: usize, request: &Recorded) -> Reply {
    match tool_result_count(request) {
        0 if offers(request, ASK_USER) => call_answer("tc_ask", ASK_USER, &question()),
        1 => call_answer(
            "tc_write",
            "write_file",
            r#"{"path":"out.txt","content":"gateway"}"#,
        ),
        _ => text_answer("ok"),
    }
}

fn result_of(seen: &super::run::Seen, call: &str) -> String {
    events(seen)
        .iter()
        .find_map(|event| match event {
            AgentEvent::ToolResult { call_id, parts, .. } if call_id == call => {
                Some(text_of(parts))
            }
            _ => None,
        })
        .unwrap_or_default()
}

async fn asked(seen: &super::run::Seen) {
    wait_until("the question gate", || {
        events(seen)
            .iter()
            .any(|e| matches!(e, AgentEvent::GateOpened { .. }))
    })
    .await;
}

#[tokio::test(flavor = "multi_thread")]
async fn an_answer_is_information_and_a_later_write_still_asks() {
    let world = world(asker);
    let workspace = workspace_with("a.txt", "a");
    let session = chat(&world, ToolApproval::AutoReadOnly, Some(workspace.path()));
    let seen = watch(&world, &session);
    let started = agent_run_start_impl(&world.state, input(&session, "fix 429"))
        .await
        .unwrap();
    asked(&seen).await;
    // A reload still shows the question.
    let detail =
        agent_session_get_impl(&world.state, &session.id, AgentPageRequest::default()).unwrap();
    let gates = detail.live.unwrap().gates;
    assert!(matches!(
        &gates[..],
        [gate] if matches!(&gate.body, HumanGateBody::Question { options, multi: false, .. } if options.len() == 3)
    ));
    // An option the question lacks is refused and the question keeps waiting.
    let wrong = AgentQuestionAnswer {
        choices: vec![4],
        ..AgentQuestionAnswer::default()
    };
    assert!(agent_question_answer_impl(&world.state, &started.run_id, "tc_ask", wrong).is_err());
    let pick = AgentQuestionAnswer {
        choices: vec![2],
        ..AgentQuestionAnswer::default()
    };
    assert!(agent_question_answer_impl(&world.state, &started.run_id, "tc_ask", pick).unwrap());

    // The answer approves nothing: the write that follows still waits for approval.
    wait_until("the write's approval", || {
        !world.state.agent.approvals.list().is_empty()
    })
    .await;
    let text = result_of(&seen, "tc_ask");
    assert!(text.starts_with(NOT_AN_APPROVAL), "{text}");
    assert!(text.contains("User chose: (2) Gateway"), "{text}");
    assert!(events(&seen).iter().any(|e| matches!(
        e,
        AgentEvent::GateResolved {
            outcome: GateOutcomeDto::Answered,
            ..
        }
    )));
    assert!(agent_run_stop_impl(&world.state, &started.run_id));
    wait_terminal(&seen, 1).await;
}

#[tokio::test(flavor = "multi_thread")]
async fn a_composer_message_instead_of_an_option_is_the_answer() {
    let world = world(|_, request| {
        if offers(request, ASK_USER) && !has_tool_result(request) {
            call_answer("tc_ask", ASK_USER, &question())
        } else {
            text_answer("ok")
        }
    });
    let session = chat(&world, ToolApproval::AutoReadOnly, None);
    let seen = watch(&world, &session);
    let started = agent_run_start_impl(&world.state, input(&session, "fix 429"))
        .await
        .unwrap();
    asked(&seen).await;
    assert!(
        agent_run_steer_impl(
            &world.state,
            &started.run_id,
            "only the docs",
            &[],
            Vec::new(),
            &[]
        )
        .unwrap()
    );
    wait_terminal(&seen, 1).await;
    assert!(result_of(&seen, "tc_ask").contains("their next message is the answer"));
    assert!(
        super::upstream::last_user_text(world.upstream.requests().last().unwrap())
            .contains("only the docs")
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn a_subagent_cannot_ask_and_is_told_to_report() {
    let world = world(|_, request| {
        if is_subagent(request) {
            if has_tool_result(request) {
                text_answer("report")
            } else {
                call_answer("child_ask", ASK_USER, &question())
            }
        } else if has_tool_result(request) {
            text_answer("parent done")
        } else {
            call_answer(
                "call_task",
                "task",
                r#"{"description":"Look around","prompt":"Read a.txt and report","kind":"general"}"#,
            )
        }
    });
    let workspace = workspace_with("a.txt", "a");
    let session = chat(&world, ToolApproval::AutoReadOnly, Some(workspace.path()));
    let seen = watch(&world, &session);
    agent_run_start_impl(&world.state, input(&session, "delegate"))
        .await
        .unwrap();
    wait_terminal(&seen, 1).await;
    let requests = world.upstream.requests();
    let child: Vec<&Recorded> = requests.iter().filter(|r| is_subagent(r)).collect();
    assert!(!child.is_empty());
    assert!(child.iter().all(|r| !offers(r, ASK_USER)));
    assert!(
        child
            .iter()
            .any(|r| has_tool_result(r) && r.body.to_string().contains(ASK_REFUSAL))
    );
    assert!(
        world
            .state
            .agent
            .gates
            .list(crate::agent::gate::GateKind::Question)
            .is_empty()
    );
}
