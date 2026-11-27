//! The `list_environments` tool: the environments of the chat's workspace without a single value
//!
//! Single responsibility: name each environment, whether it is guarded, its variables' names and
//! whether each is secret, and the hosts it declares for secrets (the same computation
//! `send_request` checks against). No variable value is returned, secret or not.

use futures_util::future::BoxFuture;
use serde_json::{Value, json};

use super::common::{bound_scope, error_output, json_output, tool_spec};
use super::schema::parse_args;
use crate::agent::tools::{
    AgentTool, CallInspection, ToolClass, ToolContext, ToolOutput, ToolSpec,
};
use crate::app::secret_hosts::declared_hosts;
use crate::app::{AppState, command_error};
use crate::error::AppError;

/// Name of the tool.
pub const LIST_ENVIRONMENTS: &str = "list_environments";

/// Cap of one result.
const RESULT_CAP: usize = 16 * 1024;

/// `list_environments {}`.
pub struct ListEnvironmentsTool {
    spec: ToolSpec,
}

impl ListEnvironmentsTool {
    /// The tool with its static spec: class `SpanBayRead`, read-only, parallel-safe.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: tool_spec(
                LIST_ENVIRONMENTS,
                "List the environments of this chat's SpanBay workspace: name, whether sends \
                 into it are guarded, variable names and which are secret, and the hosts it \
                 declares (a secret is only sent to a declared host). No values. The chat's \
                 environment is marked bound.",
                ToolClass::SpanBayRead,
                false,
                json!({"type": "object", "additionalProperties": false, "properties": {}}),
            ),
        }
    }
}

impl Default for ListEnvironmentsTool {
    fn default() -> Self {
        Self::new()
    }
}

fn list(state: &AppState, session_id: &str) -> Result<ToolOutput, AppError> {
    let scope = bound_scope(state, session_id)?;
    let workspace_id = scope.workspace.id.clone();
    let (environments, untrusted) = {
        let store = state.store();
        (
            store
                .list_environments(workspace_id.as_str())
                .map_err(command_error)?,
            store
                .agent_touched_request_ids(workspace_id.as_str())
                .map_err(command_error)?,
        )
    };
    let mut rows = Vec::with_capacity(environments.len());
    for environment in &environments {
        let hosts = declared_hosts(state, &workspace_id, Some(environment), &untrusted)?;
        let variables: Vec<Value> = environment
            .variables
            .iter()
            .filter(|variable| variable.enabled)
            .map(|variable| json!({"name": variable.key, "secret": variable.is_secret}))
            .collect();
        rows.push(json!({
            "name": environment.name,
            "bound": scope.environment_id.as_deref() == Some(environment.id.as_str()),
            "guarded": environment.confirm_before_send,
            "variables": variables,
            "declared_hosts": hosts.iter().map(|host| host.host.as_str()).collect::<Vec<_>>(),
        }));
    }
    if rows.is_empty() {
        return Ok(ToolOutput::text(format!(
            "Workspace \"{}\" has no environments.",
            scope.workspace.name
        )));
    }
    let value = json!({"workspace": scope.tag(), "environments": rows});
    // Names only: no value is in the text, so no secret list is needed to mask one.
    Ok(json_output(&value, &[], RESULT_CAP))
}

impl AgentTool for ListEnvironmentsTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        let _: serde_json::Map<String, Value> = parse_args(&self.spec.input_schema, arguments)?;
        bound_scope(crate::agent_tools::app_state(ctx), ctx.session_id)?;
        Ok(CallInspection {
            read_only: true,
            ..CallInspection::default()
        })
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            if let Err(error) =
                parse_args::<serde_json::Map<String, Value>>(&self.spec.input_schema, &arguments)
            {
                return error_output(&error, &[]);
            }
            list(crate::agent_tools::app_state(ctx), ctx.session_id)
                .unwrap_or_else(|error| error_output(&error, &[]))
        })
    }
}
