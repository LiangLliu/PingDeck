//! The `list_requests` tool: find saved requests or history entries.
//!
//! Single responsibility: list the chat's SpanBay workspace, never another one. Saved
//! rows carry the request id, its place, name, method and the URL template with `{{variables}}`
//! unresolved; history rows add time, status and duration and page with a cursor, filtered by
//! status, time and origin in the store. No body, header or secret value is returned;
//! the text is capped at 16 KB and 50 rows.

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::common::{Tree, bound_scope, error_output, json_output, tool_spec, workspace_secrets};
use super::draft::{method_name, method_of};
use super::gateway::since_ms;
use super::redact::redact_for_model;
use super::schema::parse_args;
use crate::agent::tools::{
    AgentTool, CallInspection, ToolClass, ToolContext, ToolOutput, ToolSpec,
};
use crate::app::{AppState, command_error, current_time_ms};
use crate::error::AppError;
use crate::workspace::{HistoryCursor, HistoryFilter, HistorySource};

/// Name of the tool.
pub const LIST_REQUESTS: &str = "list_requests";

/// Most rows one call returns.
const MAX_ROWS: usize = 50;
/// Rows when the model names no limit.
const DEFAULT_ROWS: usize = 20;
/// Cap of one result.
const RESULT_CAP: usize = 16 * 1024;

/// `list_requests {query?, collection?, folder?, method?, source?, limit?}`; the history view
/// also takes `status`, `since`, `origin` and `cursor`.
pub struct ListRequestsTool {
    spec: ToolSpec,
}

impl ListRequestsTool {
    /// The tool with its static spec: class `SpanBayRead`, read-only, parallel-safe.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: tool_spec(
                LIST_REQUESTS,
                "List saved requests (default) or history entries of the SpanBay workspace this \
                 chat belongs to. Rows hold the request id, folder path, name, method and the URL \
                 template with {{variables}} unresolved; history rows add time, status and \
                 duration, newest first, and filter by status, since and origin; pass next_cursor \
                 back as cursor for the next page. No bodies.",
                ToolClass::SpanBayRead,
                false,
                json!({
                    "type": "object", "additionalProperties": false,
                    "properties": {
                        "query": {"type": "string", "description": "Text to look for in names and URLs"},
                        "collection": {"type": "string", "description": "Collection name (saved requests only)"},
                        "folder": {"type": "string", "description": "Folder name or path (saved requests only)"},
                        "method": {"type": "string", "enum": ["GET", "POST", "PUT", "PATCH", "DELETE", "HEAD", "OPTIONS"], "description": "Only requests with this method"},
                        "source": {"type": "string", "enum": ["saved", "history"], "description": "Saved requests or the send history"},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 50, "description": "Most rows to return"},
                        "status": {"type": "string", "enum": ["2xx", "3xx", "4xx", "5xx", "failed"], "description": "History only: status class, or failed (no response, 400 and above, or a failed check)"},
                        "since": {"type": "string", "description": "History only: 1h, 24h, 7d or an ISO date (YYYY-MM-DD)"},
                        "origin": {"type": "string", "enum": ["request", "runner", "flow", "mock", "agent"], "description": "History only: what sent it (Requests module, collection run, flow, mock server, agent)"},
                        "cursor": {"type": "string", "minLength": 1, "description": "History only: next_cursor of the previous page"}
                    }
                }),
            ),
        }
    }
}

impl Default for ListRequestsTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct ListArgs {
    query: Option<String>,
    collection: Option<String>,
    folder: Option<String>,
    method: Option<String>,
    source: Option<String>,
    limit: Option<usize>,
    status: Option<String>,
    since: Option<String>,
    origin: Option<String>,
    cursor: Option<String>,
}

fn contains(haystack: &str, needle: &str) -> bool {
    haystack
        .to_ascii_lowercase()
        .contains(&needle.to_ascii_lowercase())
}

/// The store filter of a history call: text, method, status class, start and origin.
fn history_filter(args: &ListArgs, query: Option<&str>) -> Result<HistoryFilter, AppError> {
    let mut filter = HistoryFilter {
        query: query.unwrap_or_default().to_owned(),
        ..HistoryFilter::default()
    };
    if let Some(method) = &args.method {
        filter.methods = vec![method_of(method)?];
    }
    match args.status.as_deref() {
        Some("failed") => filter.include_failed = true,
        Some(class) => {
            let digit = class.chars().next().and_then(|c| c.to_digit(10));
            filter.status_classes = digit
                .and_then(|d| u8::try_from(d).ok())
                .into_iter()
                .collect();
        }
        None => {}
    }
    if let Some(since) = &args.since {
        filter.since_ms = Some(since_ms(since, current_time_ms())?);
    }
    if let Some(origin) = &args.origin {
        let source = HistorySource::parse(origin)
            .ok_or_else(|| AppError::validation(format!("unknown origin {origin}")))?;
        filter.sources = vec![source];
    }
    Ok(filter)
}

