//! Message rows: append, partial streaming rows, the crash-safe tool turn, the active path (read
//! and paged), model history and compaction summaries. Tree edits live in `tree.rs`; what a row
//! puts into the search index lives in `messages/indexing.rs`.

mod indexing;
mod query;

use rusqlite::{Connection, OptionalExtension, params};

use spanbay_model::agent::{AgentMessage, AgentMessageMeta, AgentRole, AgentStopReason};
use spanbay_model::hub::HubPart;
use spanbay_model::variables::uuid_v4;

use spanbay_base::time::now_ms;

use super::super::{StorageError, WorkspaceStore};
use super::invalid_data;
use super::sessions::load_session;
use super::tree::advance_head;
use crate::storage::attachments::link_attachments_on;
use indexing::{PENDING_RESULT_TEXT, attachment_ids, indexed_text, search_text};

pub(super) use query::load_message;

/// Active path from the session's head to the root: `?1` is the session id.
pub(super) const PATH_CTE: &str = "WITH RECURSIVE path(id, parent_id) AS (
        SELECT m.id, m.parent_id FROM agent_messages m
        WHERE m.session_id = ?1
          AND m.id = (SELECT head_message_id FROM agent_sessions WHERE id = ?1)
        UNION ALL
        SELECT m.id, m.parent_id FROM agent_messages m JOIN path ON m.id = path.parent_id
        WHERE m.session_id = ?1
    )";

/// The predicate of a turn boundary on an `agent_messages` row aliased `m`: a user message that
/// is a new instruction, not a summary, a steer or a message the app wrote.
pub(super) const BOUNDARY_SQL: &str = "(m.role = 'user'
    AND COALESCE(json_extract(m.meta_json, '$.steered'), 0) = 0
    AND COALESCE(json_extract(m.meta_json, '$.system'), 0) = 0)";

/// A new finished message with no usage, model, error, cost, siblings or attachments. `seq`,
/// `parent_id` and `branch_index` are placeholders: the insert assigns the stored values.
pub(super) fn new_message(
    session_id: &str,
    id: String,
    role: AgentRole,
    parts: Vec<HubPart>,
    run_id: Option<String>,
    meta: AgentMessageMeta,
    created_at_ms: i64,
) -> AgentMessage {
    AgentMessage {
        id,
        session_id: session_id.to_owned(),
        seq: 0,
        role,
        parts,
        usage: None,
        model: None,
        error: None,
        run_id,
        parent_id: None,
        branch_index: 0,
        partial: false,
        cost_micro_usd: 0,
        cost_currency: None,
        meta,
        branch_siblings: Vec::new(),
        attachments: Vec::new(),
        created_at_ms,
    }
}

/// The `seq` a new message of the session takes: one past the largest.
pub(super) fn next_seq(connection: &Connection, session_id: &str) -> Result<i64, StorageError> {
    Ok(connection
        .prepare_cached(
            "SELECT COALESCE(MAX(seq), 0) + 1 FROM agent_messages WHERE session_id = ?1",
        )?
        .query_row(params![session_id], |row| row.get(0))?)
}

/// The branch index a new child of `parent_id` takes: one past its last sibling's.
pub(super) fn next_branch_index(
    connection: &Connection,
    session_id: &str,
    parent_id: Option<&str>,
) -> Result<i64, StorageError> {
    Ok(connection
        .prepare_cached(
            "SELECT COALESCE(MAX(branch_index) + 1, 0) FROM agent_messages
             WHERE session_id = ?1 AND parent_id IS ?2",
        )?
        .query_row(params![session_id, parent_id], |row| row.get(0))?)
}

/// Bumps a session's change time to `at_ms` (never backwards).
pub(super) fn touch_session(
    connection: &Connection,
    session_id: &str,
    at_ms: i64,
) -> Result<(), StorageError> {
    connection
        .prepare_cached(
            "UPDATE agent_sessions SET updated_at_ms = MAX(updated_at_ms, ?2) WHERE id = ?1",
        )?
        .execute(params![session_id, at_ms])?;
    Ok(())
}

