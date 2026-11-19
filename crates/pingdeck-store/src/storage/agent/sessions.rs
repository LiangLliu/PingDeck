//! Session rows: save, load, task list, usage rollups, the tombstone (soft delete, restore,
//! purge). The filtered and paged list lives in `sessions/list.rs`.

mod list;

use rusqlite::{Connection, OptionalExtension, params};

use pingdeck_model::agent::{AgentSession, AgentTarget, AgentTodoItem};
use pingdeck_model::hub::HubUsage;

use super::super::{StorageError, WorkspaceStore};
use super::{enum_text, parse_enum, to_i64};
use crate::sqlite::to_u64;

const SESSION_COLUMNS: &str = "id, title, target_json, workspace_dir, permission_mode, \
    reasoning_effort, disabled_tools_json, disabled_skills_json, web_enabled, title_locked, \
    head_message_id, pinned_at_ms, archived_at_ms, deleted_at_ms, forked_from, \
    forked_from_message_id, parent_session_id, parent_run_id, todos_json, total_usage_json, \
    total_cost_micro_usd, last_context_tokens, created_at_ms, updated_at_ms, \
    pingdeck_workspace_id, pingdeck_environment_id, \
    (SELECT name FROM workspaces WHERE id = agent_sessions.pingdeck_workspace_id), notice, \
    instructions";

/// A session row as SQLite returns it, before its JSON columns are parsed.
struct RawSession {
    id: String,
    title: String,
    target_json: Option<String>,
    workspace_dir: Option<String>,
    permission_mode: String,
    reasoning_effort: Option<String>,
    disabled_tools_json: String,
    disabled_skills_json: String,
    web_enabled: bool,
    title_locked: bool,
    head_message_id: Option<String>,
    pinned_at_ms: Option<i64>,
    archived_at_ms: Option<i64>,
    deleted_at_ms: Option<i64>,
    forked_from: Option<String>,
    forked_from_message_id: Option<String>,
    parent_session_id: Option<String>,
    parent_run_id: Option<String>,
    todos_json: String,
    total_usage_json: String,
    total_cost_micro_usd: i64,
    last_context_tokens: i64,
    created_at_ms: i64,
    updated_at_ms: i64,
    pingdeck_workspace_id: Option<String>,
    pingdeck_environment_id: Option<String>,
    pingdeck_workspace_name: Option<String>,
    notice: Option<String>,
    instructions: Option<String>,
}

fn session_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawSession> {
    Ok(RawSession {
        id: row.get(0)?,
        title: row.get(1)?,
        target_json: row.get(2)?,
        workspace_dir: row.get(3)?,
        permission_mode: row.get(4)?,
        reasoning_effort: row.get(5)?,
        disabled_tools_json: row.get(6)?,
        disabled_skills_json: row.get(7)?,
        web_enabled: row.get(8)?,
        title_locked: row.get(9)?,
        head_message_id: row.get(10)?,
        pinned_at_ms: row.get(11)?,
        archived_at_ms: row.get(12)?,
        deleted_at_ms: row.get(13)?,
        forked_from: row.get(14)?,
        forked_from_message_id: row.get(15)?,
        parent_session_id: row.get(16)?,
        parent_run_id: row.get(17)?,
        todos_json: row.get(18)?,
        total_usage_json: row.get(19)?,
        total_cost_micro_usd: row.get(20)?,
        last_context_tokens: row.get(21)?,
        created_at_ms: row.get(22)?,
        updated_at_ms: row.get(23)?,
        pingdeck_workspace_id: row.get(24)?,
        pingdeck_environment_id: row.get(25)?,
        pingdeck_workspace_name: row.get(26)?,
        notice: row.get(27)?,
        instructions: row.get(28)?,
    })
}

