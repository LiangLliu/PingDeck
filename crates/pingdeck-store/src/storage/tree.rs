//! Bulk reads and re-linking for the Requests pane: the whole tree as small summaries, and the
//! request-to-folder links a folder delete drops (`requests.folder_id` is `ON DELETE SET NULL`).

use rusqlite::params;

use pingdeck_model::ai_request::RequestMode;
use pingdeck_model::hierarchy::{
    CollectionSummary, FolderSummary, RequestSummary, URL_SUMMARY_MAX_CHARS, WorkspaceTree,
};
use pingdeck_model::models::{CollectionId, FolderId, RequestId, WorkspaceId};

use super::requests::{http_method_from_storage, protocol_from_storage};
use super::{StorageError, WorkspaceStore};
use crate::sqlite::i64_from_usize;

impl WorkspaceStore {
    /// Reads the whole tree in three queries, without any body.
    pub fn workspace_tree(
        &self,
        workspace_id: &WorkspaceId,
    ) -> Result<WorkspaceTree, StorageError> {
        let workspace = workspace_id.as_str();
        Ok(WorkspaceTree {
            collections: self.collection_summaries(workspace)?,
            folders: self.folder_summaries(workspace)?,
            requests: self.request_summaries(workspace)?,
        })
    }

    fn collection_summaries(
        &self,
        workspace: &str,
    ) -> Result<Vec<CollectionSummary>, StorageError> {
        let collections = self
            .connection
            .prepare_cached(
                "SELECT id, name, position FROM collections WHERE workspace_id = ?1
                 ORDER BY position ASC, created_at ASC, id ASC",
            )?
            .query_map(params![workspace], |row| {
                Ok(CollectionSummary {
                    id: row.get(0)?,
                    name: row.get(1)?,
                    position: row.get(2)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(collections)
    }

    fn folder_summaries(&self, workspace: &str) -> Result<Vec<FolderSummary>, StorageError> {
        let folders = self
            .connection
            .prepare_cached(
                "SELECT f.id, f.collection_id, f.parent_folder_id, f.name, f.position
                 FROM folders f JOIN collections c ON c.id = f.collection_id
                 WHERE c.workspace_id = ?1
                 ORDER BY f.position ASC, f.created_at ASC, f.id ASC",
            )?
            .query_map(params![workspace], |row| {
                Ok(FolderSummary {
                    id: row.get(0)?,
                    collection_id: row.get(1)?,
                    parent_folder_id: row.get(2)?,
                    name: row.get(3)?,
                    position: row.get(4)?,
                })
            })?
            .collect::<Result<Vec<_>, _>>()?;
        Ok(folders)
    }

    fn request_summaries(&self, workspace: &str) -> Result<Vec<RequestSummary>, StorageError> {
        // `substr` counts characters, so the cut never splits a code point.
        let mut statement = self.connection.prepare_cached(
            "SELECT id, collection_id, folder_id, name, protocol, method, mode,
                    substr(url, 1, ?2), position, agent_touched_ms
             FROM requests WHERE workspace_id = ?1
             ORDER BY position ASC, created_at ASC, id ASC",
        )?;
        let rows = statement.query_map(
            params![workspace, i64_from_usize(URL_SUMMARY_MAX_CHARS)],
            |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, Option<String>>(1)?,
                    row.get::<_, Option<String>>(2)?,
                    row.get::<_, String>(3)?,
                    row.get::<_, String>(4)?,
                    row.get::<_, String>(5)?,
                    row.get::<_, String>(6)?,
                    row.get::<_, String>(7)?,
                    row.get::<_, i64>(8)?,
                    row.get::<_, Option<i64>>(9)?,
                ))
            },
        )?;
        let mut requests = Vec::new();
        for row in rows {
            let (
                id,
                collection_id,
                folder_id,
                name,
                protocol,
                method,
                mode,
                url,
                position,
                touched,
            ) = row?;
            requests.push(RequestSummary {
                // Protocol and method are strict (an unknown value is a storage error); the mode
                // is kept lenient as before: an unknown mode falls back to the default.
                protocol: protocol_from_storage(&id, &protocol)?,
                method: http_method_from_storage(&id, &method)?,
                mode: RequestMode::parse(&mode).unwrap_or_default(),
                id,
                collection_id,
                folder_id,
                name,
                url,
                position,
                agent_touched_ms: touched,
            });
        }
        Ok(requests)
    }

    /// The requests of a collection that sit in a folder, paired with that folder: what a folder
    /// delete is about to unlink.
    pub fn folder_request_links(
        &self,
        workspace_id: &WorkspaceId,
        collection_id: &CollectionId,
    ) -> Result<Vec<(RequestId, FolderId)>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id, folder_id FROM requests
             WHERE workspace_id = ?1 AND collection_id = ?2 AND folder_id IS NOT NULL
             ORDER BY position ASC, created_at ASC, id ASC",
        )?;
        let rows = statement.query_map(
            params![workspace_id.as_str(), collection_id.as_str()],
            |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
        )?;
        let mut links = Vec::new();
        for row in rows {
            let (request_id, folder_id) = row?;
            links.push((RequestId::new(request_id)?, FolderId::new(folder_id)?));
        }
        Ok(links)
    }

    /// Puts a request that now sits at the collection root back into `folder_id`; returns whether
    /// it did. Nothing changes when the request is gone, moved elsewhere, or is not in the
    /// folder's collection.
    pub fn relink_request_folder(
        &self,
        workspace_id: &WorkspaceId,
        request_id: &RequestId,
        folder_id: &FolderId,
    ) -> Result<bool, StorageError> {
        let changed = self.connection.execute(
            "UPDATE requests SET folder_id = ?3,
                    updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
             WHERE workspace_id = ?1 AND id = ?2 AND folder_id IS NULL
               AND collection_id = (SELECT collection_id FROM folders WHERE id = ?3)",
            params![
                workspace_id.as_str(),
                request_id.as_str(),
                folder_id.as_str()
            ],
        )?;
        Ok(changed > 0)
    }
}
