//! The PingDeck-native agent tools: `send_request`, `get_trace`, `list_requests`, `get_request`,
//! `list_environments`, `run_probe`, `query_gateway_log` and `create_request` (PRD §5,
//! `docs/decisions.md` §12 D-84).
//!
//! Single responsibility: adapt the use cases PingDeck already has (the execution pipeline, the
//! history store, the Test bench, the request log, the workspace store) to the
//! [`AgentTool`](super::AgentTool) contract, with one redaction step in front of the model
//! ([`redact_for_model`]) and one argument validator. No domain logic lives here: sending,
//! probing and logging are the modules' own. The five read tools are class `PingDeckRead`
//! (`get_trace`, `list_requests`, `query_gateway_log`, `list_environments`: no bodies;
//! `get_request`: one saved request's body); the three that send or change data are
//! `PingDeckWrite` (`send_request`, `run_probe`, `create_request`).
//!
//! Scope: every tool but `run_probe` and `query_gateway_log` acts on the
//! PingDeck workspace the chat is bound to (`agent_sessions.pingdeck_workspace_id`, set with
//! `WorkspaceStore::set_session_pingdeck_binding`), never on the one open when the call runs;
//! `run_probe` and `query_gateway_log` are app-wide. Every result carries a `view` hint the UI
//! turns into a link.
//!
//! [`WORKSPACE_SCOPED`] names the six tools that need that binding. A chat without one is not
//! offered them (the session's tool table drops them), and the UI reads the same list through
//! `AgentToolInfo::needs_workspace`, so no second copy of the names exists.

pub(crate) mod common;
mod create;
mod draft;
mod environments;
mod gateway;
mod get_request;
mod list;
mod probe;
mod redact;
pub(crate) mod schema;
mod send;
#[cfg(test)]
mod tests;
mod trace;

use std::sync::Arc;

use crate::agent::tools::ToolRegistry;

pub use common::{all_secrets, workspace_secrets};
pub use create::CreateRequestTool;
pub use environments::ListEnvironmentsTool;
pub use gateway::QueryGatewayLogTool;
pub use get_request::GetRequestTool;
pub use list::ListRequestsTool;
pub use probe::RunProbeTool;
pub use redact::{NamedSecret, is_credential_header, redact_for_model};
pub use send::SendRequestTool;
pub use trace::GetTraceTool;

/// The tools that act on the chat's bound PingDeck workspace. `run_probe` and `query_gateway_log`
/// are app-level and work without a binding, so they are not listed.
pub const WORKSPACE_SCOPED: [&str; 6] = [
    "send_request",
    "get_trace",
    "list_requests",
    "get_request",
    "list_environments",
    "create_request",
];

/// Registers the eight PingDeck tools (source `Builtin`). The caller drops the ones the session
/// disabled, as for every other built-in tool.
pub fn register(registry: &mut ToolRegistry) {
    registry.register(Arc::new(SendRequestTool::new()));
    registry.register(Arc::new(GetTraceTool::new()));
    registry.register(Arc::new(ListRequestsTool::new()));
    registry.register(Arc::new(GetRequestTool::new()));
    registry.register(Arc::new(ListEnvironmentsTool::new()));
    registry.register(Arc::new(RunProbeTool::new()));
    registry.register(Arc::new(QueryGatewayLogTool::new()));
    registry.register(Arc::new(CreateRequestTool::new()));
}
