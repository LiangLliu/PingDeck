//! Collection/environment import and export, plus response-body file saving.
//!
//! Single responsibility: convert between PingDeck's stored tree and the file
//! formats users exchange (Postman v2.1, environment JSON, cURL, HAR,
//! OpenAPI/Swagger, Insomnia, Bruno, Hoppscotch) and persist exported text or captured bytes
//! to disk.

use std::{
    collections::BTreeMap,
    io::Read,
    path::{Path, PathBuf},
};

use base64::{Engine as _, engine::general_purpose::STANDARD as BASE64_STANDARD};

use crate::error::AppError;
use crate::llm::wire::endpoint_url;
use crate::platform::{normalize_display_path, portable_file_name, write_private_file};
use crate::workspace::ai_request::{AiTarget, RequestMode};
use crate::workspace::{
    Collection, CollectionId, Environment, EnvironmentId, Folder, FolderId, Request, RequestId,
    WorkspaceId, WorkspaceStore,
};

use super::environment_service::{first_free_environment_name, save_environment_unique};
use super::import_service::{ImportConflict, import_collection_text, import_environment_text};
use super::{
    AppState, EnvironmentDto, PostmanExportDto, PostmanExportSaveResult, PostmanImportResult,
    ResponseBodyEncoding, ResponseSaveResult, command_error,
};

