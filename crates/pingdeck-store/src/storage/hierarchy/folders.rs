//! Folder CRUD, parent cycle checks and cross-collection subtree moves.

use std::collections::HashSet;

use rusqlite::{Connection, OptionalExtension, params};

use pingdeck_model::hierarchy::FolderMoveResult;
use pingdeck_model::models::{AuthConfig, CollectionId, Folder, FolderId, RequestId, WorkspaceId};

use crate::storage::{PlacementFault, StorageError, WorkspaceStore};

impl WorkspaceStore {
    /// Upserts a folder after rejecting collection changes, missing parents, and parent cycles.
    pub fn save_folder(&self, folder: &Folder) -> Result<(), StorageError> {
        self.ensure_collection_unchanged(folder)?;
        self.ensure_parent_chain(folder)?;

        self.connection.execute(
            "
            INSERT INTO folders (
                id, collection_id, parent_folder_id, name, description, auth_json,
                pre_request_script, test_script, position, updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            ON CONFLICT(id) DO UPDATE SET
                collection_id = excluded.collection_id,
                parent_folder_id = excluded.parent_folder_id,
                name = excluded.name,
                description = excluded.description,
                auth_json = excluded.auth_json,
                pre_request_script = excluded.pre_request_script,
                test_script = excluded.test_script,
                position = excluded.position,
                updated_at = excluded.updated_at
            ",
            params![
                folder.id.as_str(),
                folder.collection_id.as_str(),
                folder.parent_folder_id.as_ref().map(FolderId::as_str),
                folder.name,
                folder.description,
                serde_json::to_string(&folder.auth)?,
                folder.pre_request_script,
                folder.test_script,
                folder.position,
            ],
        )?;

        Ok(())
    }

    /// Rejects a save that would change an existing folder's collection: a folder id of another
    /// workspace is an ownership clash, anything else must go through `move_folder_tree`.
    fn ensure_collection_unchanged(&self, folder: &Folder) -> Result<(), StorageError> {
        let existing_collection: Option<String> = self
            .connection
            .query_row(
                "SELECT collection_id FROM folders WHERE id = ?1",
                params![folder.id.as_str()],
                |row| row.get(0),
            )
            .optional()?;
        if let Some(existing_collection) = existing_collection
            .as_deref()
            .filter(|id| *id != folder.collection_id.as_str())
        {
            let target_workspace = self.collection_workspace_id(folder.collection_id.as_str())?;
            if target_workspace.is_some()
                && self.collection_workspace_id(existing_collection)? != target_workspace
            {
                return Err(StorageError::ItemWorkspaceMismatch {
                    kind: "folder",
                    id: folder.id.as_str().to_owned(),
                });
            }
            return Err(StorageError::InvalidFolderMove {
                reason: "use move_folder_tree to change a folder's collection".into(),
                fault: PlacementFault::Invalid,
            });
        }
        Ok(())
    }

    /// Walks the parent chain of `folder`: every parent must exist in the same collection and
    /// the chain must not loop back.
    fn ensure_parent_chain(&self, folder: &Folder) -> Result<(), StorageError> {
        let mut parent_id = folder.parent_folder_id.clone();
        let mut seen = HashSet::new();
        while let Some(id) = parent_id {
            if id == folder.id || !seen.insert(id.clone()) {
                return Err(StorageError::InvalidFolderMove {
                    reason: "parent folder would create or contain a cycle".into(),
                    fault: PlacementFault::Invalid,
                });
            }
            let parent: Option<(String, Option<String>)> = self
                .connection
                .query_row(
                    "SELECT collection_id, parent_folder_id FROM folders WHERE id = ?1",
                    params![id.as_str()],
                    |row| Ok((row.get(0)?, row.get(1)?)),
                )
                .optional()?;
            let Some((collection_id, next_parent)) = parent else {
                return Err(StorageError::InvalidFolderMove {
                    reason: format!("parent folder {} was not found", id.as_str()),
                    fault: PlacementFault::NotFound,
                });
            };
            if collection_id != folder.collection_id.as_str() {
                return Err(StorageError::InvalidFolderMove {
                    reason: "parent folder must belong to the same collection".into(),
                    fault: PlacementFault::Conflict,
                });
            }
            parent_id = next_parent.map(FolderId::new).transpose()?;
        }
        Ok(())
    }

    /// Loads one folder of a workspace, or `None` when it does not exist there.
    pub fn get_folder(
        &self,
        workspace_id: &WorkspaceId,
        id: &FolderId,
    ) -> Result<Option<Folder>, StorageError> {
        let row = self
            .connection
            .query_row(
                "
                SELECT id, collection_id, parent_folder_id, name, description, auth_json,
                       pre_request_script, test_script, position
                FROM folders
                WHERE id = ?2
                  AND collection_id IN (SELECT id FROM collections WHERE workspace_id = ?1)
                ",
                params![workspace_id.as_str(), id.as_str()],
                stored_folder_row,
            )
            .optional()?;

        row.map(StoredFolder::into_domain).transpose()
    }

    /// Lists a workspace collection's folders ordered by position, creation time, and id.
    ///
    /// A collection that is unknown or belongs to another workspace has no folders here.
    pub fn list_folders(
        &self,
        workspace_id: &WorkspaceId,
        collection_id: &CollectionId,
    ) -> Result<Vec<Folder>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "
            SELECT id, collection_id, parent_folder_id, name, description, auth_json,
                   pre_request_script, test_script, position
            FROM folders
            WHERE collection_id = ?2
              AND collection_id IN (SELECT id FROM collections WHERE workspace_id = ?1)
            ORDER BY position ASC, created_at ASC, id ASC
            ",
        )?;
        let rows = statement.query_map(
            params![workspace_id.as_str(), collection_id.as_str()],
            stored_folder_row,
        )?;