impl RawSession {
    fn into_domain(self) -> Result<AgentSession, StorageError> {
        Ok(AgentSession {
            permission_mode: parse_enum("permission_mode", &self.permission_mode)?,
            reasoning_effort: self
                .reasoning_effort
                .as_deref()
                .map(|text| parse_enum("reasoning_effort", text))
                .transpose()?,
            target: self
                .target_json
                .map(|json| serde_json::from_str::<AgentTarget>(&json))
                .transpose()?,
            disabled_tools: serde_json::from_str(&self.disabled_tools_json)?,
            disabled_skills: serde_json::from_str(&self.disabled_skills_json)?,
            todos: serde_json::from_str(&self.todos_json)?,
            total_usage: serde_json::from_str(&self.total_usage_json)?,
            last_context_tokens: to_u64(self.last_context_tokens),
            id: self.id,
            title: self.title,
            workspace_dir: self.workspace_dir,
            web_enabled: self.web_enabled,
            title_locked: self.title_locked,
            head_message_id: self.head_message_id,
            pinned_at_ms: self.pinned_at_ms,
            archived_at_ms: self.archived_at_ms,
            deleted_at_ms: self.deleted_at_ms,
            forked_from: self.forked_from,
            forked_from_message_id: self.forked_from_message_id,
            parent_session_id: self.parent_session_id,
            parent_run_id: self.parent_run_id,
            total_cost_micro_usd: self.total_cost_micro_usd,
            created_at_ms: self.created_at_ms,
            updated_at_ms: self.updated_at_ms,
            pingdeck_workspace_id: self.pingdeck_workspace_id,
            pingdeck_environment_id: self.pingdeck_environment_id,
            pingdeck_workspace_name: self.pingdeck_workspace_name,
            notice: self.notice,
            instructions: self.instructions,
        })
    }
}

/// Inserts a session, or updates the settings of an existing one. On insert every field is
/// written; on update only the settings are (title, model, folder, mode, effort, tool and skill
/// switches, web, title lock, pin, archive, own instructions, change time): the head, the
/// tombstone, the lineage, the task list, the usage rollups and the PingDeck binding are
/// maintained by their own queries, so a settings save from a stale copy cannot undo them.
pub(super) fn upsert_session(
    connection: &Connection,
    session: &AgentSession,
) -> Result<(), StorageError> {
    connection.execute(
        "
        INSERT INTO agent_sessions (
            id, title, target_json, workspace_dir, permission_mode, reasoning_effort,
            disabled_tools_json, disabled_skills_json, web_enabled, title_locked,
            head_message_id, pinned_at_ms, archived_at_ms, deleted_at_ms,
            forked_from, forked_from_message_id, parent_session_id, parent_run_id, todos_json,
            total_usage_json, total_cost_micro_usd, last_context_tokens, created_at_ms,
            updated_at_ms, pingdeck_workspace_id, pingdeck_environment_id, instructions
        ) VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14,
                  ?15, ?16, ?17, ?18, ?19, ?20, ?21, ?22, ?23, ?24, ?25, ?26, ?27)
        ON CONFLICT(id) DO UPDATE SET
            title = excluded.title,
            target_json = excluded.target_json,
            workspace_dir = excluded.workspace_dir,
            permission_mode = excluded.permission_mode,
            reasoning_effort = excluded.reasoning_effort,
            disabled_tools_json = excluded.disabled_tools_json,
            disabled_skills_json = excluded.disabled_skills_json,
            web_enabled = excluded.web_enabled,
            title_locked = excluded.title_locked,
            pinned_at_ms = excluded.pinned_at_ms,
            archived_at_ms = excluded.archived_at_ms,
            updated_at_ms = excluded.updated_at_ms,
            instructions = excluded.instructions
        ",
        params![
            session.id,
            session.title,
            session
                .target
                .as_ref()
                .map(serde_json::to_string)
                .transpose()?,
            session.workspace_dir,
            enum_text(&session.permission_mode)?,
            session
                .reasoning_effort
                .as_ref()
                .map(enum_text)
                .transpose()?,
            serde_json::to_string(&session.disabled_tools)?,
            serde_json::to_string(&session.disabled_skills)?,
            session.web_enabled,
            session.title_locked,
            session.head_message_id,
            session.pinned_at_ms,
            session.archived_at_ms,
            session.deleted_at_ms,
            session.forked_from,
            session.forked_from_message_id,
            session.parent_session_id,
            session.parent_run_id,
            serde_json::to_string(&session.todos)?,
            serde_json::to_string(&session.total_usage)?,
            session.total_cost_micro_usd,
            to_i64(session.last_context_tokens),
            session.created_at_ms,
            session.updated_at_ms,
            session.pingdeck_workspace_id,
            session.pingdeck_environment_id,
            session.instructions,
        ],
    )?;
    Ok(())
}

