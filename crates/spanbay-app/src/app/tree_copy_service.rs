//! Copying a request, folder or collection, inside its workspace or into another one.
//!
//! Single responsibility: write a fresh-id copy of a stored subtree in one transaction. Secret
//! values travel with it (collection secret variables, a request's proxy password) because both
//! ends are this machine's store; nothing about the source changes. Flows, history and cookies
//! stay with the source.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::workspace::{
    Collection, CollectionId, Folder, FolderId, Request, RequestId, WorkspaceId, WorkspaceStore,
};

use super::factory_service::new_entity_id;
use super::tree_service::snapshot_collection;
use super::{AppState, command_error, ensure_workspace_exists};

/// What a copy duplicates.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CopyItemKind {
    Request,
    Folder,
    Collection,
}

/// Input of `tree_copy_apply`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyItemInput {
    /// Workspace that owns the source.
    pub workspace_id: String,
    pub kind: CopyItemKind,
    /// Id of the request, folder or collection to copy.
    pub id: String,
    /// Workspace the copy goes into (the source's own for a duplicate).
    pub target_workspace_id: String,
    /// Collection of the target workspace a request or folder goes into; a collection copy
    /// ignores it, a folder copy needs it, a request without it lands outside any collection.
    #[serde(default)]
    pub target_collection_id: Option<String>,
    /// Folder of that collection a request or folder goes into; `None` is the collection root.
    #[serde(default)]
    pub target_folder_id: Option<String>,
}

/// What a copy wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CopyItemResult {
    /// Workspace the copy is in.
    pub workspace_id: String,
    /// Id of the copied request, folder or collection.
    pub id: String,
    /// Collection the copy is in (the copy itself for a collection), if any.
    pub collection_id: Option<String>,
    /// Folders written (the copied folder and its subfolders, or a collection's folders).
    pub folders: usize,
    /// Requests written.
    pub requests: usize,
}

fn fresh_folder_id() -> Result<FolderId, AppError> {
    FolderId::new(new_entity_id("fold")).map_err(command_error)
}

/// The fresh id `map` gave `id`.
fn mapped(map: &HashMap<FolderId, FolderId>, id: &FolderId) -> Result<FolderId, AppError> {
    map.get(id)
        .cloned()
        .ok_or_else(|| AppError::internal(format!("folder {} has no copy", id.as_str())))
}

fn fresh_request_id() -> Result<RequestId, AppError> {
    RequestId::new(new_entity_id("req")).map_err(command_error)
}

/// `name`, or `name (copy)`, `name (copy 2)`, … when `taken` already uses it (ignoring case).
fn free_name(name: &str, taken: &[&str]) -> String {
    let used = |candidate: &str| {
        taken
            .iter()
            .any(|other| other.trim().eq_ignore_ascii_case(candidate.trim()))
    };
    if !used(name) {
        return name.to_owned();
    }
    let first = format!("{name} (copy)");
    if !used(&first) {
        return first;
    }
    (2_u32..)
        .map(|n| format!("{name} (copy {n})"))
        .find(|candidate| !used(candidate))
        .unwrap_or(first)
}

/// `requests` re-keyed into `collection_id` with fresh ids, folders mapped through `folders`
/// (a request in no mapped folder lands at `root`), in `workspace`.
fn copy_requests(
    requests: Vec<Request>,
    workspace: &WorkspaceId,
    collection_id: &CollectionId,
    folders: &HashMap<FolderId, FolderId>,
    root: Option<&FolderId>,
) -> Result<Vec<Request>, AppError> {
    requests
        .into_iter()
        .map(|mut request| {
            request.id = fresh_request_id()?;
            request.workspace_id = workspace.clone();
            request.collection_id = Some(collection_id.clone());
            request.folder_id = request
                .folder_id
                .as_ref()
                .and_then(|id| folders.get(id).cloned())
                .or_else(|| root.cloned());
            request.agent_touched_ms = None;
            Ok(request)
        })
        .collect()
}

