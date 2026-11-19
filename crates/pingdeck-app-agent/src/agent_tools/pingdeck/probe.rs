//! The `run_probe` tool: test models of a provider from the chat.
//!
//! Single responsibility: map `{provider, models, checks}` onto the Test bench use case
//! (`llm_probe_run_impl`) and report each cell with the fixed status vocabulary. The
//! cost rule of the bench holds: a run whose estimate needs the cost dialog starts only with the
//! user's approval. [`AgentTool::inspect`] puts the one estimate string on the approval card and
//! forces the card in every mode; the call it let through carries the approval, which maps to
//! `confirmed_cost`. A call that was never inspected is refused with the estimate. What the probe
//! spends counts against the run's cost cap as the estimate's share of the finished cells. Results
//! are never applied (D-13).

use std::collections::HashSet;
use std::sync::{Mutex, PoisonError};
use std::time::Duration;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use super::common::{RunLimit, all_secrets, error_output, json_output, timed_out, tool_spec};
use super::schema::parse_args;
use crate::agent::tools::{
    AgentTool, CallInspection, ToolApprovalPreview, ToolClass, ToolContext, ToolOutput, ToolSpec,
};
use crate::app::AppState;
use crate::error::AppError;
use crate::events::EventSink;
use crate::llm::probe::ProbeTest;
use crate::llm::probe::plan::CheckSelection;
use pingdeck_app_llm::{
    ProbeCellDto, ProbeEstimateDto, ProbeEvent, ProbeItem, ProbeRunInput, llm_key_list_impl,
    llm_probe_estimate_impl, llm_probe_run_impl, llm_provider_list_impl,
};

/// Name of the tool.
pub const RUN_PROBE: &str = "run_probe";

/// Probes one run may start (PRD §6).
const PROBES_PER_RUN: u32 = 3;
/// Models one probe tests.
const MAX_MODELS: usize = 8;
/// Longest a probe runs.
const PROBE_TIMEOUT_SECS: u64 = 60;
/// Output tokens from which a tokens-per-second figure is meaningful.
const TPS_MIN_TOKENS: u64 = 300;
/// Cap of one result.
const RESULT_CAP: usize = 16 * 1024;
/// The checks of a call that names none.
const DEFAULT_CHECKS: [&str; 4] = ["connectivity", "auth", "chat", "stream"];
/// The checks the tool accepts; they all send real requests except `connectivity` and `auth`.
const CHECKS: [&str; 9] = [
    "connectivity",
    "auth",
    "chat",
    "stream",
    "tools",
    "json_schema",
    "vision",
    "reasoning",
    "cache",
];

/// `run_probe {provider, models, checks?}`.
pub struct RunProbeTool {
    spec: ToolSpec,
    limit: RunLimit,
    /// Call ids whose estimate needed the cost dialog and went to an approval card.
    shown: Mutex<HashSet<String>>,
}

impl RunProbeTool {
    /// The tool with its static spec: class `PingDeckWrite` (real requests spend money), a barrier.
    #[must_use]
    pub fn new() -> Self {
        Self {
            spec: tool_spec(
                RUN_PROBE,
                "Test models of a provider with the Test bench checks and report each result with \
                 the fixed status words. Real-request checks cost tokens: the user approves the \
                 estimate first. Results are not applied to the provider.",
                ToolClass::PingDeckWrite,
                false,
                json!({
                    "type": "object", "additionalProperties": false,
                    "required": ["provider", "models"],
                    "properties": {
                        "provider": {"type": "string", "minLength": 1, "description": "Provider name or id"},
                        "models": {"type": "array", "minItems": 1, "maxItems": 8, "items": {"type": "string", "minLength": 1}, "description": "Model ids to probe"},
                        "checks": {"type": "array", "minItems": 1, "items": {"type": "string", "enum": CHECKS}, "description": "Checks to run on each model"}
                    }
                }),
            ),
            limit: RunLimit::new(PROBES_PER_RUN),
            shown: Mutex::new(HashSet::new()),
        }
    }
}

impl Default for RunProbeTool {
    fn default() -> Self {
        Self::new()
    }
}

#[derive(Deserialize)]
struct ProbeArgs {
    provider: String,
    models: Vec<String>,
    checks: Option<Vec<String>>,
}

