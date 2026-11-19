//! Import preview and name-clash handling for collection and environment imports.
//!
//! Single responsibility: parse an import payload without writing anything ([`import_preview_impl`])
//! and apply an [`ImportConflict`] choice when a collection or environment of the same name
//! already exists. Collections and environments are matched by name, ignoring case and
//! surrounding spaces; mock servers (Mockoon) and HAR/cURL imports of an unnamed source never
//! clash. "Update" and "Replace" rewrite the stored collection in place (`import_merge`), so
//! paired requests keep their ids.

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::mock::import_mockoon_environment;
use crate::workspace::{
    AuthConfig, Collection, CollectionId, Environment, EnvironmentVariable, Folder, Request,
    WorkspaceId,
};

use super::environment_service::{EnvironmentDto, save_environment_unique};
use super::import_export_service::{
    PINGDECK_EXTENSION, persist_imported_collection, save_imported_environment,
};
use super::import_merge::{MergeMode, count_matches, plan_merge};
use super::tree_service::{hold_update, snapshot_collection};
use super::{AppState, PostmanImportResult, command_error};

/// What to do when an import's collection or environment name is already taken.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "kebab-case")]
pub enum ImportConflict {
    /// Import under a free name next to the existing one.
    #[default]
    KeepBoth,
    /// Refresh the existing collection from the file in place: requests and folders the file
    /// pairs with stored ones keep their ids and what the user added (assertions, scripts,
    /// network options, filled-in values, auth secrets); new ones are added and stored ones the
    /// file lacks are kept. Held for undo (the result's `undo_token`). An environment is
    /// replaced as with `Replace`.
    Update,
    /// Replace the existing item: the collection becomes exactly the file, but paired requests
    /// and folders keep their ids (flow steps and history stay linked) and stored ones the file
    /// lacks are removed; other collections of that name are deleted. Held for undo (the
    /// result's `undo_token`). An environment keeps its id, its declared hosts and the stored
    /// value of every secret the file leaves blank, and takes the file's other variables and
    /// guard flags.
    Replace,
    /// Import nothing and report the existing item.
    Skip,
}

/// Where an import's name clash is looked up.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ImportClashKind {
    /// Against the workspace's collections.
    Collection,
    /// Against the workspace's environments.
    Environment,
    /// This format never clashes.
    None,
}

/// The existing item an import would clash with.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportClashDto {
    /// What the name is compared against.
    pub kind: ImportClashKind,
    /// Id of the first existing item with the same name; `None` when there is no clash.
    pub existing_id: Option<String>,
}

/// A script API the sandbox lacks: in the import preview, the first item whose script
/// uses it; on the Scripts tab, one script.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptCompatIssue {
    /// The call or member as written (`require`, `CryptoJS.AES`, `setTimeout`).
    pub api: String,
    /// 1-based line of the first use in that script.
    pub line: u32,
    /// Collection, folder or request name whose script uses it first; empty for one script.
    pub location: String,
    /// Scripts that use it.
    pub uses: u32,
}

/// The APIs one script uses that the sandbox lacks, in order of first use.
#[must_use]
pub fn script_compat_list_impl(source: &str) -> Vec<ScriptCompatIssue> {
    crate::scripts::script_compat_issues(source)
        .into_iter()
        .map(|issue| ScriptCompatIssue {
            api: issue.api,
            line: issue.line,
            location: String::new(),
            uses: 1,
        })
        .collect()
}

/// Every script of an import, with its owner's name, folded into one issue per API.
fn import_script_issues<'a>(
    scripts: impl Iterator<Item = (&'a str, &'a str)>,
) -> Vec<ScriptCompatIssue> {
    let mut issues: Vec<ScriptCompatIssue> = Vec::new();
    for (owner, source) in scripts.filter(|(_, source)| !source.trim().is_empty()) {
        for issue in crate::scripts::script_compat_issues(source) {
            match issues.iter_mut().find(|seen| seen.api == issue.api) {
                Some(seen) => seen.uses = seen.uses.saturating_add(1),
                None => issues.push(ScriptCompatIssue {
                    api: issue.api,
                    line: issue.line,
                    location: owner.to_owned(),
                    uses: 1,
                }),
            }
        }
    }
    issues
}

