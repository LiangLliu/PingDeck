//! Workspace, collection, globals, folder, request and tab-session use-cases.
//!
//! Single responsibility: everything the sidebar and request tree need to read
//! or write saved configuration, with no UI, IPC or dialog code involved.

use crate::error::AppError;
use crate::workspace::{
    Collection, CollectionId, EnvironmentVariable, Folder, FolderId, FolderMoveResult, Request,
    RequestId, Workspace, WorkspaceId,
};

use super::environment_service::{ensure_revision, redact_variables, variables_from_rows};
use super::{
    AppState, CollectionDto, GlobalsDto, GlobalsSaveInput, MoveFolderInput, MoveRequestInput,
    SaveCollectionInput, SaveFolderInput, SaveRequestInput, command_error, ensure_workspace_exists,
};
use crate::storage::{collection_revision, globals_revision};
use pingdeck_base::crypto::{hex_lower, sha256};

/// Fields of a request or folder JSON that are not content: identity, name, placement, order and
/// the read-only markers. Open request tabs follow a rename or move in the tree without reloading
/// (a dirty tab keeps its own name), so renaming, moving or reordering an item does not make an
/// open editor stale.
const NON_CONTENT_FIELDS: [&str; 9] = [
    "id",
    "workspace_id",
    "collection_id",
    "folder_id",
    "parent_folder_id",
    "name",
    "position",
    "agent_touched_ms",
    "revision",
];

/// The first 8 bytes (hex) of the SHA-256 of `item`'s JSON without [`NON_CONTENT_FIELDS`]. Stable
/// across launches (a restored tab keeps a valid revision) and equal for equal content. A request
/// or folder holds nothing the webview does not already see, so a plain digest reveals nothing.
fn content_revision(item: &impl serde::Serialize) -> String {
    let mut value = serde_json::to_value(item).unwrap_or_default();
    if let Some(fields) = value.as_object_mut() {
        for key in NON_CONTENT_FIELDS {
            fields.remove(key);
        }
    }
    hex_lower(&sha256(value.to_string().as_bytes())[..8])
}

/// The revision of a saved request (see [`content_revision`]).
#[must_use]
pub fn request_revision(request: &Request) -> String {
    content_revision(request)
}

/// The revision of a saved folder (see [`content_revision`]).
#[must_use]
pub fn folder_revision(folder: &Folder) -> String {
    content_revision(folder)
}

/// `request` with its revision filled, as the editor reads it.
#[must_use]
pub fn with_request_revision(mut request: Request) -> Request {
    request.revision = Some(request_revision(&request));
    request
}

/// `folder` with its revision filled, as the editor reads it.
#[must_use]
pub fn with_folder_revision(mut folder: Folder) -> Folder {
    folder.revision = Some(folder_revision(&folder));
    folder
}

/// Validates and saves a new workspace.
pub fn workspace_create_impl(
    state: &AppState,
    id: String,
    name: String,
) -> Result<Workspace, AppError> {
    let workspace = Workspace::new(&id, &name).map_err(command_error)?;
    let store = state.store();
    store.save_workspace(&workspace).map_err(command_error)?;
    state.sync.notify_changed();
    Ok(workspace)
}

/// Lists every workspace.
pub fn workspace_list_impl(state: &AppState) -> Result<Vec<Workspace>, AppError> {
    let store = state.store();
    store.list_workspaces().map_err(command_error)
}

/// Marks a workspace as most recently used.
pub fn workspace_touch_impl(state: &AppState, id: String) -> Result<bool, AppError> {
    let workspace_id = WorkspaceId::new(&id).map_err(command_error)?;
    let store = state.store();
    store.touch_workspace(&workspace_id).map_err(command_error)
}

/// Deletes a workspace and stops its mock servers.
///
/// The ids are listed before the delete (the rows cascade away with the workspace) and the
/// listeners are stopped only after the delete succeeded and the store lock is released:
/// `MockServerManager::stop` joins a thread, and a failed delete must leave servers running.
pub fn workspace_delete_impl(state: &AppState, id: String) -> Result<(), AppError> {
    let workspace_id = WorkspaceId::new(&id).map_err(command_error)?;
    let (deleted, server_ids) = {
        let store = state.store();
        let server_ids: Vec<String> = store
            .list_mock_servers(workspace_id.as_str())
            .map_err(command_error)?
            .into_iter()
            .map(|server| server.id)
            .collect();
        let deleted = store
            .delete_workspace(&workspace_id)
            .map_err(command_error)?;
        (deleted, server_ids)
    };
    if !deleted {
        return Err(AppError::not_found(format!(
            "workspace {} not found",
            workspace_id.as_str()
        )));
    }
    for server_id in &server_ids {
        state.mocks.stop(server_id);
    }
    state.sync.notify_changed();
    Ok(())
}

