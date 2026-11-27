//! Plumbing shared by the six SpanBay tools.
//!
//! Single responsibility: the chat's SpanBay scope (workspace and default environment),
//! environments by name, the secret values of a workspace for the redaction step, the
//! collection and folder paths requests are listed under, the per-run call limits, and the
//! shape of a tool result (JSON text through [`redact_for_model`], errors as results).

use std::collections::HashMap;
use std::sync::Mutex;

use serde_json::{Value, json};

use super::redact::{NamedSecret, redact_for_model};
use crate::agent::model::{ToolErrorCode, coded_error_text};
use crate::agent::tools::{
    ToolClass, ToolOutput, ToolResultMeta, ToolResultView, ToolSource, ToolSpec,
};
use crate::app::{AppState, command_error};
use crate::error::AppError;
use crate::workspace::models::{Collection, EnvironmentVariable, Folder, Request, Workspace};
use crate::workspace::{Environment, EnvironmentId, WorkspaceId, WorkspaceStore};

/// The error a tool returns when its chat has no SpanBay workspace.
const UNBOUND: &str = "This chat is not bound to a SpanBay workspace, so requests and \
                       environments are out of reach. Ask the user to bind it from the chat \
                       header.";

/// The error a tool returns when the workspace its chat is bound to was deleted.
const WORKSPACE_DELETED: &str = "The SpanBay workspace this chat was bound to has been \
                                 deleted, so requests and environments are out of reach. Ask \
                                 the user to bind the chat to another workspace from the chat \
                                 header.";

/// The static spec of a SpanBay tool: reads are read-only and parallel-safe, writes are a barrier.
pub(super) fn tool_spec(
    name: &str,
    description: &str,
    class: ToolClass,
    destructive: bool,
    input_schema: Value,
) -> ToolSpec {
    let read = class == ToolClass::SpanBayRead;
    ToolSpec {
        name: name.to_owned(),
        description: description.to_owned(),
        input_schema,
        read_only: read,
        destructive,
        source: ToolSource::Builtin,
        class,
        parallel_safe: read,
    }
}

/// The SpanBay scope a chat acts in, fixed when the chat started.
pub(super) struct Scope {
    /// The workspace the chat is bound to.
    pub workspace: Workspace,
    /// The environment that was active then, the default of a send.
    pub environment_id: Option<String>,
}

impl Scope {
    /// `{id, name}` of the workspace, which every result names.
    pub(super) fn tag(&self) -> Value {
        json!({"id": self.workspace.id.as_str(), "name": self.workspace.name})
    }
}

/// The scope of `session_id`.
///
/// # Errors
/// [`AppError::Validation`] when the chat is unbound or its workspace was deleted.
pub(super) fn bound_scope(state: &AppState, session_id: &str) -> Result<Scope, AppError> {
    let store = state.store();
    let binding = store
        .session_spanbay_binding(session_id)
        .map_err(command_error)?
        .ok_or_else(|| AppError::validation(coded_error_text(ToolErrorCode::NotBound, UNBOUND)))?;
    let id = WorkspaceId::new(&binding.workspace_id).map_err(command_error)?;
    let workspace = store
        .get_workspace(&id)
        .map_err(command_error)?
        .ok_or_else(|| {
            AppError::validation(coded_error_text(
                ToolErrorCode::WorkspaceDeleted,
                WORKSPACE_DELETED,
            ))
        })?;
    Ok(Scope {
        workspace,
        environment_id: binding.environment_id,
    })
}

/// The environment `name` of the workspace (exact name first, then ignoring case); without a
/// name the chat's default one, if it still exists.
///
/// # Errors
/// [`AppError::Validation`] `No environment "<name>". Available: …` for an unknown name.
pub(super) fn find_environment(
    store: &WorkspaceStore,
    workspace_id: &WorkspaceId,
    name: Option<&str>,
    default_id: Option<&str>,
) -> Result<Option<Environment>, AppError> {
    let Some(name) = name.map(str::trim).filter(|name| !name.is_empty()) else {
        return match default_id.map(EnvironmentId::new).transpose() {
            Ok(Some(id)) => store
                .get_environment(workspace_id, &id)
                .map_err(command_error),
            _ => Ok(None),
        };
    };
    let all = store
        .list_environments(workspace_id.as_str())
        .map_err(command_error)?;
    if let Some(found) = all
        .iter()
        .find(|environment| environment.name == name)
        .or_else(|| {
            all.iter()
                .find(|environment| environment.name.eq_ignore_ascii_case(name))
        })
    {
        return Ok(Some(found.clone()));
    }
    let names: Vec<&str> = all
        .iter()
        .map(|environment| environment.name.as_str())
        .collect();
    Err(AppError::validation(format!(
        "No environment \"{name}\". Available: {}.",
        if names.is_empty() {
            "none".to_owned()
        } else {
            names.join(", ")
        }
    )))
}

/// Every secret variable value of the workspace: its environments, its globals and its
/// collections. Redaction masks all of them, not just the environment a call uses.
///
/// # Errors
/// The store failure: a result is never built without its secrets.
pub fn workspace_secrets(
    store: &WorkspaceStore,
    workspace_id: &str,
) -> Result<Vec<NamedSecret>, AppError> {
    let mut secrets = Vec::new();
    let mut take = |variables: &[EnvironmentVariable]| {
        for variable in variables.iter().filter(|variable| variable.is_secret) {
            if let Some(value) = variable.value.as_deref().filter(|value| !value.is_empty()) {
                secrets.push(NamedSecret {
                    name: variable.key.clone(),
                    value: value.to_owned(),
                });
            }
        }
    };
    for environment in store
        .list_environments(workspace_id)
        .map_err(command_error)?
    {
        take(&environment.variables);
    }
    take(
        &store
            .list_global_variables(workspace_id)
            .map_err(command_error)?,
    );
    for collection in store
        .list_collections(workspace_id)
        .map_err(command_error)?
    {
        take(&collection.variables);
    }
    Ok(secrets)
}