/// Copies a request, folder (with its subfolders and requests) or collection (with everything
/// below it) into `target_workspace_id`, under fresh ids and in one transaction. A copy into the
/// source's own place is named `… (copy)`; a collection copy takes a free name in the target.
///
/// # Errors
/// `NotFound` for an unknown source, target workspace, collection or folder; `Validation` for a
/// folder copy without a target collection or into its own subtree.
pub fn tree_copy_impl(state: &AppState, input: CopyItemInput) -> Result<CopyItemResult, AppError> {
    let source_workspace = WorkspaceId::new(&input.workspace_id).map_err(command_error)?;
    let target_workspace = WorkspaceId::new(&input.target_workspace_id).map_err(command_error)?;
    let target_collection = input
        .target_collection_id
        .as_deref()
        .map(CollectionId::new)
        .transpose()
        .map_err(command_error)?;
    let target_folder = input
        .target_folder_id
        .as_deref()
        .map(FolderId::new)
        .transpose()
        .map_err(command_error)?;
    let store = state.store();
    ensure_workspace_exists(&store, target_workspace.as_str())?;
    // The destination collection and folder must belong to the target workspace.
    let destination = match &target_collection {
        Some(id) => Some(
            store
                .get_collection(&target_workspace, id)
                .map_err(command_error)?
                .ok_or_else(|| {
                    AppError::not_found(format!("collection {} not found", id.as_str()))
                })?,
        ),
        None => None,
    };
    if let Some(folder) = &target_folder {
        let found = store
            .get_folder(&target_workspace, folder)
            .map_err(command_error)?
            .filter(|found| Some(&found.collection_id) == target_collection.as_ref());
        if found.is_none() {
            return Err(AppError::not_found(format!(
                "folder {} not found in the target collection",
                folder.as_str()
            )));
        }
    }

    let result = match input.kind {
        CopyItemKind::Request => copy_request(
            &store,
            &source_workspace,
            &target_workspace,
            &input.id,
            destination.as_ref(),
            target_folder.as_ref(),
        )?,
        CopyItemKind::Folder => {
            let Some(destination) = destination else {
                return Err(AppError::validation(
                    "a folder copy needs a target collection",
                ));
            };
            copy_folder(
                &store,
                &source_workspace,
                &target_workspace,
                &input.id,
                destination,
                target_folder.as_ref(),
            )?
        }
        CopyItemKind::Collection => {
            copy_collection(&store, &source_workspace, &target_workspace, &input.id)?
        }
    };
    drop(store);
    state.sync.notify_changed();
    Ok(result)
}

fn copy_request(
    store: &WorkspaceStore,
    source_workspace: &WorkspaceId,
    target_workspace: &WorkspaceId,
    id: &str,
    destination: Option<&Collection>,
    target_folder: Option<&FolderId>,
) -> Result<CopyItemResult, AppError> {
    let request_id = RequestId::new(id).map_err(command_error)?;
    let mut request = store
        .get_request(source_workspace, &request_id)
        .map_err(command_error)?
        .ok_or_else(|| AppError::not_found(format!("request {id} not found")))?;
    let same_place = source_workspace == target_workspace
        && request.collection_id.as_ref() == destination.map(|collection| &collection.id)
        && request.folder_id.as_ref() == target_folder;
    if same_place {
        request.name = format!("{} (copy)", request.name);
        request.position += 1;
    }
    request.id = fresh_request_id()?;
    request.workspace_id = target_workspace.clone();
    request.collection_id = destination.map(|collection| collection.id.clone());
    request.folder_id = target_folder.cloned();
    request.agent_touched_ms = None;
    store.save_request(&request).map_err(command_error)?;
    Ok(CopyItemResult {
        workspace_id: target_workspace.as_str().to_owned(),
        id: request.id.as_str().to_owned(),
        collection_id: request.collection_id.map(|id| id.as_str().to_owned()),
        folders: 0,
        requests: 1,
    })
}

