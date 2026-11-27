//! Selective node-level recovery for structured Task Graphs (`任务图`).
//!
//! Supports three targeted operations without re-running already-completed upstream or sibling
//! branches (Selective Graph Recovery):
//! - [`NodeRecoveryAction::Retry`]: resets the target node + transitively dependent downstream
//!   nodes, preserves completed branches, and launches a focused recovery run with optional user
//!   guidance.
//! - [`NodeRecoveryAction::UndoAndRetry`]: safely undoes pending file changes recorded for the
//!   failed run (`agent_file_changes`) before performing [`NodeRecoveryAction::Retry`].
//! - [`NodeRecoveryAction::Skip`]: marks the target node skipped (`Cancelled`), unblocks waiting
//!   downstream nodes, and launches a continuation run.

use std::collections::HashSet;

use crate::agent::dto::{AgentRunInput, AgentRunStarted};
use crate::agent::model::{
    AgentTodoItem, AgentTodoStatus, FileChangeStatus, NodeRecoveryAction, downstream_dependents,
};
use crate::error::AppError;

use super::changes::agent_change_undo_impl;
use super::host::blocking;
use super::run::agent_run_start_impl;
use super::sessions::load_session;
use crate::app::{AppState, command_error};

/// Recovers one Task Graph node (`node_id`) inside `session_id` according to `action`.
///
/// # Errors
/// - [`AppError::NotFound`] when the session or `node_id` does not exist.
/// - [`AppError::Conflict`] when a run is already active for `session_id`, or when
///   [`NodeRecoveryAction::UndoAndRetry`] hits an externally modified file.
pub async fn agent_task_node_recover_impl(
    state: &AppState,
    session_id: &str,
    node_id: &str,
    action: NodeRecoveryAction,
    guidance: Option<String>,
) -> Result<AgentRunStarted, AppError> {
    if state.agent.runs.active_for_session(session_id).is_some() {
        return Err(AppError::conflict(
            "stop or wait for the active run before recovering a task node",
        ));
    }

    let sid = session_id.to_owned();
    let nid = node_id.to_owned();
    let clean_guidance = guidance
        .map(|g| g.trim().to_owned())
        .filter(|g| !g.is_empty());

    let (prompt_text, updated_todos) = blocking(state, move |state| {
        let session = load_session(state, &sid)?;
        let mut todos = session.todos;
        let Some(target_idx) = todos.iter().position(|t| t.id == nid) else {
            return Err(AppError::not_found(format!(
                "task node '{nid}' not found in chat {sid}"
            )));
        };

        if action == NodeRecoveryAction::UndoAndRetry {
            let target_run_id = todos[target_idx].run_id.clone().or_else(|| {
                state
                    .store()
                    .list_session_main_runs(&sid, 1)
                    .ok()
                    .and_then(|mut v| v.pop().map(|r| r.run_id))
            });
            if let Some(rid) = target_run_id {
                let change_ids: Vec<String> = state
                    .store()
                    .list_chat_file_changes(&sid)
                    .map_err(command_error)?
                    .into_iter()
                    .filter(|c| c.run_id == rid && c.status == FileChangeStatus::Pending)
                    .map(|c| c.id)
                    .collect();
                if !change_ids.is_empty() {
                    let undone = agent_change_undo_impl(state, &change_ids, false)?;
                    if let Some(conflict) = undone
                        .iter()
                        .find(|row| row.status == FileChangeStatus::Conflict)
                    {
                        return Err(AppError::conflict(format!(
                            "{} changed since the agent edited it; undo stopped there",
                            conflict.path
                        )));
                    }
                }
            }
        }

        let downstream: HashSet<String> = downstream_dependents(&todos, &nid).into_iter().collect();
        let target_content = todos[target_idx].content.clone();
        let prev_error = todos[target_idx].error.clone();

        match action {
            NodeRecoveryAction::Retry | NodeRecoveryAction::UndoAndRetry => {
                let node = &mut todos[target_idx];
                node.status = AgentTodoStatus::Pending;
                node.attempt = node.attempt.saturating_add(1);
                node.error = None;
                node.run_id = None;
                for item in &mut todos {
                    if downstream.contains(&item.id)
                        && matches!(
                            item.status,
                            AgentTodoStatus::Failed
                                | AgentTodoStatus::Blocked
                                | AgentTodoStatus::Cancelled
                        )
                    {
                        item.status = AgentTodoStatus::Pending;
                        item.error = None;
                    }
                }
            }
            NodeRecoveryAction::Skip => {
                let node = &mut todos[target_idx];
                node.status = AgentTodoStatus::Cancelled;
                node.error = None;
                if node.summary.is_none() {
                    node.summary = Some("Skipped by user".to_owned());
                }
                for item in &mut todos {
                    if downstream.contains(&item.id) && item.status == AgentTodoStatus::Blocked {
                        item.status = AgentTodoStatus::Pending;
                    }
                }
            }
        }

        state
            .store()
            .save_session_todos(&sid, &todos)
            .map_err(command_error)?;

        let prompt = build_recovery_prompt(
            &todos,
            &nid,
            &target_content,
            action,
            prev_error.as_deref(),
            clean_guidance.as_deref(),
            &downstream,
        );
        Ok((prompt, todos))
    })
    .await??;

    let started = agent_run_start_impl(
        state,
        AgentRunInput {
            session_id: session_id.to_owned(),
            text: prompt_text,
            ..AgentRunInput::default()
        },
    )
    .await;

    if started.is_ok()
        && let Some(active) = state.agent.runs.active_for_session(session_id)
    {
        active.emit(crate::agent::model::AgentEvent::TodosUpdated {
            todos: updated_todos,
        });
    }
    started
}