/// `(partial, session_id)` of the stored message `id`, `None` when there is none.
fn row_state(connection: &Connection, id: &str) -> Result<Option<(bool, String)>, StorageError> {
    Ok(connection
        .prepare_cached("SELECT partial, session_id FROM agent_messages WHERE id = ?1")?
        .query_row(params![id], |row| Ok((row.get(0)?, row.get(1)?)))
        .optional()?)
}

/// Links the attachments `parts` reference to `message_id`. A reference to an attachment that
/// does not exist fails on its foreign key, so the surrounding transaction rolls back and a
/// message can never point at a missing blob.
pub(super) fn link_part_attachments(
    connection: &Connection,
    message_id: &str,
    parts: &[HubPart],
) -> Result<(), StorageError> {
    link_attachments_on(connection, message_id, &attachment_ids(parts))
}

/// Inserts `message` as a new row at `seq` under `parent_id` with `branch_index`.
pub(super) fn insert_row(
    connection: &Connection,
    message: &AgentMessage,
    seq: i64,
    parent_id: Option<&str>,
    branch_index: i64,
) -> Result<(), StorageError> {
    connection.execute(
        "
        INSERT INTO agent_messages (
            id, session_id, seq, role, parts_json, usage_json, model, error, search_text,
            created_at_ms, run_id, cost_micro_usd, cost_currency, parent_id, branch_index,
            partial, meta_json
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17)
        ",
        params![
            message.id,
            message.session_id,
            seq,
            message.role.as_str(),
            serde_json::to_string(&message.parts)?,
            message
                .usage
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            message.model,
            message.error,
            indexed_text(message),
            message.created_at_ms,
            message.run_id,
            message.cost_micro_usd,
            message.cost_currency,
            parent_id,
            branch_index,
            message.partial,
            serde_json::to_string(&message.meta)?,
        ],
    )?;
    link_part_attachments(connection, &message.id, &message.parts)
}

/// Finishes the unfinished row of `message.id` in place: it keeps its `seq` and parent and takes
/// everything else from `message`.
fn finish_partial_in_place(
    connection: &Connection,
    message: &AgentMessage,
) -> Result<(), StorageError> {
    connection.execute(
        "
        UPDATE agent_messages
        SET role = ?2, parts_json = ?3, usage_json = ?4, model = ?5, error = ?6,
            search_text = ?7, run_id = ?8, cost_micro_usd = ?9,
            partial = ?10, meta_json = ?11, cost_currency = ?12
        WHERE id = ?1
        ",
        params![
            message.id,
            message.role.as_str(),
            serde_json::to_string(&message.parts)?,
            message
                .usage
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            message.model,
            message.error,
            indexed_text(message),
            message.run_id,
            message.cost_micro_usd,
            message.partial,
            serde_json::to_string(&message.meta)?,
            message.cost_currency,
        ],
    )?;
    link_part_attachments(connection, &message.id, &message.parts)
}

/// Inserts `message` as the newest child of the session's head (the head itself does not move).
fn insert_under_head(connection: &Connection, message: &AgentMessage) -> Result<(), StorageError> {
    let session_id = &message.session_id;
    let head = load_session(connection, session_id)?
        .ok_or_else(|| invalid_data(format!("chat session {session_id} not found")))?
        .head_message_id;
    let branch_index = next_branch_index(connection, session_id, head.as_deref())?;
    insert_row(
        connection,
        message,
        next_seq(connection, session_id)?,
        head.as_deref(),
        branch_index,
    )
}

/// Stores `message` under the session's current head and moves the head to it; returns the
/// stored row. When `message.id` is an unfinished (`partial`) row of the session, that row is
/// finished in place instead: it keeps its `seq` and parent, takes the new content and loses the
/// `partial` flag unless `message.partial` still says so.
fn store_at_head(
    connection: &Connection,
    message: &AgentMessage,
) -> Result<AgentMessage, StorageError> {
    let session_id = &message.session_id;
    match row_state(connection, &message.id)? {
        Some((true, owner)) if owner == *session_id => {
            finish_partial_in_place(connection, message)?;
        }
        Some(_) => {
            return Err(invalid_data(format!(
                "message {} is already stored",
                message.id
            )));
        }
        None => insert_under_head(connection, message)?,
    }
    advance_head(connection, session_id, &message.id)?;
    touch_session(connection, session_id, message.created_at_ms)?;
    load_message(connection, session_id, &message.id)?
        .ok_or_else(|| invalid_data(format!("message {} vanished", message.id)))
}