fn list(state: &AppState, session_id: &str, args: &ListArgs) -> Result<ToolOutput, AppError> {
    let scope = bound_scope(state, session_id)?;
    let workspace_id = scope.workspace.id.clone();
    let history = args.source.as_deref() == Some("history");
    if history && (args.collection.is_some() || args.folder.is_some()) {
        return Err(AppError::validation(
            "`collection` and `folder` only apply to saved requests.",
        ));
    }
    let history_only = [&args.status, &args.since, &args.origin, &args.cursor];
    if !history && history_only.iter().any(|arg| arg.is_some()) {
        return Err(AppError::validation(
            "`status`, `since`, `origin` and `cursor` only apply to source \"history\".",
        ));
    }
    let limit = args.limit.unwrap_or(DEFAULT_ROWS).min(MAX_ROWS);
    let query = args
        .query
        .as_deref()
        .map(str::trim)
        .filter(|q| !q.is_empty());
    let method = args.method.as_deref();
    let store = state.store();
    let secrets = workspace_secrets(&store, workspace_id.as_str())?;
    let mut next_cursor = None;
    let (rows, matched) = if history {
        let filter = history_filter(args, query)?;
        let cursor = args
            .cursor
            .as_deref()
            .map(|text| {
                HistoryCursor::decode(text.trim()).ok_or_else(|| {
                    AppError::validation("`cursor` is not a next_cursor this tool returned.")
                })
            })
            .transpose()?;
        let page = store
            .query_execution_history(
                workspace_id.as_str(),
                &filter,
                u32::try_from(limit).unwrap_or(1),
                cursor.as_ref(),
            )
            .map_err(command_error)?;
        next_cursor = page.next_cursor.as_ref().map(HistoryCursor::encode);
        let matching: Vec<Value> = page
            .entries
            .iter()
            .map(|entry| {
                json!({
                    "history_id": entry.id.as_str(),
                    "request_id": entry.request_id.as_ref().map(|id| id.as_str()),
                    "time": entry.executed_at,
                    "status": entry.status_code,
                    "duration_ms": u64::try_from(entry.elapsed_ms).unwrap_or(u64::MAX),
                    "method": method_name(entry.method),
                    "name": entry.request_name,
                    "url": entry.request_url,
                    "source": entry.source.as_str(),
                    "environment": entry.environment_name,
                })
            })
            .collect();
        let matched = matching.len();
        (matching, matched)
    } else {
        let tree = Tree::load(&store, &workspace_id)?;
        let touched = store
            .agent_touched_request_ids(workspace_id.as_str())
            .map_err(command_error)?;
        let requests = store
            .list_requests(workspace_id.as_str())
            .map_err(command_error)?;
        let matching: Vec<Value> = requests
            .iter()
            .filter(|request| {
                let location = tree.location(request);
                let collection_ok = args.collection.as_deref().is_none_or(|wanted| {
                    location
                        .split('/')
                        .next()
                        .is_some_and(|name| name.eq_ignore_ascii_case(wanted.trim()))
                });
                let folder_ok = args.folder.as_deref().is_none_or(|wanted| {
                    let wanted = wanted.trim().trim_matches('/').to_ascii_lowercase();
                    let location = location.to_ascii_lowercase();
                    location == wanted || location.ends_with(&format!("/{wanted}"))
                });
                let method_ok = method
                    .is_none_or(|method| method_name(request.method).eq_ignore_ascii_case(method));
                let query_ok = query.is_none_or(|query| {
                    contains(&request.name, query)
                        || contains(&request.url, query)
                        || contains(&location, query)
                });
                collection_ok && folder_ok && method_ok && query_ok
            })
            .map(|request| {
                json!({
                    "request_id": request.id.as_str(),
                    "folder": tree.location(request),
                    "name": request.name,
                    "method": method_name(request.method),
                    "url": request.url,
                    "agent_touched": touched.contains(request.id.as_str()),
                })
            })
            .collect();
        let matched = matching.len();
        (matching, matched)
    };
    drop(store);
    if rows.is_empty() {
        return Ok(ToolOutput::text(if history {
            "No history entries match."
        } else {
            "No saved requests match."
        }));
    }
    let mut shown: Vec<Value> = rows.into_iter().take(limit).collect();
    let page = |shown: &[Value], next_cursor: Option<&str>| {
        let mut value = json!({
            "workspace": scope.tag(),
            "source": if history { "history" } else { "saved" },
            "shown": shown.len(),
            "rows": shown,
        });
        if history {
            value["has_more"] = json!(next_cursor.is_some());
            if let Some(cursor) = next_cursor {
                value["next_cursor"] = json!(cursor);
            }
        } else {
            value["matched"] = json!(matched);
        }
        value
    };
    let mut value = page(&shown, next_cursor.as_deref());
    // A history page cut by the size cap would let next_cursor skip the cut rows: end the page
    // at the last row that fits and continue from it.
    while history
        && shown.len() > 1
        && redact_for_model(&value.to_string(), &secrets, usize::MAX).len() > RESULT_CAP
    {
        shown.pop();
        next_cursor = shown.last().and_then(row_cursor);
        value = page(&shown, next_cursor.as_deref());
    }
    Ok(json_output(&value, &secrets, RESULT_CAP))
}

/// The cursor that continues after a history row.
fn row_cursor(row: &Value) -> Option<String> {
    Some(
        HistoryCursor {
            executed_at: row["time"].as_str()?.to_owned(),
            id: row["history_id"].as_str()?.to_owned(),
        }
        .encode(),
    )
}

impl AgentTool for ListRequestsTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        let _: ListArgs = parse_args(&self.spec.input_schema, arguments)?;
        bound_scope(crate::agent_tools::app_state(ctx), ctx.session_id)?;
        Ok(CallInspection {
            read_only: true,
            ..CallInspection::default()
        })
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let args: ListArgs = match parse_args(&self.spec.input_schema, &arguments) {
                Ok(args) => args,
                Err(error) => return error_output(&error, &[]),
            };
            list(crate::agent_tools::app_state(ctx), ctx.session_id, &args)
                .unwrap_or_else(|error| error_output(&error, &[]))
        })
    }
}
