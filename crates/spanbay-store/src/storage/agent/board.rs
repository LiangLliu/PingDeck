//! Queries for the run board, the attention feed, pending file changes and per-currency costs.
//!
//! Run summaries live in `runs.rs`; this module holds the views derived from them and the run
//! columns a summary does not carry ([`RunExtras`]).

use std::collections::HashMap;

use rusqlite::params;

use spanbay_model::agent::{CurrencyCost, CurrencyTotal};
use spanbay_model::hub::HubPart;

use super::super::{StorageError, WorkspaceStore};
use super::messages::PATH_CTE;
use super::runs::ACTIVE_RUN_STATUSES;
use crate::sqlite::{to_i64, to_u32, to_u64};

/// What the run lifecycle keeps about a run besides its summary row.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct RunExtras {
    /// When the user opened the finished run; `None` while unseen (and while it is active).
    pub seen_at_ms: Option<i64>,
    /// A tool approval of the run timed out.
    pub approval_timed_out: bool,
    /// Currency of the run's cost (`USD`, `CNY`); `None` while no priced call was made.
    pub cost_currency: Option<String>,
    /// Elapsed run time without machine sleep, in milliseconds.
    pub awake_ms: u64,
}

/// The runs a board shows, in the order of its lanes.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BoardRunIds {
    /// Queued and running runs, newest first.
    pub running: Vec<String>,
    /// Runs waiting for the user (approval or cost pause), oldest first.
    pub waiting: Vec<String>,
    /// Runs that ended in any status and that the user has not opened, newest first.
    pub unseen: Vec<String>,
    /// The most recent opened runs that ended after `since_ms`, newest first.
    pub done: Vec<String>,
}

/// An ended top-level run nobody has opened.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct UnseenRun {
    pub run_id: String,
    pub session_id: String,
}

/// Pending file changes of one run and its sub-agents.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct PendingChanges {
    /// Distinct files.
    pub files: u32,
    pub added: u32,
    pub removed: u32,
}

/// Lifecycle breakdown of sub-agent runs spawned by one parent run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SubagentStatusCounts {
    pub total: u32,
    pub active: u32,
    pub waiting: u32,
    pub completed: u32,
    pub failed: u32,
    pub cancelled: u32,
}

/// First non-empty line of the text parts in `parts_json`, cut to `max_chars`.
fn first_line_of(parts_json: &str, max_chars: usize) -> Option<String> {
    let parts: Vec<HubPart> = serde_json::from_str(parts_json).ok()?;
    parts.iter().find_map(|part| match part {
        HubPart::Text { text, .. } => text
            .lines()
            .map(str::trim)
            .find(|line| !line.is_empty())
            .map(|line| line.chars().take(max_chars).collect()),
        _ => None,
    })
}