/// Saves an imported collection tree under fresh ids and reports what landed.
pub(crate) fn persist_imported_collection(
    state: &AppState,
    workspace_id: &WorkspaceId,
    mut collection: Collection,
    mut folders: Vec<Folder>,
    mut requests: Vec<Request>,
    warnings: Vec<String>,
) -> Result<PostmanImportResult, AppError> {
    let store = state.store();
    if store
        .get_workspace(workspace_id)
        .map_err(command_error)?
        .is_none()
    {
        return Err(AppError::not_found(format!(
            "workspace {} not found",
            workspace_id.as_str()
        )));
    }
    // Importer-generated IDs are deterministic and database IDs are global.
    // Find a suffix for the entire tree while holding the store lock.
    let mut suffix = 1_u64;
    let (new_collection_id, folder_ids, request_ids) = loop {
        let new_collection_id =
            CollectionId::new(format!("{}__import_{suffix}", collection.id.as_str()))
                .map_err(command_error)?;
        let folder_ids = folders
            .iter()
            .map(|folder| {
                FolderId::new(format!("{}__import_{suffix}", folder.id.as_str()))
                    .map_err(command_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        let request_ids = requests
            .iter()
            .map(|request| {
                RequestId::new(format!("{}__import_{suffix}", request.id.as_str()))
                    .map_err(command_error)
            })
            .collect::<Result<Vec<_>, _>>()?;
        if !store
            .import_ids_occupied(&new_collection_id, &folder_ids, &request_ids)
            .map_err(command_error)?
        {
            break (new_collection_id, folder_ids, request_ids);
        }
        suffix = suffix
            .checked_add(1)
            .ok_or_else(|| AppError::internal("exhausted import IDs"))?;
    };
    let mut folder_map = BTreeMap::new();
    for (folder, new_id) in folders.iter().zip(folder_ids) {
        if folder_map
            .insert(folder.id.as_str().to_owned(), new_id)
            .is_some()
        {
            return Err(AppError::conflict(
                "imported collection contains duplicate folder IDs",
            ));
        }
    }
    let mut seen_requests = std::collections::BTreeSet::new();
    for request in &requests {
        if !seen_requests.insert(request.id.as_str()) {
            return Err(AppError::conflict(
                "imported collection contains duplicate request IDs",
            ));
        }
    }
    for folder in &mut folders {
        folder.parent_folder_id = folder
            .parent_folder_id
            .as_ref()
            .map(|id| {
                folder_map.get(id.as_str()).cloned().ok_or_else(|| {
                    AppError::not_found(format!("imported folder parent {} not found", id.as_str()))
                })
            })
            .transpose()?;
        folder.id = folder_map[folder.id.as_str()].clone();
        folder.collection_id = new_collection_id.clone();
    }
    for (request, new_id) in requests.iter_mut().zip(request_ids) {
        request.folder_id = request
            .folder_id
            .as_ref()
            .map(|id| {
                folder_map.get(id.as_str()).cloned().ok_or_else(|| {
                    AppError::not_found(format!(
                        "imported request folder {} not found",
                        id.as_str()
                    ))
                })
            })
            .transpose()?;
        request.id = new_id;
        request.collection_id = Some(new_collection_id.clone());
    }
    collection.id = new_collection_id;

    let variable_count = collection.variables.len();
    let collection_id = collection.id.as_str().to_owned();
    let collection_name = collection.name.clone();

    let folder_count = folders.len();
    let request_count = requests.len();
    store
        .save_imported_collection(&collection, &folders, &requests)
        .map_err(command_error)?;
    state.sync.notify_changed();

    Ok(PostmanImportResult {
        workspace_id: workspace_id.as_str().to_owned(),
        collection_id,
        collection_name,
        folder_count,
        request_count,
        environment_id: None,
        variable_count,
        warnings,
        skipped: false,
        replaced: 0,
        updated: 0,
        undo_token: None,
    })
}

/// Imports a Postman v2.1 collection payload; `on_conflict` decides what a collection of the
/// same name means (see [`ImportConflict`]).
pub fn postman_import_text_impl(
    state: &AppState,
    workspace_id: String,
    payload: String,
    on_conflict: ImportConflict,
) -> Result<PostmanImportResult, AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    import_collection_text(state, &workspace_id, "postman", &payload, on_conflict)
}

/// Largest import file (Postman collection, cookie jar) read into memory, in bytes.
pub(crate) const MAX_IMPORT_FILE_BYTES: u64 = 32 * 1024 * 1024;

/// Reads a UTF-8 text file of at most `max_bytes` that the user picked; a bigger file is refused
/// before it is loaded. `what` names the file in the error, for example
/// `"Postman collection"`.
///
/// # Errors
/// [`AppError::Validation`] for a file over the cap or not UTF-8 text, [`AppError::Io`] when it
/// cannot be read.
pub(crate) fn read_capped(path: &Path, max_bytes: u64, what: &str) -> Result<String, AppError> {
    let too_large = || {
        AppError::validation(format!(
            "the {what} is larger than {} MiB",
            max_bytes / (1024 * 1024)
        ))
    };
    let read_error =
        |error: std::io::Error| AppError::io(format!("failed to read {what}: {error}"));
    let file = std::fs::File::open(path).map_err(read_error)?;
    if file.metadata().map_err(read_error)?.len() > max_bytes {
        return Err(too_large());
    }
    // The file may grow after the size check; the limit applies to what is actually read.
    let mut bytes = Vec::new();
    file.take(max_bytes + 1)
        .read_to_end(&mut bytes)
        .map_err(read_error)?;
    if bytes.len() as u64 > max_bytes {
        return Err(too_large());
    }
    String::from_utf8(bytes)
        .map_err(|_| AppError::validation(format!("the {what} is not UTF-8 text")))
}

/// Key of the PingDeck extension object in an exported environment (document and variables);
/// Postman ignores it.
pub(super) const PINGDECK_EXTENSION: &str = "_pingdeck";

/// Imports a Postman environment JSON payload, including the `_pingdeck` extension keys
/// (`confirm_before_send`, `confirm_safe_methods`, per-variable `note`) when present.
/// With [`ImportConflict::KeepBoth`] a name another environment of the workspace already uses
/// becomes `Name (2)`, `Name (3)`, …; the result carries no secret values.
pub fn environment_import_text_impl(
    state: &AppState,
    workspace_id: String,
    payload: String,
    on_conflict: ImportConflict,
) -> Result<EnvironmentDto, AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    import_environment_text(state, &workspace_id, &payload, on_conflict)
}

/// Saves an imported environment, renaming its id when it would overwrite another row and its
/// name when the workspace already has one (names are unique, case-insensitively).
pub(crate) fn save_imported_environment(
    state: &AppState,
    mut environment: Environment,
) -> Result<Environment, AppError> {
    let store = state.store();
    if store
        .get_workspace(&environment.workspace_id)
        .map_err(command_error)?
        .is_none()
    {
        return Err(AppError::not_found(format!(
            "workspace {} not found",
            environment.workspace_id.as_str()
        )));
    }
    let base_id = environment.id.as_str().to_owned();
    let mut suffix = 0_u64;
    while store
        .environment_id_occupied(&environment.id)
        .map_err(command_error)?
    {
        suffix = suffix
            .checked_add(1)
            .ok_or_else(|| AppError::internal("exhausted environment import IDs"))?;
        environment.id =
            EnvironmentId::new(format!("{base_id}__import_{suffix}")).map_err(command_error)?;
    }
    let name = environment.name.clone();
    environment.name = first_free_environment_name(
        &store,
        &environment.workspace_id,
        &environment.id,
        std::iter::once(name.clone()).chain((2_u32..).map(|n| format!("{name} ({n})"))),
    )?;
    save_environment_unique(&store, &environment)?;
    state.sync.notify_changed();
    Ok(environment)
}

/// Imports a cURL, HAR, OpenAPI/Swagger, Insomnia, Bruno or Hoppscotch payload as a new
/// collection; `on_conflict` decides what a collection of the same name means (see
/// [`ImportConflict`]).
pub fn collection_import_text_impl(
    state: &AppState,
    workspace_id: String,
    format: String,
    payload: String,
    on_conflict: ImportConflict,
) -> Result<PostmanImportResult, AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let format = format.to_ascii_lowercase();
    if format == "postman" {
        return Err(AppError::unsupported("unsupported import format: postman"));
    }
    import_collection_text(state, &workspace_id, &format, &payload, on_conflict)
}

