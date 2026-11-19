//! The set of tools that need a bound workspace is one list ([`WORKSPACE_SCOPED`]); this pins it
//! to what the tools do, so a tool added later cannot be forgotten there.

use std::collections::BTreeSet;

use serde_json::{Value, json};

use super::super::{WORKSPACE_SCOPED, register};
use super::support::{World, session};
use crate::agent::tools::ToolRegistry;

/// Arguments that pass a tool's own validation, so a failure can only be about the binding.
fn minimal_args(name: &str) -> Value {
    match name {
        "send_request" | "get_request" => json!({"request_id": "req_inv"}),
        "get_trace" => json!({"latest": true}),
        "create_request" => json!({"name": "New", "url": "https://example.com"}),
        "run_probe" => json!({"provider": "p", "models": ["m"]}),
        "list_requests" | "list_environments" | "query_gateway_log" => json!({}),
        other => panic!("{other} is a new PingDeck tool: say here whether it needs a workspace"),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn exactly_the_scoped_tools_refuse_an_unbound_chat_as_not_bound() {
    let world = World::new().await;
    world
        .state
        .store()
        .save_agent_session(&session("sess_unbound"))
        .unwrap();
    let ctx = world.ctx_in("sess_unbound", "run_1");
    let mut registry = ToolRegistry::new();
    register(&mut registry);
    let mut unbound_failures = BTreeSet::new();
    for spec in registry.specs() {
        let tool = registry.get(&spec.name).unwrap();
        let refused = tool
            .inspect(&ctx, &minimal_args(&spec.name))
            .err()
            .is_some_and(|error| {
                error
                    .message()
                    .starts_with("{\"error\":{\"code\":\"not_bound\"}}\n")
            });
        if refused {
            unbound_failures.insert(spec.name.clone());
        }
    }
    let scoped: BTreeSet<String> = WORKSPACE_SCOPED
        .iter()
        .map(|name| (*name).to_owned())
        .collect();
    assert_eq!(unbound_failures, scoped);
}

#[tokio::test(flavor = "multi_thread")]
async fn a_deleted_workspace_reads_workspace_deleted_not_not_bound() {
    let world = World::new().await;
    world
        .state
        .store()
        .delete_workspace(&crate::workspace::WorkspaceId::new(super::support::WS_A).unwrap())
        .unwrap();
    let registry = {
        let mut registry = ToolRegistry::new();
        register(&mut registry);
        registry
    };
    let tool = registry.get("list_requests").unwrap();
    let error = tool.inspect(&world.ctx(), &json!({})).unwrap_err();
    assert!(
        error
            .message()
            .starts_with("{\"error\":{\"code\":\"workspace_deleted\"}}\n"),
        "{error:?}"
    );
}
