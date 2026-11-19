//! `submit_plan`: the model hands its finished plan to the user.
//!
//! Single responsibility: the tool's spec, its arguments, the gate card built from them and the
//! texts the model reads back. Offered only to a main run in Plan mode. The run host opens a
//! `Plan` gate with [`plan_card`] and waits ([`super::super::gate`]); the plan text is the call's
//! own argument, so it is stored with the message tree and needs no column. Approval ends the run
//! and a new one carries the plan out in the chosen mode.

use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use pingdeck_base::error::AppError;
use pingdeck_model::settings::ToolApproval;

use super::super::model::{HumanGateBody, HumanGateDto, HumanGateKind};
use super::{
    AgentTool, CallInspection, ToolClass, ToolContext, ToolOutput, ToolRegistry, ToolSource,
    ToolSpec,
};

/// The tool's function name.
pub const SUBMIT_PLAN: &str = "submit_plan";
/// Longest plan, in bytes of Markdown.
pub const MAX_PLAN_BYTES: usize = 16 * 1024;
/// Longest title, in characters.
const MAX_TITLE_CHARS: usize = 120;

/// The tool result of an approved plan; the run ends after it.
pub const PLAN_APPROVED_RESULT: &str =
    "The user approved the plan. Stop here: a new turn carries it out.";
/// The tool result when nobody approved the plan in time; the run ends after it.
pub const PLAN_TIMED_OUT_RESULT: &str =
    "Nobody approved the plan in time. Stay in Plan mode and do not carry it out.";
/// The tool result when the user wrote instead of approving.
pub const PLAN_REVISE_RESULT: &str = "The user did not approve the plan yet. Their next message says what to change; revise the plan and submit it again.";

/// How the user answered a submitted plan, as the first line of the tool result states it.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum PlanOutcomeKind {
    /// The user approved the plan.
    Approved,
    /// The user wrote instead of approving.
    Revise,
    /// Nobody answered in time.
    TimedOut,
}

/// The first line of a `submit_plan` result: `{"plan":{"outcome":"approved","mode":"ask"}}`. The
/// UI stamps the plan block from it and, like the error code line, it is all that survives a
/// reload. `mode` is only written for an approval that names one.
#[must_use]
pub fn plan_outcome_line(outcome: PlanOutcomeKind, mode: Option<ToolApproval>) -> String {
    let name = match outcome {
        PlanOutcomeKind::Approved => "approved",
        PlanOutcomeKind::Revise => "revise",
        PlanOutcomeKind::TimedOut => "timed_out",
    };
    let mode = mode
        .filter(|_| outcome == PlanOutcomeKind::Approved)
        .and_then(|mode| serde_json::to_value(mode).ok())
        .and_then(|value| value.as_str().map(str::to_owned));
    match mode {
        Some(mode) => format!(r#"{{"plan":{{"outcome":"{name}","mode":"{mode}"}}}}"#),
        None => format!(r#"{{"plan":{{"outcome":"{name}"}}}}"#),
    }
}

#[derive(Debug, Deserialize)]
struct Args {
    plan: String,
    #[serde(default)]
    title: Option<String>,
}

/// The plan and its title, checked.
///
/// # Errors
/// [`AppError::Validation`] for an empty or too long plan or title.
pub fn parse(arguments: &Value) -> Result<(String, Option<String>), AppError> {
    let args: Args = serde_json::from_value(arguments.clone())
        .map_err(|error| AppError::validation(format!("Invalid arguments: {error}")))?;
    let plan = args.plan.trim();
    if plan.is_empty() {
        return Err(AppError::validation("plan is empty"));
    }
    if plan.len() > MAX_PLAN_BYTES {
        return Err(AppError::validation(format!(
            "plan is longer than {MAX_PLAN_BYTES} bytes"
        )));
    }
    let title = args
        .title
        .map(|title| title.trim().to_owned())
        .filter(|title| !title.is_empty());
    if title
        .as_ref()
        .is_some_and(|title| title.chars().count() > MAX_TITLE_CHARS)
    {
        return Err(AppError::validation(format!(
            "title is longer than {MAX_TITLE_CHARS} characters"
        )));
    }
    Ok((plan.to_owned(), title))
}

/// Top-level list items of `markdown` (`1.`, `-`, `*` at the start of a line): its steps.
#[must_use]
pub fn step_count(markdown: &str) -> u32 {
    let steps = markdown
        .lines()
        .filter(|line| {
            let digits = line.bytes().take_while(u8::is_ascii_digit).count();
            (digits > 0 && line[digits..].starts_with(". "))
                || line.starts_with("- ")
                || line.starts_with("* ")
        })
        .count();
    u32::try_from(steps).unwrap_or(u32::MAX)
}

/// The card of the `Plan` gate opened by call `call_id` of `run_id`.
#[must_use]
pub fn plan_card(
    run_id: &str,
    call_id: &str,
    plan: String,
    title: Option<String>,
    now_ms: i64,
) -> HumanGateDto {
    HumanGateDto {
        id: call_id.to_owned(),
        run_id: run_id.to_owned(),
        call_id: call_id.to_owned(),
        kind: HumanGateKind::Plan,
        created_at_ms: now_ms,
        body: HumanGateBody::Plan {
            steps: step_count(&plan),
            markdown: plan,
            title,
        },
    }
}

/// `submit_plan`; the run host carries out the call (it waits on the user).
pub struct SubmitPlanTool {
    spec: ToolSpec,
}

/// Registers `submit_plan` (Plan mode only).
pub fn register(registry: &mut ToolRegistry) {
    registry.register(Arc::new(SubmitPlanTool {
        spec: ToolSpec {
            name: SUBMIT_PLAN.to_owned(),
            description: "Submit the finished plan for the user's approval. Write it as Markdown with numbered steps. Nothing is changed until the user approves; then a new turn carries it out.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "plan": {"type": "string", "description": "The plan, in Markdown, with numbered steps."},
                    "title": {"type": "string", "maxLength": MAX_TITLE_CHARS, "description": "A short title."}
                },
                "required": ["plan"]
            }),
            read_only: true,
            destructive: false,
            source: ToolSource::Builtin,
            class: ToolClass::Internal,
            parallel_safe: false,
        },
    }));
}

impl AgentTool for SubmitPlanTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        _ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        parse(arguments)?;
        Ok(CallInspection {
            read_only: true,
            ..CallInspection::default()
        })
    }

    fn call<'a>(
        &'a self,
        _ctx: &'a ToolContext<'a>,
        _arguments: Value,
    ) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async { ToolOutput::error("submit_plan is only available in Plan mode.") })
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_outcome_line_carries_a_mode_only_for_an_approval() {
        assert_eq!(
            plan_outcome_line(PlanOutcomeKind::Approved, Some(ToolApproval::AutoReadOnly)),
            r#"{"plan":{"outcome":"approved","mode":"auto_read_only"}}"#
        );
        assert_eq!(
            plan_outcome_line(PlanOutcomeKind::Approved, None),
            r#"{"plan":{"outcome":"approved"}}"#
        );
        assert_eq!(
            plan_outcome_line(PlanOutcomeKind::Revise, Some(ToolApproval::Ask)),
            r#"{"plan":{"outcome":"revise"}}"#
        );
        assert_eq!(
            plan_outcome_line(PlanOutcomeKind::TimedOut, None),
            r#"{"plan":{"outcome":"timed_out"}}"#
        );
    }
}
