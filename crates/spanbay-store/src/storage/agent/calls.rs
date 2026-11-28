//! The tool-call audit (`agent_tool_calls`), the file-change journal (`agent_file_changes`) and
//! the global per-tool policy (`agent_tool_policy`).

use rusqlite::{OptionalExtension, params};

use spanbay_model::agent::permissions::PermissionAction;
use spanbay_model::agent::{
    AgentToolCallRecord, AgentToolPolicy, FileChange, FileChangeKind, FileChangeStatus,
};

use super::super::{StorageError, WorkspaceStore};
use super::messages::BOUNDARY_SQL;
use super::{invalid, invalid_data, to_i64};
use crate::sqlite::{i64_from_usize, to_u32, to_u64};

/// Longest tool key of a policy, in bytes.
const MAX_TOOL_KEY: usize = 200;

const CALL_COLUMNS: &str = "session_id, call_id, run_id, message_id, name, class, state, \
    args_json, risk, decided_by, decision, decided_ms, started_ms, ended_ms, exit_code, \
    sandbox, truncated, output_bytes, spill_ref, requested_ms";

fn call_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentToolCallRecord> {
    Ok(AgentToolCallRecord {
        session_id: row.get(0)?,
        call_id: row.get(1)?,
        run_id: row.get(2)?,
        message_id: row.get(3)?,
        name: row.get(4)?,
        class: row.get(5)?,
        state: row.get(6)?,
        args_json: row.get(7)?,
        risk: row.get(8)?,
        decided_by: row.get(9)?,
        decision: row.get(10)?,
        decided_ms: row.get(11)?,
        started_ms: row.get(12)?,
        ended_ms: row.get(13)?,
        exit_code: row.get(14)?,
        sandbox: row.get(15)?,
        truncated: row.get(16)?,
        output_bytes: to_u64(row.get(17)?),
        spill_ref: row.get(18)?,
        requested_ms: row.get(19)?,
    })
}

const CHANGE_COLUMNS: &str = "id, session_id, run_id, call_id, path, kind, before_sha256, \
    after_sha256, added, removed, status, created_ms, folder";

fn change_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawChange> {
    Ok(RawChange {
        change: FileChange {
            id: row.get(0)?,
            session_id: row.get(1)?,
            run_id: row.get(2)?,
            call_id: row.get(3)?,
            path: row.get(4)?,
            folder: row.get(12)?,
            kind: FileChangeKind::Edit,
            before_sha256: row.get(6)?,
            after_sha256: row.get(7)?,
            added: to_u32(row.get(8)?),
            removed: to_u32(row.get(9)?),
            status: FileChangeStatus::Pending,
            created_at_ms: row.get(11)?,
        },
        kind: row.get(5)?,
        status: row.get(10)?,
    })
}

struct RawChange {
    change: FileChange,
    kind: String,
    status: String,
}

impl RawChange {
    fn into_domain(self) -> Result<FileChange, StorageError> {
        Ok(FileChange {
            kind: FileChangeKind::parse(&self.kind).ok_or_else(|| invalid("kind", self.kind))?,
            status: FileChangeStatus::parse(&self.status)
                .ok_or_else(|| invalid("status", self.status))?,
            ..self.change
        })
    }
}

