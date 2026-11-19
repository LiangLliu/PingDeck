//! Edits of the message tree: switching branches, edit and retry, deleting a subtree and
//! branching a path into a new session.
//!
//! Each message with children remembers the child that was last on the active path as
//! `active_child` in its `meta_json` (store-internal: [`AgentMessageMeta`] ignores the key), so
//! switching to a branch returns to where the user left it.
//!
//! Deleting a subtree lives in `tree/delete.rs`, branching a path into a new chat in
//! `tree/fork.rs`.
//!
//! [`AgentMessageMeta`]: pingdeck_model::agent::AgentMessageMeta

mod delete;
mod fork;

use rusqlite::{Connection, OptionalExtension, params};

use pingdeck_model::agent::{AgentMessage, AgentMessageMeta, AgentRole};
use pingdeck_model::hub::HubPart;
use pingdeck_model::variables::uuid_v4;

use pingdeck_base::time::now_ms;

use super::super::{StorageError, WorkspaceStore};
use super::invalid_data;
use super::messages::{
    insert_row, load_message, new_message, next_branch_index, next_seq, touch_session,
};

/// Makes `message_id` the session head and records the path to it as the remembered one.
pub(super) fn advance_head(
    connection: &Connection,
    session_id: &str,
    message_id: &str,
) -> Result<(), StorageError> {
    set_head_row(connection, session_id, Some(message_id))?;
    remember_chain(connection, message_id)
}

/// Points the session's head at `head` (`None`: an empty chat) without touching the remembered
/// path; the callers that also remember it go through [`advance_head`].
fn set_head_row(
    connection: &Connection,
    session_id: &str,
    head: Option<&str>,
) -> Result<(), StorageError> {
    connection
        .prepare_cached("UPDATE agent_sessions SET head_message_id = ?2 WHERE id = ?1")?
        .execute(params![session_id, head])?;
    Ok(())
}

/// Points each ancestor of `from_id`'s `active_child` along the path to it, stopping at the
/// first ancestor that already does (everything above it was recorded when the path before
/// this one was).
fn remember_chain(connection: &Connection, from_id: &str) -> Result<(), StorageError> {
    let mut parent_of =
        connection.prepare_cached("SELECT parent_id FROM agent_messages WHERE id = ?1")?;
    let mut remember = connection.prepare_cached(
        "UPDATE agent_messages
         SET meta_json = json_set(meta_json, '$.active_child', ?2)
         WHERE id = ?1 AND COALESCE(json_extract(meta_json, '$.active_child'), '') <> ?2",
    )?;
    let mut child = from_id.to_owned();
    loop {
        let parent: Option<Option<String>> = parent_of
            .query_row(params![child], |row| row.get(0))
            .optional()?;
        let Some(Some(parent)) = parent else {
            return Ok(());
        };
        let changed = remember.execute(params![parent, child])?;
        if changed == 0 {
            return Ok(());
        }
        child = parent;
    }
}

/// Sets the session's task list to the plan of the branch its head is on: the plan stored at the
/// head or, failing that, at the nearest ancestor that has one (none: an empty list). A plan
/// belongs to a branch, not to the chat, so every head move that changes the branch calls this.
pub(super) fn restore_plan(connection: &Connection, session_id: &str) -> Result<(), StorageError> {
    let head: Option<String> = connection
        .query_row(
            "SELECT head_message_id FROM agent_sessions WHERE id = ?1",
            params![session_id],
            |row| row.get(0),
        )
        .optional()?
        .flatten();
    let plan: Option<String> = match head {
        None => None,
        Some(head) => connection
            .query_row(
                "WITH RECURSIVE path(id, parent_id, depth) AS (
                     SELECT id, parent_id, 0 FROM agent_messages WHERE id = ?1
                     UNION ALL
                     SELECT m.id, m.parent_id, path.depth + 1
                     FROM agent_messages m JOIN path ON m.id = path.parent_id
                 )
                 SELECT p.todos_json FROM path JOIN agent_branch_plans p ON p.message_id = path.id
                 ORDER BY path.depth LIMIT 1",
                params![head],
                |row| row.get(0),
            )
            .optional()?,
    };
    connection.execute(
        "UPDATE agent_sessions SET todos_json = ?2 WHERE id = ?1",
        params![session_id, plan.unwrap_or_else(|| "[]".to_owned())],
    )?;
    Ok(())
}

