//! The `todo_write` tool: the model's task list or structured Task Graph (`任务图`) for one chat.
//!
//! Single responsibility: validate a whole replacement list (including optional DAG `depends_on`,
//! `kind`, `role`, `group`, Kahn cycle detection, and parallel-branch progress rules), store it on
//! the session (`save_session_todos`) and publish it as `TodosUpdated` on the run's event stream.
//! The tool has no side effect outside the session row, so it is `Internal` (never asks) and runs
//! alone (parallel calls would race on one list).

use std::collections::HashSet;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::model::{
    AgentEvent, AgentTodoItem, AgentTodoStatus, SubagentRole, TaskNodeKind, downstream_dependents,
    validate_task_graph_dependencies,
};
use crate::agent::tools::{
    AgentTool, CallInspection, ToolClass, ToolContext, ToolOutput, ToolRegistry, ToolSource,
    ToolSpec,
};
use crate::app::factory_service::new_entity_id;
use crate::error::AppError;

/// The tool's function name.
pub const TODO_WRITE: &str = "todo_write";
/// Most tasks one list holds.
pub const MAX_TODOS: usize = 50;
/// Longest task text, in characters.
pub const MAX_CONTENT_CHARS: usize = 500;
/// Longest task id the model may name, in characters.
const MAX_ID_CHARS: usize = 64;
/// Longest parallel group label in characters.
const MAX_GROUP_CHARS: usize = 64;
/// Longest summary or error text attached to a task node.
const MAX_META_CHARS: usize = 500;

/// One task as the model writes it.
#[derive(Debug, Deserialize)]
struct Item {
    #[serde(default)]
    id: Option<String>,
    content: String,
    status: AgentTodoStatus,
    #[serde(default)]
    depends_on: Vec<String>,
    #[serde(default)]
    kind: Option<TaskNodeKind>,
    #[serde(default)]
    role: Option<SubagentRole>,
    #[serde(default)]
    group: Option<String>,
    #[serde(default)]
    run_id: Option<String>,
    #[serde(default)]
    attempt: u32,
    #[serde(default)]
    summary: Option<String>,
    #[serde(default)]
    error: Option<String>,
}

/// The arguments of `todo_write`.
#[derive(Debug, Deserialize)]
struct Args {
    todos: Vec<Item>,
}

/// Checks the list (or structured Task Graph) and fills in any omitted IDs.
fn validate(arguments: &Value) -> Result<Vec<AgentTodoItem>, AppError> {
    let args: Args = serde_json::from_value(arguments.clone())
        .map_err(|error| AppError::validation(format!("Invalid arguments: {error}")))?;
    if args.todos.len() > MAX_TODOS {
        return Err(AppError::validation(format!(
            "todos holds {} items; at most {MAX_TODOS} are allowed",
            args.todos.len()
        )));
    }
    let mut seen = HashSet::new();
    let mut items = Vec::with_capacity(args.todos.len());
    for (index, item) in args.todos.into_iter().enumerate() {
        let content = item.content.trim();
        if content.is_empty() {
            return Err(AppError::validation(format!(
                "todos[{index}].content is empty"
            )));
        }
        if content.chars().count() > MAX_CONTENT_CHARS {
            return Err(AppError::validation(format!(
                "todos[{index}].content is longer than {MAX_CONTENT_CHARS} characters"
            )));
        }
        let id = match item.id.as_deref().map(str::trim) {
            Some("") | None => new_entity_id("td"),
            Some(id) if id.chars().count() > MAX_ID_CHARS => {
                return Err(AppError::validation(format!(
                    "todos[{index}].id is longer than {MAX_ID_CHARS} characters"
                )));
            }
            Some(id) => id.to_owned(),
        };
        if !seen.insert(id.clone()) {
            return Err(AppError::validation(format!(
                "todos[{index}].id {id} is used twice"
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
                "todos[{index}].group is longer than {MAX_GROUP_CHARS} characters"
            )));
        }
        let summary = item
            .summary
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .map(|s| truncate_chars(&s, MAX_META_CHARS));
        let error = item
            .error
            .map(|s| s.trim().to_owned())
            .filter(|s| !s.is_empty())
            .map(|s| truncate_chars(&s, MAX_META_CHARS));
        let depends_on: Vec<String> = item
            .depends_on
            .into_iter()
            .map(|d| d.trim().to_owned())
            .filter(|d| !d.is_empty())
            .collect();
        items.push(AgentTodoItem {
            id,
            content: content.to_owned(),
            status: item.status,
            depends_on,
            kind: item.kind,
            role: item.role,
            group,
            run_id: item.run_id.filter(|s| !s.trim().is_empty()),
            attempt: item.attempt,
            summary,
            error,
        });
    }

    validate_task_graph_dependencies(&items).map_err(AppError::validation)?;

    let uses_graph = items
        .iter()
        .any(|item| !item.depends_on.is_empty() || item.group.is_some());
    let in_progress_ids: Vec<&str> = items
        .iter()
        .filter(|item| item.status == AgentTodoStatus::InProgress)
        .map(|item| item.id.as_str())
        .collect();

    if !uses_graph {
        if in_progress_ids.len() > 1 {
            return Err(AppError::validation(format!(
                "{} tasks are in_progress; at most one may be",
                in_progress_ids.len()
            )));
        }
    } else {
        // In a structured Task Graph, independent parallel branches may run concurrently,
        // but a downstream node cannot be `in_progress` while an upstream dependency is still
        // `in_progress`.
        let active_set: HashSet<&str> = in_progress_ids.iter().copied().collect();
        for &active_id in &in_progress_ids {
            for down in downstream_dependents(&items, active_id) {
                if active_set.contains(down.as_str()) {
                    return Err(AppError::validation(format!(
                        "task '{down}' cannot be in_progress while upstream dependency '{active_id}' is still in_progress"
                    )));
                }
            }
        }
    }

    Ok(items)
}

