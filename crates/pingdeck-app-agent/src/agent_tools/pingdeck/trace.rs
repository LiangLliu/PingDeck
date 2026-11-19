//! The `get_trace` tool: explain how a request was sent.
//!
//! Single responsibility: read one history entry of the chat's PingDeck workspace and return its
//! timing, redirects, auth summary, variable names and call chain as compact JSON of at most 8 KB.
//! It never returns a request or response body, and no header; an entry sent with a
//! guarded (Production) environment always asks first.

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::common::{Scope, bound_scope, error_output, json_output, tool_spec, workspace_secrets};
use super::redact::NamedSecret;
use super::schema::parse_args;
use crate::agent::tools::{
    AgentTool, CallInspection, ToolClass, ToolContext, ToolOutput, ToolSpec,
};
use crate::app::request_model_service::{AuthSource, EffectiveAuthInput, request_effective_auth};
use crate::app::{AppState, command_error};
use crate::error::AppError;
use crate::workspace::models::ExecutionHistoryEntry;
use crate::workspace::{HistoryEntryId, HistoryFilter};

/// Name of the tool.
pub const GET_TRACE: &str = "get_trace";

/// Cap of one result.
const RESULT_CAP: usize = 8 * 1024;
/// Longest variable value shown, in characters.
const VALUE_CHARS: usize = 200;

/// `get_trace {history_id | latest, section?}`.
pub struct GetTraceTool {
    spec: ToolSpec,
}

impl GetTraceTool {
    /// The tool with its static spec: class `PingDeckRead`, read-only, parallel-safe.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: tool_spec(
                GET_TRACE,
                "Explain how a request was sent: timing, redirects, auth summary, variable names \
                 and the call chain of a history entry. Never returns request or response bodies. \
                 Give history_id (from send_request or list_requests) or latest: true.",
                ToolClass::PingDeckRead,
                false,
                json!({
                    "type": "object", "additionalProperties": false,
                    "properties": {
                        "history_id": {"type": "string", "minLength": 1, "description": "Id of a history entry"},
                        "latest": {"type": "boolean", "description": "The newest history entry"},
                        "section": {"type": "string", "enum": ["summary", "timing", "redirects", "auth", "variables", "chain"], "description": "Part of the trace to return"}
                    }
                }),
            ),
        }
    }
}

impl Default for GetTraceTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct TraceArgs {
    history_id: Option<String>,
    latest: Option<bool>,
    section: Option<String>,
}

/// The entry, the scope it was read in and the secrets to mask.
struct Loaded {
    scope: Scope,
    entry: ExecutionHistoryEntry,
    guarded: Option<String>,
    secrets: Vec<NamedSecret>,
    /// Where the request's auth is defined now (AI-apiwb-7); `None` for a draft or a deleted
    /// request.
    auth_source: Option<Value>,
}

fn load(state: &AppState, session_id: &str, args: &TraceArgs) -> Result<Loaded, AppError> {
    let latest = args.latest == Some(true);
    if latest == args.history_id.is_some() {
        return Err(AppError::validation(
            "Give exactly one of history_id or latest: true.",
        ));
    }
    let scope = bound_scope(state, session_id)?;
    let workspace_id = scope.workspace.id.as_str().to_owned();
    let store = state.store();
    let entry = if let Some(id) = &args.history_id {
        let id = HistoryEntryId::new(id.trim()).map_err(command_error)?;
        store
            .get_execution_history(&workspace_id, &id)
            .map_err(command_error)?
    } else {
        store
            .query_execution_history(&workspace_id, &HistoryFilter::default(), 1, None)
            .map_err(command_error)?
            .entries
            .into_iter()
            .next()
    }
    .ok_or_else(|| {
        AppError::not_found(format!(
            "No such history entry in workspace \"{}\".",
            scope.workspace.name
        ))
    })?;
    let guarded = match &entry.environment_id {
        Some(id) => store
            .get_environment(&scope.workspace.id, id)
            .map_err(command_error)?
            .filter(|environment| environment.confirm_before_send)
            .map(|environment| {
                format!(
                    "Asks because: this entry was sent with the guarded (Production) environment \
                     \"{}\"",
                    environment.name
                )
            }),
        None => None,
    };
    let secrets = workspace_secrets(&store, &workspace_id)?;
    // The executor sees the request after inheritance and always reports `request`.
    drop(store);
    let auth_source = entry.request_id.as_ref().and_then(|id| {
        request_effective_auth(
            state,
            EffectiveAuthInput {
                workspace_id: workspace_id.clone(),
                request_id: Some(id.as_str().to_owned()),
                collection_id: None,
                folder_id: None,
                auth: None,
            },
        )
        .ok()
        .map(|auth| match auth.source {
            AuthSource::Request => json!("request"),
            AuthSource::Folder { name, .. } => json!({"folder": name}),
            AuthSource::Collection { name, .. } => json!({"collection": name}),
            AuthSource::None => json!("none"),
        })
    });
    Ok(Loaded {
        scope,
        entry,
        guarded,
        secrets,
        auth_source,
    })
}

fn truncated(text: &str) -> String {
    text.chars().take(VALUE_CHARS).collect()
}

fn timing_json(entry: &ExecutionHistoryEntry) -> Value {
    match entry.timing {
        Some(timing) => json!({
            "to_first_byte_ms": u64::try_from(timing.request_ms).unwrap_or(u64::MAX),
            "body_ms": u64::try_from(timing.response_ms).unwrap_or(u64::MAX),
            "total_ms": u64::try_from(timing.total_ms).unwrap_or(u64::MAX),
        }),
        None => {
            json!({"total_ms": u64::try_from(entry.elapsed_ms).unwrap_or(u64::MAX), "phases": "not recorded"})
        }
    }
}

