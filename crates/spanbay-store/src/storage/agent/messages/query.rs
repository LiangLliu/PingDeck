//! Message row mapping and read queries (single, session list, active path, paged windows,
//! branch siblings and compaction-aware model history).

use std::collections::HashMap;

use rusqlite::{Connection, Params, params};

use spanbay_model::agent::{AgentMessage, AgentPathWindow, AgentRole};

use super::{BOUNDARY_SQL, PATH_CTE};
use crate::sqlite::to_u32;
use crate::storage::agent::invalid;
use crate::storage::{StorageError, WorkspaceStore};

/// Column list of [`RawMessage`]; every query aliases `agent_messages` as `m`.
const MESSAGE_COLUMNS: &str = "m.id, m.session_id, m.seq, m.role, m.parts_json, \
    m.usage_json, m.model, m.error, m.created_at_ms, m.run_id, m.cost_micro_usd, \
    m.parent_id, m.branch_index, m.partial, m.meta_json, m.cost_currency";

/// `(seq, id)` of every message of a chat's active path, oldest first.
type PathIds = Vec<(i64, String)>;

/// A message row as SQLite returns it, before its JSON columns are parsed.
struct RawMessage {
    id: String,
    session_id: String,
    seq: i64,
    role: String,
    parts_json: String,
    usage_json: Option<String>,
    model: Option<String>,
    error: Option<String>,
    created_at_ms: i64,
    run_id: Option<String>,
    cost_micro_usd: i64,
    parent_id: Option<String>,
    branch_index: i64,
    partial: bool,
    meta_json: String,
    cost_currency: Option<String>,
}

fn message_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawMessage> {
    Ok(RawMessage {
        id: row.get(0)?,
        session_id: row.get(1)?,
        seq: row.get(2)?,
        role: row.get(3)?,
        parts_json: row.get(4)?,
        usage_json: row.get(5)?,
        model: row.get(6)?,
        error: row.get(7)?,
        created_at_ms: row.get(8)?,
        run_id: row.get(9)?,
        cost_micro_usd: row.get(10)?,
        parent_id: row.get(11)?,
        branch_index: row.get(12)?,
        partial: row.get(13)?,
        meta_json: row.get(14)?,
        cost_currency: row.get(15)?,
    })
}

impl RawMessage {
    fn into_domain(self) -> Result<AgentMessage, StorageError> {
        let role = AgentRole::parse(&self.role).ok_or_else(|| invalid("role", self.role))?;
        Ok(AgentMessage {
            role,
            id: self.id,
            session_id: self.session_id,
            seq: self.seq,
            parts: serde_json::from_str(&self.parts_json)?,
            usage: self
                .usage_json
                .map(|json| serde_json::from_str(&json))
                .transpose()?,
            model: self.model,
            error: self.error,
            run_id: self.run_id,
            parent_id: self.parent_id,
            branch_index: to_u32(self.branch_index),
            partial: self.partial,
            cost_micro_usd: self.cost_micro_usd,
            cost_currency: self.cost_currency,
            meta: serde_json::from_str(&self.meta_json)?,
            branch_siblings: Vec::new(),
            attachments: Vec::new(),
            created_at_ms: self.created_at_ms,
        })
    }
}

/// Runs a message query (`sql` selects [`MESSAGE_COLUMNS`]) and fills each result's
/// `branch_siblings`.
fn query_messages(
    connection: &Connection,
    sql: &str,
    params: impl Params,
) -> Result<Vec<AgentMessage>, StorageError> {
    let mut statement = connection.prepare_cached(sql)?;
    let rows = statement.query_map(params, message_from_row)?;
    let mut messages = rows
        .map(|row| row?.into_domain())
        .collect::<Result<Vec<_>, _>>()?;
    fill_branch_siblings(connection, &mut messages)?;
    Ok(messages)
}