/// Exports an environment as Postman environment JSON with `_pingdeck` extension keys for the
/// production-guard flags and the variable notes.
///
/// Secret values are left out (blank value, type `secret`) unless `include_secrets`, which
/// writes the real values from the secret store.
pub fn environment_export_impl(
    state: &AppState,
    workspace_id: String,
    environment_id: String,
    include_secrets: bool,
) -> Result<String, AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let environment_id = EnvironmentId::new(&environment_id).map_err(command_error)?;
    let store = state.store();
    let environment = store
        .get_environment(&workspace_id, &environment_id)
        .map_err(command_error)?
        .ok_or_else(|| {
            AppError::not_found(format!("environment {} not found", environment_id.as_str()))
        })?;
    let values = environment
        .variables
        .iter()
        .map(|variable| {
            let value = if variable.is_secret && !include_secrets {
                String::new()
            } else {
                variable.value.clone().unwrap_or_default()
            };
            let mut entry = serde_json::json!({
                "key": variable.key,
                "value": value,
                "type": if variable.is_secret { "secret" } else { "default" },
                "enabled": variable.enabled,
            });
            if !variable.note.is_empty() {
                entry[PINGDECK_EXTENSION] = serde_json::json!({ "note": variable.note });
            }
            entry
        })
        .collect::<Vec<_>>();
    serde_json::to_string_pretty(&serde_json::json!({
        "id": environment.id.as_str(),
        "name": environment.name,
        "values": values,
        "_postman_variable_scope": "environment",
        PINGDECK_EXTENSION: {
            "confirm_before_send": environment.confirm_before_send,
            "confirm_safe_methods": environment.confirm_safe_methods,
        },
    }))
    .map_err(command_error)
}

/// Whether `request` belongs to `collection`, directly or through one of its folders.
#[must_use]
pub(crate) fn request_belongs_to_collection(
    request: &Request,
    collection: &Collection,
    folders: &[Folder],
) -> bool {
    request.collection_id.as_ref() == Some(&collection.id)
        || (request.collection_id.is_none()
            && request
                .folder_id
                .as_ref()
                .is_some_and(|id| folders.iter().any(|folder| &folder.id == id)))
}

