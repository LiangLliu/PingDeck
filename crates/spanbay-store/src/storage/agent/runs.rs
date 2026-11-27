//! Run rows (`agent_runs`): background-run bookkeeping, crash recovery and usage attribution.

use rusqlite::{OptionalExtension, params};

use spanbay_model::agent::{AgentRunKind, AgentRunStatus, AgentRunSummary, SubagentRole};

use super::super::{StorageError, WorkspaceStore};
use super::invalid;
use crate::sqlite::to_u32;

/// SQL list of the statuses for which [`AgentRunStatus::is_active`] holds, shared by the run,
/// session-list and board queries. Two tests keep it in step with `is_active`:
/// `restart_recovery_and_activity_queries_agree_for_all_run_statuses` and
/// `the_unseen_lane_holds_exactly_the_runs_that_are_not_active`.
pub(super) const ACTIVE_RUN_STATUSES: &str = "('queued', 'running', 'waiting_approval')";

const RUN_COLUMNS: &str = "id, session_id, parent_run_id, status, model_label, iterations, \
    usage_json, cost_micro_usd, started_at_ms, finished_at_ms, error, parent_call_id, kind, \
    last_event_ms, stop_reason, error_kind, target_json, last_step, failure_json, \
    role, cost_currency";

/// A run row before its JSON and enum columns are parsed.
struct RawRun {
    run_id: String,
    session_id: String,
    parent_run_id: Option<String>,
    status: String,
    model_label: String,
    iterations: i64,
    usage_json: String,
    cost_micro_usd: i64,
    started_at_ms: i64,
    finished_at_ms: Option<i64>,
    error: Option<String>,
    parent_call_id: Option<String>,
    kind: String,
    last_event_ms: Option<i64>,
    stop_reason: Option<String>,
    error_kind: Option<String>,
    target_json: Option<String>,
    last_step: Option<String>,
    failure_json: Option<String>,
    role: Option<String>,
    cost_currency: Option<String>,
}

fn run_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RawRun> {
    Ok(RawRun {
        run_id: row.get(0)?,
        session_id: row.get(1)?,
        parent_run_id: row.get(2)?,
        status: row.get(3)?,
        model_label: row.get(4)?,
        iterations: row.get(5)?,
        usage_json: row.get(6)?,
        cost_micro_usd: row.get(7)?,
        started_at_ms: row.get(8)?,
        finished_at_ms: row.get(9)?,
        error: row.get(10)?,
        parent_call_id: row.get(11)?,
        kind: row.get(12)?,
        last_event_ms: row.get(13)?,
        stop_reason: row.get(14)?,
        error_kind: row.get(15)?,
        target_json: row.get(16)?,
        last_step: row.get(17)?,
        failure_json: row.get(18)?,
        role: row.get(19)?,
        cost_currency: row.get(20)?,
    })
}

impl RawRun {
    fn into_domain(self) -> Result<AgentRunSummary, StorageError> {
        Ok(AgentRunSummary {
            status: AgentRunStatus::parse(&self.status)
                .ok_or_else(|| invalid("status", self.status))?,
            kind: AgentRunKind::parse(&self.kind).ok_or_else(|| invalid("kind", self.kind))?,
            target: self
                .target_json
                .map(|json| serde_json::from_str(&json))
                .transpose()?,
            run_id: self.run_id,
            session_id: self.session_id,
            parent_run_id: self.parent_run_id,
            parent_call_id: self.parent_call_id,
            model_label: self.model_label,
            iterations: to_u32(self.iterations),
            usage: serde_json::from_str(&self.usage_json)?,
            cost_micro_usd: self.cost_micro_usd,
            started_at_ms: self.started_at_ms,
            finished_at_ms: self.finished_at_ms,
            last_event_ms: self.last_event_ms,
            last_step: self.last_step,
            stop_reason: self.stop_reason,
            error_kind: self.error_kind,
            error: self.error,
            last_seq: 0,
            failure: self
                .failure_json
                .map(|json| serde_json::from_str(&json))
                .transpose()?,
            // An unknown role (a newer build's) reads as none rather than failing the row.
            role: self.role.as_deref().and_then(SubagentRole::parse),
            cost_currency: self.cost_currency,
            cap_inapplicable: None,
            status_detail: None,
        })
    }
}