/// Sets `branch_siblings` of every message that shares its parent with another one: the ids at
/// the node ordered by `branch_index`, the message itself included. A steer or a system message
/// has no switcher: it is not a turn boundary.
fn fill_branch_siblings(
    connection: &Connection,
    messages: &mut [AgentMessage],
) -> Result<(), StorageError> {
    let mut statement = connection.prepare_cached(
        "SELECT id FROM agent_messages WHERE session_id = ?1 AND parent_id IS ?2
         ORDER BY branch_index, seq",
    )?;
    let mut groups: HashMap<(String, Option<String>), Vec<String>> = HashMap::new();
    for message in messages.iter_mut() {
        if message.role == AgentRole::User && !message.is_turn_boundary() {
            continue;
        }
        let key = (message.session_id.clone(), message.parent_id.clone());
        if !groups.contains_key(&key) {
            let ids = statement
                .query_map(params![key.0, key.1], |row| row.get::<_, String>(0))?
                .collect::<Result<Vec<_>, _>>()?;
            groups.insert(key.clone(), ids);
        }
        if let Some(ids) = groups.get(&key).filter(|ids| ids.len() > 1) {
            message.branch_siblings = ids.clone();
        }
    }
    Ok(())
}

/// Loads one message, only if it belongs to `session_id`.
pub(in crate::storage::agent) fn load_message(
    connection: &Connection,
    session_id: &str,
    message_id: &str,
) -> Result<Option<AgentMessage>, StorageError> {
    Ok(query_messages(
        connection,
        &format!(
            "SELECT {MESSAGE_COLUMNS} FROM agent_messages m WHERE m.id = ?1 AND m.session_id = ?2"
        ),
        params![message_id, session_id],
    )?
    .pop())
}

/// The messages of the session's active path with `from_seq <= seq < to_seq` (no upper bound
/// when `to_seq` is `None`), oldest first.
fn path_range(
    connection: &Connection,
    session_id: &str,
    from_seq: i64,
    to_seq: Option<i64>,
) -> Result<Vec<AgentMessage>, StorageError> {
    query_messages(
        connection,
        &format!(
            "{PATH_CTE} SELECT {MESSAGE_COLUMNS} FROM agent_messages m
             JOIN path ON path.id = m.id
             WHERE m.seq >= ?2 AND (?3 IS NULL OR m.seq < ?3) ORDER BY m.seq"
        ),
        params![session_id, from_seq, to_seq],
    )
}

impl WorkspaceStore {
    /// Loads one message of a session, with its `branch_siblings`.
    pub fn get_agent_message(
        &self,
        session_id: &str,
        message_id: &str,
    ) -> Result<Option<AgentMessage>, StorageError> {
        load_message(&self.connection, session_id, message_id)
    }

    /// Every message of a session on every branch, in insertion order.
    pub fn list_agent_messages(&self, session_id: &str) -> Result<Vec<AgentMessage>, StorageError> {
        query_messages(
            &self.connection,
            &format!(
                "SELECT {MESSAGE_COLUMNS} FROM agent_messages m WHERE m.session_id = ?1
                 ORDER BY m.seq"
            ),
            params![session_id],
        )
    }

    /// The active path: the messages from the root to the session's head, oldest first. Empty
    /// for a session without a head.
    pub fn list_active_path(&self, session_id: &str) -> Result<Vec<AgentMessage>, StorageError> {
        query_messages(
            &self.connection,
            &format!(
                "{PATH_CTE} SELECT {MESSAGE_COLUMNS} FROM agent_messages m
                 JOIN path ON path.id = m.id ORDER BY m.seq"
            ),
            params![session_id],
        )
    }

