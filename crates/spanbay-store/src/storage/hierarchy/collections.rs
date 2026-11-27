//! Collection rows: upsert with the secret variables moved to the secret store, reads that put
//! them back, and delete.

use rusqlite::{Connection, OptionalExtension, params};

use spanbay_model::models::{
    AuthConfig, Collection, CollectionId, EnvironmentVariable, WorkspaceId,
};

use crate::storage::{StorageError, WorkspaceStore, secrets};

impl WorkspaceStore {
    /// Upserts a collection with its auth, variables, scripts, and position.
    pub fn save_collection(&self, collection: &Collection) -> Result<(), StorageError> {
        self.atomic(|transaction| Self::save_collection_on(transaction, collection))
    }

    /// Upserts a collection on the given connection or transaction. A secret variable's value
    /// moves into the secret store (`collection.<id>.<key>`) and its JSON value stays blank.
    pub(super) fn save_collection_on(
        connection: &Connection,
        collection: &Collection,
    ) -> Result<(), StorageError> {
        Self::delete_collection_secrets(connection, collection.id.as_str())?;
        let now_ms = spanbay_base::time::now_ms();
        let mut stored_variables = collection.variables.clone();
        for variable in &mut stored_variables {
            let Some(value) = variable.value.as_deref().filter(|_| variable.is_secret) else {
                continue;
            };
            if !value.is_empty() {
                secrets::put(
                    connection,
                    &collection_secret_ref(collection.id.as_str(), &variable.key),
                    value,
                    now_ms,
                )?;
                variable.value = Some(String::new());
            }
        }

        // The conflict clause only updates a row the workspace already owns, so an id owned by
        // another workspace changes nothing (child requests stay put) and is reported.
        let changed = connection.execute(
            "
            INSERT INTO collections (
                id, workspace_id, name, description, auth_json, variables_json,
                pre_request_script, test_script, position, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                description = excluded.description,
                auth_json = excluded.auth_json,
                variables_json = excluded.variables_json,
                pre_request_script = excluded.pre_request_script,
                test_script = excluded.test_script,
                position = excluded.position,
                updated_at = excluded.updated_at
            WHERE collections.workspace_id = excluded.workspace_id
            ",
            params![
                collection.id.as_str(),
                collection.workspace_id.as_str(),
                collection.name,
                collection.description,
                serde_json::to_string(&collection.auth)?,
                serde_json::to_string(&stored_variables)?,
                collection.pre_request_script,
                collection.test_script,
                collection.position,
            ],
        )?;
        if changed == 0 {
            return Err(StorageError::ItemWorkspaceMismatch {
                kind: "collection",
                id: collection.id.as_str().to_owned(),
            });
        }

        Ok(())
    }

    /// Deletes the secret-store entries of the secret variables stored on a collection row.
    pub(super) fn delete_collection_secrets(
        connection: &Connection,
        collection_id: &str,
    ) -> Result<(), StorageError> {
        let json: Option<String> = connection
            .query_row(
                "SELECT variables_json FROM collections WHERE id = ?1",
                params![collection_id],
                |row| row.get(0),
            )
            .optional()?;
        // An unreadable row names no secrets, and must not block saving or deleting the row.
        let variables = json
            .and_then(|json| serde_json::from_str::<Vec<EnvironmentVariable>>(&json).ok())
            .unwrap_or_default();
        for variable in variables.iter().filter(|variable| variable.is_secret) {
            secrets::delete(
                connection,
                &collection_secret_ref(collection_id, &variable.key),
            )?;
        }

        Ok(())
    }

    /// Fills in the blank value of every secret variable from the secret store.
    fn load_collection_secrets(&self, collection: &mut Collection) -> Result<(), StorageError> {
        for variable in collection.variables.iter_mut().filter(|v| v.is_secret) {
            variable.value = Some(
                secrets::get(
                    &self.connection,
                    &collection_secret_ref(collection.id.as_str(), &variable.key),
                )?
                .unwrap_or_default(),
            );
        }

        Ok(())
    }

    /// Loads one collection of a workspace, or `None` when it does not exist there.
    pub fn get_collection(
        &self,
        workspace_id: &WorkspaceId,
        id: &CollectionId,
    ) -> Result<Option<Collection>, StorageError> {
        let row = self
            .connection
            .query_row(
                "
                SELECT id, workspace_id, name, description, auth_json, variables_json,
                       pre_request_script, test_script, position
                FROM collections WHERE workspace_id = ?1 AND id = ?2
                ",
                params![workspace_id.as_str(), id.as_str()],
                stored_collection_row,
            )
            .optional()?;

        let mut collection = row.map(StoredCollection::into_domain).transpose()?;
        if let Some(collection) = collection.as_mut() {
            self.load_collection_secrets(collection)?;
        }
        Ok(collection)
    }

    /// Lists a workspace's collections ordered by position, creation time, and id.
    pub fn list_collections(&self, workspace_id: &str) -> Result<Vec<Collection>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "
            SELECT id, workspace_id, name, description, auth_json, variables_json,
                   pre_request_script, test_script, position
            FROM collections
            WHERE workspace_id = ?1
            ORDER BY position ASC, created_at ASC, id ASC
            ",
        )?;
        let rows = statement.query_map(params![workspace_id], stored_collection_row)?;

        let mut collections = Vec::new();
        for row in rows {
            let mut collection = row?.into_domain()?;
            self.load_collection_secrets(&mut collection)?;
            collections.push(collection);
        }

        Ok(collections)
    }

    /// Deletes a workspace's collection by id; returns whether a row was removed.
    pub fn delete_collection(
        &self,
        workspace_id: &WorkspaceId,
        id: &CollectionId,
    ) -> Result<bool, StorageError> {
        self.atomic(|transaction| {
            // Only the secrets of a collection this workspace owns; another workspace's are kept.
            let owned: bool = transaction.query_row(
                "SELECT EXISTS(SELECT 1 FROM collections WHERE workspace_id = ?1 AND id = ?2)",
                params![workspace_id.as_str(), id.as_str()],
                |row| row.get(0),
            )?;
            if owned {
                Self::delete_collection_secrets(transaction, id.as_str())?;
            }
            let affected_rows = transaction.execute(
                "DELETE FROM collections WHERE workspace_id = ?1 AND id = ?2",
                params![workspace_id.as_str(), id.as_str()],
            )?;
            Ok(affected_rows > 0)
        })
    }
}

/// Secret-store key of a collection's secret variable.
fn collection_secret_ref(collection_id: &str, key: &str) -> String {
    format!("collection.{collection_id}.{key}")
}

struct StoredCollection {
    id: String,
    workspace_id: String,
    name: String,
    description: String,
    auth_json: String,
    variables_json: String,
    pre_request_script: String,
    test_script: String,
    position: i64,
}

fn stored_collection_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredCollection> {
    Ok(StoredCollection {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        auth_json: row.get(4)?,
        variables_json: row.get(5)?,
        pre_request_script: row.get(6)?,
        test_script: row.get(7)?,
        position: row.get(8)?,
    })
}

impl StoredCollection {
    fn into_domain(self) -> Result<Collection, StorageError> {
        let mut collection = Collection::new(&self.id, &self.workspace_id, &self.name)?;
        collection.description = self.description;
        collection.auth = serde_json::from_str::<AuthConfig>(&self.auth_json)?;
        collection.variables =
            serde_json::from_str::<Vec<EnvironmentVariable>>(&self.variables_json)?;
        collection.pre_request_script = self.pre_request_script;
        collection.test_script = self.test_script;
        collection.position = self.position;
        Ok(collection)
    }
}