/// Builds the Postman v2.1 JSON of a collection, with the chosen environment's variables laid
/// over the collection's (the environment wins for a shared key, as at send time), and
/// the warnings of what the export could not carry. Literal credentials are left empty unless
/// `include_literal_credentials`.
pub fn postman_export_impl(
    state: &AppState,
    workspace_id: String,
    collection_id: String,
    environment_id: Option<String>,
    include_literal_credentials: bool,
) -> Result<PostmanExportDto, AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let collection_id = CollectionId::new(&collection_id).map_err(command_error)?;
    let environment_id = environment_id
        .as_deref()
        .map(EnvironmentId::new)
        .transpose()
        .map_err(command_error)?;

    let store = state.store();
    let collection = store
        .get_collection(&workspace_id, &collection_id)
        .map_err(command_error)?
        .ok_or_else(|| {
            AppError::not_found(format!("collection {} not found", collection_id.as_str()))
        })?;
    let folders = store
        .list_folders(&workspace_id, &collection.id)
        .map_err(command_error)?;
    let requests = store
        .list_requests(workspace_id.as_str())
        .map_err(command_error)?
        .into_iter()
        .filter(|request| request_belongs_to_collection(request, &collection, &folders))
        .collect::<Vec<_>>();
    let variables = match environment_id.as_ref() {
        Some(environment_id) => {
            store
                .get_environment(&workspace_id, environment_id)
                .map_err(command_error)?
                .ok_or_else(|| {
                    AppError::not_found(format!(
                        "environment {} not found",
                        environment_id.as_str()
                    ))
                })?
                .variables
        }
        None => Vec::new(),
    };

    let mut context = postman_export_context(&store, &requests);
    context.include_literal_credentials = include_literal_credentials;
    crate::postman::export_collection(&collection, &folders, &requests, &variables, &context)
        .map(|export| PostmanExportDto {
            json: export.json,
            warnings: export.warnings,
        })
        .map_err(command_error)
}

/// The resolved URL of every provider-target AI request in `requests`, which the Postman export
/// cannot look up itself; a request whose provider or endpoint is gone gets none.
fn postman_export_context(
    store: &WorkspaceStore,
    requests: &[Request],
) -> crate::postman::PostmanExportContext {
    let mut context = crate::postman::PostmanExportContext::default();
    for request in requests {
        let Some(ai) = request
            .ai
            .as_ref()
            .filter(|_| request.mode == RequestMode::AiApi)
        else {
            continue;
        };
        let AiTarget::Provider {
            provider_id, model, ..
        } = &ai.target
        else {
            continue;
        };
        let Ok(Some(provider)) = crate::llm::store::get_provider(store.connection(), provider_id)
        else {
            continue;
        };
        let Some(endpoint) = provider
            .endpoints
            .iter()
            .find(|endpoint| endpoint.protocol == ai.protocol)
        else {
            continue;
        };
        if let Ok(url) = endpoint_url(
            &provider.base_url,
            &endpoint.path_prefix,
            endpoint.url_mode,
            ai.protocol,
            Some(model),
            false,
        ) {
            context.ai_urls.insert(request.id.as_str().to_owned(), url);
        }
    }
    context
}

/// File name a save dialog should suggest for exporting `collection_id`.
pub fn postman_export_suggested_filename(
    state: &AppState,
    workspace_id: &str,
    collection_id: &str,
) -> Result<String, AppError> {
    let workspace_id = WorkspaceId::new(workspace_id).map_err(command_error)?;
    let collection_id = CollectionId::new(collection_id).map_err(command_error)?;
    let store = state.store();
    let collection = store
        .get_collection(&workspace_id, &collection_id)
        .map_err(command_error)?
        .ok_or_else(|| {
            AppError::not_found(format!("collection {} not found", collection_id.as_str()))
        })?;

    Ok(postman_export_filename(&collection.name))
}

/// Exports a collection as Postman v2.1 JSON to the chosen path. The path is checked first
/// so a canceled dialog builds no export; the file is owner-only because exports can carry
/// secrets.
pub fn postman_export_save_to_path_impl(
    state: &AppState,
    workspace_id: String,
    collection_id: String,
    environment_id: Option<String>,
    include_literal_credentials: bool,
    selected_path: Option<PathBuf>,
) -> Result<PostmanExportSaveResult, AppError> {
    let Some(path) = selected_path else {
        return Ok(PostmanExportSaveResult {
            saved: false,
            path: None,
            warnings: Vec::new(),
        });
    };

    let export = postman_export_impl(
        state,
        workspace_id,
        collection_id,
        environment_id,
        include_literal_credentials,
    )?;
    write_private_file(&path, export.json)
        .map_err(|error| AppError::io(format!("failed to write Postman collection: {error}")))?;
    state.note_user_file(&path);

    Ok(PostmanExportSaveResult {
        saved: true,
        path: Some(normalize_display_path(&path)),
        warnings: export.warnings,
    })
}