fn build_recovery_prompt(
    todos: &[AgentTodoItem],
    target_id: &str,
    target_content: &str,
    action: NodeRecoveryAction,
    prev_error: Option<&str>,
    guidance: Option<&str>,
    downstream: &HashSet<String>,
) -> String {
    let mut lines = Vec::new();
    match action {
        NodeRecoveryAction::Retry => {
            lines.push(format!(
                "[Task Graph Recovery · Retry] Re-run node `{target_id}` ({target_content}) and any dependent downstream nodes without redoing already-completed upstream or sibling nodes."
            ));
        }
        NodeRecoveryAction::UndoAndRetry => {
            lines.push(format!(
                "[Task Graph Recovery · Undo & Retry] Pending file changes from the failed attempt have been rolled back. Re-run node `{target_id}` ({target_content}) and its dependent downstream nodes cleanly."
            ));
        }
        NodeRecoveryAction::Skip => {
            lines.push(format!(
                "[Task Graph Recovery · Skip] Node `{target_id}` ({target_content}) has been marked skipped. Continue with the remaining unblocked nodes in the Task Graph."
            ));
        }
    }

    if let Some(err) = prev_error {
        lines.push(format!("Previous error on `{target_id}`: {err}"));
    }
    if let Some(note) = guidance {
        lines.push(format!("User guidance: {note}"));
    }

    let completed: Vec<String> = todos
        .iter()
        .filter(|t| t.status == AgentTodoStatus::Completed)
        .map(|t| match &t.summary {
            Some(s) => format!("- `{}`: {} — {s}", t.id, t.content),
            None => format!("- `{}`: {}", t.id, t.content),
        })
        .collect();
    if !completed.is_empty() {
        lines.push(format!(
            "Already completed (reuse outputs, do not repeat):\n{}",
            completed.join("\n")
        ));
    }

    let remaining: Vec<String> = todos
        .iter()
        .filter(|t| t.id == target_id || downstream.contains(&t.id))
        .filter(|t| t.status != AgentTodoStatus::Cancelled)
        .map(|t| format!("- `{}`: {}", t.id, t.content))
        .collect();
    if !remaining.is_empty() {
        lines.push(format!(
            "Nodes to execute and update via `todo_write`:\n{}",
            remaining.join("\n")
        ));
    }

    lines.join("\n\n")
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::agent::model::AgentTodoStatus;

    #[test]
    fn recovery_prompt_preserves_completed_siblings_and_lists_downstream_scope() {
        let todos = vec![
            AgentTodoItem {
                id: "n1".to_owned(),
                content: "Audit router".to_owned(),
                status: AgentTodoStatus::Completed,
                summary: Some("Found 429 branch".to_owned()),
                ..AgentTodoItem::default()
            },
            AgentTodoItem {
                id: "n2".to_owned(),
                content: "Patch auth".to_owned(),
                status: AgentTodoStatus::Pending,
                depends_on: vec!["n1".to_owned()],
                attempt: 1,
                ..AgentTodoItem::default()
            },
            AgentTodoItem {
                id: "n3".to_owned(),
                content: "Run tests".to_owned(),
                status: AgentTodoStatus::Pending,
                depends_on: vec!["n2".to_owned()],
                ..AgentTodoItem::default()
            },
        ];
        let downstream: HashSet<String> = downstream_dependents(&todos, "n2").into_iter().collect();
        let prompt = build_recovery_prompt(
            &todos,
            "n2",
            "Patch auth",
            NodeRecoveryAction::Retry,
            Some("compile failed"),
            Some("use prepared statement"),
            &downstream,
        );
        assert!(prompt.contains("Audit router — Found 429 branch"));
        assert!(prompt.contains("User guidance: use prepared statement"));
        assert!(prompt.contains("`n2`: Patch auth"));
        assert!(prompt.contains("`n3`: Run tests"));
    }
}