fn copy_folder(
    store: &WorkspaceStore,
    source_workspace: &WorkspaceId,
    target_workspace: &WorkspaceId,
    id: &str,
    destination: Collection,
    target_folder: Option<&FolderId>,
) -> Result<CopyItemResult, AppError> {
    let folder_id = FolderId::new(id).map_err(command_error)?;
    let root = store
        .get_folder(source_workspace, &folder_id)
        .map_err(command_error)?
        .ok_or_else(|| AppError::not_found(format!("folder {id} not found")))?;
    let source_folders = store
        .list_folders(source_workspace, &root.collection_id)
        .map_err(command_error)?;
    // The subtree, parents first.
    let mut subtree = vec![root.clone()];
    let mut index = 0;
    while index < subtree.len() {
        let parent = subtree[index].id.clone();
        subtree.extend(
            source_folders
                .iter()
                .filter(|folder| folder.parent_folder_id.as_ref() == Some(&parent))
                .cloned(),
        );
        index += 1;
    }
    if let Some(target) = target_folder
        && subtree.iter().any(|folder| &folder.id == target)
    {
        return Err(AppError::validation(
            "a folder cannot be copied into itself",
        ));
    }
    let mut map: HashMap<FolderId, FolderId> = HashMap::new();
    for folder in &subtree {
        map.insert(folder.id.clone(), fresh_folder_id()?);
    }
    let target_folders = store
        .list_folders(target_workspace, &destination.id)
        .map_err(command_error)?;
    let siblings: Vec<&str> = target_folders
        .iter()
        .filter(|folder| folder.parent_folder_id.as_ref() == target_folder)
        .map(|folder| folder.name.as_str())
        .collect();
    let copied_name = free_name(&root.name, &siblings);
    let next_position = target_folders
        .iter()
        .filter(|folder| folder.parent_folder_id.as_ref() == target_folder)
        .map(|folder| folder.position + 1)
        .max()
        .unwrap_or(0);
    let mut copies: Vec<Folder> = Vec::with_capacity(subtree.len());
    for mut folder in subtree {
        let is_root = folder.id == root.id;
        folder.parent_folder_id = if is_root {
            target_folder.cloned()
        } else {
            folder
                .parent_folder_id
                .as_ref()
                .and_then(|parent| map.get(parent).cloned())
        };
        folder.id = mapped(&map, &folder.id)?;
        folder.collection_id = destination.id.clone();
        if is_root {
            folder.name.clone_from(&copied_name);
            folder.position = next_position;
        }
        copies.push(folder);
    }
    let requests = store
        .list_requests(source_workspace.as_str())
        .map_err(command_error)?
        .into_iter()
        .filter(|request| {
            request
                .folder_id
                .as_ref()
                .is_some_and(|folder| map.contains_key(folder))
        })
        .collect();
    let requests = copy_requests(requests, target_workspace, &destination.id, &map, None)?;
    // The save covers the destination's folders too: a copied folder's parent must be in it.
    let mut folders = target_folders;
    let copied_folders = copies.len();
    let new_root = copies
        .first()
        .map(|folder| folder.id.as_str().to_owned())
        .unwrap_or_default();
    folders.extend(copies);
    store
        .save_imported_collection(&destination, &folders, &requests)
        .map_err(command_error)?;
    Ok(CopyItemResult {
        workspace_id: target_workspace.as_str().to_owned(),
        id: new_root,
        collection_id: Some(destination.id.as_str().to_owned()),
        folders: copied_folders,
        requests: requests.len(),
    })
}

fn copy_collection(
    store: &WorkspaceStore,
    source_workspace: &WorkspaceId,
    target_workspace: &WorkspaceId,
    id: &str,
) -> Result<CopyItemResult, AppError> {
    let collection_id = CollectionId::new(id).map_err(command_error)?;
    let (mut collection, folders, requests) =
        snapshot_collection(store, source_workspace, &collection_id)?
            .ok_or_else(|| AppError::not_found(format!("collection {id} not found")))?;
    let existing = store
        .list_collections(target_workspace.as_str())
        .map_err(command_error)?;
    let names: Vec<&str> = existing.iter().map(|other| other.name.as_str()).collect();
    collection.name = free_name(&collection.name, &names);
    collection.position = existing
        .iter()
        .map(|other| other.position + 1)
        .max()
        .unwrap_or(0);
    collection.id = CollectionId::new(new_entity_id("col")).map_err(command_error)?;
    collection.workspace_id = target_workspace.clone();
    let mut map: HashMap<FolderId, FolderId> = HashMap::new();
    for folder in &folders {
        map.insert(folder.id.clone(), fresh_folder_id()?);
    }
    let mut copies: Vec<Folder> = Vec::with_capacity(folders.len());
    for mut folder in folders {
        folder.parent_folder_id = folder
            .parent_folder_id
            .as_ref()
            .and_then(|parent| map.get(parent).cloned());
        folder.id = mapped(&map, &folder.id)?;
        folder.collection_id = collection.id.clone();
        copies.push(folder);
    }
    let folders = copies;
    let requests = copy_requests(requests, target_workspace, &collection.id, &map, None)?;
    store
        .save_imported_collection(&collection, &folders, &requests)
        .map_err(command_error)?;
    Ok(CopyItemResult {
        workspace_id: target_workspace.as_str().to_owned(),
        id: collection.id.as_str().to_owned(),
        collection_id: Some(collection.id.as_str().to_owned()),
        folders: folders.len(),
        requests: requests.len(),
    })
}