/// What an import would create, computed from the payload by the same parser the import uses.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ImportPreviewDto {
    /// The format that was parsed (`postman`, `curl`, `har`, `openapi`, `insomnia`, `bruno`,
    /// `hoppscotch`, `environment`, `mockoon`).
    pub format: String,
    /// Collection, environment or mock server name found in the payload.
    pub name: Option<String>,
    /// Folders to create (collections only).
    pub folders: Option<u32>,
    /// Requests to create (collections only).
    pub requests: Option<u32>,
    /// Collection or environment variables.
    pub variables: Option<u32>,
    /// Mock routes: of a Mockoon file, or the ones "Also create mock routes" makes from an
    /// OpenAPI document (one per operation).
    pub routes: Option<u32>,
    /// Variables marked secret: imported with an empty value.
    pub secrets: u32,
    /// Whether any pre-request or test script is present; scripts do not run until the first send.
    pub scripts: bool,
    /// Authorization kinds in use beyond none and inherit, in order of first use
    /// (`bearer`, `basic`, `apikey`, `oauth2`, `digest`, `awsv4`).
    pub auth: Vec<String>,
    /// The parser's own notes about what it cannot import.
    pub warnings: Vec<String>,
    /// The name clash an import would meet.
    pub clash: ImportClashDto,
    /// Script APIs the sandbox lacks, one per API.
    pub script_issues: Vec<ScriptCompatIssue>,
    /// Flow steps that call a request "Replace" would remove (stored requests the file does
    /// not pair, and those of other same-name collections).
    pub affected_flow_steps: u32,
    /// Requests of the file that pair with a stored request of the clashing collection: under
    /// "Update" or "Replace" they keep their ids.
    #[serde(default)]
    pub matched_requests: u32,
}

/// A parsed collection import, whatever its format.
struct ParsedCollection {
    collection: Collection,
    folders: Vec<Folder>,
    requests: Vec<Request>,
    variables: Vec<EnvironmentVariable>,
    warnings: Vec<String>,
}

fn parse_collection(
    format: &str,
    workspace_id: &WorkspaceId,
    payload: &str,
) -> Result<ParsedCollection, AppError> {
    let workspace = workspace_id.as_str();
    let (collection, folders, requests, variables, warnings) = match format {
        "postman" => {
            let imported =
                crate::postman::import_collection(payload, workspace).map_err(command_error)?;
            (
                imported.collection,
                imported.folders,
                imported.requests,
                imported.variables,
                imported.warnings,
            )
        }
        other => {
            let imported = match other {
                "curl" => crate::importers::import_curl(payload, workspace),
                "har" => crate::importers::import_har(payload, workspace),
                "openapi" | "swagger" => crate::importers::import_openapi(payload, workspace),
                "insomnia" => crate::importers::import_insomnia(payload, workspace),
                "bruno" => crate::importers::import_bruno(payload, workspace),
                "hoppscotch" => crate::importers::import_hoppscotch(payload, workspace),
                _ => {
                    return Err(AppError::unsupported(format!(
                        "unsupported import format: {other}"
                    )));
                }
            }
            .map_err(command_error)?;
            let mut collection = imported.collection;
            // Insomnia's base environment (and OpenAPI's baseUrl) arrive as collection variables.
            let variables = std::mem::take(&mut collection.variables);
            (
                collection,
                imported.folders,
                imported.requests,
                variables,
                imported.warnings,
            )
        }
    };
    Ok(ParsedCollection {
        collection,
        folders,
        requests,
        variables,
        warnings,
    })
}

/// The ids of the workspace's collections called `name`, in workspace order.
fn clashing_collections(
    state: &AppState,
    workspace_id: &WorkspaceId,
    name: &str,
) -> Result<Vec<String>, AppError> {
    let same = |other: &str| other.trim().eq_ignore_ascii_case(name.trim());
    Ok(state
        .store()
        .list_collections(workspace_id.as_str())
        .map_err(command_error)?
        .into_iter()
        .filter(|collection| same(&collection.name))
        .map(|collection| collection.id.as_str().to_owned())
        .collect())
}