impl WorkspaceStore {
    /// Inserts a tool call's audit row or, for a known `(session_id, call_id)`, updates it (the
    /// call moves through states: streaming, approval, done). `exit_code`, `truncated`,
    /// `output_bytes`, `spill_ref` and `decided_by` keep their stored value when the new row
    /// leaves them empty (`NULL`, or `0`/`false` for the two counters), so an earlier write is
    /// never erased by a later partial one; a later value still wins. `requested_ms` keeps the
    /// first value written (when the card first appeared). The session must exist.
    pub fn upsert_tool_call(&self, call: &AgentToolCallRecord) -> Result<(), StorageError> {
        self.connection.execute(
            &format!(
                "INSERT INTO agent_tool_calls ({CALL_COLUMNS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18, ?19, ?20)
                 ON CONFLICT(session_id, call_id) DO UPDATE SET
                     run_id = excluded.run_id, message_id = excluded.message_id,
                     name = excluded.name, class = excluded.class, state = excluded.state,
                     args_json = excluded.args_json, risk = excluded.risk,
                     decision = excluded.decision,
                     decided_ms = excluded.decided_ms, started_ms = excluded.started_ms,
                     ended_ms = excluded.ended_ms,
                     exit_code = COALESCE(excluded.exit_code, exit_code),
                     sandbox = excluded.sandbox,
                     truncated = MAX(excluded.truncated, truncated),
                     output_bytes = COALESCE(NULLIF(excluded.output_bytes, 0), output_bytes),
                     spill_ref = COALESCE(excluded.spill_ref, spill_ref),
                     decided_by = COALESCE(excluded.decided_by, decided_by),
                     requested_ms = COALESCE(requested_ms, excluded.requested_ms)"
            ),
            params![
                call.session_id,
                call.call_id,
                call.run_id,
                call.message_id,
                call.name,
                call.class,
                call.state,
                call.args_json,
                call.risk,
                call.decided_by,
                call.decision,
                call.decided_ms,
                call.started_ms,
                call.ended_ms,
                call.exit_code,
                call.sandbox,
                call.truncated,
                to_i64(call.output_bytes),
                call.spill_ref,
                call.requested_ms,
            ],
        )?;
        Ok(())
    }

    /// Records the spill handle of a tool call whose full output was later written to disk (for
    /// example, during watermark pruning) so `read_output` can read it back.
    pub fn set_tool_call_spill_ref(
        &self,
        session_id: &str,
        call_id: &str,
        spill_ref: &str,
    ) -> Result<(), StorageError> {
        let mut statement = self.connection.prepare_cached(
            "UPDATE agent_tool_calls
             SET spill_ref = COALESCE(spill_ref, ?3), truncated = 1
             WHERE session_id = ?1 AND call_id = ?2",
        )?;
        statement.execute(params![session_id, call_id, spill_ref])?;
        Ok(())
    }

