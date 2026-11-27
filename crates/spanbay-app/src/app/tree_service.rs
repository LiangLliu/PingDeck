//! Requests-pane use-cases: the workspace tree in one call, single reads, deletes with undo.
//!
//! Single responsibility: everything the collection tree needs that is not a plain save. A
//! delete lifts the removed subtree (a deleted collection's secret variable values included) into
//! the in-memory [`DeleteUndoStore`] for up to 60 s (the toast dismissal shortens it to 1 s, as for
//! environments), and `restore_deleted` writes it back. Folder deletes cascade to subfolders but
//! only unlink their requests (`folder_id` is `ON DELETE SET NULL`); a restore re-links them.
//! A collection delete removes everything below it. The history rows that pointed at a deleted
//! request lose that link (`ON DELETE SET NULL`) for good.

use std::collections::HashSet;
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::workspace::hierarchy::WorkspaceTree;
use crate::workspace::{
    Collection, CollectionId, Folder, FolderId, Request, RequestId, StorageError, WorkspaceId,
    WorkspaceStore, dynamic_variable_names,
};

use super::undo_store::{UndoHeld, UndoStore, UndoToken};
use super::{AppState, CollectionDto, command_error, ensure_workspace_exists};

/// What a delete removed, and what a restore brought back or failed to bring back.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DeletedKind {
    /// A single request.
    Request,
    /// A folder (with its subfolders).
    Folder,
    /// A collection (with everything below it).
    Collection,
}

/// One thing a restore could not bring back.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreFailureDto {
    /// Id of the request, folder or collection.
    pub id: String,
    /// What the id names.
    pub kind: DeletedKind,
    /// Why it failed (an id already taken, a parent that is gone, a storage error).
    pub message: String,
}

/// The outcome of `restore_deleted`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RestoreReportDto {
    /// Ids brought back: the collection, its folders and requests, or the folders and the
    /// requests that were re-linked into them, or the request.
    pub restored: Vec<String>,
    /// Everything that could not be brought back.
    pub failed: Vec<RestoreFailureDto>,
}

/// What is held for one delete.
#[derive(Clone)]
enum Held {
    Request(Box<Request>),
    /// The folder and its subfolders (parents first) and the requests that were in any of them.
    Folder {
        folders: Vec<Folder>,
        links: Vec<(RequestId, FolderId)>,
    },
    /// The collection with its secret values, every folder (parents first) and every request.
    Collection {
        collection: Box<Collection>,
        folders: Vec<Folder>,
        requests: Vec<Request>,
    },
    /// An import's "Update" (or id-keeping "Replace") into a stored collection: that collection
    /// as it was (a `Collection`, not deleted), what the import added, which a restore deletes
    /// before it writes the old state back, and the other collections of the same name a
    /// "Replace" removed.
    Updated {
        before: Box<Held>,
        added_folders: Vec<FolderId>,
        added_requests: Vec<RequestId>,
        removed: Vec<Held>,
    },
}

/// A deleted request, folder or collection, secret values included, held for the Undo toast.
#[derive(Clone)]
pub(crate) struct DeletedSubtree {
    workspace_id: WorkspaceId,
    held: Held,
}

impl UndoHeld for DeletedSubtree {
    fn workspace_id(&self) -> &WorkspaceId {
        &self.workspace_id
    }
}

/// Deleted requests, folders and collections restorable for up to 60 s.
pub(crate) type DeleteUndoStore = UndoStore<DeletedSubtree>;

/// The names of the `{{$name}}` variables the resolver generates.
#[must_use]
pub fn list_dynamic_variable_names_impl() -> Vec<String> {
    dynamic_variable_names()
        .iter()
        .map(|name| (*name).to_owned())
        .collect()
}

fn not_found(kind: &str, id: &str, workspace_id: &str) -> AppError {
    AppError::not_found(format!("{kind} {id} not found in workspace {workspace_id}"))
}

/// Every collection, folder and request of a workspace as summaries, in one call (no bodies, URL
/// cut at 256 characters). An unknown workspace is `NotFound`.
pub fn workspace_tree_impl(
    state: &AppState,
    workspace_id: String,
) -> Result<WorkspaceTree, AppError> {
    let store = state.store();
    ensure_workspace_exists(&store, &workspace_id)?;
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    store.workspace_tree(&workspace).map_err(command_error)
}