/// The secret values of every workspace, for the app-wide tools.
///
/// # Errors
/// As [`workspace_secrets`].
pub fn all_secrets(store: &WorkspaceStore) -> Result<Vec<NamedSecret>, AppError> {
    let mut secrets = Vec::new();
    for workspace in store.list_workspaces().map_err(command_error)? {
        secrets.extend(workspace_secrets(store, workspace.id.as_str())?);
    }
    Ok(secrets)
}

/// The collections and folders of a workspace, to name the place of a request.
pub(super) struct Tree {
    collections: Vec<Collection>,
    folders: HashMap<String, Folder>,
}

impl Tree {
    /// Loads the hierarchy of `workspace_id`.
    ///
    /// # Errors
    /// The store failure.
    pub(super) fn load(
        store: &WorkspaceStore,
        workspace_id: &WorkspaceId,
    ) -> Result<Self, AppError> {
        let collections = store
            .list_collections(workspace_id.as_str())
            .map_err(command_error)?;
        let mut folders = HashMap::new();
        for collection in &collections {
            for folder in store
                .list_folders(workspace_id, &collection.id)
                .map_err(command_error)?
            {
                folders.insert(folder.id.as_str().to_owned(), folder);
            }
        }
        Ok(Self {
            collections,
            folders,
        })
    }

    /// The collections, in stored order.
    pub(super) fn collections(&self) -> &[Collection] {
        &self.collections
    }

    /// The folders of `collection_id` whose parent is `parent` (`None`: the top level).
    pub(super) fn children(&self, collection_id: &str, parent: Option<&str>) -> Vec<&Folder> {
        let mut found: Vec<&Folder> = self
            .folders
            .values()
            .filter(|folder| {
                folder.collection_id.as_str() == collection_id
                    && folder.parent_folder_id.as_ref().map(|id| id.as_str()) == parent
            })
            .collect();
        found.sort_by(|a, b| (a.position, &a.name).cmp(&(b.position, &b.name)));
        found
    }

    /// `Collection/Folder/Subfolder` of a request (the collection alone at the top level, empty
    /// for a loose request).
    pub(super) fn location(&self, request: &Request) -> String {
        let Some(collection_id) = request.collection_id.as_ref() else {
            return String::new();
        };
        let mut parts: Vec<&str> = Vec::new();
        let mut next = request.folder_id.as_ref().map(|id| id.as_str());
        // A cycle cannot be stored, but a bound keeps a damaged file from looping.
        for _ in 0..64 {
            let Some(folder) = next.and_then(|id| self.folders.get(id)) else {
                break;
            };
            parts.push(&folder.name);
            next = folder.parent_folder_id.as_ref().map(|id| id.as_str());
        }
        if let Some(collection) = self
            .collections
            .iter()
            .find(|collection| &collection.id == collection_id)
        {
            parts.push(&collection.name);
        }
        parts.reverse();
        parts.join("/")
    }

    /// `Collection/Folder/<request name>`, the `request_path` of a request.
    pub(super) fn full_path(&self, request: &Request) -> String {
        let location = self.location(request);
        if location.is_empty() {
            request.name.clone()
        } else {
            format!("{location}/{}", request.name)
        }
    }
}

/// How many calls of a tool one run may make (20 sends, 3 probes per turn).
pub(crate) struct RunLimit {
    limit: u32,
    counts: Mutex<HashMap<String, u32>>,
}

impl RunLimit {
    /// A limit of `limit` calls per run.
    pub(crate) fn new(limit: u32) -> Self {
        Self {
            limit,
            counts: Mutex::new(HashMap::new()),
        }
    }

    /// Counts one call of `run_id`; `false` once the run used its allowance.
    pub(crate) fn take(&self, run_id: &str) -> bool {
        let mut counts = self
            .counts
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // Old runs never come back: drop them rather than grow without bound.
        if counts.len() >= 256 && !counts.contains_key(run_id) {
            counts.clear();
        }
        let used = counts.entry(run_id.to_owned()).or_insert(0);
        if *used >= self.limit {
            return false;
        }
        *used += 1;
        true
    }

    /// The allowance of one run.
    pub(super) fn limit(&self) -> u32 {
        self.limit
    }
}

/// A successful result: `value` as JSON text through the redaction step, cut at `cap` bytes.
pub(super) fn json_output(value: &Value, secrets: &[NamedSecret], cap: usize) -> ToolOutput {
    // The result's own `view` object is the UI's link-out: the typed hint travels
    // beside the text, so UI and model can never disagree about it.
    let meta = value.get("view").and_then(|view| {
        serde_json::from_value::<ToolResultView>(view.clone())
            .ok()
            .map(|view| ToolResultMeta { view: Some(view) })
    });
    let mut output = ToolOutput::text(redact_for_model(&value.to_string(), secrets, cap));
    output.meta = meta;
    output
}

/// A failed call as an error result; the message passes the redaction step too.
pub(super) fn error_output(error: &AppError, secrets: &[NamedSecret]) -> ToolOutput {
    ToolOutput::error(redact_for_model(error.message(), secrets, 4 * 1024))
}

/// The `Timed out after <n> s.` result, behind a `timeout` code line.
pub(super) fn timed_out(seconds: u64) -> ToolOutput {
    ToolOutput::error(coded_error_text(
        ToolErrorCode::Timeout,
        &format!("Timed out after {seconds} s."),
    ))
}