impl WorkspaceStore {
    /// Appends `message` to the session's active path: its parent is the current head, its
    /// `seq` and `branch_index` are assigned here (any values in `message` are ignored), the head
    /// moves to it and the session's change time to the message time. A message whose id is an
    /// unfinished row (see [`WorkspaceStore::upsert_partial_assistant`]) finishes that row in
    /// place. Attachments the parts reference are linked in the same transaction.
    pub fn append_agent_message(
        &self,
        message: &AgentMessage,
    ) -> Result<AgentMessage, StorageError> {
        self.atomic(|transaction| store_at_head(transaction, message))
    }

    /// Records why the run ended on the last assistant message it stored (`meta.stop_reason`).
    pub fn set_message_stop_reason(
        &self,
        message_id: &str,
        stop_reason: AgentStopReason,
    ) -> Result<(), StorageError> {
        self.connection.execute(
            "UPDATE agent_messages SET meta_json = json_set(meta_json, '$.stop_reason', ?2)
             WHERE id = ?1",
            params![message_id, stop_reason.as_str()],
        )?;
        Ok(())
    }

    /// Saves the in-flight assistant message `message_id` of `run_id` with the content so far:
    /// the first call inserts a `partial` row under the current head and moves the head to it,
    /// later calls replace its parts in place (nothing else changes, and the search index is not
    /// touched). Finish it with [`WorkspaceStore::append_agent_message`] or
    /// [`WorkspaceStore::append_assistant_with_pending_results`] using the same id. After a
    /// crash the row stays, marked `partial`. Fails when `message_id` is a finished message.
    pub fn upsert_partial_assistant(
        &self,
        session_id: &str,
        run_id: &str,
        message_id: &str,
        parts: Vec<HubPart>,
    ) -> Result<AgentMessage, StorageError> {
        self.atomic(|transaction| {
            match row_state(transaction, message_id)? {
                Some((true, owner)) if owner == session_id => {
                    transaction
                        .prepare_cached("UPDATE agent_messages SET parts_json = ?2 WHERE id = ?1")?
                        .execute(params![message_id, serde_json::to_string(&parts)?])?;
                    link_part_attachments(transaction, message_id, &parts)?;
                }
                Some(_) => {
                    return Err(invalid_data(format!(
                        "message {message_id} is not an unfinished message of this chat"
                    )));
                }
                // The insert returns the stored row: no need to read it again.
                None => {
                    let message = AgentMessage {
                        partial: true,
                        ..new_message(
                            session_id,
                            message_id.to_owned(),
                            AgentRole::Assistant,
                            parts,
                            Some(run_id.to_owned()),
                            AgentMessageMeta::default(),
                            now_ms(),
                        )
                    };
                    return store_at_head(transaction, &message);
                }
            }
            load_message(transaction, session_id, message_id)?
                .ok_or_else(|| invalid_data(format!("message {message_id} vanished")))
        })
    }

    /// Stores an assistant message that calls tools together with the tool message that will
    /// carry their results, in one transaction, so a crash can never leave a call without an
    /// answer. The assistant message follows the head (or finishes the unfinished row of its
    /// id), the tool message is its child, and the head ends on the tool message. `tool`'s own
    /// parts are ignored: it is stored with one placeholder `ToolResult` per `ToolCall` of
    /// `assistant`, in call order, each an error reading `[interrupted before tool finished]`.
    /// Replace them one by one with [`WorkspaceStore::update_tool_result_part`]. Both rows are
    /// stored finished (`partial = 0`). Returns both stored messages.
    pub fn append_assistant_with_pending_results(
        &self,
        assistant: &AgentMessage,
        tool: &AgentMessage,
    ) -> Result<(AgentMessage, AgentMessage), StorageError> {
        if assistant.role != AgentRole::Assistant
            || tool.role != AgentRole::Tool
            || assistant.session_id != tool.session_id
        {
            return Err(invalid_data(
                "a tool turn is an assistant message and a tool message of one session",
            ));
        }
        let placeholders: Vec<HubPart> = assistant
            .parts
            .iter()
            .filter_map(|part| match part {
                HubPart::ToolCall { id, name, .. } => Some(HubPart::ToolResult {
                    call_id: id.clone(),
                    name: Some(name.clone()),
                    content: vec![HubPart::text(PENDING_RESULT_TEXT)],
                    is_error: true,
                }),
                _ => None,
            })
            .collect();
        if placeholders.is_empty() {
            return Err(invalid_data("the assistant message calls no tool"));
        }
        let assistant = AgentMessage {
            partial: false,
            ..assistant.clone()
        };
        let tool = AgentMessage {
            parts: placeholders,
            partial: false,
            ..tool.clone()
        };
        self.atomic(|transaction| {
            let assistant = store_at_head(transaction, &assistant)?;
            let tool = store_at_head(transaction, &tool)?;
            Ok((assistant, tool))
        })
    }

