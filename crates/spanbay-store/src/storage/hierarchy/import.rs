//! Collection import: id occupancy across workspaces and the all-or-nothing save of a
//! collection with its folders and requests.

use std::collections::{HashMap, HashSet, VecDeque};

use spanbay_model::models::{Collection, CollectionId, Folder, FolderId, Request, RequestId};

use crate::storage::{PlacementFault, StorageError, WorkspaceStore};

impl WorkspaceStore {
    /// Whether any workspace already owns the collection, folder or request id.
    ///
    /// Row ids are global, so an importer picking fresh ids must see rows of every workspace.
    pub fn import_ids_occupied(
        &self,
        collection_id: &CollectionId,
        folder_ids: &[FolderId],
        request_ids: &[RequestId],
    ) -> Result<bool, StorageError> {
        if self.row_id_exists("collections", collection_id.as_str())? {
            return Ok(true);
        }
        for id in folder_ids {
            if self.row_id_exists("folders", id.as_str())? {
                return Ok(true);
            }
        }
        for id in request_ids {
            if self.row_id_exists("requests", id.as_str())? {
                return Ok(true);
            }
        }
        Ok(false)
    }

    /// Atomically saves an imported collection with its folders (parents first) and requests.
    ///
    /// Folders are checked and saved before any request; a cycle is detected after the
    /// reachable folders are saved, and each request is checked just before it is saved, so
    /// with several faults the first one in that order is reported.
    pub fn save_imported_collection(
        &self,
        collection: &Collection,
        folders: &[Folder],
        requests: &[Request],
    ) -> Result<(), StorageError> {
        self.atomic(|transaction| {
            Self::save_collection_on(transaction, collection)?;

            let folder_indices = index_import_folders(collection, folders)?;
            let order = folder_save_order(folders, &folder_indices)?;
            for &index in &order {
                self.save_folder(&folders[index])?;
            }
            if order.len() != folders.len() {
                return Err(invalid_folder(
                    "imported collection contains a folder cycle".into(),
                ));
            }

            let mut request_ids = HashSet::new();
            for request in requests {
                check_import_request(collection, &folder_indices, &mut request_ids, request)?;
                self.save_request(request)?;
            }

            Ok(())
        })
    }
}

fn invalid_folder(reason: String) -> StorageError {
    StorageError::InvalidFolderMove {
        reason,
        fault: PlacementFault::Invalid,
    }
}

fn invalid_request(reason: &str) -> StorageError {
    StorageError::InvalidRequestPlacement {
        reason: reason.into(),
        fault: PlacementFault::Invalid,
    }
}

/// Each imported folder's index by id; rejects a folder of another collection or a repeated id.
fn index_import_folders<'a>(
    collection: &Collection,
    folders: &'a [Folder],
) -> Result<HashMap<&'a str, usize>, StorageError> {
    let mut folder_indices = HashMap::new();
    for (index, folder) in folders.iter().enumerate() {
        if folder.collection_id != collection.id {
            return Err(invalid_folder(
                "imported folder must belong to the imported collection".into(),
            ));
        }
        if folder_indices.insert(folder.id.as_str(), index).is_some() {
            return Err(invalid_folder(
                "imported collection contains duplicate folder IDs".into(),
            ));
        }
    }
    Ok(folder_indices)
}

/// The folder indices parents first (breadth first from the roots). Folders on a cycle are not
/// reachable and so are missing; the caller reports that. A parent outside the import fails.
fn folder_save_order(
    folders: &[Folder],
    folder_indices: &HashMap<&str, usize>,
) -> Result<Vec<usize>, StorageError> {
    let mut children = vec![Vec::new(); folders.len()];
    let mut ready = VecDeque::new();
    for (index, folder) in folders.iter().enumerate() {
        if let Some(parent_id) = &folder.parent_folder_id {
            let parent = folder_indices.get(parent_id.as_str()).ok_or_else(|| {
                invalid_folder(format!(
                    "imported folder parent {} not found",
                    parent_id.as_str()
                ))
            })?;
            children[*parent].push(index);
        } else {
            ready.push_back(index);
        }
    }

    let mut order = Vec::with_capacity(folders.len());
    while let Some(index) = ready.pop_front() {
        order.push(index);
        ready.extend(children[index].iter().copied());
    }
    Ok(order)
}

/// Rejects a request outside the imported collection and its folders, or a repeated id.
fn check_import_request<'a>(
    collection: &Collection,
    folder_indices: &HashMap<&str, usize>,
    seen: &mut HashSet<&'a str>,
    request: &'a Request,
) -> Result<(), StorageError> {
    if request.collection_id.as_ref() != Some(&collection.id)
        || request
            .folder_id
            .as_ref()
            .is_some_and(|id| !folder_indices.contains_key(id.as_str()))
    {
        return Err(invalid_request(
            "imported request must belong to the imported collection and its folders",
        ));
    }
    if !seen.insert(request.id.as_str()) {
        return Err(invalid_request(
            "imported collection contains duplicate request IDs",
        ));
    }
    Ok(())
}
