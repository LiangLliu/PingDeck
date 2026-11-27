//! The `query_gateway_log` tool: look at the traffic that went through the local gateway.
//!
//! Single responsibility: page the stored request log through the Providers module's own use case
//! (`llm_log_list_impl`) and filter the rows by time, status, client, provider, model
//! and text, as the Activity view does. Rows hold time, client, requested model to served target,
//! protocol, status, TTFT, tokens, cost and a short error: never a body, header or key.
//! It reads the stored log, so it works while the gateway is stopped.

use std::collections::BTreeMap;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::common::{all_secrets, error_output, json_output, tool_spec};
use super::schema::parse_args;
use crate::agent::tools::{
    AgentTool, CallInspection, ToolClass, ToolContext, ToolOutput, ToolSpec,
};
use crate::app::{AppState, current_time_ms};
use crate::error::AppError;
use crate::llm::store::{LogFilter, RequestLogEntry};
use crate::llm::usage::{local_day, local_midnight_ms};
use spanbay_app_llm::{llm_log_list_impl, llm_provider_list_impl};

/// Name of the tool.
pub const QUERY_GATEWAY_LOG: &str = "query_gateway_log";

/// Rows the model sees at most, whatever `limit` says.
const RENDERED_ROWS: usize = 50;
/// Rows when the model names no limit.
const DEFAULT_LIMIT: usize = 50;
/// Most log rows one call looks at.
const MAX_SCANNED: usize = 5_000;
/// Rows per page read from the log.
const PAGE: u32 = 500;
/// Cap of one result.
const RESULT_CAP: usize = 16 * 1024;
/// Longest error text shown, in characters.
const ERROR_CHARS: usize = 120;

/// `query_gateway_log {since?, status?, client?, provider?, model?, text?, limit?, group_by?}`
/// (`alias`, from before model routing, is read as `model`).
pub struct QueryGatewayLogTool {
    spec: ToolSpec,
}

impl QueryGatewayLogTool {
    /// The tool with its static spec: class `SpanBayRead`, read-only, parallel-safe.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: tool_spec(
                QUERY_GATEWAY_LOG,
                "Look at gateway traffic from the stored log: time, client, requested model to the \
                 served target, protocol, status, time to first token, tokens in and out, cost and \
                 a short error; or counts grouped by status, provider, client or requested model. \
                 No bodies, headers or keys.",
                ToolClass::SpanBayRead,
                false,
                json!({
                    "type": "object", "additionalProperties": false,
                    "properties": {
                        "since": {"type": "string", "description": "1h, 24h, 48h, 7d or an ISO date (YYYY-MM-DD)"},
                        "status": {"type": ["integer", "string"], "description": "A status code or a class such as 4xx"},
                        "client": {"type": "string", "description": "Gateway client name or id"},
                        "alias": {"type": "string", "description": "Deprecated: same as model"},
                        "provider": {"type": "string", "description": "Provider name or id"},
                        "model": {"type": "string", "description": "Model the client asked for"},
                        "text": {"type": "string", "description": "Text to look for in the error, model, provider and client"},
                        "limit": {"type": "integer", "minimum": 1, "maximum": 200, "description": "Most rows to return"},
                        "group_by": {"type": "string", "enum": ["none", "status", "provider", "client", "model", "alias"], "description": "Count rows per value instead of listing them"}
                    }
                }),
            ),
        }
    }
}

impl Default for QueryGatewayLogTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct LogArgs {
    since: Option<String>,
    status: Option<Value>,
    client: Option<String>,
    alias: Option<String>,
    provider: Option<String>,
    model: Option<String>,
    text: Option<String>,
    limit: Option<usize>,
    group_by: Option<String>,
}

/// A status filter: one code or a class (`4xx`).
enum StatusFilter {
    Code(u16),
    Class(u16),
}

impl StatusFilter {
    fn parse(value: &Value) -> Result<Self, AppError> {
        let bad = || {
            AppError::validation("`status` is a status code such as 429 or a class such as 4xx.")
        };
        match value {
            Value::Number(number) => number
                .as_u64()
                .and_then(|code| u16::try_from(code).ok())
                .map(Self::Code)
                .ok_or_else(bad),
            Value::String(text) => {
                let text = text.trim().to_ascii_lowercase();
                if let Some(class) = text.strip_suffix("xx") {
                    class
                        .parse::<u16>()
                        .ok()
                        .filter(|class| (1..=5).contains(class))
                        .map(Self::Class)
                        .ok_or_else(bad)
                } else {
                    text.parse().map(Self::Code).map_err(|_| bad())
                }
            }
            _ => Err(bad()),
        }
    }