/// The run the arguments describe, and the provider's display name and id.
fn bench_input(
    state: &AppState,
    args: &ProbeArgs,
    confirmed: bool,
) -> Result<(ProbeRunInput, (String, String)), AppError> {
    let providers = llm_provider_list_impl(state)?;
    let wanted = args.provider.trim();
    let provider = providers
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
                if providers.is_empty() {
                    "none".to_owned()
                } else {
                    providers
                        .iter()
                        .map(|provider| provider.name.as_str())
                        .collect::<Vec<_>>()
                        .join(", ")
                }
            ))
        })?;
    let key = llm_key_list_impl(state, Some(&provider.id))?
        .into_iter()
        .find(|key| key.enabled)
        .ok_or_else(|| {
            AppError::validation(format!(
                "Provider \"{}\" has no enabled key.",
                provider.name
            ))
        })?;
    let names: Vec<&str> = args.checks.as_ref().map_or_else(
        || DEFAULT_CHECKS.to_vec(),
        |checks| checks.iter().map(String::as_str).collect(),
    );
    let tests: Vec<ProbeTest> = names
        .iter()
        .filter_map(|name| ProbeTest::parse(name))
        .collect();
    let mut seen = HashSet::new();
    let items = args
        .models
        .iter()
        .map(|model| model.trim().to_owned())
        .filter(|model| seen.insert(model.clone()))
        .map(|model_id| ProbeItem {
            key_id: key.id.clone(),
            model_id,
        })
        .collect::<Vec<_>>();
    if items.len() > MAX_MODELS {
        return Err(AppError::validation("`models` takes at most 8 items."));
    }
    Ok((
        ProbeRunInput {
            items,
            checks: CheckSelection::Custom { tests },
            protocol: None,
            concurrency: None,
            retry_of: None,
            remaining_of: None,
            alias: None,
            confirmed_cost: confirmed,
        },
        (provider.name.clone(), provider.id.clone()),
    ))
}

/// The estimate of `input` from a synchronous caller: [`AgentTool::inspect`] is not async, so the
/// use case runs on a thread of its own with a small runtime.
fn estimate_blocking(state: &AppState, input: ProbeRunInput) -> Result<ProbeEstimateDto, AppError> {
    let state = state.clone();
    std::thread::Builder::new()
        .name("pingdeck-probe-estimate".to_owned())
        .spawn(move || {
            let runtime = tokio::runtime::Builder::new_current_thread()
                .enable_all()
                .build()
                .map_err(|error| AppError::internal(format!("estimate runtime failed: {error}")))?;
            runtime.block_on(llm_probe_estimate_impl(&state, input))
        })
        .map_err(|error| AppError::internal(format!("estimate thread failed: {error}")))?
        .join()
        .map_err(|_| AppError::internal("the estimate thread panicked"))?
}

/// Collects the finished cells of a run.
struct Cells(Mutex<Vec<ProbeCellDto>>);

impl EventSink<ProbeEvent> for Cells {
    fn emit(&self, event: ProbeEvent) -> bool {
        if let ProbeEvent::Cell { cell, .. } = event {
            self.0
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .push(cell);
        }
        true
    }
}

fn cell_json(cell: &ProbeCellDto) -> Value {
    let result = cell.result.as_ref();
    let tps = result
        .filter(|result| result.output_tokens.unwrap_or(0) >= TPS_MIN_TOKENS)
        .and_then(|result| result.tps)
        .map(|tps| (tps * 10.0).round() / 10.0);
    json!({
        "model": cell.model_id,
        "check": cell.test.as_str(),
        "status": cell.status.label(),
        "reason": cell.reason,
        "latency_ms": result.and_then(|result| result.latency_ms),
        "ttft_ms": result.and_then(|result| result.ttft_ms),
        "tokens_per_second": tps,
    })
}

/// Counts the share of `estimate` the finished cells (`done` of `total`) stand for against the
/// cost cap of the call's run, in each currency the estimate priced.
fn charge_probe(ctx: &ToolContext<'_>, estimate: &ProbeEstimateDto, done: u32, total: u32) {
    if total == 0 {
        return;
    }
    for amount in &estimate.cost {
        let share = i128::from(amount.micro) * i128::from(done.min(total)) / i128::from(total);
        ctx.charge_run(&amount.currency, i64::try_from(share).unwrap_or(i64::MAX));
    }
}

