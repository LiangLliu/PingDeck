//! A chat without a SpanBay workspace is not offered the tools that need one, and the system
//! prompt says so (docs/architecture.md §11.1); a model that calls one anyway reads a `not_bound` result.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::BTreeSet;

use super::run::{chat, events, input, wait_terminal, watch, world};
use super::upstream::{call_answer, has_tool_result, offers, text_answer};
use super::*;
use crate::agent::model::AgentEvent;
use crate::agent::runner::ModelInfo;
use crate::agent_tools::spanbay::WORKSPACE_SCOPED;
use crate::llm::hub::text_of;
use crate::settings::ToolApproval;
use crate::workspace::Workspace;
use crate::workspace::storage::SpanBayBinding;

use super::super::agent_run_start_impl;
use super::super::resolve::resolve;
use super::super::tools::Role;

fn scoped() -> BTreeSet<String> {
    WORKSPACE_SCOPED
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

fn tool_names(resolved: &super::super::resolve::Resolved) -> BTreeSet<String> {
    resolved
        .specs
        .iter()
        .map(|spec| spec.name.clone())
        .collect()
}

fn bind(state: &AppState, session_id: &str, workspace: &str) {
    let store = state.store();
    store
        .save_workspace(&Workspace::new("ws_bound", workspace).unwrap())
        .unwrap();
    store
        .set_session_spanbay_binding(
            session_id,
            Some(&SpanBayBinding {
                workspace_id: "ws_bound".to_owned(),
                environment_id: None,
            }),
        )
        .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn an_unbound_chat_gets_neither_the_scoped_tools_nor_a_silent_prompt() {
    let state = state();
    let chat = session(&state, "unbound");
    let resolved = resolve(&state, &chat, &ModelInfo::default(), Role::Main, false).await;
    let names = tool_names(&resolved);
    assert!(names.is_disjoint(&scoped()), "{names:?}");
    // The app-level SpanBay tools do not need a binding.
    assert!(names.contains("run_probe") && names.contains("query_gateway_log"));
    assert!(resolved.system.contains("Workspace: none ("));
    assert!(!resolved.fingerprint.bound);
}

#[tokio::test(flavor = "multi_thread")]
async fn binding_a_workspace_brings_the_scoped_tools_and_its_name_into_the_next_resolve() {
    let state = state();
    let chat = session(&state, "bound");
    let before = resolve(&state, &chat, &ModelInfo::default(), Role::Main, false).await;
    bind(&state, &chat.id, "Billing platform");
    let after = resolve(&state, &chat, &ModelInfo::default(), Role::Main, false).await;
    assert!(scoped().is_subset(&tool_names(&after)));
    assert!(after.system.contains("\nWorkspace: Billing platform\n"));
    assert!(!after.system.contains("Workspace: none"));
    // The step boundary notices the change and rebuilds the tool table.
    assert_ne!(before.fingerprint, after.fingerprint);
    // A deleted workspace reads as unbound again.
    state
        .store()
        .delete_workspace(&crate::workspace::WorkspaceId::new("ws_bound").unwrap())
        .unwrap();
    let gone = resolve(&state, &chat, &ModelInfo::default(), Role::Main, false).await;
    assert!(tool_names(&gone).is_disjoint(&scoped()));
}

#[tokio::test(flavor = "multi_thread")]
async fn a_scoped_tool_called_in_an_unbound_chat_reads_not_bound_first() {
    let world = world(|_, request| {
        if has_tool_result(request) {
            text_answer("ok")
        } else {
            // The model calls a tool it was never offered (an older turn used it).
            assert!(!offers(request, "list_requests"));
            call_answer("tc_1", "list_requests", "{}")
        }
    });
    let session = chat(&world, ToolApproval::AutoAll, None);
    let seen = watch(&world, &session);
    agent_run_start_impl(&world.state, input(&session, "list my requests"))
        .await
        .unwrap();
    wait_terminal(&seen, 1).await;
    let result: String = events(&seen)
        .iter()
        .filter_map(|event| match event {
            AgentEvent::ToolResult { parts, .. } => Some(text_of(parts)),
            _ => None,
        })
        .collect();
    assert!(
        result.starts_with("{\"error\":{\"code\":\"not_bound\"}}\n"),
        "{result}"
    );
    assert!(result.contains("list_requests"));
}

#[test]
fn a_name_the_run_does_not_have_reads_tool_unavailable() {
    use crate::agent::tools::ToolRegistry;
    let registry = ToolRegistry::new();
    let none = super::super::tools::unknown_tool_text("no_such_tool", &registry, &[], false);
    assert!(
        none.starts_with("{\"error\":{\"code\":\"tool_unavailable\"}}\n"),
        "{none}"
    );
    let off = vec!["list_requests".to_owned()];
    // A bound chat that switched a scoped tool off is `tool_unavailable`, not `not_bound`.
    let switched = super::super::tools::unknown_tool_text("list_requests", &registry, &off, true);
    assert!(
        switched.starts_with("{\"error\":{\"code\":\"tool_unavailable\"}}\n"),
        "{switched}"
    );
    assert!(switched.contains("switched off"));
}
