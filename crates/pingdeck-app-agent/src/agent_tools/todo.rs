//! The `todo_write` tool: the model's task list for one chat (the plan checklist).
//!
//! Single responsibility: validate a whole replacement list, store it on the session
//! (`save_session_todos`) and publish it as `TodosUpdated` on the run's event stream. The tool has
//! no side effect outside the session row, so it is `Internal` (never asks) and runs alone
//! (parallel calls would race on one list).

use std::collections::HashSet;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::Deserialize;
use serde_json::{Value, json};

use crate::agent::model::{AgentEvent, AgentTodoItem, AgentTodoStatus};
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

/// One task as the model writes it.
#[derive(Debug, Deserialize)]
struct Item {
    #[serde(default)]
    id: Option<String>,
    content: String,
    status: AgentTodoStatus,
}

/// The arguments of `todo_write`.
#[derive(Debug, Deserialize)]
struct Args {
    todos: Vec<Item>,
}

/// Checks the list and fills in the ids the model left out.
fn validate(arguments: &Value) -> Result<Vec<AgentTodoItem>, AppError> {
    let args: Args = serde_json::from_value(arguments.clone())
        .map_err(|error| AppError::validation(format!("Invalid arguments: {error}")))?;
    if args.todos.len() > MAX_TODOS {
        return Err(AppError::validation(format!(
            "todos holds {} items; at most {MAX_TODOS} are allowed",
            args.todos.len()
        )));
    }
    let in_progress = args
        .todos
        .iter()
        .filter(|item| item.status == AgentTodoStatus::InProgress)
        .count();
    if in_progress > 1 {
        return Err(AppError::validation(format!(
            "{in_progress} tasks are in_progress; at most one may be"
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
        items.push(AgentTodoItem {
            id,
            content: content.to_owned(),
            status: item.status,
        });
    }
    Ok(items)
}

/// `Task list updated: 3 items (1 completed, 1 in progress, 1 pending).`
fn summary(items: &[AgentTodoItem]) -> String {
    let count = |status| items.iter().filter(|item| item.status == status).count();
    format!(
        "Task list updated: {} items ({} completed, {} in progress, {} pending, {} cancelled).",
        items.len(),
        count(AgentTodoStatus::Completed),
        count(AgentTodoStatus::InProgress),
        count(AgentTodoStatus::Pending),
        count(AgentTodoStatus::Cancelled),
    )
}

/// `todo_write` over the session's task list.
pub struct TodoWriteTool {
    spec: ToolSpec,
}

impl TodoWriteTool {
    fn new() -> Self {
        Self {
            spec: ToolSpec {
                name: TODO_WRITE.to_owned(),
                description: "Replace the chat's task list with `todos` (send the whole list every time). Use it for work of three or more steps: write the plan first, keep exactly one task in_progress while you work on it, and mark tasks completed as you finish them.".to_owned(),
                input_schema: json!({
                    "type": "object",
                    "properties": {
                        "todos": {
                            "type": "array",
                            "maxItems": MAX_TODOS,
                            "description": "The complete task list, in order.",
                            "items": {
                                "type": "object",
                                "properties": {
                                    "id": {"type": "string", "description": "Stable id of the task; leave it out for a new task."},
                                    "content": {"type": "string", "maxLength": MAX_CONTENT_CHARS, "description": "What to do, in one short sentence."},
                                    "status": {"type": "string", "enum": ["pending", "in_progress", "completed", "cancelled"]}
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

        // The whole list is replaced, and a refused list leaves the stored one alone.
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

    /// A call for a session that does not exist fails instead of claiming success.
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