    /// Replaces the `ToolResult` part answering `call_id` in message `message_id` with `result`
    /// (itself a `ToolResult` for `call_id`), in a transaction. Idempotent. Returns `false`
    /// when the message does not exist or holds no result for `call_id`.
    pub fn update_tool_result_part(
        &self,
        message_id: &str,
        call_id: &str,
        result: &HubPart,
    ) -> Result<bool, StorageError> {
        if !matches!(result, HubPart::ToolResult { call_id: id, .. } if id == call_id) {
            return Err(invalid_data(format!(
                "the replacement must be a tool result for call {call_id}"
            )));
        }
        self.atomic(|transaction| {
            let Some(parts_json) = transaction
                .query_row(
                    "SELECT parts_json FROM agent_messages WHERE id = ?1",
                    params![message_id],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
            else {
                return Ok(false);
            };
            let mut parts: Vec<HubPart> = serde_json::from_str(&parts_json)?;
            let Some(slot) = parts.iter_mut().find(
                |part| matches!(part, HubPart::ToolResult { call_id: id, .. } if id == call_id),
            ) else {
                return Ok(false);
            };
            *slot = result.clone();
            transaction.execute(
                "UPDATE agent_messages SET parts_json = ?2, search_text = ?3 WHERE id = ?1",
                params![
                    message_id,
                    serde_json::to_string(&parts)?,
                    search_text(&parts)
                ],
            )?;
            Ok(true)
        })
    }

    /// Appends the summary that replaces history up to and including `compacted_through` (a
    /// message of the session's active path) to the active path and returns it. The summary row
    /// records the cut as its `compacted_through_seq`; the replaced rows stay stored for display
    /// and [`WorkspaceStore::list_model_history`] skips them.
    pub fn append_compaction_summary(
        &self,
        session_id: &str,
        run_id: Option<&str>,
        parts: Vec<HubPart>,
        compacted_through: &str,
        skills: Vec<String>,
    ) -> Result<AgentMessage, StorageError> {
        self.atomic(|transaction| {
            let Some(through_seq) = transaction
                .query_row(
                    "SELECT seq FROM agent_messages WHERE id = ?1 AND session_id = ?2",
                    params![compacted_through, session_id],
                    |row| row.get::<_, i64>(0),
                )
                .optional()?
            else {
                return Err(invalid_data(format!(
                    "message {compacted_through} is not a message of chat {session_id}"
                )));
            };
            let summary = store_at_head(
                transaction,
                &new_message(
                    session_id,
                    format!("am_{}", uuid_v4()),
                    AgentRole::Summary,
                    parts,
                    run_id.map(str::to_owned),
                    AgentMessageMeta {
                        skills,
                        ..AgentMessageMeta::default()
                    },
                    now_ms(),
                ),
            )?;
            transaction.execute(
                "UPDATE agent_messages SET compacted_through_seq = ?2 WHERE id = ?1",
                params![summary.id, through_seq],
            )?;
            Ok(summary)
        })
    }

    /// Records the currency of the message's cost (`cost_micro_usd`), once a priced call was made.
    pub fn set_message_cost_currency(
        &self,
        message_id: &str,
        currency: &str,
    ) -> Result<(), StorageError> {
        self.connection.execute(
            "UPDATE agent_messages SET cost_currency = ?2 WHERE id = ?1",
            params![message_id, currency],
        )?;
        Ok(())
    }
}