/// Loads one session row, tombstoned or not.
pub(super) fn load_session(
    connection: &Connection,
    id: &str,
) -> Result<Option<AgentSession>, StorageError> {
    connection
        .prepare_cached(&format!(
            "SELECT {SESSION_COLUMNS} FROM agent_sessions WHERE id = ?1"
        ))?
        .query_row(params![id], session_from_row)
        .optional()?
        .map(RawSession::into_domain)
        .transpose()
}

fn add_tokens(total: &mut Option<u64>, extra: Option<u64>) {
    if let Some(extra) = extra {
        *total = Some(total.unwrap_or(0).saturating_add(extra));
    }
}

impl WorkspaceStore {
    /// Upserts a session (not its messages). See [`upsert_session`] for what an update leaves
    /// alone.
    pub fn save_agent_session(&self, session: &AgentSession) -> Result<(), StorageError> {
        upsert_session(&self.connection, session)
    }

    /// Loads one session, including a tombstoned one (callers decide what a tombstone means).
    pub fn get_agent_session(&self, id: &str) -> Result<Option<AgentSession>, StorageError> {
        load_session(&self.connection, id)
    }

    /// Full access lasts until the app restarts: every chat in it drops to Auto-approve
    /// reads with the `full_access_off` note. Returns how many chats changed. A plain row update,
    /// safe to run at start-up before any chat opens.
    pub fn end_full_access(&self) -> Result<usize, StorageError> {
        Ok(self.connection.execute(
            "UPDATE agent_sessions SET permission_mode = 'auto_read_only', notice = 'full_access_off'
             WHERE permission_mode = 'auto_all'",
            [],
        )?)
    }

    /// Clears the one-time inline note of a chat; returns whether it had one.
    pub fn clear_session_notice(&self, session_id: &str) -> Result<bool, StorageError> {
        Ok(self.connection.execute(
            "UPDATE agent_sessions SET notice = NULL WHERE id = ?1 AND notice IS NOT NULL",
            params![session_id],
        )? > 0)
    }

    /// Puts the chat in or out of Plan mode (`agent_sessions.plan_mode`); returns whether the
    /// chat exists. A fork or a sub-agent chat is a new row and starts outside Plan.
    pub fn set_session_plan_mode(&self, session_id: &str, on: bool) -> Result<bool, StorageError> {
        Ok(self.connection.execute(
            "UPDATE agent_sessions SET plan_mode = ?2 WHERE id = ?1",
            params![session_id, on],
        )? > 0)
    }