fn truncate_chars(text: &str, max: usize) -> String {
    text.chars().take(max).collect()
}

/// `Task list updated: 3 items (1 completed, 1 in progress, 1 pending, 0 failed, 0 blocked, 0 cancelled).`
fn summary(items: &[AgentTodoItem]) -> String {
    let count = |status| items.iter().filter(|item| item.status == status).count();
    format!(
        "Task list updated: {} items ({} completed, {} in progress, {} pending, {} failed, {} blocked, {} cancelled).",
        items.len(),
        count(AgentTodoStatus::Completed),
        count(AgentTodoStatus::InProgress),
        count(AgentTodoStatus::Pending),
        count(AgentTodoStatus::Failed),
        count(AgentTodoStatus::Blocked),
        count(AgentTodoStatus::Cancelled),
    )
}

/// `todo_write` over the session's task list / Task Graph.
pub struct TodoWriteTool {
    spec: ToolSpec,
}

impl TodoWriteTool {
    fn new() -> Self {
        Self {
            spec: ToolSpec {
                name: TODO_WRITE.to_owned(),
                description: "Replace the chat's task list or structured Task Graph (`todos`; send the whole list every time). Use it for work of three or more steps: write the plan first, declare `depends_on`, `kind`, and optional `group` for multi-stage or parallel branches, and mark items completed, failed, or blocked as work progresses.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "maxItems": MAX_TODOS,
                            "description": "The complete task list or acyclic Task Graph.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": {"type": "string", "description": "Stable id of the task; leave it out for a new task."},
                                    "content": {"type": "string", "maxLength": MAX_CONTENT_CHARS, "description": "What to do, in one short sentence."},
                                    "status": {"type": "string", "enum": ["pending", "in_progress", "completed", "cancelled", "failed", "blocked"]},
                                    "depends_on": {"type": "array", "items": {"type": "string"}, "description": "Optional upstream task IDs that must finish before this task starts."},
                                    "kind": {"type": "string", "enum": ["explore", "implement", "verify", "gate"]},
                                    "role": {"type": "string", "enum": ["fast", "deep", "vision"]},
                                    "group": {"type": "string", "maxLength": MAX_GROUP_CHARS, "description": "Optional parallel group label shared by concurrent sibling tasks."},
                                    "summary": {"type": "string", "maxLength": MAX_META_CHARS, "description": "Short completion summary for downstream handoff."},
                                    "error": {"type": "string", "maxLength": MAX_META_CHARS, "description": "Failure reason when status is failed."}
                                },
                                "required": ["content", "status"]
                            }
                        }
                    },
                    "required": ["todos"]
                }),
                read_only: true,
                destructive: false,
                source: ToolSource::Builtin,
                class: ToolClass::Internal,
                parallel_safe: false,
            },
        }
    }
}