/// One saved request; an unknown id, or one owned by another workspace, is `NotFound`.
pub fn request_get_impl(
    state: &AppState,
    workspace_id: String,
    id: String,
) -> Result<Request, AppError> {
    let request_id = RequestId::new(&id).map_err(command_error)?;
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    state
        .store()
        .get_request(&workspace, &request_id)
        .map_err(command_error)?
        .map(super::workspace_service::with_request_revision)
        .ok_or_else(|| not_found("request", &id, &workspace_id))
}

/// One collection with its secret variable values redacted; an unknown id, or one owned by
/// another workspace, is `NotFound`.
pub fn collection_get_impl(
    state: &AppState,
    workspace_id: String,
    id: String,
) -> Result<CollectionDto, AppError> {
    let collection_id = CollectionId::new(&id).map_err(command_error)?;
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    state
        .store()
        .get_collection(&workspace, &collection_id)
        .map_err(command_error)?
        .map(|collection| CollectionDto::from(&collection))
        .ok_or_else(|| not_found("collection", &id, &workspace_id))
}

/// One folder with its revision; an unknown id, or one owned by another workspace, is
/// `NotFound`.
pub fn folder_get_impl(
    state: &AppState,
    workspace_id: String,
    id: String,
) -> Result<Folder, AppError> {
    let folder_id = FolderId::new(&id).map_err(command_error)?;
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    state
        .store()
        .get_folder(&workspace, &folder_id)
        .map_err(command_error)?
        .map(super::workspace_service::with_folder_revision)
        .ok_or_else(|| not_found("folder", &id, &workspace_id))
}

/// `seeds` followed by every folder of `all` below them, parents before children.
fn parents_first(all: &[Folder], seeds: Vec<Folder>) -> Vec<Folder> {
    let mut ordered = seeds;
    let mut next = 0;
    while next < ordered.len() {
        let parent = ordered[next].id.clone();
        ordered.extend(
            all.iter()
                .filter(|folder| folder.parent_folder_id.as_ref() == Some(&parent))
                .cloned(),
        );
        next += 1;
    }
    ordered
}

/// Reads what deleting `id` removes, then deletes it. The caller holds the store lock, so
/// nothing changes in between.
fn capture_and_delete(
    store: &WorkspaceStore,
    workspace: &WorkspaceId,
    kind: DeletedKind,
    id: &str,
) -> Result<Option<Held>, AppError> {
    match kind {
        DeletedKind::Request => {
            let request_id = RequestId::new(id).map_err(command_error)?;
            let Some(request) = store
                .get_request(workspace, &request_id)
                .map_err(command_error)?
            else {
                return Ok(None);
            };
            store
                .delete_request(workspace, &request_id)
                .map_err(command_error)?;
            Ok(Some(Held::Request(Box::new(request))))
        }
        DeletedKind::Folder => {
            let folder_id = FolderId::new(id).map_err(command_error)?;
            let Some(root) = store
                .get_folder(workspace, &folder_id)
                .map_err(command_error)?
            else {
                return Ok(None);
            };
            let all = store
                .list_folders(workspace, &root.collection_id)
                .map_err(command_error)?;
            let collection_id = root.collection_id.clone();
            let folders = parents_first(&all, vec![root]);
            let removed: HashSet<&FolderId> = folders.iter().map(|folder| &folder.id).collect();
            let links = store
                .folder_request_links(workspace, &collection_id)
                .map_err(command_error)?
                .into_iter()
                .filter(|(_, folder)| removed.contains(folder))
                .collect();
            store
                .delete_folder(workspace, &folder_id)
                .map_err(command_error)?;
            Ok(Some(Held::Folder { folders, links }))
        }
        DeletedKind::Collection => {
            let collection_id = CollectionId::new(id).map_err(command_error)?;
            let Some(collection) = store
                .get_collection(workspace, &collection_id)
                .map_err(command_error)?
            else {
                return Ok(None);
            };
            let all = store
                .list_folders(workspace, &collection_id)
                .map_err(command_error)?;
            let roots = all
                .iter()
                .filter(|folder| folder.parent_folder_id.is_none())
                .cloned()
                .collect();
            let folders = parents_first(&all, roots);
            let requests = store
                .list_requests(workspace.as_str())
                .map_err(command_error)?
                .into_iter()
                .filter(|request| request.collection_id.as_ref() == Some(&collection_id))
                .collect();
            store
                .delete_collection(workspace, &collection_id)
                .map_err(command_error)?;
            Ok(Some(Held::Collection {
                collection: Box::new(collection),
                folders,
                requests,
            }))
        }
    }
}