/// Validates and saves a collection unconditionally (see [`collection_save_checked_impl`]).
pub fn collection_save_impl(
    state: &AppState,
    input: SaveCollectionInput,
) -> Result<CollectionDto, AppError> {
    collection_save_checked_impl(state, input, None)
}

/// Validates and saves a collection.
///
/// A secret row without a value keeps the stored value of the variable it names by id (so a
/// renamed key keeps it) or of the same key; the result carries no secret values. With
/// `base_revision` (the `revision` the editor loaded), a stored collection that changed since is
/// refused with `Conflict` (`stale_write`); a collection not stored yet is created.
pub fn collection_save_checked_impl(
    state: &AppState,
    input: SaveCollectionInput,
    base_revision: Option<&str>,
) -> Result<CollectionDto, AppError> {
    let mut collection =
        Collection::new(&input.id, &input.workspace_id, &input.name).map_err(command_error)?;
    collection.description = input.description;
    collection.auth = input.auth;
    collection.pre_request_script = input.pre_request_script;
    collection.test_script = input.test_script;
    collection.position = input.position;
    let store = state.store();
    ensure_workspace_exists(&store, collection.workspace_id.as_str())?;
    let stored = store
        .get_collection(&collection.workspace_id, &collection.id)
        .map_err(command_error)?;
    if let (Some(base), Some(stored)) = (base_revision, stored.as_ref()) {
        ensure_revision(Some(&collection_revision(stored)), base, "collection")?;
    }
    let stored_variables = stored.as_ref().map_or(&[][..], |stored| &stored.variables);
    collection.variables = input
        .variables
        .into_iter()
        .map(|row| row.into_domain(collection.id.as_str(), stored_variables))
        .collect::<Result<Vec<_>, _>>()?;
    store.save_collection(&collection).map_err(command_error)?;
    state.sync.notify_changed();
    Ok(CollectionDto::from(&collection))
}

/// Lists the collections of a workspace, secret variable values redacted.
pub fn collection_list_impl(
    state: &AppState,
    workspace_id: String,
) -> Result<Vec<CollectionDto>, AppError> {
    let store = state.store();
    let collections = store
        .list_collections(&workspace_id)
        .map_err(command_error)?;
    Ok(collections.iter().map(CollectionDto::from).collect())
}

/// Validates and saves a folder unconditionally (see [`folder_save_checked_impl`]).
pub fn folder_save_impl(state: &AppState, input: SaveFolderInput) -> Result<Folder, AppError> {
    folder_save_checked_impl(state, input, None)
}

/// Validates and saves a folder; the result carries its new revision. With `base_revision`, a
/// stored folder that changed since is refused with `Conflict` (`stale_write`).
pub fn folder_save_checked_impl(
    state: &AppState,
    input: SaveFolderInput,
    base_revision: Option<&str>,
) -> Result<Folder, AppError> {
    let mut folder =
        Folder::new(&input.id, &input.collection_id, &input.name).map_err(command_error)?;
    if let Some(parent_folder_id) = input.parent_folder_id {
        folder = folder
            .with_parent(parent_folder_id)
            .map_err(command_error)?;
    }
    folder.description = input.description;
    folder.auth = input.auth;
    folder.pre_request_script = input.pre_request_script;
    folder.test_script = input.test_script;
    folder.position = input.position;

    let store = state.store();
    let workspace = store
        .collection_workspace_id(folder.collection_id.as_str())
        .map_err(command_error)?;
    let stored = match &workspace {
        Some(workspace) => store
            .get_folder(workspace, &folder.id)
            .map_err(command_error)?,
        None => None,
    };
    if let (Some(base), Some(stored)) = (base_revision, stored.as_ref()) {
        ensure_revision(Some(&folder_revision(stored)), base, "folder")?;
    }
    store.save_folder(&folder).map_err(command_error)?;
    state.sync.notify_changed();
    Ok(with_folder_revision(folder))
}

