//! Per-workspace tab session persistence.

use rusqlite::{OptionalExtension, params};

use super::{StorageError, WorkspaceStore};

/// Largest tab session payload, in UTF-8 bytes. Checked when saving and, through
/// `octet_length`, before a stored row is read.
pub const MAX_TAB_SESSION_BYTES: usize = 8 * 1024 * 1024;

fn too_large(workspace_id: &str, size_bytes: usize) -> StorageError {
    StorageError::TabSessionTooLarge {
        workspace_id: workspace_id.to_owned(),
        size_bytes,
        limit_bytes: MAX_TAB_SESSION_BYTES,
    }
}

impl WorkspaceStore {
    pub fn load_tab_session(&self, workspace_id: &str) -> Result<Option<String>, StorageError> {
        // Check the stored byte size in SQL first so an oversized row is never read into memory.
        let size: Option<i64> = self
            .connection
            .prepare_cached(
                "SELECT octet_length(payload) FROM tab_sessions WHERE workspace_id = ?1",
            )?
            .query_row(params![workspace_id], |row| row.get(0))
            .optional()?;
        let Some(size) = size else {
            return Ok(None);
        };
        let size = usize::try_from(size).unwrap_or(usize::MAX);
        if size > MAX_TAB_SESSION_BYTES {
            return Err(too_large(workspace_id, size));
        }

        Ok(self
            .connection
            .prepare_cached("SELECT payload FROM tab_sessions WHERE workspace_id = ?1")?
            .query_row(params![workspace_id], |row| row.get::<_, String>(0))
            .optional()?)
    }

    /// Persists the UI session for a workspace; the workspace foreign key rejects unknown ids.
    pub fn save_tab_session(&self, workspace_id: &str, payload: &str) -> Result<(), StorageError> {
        if payload.len() > MAX_TAB_SESSION_BYTES {
            return Err(too_large(workspace_id, payload.len()));
        }

        self.connection
            .prepare_cached(
                "
            INSERT INTO tab_sessions (workspace_id, payload) VALUES (?1, ?2)
            ON CONFLICT(workspace_id) DO UPDATE SET payload = excluded.payload
            ",
            )?
            .execute(params![workspace_id, payload])?;

        Ok(())
    }
}