/// Imports a Postman, cURL, HAR, OpenAPI/Swagger, Insomnia, Bruno or Hoppscotch payload as a
/// collection, applying `on_conflict` when a collection of the same name exists.
pub(super) fn import_collection_text(
    state: &AppState,
    workspace_id: &WorkspaceId,
    format: &str,
    payload: &str,
    on_conflict: ImportConflict,
) -> Result<PostmanImportResult, AppError> {
    let mut parsed = parse_collection(format, workspace_id, payload)?;
    parsed.collection.variables = std::mem::take(&mut parsed.variables);
    let clashing = clashing_collections(state, workspace_id, &parsed.collection.name)?;

    let mode = match on_conflict {
        ImportConflict::KeepBoth => None,
        ImportConflict::Update => Some(MergeMode::Update),
        ImportConflict::Replace => Some(MergeMode::Replace),
        ImportConflict::Skip => {
            if let Some(existing) = clashing.first() {
                return Ok(PostmanImportResult {
                    workspace_id: workspace_id.as_str().to_owned(),
                    collection_id: existing.clone(),
                    collection_name: parsed.collection.name,
                    folder_count: 0,
                    request_count: 0,
                    environment_id: None,
                    variable_count: 0,
                    warnings: parsed.warnings,
                    skipped: true,
                    replaced: 0,
                    updated: 0,
                    undo_token: None,
                });
            }
            None
        }
    };
    if let (Some(mode), Some((existing, others))) = (mode, clashing.split_first()) {
        return import_into_existing(state, workspace_id, parsed, existing, others, mode);
    }
    persist_imported_collection(
        state,
        workspace_id,
        parsed.collection,
        parsed.folders,
        parsed.requests,
        parsed.warnings,
    )
}

/// "Update" or "Replace" into the stored collection `existing`: one transaction saves the
/// merged tree (paired rows keep their ids), then what a replace drops is deleted and the old
/// state is held for undo with the other same-name collections `others` a replace removes.
fn import_into_existing(
    state: &AppState,
    workspace_id: &WorkspaceId,
    parsed: ParsedCollection,
    existing: &str,
    others: &[String],
    mode: MergeMode,
) -> Result<PostmanImportResult, AppError> {
    let existing_id = CollectionId::new(existing).map_err(command_error)?;
    let mut warnings = parsed.warnings;
    let file_requests = parsed.requests.len();
    let file_folders = parsed.folders.len();
    let (before, plan) = {
        let store = state.store();
        let Some(before) = snapshot_collection(&store, workspace_id, &existing_id)? else {
            return Err(AppError::not_found(format!(
                "collection {existing} not found"
            )));
        };
        let plan = plan_merge(
            (&before.0, &before.1, &before.2),
            (parsed.collection, parsed.folders, parsed.requests),
            mode,
        );
        store
            .save_imported_collection(&plan.collection, &plan.folders, &plan.requests)
            .map_err(command_error)?;
        for id in &plan.removed_requests {
            if let Err(error) = store.delete_request(workspace_id, id) {
                warnings.push(format!(
                    "request {} could not be removed: {}",
                    id.as_str(),
                    command_error(error).message()
                ));
            }
        }
        for id in &plan.removed_folders {
            if let Err(error) = store.delete_folder(workspace_id, id) {
                warnings.push(format!(
                    "folder {} could not be removed: {}",
                    id.as_str(),
                    command_error(error).message()
                ));
            }
        }
        (before, plan)
    };
    let others: &[String] = if mode == MergeMode::Replace {
        others
    } else {
        &[]
    };
    let removed = plan.removed_requests.len();
    // The merged tree is committed: from here on a failure is a warning, never an error that
    // would hide the change and its undo token.
    let (token, replaced) = hold_update(
        state,
        workspace_id,
        before,
        plan.added_folders.clone(),
        plan.added_requests.clone(),
        others,
        &mut warnings,
    );
    if mode == MergeMode::Update {
        let kept = plan.kept_requests;
        if kept > 0 {
            warnings.push(format!(
                "{kept} stored request(s) are not in the file and were kept"
            ));
        }
    } else if removed > 0 {
        warnings.push(format!(
            "{removed} stored request(s) are not in the file and were removed"
        ));
    }
    state.sync.notify_changed();
    Ok(PostmanImportResult {
        workspace_id: workspace_id.as_str().to_owned(),
        collection_id: plan.collection.id.as_str().to_owned(),
        collection_name: plan.collection.name.clone(),
        folder_count: file_folders,
        request_count: file_requests,
        environment_id: None,
        variable_count: plan.collection.variables.len(),
        warnings,
        skipped: false,
        replaced,
        updated: plan.matched,
        undo_token: Some(token),
    })
}