/// Lists the folders of a workspace's collection; an unknown collection, or one owned by another
/// workspace, is `NotFound`.
pub fn folder_list_impl(
    state: &AppState,
    workspace_id: String,
    collection_id: String,
) -> Result<Vec<Folder>, AppError> {
    let collection_id = CollectionId::new(&collection_id).map_err(command_error)?;
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let store = state.store();
    if store
        .get_collection(&workspace, &collection_id)
        .map_err(command_error)?
        .is_none()
    {
        return Err(AppError::not_found(format!(
            "collection {} not found in workspace {workspace_id}",
            collection_id.as_str()
        )));
    }
    Ok(store
        .list_folders(&workspace, &collection_id)
        .map_err(command_error)?
        .into_iter()
        .map(with_folder_revision)
        .collect())
}

/// Moves a folder subtree to another collection or parent folder.
pub fn folder_move_impl(
    state: &AppState,
    input: MoveFolderInput,
) -> Result<Option<FolderMoveResult>, AppError> {
    let workspace_id = WorkspaceId::new(&input.workspace_id).map_err(command_error)?;
    let folder_id = FolderId::new(&input.folder_id).map_err(command_error)?;
    let target_collection_id =
        CollectionId::new(&input.target_collection_id).map_err(command_error)?;
    let target_parent_folder_id = input
        .target_parent_folder_id
        .as_deref()
        .map(FolderId::new)
        .transpose()
        .map_err(command_error)?;

    let store = state.store();
    let moved = store
        .move_folder_tree(
            &workspace_id,
            &folder_id,
            &target_collection_id,
            target_parent_folder_id.as_ref(),
        )
        .map_err(command_error)?;
    state.sync.notify_changed();
    moved.map(Some).ok_or_else(|| {
        AppError::not_found(format!(
            "folder {} not found in workspace {}",
            input.folder_id, input.workspace_id
        ))
    })
}

/// Moves a request to another collection/folder/position.
pub fn request_move_impl(
    state: &AppState,
    input: MoveRequestInput,
) -> Result<Option<Request>, AppError> {
    let workspace_id = WorkspaceId::new(&input.workspace_id).map_err(command_error)?;
    let request_id = RequestId::new(&input.request_id).map_err(command_error)?;
    let target_collection_id = input
        .target_collection_id
        .as_deref()
        .map(CollectionId::new)
        .transpose()
        .map_err(command_error)?;
    let target_folder_id = input
        .target_folder_id
        .as_deref()
        .map(FolderId::new)
        .transpose()
        .map_err(command_error)?;
    let store = state.store();
    let moved = store
        .move_request(
            &workspace_id,
            &request_id,
            target_collection_id.as_ref(),
            target_folder_id.as_ref(),
            input.position,
        )
        .map_err(command_error)?;
    state.sync.notify_changed();
    moved.map(Some).ok_or_else(|| {
        AppError::not_found(format!(
            "request {} not found in workspace {}",
            input.request_id, input.workspace_id
        ))
    })
}

fn globals_dto(workspace_id: &WorkspaceId, variables: &[EnvironmentVariable]) -> GlobalsDto {
    GlobalsDto {
        variables: redact_variables(workspace_id.as_str(), variables),
        revision: globals_revision(variables),
    }
}

/// Lists the workspace globals, secret values redacted, with their revision.
pub fn globals_get_impl(state: &AppState, workspace_id: String) -> Result<GlobalsDto, AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let store = state.store();
    let variables = store
        .list_global_variables(workspace_id.as_str())
        .map_err(command_error)?;
    Ok(globals_dto(&workspace_id, &variables))
}

/// Replaces the workspace globals; nothing is stored unless every variable is valid and every
/// key unique. A secret row without a value keeps the stored value of the variable it names by
/// id, or of the same key. With `base_revision`, globals changed since (a script ran
/// `pm.globals.set`) refuse the save with `Conflict`.
pub fn globals_save_impl(
    state: &AppState,
    input: GlobalsSaveInput,
) -> Result<GlobalsDto, AppError> {
    let workspace_id = WorkspaceId::new(&input.workspace_id).map_err(command_error)?;
    let store = state.store();
    ensure_workspace_exists(&store, workspace_id.as_str())?;
    let stored = store
        .list_global_variables(workspace_id.as_str())
        .map_err(command_error)?;
    if let Some(base) = input.base_revision.as_deref() {
        ensure_revision(Some(&globals_revision(&stored)), base, "globals")?;
    }
    let variables = variables_from_rows(input.variables, workspace_id.as_str(), &stored)?;
    store
        .save_global_variables(workspace_id.as_str(), &variables)
        .map_err(command_error)?;
    state.sync.notify_changed();
    Ok(globals_dto(&workspace_id, &variables))
}

