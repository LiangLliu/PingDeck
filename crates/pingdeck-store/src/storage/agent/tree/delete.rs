//! Deleting a message with its subtree, and the preview of what that removes.

use rusqlite::{Connection, OptionalExtension, params};

use pingdeck_model::agent::MessageDeletion;

use pingdeck_base::time::now_ms;

use super::{descend_to_leaf, remember_chain, restore_plan, set_head_row};
use crate::storage::agent::messages::touch_session;
use crate::storage::agent::sessions::load_session;
use crate::storage::{StorageError, WorkspaceStore};

/// The ids of `root` and its descendants within `session_id`; empty when `root` is not a
/// message of it.
fn subtree_ids(
    connection: &Connection,
    session_id: &str,
    root: &str,
) -> Result<Vec<String>, StorageError> {
    connection
        .prepare_cached(
            "WITH RECURSIVE sub(id) AS (
                 SELECT id FROM agent_messages WHERE id = ?2 AND session_id = ?1
                 UNION ALL
                 SELECT m.id FROM agent_messages m JOIN sub ON m.parent_id = sub.id
                 WHERE m.session_id = ?1
             ) SELECT id FROM sub",
        )?
        .query_map(params![session_id, root], |row| row.get(0))?
        .collect::<Result<_, _>>()
        .map_err(Into::into)
}

/// What deleting `ids` removes besides the messages: the ids of the runs that would be left with
/// no message, and the audit and journal rows that go with messages and runs.
fn deletion_effects(
    connection: &Connection,
    ids_json: &str,
) -> Result<(Vec<String>, MessageDeletion), StorageError> {
    let runs: Vec<String> = connection
        .prepare_cached(
            "SELECT DISTINCT m.run_id FROM agent_messages m
             WHERE m.id IN (SELECT value FROM json_each(?1)) AND m.run_id IS NOT NULL
               AND NOT EXISTS (
                   SELECT 1 FROM agent_messages o WHERE o.run_id = m.run_id
                     AND o.id NOT IN (SELECT value FROM json_each(?1)))",
        )?
        .query_map(params![ids_json], |row| row.get(0))?
        .collect::<Result<_, _>>()?;
    let runs_json = serde_json::to_string(&runs)?;
    let count = |sql: &str| -> Result<usize, StorageError> {
        let n: i64 = connection.query_row(sql, params![ids_json, runs_json], |row| row.get(0))?;
        Ok(usize::try_from(n).unwrap_or(0))
    };
    let tool_calls = count(
        "SELECT COUNT(*) FROM agent_tool_calls
         WHERE message_id IN (SELECT value FROM json_each(?1))
            OR run_id IN (SELECT value FROM json_each(?2))",
    )?;
    let file_changes = count(
        "SELECT COUNT(*) FROM agent_file_changes
         WHERE run_id IN (SELECT value FROM json_each(?2))",
    )?;
    let pending_file_changes = count(
        "SELECT COUNT(*) FROM agent_file_changes
         WHERE run_id IN (SELECT value FROM json_each(?2))
           AND status IN ('pending', 'conflict')",
    )?;
    // A leaf is a message of the subtree with no child: one per branch that ends below the root.
    let leaves: i64 = connection.query_row(
        "SELECT COUNT(*) FROM agent_messages m
         WHERE m.id IN (SELECT value FROM json_each(?1))
           AND NOT EXISTS (SELECT 1 FROM agent_messages c WHERE c.parent_id = m.id)",
        params![ids_json],
        |row| row.get(0),
    )?;
    let branches = usize::try_from(leaves).unwrap_or(0);
    let deletion = MessageDeletion {
        messages: serde_json::from_str::<Vec<String>>(ids_json)?.len(),
        runs: runs.len(),
        tool_calls,
        file_changes,
        pending_file_changes,
        branches,
    };
    Ok((runs, deletion))
}