        let mut folders = Vec::new();
        for row in rows {
            folders.push(row?.into_domain()?);
        }

        Ok(folders)
    }

    /// Deletes a workspace's folder by id; returns whether a row was removed.
    pub fn delete_folder(
        &self,
        workspace_id: &WorkspaceId,
        id: &FolderId,
    ) -> Result<bool, StorageError> {
        let affected_rows = self.connection.execute(
            "
            DELETE FROM folders
            WHERE id = ?2
              AND collection_id IN (SELECT id FROM collections WHERE workspace_id = ?1)
            ",
            params![workspace_id.as_str(), id.as_str()],
        )?;

        Ok(affected_rows > 0)
    }

    /// Workspace that owns a collection, or `None` when the collection does not exist.
    pub fn collection_workspace_id(
        &self,
        collection_id: &str,
    ) -> Result<Option<WorkspaceId>, StorageError> {
        self.connection
            .query_row(
                "SELECT workspace_id FROM collections WHERE id = ?1",
                params![collection_id],
                |row| row.get::<_, String>(0),
            )
            .optional()?
            .map(WorkspaceId::new)
            .transpose()
            .map_err(StorageError::from)
    }

    /// Moves a workspace's folder subtree and its requests to another collection of the same
    /// workspace; `None` when the folder does not exist in that workspace.
    pub fn move_folder_tree(
        &self,
        workspace_id: &WorkspaceId,
        folder_id: &FolderId,
        target_collection_id: &CollectionId,
        target_parent_folder_id: Option<&FolderId>,
    ) -> Result<Option<FolderMoveResult>, StorageError> {
        let Some((moved_folder_ids, affected_request_ids)) = self.atomic(|transaction| {
            let Some(folder_to_move) = self.get_folder(workspace_id, folder_id)? else {
                return Ok(None);
            };
            self.check_target_collection(workspace_id, target_collection_id)?;
            let source_folders = self.list_folders(workspace_id, &folder_to_move.collection_id)?;
            let moved_folder_ids = folder_and_descendant_ids(&source_folders, folder_id);
            if let Some(target_parent_folder_id) = target_parent_folder_id {
                self.check_target_parent(
                    workspace_id,
                    target_collection_id,
                    target_parent_folder_id,
                    &moved_folder_ids,
                )?;
            }

            let affected_request_ids = request_ids_under(transaction, &moved_folder_ids)?;
            reparent_subtree(
                transaction,
                folder_id,
                &moved_folder_ids,
                target_collection_id,
                target_parent_folder_id,
            )?;
            Ok(Some((moved_folder_ids, affected_request_ids)))
        })?
        else {
            return Ok(None);
        };

        self.load_move_result(workspace_id, moved_folder_ids, affected_request_ids)
            .map(Some)
    }

    /// A subtree may only move to an existing collection of the same workspace.
    fn check_target_collection(
        &self,
        workspace_id: &WorkspaceId,
        target_collection_id: &CollectionId,
    ) -> Result<(), StorageError> {
        match self.collection_workspace_id(target_collection_id.as_str())? {
            None => Err(StorageError::InvalidFolderMove {
                reason: format!(
                    "target collection {} was not found",
                    target_collection_id.as_str()
                ),
                fault: PlacementFault::NotFound,
            }),
            Some(target_workspace) if target_workspace != *workspace_id => {
                Err(StorageError::InvalidFolderMove {
                    reason: "target collection must belong to the same workspace".into(),
                    fault: PlacementFault::Conflict,
                })
            }
            Some(_) => Ok(()),
        }
    }

    /// The new parent of a subtree must be a folder of the target collection outside the
    /// subtree.
    fn check_target_parent(
        &self,
        workspace_id: &WorkspaceId,
        target_collection_id: &CollectionId,
        target_parent_folder_id: &FolderId,
        moved_folder_ids: &[FolderId],
    ) -> Result<(), StorageError> {
        if moved_folder_ids.contains(target_parent_folder_id) {
            return Err(StorageError::InvalidFolderMove {
                reason: "target parent cannot be the moved folder or its descendant".into(),
                fault: PlacementFault::Invalid,
            });
        }
        match self.get_folder(workspace_id, target_parent_folder_id)? {
            Some(target_parent_folder)
                if target_parent_folder.collection_id == *target_collection_id =>
            {
                Ok(())
            }
            Some(_) => Err(StorageError::InvalidFolderMove {
                reason: "target parent folder must belong to the target collection".into(),
                fault: PlacementFault::Conflict,
            }),
            None => Err(StorageError::InvalidFolderMove {
                reason: format!(
                    "target parent folder {} was not found",
                    target_parent_folder_id.as_str()
                ),
                fault: PlacementFault::NotFound,
            }),
        }
    }

    /// Reloads the moved folders and requests (in the given orders) after the move commits.
    fn load_move_result(
        &self,
        workspace_id: &WorkspaceId,
        folder_ids: Vec<FolderId>,
        request_ids: Vec<RequestId>,
    ) -> Result<FolderMoveResult, StorageError> {
        let mut folders = Vec::new();
        for folder_id in folder_ids {
            if let Some(folder) = self.get_folder(workspace_id, &folder_id)? {
                folders.push(folder);
            }
        }

        let mut requests = Vec::new();
        for request_id in request_ids {
            if let Some(request) = self.get_request(workspace_id, &request_id)? {
                requests.push(request);
            }
        }

        Ok(FolderMoveResult { folders, requests })
    }
}

