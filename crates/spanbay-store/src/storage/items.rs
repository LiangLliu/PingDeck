//! Workspace item persistence: mock servers, flows, and TLS certificates, one submodule each.
//! Nested documents are stored as JSON columns so each item saves and loads in one row.
//!
//! Every upsert checks ownership with `ensure_item_workspace` inside the same transaction, so an
//! upsert can never steal or overwrite another workspace's row, even with a concurrent writer.

mod flows;
mod mock_servers;
mod tls_certificates;

use rusqlite::{OptionalExtension, params};

use crate::storage::{StorageError, WorkspaceStore};

impl WorkspaceStore {
    /// Rejects an item id already owned by a different workspace, so an upsert
    /// can never steal or overwrite another workspace's row. `table` is interpolated into
    /// the SQL and must be a literal from this module, never caller input.
    fn ensure_item_workspace(
        &self,
        table: &'static str,
        kind: &'static str,
        id: &str,
        workspace_id: &str,
    ) -> Result<(), StorageError> {
        let owner: Option<String> = self
            .connection
            .query_row(
                &format!("SELECT workspace_id FROM {table} WHERE id = ?1"),
                params![id],
                |row| row.get(0),
            )
            .optional()?;

        match owner {
            Some(owner) if owner != workspace_id => Err(StorageError::ItemWorkspaceMismatch {
                kind,
                id: id.to_owned(),
            }),
            _ => Ok(()),
        }
    }
}

/// Rejects a negative stored INTEGER instead of truncating it.
fn stored_u64(table: &'static str, column: &'static str, value: i64) -> Result<u64, StorageError> {
    u64::try_from(value).map_err(|_| StorageError::InvalidStoredValue {
        table,
        column,
        value: value.to_string(),
    })
}
