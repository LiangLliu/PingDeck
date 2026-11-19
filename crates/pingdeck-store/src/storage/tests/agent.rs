//! Storage tests for the agent tables, one module per table family; this file holds the shared
//! fixtures.
//!
//! - `sessions`: session rows, settings saves, Plan mode, the tombstone, the filtered list, the
//!   one-time notice, the stop reason of a message and the PingDeck binding.
//! - `tree`: the message tree (append under the head, edit, retry, switch, delete, branch) and
//!   the plan of each branch.
//! - `messages`: partial rows, tool turns, model history, paging and message attachments.
//! - `search`: cross-chat search, the in-chat find and the index.
//! - `queue`: the per-session queue.
//! - `runs`: run rows and restart recovery.
//! - `board`: the run board, attention feed, pending changes and per-currency costs.
//! - `calls`: the tool-call audit, the file-change journal and tool policies.
//! - `rules`: permission rules.
//! - `mcp`: MCP servers, tool pins, review baselines and skill switches.
//! - `trust`: folder trust.

mod board;
mod calls;
mod mcp;
mod messages;
mod queue;
mod rules;
mod runs;
mod search;
mod sessions;
mod tree;
mod trust;

use pingdeck_model::agent::permissions::{PermissionAction, PermissionRule};
use pingdeck_model::agent::repair_orphan_tool_calls;
use pingdeck_model::agent::{
    AgentMessage, AgentMessageMeta, AgentRole, AgentRunKind, AgentRunStatus, AgentRunSummary,
    AgentSession, AgentSessionFilter, AgentTarget, AgentToolCallRecord, FileChange, FileChangeKind,
    FileChangeStatus,
};
use pingdeck_model::hub::{HubMessage, HubPart, HubRole, HubUsage};
use pingdeck_model::settings::ToolApproval;

use super::super::WorkspaceStore;

/// An in-memory store holding a session for each of `sessions`.
fn store_with(sessions: &[&str]) -> WorkspaceStore {
    let store = WorkspaceStore::open_in_memory().unwrap();
    for id in sessions {
        store.save_agent_session(&session(id, 1)).unwrap();
    }
    store
}

pub(super) fn session(id: &str, updated_at_ms: i64) -> AgentSession {
    AgentSession {
        id: id.to_owned(),
        title: format!("Session {id}"),
        target: Some(AgentTarget::Pinned {
            provider_id: "p".to_owned(),
            key_id: Some("k".to_owned()),
            model: "m".to_owned(),
        }),
        workspace_dir: None,
        permission_mode: ToolApproval::AutoReadOnly,
        reasoning_effort: None,
        disabled_tools: Vec::new(),
        disabled_skills: Vec::new(),
        web_enabled: false,
        title_locked: false,
        head_message_id: None,
        pinned_at_ms: None,
        archived_at_ms: None,
        deleted_at_ms: None,
        forked_from: None,
        forked_from_message_id: None,
        parent_session_id: None,
        parent_run_id: None,
        todos: Vec::new(),
        total_usage: HubUsage::default(),
        total_cost_micro_usd: 0,
        last_context_tokens: 0,
        pingdeck_workspace_id: None,
        pingdeck_environment_id: None,
        pingdeck_workspace_name: None,
        notice: None,
        instructions: None,
        created_at_ms: 1,
        updated_at_ms,
    }
}

pub(super) fn message(session_id: &str, id: &str, text: &str, at: i64) -> AgentMessage {
    AgentMessage {
        id: id.to_owned(),
        session_id: session_id.to_owned(),
        seq: 0,
        role: AgentRole::User,
        parts: vec![HubPart::text(text)],
        usage: None,
        model: None,
        error: None,
        run_id: None,
        parent_id: None,
        branch_index: 0,
        partial: false,
        cost_micro_usd: 0,
        cost_currency: None,
        meta: AgentMessageMeta::default(),
        branch_siblings: Vec::new(),
        attachments: Vec::new(),
        created_at_ms: at,
    }
}

fn assistant(session_id: &str, id: &str, text: &str, at: i64) -> AgentMessage {
    AgentMessage {
        role: AgentRole::Assistant,
        ..message(session_id, id, text, at)
    }
}

fn run(id: &str, session_id: &str, status: AgentRunStatus, started_at_ms: i64) -> AgentRunSummary {
    AgentRunSummary {
        run_id: id.to_owned(),
        session_id: session_id.to_owned(),
        kind: AgentRunKind::Main,
        parent_run_id: None,
        parent_call_id: None,
        status,
        model_label: "p/m".to_owned(),
        target: None,
        iterations: 0,
        usage: HubUsage::default(),
        cost_micro_usd: 0,
        started_at_ms,
        finished_at_ms: None,
        last_event_ms: None,
        last_step: None,
        stop_reason: None,
        error_kind: None,
        error: None,
        prompt_version: 1,
        last_seq: 0,
        failure: None,
        role: None,
        cost_currency: None,
        cap_inapplicable: None,
        status_detail: None,
    }
}

fn rule(id: &str, tool: &str, action: PermissionAction) -> PermissionRule {
    PermissionRule {
        id: id.to_owned(),
        session_id: None,
        workspace_dir: None,
        tool_pattern: tool.to_owned(),
        arg_pattern: None,
        action,
        created_at_ms: 10,
    }
}

/// A rule for the folder `/work`: the only scope besides a chat (global rules are refused).
fn folder_rule(id: &str, tool: &str, action: PermissionAction) -> PermissionRule {
    PermissionRule {
        workspace_dir: Some("/work".to_owned()),
        ..rule(id, tool, action)
    }
}