/// Deletes a request, folder or collection of the workspace and holds it from `now`. An unknown
/// id, or one owned by another workspace, is `NotFound` and leaves every row untouched.
pub(crate) fn delete_at(
    state: &AppState,
    workspace_id: &str,
    kind: DeletedKind,
    id: &str,
    now: Instant,
) -> Result<UndoToken, AppError> {
    let workspace = WorkspaceId::new(workspace_id).map_err(command_error)?;
    state.delete_undo.purge_at(now);
    let held = {
        let store = state.store();
        capture_and_delete(&store, &workspace, kind, id)?
    };
    let Some(held) = held else {
        let name = match kind {
            DeletedKind::Request => "request",
            DeletedKind::Folder => "folder",
            DeletedKind::Collection => "collection",
        };
        return Err(not_found(name, id, workspace_id));
    };
    state.sync.notify_changed();
    Ok(state.delete_undo.insert_at(
        now,
        DeletedSubtree {
            workspace_id: workspace,
            held,
        },
    ))
}

fn delete_impl(
    state: &AppState,
    workspace_id: &str,
    kind: DeletedKind,
    id: &str,
) -> Result<UndoToken, AppError> {
    let token = delete_at(state, workspace_id, kind, id, Instant::now())?;
    state.delete_undo.arm_timer();
    Ok(token)
}

/// Deletes a workspace's saved request. The removed request stays restorable
/// ([`restore_deleted_impl`]) until the toast is dismissed ([`commit_delete_undo_impl`]) plus 1 s,
/// and at most 60 s. An unknown id, or one owned by another workspace, is `NotFound`.
pub fn request_delete_impl(
    state: &AppState,
    workspace_id: String,
    id: String,
) -> Result<UndoToken, AppError> {
    delete_impl(state, &workspace_id, DeletedKind::Request, &id)
}

/// Deletes a workspace's folder with its subfolders; its requests stay and lose the folder.
/// Restorable like [`request_delete_impl`], re-linking the requests.
pub fn folder_delete_impl(
    state: &AppState,
    workspace_id: String,
    id: String,
) -> Result<UndoToken, AppError> {
    delete_impl(state, &workspace_id, DeletedKind::Folder, &id)
}

/// Deletes a workspace's collection with its folders and requests. Restorable like
/// [`request_delete_impl`], with the collection's secret variable values.
pub fn collection_delete_impl(
    state: &AppState,
    workspace_id: String,
    id: String,
) -> Result<UndoToken, AppError> {
    delete_impl(state, &workspace_id, DeletedKind::Collection, &id)
}

/// A stored collection with every folder (parents first) and request, secrets included.
pub(crate) type CollectionSnapshot = (Collection, Vec<Folder>, Vec<Request>);

/// The stored collection `id` as it is now, for an import that rewrites it in place or a copy.
pub(crate) fn snapshot_collection(
    store: &WorkspaceStore,
    workspace: &WorkspaceId,
    id: &CollectionId,
) -> Result<Option<CollectionSnapshot>, AppError> {
    let Some(collection) = store.get_collection(workspace, id).map_err(command_error)? else {
        return Ok(None);
    };
    let all = store.list_folders(workspace, id).map_err(command_error)?;
    let roots = all
        .iter()
        .filter(|folder| folder.parent_folder_id.is_none())
        .cloned()
        .collect();
    let folders = parents_first(&all, roots);
    let requests = store
        .list_requests(workspace.as_str())
        .map_err(command_error)?
        .into_iter()
        .filter(|request| request.collection_id.as_ref() == Some(id))
        .collect();
    Ok(Some((collection, folders, requests)))
}

