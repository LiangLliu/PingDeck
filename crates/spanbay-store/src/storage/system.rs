//! Read-only aggregate queries for the system use-cases: row counts of a workspace (the delete
//! confirm) and the folders the chats are bound to (what `reveal_path` may open).

use rusqlite::params;

use spanbay_model::hierarchy::WorkspaceCounts;

use super::{StorageError, WorkspaceStore};
use crate::sqlite::to_u64;

impl WorkspaceStore {
    /// The row counts of `workspace_id` (all zero for an unknown id).
    pub fn workspace_counts(&self, workspace_id: &str) -> Result<WorkspaceCounts, StorageError> {
        let counts = self.connection.query_row(
            "SELECT
                 (SELECT COUNT(*) FROM collections WHERE workspace_id = ?1),
                 (SELECT COUNT(*) FROM requests WHERE workspace_id = ?1),
                 (SELECT COUNT(*) FROM environments WHERE workspace_id = ?1),
                 (SELECT COUNT(*) FROM mock_servers WHERE workspace_id = ?1),
                 (SELECT COUNT(*) FROM flows WHERE workspace_id = ?1),
                 (SELECT COUNT(*) FROM execution_history WHERE workspace_id = ?1),
                 (SELECT COUNT(*) FROM agent_sessions WHERE spanbay_workspace_id = ?1
                      AND deleted_at_ms IS NULL AND parent_session_id IS NULL)",
            params![workspace_id],
            |row| {
                let count = |index| row.get::<_, i64>(index).map(to_u64);
                Ok(WorkspaceCounts {
                    collections: count(0)?,
                    requests: count(1)?,
                    environments: count(2)?,
                    mock_servers: count(3)?,
                    flows: count(4)?,
                    history_entries: count(5)?,
                    chats: count(6)?,
                })
            },
        )?;
        Ok(counts)
    }

    /// The distinct folders chats are bound to (each chat's `workspace_dir`).
    pub fn agent_session_folders(&self) -> Result<Vec<String>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT DISTINCT workspace_dir FROM agent_sessions
             WHERE workspace_dir IS NOT NULL AND workspace_dir <> ''",
        )?;
        let folders = statement
            .query_map([], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(folders)
    }
}