async fn run(
    tool: &RunProbeTool,
    ctx: &ToolContext<'_>,
    args: &ProbeArgs,
) -> Result<ToolOutput, AppError> {
    let approved = tool
        .shown
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .remove(ctx.call_id);
    let (input, (provider, provider_id)) =
        bench_input(crate::agent_tools::app_state(ctx), args, approved)?;
    if !tool.limit.take(ctx.run_id) {
        return Err(AppError::validation(format!(
            "At most {} probes per turn.",
            tool.limit.limit()
        )));
    }
    let secrets = all_secrets(&crate::agent_tools::app_state(ctx).store())?;
    // The bench's own estimate prices what the probe spends against the run's cost cap.
    let estimate = llm_probe_estimate_impl(crate::agent_tools::app_state(ctx), input.clone())
        .await
        .ok();
    let token = format!("agent-probe-{}-{}", ctx.run_id, ctx.call_id);
    let cells = Cells(Mutex::new(Vec::new()));
    let run = llm_probe_run_impl(
        crate::agent_tools::app_state(ctx),
        input,
        Some(token.clone()),
        &cells,
    );
    let stop = async {
        tokio::select! {
            () = ctx.cancel.cancelled() => false,
            () = tokio::time::sleep(Duration::from_secs(PROBE_TIMEOUT_SECS)) => true,
        }
    };
    tokio::pin!(run, stop);
    let (summary, timed_out_run) = tokio::select! {
        done = &mut run => (done, false),
        timeout = &mut stop => {
            crate::agent_tools::app_state(ctx).cancellations.cancel(&token);
            (run.await, timeout)
        }
    };
    if let (Some(estimate), Ok(summary)) = (&estimate, &summary) {
        charge_probe(ctx, estimate, summary.done, summary.total);
    }
    if timed_out_run {
        return Ok(timed_out(PROBE_TIMEOUT_SECS));
    }
    let summary = summary?;
    let results: Vec<Value> = cells
        .0
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .iter()
        .map(cell_json)
        .collect();
    let value = json!({
        "provider": provider,
        "run_id": summary.run_id,
        "totals": {
            "planned": summary.total, "done": summary.done, "passed": summary.passed,
            "failed": summary.failed, "rate_limited": summary.rate_limited,
            "unsupported": summary.unsupported, "skipped": summary.skipped,
            "not_tested": summary.not_tested, "stopped": summary.cancelled,
        },
        "results": results,
        // `provider_id` is for the UI's "Open in Providers" link; the typed meta keeps `run_id` only.
        "view": {"kind": "probe", "run_id": summary.run_id, "provider_id": provider_id},
    });
    Ok(json_output(&value, &secrets, RESULT_CAP))
}

impl AgentTool for RunProbeTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        let args: ProbeArgs = parse_args(&self.spec.input_schema, arguments)?;
        let (input, (provider, _)) = bench_input(crate::agent_tools::app_state(ctx), &args, false)?;
        let models = input.items.len();
        let checks: Vec<&str> = args.checks.as_ref().map_or_else(
            || DEFAULT_CHECKS.to_vec(),
            |checks| checks.iter().map(String::as_str).collect(),
        );
        let estimate = estimate_blocking(crate::agent_tools::app_state(ctx), input)?;
        let always_ask = estimate.needs_confirmation.then(|| {
            self.shown
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(ctx.call_id.to_owned());
            format!(
                "Asks because: real requests cost money ({})",
                estimate.summary
            )
        });
        Ok(CallInspection {
            read_only: false,
            destructive: false,
            preview: Some(ToolApprovalPreview::Summary {
                text: format!(
                    "Probe {models} model{} of {provider} ({}): {}",
                    if models == 1 { "" } else { "s" },
                    checks.join(", "),
                    estimate.summary
                ),
            }),
            ask_code: always_ask.is_some().then_some("probe_cost"),
            always_ask,
            arg_subject: Some(provider),
            ..CallInspection::default()
        })
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let args: ProbeArgs = match parse_args(&self.spec.input_schema, &arguments) {
                Ok(args) => args,
                Err(error) => return error_output(&error, &[]),
            };
            run(self, ctx, &args)
                .await
                .unwrap_or_else(|error| error_output(&error, &[]))
        })
    }
}
