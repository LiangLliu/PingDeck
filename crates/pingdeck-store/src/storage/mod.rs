//! SQLite persistence for workspace data: the store handle, error type, and shared helpers.
//!
//! Each aggregate lives in its own submodule as an `impl WorkspaceStore` block.

mod agent;
mod attachments;
mod collection_runs;
mod cookies;
mod data_usage;
mod environments;
mod error;
mod flow_runs;
mod hierarchy;
mod history;
mod items;
mod maintenance;
mod pool;
mod process_ledger;
mod requests;
mod schema;
pub mod secrets;
mod sessions;
mod settings;
mod sync_state;
mod system;
#[cfg(test)]
mod tests;
mod tree;
mod vault;

pub use agent::{
    BoardRunIds, MCP_REVIEW_PENDING, PendingChanges, PingDeckBinding, RunExtras,
    SubagentStatusCounts, UnseenRun,
};
pub use cookies::{COOKIE_JAR_MAX_ROWS, COOKIE_MAX_BYTES};
pub use data_usage::{CaptureUsage, ChatUsage, ClearableAttachment};
pub use environments::{collection_revision, environment_revision, globals_revision};
pub use error::{PlacementFault, StorageError};
pub use history::{
    HISTORY_MOCK_RETENTION, HISTORY_RETENTION_PER_WORKSPACE, HISTORY_ROW_BUCKETS, HistoryRows,
};
pub use maintenance::{
    BackupFiles, SetAsideDatabase, backup_database, compact_database, free_bytes,
};
pub use pool::{DbConn, DbHandle};
pub use sessions::MAX_TAB_SESSION_BYTES;
pub use settings::raw_app_setting_json;
pub use sync_state::WorkspaceSyncStamps;

use std::path::Path;

use rusqlite::Connection;

use crate::sqlite::i64_from_usize;

/// SQLite-backed store for workspaces and everything they contain.
pub struct WorkspaceStore {
    connection: Connection,
    /// History rows kept per workspace (a global setting; see `settings`).
    history_retention: usize,
}

impl WorkspaceStore {
    /// Opens the owner-only database at `path`, attaches its vault and ensures the schema is initialized.
    pub fn open(path: impl AsRef<Path>) -> Result<Self, StorageError> {
        let path = path.as_ref();
        let connection = Connection::open(path)?;
        pingdeck_base::platform::restrict_file_to_owner(path)?;
        Self::from_connection_at(connection, Some(path))
    }

    /// The underlying connection, for feature modules that own their own tables (the LLM layer).
    /// Hold the store lock only for the one synchronous call that uses it.
    pub fn connection(&self) -> &Connection {
        &self.connection
    }

    /// Opens an in-memory store with the full schema for tests and headless callers.
    pub fn open_in_memory() -> Result<Self, StorageError> {
        let connection = Connection::open_in_memory()?;
        Self::from_connection_at(connection, None)
    }

    /// Runs `work` atomically under the [`crate::sqlite::atomic_with`] contract. `work` may call
    /// other `&self` methods: they share the connection and therefore the transaction.
    pub(super) fn atomic<T>(
        &self,
        work: impl FnOnce(&Connection) -> Result<T, StorageError>,
    ) -> Result<T, StorageError> {
        crate::sqlite::atomic(&self.connection, work)
    }

    /// Runs `work` atomically for callers outside the store with their own error type
    /// (directory-sync import): `BEGIN IMMEDIATE` when idle, a `SAVEPOINT` when nested; `Err`
    /// rolls back. Note the argument order, `(work, map_err)`, the reverse of the crate-private
    /// `crate::sqlite::atomic_with`.
    pub fn with_transaction<T, E>(
        &self,
        work: impl FnOnce(&Connection) -> Result<T, E>,
        map_err: impl Fn(rusqlite::Error) -> E,
    ) -> Result<T, E> {
        crate::sqlite::atomic_with(&self.connection, map_err, work)
    }

    /// History rows kept per workspace.
    #[must_use]
    pub fn history_retention(&self) -> usize {
        self.history_retention
    }

    pub fn set_history_retention(&mut self, rows: usize) {
        self.history_retention = rows;
    }

    fn from_connection_at(
        connection: Connection,
        path: Option<&Path>,
    ) -> Result<Self, StorageError> {
        let store = Self {
            connection,
            history_retention: HISTORY_RETENTION_PER_WORKSPACE,
        };
        let stored = store.schema_version()?;
        store.apply_pragmas()?;
        store
            .connection
            .set_prepared_statement_cache_capacity(schema::STATEMENT_CACHE_CAPACITY);
        history::register_search_functions(&store.connection)?;
        if let Some(path) = path {
            vault::attach_vault(&store.connection, path)?;
        }
        schema::ensure_schema(&store.connection, stored, path.is_some())?;
        Ok(store)
    }
}

/// Keeps at most `keep` newest rows for `parent_id` inside `table` (`collection_runs` or
/// `flow_runs`). `table` and `parent_column` must be static column/table names.
pub(super) fn prune_runs_by_parent(
    connection: &Connection,
    table: &'static str,
    parent_column: &'static str,
    parent_id: &str,
    keep: usize,
) -> Result<(), StorageError> {
    connection.execute(
        &format!(
            "DELETE FROM {table}
             WHERE {parent_column} = ?1
               AND rowid NOT IN (
                   SELECT rowid FROM {table}
                   WHERE {parent_column} = ?1
                   ORDER BY started_at_ms DESC, rowid DESC
                   LIMIT ?2
               )"
        ),
        rusqlite::params![parent_id, i64_from_usize(keep)],
    )?;
    Ok(())
}

/// A fresh in-memory connection at the current schema (foreign keys on), for tests of modules
/// that own tables (the LLM layer, the secret store). Behind `test-support` because those tests
/// also live in other crates.
#[cfg(any(test, feature = "test-support"))]
#[allow(clippy::unwrap_used)] // test support: panicking is the intended failure mode
pub fn migrated_memory_connection() -> Connection {
    let connection = Connection::open_in_memory().unwrap();
    connection
        .execute_batch("PRAGMA foreign_keys = ON;")
        .unwrap();
    schema::ensure_schema(&connection, 0, false).unwrap();
    connection
}