impl WorkspaceStore {
    /// Stores a new run row. `last_seq` is not persisted.
    pub fn insert_agent_run(&self, run: &AgentRunSummary) -> Result<(), StorageError> {
        self.connection.execute(
            &format!(
                "INSERT INTO agent_runs ({RUN_COLUMNS})
                 VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16,
                         ?17, ?18, ?19, ?20, ?21)"
            ),
            params![
                run.run_id,
                run.session_id,
                run.parent_run_id,
                run.status.as_str(),
                run.model_label,
                run.iterations,
                serde_json::to_string(&run.usage)?,
                run.cost_micro_usd,
                run.started_at_ms,
                run.finished_at_ms,
                run.error,
                run.parent_call_id,
                run.kind.as_str(),
                run.last_event_ms,
                run.stop_reason,
                run.error_kind,
                run.target.as_ref().map(serde_json::to_string).transpose()?,
                run.last_step,
                run.failure
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                run.role.map(SubagentRole::as_str),
                run.cost_currency,
            ],
        )?;
        Ok(())
    }

    /// Writes a run's progress: status, model label and target, iterations, usage, cost and its
    /// currency, end time, last event and step, stop reason, error kind and text.
    /// The session, kind, role, parent links and start time never change. Returns whether the run
    /// exists.
    pub fn update_agent_run(&self, run: &AgentRunSummary) -> Result<bool, StorageError> {
        Ok(self.connection.execute(
            "
            UPDATE agent_runs
            SET status = ?2, model_label = ?3, iterations = ?4, usage_json = ?5,
                cost_micro_usd = ?6, finished_at_ms = ?7, error = ?8, last_event_ms = ?9,
                stop_reason = ?10, error_kind = ?11, target_json = ?12,
                last_step = ?13, failure_json = ?14, cost_currency = ?15
            WHERE id = ?1
            ",
            params![
                run.run_id,
                run.status.as_str(),
                run.model_label,
                run.iterations,
                serde_json::to_string(&run.usage)?,
                run.cost_micro_usd,
                run.finished_at_ms,
                run.error,
                run.last_event_ms,
                run.stop_reason,
                run.error_kind,
                run.target.as_ref().map(serde_json::to_string).transpose()?,
                run.last_step,
                run.failure
                    .as_ref()
                    .map(serde_json::to_string)
                    .transpose()?,
                run.cost_currency,
            ],
        )? > 0)
    }

    pub fn get_agent_run(&self, id: &str) -> Result<Option<AgentRunSummary>, StorageError> {
        self.connection
            .prepare_cached(&format!(
                "SELECT {RUN_COLUMNS} FROM agent_runs WHERE id = ?1"
            ))?
            .query_row(params![id], run_from_row)
            .optional()?
            .map(RawRun::into_domain)
            .transpose()
    }

    /// Lists runs, newest start first, at most `limit`: of one session or of all, only the
    /// active ones when `active_only`.
    pub fn list_agent_runs(
        &self,
        session_id: Option<&str>,
        active_only: bool,
        limit: u32,
    ) -> Result<Vec<AgentRunSummary>, StorageError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "
            SELECT {RUN_COLUMNS} FROM agent_runs
            WHERE (?1 IS NULL OR session_id = ?1)
              AND (?2 = 0 OR status IN {ACTIVE_RUN_STATUSES})
            ORDER BY started_at_ms DESC, id
            LIMIT ?3
            "
        ))?;
        let rows = statement.query_map(params![session_id, active_only, limit], run_from_row)?;
        rows.map(|row| row?.into_domain()).collect()
    }

    /// Lists a chat's top-level (`main`) runs, newest start first, at most `limit`.
    pub fn list_session_main_runs(
        &self,
        session_id: &str,
        limit: u32,
    ) -> Result<Vec<AgentRunSummary>, StorageError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {RUN_COLUMNS} FROM agent_runs
             WHERE session_id = ?1 AND kind = 'main'
             ORDER BY started_at_ms DESC, id
             LIMIT ?2"
        ))?;
        let rows = statement.query_map(params![session_id, limit], run_from_row)?;
        rows.map(|row| row?.into_domain()).collect()
    }

    /// Lists all child (`subagent`) runs spawned by `parent_run_id`, oldest start first, paired
    /// with the child session's title (the `task` tool's `description`).
    pub fn list_child_agent_runs(
        &self,
        parent_run_id: &str,
    ) -> Result<Vec<(AgentRunSummary, String)>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT r.id, r.session_id, r.parent_run_id, r.status, r.model_label, r.iterations, \
                    r.usage_json, r.cost_micro_usd, r.started_at_ms, r.finished_at_ms, r.error, \
                    r.parent_call_id, r.kind, r.last_event_ms, r.stop_reason, r.error_kind, \
                    r.target_json, r.last_step, r.failure_json, r.role, \
                    r.cost_currency, COALESCE(s.title, '') \
             FROM agent_runs r \
             LEFT JOIN agent_sessions s ON s.id = r.session_id \
             WHERE r.parent_run_id = ?1 \
             ORDER BY r.started_at_ms ASC, r.id ASC",
        )?;
        let rows = statement.query_map(params![parent_run_id], |row| {
            Ok((run_from_row(row)?, row.get::<_, String>(21)?))
        })?;
        rows.map(|row| {
            let (raw, title) = row?;
            Ok((raw.into_domain()?, title))
        })
        .collect()
    }

    /// Whether `session_id` has a queued, running or approval-waiting run.
    pub fn has_active_agent_run(&self, session_id: &str) -> Result<bool, StorageError> {
        Ok(self.connection.query_row(
            &format!(
                "SELECT EXISTS (SELECT 1 FROM agent_runs
                                WHERE session_id = ?1 AND status IN {ACTIVE_RUN_STATUSES})"
            ),
            params![session_id],
            |row| row.get(0),
        )?)
    }

    /// Marks every run a previous process left `queued`, `running` or `waiting_approval` as
    /// `interrupted` (nothing is executing it any more) with the stop reason `interrupted`;
    /// returns how many it changed. Their unfinished assistant rows stay `partial`.
    pub fn interrupt_unfinished_agent_runs(&self, now_ms: i64) -> Result<usize, StorageError> {
        Ok(self.connection.execute(
            &format!(
                "UPDATE agent_runs
                 SET status = 'interrupted', finished_at_ms = ?1, error = 'application restarted',
                     stop_reason = 'interrupted'
                 WHERE status IN {ACTIVE_RUN_STATUSES}"
            ),
            params![now_ms],
        )?)
    }
}