/// Parses a Postman environment payload, including the `_pingdeck` extension keys.
pub(super) fn parse_environment(
    workspace_id: &WorkspaceId,
    payload: &str,
) -> Result<Environment, AppError> {
    let root: serde_json::Value = serde_json::from_str(payload).map_err(|error| {
        AppError::validation(format!("invalid Postman environment JSON: {error}"))
    })?;
    let name = root
        .get("name")
        .and_then(serde_json::Value::as_str)
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .ok_or_else(|| AppError::validation("Postman environment is missing name"))?;
    let values = root
        .get("values")
        .and_then(serde_json::Value::as_array)
        .ok_or_else(|| AppError::validation("Postman environment is missing values"))?;

    let mut environment = Environment::new(
        format!(
            "env_import_{}",
            std::time::SystemTime::now()
                .duration_since(std::time::UNIX_EPOCH)
                .map_or(0, |duration| duration.as_millis())
        ),
        workspace_id.as_str(),
        name,
    )
    .map_err(command_error)?;

    for value in values {
        let key = value
            .get("key")
            .and_then(serde_json::Value::as_str)
            .map(str::trim)
            .filter(|key| !key.is_empty());
        let Some(key) = key else {
            continue;
        };
        let raw = value
            .get("value")
            .map(|value| match value {
                serde_json::Value::String(text) => text.clone(),
                serde_json::Value::Null => String::new(),
                other => other.to_string(),
            })
            .unwrap_or_default();
        let is_secret = value
            .get("type")
            .and_then(serde_json::Value::as_str)
            .is_some_and(|value_type| value_type.eq_ignore_ascii_case("secret"));
        let mut variable = if is_secret {
            EnvironmentVariable::masked(key, raw)
        } else {
            EnvironmentVariable::plain(key, raw)
        }
        .map_err(command_error)?;
        variable.enabled = value
            .get("enabled")
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(true);
        if let Some(note) = value
            .get(PINGDECK_EXTENSION)
            .and_then(|extension| extension.get("note"))
            .and_then(serde_json::Value::as_str)
        {
            variable.note = note.to_owned();
        }
        environment.variables.push(variable);
    }
    let guard = |key: &str| {
        root.get(PINGDECK_EXTENSION)
            .and_then(|extension| extension.get(key))
            .and_then(serde_json::Value::as_bool)
            .unwrap_or(false)
    };
    environment.confirm_before_send = guard("confirm_before_send");
    environment.confirm_safe_methods = guard("confirm_safe_methods");
    Ok(environment)
}

/// The workspace's environment called like `name`, if any.
fn clashing_environment(
    state: &AppState,
    workspace_id: &WorkspaceId,
    name: &str,
) -> Result<Option<Environment>, AppError> {
    Ok(state
        .store()
        .list_environments(workspace_id.as_str())
        .map_err(command_error)?
        .into_iter()
        .find(|environment| environment.name.trim().eq_ignore_ascii_case(name.trim())))
}