    fn matches(&self, status: u16) -> bool {
        match *self {
            Self::Code(code) => status == code,
            Self::Class(class) => status / 100 == class,
        }
    }
}

/// The start of the window, Unix ms: `1h`, `24h`, `7d` (any `<n>h` or `<n>d`) or `YYYY-MM-DD`.
pub(super) fn since_ms(text: &str, now_ms: i64) -> Result<i64, AppError> {
    let text = text.trim();
    let relative = |unit: i64| {
        text[..text.len() - 1]
            .parse::<i64>()
            .ok()
            .filter(|count| *count >= 1)
            .map(|count| now_ms.saturating_sub(count.saturating_mul(unit)))
    };
    let found = if text.ends_with('h') {
        relative(3_600_000)
    } else if text.ends_with('d') {
        relative(86_400_000)
    } else {
        local_midnight_ms(text, 0)
    };
    found.ok_or_else(|| {
        AppError::validation("`since` is 1h, 24h, 48h, 7d or an ISO date (YYYY-MM-DD).")
    })
}

fn iso(ts_ms: i64) -> String {
    let seconds = ts_ms.div_euclid(1000).rem_euclid(86_400);
    format!(
        "{}T{:02}:{:02}:{:02}Z",
        local_day(ts_ms, 0),
        seconds / 3600,
        seconds % 3600 / 60,
        seconds % 60
    )
}

fn contains(haystack: Option<&str>, needle: &str) -> bool {
    haystack.is_some_and(|text| {
        text.to_ascii_lowercase()
            .contains(&needle.to_ascii_lowercase())
    })
}

fn same(value: Option<&str>, wanted: &str) -> bool {
    value.is_some_and(|value| value.eq_ignore_ascii_case(wanted.trim()))
}

/// The cost as text: `0.0123 USD`, `-` when no price was known.
fn cost_text(entry: &RequestLogEntry) -> Option<String> {
    entry
        .cost_micro_usd
        .map(|micro| format!("{:.6} {}", micro as f64 / 1_000_000.0, entry.cost_currency))
}

fn row(entry: &RequestLogEntry) -> Value {
    let served = match (&entry.provider_name, &entry.upstream_model) {
        (Some(provider), Some(model)) => Some(format!("{provider}/{model}")),
        (Some(provider), None) => Some(provider.clone()),
        _ => None,
    };
    json!({
        "id": entry.id,
        "time": iso(entry.ts_ms),
        "client": entry.client_name,
        "requested_model": entry.requested_model,
        "served": served,
        "protocol": match &entry.protocol_out {
            Some(out) if out != &entry.protocol_in => format!("{} -> {out}", entry.protocol_in),
            _ => entry.protocol_in.clone(),
        },
        "status": entry.status,
        "ttft_ms": entry.ttft_ms,
        "input_tokens": entry.input_tokens,
        "output_tokens": entry.output_tokens,
        "cost": cost_text(entry),
        "error": entry.error.as_deref().map(|error| error.chars().take(ERROR_CHARS).collect::<String>()),
    })
}

fn group_key(entry: &RequestLogEntry, by: &str) -> String {
    match by {
        "status" => entry.status.to_string(),
        "provider" => entry
            .provider_name
            .clone()
            .or_else(|| entry.provider_id.clone())
            .unwrap_or_else(|| "(none)".to_owned()),
        "client" => entry
            .client_name
            .clone()
            .unwrap_or_else(|| "(none)".to_owned()),
        // `model` (and the retired `alias`): the name the client asked for.
        _ => entry.requested_model.clone(),
    }
}