/// Saves a captured response body to the chosen path (owner-only permissions: it may hold
/// credentials); a `None` path reports the cancel and writes nothing. A body over `max_bytes`
/// (`data.response_body_cap_bytes`) is rejected.
pub fn response_save_to_path_impl(
    state: &AppState,
    max_bytes: usize,
    body: String,
    encoding: ResponseBodyEncoding,
    selected_path: Option<PathBuf>,
) -> Result<ResponseSaveResult, AppError> {
    let too_large = || {
        AppError::validation(format!(
            "response body is larger than the {max_bytes}-byte save limit"
        ))
    };
    let body = match encoding {
        ResponseBodyEncoding::Text => body.into_bytes(),
        ResponseBodyEncoding::Base64 => {
            if body.len() > max_bytes.div_ceil(3) * 4 {
                return Err(too_large());
            }
            BASE64_STANDARD.decode(body).map_err(|error| {
                AppError::validation(format!("invalid response body encoding: {error}"))
            })?
        }
    };
    if body.len() > max_bytes {
        return Err(too_large());
    }
    response_save_bytes_to_path_impl(state, body, selected_path)
}

/// The file name a save dialog should offer: `suggested_name` reduced to its last path
/// component (it may come from a URL or a header), or `pingdeck-response.txt` when nothing
/// usable was suggested.
#[must_use]
pub fn response_suggested_file_name(suggested_name: &str) -> String {
    let name = portable_file_name(suggested_name.trim());
    if name.is_empty() {
        "pingdeck-response.txt".to_owned()
    } else {
        name.to_owned()
    }
}

/// Saves the stored body behind `handle` (see `body_handle` on an execution result) to the chosen
/// path, without the bytes ever passing through the shell; a `None` path writes nothing.
///
/// The body is fetched before the path is looked at, so an evicted handle is `NotFound` even
/// for a cancelled dialog.
pub fn response_save_handle_to_path_impl(
    state: &AppState,
    handle: &str,
    selected_path: Option<PathBuf>,
) -> Result<ResponseSaveResult, AppError> {
    let body = state.bodies.get(handle)?;
    response_save_bytes_to_path_impl(state, body, selected_path)
}

/// Writes `body` owner-only to `selected_path` (it may hold credentials); `None` reports the
/// cancelled dialog and writes nothing. For shells that already hold the bytes, e.g. fetched
/// with `response_body_fetch_impl` before the dialog opened.
pub fn response_save_bytes_to_path_impl(
    state: &AppState,
    body: impl AsRef<[u8]>,
    selected_path: Option<PathBuf>,
) -> Result<ResponseSaveResult, AppError> {
    let Some(path) = selected_path else {
        return Ok(ResponseSaveResult {
            saved: false,
            path: None,
        });
    };
    write_private_file(&path, body)
        .map_err(|error| AppError::io(format!("failed to save response body: {error}")))?;
    state.note_user_file(&path);
    Ok(ResponseSaveResult {
        saved: true,
        path: Some(normalize_display_path(&path)),
    })
}

/// Slugged `*.postman_collection.json` file name for a collection name.
#[must_use]
fn postman_export_filename(collection_name: &str) -> String {
    let mut slug = String::new();
    for character in collection_name.chars() {
        if character.is_ascii_alphanumeric() {
            slug.push(character.to_ascii_lowercase());
        } else if !slug.ends_with('-') {
            slug.push('-');
        }
    }
    let slug = slug.trim_matches('-');

    if slug.is_empty() {
        "pingdeck-collection.postman_collection.json".into()
    } else {
        format!("{slug}.postman_collection.json")
    }
}
