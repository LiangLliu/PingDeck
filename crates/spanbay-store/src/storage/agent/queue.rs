//! The per-session queue of messages waiting for the active run to end (`agent_queue`).

use rusqlite::{Connection, params};

use spanbay_model::agent::{MAX_QUEUED_TURNS, QueuedContent, QueuedTurn};
use spanbay_model::variables::uuid_v4;

use spanbay_base::time::now_ms;

use super::super::{StorageError, WorkspaceStore};
use super::require_session;
use crate::sqlite::to_u32;

fn turn_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<(QueuedTurn, String)> {
    Ok((
        QueuedTurn {
            id: row.get(0)?,
            session_id: row.get(1)?,
            ord: to_u32(row.get(2)?),
            parts: Vec::new(),
            skills: Vec::new(),
            contexts: Vec::new(),
            created_at_ms: row.get(4)?,
        },
        row.get(3)?,
    ))
}

fn list(connection: &Connection, session_id: &str) -> Result<Vec<QueuedTurn>, StorageError> {
    let mut statement = connection.prepare_cached(
        "SELECT id, session_id, ord, content_json, created_ms FROM agent_queue
         WHERE session_id = ?1 ORDER BY ord, id",
    )?;
    let rows = statement.query_map(params![session_id], turn_from_row)?;
    let mut turns = Vec::new();
    for row in rows {
        let (mut turn, content) = row?;
        let content: QueuedContent = serde_json::from_str(&content)?;
        (turn.parts, turn.skills, turn.contexts) =
            (content.parts, content.skills, content.contexts);
        turns.push(turn);
    }
    Ok(turns)
}

/// Rewrites `ord` as 0, 1, 2, … in the current order, so removal leaves no gap.
fn renumber(connection: &Connection, session_id: &str) -> Result<(), StorageError> {
    connection.execute(
        "UPDATE agent_queue SET ord = (
             SELECT COUNT(*) FROM agent_queue other
             WHERE other.session_id = agent_queue.session_id
               AND (other.ord < agent_queue.ord
                    OR (other.ord = agent_queue.ord AND other.id < agent_queue.id)))
         WHERE session_id = ?1",
        params![session_id],
    )?;
    Ok(())
}

impl WorkspaceStore {
    /// Queues `content` at the end of the session's queue. Returns `None` when the queue already
    /// holds [`MAX_QUEUED_TURNS`] messages; an unknown session is invalid.
    pub fn add_queued_turn(
        &self,
        session_id: &str,
        content: &QueuedContent,
    ) -> Result<Option<QueuedTurn>, StorageError> {
        self.atomic(|transaction| {
            require_session(transaction, session_id)?;
            let queued: i64 = transaction.query_row(
                "SELECT COUNT(*) FROM agent_queue WHERE session_id = ?1",
                params![session_id],
                |row| row.get(0),
            )?;
            if usize::try_from(queued).unwrap_or(usize::MAX) >= MAX_QUEUED_TURNS {
                return Ok(None);
            }
            let turn = QueuedTurn {
                id: format!("aq_{}", uuid_v4()),
                session_id: session_id.to_owned(),
                ord: to_u32(queued),
                parts: content.parts.clone(),
                skills: content.skills.clone(),
                contexts: content.contexts.clone(),
                created_at_ms: now_ms(),
            };
            transaction.execute(
                "INSERT INTO agent_queue (id, session_id, ord, content_json, created_ms)
                 VALUES (?1, ?2, ?3, ?4, ?5)",
                params![
                    turn.id,
                    turn.session_id,
                    turn.ord,
                    serde_json::to_string(content)?,
                    turn.created_at_ms,
                ],
            )?;
            Ok(Some(turn))
        })
    }

    /// Replaces the content of a queued message; returns whether it exists in that session.
    pub fn update_queued_turn(
        &self,
        session_id: &str,
        id: &str,
        content: &QueuedContent,
    ) -> Result<bool, StorageError> {
        Ok(self.connection.execute(
            "UPDATE agent_queue SET content_json = ?3 WHERE id = ?2 AND session_id = ?1",
            params![session_id, id, serde_json::to_string(content)?],
        )? > 0)
    }

    /// Puts `content` back at the front of the session's queue (a message whose start failed, a
    /// steer the run did not take). Never refused for a full queue: nothing the user wrote is lost.
    pub fn requeue_front(
        &self,
        session_id: &str,
        content: &QueuedContent,
    ) -> Result<(), StorageError> {
        self.atomic(|transaction| {
            transaction.execute(
                "UPDATE agent_queue SET ord = ord + 1 WHERE session_id = ?1",
                params![session_id],
            )?;
            transaction.execute(
                "INSERT INTO agent_queue (id, session_id, ord, content_json, created_ms)
                 VALUES (?1, ?2, 0, ?3, ?4)",
                params![
                    format!("aq_{}", uuid_v4()),
                    session_id,
                    serde_json::to_string(content)?,
                    now_ms()
                ],
            )?;
            Ok(())
        })
    }

    pub fn sessions_with_queue(&self) -> Result<Vec<String>, StorageError> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT DISTINCT session_id FROM agent_queue ORDER BY session_id")?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Removes a queued message and closes the gap; returns whether it existed in that session.
    pub fn remove_queued_turn(&self, session_id: &str, id: &str) -> Result<bool, StorageError> {
        self.atomic(|transaction| {
            let removed = transaction.execute(
                "DELETE FROM agent_queue WHERE id = ?2 AND session_id = ?1",
                params![session_id, id],
            )? > 0;
            if removed {
                renumber(transaction, session_id)?;
            }
            Ok(removed)
        })
    }

    /// Removes queued message `id` of the chat and returns it, in one transaction.
    pub fn take_queued_turn(
        &self,
        session_id: &str,
        id: &str,
    ) -> Result<Option<QueuedTurn>, StorageError> {
        self.atomic(|transaction| {
            let Some(turn) = list(transaction, session_id)?
                .into_iter()
                .find(|turn| turn.id == id)
            else {
                return Ok(None);
            };
            transaction.execute("DELETE FROM agent_queue WHERE id = ?1", params![turn.id])?;
            renumber(transaction, session_id)?;
            Ok(Some(turn))
        })
    }

    /// Empties a session's queue; returns how many messages it held.
    pub fn clear_queue(&self, session_id: &str) -> Result<usize, StorageError> {
        Ok(self.connection.execute(
            "DELETE FROM agent_queue WHERE session_id = ?1",
            params![session_id],
        )?)
    }

    /// A session's queued messages, first to run first.
    pub fn list_queue(&self, session_id: &str) -> Result<Vec<QueuedTurn>, StorageError> {
        list(&self.connection, session_id)
    }

    /// Removes and returns the message that runs next, closing the gap.
    pub fn pop_queue_front(&self, session_id: &str) -> Result<Option<QueuedTurn>, StorageError> {
        self.atomic(|transaction| {
            let Some(front) = list(transaction, session_id)?.into_iter().next() else {
                return Ok(None);
            };
            transaction.execute("DELETE FROM agent_queue WHERE id = ?1", params![front.id])?;
            renumber(transaction, session_id)?;
            Ok(Some(front))
        })
    }
}
