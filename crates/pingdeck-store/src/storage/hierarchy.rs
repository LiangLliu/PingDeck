//! Workspace rows and the parts of the hierarchy that span aggregates: the id occupancy check
//! and the atomic save of a script's changes. Collections, folders and imports live in the
//! submodules.

mod collections;
mod folders;
mod import;

use rusqlite::{Connection, OptionalExtension, params};

use pingdeck_model::models::{
    Collection, Environment, EnvironmentVariable, Workspace, WorkspaceId,
};

use crate::storage::{StorageError, WorkspaceStore};

impl WorkspaceStore {
    /// Upserts a workspace's name, refreshing its update timestamp.
    pub fn save_workspace(&self, workspace: &Workspace) -> Result<(), StorageError> {
        self.connection.execute(
            "
            INSERT INTO workspaces (id, name, updated_at)
            VALUES (?1, ?2, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                updated_at = excluded.updated_at
            ",
            params![workspace.id.as_str(), workspace.name],
        )?;

        Ok(())
    }

    /// Stamps a workspace's last-opened time; returns whether the workspace exists.
    pub fn touch_workspace(&self, id: &WorkspaceId) -> Result<bool, StorageError> {
        let affected_rows = self.connection.execute(
            "
            UPDATE workspaces
            SET last_opened_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
            WHERE id = ?1
            ",
            params![id.as_str()],
        )?;

        Ok(affected_rows > 0)
    }

    pub fn get_workspace(&self, id: &WorkspaceId) -> Result<Option<Workspace>, StorageError> {
        let row = self
            .connection
            .query_row(
                "SELECT id, name FROM workspaces WHERE id = ?1",
                params![id.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?;

        row.map(|(id, name)| Ok(Workspace::new(&id, &name)?))
            .transpose()
    }

    /// Lists workspaces, most recently opened first, then by name and id.
    pub fn list_workspaces(&self) -> Result<Vec<Workspace>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "
            SELECT id, name
            FROM workspaces
            ORDER BY last_opened_at IS NULL, last_opened_at DESC, name ASC, id ASC
            ",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?))
        })?;

        let mut workspaces = Vec::new();
        for row in rows {
            let (id, name) = row?;
            workspaces.push(Workspace::new(&id, &name)?);
        }

        Ok(workspaces)
    }

    /// Deletes a workspace by id (cascading to its contents); returns whether a row was removed.
    pub fn delete_workspace(&self, id: &WorkspaceId) -> Result<bool, StorageError> {
        self.atomic(|transaction| {
            Self::delete_workspace_secrets(transaction, id.as_str())?;
            let affected_rows = transaction
                .execute("DELETE FROM workspaces WHERE id = ?1", params![id.as_str()])?;
            Ok(affected_rows > 0)
        })
    }

    #[cfg(test)]
    pub(crate) fn workspace_count(&self) -> Result<i64, StorageError> {
        Ok(self
            .connection
            .query_row("SELECT COUNT(*) FROM workspaces", [], |row| row.get(0))?)
    }

    /// Deletes every secret a workspace owns: those of its environment, global and collection
    /// variables, its TLS key passphrases and its OAuth 2.0 tokens. Runs before the workspace row
    /// (and with it the rows naming them) is deleted; the secrets table has no foreign key to them.
    fn delete_workspace_secrets(
        connection: &Connection,
        workspace_id: &str,
    ) -> Result<(), StorageError> {
        connection.execute(
            "
            DELETE FROM secrets WHERE secret_ref IN (
                SELECT 'env.' || v.environment_id || '.' || v.key
                FROM environment_variables v
                JOIN environments e ON e.id = v.environment_id
                WHERE e.workspace_id = ?1 AND v.is_secret = 1
                UNION ALL
                SELECT 'global.' || workspace_id || '.' || key
                FROM global_variables
                WHERE workspace_id = ?1 AND is_secret = 1
            )
            ",
            params![workspace_id],
        )?;
        let collection_ids = connection
            .prepare_cached("SELECT id FROM collections WHERE workspace_id = ?1")?
            .query_map(params![workspace_id], |row| row.get::<_, String>(0))?
            .collect::<Result<Vec<_>, _>>()?;
        for collection_id in collection_ids {
            Self::delete_collection_secrets(connection, &collection_id)?;
        }
        // TLS key passphrases (`tls.<workspace>.<certificate>.passphrase`, or a custom ref).
        connection.execute(
            "
                DELETE FROM secrets WHERE secret_ref IN (
                    SELECT passphrase_ref FROM tls_certificates
                    WHERE workspace_id = ?1 AND passphrase_ref IS NOT NULL
                    UNION
                    SELECT 'tls.' || workspace_id || '.' || id || '.passphrase'
                    FROM tls_certificates WHERE workspace_id = ?1
                )
                ",
            params![workspace_id],
        )?;
        // OAuth 2.0 tokens (`oauth.<workspace>.<16 hex digest>.{access,refresh,meta}`): keyed by
        // the workspace, not by a row. A prefix compare, not LIKE (`_` in an id is a LIKE
        // wildcard), and the rest must be exactly a digest and a suffix: a workspace id may hold
        // dots, so `oauth.team.` also prefixes the tokens of workspace `team.dev`.
        connection.execute(
            "DELETE FROM secrets
             WHERE substr(secret_ref, 1, length(?1)) = ?1
               AND substr(secret_ref, length(?1) + 1, 16) NOT GLOB '*[^0-9a-f]*'
               AND substr(secret_ref, length(?1) + 17) IN ('.access', '.refresh', '.meta')",
            params![format!("oauth.{workspace_id}.")],
        )?;
        Ok(())
    }

    /// Whether `table` (a literal table name) has a row with this id in any workspace. Four
    /// fixed shapes, called per id by an import, so the statement is cached.
    pub(super) fn row_id_exists(
        &self,
        table: &'static str,
        id: &str,
    ) -> Result<bool, StorageError> {
        Ok(self
            .connection
            .prepare_cached(&format!(
                "SELECT EXISTS(SELECT 1 FROM {table} WHERE id = ?1)"
            ))?
            .query_row(params![id], |row| row.get(0))?)
    }

    /// Atomically persists whichever of collection, environment, and globals a script changed.
    pub fn save_script_state(
        &self,
        workspace_id: &str,
        collection: Option<&Collection>,
        environment: Option<&Environment>,
        globals: Option<&[EnvironmentVariable]>,
    ) -> Result<(), StorageError> {
        if collection.is_none() && environment.is_none() && globals.is_none() {
            return Ok(());
        }
        self.atomic(|transaction| {
            if let Some(collection) = collection {
                Self::save_collection_on(transaction, collection)?;
            }
            if let Some(environment) = environment {
                Self::save_environment_on(transaction, environment)?;
            }
            if let Some(globals) = globals {
                Self::save_global_variables_on(transaction, workspace_id, globals)?;
            }
            Ok(())
        })
    }
}
