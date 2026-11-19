//! Collection run persistence: the newest runs of each collection (`collection_runs`).
//!
//! A row holds `summary_json`, the whole [`CollectionRunSummary`]. Deleting the collection or
//! the workspace removes its runs through the foreign keys.

use rusqlite::{OptionalExtension, params};

use pingdeck_model::runner_item::CollectionRunSummary;

use super::{StorageError, WorkspaceStore, prune_runs_by_parent};

/// Runs kept per collection; older ones are pruned in the transaction that inserts a newer one.
pub(crate) const COLLECTION_RUN_RETENTION: usize = 20;

impl WorkspaceStore {
    /// Stores a finished run and prunes its collection to the newest
    /// `COLLECTION_RUN_RETENTION` runs, in one transaction.
    ///
    /// Returns `false` (and stores nothing) when the collection no longer exists in the run's
    /// workspace.
    pub fn save_collection_run(
        &self,
        summary: &CollectionRunSummary,
    ) -> Result<bool, StorageError> {
        self.atomic(|transaction| {
            let inserted = transaction.execute(
                "
                INSERT INTO collection_runs (id, workspace_id, collection_id, started_at_ms, summary_json)
                SELECT ?1, ?2, ?3, ?4, ?5
                WHERE EXISTS (SELECT 1 FROM collections WHERE id = ?3 AND workspace_id = ?2)
                ",
                params![
                    summary.run_id,
                    summary.workspace_id,
                    summary.collection_id,
                    summary.started_at_ms,
                    serde_json::to_string(summary)?,
                ],
            )?;
            if inserted > 0 {
                prune_runs_by_parent(
                    transaction,
                    "collection_runs",
                    "collection_id",
                    &summary.collection_id,
                    COLLECTION_RUN_RETENTION,
                )?;
            }
            Ok(inserted > 0)
        })
    }

    /// One stored run of `workspace_id`; `None` when it is unknown, pruned or owned by another
    /// workspace.
    pub fn get_collection_run(
        &self,
        workspace_id: &str,
        run_id: &str,
    ) -> Result<Option<CollectionRunSummary>, StorageError> {
        let json = self
            .connection
            .prepare_cached(
                "SELECT summary_json FROM collection_runs WHERE id = ?1 AND workspace_id = ?2",
            )?
            .query_row(params![run_id, workspace_id], |row| row.get::<_, String>(0))
            .optional()?;
        json.map(|json| serde_json::from_str(&json).map_err(StorageError::from))
            .transpose()
    }
}