/// The requests directly in each of `folder_ids`, folder by folder in that order, then by id.
fn request_ids_under(
    connection: &Connection,
    folder_ids: &[FolderId],
) -> Result<Vec<RequestId>, StorageError> {
    let mut statement = connection.prepare_cached(
        "
                    SELECT id
                    FROM requests
                    WHERE folder_id = ?1
                    ORDER BY id ASC
                    ",
    )?;
    let mut request_ids = Vec::new();
    for folder_id in folder_ids {
        let rows =
            statement.query_map(params![folder_id.as_str()], |row| row.get::<_, String>(0))?;
        for row in rows {
            request_ids.push(RequestId::new(row?)?);
        }
    }
    Ok(request_ids)
}

/// Points the subtree at its new place: the root `folder_id` takes the target collection and
/// parent, its descendants and every request in the subtree take the target collection.
fn reparent_subtree(
    connection: &Connection,
    folder_id: &FolderId,
    moved_folder_ids: &[FolderId],
    target_collection_id: &CollectionId,
    target_parent_folder_id: Option<&FolderId>,
) -> Result<(), StorageError> {
    connection.execute(
        "
                UPDATE folders
                SET
                    collection_id = ?1,
                    parent_folder_id = ?2,
                    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                WHERE id = ?3
                ",
        params![
            target_collection_id.as_str(),
            target_parent_folder_id.map(FolderId::as_str),
            folder_id.as_str(),
        ],
    )?;

    let mut move_folder = connection.prepare(
        "
                    UPDATE folders
                    SET
                        collection_id = ?1,
                        updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                    WHERE id = ?2
                    ",
    )?;
    for moved_folder_id in moved_folder_ids
        .iter()
        .filter(|moved_folder_id| *moved_folder_id != folder_id)
    {
        move_folder.execute(params![
            target_collection_id.as_str(),
            moved_folder_id.as_str()
        ])?;
    }

    let mut move_requests = connection.prepare(
        "
                    UPDATE requests
                    SET
                        collection_id = ?1,
                        updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
                    WHERE folder_id = ?2
                    ",
    )?;
    for moved_folder_id in moved_folder_ids {
        move_requests.execute(params![
            target_collection_id.as_str(),
            moved_folder_id.as_str()
        ])?;
    }
    Ok(())
}

struct StoredFolder {
    id: String,
    collection_id: String,
    parent_folder_id: Option<String>,
    name: String,
    description: String,
    auth_json: String,
    pre_request_script: String,
    test_script: String,
    position: i64,
}

fn stored_folder_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredFolder> {
    Ok(StoredFolder {
        id: row.get(0)?,
        collection_id: row.get(1)?,
        parent_folder_id: row.get(2)?,
        name: row.get(3)?,
        description: row.get(4)?,
        auth_json: row.get(5)?,
        pre_request_script: row.get(6)?,
        test_script: row.get(7)?,
        position: row.get(8)?,
    })
}

impl StoredFolder {
    fn into_domain(self) -> Result<Folder, StorageError> {
        let mut folder = Folder::new(&self.id, &self.collection_id, &self.name)?;
        if let Some(parent_folder_id) = self.parent_folder_id {
            folder = folder.with_parent(parent_folder_id)?;
        }
        folder.description = self.description;
        folder.auth = serde_json::from_str::<AuthConfig>(&self.auth_json)?;
        folder.pre_request_script = self.pre_request_script;
        folder.test_script = self.test_script;
        folder.position = self.position;
        Ok(folder)
    }
}

fn folder_and_descendant_ids(folders: &[Folder], folder_id: &FolderId) -> Vec<FolderId> {
    let mut folder_ids = vec![folder_id.clone()];
    let mut changed = true;

    while changed {
        changed = false;

        for folder in folders {
            if folder
                .parent_folder_id
                .as_ref()
                .is_some_and(|parent_folder_id| folder_ids.contains(parent_folder_id))
                && !folder_ids.contains(&folder.id)
            {
                folder_ids.push(folder.id.clone());
                changed = true;
            }
        }
    }

    folder_ids
}
