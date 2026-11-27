//! `submit_plan`: the model hands its finished plan (Markdown + optional structured Task Graph)
//! to the user.
//!
//! Single responsibility: the tool's spec, its arguments (including Kahn cycle validation on
//! `tasks`), the gate card built from them and the texts the model reads back. Offered only to a
//! main run in Plan mode. The run host opens a `Plan` gate with [`plan_card`] and waits
//! ([`super::super::gate`]); approving persists any structured `tasks` into `todos_json` and
//! starts the execution run in the chosen permission mode.

use std::collections::HashSet;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use spanbay_base::error::AppError;
use spanbay_model::agent::{
    AgentTodoItem, AgentTodoStatus, SubagentRole, TaskNodeKind, validate_task_graph_dependencies,
};
use spanbay_model::settings::ToolApproval;

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
/// Maximum structured Task Graph nodes allowed in one plan.
pub const MAX_PLAN_TASKS: usize = 20;
/// Longest task id in characters.
const MAX_TASK_ID_CHARS: usize = 64;
/// Longest task description in characters.
const MAX_TASK_CONTENT_CHARS: usize = 500;
/// Longest parallel group label in characters.
const MAX_GROUP_CHARS: usize = 64;

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
struct PlanTaskArg {
    id: String,
    content: String,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    kind: Option<TaskNodeKind>,
    #[serde(default)]
    role: Option<SubagentRole>,
    #[serde(default)]
    group: Option<String>,
}

#[derive(Debug, Deserialize)]
struct Args {
    plan: String,
    #[serde(default)]
    title: Option<String>,
    #[serde(default)]
    tasks: Vec<PlanTaskArg>,
}

/// Parsed and validated `submit_plan` arguments.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ParsedPlan {
    pub plan: String,
    pub title: Option<String>,
    pub tasks: Vec<AgentTodoItem>,
}