/// Imports a Postman environment, applying `on_conflict` when one of the same name exists. The
/// result never carries a secret value; for `Skip` it is the existing environment.
pub(super) fn import_environment_text(
    state: &AppState,
    workspace_id: &WorkspaceId,
    payload: &str,
    on_conflict: ImportConflict,
) -> Result<EnvironmentDto, AppError> {
    let mut environment = parse_environment(workspace_id, payload)?;
    let existing = match on_conflict {
        ImportConflict::KeepBoth => None,
        _ => clashing_environment(state, workspace_id, &environment.name)?,
    };
    let Some(existing) = existing else {
        return save_imported_environment(state, environment)
            .map(|saved| EnvironmentDto::from(&saved));
    };
    if on_conflict == ImportConflict::Skip {
        return Ok(EnvironmentDto::from(&existing));
    }

    // Replace: same id and name, the file's variables; a secret the file blanks keeps its value.
    for variable in &mut environment.variables {
        let Some(stored) = existing
            .variables
            .iter()
            .find(|stored| stored.key == variable.key)
        else {
            continue;
        };
        variable.id.clone_from(&stored.id);
        if variable.is_secret
            && stored.is_secret
            && variable.value.as_deref().is_none_or(str::is_empty)
        {
            variable.value.clone_from(&stored.value);
        }
    }
    environment.id = existing.id.clone();
    environment.name = existing.name.clone();
    environment.declared_hosts = existing.declared_hosts.clone();
    save_environment_unique(&state.store(), &environment)?;
    state.sync.notify_changed();
    Ok(EnvironmentDto::from(&environment))
}

fn auth_kind(auth: &AuthConfig) -> Option<&'static str> {
    match auth {
        AuthConfig::Inherit | AuthConfig::None => None,
        AuthConfig::Bearer { .. } => Some("bearer"),
        AuthConfig::Basic { .. } => Some("basic"),
        AuthConfig::ApiKey { .. } => Some("apikey"),
        AuthConfig::OAuth2 { .. } => Some("oauth2"),
        AuthConfig::Digest { .. } => Some("digest"),
        AuthConfig::AwsSignatureV4 { .. } => Some("awsv4"),
    }
}

/// What a re-import into the first of `clashing` would do: the file's requests that pair with
/// stored ones, and the flow steps that call a request "Replace" removes (unpaired requests of
/// the first collection and every request of the others).
fn reimport_effects(
    state: &AppState,
    workspace_id: &WorkspaceId,
    clashing: &[String],
    parsed: &ParsedCollection,
) -> Result<(u32, u32), AppError> {
    let Some((first, others)) = clashing.split_first() else {
        return Ok((0, 0));
    };
    let store = state.store();
    let first_id = CollectionId::new(first).map_err(command_error)?;
    let Some(before) = snapshot_collection(&store, workspace_id, &first_id)? else {
        return Ok((0, 0));
    };
    let (matched, removed) = count_matches(
        (&before.0, &before.1, &before.2),
        (
            parsed.collection.clone(),
            parsed.folders.clone(),
            parsed.requests.clone(),
        ),
    );
    let mut gone: std::collections::HashSet<String> =
        removed.iter().map(|id| id.as_str().to_owned()).collect();
    gone.extend(
        store
            .list_requests(workspace_id.as_str())
            .map_err(command_error)?
            .into_iter()
            .filter(|request| {
                request
                    .collection_id
                    .as_ref()
                    .is_some_and(|id| others.iter().any(|other| other == id.as_str()))
            })
            .map(|request| request.id.as_str().to_owned()),
    );
    let steps = store
        .list_flows(workspace_id.as_str())
        .map_err(command_error)?
        .iter()
        .flat_map(|flow| &flow.steps)
        .filter(|step| gone.contains(step.request_id.trim()))
        .count();
    Ok((count(matched), count(steps)))
}

fn count(length: usize) -> u32 {
    u32::try_from(length).unwrap_or(u32::MAX)
}

