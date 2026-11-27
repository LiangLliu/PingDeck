//! Directory-sync bookkeeping: row stamps, settings section times, and the export-file ledger.

use std::collections::HashMap;

use rusqlite::{Connection, params};

use super::{StorageError, WorkspaceStore};

/// SQL turning a stored `updated_at` text into Unix milliseconds.
const MS: &str = "(CAST(strftime('%s', updated_at) AS INTEGER) * 1000 + CAST(substr(updated_at, 21, 3) AS INTEGER))";
/// The stored text of `?1` milliseconds; an unrepresentable stamp keeps the row's current one.
const STAMP: &str =
    "COALESCE(strftime('%Y-%m-%dT%H:%M:%fZ', ?1 / 1000.0, 'unixepoch'), updated_at)";

/// Row timestamps of one workspace for directory sync.
#[derive(Debug, Default)]
pub struct WorkspaceSyncStamps {
    pub workspaces: HashMap<String, i64>,
    pub environments: HashMap<String, i64>,
    pub collections: HashMap<String, i64>,
    pub folders: HashMap<String, i64>,
    pub loose_requests: HashMap<String, i64>,
    pub collection_requests: HashMap<String, i64>,
}

/// Runs a `SELECT key, ms ... WHERE ... = ?1` stamp query; rows without a stamp are skipped.
fn query_stamps(
    connection: &Connection,
    sql: &str,
    workspace_id: &str,
) -> Result<HashMap<String, i64>, StorageError> {
    let mut statement = connection.prepare_cached(sql)?;
    let rows = statement.query_map(params![workspace_id], |row| {
        Ok((row.get::<_, String>(0)?, row.get::<_, Option<i64>>(1)?))
    })?;
    let mut out = HashMap::new();
    for row in rows {
        if let (id, Some(ms)) = row? {
            out.insert(id, ms);
        }
    }
    Ok(out)
}

impl WorkspaceStore {
    /// The `updated_at` timestamps (Unix ms) of all syncable entities in `workspace_id`.
    pub fn sync_workspace_stamps(
        &self,
        workspace_id: &str,
    ) -> Result<WorkspaceSyncStamps, StorageError> {
        let c = &self.connection;
        // `MAX{MS}` is a valid call because `MS` starts with `(`.
        Ok(WorkspaceSyncStamps {
            workspaces: query_stamps(
                c,
                &format!("SELECT id, {MS} FROM workspaces WHERE id = ?1"),
                workspace_id,
            )?,
            environments: query_stamps(
                c,
                &format!("SELECT id, {MS} FROM environments WHERE workspace_id = ?1"),
                workspace_id,
            )?,
            collections: query_stamps(
                c,
                &format!("SELECT id, {MS} FROM collections WHERE workspace_id = ?1"),
                workspace_id,
            )?,
            folders: query_stamps(
                c,
                &format!(
                    "SELECT collection_id, MAX{MS} FROM folders WHERE collection_id IN \
                     (SELECT id FROM collections WHERE workspace_id = ?1) GROUP BY collection_id"
                ),
                workspace_id,
            )?,
            loose_requests: query_stamps(
                c,
                &format!(
                    "SELECT workspace_id, MAX{MS} FROM requests \
                     WHERE workspace_id = ?1 AND collection_id IS NULL GROUP BY workspace_id"
                ),
                workspace_id,
            )?,
            collection_requests: query_stamps(
                c,
                &format!(
                    "SELECT collection_id, MAX{MS} FROM requests \
                     WHERE workspace_id = ?1 AND collection_id IS NOT NULL GROUP BY collection_id"
                ),
                workspace_id,
            )?,
        })
    }

    /// When each settings section was last written (Unix ms), keyed by section.
    pub fn sync_section_times(&self) -> Result<HashMap<String, i64>, StorageError> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT section, updated_at_ms FROM app_settings")?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, i64>(1)?))
        })?;
        rows.collect::<Result<_, _>>().map_err(StorageError::from)
    }

    /// Stamps `updated_at` on a collection, its folders, and its requests after sync import.
    pub fn sync_stamp_collection(&self, collection_id: &str, ms: i64) -> Result<(), StorageError> {
        for (table, key) in [
            ("collections", "id"),
            ("folders", "collection_id"),
            ("requests", "collection_id"),
        ] {
            self.connection.execute(
                &format!("UPDATE {table} SET updated_at = {STAMP} WHERE {key} = ?2"),
                params![ms, collection_id],
            )?;
        }
        Ok(())
    }

    pub fn sync_stamp_environment(
        &self,
        environment_id: &str,
        ms: i64,
    ) -> Result<(), StorageError> {
        self.connection.execute(
            &format!("UPDATE environments SET updated_at = {STAMP} WHERE id = ?2"),
            params![ms, environment_id],
        )?;
        Ok(())
    }

    pub fn sync_stamp_mcp_server(&self, server_id: &str, ms: i64) -> Result<(), StorageError> {
        self.connection.execute(
            "UPDATE agent_mcp_servers SET updated_at_ms = ?1 WHERE id = ?2",
            params![ms, server_id],
        )?;
        Ok(())
    }

    /// Relative paths a config-sync export wrote into `directory`, with the SHA-256 of the bytes
    /// written (local-only, never exported).
    pub fn list_sync_export_files(
        &self,
        directory: &str,
    ) -> Result<HashMap<String, String>, StorageError> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT path, sha256 FROM sync_export_files WHERE directory = ?1")?;
        let rows = statement.query_map(params![directory], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Records `written` `(path, sha256)` pairs as files this machine wrote into `directory` and
    /// forgets `gone`.
    pub fn update_sync_export_files(
        &self,
        directory: &str,
        written: &[(String, String)],
        gone: &[String],
    ) -> Result<(), StorageError> {
        self.atomic(|transaction| {
            for (path, sha256) in written {
                transaction.execute(
                    "INSERT OR REPLACE INTO sync_export_files (directory, path, sha256)
                     VALUES (?1, ?2, ?3)",
                    params![directory, path, sha256],
                )?;
            }
            for path in gone {
                transaction.execute(
                    "DELETE FROM sync_export_files WHERE directory = ?1 AND path = ?2",
                    params![directory, path],
                )?;
            }
            Ok(())
        })
    }
}