/// Where the head goes when the subtree `ids` of `message_id` is deleted: `None` when the head
/// `head` is outside the subtree (it stays), else `Some` of the last message of the previous
/// sibling's branch, else of the next sibling's, else the parent (`None` at the root). Reads the
/// siblings and their branches, so it must run before anything is deleted.
fn head_after_deletion(
    connection: &Connection,
    session_id: &str,
    head: Option<&str>,
    message_id: &str,
    ids: &[String],
) -> Result<Option<Option<String>>, StorageError> {
    let Some(head) = head else {
        return Ok(None);
    };
    if !ids.iter().any(|id| id == head) {
        return Ok(None);
    }
    let (parent, branch_index): (Option<String>, i64) = connection.query_row(
        "SELECT parent_id, branch_index FROM agent_messages WHERE id = ?1",
        params![message_id],
        |row| Ok((row.get(0)?, row.get(1)?)),
    )?;
    let sibling: Option<String> = connection
        .query_row(
            "SELECT id FROM agent_messages
             WHERE session_id = ?1 AND parent_id IS ?2 AND id <> ?3
             ORDER BY branch_index < ?4 DESC,
                      CASE WHEN branch_index < ?4 THEN -branch_index
                           ELSE branch_index END
             LIMIT 1",
            params![session_id, parent, message_id, branch_index],
            |row| row.get(0),
        )
        .optional()?;
    Ok(Some(match sibling {
        Some(sibling) => Some(descend_to_leaf(connection, session_id, &sibling)?),
        None => parent,
    }))
}

/// Deletes the messages `ids_json` and the runs `runs_json` (JSON arrays of ids) with the
/// tool-call and file-change rows that go with them, children first.
fn delete_subtree_rows(
    connection: &Connection,
    ids_json: &str,
    runs_json: &str,
) -> Result<(), StorageError> {
    connection.execute(
        "DELETE FROM agent_tool_calls
         WHERE message_id IN (SELECT value FROM json_each(?1))
            OR run_id IN (SELECT value FROM json_each(?2))",
        params![ids_json, runs_json],
    )?;
    connection.execute(
        "DELETE FROM agent_file_changes WHERE run_id IN (SELECT value FROM json_each(?1))",
        params![runs_json],
    )?;
    connection.execute(
        "DELETE FROM agent_messages WHERE id IN (SELECT value FROM json_each(?1))",
        params![ids_json],
    )?;
    connection.execute(
        "DELETE FROM agent_runs WHERE id IN (SELECT value FROM json_each(?1))",
        params![runs_json],
    )?;
    Ok(())
}

impl WorkspaceStore {
    /// What [`WorkspaceStore::delete_message`] would remove, or `None` when `message_id` is not a
    /// message of the session.
    pub fn preview_message_deletion(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<Option<MessageDeletion>, StorageError> {
        let ids = subtree_ids(&self.connection, session_id, message_id)?;
        if ids.is_empty() {
            return Ok(None);
        }
        Ok(Some(
            deletion_effects(&self.connection, &serde_json::to_string(&ids)?)?.1,
        ))
    }

    /// Deletes a message with all its descendants, the runs left without any message and the
    /// tool-call and file-change rows of those (the files on disk stay), in one transaction.
    /// When the head was among the deleted messages it moves to the previous sibling's branch
    /// (else the next sibling's, else the parent; an empty chat has no head). Returns what was
    /// removed, or `None` when `message_id` is not a message of the session.
    pub fn delete_message(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<Option<MessageDeletion>, StorageError> {
        self.atomic(|transaction| {
            let ids = subtree_ids(transaction, session_id, message_id)?;
            if ids.is_empty() {
                return Ok(None);
            }
            let Some(session) = load_session(transaction, session_id)? else {
                return Ok(None);
            };
            let new_head = head_after_deletion(
                transaction,
                session_id,
                session.head_message_id.as_deref(),
                message_id,
                &ids,
            )?;
            let ids_json = serde_json::to_string(&ids)?;
            let (runs, effects) = deletion_effects(transaction, &ids_json)?;
            delete_subtree_rows(transaction, &ids_json, &serde_json::to_string(&runs)?)?;
            if let Some(head) = new_head {
                set_head_row(transaction, session_id, head.as_deref())?;
                if let Some(head) = head {
                    remember_chain(transaction, &head)?;
                }
                restore_plan(transaction, session_id)?;
            }
            touch_session(transaction, session_id, now_ms())?;
            Ok(Some(effects))
        })
    }
}