/// What importing `payload` as `format` would create, without writing anything: counts, scripts,
/// secret-typed variables, authorization kinds, the parser's warnings and the name clash.
pub fn import_preview_impl(
    state: &AppState,
    workspace_id: String,
    format: String,
    payload: String,
) -> Result<ImportPreviewDto, AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let format = format.to_ascii_lowercase();
    let mut preview = ImportPreviewDto {
        format: format.clone(),
        name: None,
        folders: None,
        requests: None,
        variables: None,
        routes: None,
        secrets: 0,
        scripts: false,
        auth: Vec::new(),
        warnings: Vec::new(),
        clash: ImportClashDto {
            kind: ImportClashKind::None,
            existing_id: None,
        },
        script_issues: Vec::new(),
        affected_flow_steps: 0,
        matched_requests: 0,
    };
    match format.as_str() {
        "environment" => {
            let environment = parse_environment(&workspace_id, &payload)?;
            preview.variables = Some(count(environment.variables.len()));
            preview.secrets = count(
                environment
                    .variables
                    .iter()
                    .filter(|variable| variable.is_secret)
                    .count(),
            );
            preview.clash = ImportClashDto {
                kind: ImportClashKind::Environment,
                existing_id: clashing_environment(state, &workspace_id, &environment.name)?
                    .map(|existing| existing.id.as_str().to_owned()),
            };
            preview.name = Some(environment.name);
        }
        "mockoon" => {
            let (server, warnings) = import_mockoon_environment(&workspace_id, &payload)
                .map_err(AppError::validation)?;
            preview.routes = Some(count(server.routes.len()));
            preview.name = Some(server.name);
            preview.warnings = warnings;
        }
        _ => {
            let parsed = parse_collection(&format, &workspace_id, &payload)?;
            preview.folders = Some(count(parsed.folders.len()));
            preview.requests = Some(count(parsed.requests.len()));
            if matches!(format.as_str(), "openapi" | "swagger") {
                preview.routes = preview.requests;
            }
            preview.variables = Some(count(parsed.variables.len()));
            preview.secrets = count(
                parsed
                    .variables
                    .iter()
                    .filter(|variable| variable.is_secret)
                    .count(),
            );
            let blank = |text: &str| text.trim().is_empty();
            preview.scripts = !blank(&parsed.collection.pre_request_script)
                || !blank(&parsed.collection.test_script)
                || parsed.folders.iter().any(|folder| {
                    !blank(&folder.pre_request_script) || !blank(&folder.test_script)
                })
                || parsed.requests.iter().any(|request| {
                    !blank(&request.pre_request_script) || !blank(&request.test_script)
                });
            let auths = std::iter::once(&parsed.collection.auth)
                .chain(parsed.folders.iter().map(|folder| &folder.auth))
                .chain(parsed.requests.iter().map(|request| &request.auth));
            for kind in auths.filter_map(auth_kind) {
                if !preview.auth.iter().any(|seen| seen == kind) {
                    preview.auth.push(kind.to_owned());
                }
            }
            let collection = &parsed.collection;
            let scripts = [
                (
                    collection.name.as_str(),
                    collection.pre_request_script.as_str(),
                ),
                (collection.name.as_str(), collection.test_script.as_str()),
            ]
            .into_iter()
            .chain(parsed.folders.iter().flat_map(|folder| {
                [
                    (folder.name.as_str(), folder.pre_request_script.as_str()),
                    (folder.name.as_str(), folder.test_script.as_str()),
                ]
            }))
            .chain(parsed.requests.iter().flat_map(|request| {
                [
                    (request.name.as_str(), request.pre_request_script.as_str()),
                    (request.name.as_str(), request.test_script.as_str()),
                ]
            }));
            preview.script_issues = import_script_issues(scripts);
            let clashing = clashing_collections(state, &workspace_id, &parsed.collection.name)?;
            (preview.matched_requests, preview.affected_flow_steps) =
                reimport_effects(state, &workspace_id, &clashing, &parsed)?;
            preview.clash = ImportClashDto {
                kind: ImportClashKind::Collection,
                existing_id: clashing.into_iter().next(),
            };
            preview.name = Some(parsed.collection.name);
            preview.warnings = parsed.warnings;
        }
    }
    Ok(preview)
}

/// One pasted cURL command as an unsaved request: the cURL importer's request with no
/// collection or folder; nothing is written. A command without a URL is a validation error, so the
/// URL box keeps the pasted text.
pub fn request_curl_import_impl(workspace_id: String, text: String) -> Result<Request, AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let imported = crate::importers::import_curl(&text, workspace_id.as_str())
        .map_err(|error| AppError::validation(error.to_string()))?;
    let mut request = imported
        .requests
        .into_iter()
        .next()
        .ok_or_else(|| AppError::validation("the cURL command has no request"))?;
    request.collection_id = None;
    request.folder_id = None;
    request.position = 0;
    Ok(request)
}