/// The last message of the branch that starts at `start`: from each message the remembered
/// active child, or the newest child when none is remembered (or it is gone), until a message
/// has no children.
fn descend_to_leaf(
    connection: &Connection,
    session_id: &str,
    start: &str,
) -> Result<String, StorageError> {
    let mut current = start.to_owned();
    loop {
        let children: Vec<String> = connection
            .prepare_cached(
                "SELECT id FROM agent_messages WHERE session_id = ?1 AND parent_id = ?2
                 ORDER BY seq DESC",
            )?
            .query_map(params![session_id, current], |row| row.get(0))?
            .collect::<Result<_, _>>()?;
        let Some(newest) = children.first() else {
            return Ok(current);
        };
        let remembered: Option<String> = connection.query_row(
            "SELECT json_extract(meta_json, '$.active_child') FROM agent_messages WHERE id = ?1",
            params![current],
            |row| row.get(0),
        )?;
        current = match remembered {
            Some(id) if children.contains(&id) => id,
            _ => newest.clone(),
        };
    }
}

impl WorkspaceStore {
    /// Switches the active branch to the one containing `message_id`: the head becomes the last
    /// message of that branch (following each message's remembered child, else its newest, see
    /// the module docs) and the path to it is remembered. Returns the new head, or `None` when
    /// `message_id` is not a message of the session.
    pub fn set_head(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<Option<String>, StorageError> {
        self.atomic(|transaction| {
            let exists: bool = transaction.query_row(
                "SELECT EXISTS (SELECT 1 FROM agent_messages WHERE id = ?1 AND session_id = ?2)",
                params![message_id, session_id],
                |row| row.get(0),
            )?;
            if !exists {
                return Ok(None);
            }
            let head = descend_to_leaf(transaction, session_id, message_id)?;
            advance_head(transaction, session_id, &head)?;
            restore_plan(transaction, session_id)?;
            Ok(Some(head))
        })
    }

    /// Edit: stores a new user message with `parts` and `meta` as a sibling of `message_id`
    /// (same parent, next branch index) and makes it the head, so the next run answers it while
    /// the old branch stays intact. Only a user message that opens a turn can be edited. Linked
    /// attachments follow `parts`: carry the old `AttachmentRef`s over to keep them. Returns the
    /// new message, or `None` when `message_id` is not a message of the session.
    pub fn create_sibling_user_message(
        &self,
        session_id: &str,
        message_id: &str,
        parts: Vec<HubPart>,
        meta: AgentMessageMeta,
    ) -> Result<Option<AgentMessage>, StorageError> {
        self.atomic(|transaction| {
            let Some(old) = load_message(transaction, session_id, message_id)? else {
                return Ok(None);
            };
            if !old.is_turn_boundary() {
                return Err(invalid_data(
                    "only a user message that opens a turn can be edited",
                ));
            }
            let seq = next_seq(transaction, session_id)?;
            let now = now_ms();
            let message = new_message(
                session_id,
                format!("am_{}", uuid_v4()),
                AgentRole::User,
                parts,
                None,
                meta,
                now,
            );
            let branch_index =
                next_branch_index(transaction, session_id, old.parent_id.as_deref())?;
            insert_row(
                transaction,
                &message,
                seq,
                old.parent_id.as_deref(),
                branch_index,
            )?;
            advance_head(transaction, session_id, &message.id)?;
            restore_plan(transaction, session_id)?;
            touch_session(transaction, session_id, now)?;
            load_message(transaction, session_id, &message.id)?
                .ok_or_else(|| invalid_data("the edited message vanished"))
                .map(Some)
        })
    }

    /// Retry: finds the user message that opened the turn `message_id` belongs to (the message
    /// itself when it opens one; steer messages never do) and makes it the head, so the next
    /// assistant message becomes a new child of it, a sibling of the retried answer. Returns
    /// that user message, or `None` when `message_id` is not a message of the session. The old
    /// branch is untouched and reachable through `set_head`; a caller whose run then fails to
    /// start should restore the previous head the same way.
    pub fn prepare_retry(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<Option<AgentMessage>, StorageError> {
        self.atomic(|transaction| {
            let Some(mut current) = load_message(transaction, session_id, message_id)? else {
                return Ok(None);
            };
            while !current.is_turn_boundary() {
                let Some(parent) = current.parent_id.as_deref() else {
                    return Err(invalid_data("no user message opens this turn"));
                };
                current = load_message(transaction, session_id, parent)?
                    .ok_or_else(|| invalid_data("no user message opens this turn"))?;
            }
            set_head_row(transaction, session_id, Some(&current.id))?;
            restore_plan(transaction, session_id)?;
            Ok(Some(current))
        })
    }
}