    /// One page of the active path in ascending order: the newest `limit_turns` turns (a turn
    /// opens with a boundary user message and runs to the next one) among the messages whose
    /// `seq` is below `before_seq` (all of them when `None`), and whether older turns remain.
    /// Messages before the first boundary (a leading summary) belong to the first turn. A page
    /// ends where the previous page began: pass the first returned `seq` as `before_seq`.
    pub fn list_active_path_page(
        &self,
        session_id: &str,
        before_seq: Option<i64>,
        limit_turns: u32,
    ) -> Result<(Vec<AgentMessage>, bool), StorageError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "{PATH_CTE}
             SELECT m.seq, {BOUNDARY_SQL}
             FROM agent_messages m JOIN path ON path.id = m.id
             WHERE ?2 IS NULL OR m.seq < ?2
             ORDER BY m.seq"
        ))?;
        let entries = statement
            .query_map(params![session_id, before_seq], |row| {
                Ok((row.get::<_, i64>(0)?, row.get::<_, bool>(1)?))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let boundaries: Vec<i64> = entries
            .iter()
            .filter(|(_, boundary)| *boundary)
            .map(|(seq, _)| *seq)
            .collect();
        let keep = limit_turns.max(1) as usize;
        let (from_seq, has_more) = if boundaries.len() > keep {
            (boundaries[boundaries.len() - keep], true)
        } else {
            (i64::MIN, false)
        };
        let messages = path_range(&self.connection, session_id, from_seq, before_seq)?;
        Ok((messages, has_more))
    }

    /// The seq of the first message of every turn of the active path, oldest first (a leading
    /// summary belongs to the first turn, so the first entry is the path's first message), with
    /// the seq of every message of the path.
    fn turn_starts(&self, session_id: &str) -> Result<(Vec<i64>, PathIds), StorageError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "{PATH_CTE}
             SELECT m.seq, {BOUNDARY_SQL}, m.id
             FROM agent_messages m JOIN path ON path.id = m.id
             ORDER BY m.seq"
        ))?;
        let rows = statement
            .query_map(params![session_id], |row| {
                Ok((
                    row.get::<_, i64>(0)?,
                    row.get::<_, bool>(1)?,
                    row.get::<_, String>(2)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let mut starts: Vec<i64> = Vec::new();
        for (index, (seq, boundary, _)) in rows.iter().enumerate() {
            if *boundary || index == 0 {
                starts.push(*seq);
            }
        }
        let ids = rows.into_iter().map(|(seq, _, id)| (seq, id)).collect();
        Ok((starts, ids))
    }

    /// The messages of the active path whose turn index is in `first..end`.
    fn window(
        &self,
        session_id: &str,
        starts: &[i64],
        first: usize,
        end: usize,
    ) -> Result<AgentPathWindow, StorageError> {
        let from_seq = starts.get(first).copied().unwrap_or(i64::MAX);
        let to_seq = starts.get(end).copied();
        let messages = path_range(&self.connection, session_id, from_seq, to_seq)?;
        Ok(AgentPathWindow {
            messages,
            has_before: first > 0,
            has_after: end < starts.len(),
        })
    }

    /// The page of the active path around the turn that holds `message_id`: `limit_turns` whole
    /// turns, the turn of the message in the middle (as far as the path allows). One call opens
    /// a search hit however far back it is. `None` when the message is not on the active path.
    pub fn list_active_path_around(
        &self,
        session_id: &str,
        message_id: &str,
        limit_turns: u32,
    ) -> Result<Option<AgentPathWindow>, StorageError> {
        let (starts, ids) = self.turn_starts(session_id)?;
        let Some(seq) = ids
            .iter()
            .find(|(_, id)| id == message_id)
            .map(|(seq, _)| *seq)
        else {
            return Ok(None);
        };
        let limit = limit_turns.max(1) as usize;
        let turn = starts
            .partition_point(|start| *start <= seq)
            .saturating_sub(1);
        let end = (turn.saturating_sub(limit / 2) + limit).min(starts.len());
        let first = end.saturating_sub(limit);
        self.window(session_id, &starts, first, end).map(Some)
    }

    /// The next `limit_turns` turns of the active path after the message with `after_seq` (the
    /// last `seq` the caller holds): scrolling forward from a page opened around a turn.
    pub fn list_active_path_after(
        &self,
        session_id: &str,
        after_seq: i64,
        limit_turns: u32,
    ) -> Result<AgentPathWindow, StorageError> {
        let (starts, _) = self.turn_starts(session_id)?;
        let first = starts.partition_point(|start| *start <= after_seq);
        let end = (first + limit_turns.max(1) as usize).min(starts.len());
        self.window(session_id, &starts, first, end)
    }

    /// The messages the model sees: the latest compaction summary on the active path followed by
    /// every message of the path after the range it covers (a summary that records none covers
    /// everything before itself); the whole path when it holds no summary. Replaced rows and
    /// older summaries are left out.
    pub fn list_model_history(&self, session_id: &str) -> Result<Vec<AgentMessage>, StorageError> {
        let path = self.list_active_path(session_id)?;
        let Some(summary) = path.iter().rfind(|m| m.role == AgentRole::Summary).cloned() else {
            return Ok(path);
        };
        let covered: Option<i64> = self.connection.query_row(
            "SELECT compacted_through_seq FROM agent_messages WHERE id = ?1",
            params![summary.id],
            |row| row.get(0),
        )?;
        let covered = covered.unwrap_or(summary.seq);
        let mut history = vec![summary];
        history.extend(
            path.into_iter()
                .filter(|m| m.seq > covered && m.role != AgentRole::Summary),
        );
        Ok(history)
    }
}