fn redirects_json(entry: &ExecutionHistoryEntry) -> Value {
    let hops: Vec<Value> = entry.call_chain.as_ref().map_or_else(Vec::new, |chain| {
        chain
            .transport
            .redirect_hops
            .iter()
            .map(|hop| {
                json!({
                    "hop": hop.hop, "method": hop.method, "url": hop.url,
                    "status": hop.status_code, "location": hop.location,
                    "duration_ms": u64::try_from(hop.duration_ms).unwrap_or(u64::MAX),
                })
            })
            .collect()
    });
    json!(hops)
}

fn auth_json(entry: &ExecutionHistoryEntry, source: Option<&Value>) -> Value {
    entry.call_chain.as_ref().map_or_else(
        || json!({"recorded": false}),
        |chain| {
            json!({
                "type": chain.auth.auth_type,
                "source": source.cloned().unwrap_or_else(|| json!(chain.auth.source)),
                "applied_to": chain.auth.applied_to,
                "details": chain.auth.details.iter().map(|row| json!({"name": row.key, "value": truncated(&row.value)})).collect::<Vec<_>>(),
            })
        },
    )
}

/// Variable names with their source; a value only when the variable is not secret.
fn variables_json(entry: &ExecutionHistoryEntry) -> Value {
    entry.call_chain.as_ref().map_or_else(
        || json!({"recorded": false}),
        |chain| {
            json!(
                chain
                    .variables
                    .iter()
                    .map(|variable| {
                        let mut row = json!({
                            "name": variable.name,
                            "source": format!("{:?}", variable.source),
                            "secret": variable.secret,
                        });
                        if !variable.secret {
                            row["value"] = json!(truncated(&variable.resolved_value));
                        }
                        row
                    })
                    .collect::<Vec<_>>()
            )
        },
    )
}

fn chain_json(entry: &ExecutionHistoryEntry, auth_source: Option<&Value>) -> Value {
    entry.call_chain.as_ref().map_or_else(
        || json!({"recorded": false}),
        |chain| {
            json!({
                "transport": {
                    "protocol": chain.transport.protocol,
                    "remote_addr": chain.transport.remote_addr,
                    "proxy": chain.transport.proxy_url,
                    "proxy_bypassed": chain.transport.proxy_bypassed,
                    "custom_ca_count": chain.transport.custom_ca_count,
                    "client_cert_host": chain.transport.client_cert_host,
                    "tls_disabled_versions": chain.transport.tls_disabled_versions,
                },
                "auth": auth_json(entry, auth_source),
                "variables": variables_json(entry),
                "redirects": redirects_json(entry),
            })
        },
    )
}

fn summary_json(entry: &ExecutionHistoryEntry) -> Value {
    let chain = entry.call_chain.as_ref();
    json!({
        "history_id": entry.id.as_str(),
        "executed_at": entry.executed_at,
        "source": entry.source.as_str(),
        "method": format!("{:?}", entry.method).to_ascii_uppercase(),
        "request_name": entry.request_name,
        "url_template": entry.request_url,
        "final_url": entry.final_url,
        "status": entry.status_code,
        "status_text": entry.status_text,
        "result": format!("{:?}", entry.result_kind).to_ascii_lowercase(),
        "error_kind": entry.error_kind,
        "error_message": entry.error_message,
        "attempts": entry.attempt_count,
        "protocol": entry.protocol,
        "environment": entry.environment_name,
        "timing": timing_json(entry),
        "redirects": chain.map_or(0, |chain| chain.transport.redirect_hops.len()),
        "variables": chain.map_or(0, |chain| chain.variables.len()),
        "auth_type": chain.map(|chain| chain.auth.auth_type.clone()),
    })
}

fn render(loaded: &Loaded, section: &str) -> Value {
    let entry = &loaded.entry;
    let mut body = match section {
        "timing" => json!({"timing": timing_json(entry)}),
        "redirects" => json!({"redirects": redirects_json(entry)}),
        "auth" => json!({"auth": auth_json(entry, loaded.auth_source.as_ref())}),
        "variables" => json!({"variables": variables_json(entry)}),
        "chain" => json!({"chain": chain_json(entry, loaded.auth_source.as_ref())}),
        _ => json!({"summary": summary_json(entry)}),
    };
    body["section"] = json!(section);
    body["workspace"] = loaded.scope.tag();
    body["view"] = json!({
        "kind": "trace",
        "history_id": entry.id.as_str(),
        "workspace_id": loaded.scope.workspace.id.as_str(),
    });
    body
}

impl AgentTool for GetTraceTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        let args: TraceArgs = parse_args(&self.spec.input_schema, arguments)?;
        let loaded = load(crate::agent_tools::app_state(ctx), ctx.session_id, &args)?;
        Ok(CallInspection {
            read_only: true,
            ask_code: loaded.guarded.is_some().then_some("production"),
            always_ask: loaded.guarded,
            arg_subject: Some(loaded.entry.id.as_str().to_owned()),
            ..CallInspection::default()
        })
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let args: TraceArgs = match parse_args(&self.spec.input_schema, &arguments) {
                Ok(args) => args,
                Err(error) => return error_output(&error, &[]),
            };
            match load(crate::agent_tools::app_state(ctx), ctx.session_id, &args) {
                Ok(loaded) => {
                    let section = args.section.as_deref().unwrap_or("summary");
                    json_output(&render(&loaded, section), &loaded.secrets, RESULT_CAP)
                }
                Err(error) => error_output(&error, &[]),
            }
        })
    }
}