/// Registers `todo_write`.
pub fn register(registry: &mut ToolRegistry) {
    registry.register(Arc::new(TodoWriteTool::new()));
}

impl AgentTool for TodoWriteTool {
    fn spec(&self) -> &ToolSpec {
        &self.spec
    }

    fn inspect(
        &self,
        _ctx: &ToolContext<'_>,
        arguments: &Value,
    ) -> Result<CallInspection, AppError> {
        validate(arguments)?;
        Ok(CallInspection {
            read_only: true,
            ..CallInspection::default()
        })
    }

    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput> {
        Box::pin(async move {
            let items = match validate(&arguments) {
                Ok(items) => items,
                Err(error) => return ToolOutput::error(error.message()),
            };
            let (state, session_id, stored) = (
                crate::agent_tools::app_state(ctx).clone(),
                ctx.session_id.to_owned(),
                items.clone(),
            );
            let saved = tokio::task::spawn_blocking(move || {
                state.store().save_session_todos(&session_id, &stored)
            })
            .await;
            match saved {
                Ok(Ok(true)) => {}
                Ok(Ok(false)) => return ToolOutput::error("This chat no longer exists."),
                Ok(Err(error)) => {
                    return ToolOutput::error(format!("The task list could not be saved: {error}"));
                }
                Err(error) => {
                    return ToolOutput::error(format!("The task list could not be saved: {error}"));
                }
            }
            let text = summary(&items);
            if let Some(run) = crate::agent_tools::app_state(ctx)
                .agent
                .runs
                .get(ctx.run_id)
            {
                run.emit(AgentEvent::TodosUpdated { todos: items });
            }
            ToolOutput::text(text)
        })
    }
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::agent::model::AgentSessionInput;
    use crate::agent::tools::TestParts;
    use crate::agent_service::agent_session_create_impl;
    use crate::app::AppState;
    use crate::cancel::CancelToken;

    fn item(content: &str, status: &str) -> Value {
        json!({"content": content, "status": status})
    }

    fn rejected(todos: Value) -> String {
        validate(&json!({ "todos": todos }))
            .expect_err("the list must be refused")
            .message()
            .to_owned()
    }

    #[test]
    fn a_list_is_checked_and_ids_are_filled_in() {
        let items = validate(&json!({"todos": [
            {"id": "a", "content": "  first  ", "status": "completed"},
            item("second", "in_progress"),
            item("third", "pending"),
        ]}))
        .expect("valid list");
        assert_eq!(items.len(), 3);
        assert_eq!(items[0].id, "a");
        assert_eq!(items[0].content, "first");
        assert!(items[1].id.starts_with("td"), "{}", items[1].id);
        assert_ne!(items[1].id, items[2].id);
        assert!(
            validate(&json!({"todos": []}))
                .expect("an empty list clears")
                .is_empty()
        );
    }

    #[test]
    fn limits_are_enforced() {
        let many: Vec<Value> = (0..=MAX_TODOS)
            .map(|n| item(&format!("t{n}"), "pending"))
            .collect();
        assert!(rejected(json!(many)).contains("at most 50"));
        let exact: Vec<Value> = (0..MAX_TODOS)
            .map(|n| item(&format!("t{n}"), "pending"))
            .collect();
        assert!(validate(&json!({"todos": exact})).is_ok());

        let long = "x".repeat(MAX_CONTENT_CHARS + 1);
        assert!(rejected(json!([item(&long, "pending")])).contains("500 characters"));
        let fits = "é".repeat(MAX_CONTENT_CHARS);
        assert!(validate(&json!({"todos": [item(&fits, "pending")]})).is_ok());

        assert!(rejected(json!([item("  ", "pending")])).contains("empty"));
        assert!(
            rejected(json!([item("a", "in_progress"), item("b", "in_progress")]))
                .contains("at most one")
        );
        assert!(
            rejected(json!([
                {"id": "x", "content": "a", "status": "pending"},
                {"id": "x", "content": "b", "status": "pending"},
            ]))
            .contains("used twice")
        );
        assert!(rejected(json!([item("a", "started")])).contains("Invalid arguments"));
    }