/// The plan, its title, and optional validated structured Task Graph (`tasks`).
///
/// # Errors
/// [`AppError::Validation`] for empty/oversized fields, duplicate IDs, unknown dependencies,
/// self-dependencies, or cycles.
pub fn parse(arguments: &Value) -> Result<ParsedPlan, AppError> {
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
    if args.tasks.len() > MAX_PLAN_TASKS {
        return Err(AppError::validation(format!(
            "tasks holds {} nodes; at most {MAX_PLAN_TASKS} are allowed",
            args.tasks.len()
        )));
    }

    let mut seen_ids = HashSet::with_capacity(args.tasks.len());
    let mut tasks = Vec::with_capacity(args.tasks.len());
    for (index, item) in args.tasks.into_iter().enumerate() {
        let id = item.id.trim().to_owned();
        if id.is_empty() {
            return Err(AppError::validation(format!("tasks[{index}].id is empty")));
        }
        if id.chars().count() > MAX_TASK_ID_CHARS {
            return Err(AppError::validation(format!(
                "tasks[{index}].id is longer than {MAX_TASK_ID_CHARS} characters"
            )));
        }
        if !seen_ids.insert(id.clone()) {
            return Err(AppError::validation(format!(
                "tasks[{index}].id '{id}' is used twice"
            )));
        }
        let content = item.content.trim().to_owned();
        if content.is_empty() {
            return Err(AppError::validation(format!(
                "tasks[{index}].content is empty"
            )));
        }
        if content.chars().count() > MAX_TASK_CONTENT_CHARS {
            return Err(AppError::validation(format!(
                "tasks[{index}].content is longer than {MAX_TASK_CONTENT_CHARS} characters"
            )));
        }
        let group = item
            .group
            .map(|g| g.trim().to_owned())
            .filter(|g| !g.is_empty());
        if group
            .as_ref()
            .is_some_and(|g| g.chars().count() > MAX_GROUP_CHARS)
        {
            return Err(AppError::validation(format!(
                "tasks[{index}].group is longer than {MAX_GROUP_CHARS} characters"
            )));
        }
        let depends_on: Vec<String> = item
            .depends_on
            .into_iter()
            .map(|d| d.trim().to_owned())
            .filter(|d| !d.is_empty())
            .collect();
        tasks.push(AgentTodoItem {
            id,
            content,
            status: AgentTodoStatus::Pending,
            depends_on,
            kind: item.kind,
            role: item.role,
            group,
            ..AgentTodoItem::default()
        });
    }

    if !tasks.is_empty() {
        validate_task_graph_dependencies(&tasks).map_err(AppError::validation)?;
    }

    Ok(ParsedPlan {
        plan: plan.to_owned(),
        title,
        tasks,
    })
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
    tasks: Vec<AgentTodoItem>,
    now_ms: i64,
) -> HumanGateDto {
    let steps = if tasks.is_empty() {
        step_count(&plan)
    } else {
        u32::try_from(tasks.len()).unwrap_or(u32::MAX)
    };
    HumanGateDto {
        id: call_id.to_owned(),
        run_id: run_id.to_owned(),
        call_id: call_id.to_owned(),
        kind: HumanGateKind::Plan,
        created_at_ms: now_ms,
        body: HumanGateBody::Plan {
            steps,
            markdown: plan,
            title,
            tasks,
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
            description: "Submit the finished plan for the user's approval. Write `plan` as Markdown with numbered steps, and for multi-stage or parallelizable work include structured `tasks` (an acyclic Task Graph with `id`, `depends_on`, `kind`, optional `role` and `group`) so the user can preview the Execution Graph before approving. Nothing is changed until approved.".to_owned(),
            input_schema: json!({
                "type": "object",
                "properties": {
                    "plan": {"type": "string", "description": "The plan, in Markdown, with numbered steps."},
                    "title": {"type": "string", "maxLength": MAX_TITLE_CHARS, "description": "A short title."},
                    "tasks": {
                        "type": "array",
                        "maxItems": MAX_PLAN_TASKS,
                        "description": "Optional structured Task Graph (DAG) nodes executed after approval.",
                        "items": {
                            "type": "object",
                            "properties": {
                                "id": {"type": "string", "maxLength": MAX_TASK_ID_CHARS, "description": "Short unique node id (e.g. 'explore_auth', 'impl_api', 'verify')."},
                                "content": {"type": "string", "maxLength": MAX_TASK_CONTENT_CHARS, "description": "Concise action title for this node."},
                                "depends_on": {
                                    "type": "array",
                                    "items": {"type": "string"},
                                    "description": "IDs of upstream prerequisite nodes that must complete first."
                                },
                                "kind": {"type": "string", "enum": ["explore", "implement", "verify", "gate"]},
                                "role": {"type": "string", "enum": ["fast", "deep", "vision"]},
                                "group": {"type": "string", "maxLength": MAX_GROUP_CHARS, "description": "Optional parallel group label shared by concurrent sibling nodes."}
                            },
                            "required": ["id", "content"]
                        }
                    }
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

    #[test]
    fn parse_validates_structured_task_graph_and_rejects_cycles_or_missing_deps() {
        let valid = parse(&json!({
            "title": "Auth refactor",
            "plan": "1. Explore\n2. Implement\n3. Verify",
            "tasks": [
                {"id": "t1", "content": "Explore router", "kind": "explore", "role": "fast", "group": "Audit"},
                {"id": "t2", "content": "Explore store", "kind": "explore", "role": "fast", "group": "Audit"},
                {"id": "t3", "content": "Implement changes", "depends_on": ["t1", "t2"], "kind": "implement"},
                {"id": "t4", "content": "Run cargo test", "depends_on": ["t3"], "kind": "verify"}
            ]
        }))
        .expect("valid DAG plan");
        assert_eq!(valid.tasks.len(), 4);
        assert_eq!(valid.tasks[2].depends_on, vec!["t1", "t2"]);

        let cycle = parse(&json!({
            "plan": "1. A\n2. B",
            "tasks": [
                {"id": "a", "content": "A", "depends_on": ["b"]},
                {"id": "b", "content": "B", "depends_on": ["a"]}
            ]
        }))
        .expect_err("cycle must fail");
        assert!(cycle.message().contains("cycle"));

        let missing = parse(&json!({
            "plan": "1. A",
            "tasks": [
                {"id": "a", "content": "A", "depends_on": ["ghost"]}
            ]
        }))
        .expect_err("missing dependency must fail");
        assert!(missing.message().contains("unknown task"));
    }
}