/// Loads the serialized open-tab session of a workspace.
pub(crate) fn load_tab_session_impl(
    state: &AppState,
    workspace_id: String,
) -> Result<Option<String>, AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let store = state.store();
    store
        .load_tab_session(workspace_id.as_str())
        .map_err(command_error)
}

/// Stores the serialized open-tab session of a workspace.
pub(crate) fn save_tab_session_impl(
    state: &AppState,
    workspace_id: String,
    payload: String,
) -> Result<(), AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let store = state.store();
    ensure_workspace_exists(&store, workspace_id.as_str())?;
    store
        .save_tab_session(workspace_id.as_str(), &payload)
        .map_err(command_error)
}

/// Builds a validated domain request from the editor input without saving it.
pub(crate) fn request_from_input(input: SaveRequestInput) -> Result<Request, AppError> {
    let mut request = Request::new(&input.id, &input.workspace_id, &input.name, &input.url)
        .map_err(command_error)?;
    if let Some(collection_id) = input.collection_id {
        request = request
            .with_collection(collection_id)
            .map_err(command_error)?;
    }
    if let Some(folder_id) = input.folder_id {
        request = request.with_folder(folder_id).map_err(command_error)?;
    }

    request.protocol = input.protocol;
    request.method = input.method;
    request.query_params = input.query_params;
    request.path_variables = input.path_variables;
    request.headers = input.headers;
    request.auth = input.auth;
    request.body = input.body;
    request.network_options = input.network_options;
    request.assertions = input.assertions;
    request.description = input.description;
    request.pre_request_script = input.pre_request_script;
    request.test_script = input.test_script;
    request.position = input.position;
    request.mode = input.mode;
    request.ai = input.ai;
    request.normalize_ai();
    request.validate_assertions().map_err(command_error)?;
    request.validate_ai().map_err(command_error)?;

    Ok(request)
}

/// Validates and saves a request unconditionally (see [`request_save_checked_impl`]).
pub fn request_save_impl(state: &AppState, input: SaveRequestInput) -> Result<Request, AppError> {
    request_save_checked_impl(state, input, None)
}

/// Validates and saves a request and answers it as stored, with its new revision. An input
/// without `examples` keeps the stored request's examples; one with a list replaces them. With
/// `base_revision` (the `revision` the editor loaded), a stored request whose content changed
/// since (the agent, sync, another tab or a script wrote it) is refused with `Conflict`
/// (`stale_write`) and nothing is written; a request not stored yet is created.
pub fn request_save_checked_impl(
    state: &AppState,
    input: SaveRequestInput,
    base_revision: Option<&str>,
) -> Result<Request, AppError> {
    let mut input = input;
    let examples = input.examples.take();
    let mut request = request_from_input(input)?;
    if let Some(examples) = &examples {
        request.examples.clone_from(examples);
        request.validate_examples().map_err(command_error)?;
    }
    let store = state.store();
    ensure_workspace_exists(&store, request.workspace_id.as_str())?;
    if let Some(stored) = store
        .get_request(&request.workspace_id, &request.id)
        .map_err(command_error)?
    {
        if let Some(base) = base_revision {
            ensure_revision(Some(&request_revision(&stored)), base, "request")?;
        }
        if examples.is_none() {
            request.examples = stored.examples;
        }
    }
    store.save_request(&request).map_err(command_error)?;
    let saved = store
        .get_request(&request.workspace_id, &request.id)
        .map_err(command_error)?
        .unwrap_or(request);
    state.sync.notify_changed();
    Ok(with_request_revision(saved))
}

/// Lists the saved requests of a workspace, each with its revision.
pub fn request_list_impl(state: &AppState, workspace_id: String) -> Result<Vec<Request>, AppError> {
    let store = state.store();
    Ok(store
        .list_requests(&workspace_id)
        .map_err(command_error)?
        .into_iter()
        .map(with_request_revision)
        .collect())
}