/// Holds an import that rewrote a stored collection in place (`before`, from
/// [`snapshot_collection`]) under one token, after deleting the other same-name collections
/// `others` (a "Replace"): `restore_deleted` deletes what the import added, writes `before`
/// back with its ids and brings `others` back. The rewrite is already committed, so a
/// collection of `others` that cannot be removed goes to `warnings` and the token still holds
/// `before` and every collection that was removed. Returns the token and how many of `others`
/// were removed.
pub(crate) fn hold_update(
    state: &AppState,
    workspace: &WorkspaceId,
    before: CollectionSnapshot,
    added_folders: Vec<FolderId>,
    added_requests: Vec<RequestId>,
    others: &[String],
    warnings: &mut Vec<String>,
) -> (UndoToken, usize) {
    let now = Instant::now();
    state.delete_undo.purge_at(now);
    let mut removed = Vec::new();
    {
        let store = state.store();
        for id in others {
            match capture_and_delete(&store, workspace, DeletedKind::Collection, id) {
                Ok(Some(held)) => removed.push(held),
                Ok(None) => {}
                Err(error) => warnings.push(format!(
                    "collection {id} could not be replaced: {}",
                    error.message()
                )),
            }
        }
    }
    let replaced = removed.len();
    let (collection, folders, requests) = before;
    let token = state.delete_undo.insert_at(
        now,
        DeletedSubtree {
            workspace_id: workspace.clone(),
            held: Held::Updated {
                before: Box::new(Held::Collection {
                    collection: Box::new(collection),
                    folders,
                    requests,
                }),
                added_folders,
                added_requests,
                removed,
            },
        },
    );
    state.delete_undo.arm_timer();
    (token, replaced)
}

/// Deletes for good, without the undo hold: used where the removal comes from a sync import, so
/// no secret value is kept in memory for it.
pub fn delete_discarding(
    state: &AppState,
    workspace_id: &str,
    kind: DeletedKind,
    id: &str,
) -> Result<(), AppError> {
    let token = delete_at(state, workspace_id, kind, id, Instant::now())?;
    state.delete_undo.discard(&token.0);
    Ok(())
}

/// The Undo toast of `token` was dismissed: the held delete stays restorable for 1 s more (never
/// past 60 s after the delete) and is then dropped with its secret values. Returns `false` for an
/// unknown or expired token.
pub fn commit_delete_undo_impl(state: &AppState, token: UndoToken) -> bool {
    let committed = state.delete_undo.commit_at(Instant::now(), &token.0);
    if committed {
        state.delete_undo.arm_timer();
    }
    committed
}

/// Brings back what a delete command's token names. An expired, unknown or foreign-workspace
/// token is `NotFound`. What could not be brought back is in the report; the token stays usable
/// until it expires when nothing at all was restored, and is spent otherwise.
pub fn restore_deleted_impl(
    state: &AppState,
    workspace_id: String,
    token: UndoToken,
) -> Result<RestoreReportDto, AppError> {
    restore_at(state, &workspace_id, &token, Instant::now())
}

pub(crate) fn restore_at(
    state: &AppState,
    workspace_id: &str,
    token: &UndoToken,
    now: Instant,
) -> Result<RestoreReportDto, AppError> {
    let workspace = WorkspaceId::new(workspace_id).map_err(command_error)?;
    let deleted = state
        .delete_undo
        .peek_at(now, &workspace, &token.0)
        .ok_or_else(|| AppError::not_found("the deleted items can no longer be restored"))?;
    let report = {
        let store = state.store();
        ensure_workspace_exists(&store, workspace.as_str())?;
        restore(&store, &workspace, &deleted.held)
    };
    if !report.restored.is_empty() {
        state.delete_undo.discard(&token.0);
        state.sync.notify_changed();
    }
    Ok(report)
}

/// The text of a storage failure, for a report line.
fn text(error: StorageError) -> String {
    command_error(error).message().to_owned()
}

impl RestoreReportDto {
    fn fail(&mut self, id: &str, kind: DeletedKind, message: String) {
        self.failed.push(RestoreFailureDto {
            id: id.to_owned(),
            kind,
            message,
        });
    }
}