impl WorkspaceStore {
    /// The extra columns of `run_ids`; unknown ids are absent.
    pub fn agent_run_extras(
        &self,
        run_ids: &[String],
    ) -> Result<HashMap<String, RunExtras>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id, seen_at_ms, approval_timed_out, cost_currency, awake_ms
             FROM agent_runs WHERE id IN (SELECT value FROM json_each(?1))",
        )?;
        let rows = statement.query_map(params![serde_json::to_string(run_ids)?], |row| {
            Ok((
                row.get::<_, String>(0)?,
                RunExtras {
                    seen_at_ms: row.get(1)?,
                    approval_timed_out: row.get(2)?,
                    cost_currency: row.get(3)?,
                    awake_ms: to_u64(row.get(4)?),
                },
            ))
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Writes the currency, the awake time and the timed-out flag of a run (progress or end).
    pub fn set_agent_run_extras(
        &self,
        run_id: &str,
        cost_currency: Option<&str>,
        awake_ms: u64,
        approval_timed_out: bool,
    ) -> Result<(), StorageError> {
        self.connection.execute(
            "UPDATE agent_runs
             SET cost_currency = COALESCE(?2, cost_currency), awake_ms = ?3,
                 approval_timed_out = approval_timed_out OR ?4
             WHERE id = ?1",
            params![run_id, cost_currency, to_i64(awake_ms), approval_timed_out],
        )?;
        Ok(())
    }

    /// Marks the ended, unseen runs of `session_id` (its sub-agents' included) as seen at
    /// `now_ms`; returns how many changed.
    pub fn mark_agent_runs_seen(
        &self,
        session_id: &str,
        now_ms: i64,
    ) -> Result<usize, StorageError> {
        Ok(self.connection.execute(
            &format!(
                "UPDATE agent_runs SET seen_at_ms = ?2
                 WHERE seen_at_ms IS NULL
                   AND status NOT IN {ACTIVE_RUN_STATUSES}
                   AND (session_id = ?1
                        OR session_id IN (SELECT id FROM agent_sessions
                                          WHERE parent_session_id = ?1))"
            ),
            params![session_id, now_ms],
        )?)
    }

    /// Marks one ended run as seen; returns whether it changed.
    pub fn mark_agent_run_seen(&self, run_id: &str, now_ms: i64) -> Result<bool, StorageError> {
        Ok(self.connection.execute(
            &format!(
                "UPDATE agent_runs SET seen_at_ms = ?2
                 WHERE id = ?1 AND seen_at_ms IS NULL
                   AND status NOT IN {ACTIVE_RUN_STATUSES}"
            ),
            params![run_id, now_ms],
        )? > 0)
    }

    /// The ended top-level runs (a sub-agent's end is never news) the user has not opened.
    pub fn list_unseen_agent_runs(&self) -> Result<Vec<UnseenRun>, StorageError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT id, session_id FROM agent_runs
             WHERE seen_at_ms IS NULL AND kind = 'main'
               AND status NOT IN {ACTIVE_RUN_STATUSES}
             ORDER BY started_at_ms"
        ))?;
        let rows = statement.query_map([], |row| {
            Ok(UnseenRun {
                run_id: row.get(0)?,
                session_id: row.get(1)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// The runs of the board: top-level runs only (sub-agents are reached through their parent).
    /// `done_limit` opened runs that ended at or after `since_ms` make the Done lane.
    pub fn list_board_run_ids(
        &self,
        since_ms: i64,
        done_limit: u32,
    ) -> Result<BoardRunIds, StorageError> {
        let ids = |sql: &str, bind: &[&dyn rusqlite::ToSql]| -> Result<Vec<String>, StorageError> {
            let mut statement = self.connection.prepare_cached(sql)?;
            let rows = statement.query_map(bind, |row| row.get::<_, String>(0))?;
            rows.collect::<Result<_, _>>().map_err(Into::into)
        };
        Ok(BoardRunIds {
            running: ids(
                "SELECT id FROM agent_runs
                 WHERE kind = 'main' AND status IN ('queued', 'running')
                 ORDER BY started_at_ms DESC, id",
                &[],
            )?,
            waiting: ids(
                "SELECT id FROM agent_runs
                 WHERE kind = 'main' AND status = 'waiting_approval'
                 ORDER BY started_at_ms, id",
                &[],
            )?,
            unseen: ids(
                &format!(
                    "SELECT id FROM agent_runs
                     WHERE kind = 'main' AND seen_at_ms IS NULL
                       AND status NOT IN {ACTIVE_RUN_STATUSES}
                     ORDER BY COALESCE(finished_at_ms, started_at_ms) DESC, id"
                ),
                &[],
            )?,
            done: ids(
                &format!(
                    "SELECT id FROM agent_runs
                     WHERE kind = 'main' AND seen_at_ms IS NOT NULL
                       AND status NOT IN {ACTIVE_RUN_STATUSES}
                       AND COALESCE(finished_at_ms, started_at_ms) >= ?1
                     ORDER BY COALESCE(finished_at_ms, started_at_ms) DESC, id
                     LIMIT ?2"
                ),
                &[&since_ms, &done_limit],
            )?,
        })
    }

    /// How many sub-agent runs each of `run_ids` started.
    pub fn agent_subagent_counts(
        &self,
        run_ids: &[String],
    ) -> Result<HashMap<String, u32>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT parent_run_id, COUNT(*) FROM agent_runs
             WHERE parent_run_id IN (SELECT value FROM json_each(?1)) GROUP BY parent_run_id",
        )?;
        let rows = statement.query_map(params![serde_json::to_string(run_ids)?], |row| {
            Ok((row.get::<_, String>(0)?, to_u32(row.get(1)?)))
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Lifecycle status breakdown of sub-agent runs per parent run id.
    pub fn agent_subagent_status_counts(
        &self,
        run_ids: &[String],
    ) -> Result<HashMap<String, SubagentStatusCounts>, StorageError> {
        if run_ids.is_empty() {
            return Ok(HashMap::new());
        }
        let mut statement = self.connection.prepare_cached(
            "SELECT parent_run_id, status, COUNT(*) FROM agent_runs
             WHERE parent_run_id IN (SELECT value FROM json_each(?1))
             GROUP BY parent_run_id, status",
        )?;
        let rows = statement.query_map(params![serde_json::to_string(run_ids)?], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                to_u32(row.get(2)?),
            ))
        })?;
        let mut map: HashMap<String, SubagentStatusCounts> = HashMap::new();
        for row in rows {
            let (parent_id, status, n) = row?;
            let entry = map.entry(parent_id).or_default();
            entry.total = entry.total.saturating_add(n);
            match status.as_str() {
                "queued" | "running" => entry.active = entry.active.saturating_add(n),
                "waiting_approval" => entry.waiting = entry.waiting.saturating_add(n),
                "completed" => entry.completed = entry.completed.saturating_add(n),
                "failed" | "interrupted" => entry.failed = entry.failed.saturating_add(n),
                "cancelled" => entry.cancelled = entry.cancelled.saturating_add(n),
                _ => {}
            }
        }
        Ok(map)
    }

    /// The pending (or conflicting) file changes of `run_id` and of its sub-agents.
    pub fn agent_pending_changes(&self, run_id: &str) -> Result<PendingChanges, StorageError> {
        Ok(self.connection.query_row(
            "SELECT COUNT(DISTINCT path), COALESCE(SUM(added), 0), COALESCE(SUM(removed), 0)
             FROM agent_file_changes
             WHERE status IN ('pending', 'conflict')
               AND (run_id = ?1 OR run_id IN (SELECT id FROM agent_runs WHERE parent_run_id = ?1))",
            params![run_id],
            |row| {
                Ok(PendingChanges {
                    files: to_u32(row.get(0)?),
                    added: to_u32(row.get(1)?),
                    removed: to_u32(row.get(2)?),
                })
            },
        )?)
    }

    /// The first line of the run's latest assistant answer that has text, cut to `max_chars`.
    pub fn agent_run_last_answer(
        &self,
        run_id: &str,
        max_chars: usize,
    ) -> Result<Option<String>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT parts_json FROM agent_messages
             WHERE run_id = ?1 AND role = 'assistant' AND partial = 0
             ORDER BY seq DESC LIMIT 5",
        )?;
        let rows = statement.query_map(params![run_id], |row| row.get::<_, String>(0))?;
        for parts in rows {
            if let Some(line) = first_line_of(&parts?, max_chars) {
                return Ok(Some(line));
            }
        }
        Ok(None)
    }

    /// Cost of the runs started at or after `since_ms`, per currency (a run without a recorded
    /// currency predates the column and was priced in USD).
    pub fn agent_cost_since(&self, since_ms: i64) -> Result<Vec<CurrencyTotal>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT COALESCE(cost_currency, 'USD'), SUM(cost_micro_usd) FROM agent_runs
             WHERE started_at_ms >= ?1 AND cost_micro_usd > 0
             GROUP BY COALESCE(cost_currency, 'USD') ORDER BY 1",
        )?;
        let rows = statement.query_map(params![since_ms], |row| {
            Ok(CurrencyTotal {
                currency: row.get(0)?,
                cost_micro: row.get(1)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// What the runs of chat `session_id` and their sub-agents cost, per currency (a run without
    /// a recorded currency predates the column and was priced in USD).
    pub fn agent_session_cost_by_currency(
        &self,
        session_id: &str,
    ) -> Result<Vec<CurrencyCost>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT COALESCE(cost_currency, 'USD'), SUM(cost_micro_usd) FROM agent_runs
             WHERE cost_micro_usd > 0 AND (session_id = ?1 OR parent_run_id IN
                 (SELECT id FROM agent_runs WHERE session_id = ?1))
             GROUP BY COALESCE(cost_currency, 'USD') ORDER BY 1",
        )?;
        let rows = statement.query_map(params![session_id], |row| {
            Ok(CurrencyCost {
                currency: row.get(0)?,
                cost_micro: row.get(1)?,
            })
        })?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Ids of the pending file changes of `session_id` (and its sub-agent chats) whose run is not
    /// on the chat's active path: the files an abandoned branch left on disk.
    pub fn off_branch_pending_change_ids(
        &self,
        session_id: &str,
    ) -> Result<Vec<String>, StorageError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "{PATH_CTE},
             on_path(run_id) AS (
                 SELECT DISTINCT m.run_id FROM agent_messages m JOIN path ON m.id = path.id
                 WHERE m.run_id IS NOT NULL
             )
             SELECT c.id FROM agent_file_changes c
             WHERE c.status IN ('pending', 'conflict')
               AND (c.session_id = ?1
                    OR c.session_id IN (SELECT id FROM agent_sessions WHERE parent_session_id = ?1))
               AND c.run_id NOT IN (SELECT run_id FROM on_path)
               AND COALESCE((SELECT parent_run_id FROM agent_runs WHERE id = c.run_id), '')
                   NOT IN (SELECT run_id FROM on_path)
             ORDER BY c.created_ms DESC, c.rowid DESC"
        ))?;
        let rows = statement.query_map(params![session_id], |row| row.get::<_, String>(0))?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn the_first_line_is_the_first_non_empty_line_of_the_text() {
        let parts = serde_json::to_string(&vec![HubPart::text(
            "\n  \nDone: wrote the file.\nSecond line",
        )])
        .unwrap_or_default();
        assert_eq!(
            first_line_of(&parts, 100).as_deref(),
            Some("Done: wrote the file.")
        );
        assert_eq!(first_line_of(&parts, 4).as_deref(), Some("Done"));
        assert_eq!(first_line_of("[]", 10), None);
    }
}
