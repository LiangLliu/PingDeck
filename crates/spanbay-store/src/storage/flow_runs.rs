//! Flow run persistence: the newest runs of each flow (`flow_runs`).
//!
//! A row holds `summary_json`, the whole [`FlowRunSummary`]; step bodies are never stored (they
//! are shown for a live run only). Deleting a flow removes its runs through the foreign key.

use std::collections::HashMap;

use rusqlite::params;

use spanbay_model::items::FlowRunSummary;

use super::{StorageError, WorkspaceStore, prune_runs_by_parent};

/// Runs kept per flow; older ones are pruned in the transaction that inserts a newer one.
pub(crate) const FLOW_RUN_RETENTION: usize = 20;

impl WorkspaceStore {
    /// Stores the summary of a finished run and prunes the flow to its newest
    /// `FLOW_RUN_RETENTION` runs, in one transaction.
    ///
    /// Returns `false` (and stores nothing) when the flow no longer exists in `workspace_id`,
    /// e.g. it was deleted while it ran.
    pub fn save_flow_run(
        &self,
        workspace_id: &str,
        summary: &FlowRunSummary,
    ) -> Result<bool, StorageError> {
        self.atomic(|transaction| {
            let inserted = transaction.execute(
                "
                INSERT INTO flow_runs (id, flow_id, started_at_ms, summary_json)
                SELECT ?1, ?2, ?3, ?4
                WHERE EXISTS (SELECT 1 FROM flows WHERE id = ?2 AND workspace_id = ?5)
                ",
                params![
                    summary.run_id,
                    summary.flow_id,
                    summary.started_at_ms,
                    serde_json::to_string(summary)?,
                    workspace_id,
                ],
            )?;
            if inserted > 0 {
                prune_runs_by_parent(
                    transaction,
                    "flow_runs",
                    "flow_id",
                    &summary.flow_id,
                    FLOW_RUN_RETENTION,
                )?;
            }
            Ok(inserted > 0)
        })
    }

    /// The newest stored run of every flow of a workspace that has one, keyed by flow id, in a
    /// single query.
    pub fn last_flow_runs(
        &self,
        workspace_id: &str,
    ) -> Result<HashMap<String, FlowRunSummary>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "
            SELECT summary_json FROM (
                SELECT r.summary_json, ROW_NUMBER() OVER (
                    PARTITION BY r.flow_id ORDER BY r.started_at_ms DESC, r.rowid DESC
                ) AS rank
                FROM flow_runs r
                JOIN flows f ON f.id = r.flow_id
                WHERE f.workspace_id = ?1
            )
            WHERE rank = 1
            ",
        )?;
        let rows = statement.query_map(params![workspace_id], |row| row.get::<_, String>(0))?;

        let mut runs = HashMap::new();
        for row in rows {
            let summary: FlowRunSummary = serde_json::from_str(&row?)?;
            runs.insert(summary.flow_id.clone(), summary);
        }
        Ok(runs)
    }

    /// The stored runs of one flow, newest first.
    pub fn list_flow_runs(&self, flow_id: &str) -> Result<Vec<FlowRunSummary>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "
            SELECT summary_json FROM flow_runs
            WHERE flow_id = ?1
            ORDER BY started_at_ms DESC, rowid DESC
            ",
        )?;
        let rows = statement.query_map(params![flow_id], |row| row.get::<_, String>(0))?;

        let mut runs = Vec::new();
        for row in rows {
            runs.push(serde_json::from_str(&row?)?);
        }
        Ok(runs)
    }
}