fn call(id: &str, name: &str) -> HubPart {
    HubPart::ToolCall {
        id: id.to_owned(),
        name: name.to_owned(),
        arguments: "{}".to_owned(),
        signature: None,
        origin: None,
    }
}

fn result(call_id: &str, text: &str, is_error: bool) -> HubPart {
    HubPart::ToolResult {
        call_id: call_id.to_owned(),
        name: Some("t".to_owned()),
        content: vec![HubPart::text(text)],
        is_error,
    }
}

fn assistant_calling(session_id: &str, id: &str, calls: &[(&str, &str)], at: i64) -> AgentMessage {
    AgentMessage {
        role: AgentRole::Assistant,
        parts: calls.iter().map(|(id, name)| call(id, name)).collect(),
        ..message(session_id, id, "", at)
    }
}

fn tool_shell(session_id: &str, id: &str, at: i64) -> AgentMessage {
    AgentMessage {
        role: AgentRole::Tool,
        parts: Vec::new(),
        ..message(session_id, id, "", at)
    }
}

fn count(store: &WorkspaceStore, table: &str) -> i64 {
    store
        .connection
        .query_row(&format!("SELECT COUNT(*) FROM {table}"), [], |row| {
            row.get(0)
        })
        .unwrap()
}

fn ids(messages: &[AgentMessage]) -> Vec<&str> {
    messages.iter().map(|m| m.id.as_str()).collect()
}

fn add_attachment(store: &WorkspaceStore, id: &str, name: &str) {
    store
        .connection
        .execute(
            "INSERT INTO agent_attachments
                 (id, sha256, name, media_type, size_bytes, kind, width, height, created_at_ms)
             VALUES (?1, ?2, ?3, 'image/png', 12, 'image', 4, 3, 7)",
            rusqlite::params![id, format!("sha-{id}"), name],
        )
        .unwrap();
}

/// A user message with text and one attachment reference.
fn with_attachment(mut message: AgentMessage, attachment_id: &str) -> AgentMessage {
    message.parts.push(HubPart::Image {
        image: pingdeck_model::hub::HubImage::AttachmentRef {
            attachment_id: attachment_id.to_owned(),
            media_type: "image/png".to_owned(),
        },
    });
    message
}

fn links(store: &WorkspaceStore, message_id: &str) -> Vec<String> {
    store
        .connection
        .prepare(
            "SELECT attachment_id FROM agent_message_attachments WHERE message_id = ?1 ORDER BY ord",
        )
        .unwrap()
        .query_map([message_id], |row| row.get(0))
        .unwrap()
        .collect::<Result<_, _>>()
        .unwrap()
}

fn page(store: &WorkspaceStore, filter: &AgentSessionFilter) -> Vec<String> {
    store
        .list_agent_sessions(filter, None, None)
        .unwrap()
        .sessions
        .into_iter()
        .map(|s| s.id)
        .collect()
}

fn tool_call(session_id: &str, call_id: &str, run_id: &str) -> AgentToolCallRecord {
    AgentToolCallRecord {
        session_id: session_id.to_owned(),
        call_id: call_id.to_owned(),
        run_id: run_id.to_owned(),
        message_id: None,
        name: "shell".to_owned(),
        class: "exec".to_owned(),
        state: "input-available".to_owned(),
        args_json: r#"{"cmd":"ls"}"#.to_owned(),
        risk: "low".to_owned(),
        decided_by: None,
        decision: None,
        decided_ms: None,
        started_ms: None,
        ended_ms: None,
        exit_code: None,
        sandbox: None,
        truncated: false,
        output_bytes: 0,
        spill_ref: None,
        requested_ms: None,
    }
}

fn change(session_id: &str, id: &str, run_id: &str, status: FileChangeStatus) -> FileChange {
    FileChange {
        id: id.to_owned(),
        session_id: session_id.to_owned(),
        run_id: run_id.to_owned(),
        call_id: "c1".to_owned(),
        path: format!("src/{id}.rs"),
        folder: Some("/work/dir".to_owned()),
        kind: FileChangeKind::Edit,
        before_sha256: Some("aa".to_owned()),
        after_sha256: Some("bb".to_owned()),
        added: 3,
        removed: 1,
        status,
        created_at_ms: 10,
    }
}

fn history_for_provider(store: &WorkspaceStore, session_id: &str) -> Vec<HubMessage> {
    let mut history: Vec<HubMessage> = store
        .list_model_history(session_id)
        .unwrap()
        .into_iter()
        .map(|m| HubMessage {
            role: match m.role {
                AgentRole::User | AgentRole::Summary => HubRole::User,
                AgentRole::Assistant => HubRole::Assistant,
                AgentRole::Tool => HubRole::Tool,
            },
            content: m.parts,
        })
        .collect();
    repair_orphan_tool_calls(&mut history);
    history
}

fn answered_calls(history: &[HubMessage]) -> Vec<Vec<String>> {
    history
        .iter()
        .filter(|m| m.role == HubRole::Tool)
        .map(|m| {
            m.content
                .iter()
                .filter_map(|p| match p {
                    HubPart::ToolResult { call_id, .. } => Some(call_id.clone()),
                    _ => None,
                })
                .collect()
        })
        .collect()
}

/// `n` turns `u<i>` / `a<i>` appended in order.
fn turns_of(store: &WorkspaceStore, session_id: &str, n: usize) {
    for i in 1..=n {
        let at = i64::try_from(i).unwrap() * 10;
        store
            .append_agent_message(&message(
                session_id,
                &format!("u{i}"),
                &format!("question {i}"),
                at,
            ))
            .unwrap();
        store
            .append_agent_message(&assistant(
                session_id,
                &format!("a{i}"),
                &format!("answer {i}"),
                at + 1,
            ))
            .unwrap();
    }
}