    #[test]
    fn structured_task_graph_allows_parallel_branches_and_rejects_cycles_or_dependent_overlap() {
        let ok = validate(&json!({"todos": [
            {"id": "root", "content": "Locate files", "status": "completed", "kind": "explore"},
            {"id": "b1", "content": "Audit auth", "status": "in_progress", "depends_on": ["root"], "group": "Audit"},
            {"id": "b2", "content": "Audit store", "status": "in_progress", "depends_on": ["root"], "group": "Audit"},
            {"id": "join", "content": "Implement fix", "status": "pending", "depends_on": ["b1", "b2"], "kind": "implement"}
        ]}))
        .expect("independent parallel branches may run simultaneously");
        assert_eq!(ok.len(), 4);
        assert_eq!(ok[1].group.as_deref(), Some("Audit"));

        // Dependent nodes cannot both be in_progress simultaneously.
        let dep_overlap = rejected(json!([
            {"id": "a", "content": "First", "status": "in_progress"},
            {"id": "b", "content": "Second", "status": "in_progress", "depends_on": ["a"]}
        ]));
        assert!(dep_overlap.contains("upstream dependency"), "{dep_overlap}");

        // Cycles are rejected.
        let cycle = rejected(json!([
            {"id": "a", "content": "First", "status": "pending", "depends_on": ["b"]},
            {"id": "b", "content": "Second", "status": "pending", "depends_on": ["a"]}
        ]));
        assert!(cycle.contains("cycle"), "{cycle}");
    }

    #[tokio::test(flavor = "multi_thread")]
    async fn a_call_replaces_the_session_list() {
        let state = AppState::open_in_memory().expect("state");
        let session =
            agent_session_create_impl(&state, AgentSessionInput::default()).expect("session");
        let settings = state.settings();
        let cancel = CancelToken::new();
        let parts = TestParts::default();
        let mut ctx = ToolContext::for_test(
            Some(&state as &(dyn std::any::Any + Send + Sync)),
            &settings,
            &cancel,
            &parts,
        );
        ctx.session_id = &session.id;
        let tool = TodoWriteTool::new();

        let first = tool
            .call(
                &ctx,
                json!({"todos": [item("one", "in_progress"), item("two", "pending")]}),
            )
            .await;
        assert!(!first.is_error, "{}", first.text_content());
        assert!(first.text_content().contains("2 items"));
        let stored = state
            .store()
            .get_agent_session(&session.id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.todos.len(), 2);

        let bad = tool
            .call(&ctx, json!({"todos": [item("", "pending")]}))
            .await;
        assert!(bad.is_error);
        let second = tool
            .call(&ctx, json!({"todos": [item("only", "completed")]}))
            .await;
        assert!(!second.is_error);
        let stored = state
            .store()
            .get_agent_session(&session.id)
            .unwrap()
            .unwrap();
        assert_eq!(stored.todos.len(), 1);
        assert_eq!(stored.todos[0].content, "only");
        assert_eq!(stored.todos[0].status, AgentTodoStatus::Completed);

        ctx_gone(&tool, &state, &settings, &cancel, &parts).await;
    }

    async fn ctx_gone(
        tool: &TodoWriteTool,
        state: &crate::app::AppState,
        settings: &crate::settings::Settings,
        cancel: &CancelToken,
        parts: &TestParts,
    ) {
        let ctx = ToolContext::for_test(
            Some(state as &(dyn std::any::Any + Send + Sync)),
            settings,
            cancel,
            parts,
        );
        let output = tool
            .call(&ctx, json!({"todos": [item("x", "pending")]}))
            .await;
        assert!(output.is_error);
        assert!(output.text_content().contains("no longer exists"));
    }

    #[test]
    fn the_tool_is_internal_read_only_and_not_parallel() {
        let tool = TodoWriteTool::new();
        let spec = tool.spec();
        assert_eq!(spec.name, "todo_write");
        assert_eq!(spec.class, ToolClass::Internal);
        assert!(spec.read_only && !spec.destructive && !spec.parallel_safe);
    }
}