fn query(state: &AppState, args: &LogArgs) -> Result<ToolOutput, AppError> {
    let now = current_time_ms();
    let since = args
        .since
        .as_deref()
        .map(|text| since_ms(text, now))
        .transpose()?;
    let status = args.status.as_ref().map(StatusFilter::parse).transpose()?;
    let group_by = args.group_by.as_deref().filter(|by| *by != "none");
    let limit = args.limit.unwrap_or(DEFAULT_LIMIT);
    let provider_id = match args
        .provider
        .as_deref()
        .map(str::trim)
        .filter(|p| !p.is_empty())
    {
        Some(wanted) => {
            let providers = llm_provider_list_impl(state)?;
            let found = providers
                .iter()
                .find(|provider| provider.id == wanted)
                .or_else(|| {
                    providers
                        .iter()
                        .find(|provider| provider.name.eq_ignore_ascii_case(wanted))
                })
                .ok_or_else(|| {
                    AppError::not_found(format!(
                        "No provider \"{wanted}\". Available: {}.",
                        providers
                            .iter()
                            .map(|provider| provider.name.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    ))
                })?;
            Some(found.id.clone())
        }
        None => None,
    };
    let filter = LogFilter {
        source: Some("gateway".to_owned()),
        provider_id,
        ..LogFilter::default()
    };
    let mut matched: Vec<RequestLogEntry> = Vec::new();
    let mut groups: BTreeMap<String, u64> = BTreeMap::new();
    let mut matched_total = 0_u64;
    let mut scanned = 0_usize;
    let mut before = None;
    'pages: loop {
        let page = llm_log_list_impl(state, Some(filter.clone()), Some(PAGE), before)?;
        let Some(last) = page.last() else { break };
        before = Some(last.id);
        let page_len = page.len();
        for entry in page {
            scanned += 1;
            if since.is_some_and(|since| entry.ts_ms < since) || scanned > MAX_SCANNED {
                break 'pages;
            }
            let keep = status
                .as_ref()
                .is_none_or(|status| status.matches(entry.status))
                && args.client.as_deref().is_none_or(|client| {
                    same(entry.client_name.as_deref(), client)
                        || same(entry.client_id.as_deref(), client)
                })
                && args
                    .model
                    .as_deref()
                    .or(args.alias.as_deref())
                    .is_none_or(|model| {
                        contains(Some(&entry.requested_model), model)
                            || contains(entry.upstream_model.as_deref(), model)
                    })
                && args.text.as_deref().is_none_or(|text| {
                    contains(entry.error.as_deref(), text)
                        || contains(Some(&entry.requested_model), text)
                        || contains(entry.upstream_model.as_deref(), text)
                        || contains(entry.provider_name.as_deref(), text)
                        || contains(entry.client_name.as_deref(), text)
                });
            if !keep {
                continue;
            }
            matched_total += 1;
            if let Some(by) = group_by {
                *groups.entry(group_key(&entry, by)).or_insert(0) += 1;
            } else {
                matched.push(entry);
                if matched.len() >= limit {
                    break 'pages;
                }
            }
        }
        if page_len < PAGE as usize {
            break;
        }
    }
    let secrets = all_secrets(&state.store())?;
    let echo = json!({
        "since": args.since, "status": args.status, "client": args.client,
        "provider": args.provider, "model": args.model.as_ref().or(args.alias.as_ref()), "text": args.text, "group_by": group_by,
    });
    let mut value = json!({
        "scanned": scanned.min(MAX_SCANNED),
        "matched": matched_total,
        "filters": echo,
        "view": {"kind": "gateway_log", "filters": echo},
    });
    if let Some(by) = group_by {
        let mut counted: Vec<(String, u64)> = groups.into_iter().collect();
        counted.sort_by(|a, b| b.1.cmp(&a.1).then_with(|| a.0.cmp(&b.0)));
        value["group_by"] = json!(by);
        value["groups"] = json!(
            counted
                .into_iter()
                .take(RENDERED_ROWS)
                .map(|(key, count)| json!({"key": key, "count": count}))
                .collect::<Vec<_>>()
        );
    } else {
        let rows: Vec<Value> = matched.iter().take(RENDERED_ROWS).map(row).collect();
        value["shown"] = json!(rows.len());
        value["rows"] = json!(rows);
    }
    Ok(json_output(&value, &secrets, RESULT_CAP))
}

impl AgentTool for QueryGatewayLogTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        _ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        let args: LogArgs = parse_args(&self.spec.input_schema, arguments)?;
        if let Some(since) = &args.since {
            since_ms(since, current_time_ms())?;
        }
        if let Some(status) = &args.status {
            StatusFilter::parse(status)?;
        }
        Ok(CallInspection {
            read_only: true,
            ..CallInspection::default()
        })
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let args: LogArgs = match parse_args(&self.spec.input_schema, &arguments) {
                Ok(args) => args,
                Err(error) => return error_output(&error, &[]),
            };
            query(crate::agent_tools::app_state(ctx), &args)
                .unwrap_or_else(|error| error_output(&error, &[]))
        })
    }
}