    /// The chats in Plan mode that are not deleted, read once at start-up.
    pub fn plan_mode_sessions(&self) -> Result<Vec<String>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id FROM agent_sessions WHERE plan_mode = 1 AND deleted_at_ms IS NULL",
        )?;
        let rows = statement.query_map([], |row| row.get(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Replaces the task list of the branch the session's head is on; returns whether the session
    /// exists. The plan is kept per branch head: it is stored at the head message, a
    /// later message of the branch inherits it, and moving the head to another branch restores
    /// that branch's plan. The change time is left alone: the task list is progress, not a
    /// conversation change.
    pub fn save_session_todos(
        &self,
        session_id: &str,
        todos: &[AgentTodoItem],
    ) -> Result<bool, StorageError> {
        let json = serde_json::to_string(todos)?;
        self.atomic(|transaction| {
            let head: Option<Option<String>> = transaction
                .query_row(
                    "SELECT head_message_id FROM agent_sessions WHERE id = ?1",
                    params![session_id],
                    |row| row.get(0),
                )
                .optional()?;
            let Some(head) = head else {
                return Ok(false);
            };
            transaction.execute(
                "UPDATE agent_sessions SET todos_json = ?2 WHERE id = ?1",
                params![session_id, json],
            )?;
            if let Some(head) = head {
                transaction.execute(
                    "INSERT INTO agent_branch_plans (message_id, todos_json)
                     VALUES (?1, ?2)
                     ON CONFLICT(message_id) DO UPDATE SET todos_json = excluded.todos_json",
                    params![head, json],
                )?;
            }
            Ok(true)
        })
    }

    /// Adds one model call's `usage` and `cost_micro_usd` to the session's totals and records
    /// `last_context_tokens` (the call's prompt size); returns whether the session exists. A
    /// figure the session has not seen yet starts from the call's value; sums saturate.
    pub fn add_session_usage(
        &self,
        session_id: &str,
        usage: &HubUsage,
        cost_micro_usd: i64,
        last_context_tokens: u64,
    ) -> Result<bool, StorageError> {
        self.atomic(|transaction| {
            let Some((usage_json, total_cost)) = transaction
                .query_row(
                    "SELECT total_usage_json, total_cost_micro_usd FROM agent_sessions WHERE id = ?1",
                    params![session_id],
                    |row| Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?)),
                )
                .optional()?
            else {
                return Ok(false);
            };
            let mut total: HubUsage = serde_json::from_str(&usage_json)?;
            add_tokens(&mut total.input_tokens, usage.input_tokens);
            add_tokens(&mut total.output_tokens, usage.output_tokens);
            add_tokens(&mut total.cached_input_tokens, usage.cached_input_tokens);
            add_tokens(&mut total.cache_write_tokens, usage.cache_write_tokens);
            add_tokens(
                &mut total.cache_write_long_tokens,
                usage.cache_write_long_tokens,
            );
            add_tokens(&mut total.reasoning_tokens, usage.reasoning_tokens);
            transaction.execute(
                "UPDATE agent_sessions
                 SET total_usage_json = ?2, total_cost_micro_usd = ?3, last_context_tokens = ?4
                 WHERE id = ?1",
                params![
                    session_id,
                    serde_json::to_string(&total)?,
                    total_cost.saturating_add(cost_micro_usd),
                    to_i64(last_context_tokens),
                ],
            )?;
            Ok(true)
        })
    }

    /// Tombstones a session at `now_ms` together with the sub-agent sessions it spawned, in one
    /// transaction. Returns `false` when the session does not exist or is already tombstoned.
    /// Rows stay until [`WorkspaceStore::purge_deleted_sessions`].
    pub fn soft_delete_session(&self, id: &str, now_ms: i64) -> Result<bool, StorageError> {
        self.atomic(|transaction| {
            let changed = transaction.execute(
                "UPDATE agent_sessions SET deleted_at_ms = ?2 WHERE id = ?1 AND deleted_at_ms IS NULL",
                params![id, now_ms],
            )?;
            if changed > 0 {
                transaction.execute(
                    "UPDATE agent_sessions SET deleted_at_ms = ?2
                     WHERE parent_session_id = ?1 AND deleted_at_ms IS NULL",
                    params![id, now_ms],
                )?;
            }
            Ok(changed > 0)
        })
    }

    /// Clears a session's tombstone and that of the sub-agent sessions deleted with it; returns
    /// `false` when the session does not exist or is not tombstoned (a purged one is gone).
    pub fn restore_session(&self, id: &str) -> Result<bool, StorageError> {
        self.atomic(|transaction| {
            let deleted_at: Option<i64> = transaction
                .query_row(
                    "SELECT deleted_at_ms FROM agent_sessions WHERE id = ?1",
                    params![id],
                    |row| row.get(0),
                )
                .optional()?
                .flatten();
            let Some(deleted_at) = deleted_at else {
                return Ok(false);
            };
            transaction.execute(
                "UPDATE agent_sessions SET deleted_at_ms = NULL
                 WHERE id = ?1 OR (parent_session_id = ?1 AND deleted_at_ms = ?2)",
                params![id, deleted_at],
            )?;
            Ok(true)
        })
    }

    /// Permanently deletes every session tombstoned before `before_ms` with its messages, runs,
    /// queue, journal and rules (foreign-key cascades) and returns their ids so the caller can
    /// remove what lives outside the database (tool output, temp folders, attachment blobs).
    pub fn purge_deleted_sessions(&self, before_ms: i64) -> Result<Vec<String>, StorageError> {
        self.atomic(|transaction| {
            let ids: Vec<String> = transaction
                .prepare_cached(
                    "SELECT id FROM agent_sessions
                     WHERE deleted_at_ms IS NOT NULL AND deleted_at_ms < ?1 ORDER BY id",
                )?
                .query_map(params![before_ms], |row| row.get(0))?
                .collect::<Result<_, _>>()?;
            for id in &ids {
                transaction.execute("DELETE FROM agent_sessions WHERE id = ?1", params![id])?;
            }
            Ok(ids)
        })
    }
}