const TAKEN: &str = "it already exists";

/// Writes `held` back; a thing that cannot come back is reported and the rest still does.
fn restore(store: &WorkspaceStore, workspace: &WorkspaceId, held: &Held) -> RestoreReportDto {
    let mut report = RestoreReportDto {
        restored: Vec::new(),
        failed: Vec::new(),
    };
    match held {
        Held::Request(request) => {
            let id = request.id.as_str();
            let result = match store.get_request(workspace, &request.id) {
                Ok(Some(_)) => Err(TAKEN.to_owned()),
                Ok(None) => store.save_request(request).map_err(text),
                Err(error) => Err(text(error)),
            };
            match result {
                Ok(()) => report.restored.push(id.to_owned()),
                Err(message) => report.fail(id, DeletedKind::Request, message),
            }
        }
        Held::Folder { folders, links } => {
            let mut back: HashSet<&FolderId> = HashSet::new();
            for folder in folders {
                let id = folder.id.as_str();
                let result = match store.get_folder(workspace, &folder.id) {
                    Ok(Some(_)) => Err(TAKEN.to_owned()),
                    Ok(None) => store.save_folder(folder).map_err(text),
                    Err(error) => Err(text(error)),
                };
                match result {
                    Ok(()) => {
                        back.insert(&folder.id);
                        report.restored.push(id.to_owned());
                    }
                    Err(message) => report.fail(id, DeletedKind::Folder, message),
                }
            }
            for (request_id, folder_id) in links.iter().filter(|(_, folder)| back.contains(folder))
            {
                // A request deleted or moved since stays where it is: that is not a failure.
                match store.relink_request_folder(workspace, request_id, folder_id) {
                    Ok(true) => report.restored.push(request_id.as_str().to_owned()),
                    Ok(false) => {}
                    Err(error) => {
                        report.fail(request_id.as_str(), DeletedKind::Request, text(error));
                    }
                }
            }
        }
        Held::Collection {
            collection,
            folders,
            requests,
        } => {
            let id = collection.id.as_str();
            // One transaction: a collection comes back whole or not at all.
            let result = match store.get_collection(workspace, &collection.id) {
                Ok(Some(_)) => Err(TAKEN.to_owned()),
                Ok(None) => store
                    .save_imported_collection(collection, folders, requests)
                    .map_err(text),
                Err(error) => Err(text(error)),
            };
            match result {
                Ok(()) => {
                    report.restored.push(id.to_owned());
                    report
                        .restored
                        .extend(folders.iter().map(|folder| folder.id.as_str().to_owned()));
                    report.restored.extend(
                        requests
                            .iter()
                            .map(|request| request.id.as_str().to_owned()),
                    );
                }
                Err(message) => report.fail(id, DeletedKind::Collection, message),
            }
        }
        Held::Updated {
            before,
            added_folders,
            added_requests,
            removed,
        } => {
            // What the import added goes first; one already deleted by hand is not a failure.
            for id in added_requests {
                if let Err(error) = store.delete_request(workspace, id) {
                    report.fail(id.as_str(), DeletedKind::Request, text(error));
                }
            }
            for id in added_folders {
                if let Err(error) = store.delete_folder(workspace, id) {
                    report.fail(id.as_str(), DeletedKind::Folder, text(error));
                }
            }
            if let Held::Collection {
                collection,
                folders,
                requests,
            } = before.as_ref()
            {
                // Upserts in one transaction: the stored rows keep their ids (and history links).
                match store.save_imported_collection(collection, folders, requests) {
                    Ok(()) => {
                        report.restored.push(collection.id.as_str().to_owned());
                        report
                            .restored
                            .extend(folders.iter().map(|folder| folder.id.as_str().to_owned()));
                        report.restored.extend(
                            requests
                                .iter()
                                .map(|request| request.id.as_str().to_owned()),
                        );
                    }
                    Err(error) => {
                        report.fail(collection.id.as_str(), DeletedKind::Collection, text(error));
                    }
                }
            }
            for held in removed {
                let part = restore(store, workspace, held);
                report.restored.extend(part.restored);
                report.failed.extend(part.failed);
            }
        }
    }
    report
}