    /// A session's tool calls in the order they were first recorded.
    pub fn list_tool_calls(
        &self,
        session_id: &str,
    ) -> Result<Vec<AgentToolCallRecord>, StorageError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {CALL_COLUMNS} FROM agent_tool_calls WHERE session_id = ?1 ORDER BY rowid"
        ))?;
        let rows = statement.query_map(params![session_id], call_from_row)?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Tool calls across `run_ids` in the order they were first recorded.
    pub fn list_runs_tool_calls(
        &self,
        run_ids: &[String],
    ) -> Result<Vec<AgentToolCallRecord>, StorageError> {
        if run_ids.is_empty() {
            return Ok(Vec::new());
        }
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {CALL_COLUMNS} FROM agent_tool_calls
             WHERE run_id IN (SELECT value FROM json_each(?1))
             ORDER BY rowid"
        ))?;
        let rows = statement.query_map(params![serde_json::to_string(run_ids)?], call_from_row)?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// The newest `limit` calls refused by a Block or a hard block (`decided_by` starts with
    /// `block:` or `hard_block:`), newest first, across chats.
    pub fn recent_blocked_calls(
        &self,
        limit: usize,
    ) -> Result<Vec<AgentToolCallRecord>, StorageError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {CALL_COLUMNS} FROM agent_tool_calls
             WHERE decided_by LIKE 'block:%' OR decided_by LIKE 'hard_block:%'
             ORDER BY rowid DESC LIMIT ?1"
        ))?;
        let rows = statement.query_map(params![i64_from_usize(limit)], call_from_row)?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// One tool call's audit row; `None` when the session never made that call.
    pub fn get_tool_call(
        &self,
        session_id: &str,
        call_id: &str,
    ) -> Result<Option<AgentToolCallRecord>, StorageError> {
        Ok(self
            .connection
            .prepare_cached(&format!(
                "SELECT {CALL_COLUMNS} FROM agent_tool_calls
                 WHERE session_id = ?1 AND call_id = ?2"
            ))?
            .query_row(params![session_id, call_id], call_from_row)
            .optional()?)
    }

    /// Appends a row to the file-change journal. The session must exist.
    pub fn insert_file_change(&self, change: &FileChange) -> Result<(), StorageError> {
        self.connection.execute(
            &format!(
                "INSERT INTO agent_file_changes ({CHANGE_COLUMNS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13)"
            ),
            params![
                change.id,
                change.session_id,
                change.run_id,
                change.call_id,
                change.path,
                change.kind.as_str(),
                change.before_sha256,
                change.after_sha256,
                change.added,
                change.removed,
                change.status.as_str(),
                change.created_at_ms,
                change.folder,
            ],
        )?;
        Ok(())
    }

    /// The journal of a chat and of the sub-agent sessions it spawned, oldest change first (undo
    /// walks it backwards). Rows still being applied ([`FileChangeStatus::Applying`]) are not
    /// listed.
    pub fn list_chat_file_changes(
        &self,
        session_id: &str,
    ) -> Result<Vec<FileChange>, StorageError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {CHANGE_COLUMNS} FROM agent_file_changes
             WHERE (session_id = ?1
                OR session_id IN (SELECT id FROM agent_sessions WHERE parent_session_id = ?1))
               AND status <> 'applying'
             ORDER BY created_ms, rowid"
        ))?;
        let rows = statement.query_map(params![session_id], change_from_row)?;
        rows.map(|row| row?.into_domain()).collect()
    }

    /// The journal rows named by `ids`, whichever session they belong to, oldest first; rows
    /// still being applied are not returned.
    pub fn get_file_changes(&self, ids: &[String]) -> Result<Vec<FileChange>, StorageError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {CHANGE_COLUMNS} FROM agent_file_changes
             WHERE id IN (SELECT value FROM json_each(?1)) AND status <> 'applying'
             ORDER BY created_ms, rowid"
        ))?;
        let rows = statement.query_map(params![serde_json::to_string(ids)?], change_from_row)?;
        rows.map(|row| row?.into_domain()).collect()
    }

    /// Sets the review state of the journal rows `ids` of `session_id`; returns how many
    /// existed in that session.
    pub fn set_file_change_status(
        &self,
        session_id: &str,
        ids: &[String],
        status: FileChangeStatus,
    ) -> Result<usize, StorageError> {
        Ok(self.connection.execute(
            "UPDATE agent_file_changes SET status = ?3
             WHERE session_id = ?1 AND id IN (SELECT value FROM json_each(?2))",
            params![session_id, serde_json::to_string(ids)?, status.as_str()],
        )?)
    }

    /// Updates the post-change hash, line counts, and status of one journal row after a partial
    /// (per-hunk) revert.
    pub fn update_file_change_after_revert(
        &self,
        session_id: &str,
        id: &str,
        after_sha256: Option<&str>,
        added: u32,
        removed: u32,
        status: FileChangeStatus,
    ) -> Result<bool, StorageError> {
        Ok(self.connection.execute(
            "UPDATE agent_file_changes
             SET after_sha256 = ?3, added = ?4, removed = ?5, status = ?6
             WHERE session_id = ?1 AND id = ?2",
            params![
                session_id,
                id,
                after_sha256,
                added,
                removed,
                status.as_str()
            ],
        )? > 0)
    }

    /// Flips a row written before its file was ([`FileChangeStatus::Applying`]) to
    /// [`FileChangeStatus::Pending`] once the file is in place; returns whether it was applying.
    pub fn finish_file_change(&self, id: &str) -> Result<bool, StorageError> {
        Ok(self.connection.execute(
            "UPDATE agent_file_changes SET status = 'pending'
             WHERE id = ?1 AND status = 'applying'",
            params![id],
        )? > 0)
    }

    /// Deletes one journal row (an applying row whose write failed, or one recovery found
    /// unwritten); returns whether it existed.
    pub fn delete_file_change(&self, id: &str) -> Result<bool, StorageError> {
        Ok(self
            .connection
            .execute("DELETE FROM agent_file_changes WHERE id = ?1", params![id])?
            > 0)
    }

    /// Every row still [`FileChangeStatus::Applying`], whatever the session, oldest first: what
    /// startup recovery looks at after a crash.
    pub fn list_applying_file_changes(&self) -> Result<Vec<FileChange>, StorageError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {CHANGE_COLUMNS} FROM agent_file_changes WHERE status = 'applying'
             ORDER BY created_ms, rowid"
        ))?;
        let rows = statement.query_map([], change_from_row)?;
        rows.map(|row| row?.into_domain()).collect()
    }

    /// The file changes made in the turn that `boundary_message_id` (the user message that
    /// opened it) starts: those of every run that stored a message in the turn, retried answers
    /// and sub-agent chats included, up to the next boundary. Oldest first. Empty when the
    /// message is not a message of the session; retry and edit-and-run ask this before they start.
    pub fn turn_file_changes(
        &self,
        session_id: &str,
        boundary_message_id: &str,
    ) -> Result<Vec<FileChange>, StorageError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "WITH RECURSIVE sub(id) AS (
                 SELECT id FROM agent_messages WHERE id = ?2 AND session_id = ?1
                 UNION ALL
                 SELECT m.id FROM agent_messages m JOIN sub ON m.parent_id = sub.id
                 WHERE m.session_id = ?1 AND NOT {BOUNDARY_SQL}
             ), runs(id) AS (
                 SELECT DISTINCT run_id FROM agent_messages
                 WHERE id IN (SELECT id FROM sub) AND run_id IS NOT NULL
                 UNION
                 SELECT run_id FROM agent_tool_calls
                 WHERE session_id = ?1 AND message_id IN (SELECT id FROM sub)
             )
             SELECT {CHANGE_COLUMNS} FROM agent_file_changes c
             WHERE c.status <> 'applying'
               AND ((c.session_id = ?1 AND c.run_id IN (SELECT id FROM runs))
                    OR c.session_id IN (SELECT id FROM agent_sessions
                                        WHERE parent_session_id = ?1
                                          AND parent_run_id IN (SELECT id FROM runs)))
             ORDER BY c.created_ms, c.rowid"
        ))?;
        let rows =
            statement.query_map(params![session_id, boundary_message_id], change_from_row)?;
        rows.map(|row| row?.into_domain()).collect()
    }

    /// The global per-tool policies, ordered by tool key.
    pub fn list_tool_policy(&self) -> Result<Vec<AgentToolPolicy>, StorageError> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT tool_key, policy FROM agent_tool_policy ORDER BY tool_key")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;
        let mut policies = Vec::new();
        for row in rows {
            let (tool_key, policy) = row?;
            policies.push(AgentToolPolicy {
                policy: PermissionAction::parse(&policy)
                    .ok_or_else(|| invalid("policy", policy))?,
                tool_key,
            });
        }
        Ok(policies)
    }

    /// Sets the policy of a tool, replacing the previous one. A tool key is 1 to 200 characters
    /// of letters, digits and `_ . : -`: one tool, never a pattern (an MCP tool is
    /// `mcp:<server id>:<tool>`).
    pub fn set_tool_policy(&self, policy: &AgentToolPolicy) -> Result<(), StorageError> {
        let key = &policy.tool_key;
        if key.is_empty()
            || key.len() > MAX_TOOL_KEY
            || !key
                .chars()
                .all(|c| c.is_ascii_alphanumeric() || matches!(c, '_' | '.' | ':' | '-'))
        {
            return Err(invalid_data(format!(
                "tool_key must be 1 to {MAX_TOOL_KEY} characters of letters, digits and _ . : -"
            )));
        }
        self.connection.execute(
            "INSERT INTO agent_tool_policy (tool_key, policy) VALUES (?1, ?2)
             ON CONFLICT(tool_key) DO UPDATE SET policy = excluded.policy",
            params![policy.tool_key, policy.policy.as_str()],
        )?;
        Ok(())
    }

    /// Removes the policies of every tool of MCP server `server_id` (`mcp:<id>:*`); returns how
    /// many.
    pub fn delete_mcp_tool_policies(&self, server_id: &str) -> Result<usize, StorageError> {
        Ok(self.connection.execute(
            "DELETE FROM agent_tool_policy WHERE substr(tool_key, 1, length(?1)) = ?1",
            params![format!("mcp:{server_id}:")],
        )?)
    }

    /// Removes a tool's policy (back to the default); returns whether it existed.
    pub fn delete_tool_policy(&self, tool_key: &str) -> Result<bool, StorageError> {
        Ok(self.connection.execute(
            "DELETE FROM agent_tool_policy WHERE tool_key = ?1",
            params![tool_key],
        )? > 0)
    }
}
