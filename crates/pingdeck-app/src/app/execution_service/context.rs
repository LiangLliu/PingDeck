use std::collections::{BTreeMap, BTreeSet};

use tokio::task::spawn_blocking;

use super::super::{AppState, command_error};
use crate::error::AppError;
use crate::workspace::models::TlsCertificate;
use crate::workspace::{
    Collection, CollectionId, CookieJarEntry, Environment, EnvironmentId, EnvironmentVariable,
    Folder, FolderId, Request, VariableScopes, WorkspaceId, WorkspaceStore,
};

/// Loop position and data row a collection run hands to scripts as `pm.info` and iteration data,
/// plus the caller's answer to the production guard.
#[derive(Debug, Clone, Default)]
pub struct ExecutionMeta {
    pub(super) iteration: u32,
    pub(super) iteration_count: u32,
    pub(super) data: BTreeMap<String, String>,
    /// The caller's `confirmed` flag: a pre-request script that widens the method to one the
    /// environment guards is only let through when the caller confirmed.
    pub(super) confirmed: Option<bool>,
    /// A Stop after the response headers keeps the partial response (a single send from the
    /// UI); the post-response scripts are skipped and the exchange is recorded.
    pub(super) keep_partial_on_cancel: bool,
    /// The whole response body is streamed to this file, chosen in a native save dialog.
    pub(super) save_body_to: Option<std::path::PathBuf>,
    /// Receives the credential values the send actually carried (see [`SentCredentials`]).
    pub(super) sent_credentials: Option<SentCredentials>,
}

/// The credential values one send actually carried: its auth fields, credential headers and
/// query parameters after the pre-request scripts and inherited auth, and the OAuth 2.0 access
/// token it attached. Memory only, never serialized; the agent's `send_request` masks them by
/// value in what the model reads (`docs/architecture.md` §11).
#[derive(Clone, Default)]
pub struct SentCredentials(std::sync::Arc<std::sync::Mutex<Vec<String>>>);

impl SentCredentials {
    /// Records `values` (empty ones and duplicates are skipped).
    pub(crate) fn extend(&self, values: impl IntoIterator<Item = String>) {
        let mut held = self
            .0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        for value in values {
            if !value.is_empty() && !held.contains(&value) {
                held.push(value);
            }
        }
    }

    /// The values recorded so far.
    #[must_use]
    pub fn values(&self) -> Vec<String> {
        self.0
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .clone()
    }
}

impl std::fmt::Debug for SentCredentials {
    fn fmt(&self, formatter: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        formatter.write_str("SentCredentials(…)")
    }
}

impl ExecutionMeta {
    /// Collects the credential values the send carries into `sink`.
    #[must_use]
    pub fn reporting_credentials(mut self, sink: SentCredentials) -> Self {
        self.sent_credentials = Some(sink);
        self
    }

    /// The meta of an execution whose caller answered the production guard with `confirmed`.
    pub fn confirmed(confirmed: Option<bool>) -> Self {
        Self {
            confirmed,
            ..Self::default()
        }
    }

    /// A single send from the UI: Stop keeps what arrived.
    #[must_use]
    pub fn interactive(mut self) -> Self {
        self.keep_partial_on_cancel = true;
        self
    }

    /// Streams the whole response body to `path` as it arrives.
    #[must_use]
    pub fn saving_body_to(mut self, path: std::path::PathBuf) -> Self {
        self.save_body_to = Some(path);
        self
    }
}

/// Everything one execution reads: the environment, cookie jar, collection and
/// folder chain, globals and TLS certificates, plus script-local variables.
#[derive(Clone)]
pub struct ExecutionContext {
    pub environment: Option<Environment>,
    pub cookie_jar: Vec<CookieJarEntry>,
    pub collection: Option<Collection>,
    pub folders: Vec<Folder>,
    pub globals: Vec<EnvironmentVariable>,
    pub certificates: Vec<TlsCertificate>,
    pub local: BTreeMap<String, String>,
    /// Keys of `local` whose values are secrets (a flow's secret extractions): masked wherever
    /// the execution records what it sent.
    pub local_secrets: BTreeSet<String>,
    /// The runner's current data row; sits between the environment and local scopes.
    pub data: BTreeMap<String, String>,
}

impl ExecutionContext {
    /// The variable scopes this execution resolves `{{tokens}}` against.
    pub fn scopes(&self) -> VariableScopes {
        VariableScopes::new()
            .with_globals(self.globals.clone())
            .with_collection_vars(
                self.collection
                    .as_ref()
                    .map(|collection| collection.variables.clone())
                    .unwrap_or_default(),
            )
            .with_environment_vars(
                self.environment
                    .as_ref()
                    .map(|environment| environment.variables.clone())
                    .unwrap_or_default(),
            )
            .with_data_vars(plain_variables(&self.data))
            .with_local_vars(
                plain_variables(&self.local)
                    .into_iter()
                    .map(|variable| EnvironmentVariable {
                        is_secret: self.local_secrets.contains(&variable.key),
                        ..variable
                    })
                    .collect(),
            )
    }

    /// A copy with only the variable scopes (environment, collection, globals): what script
    /// persistence compares, without the cookie jar and certificates.
    pub(super) fn variable_scopes(&self) -> Self {
        Self {
            environment: self.environment.clone(),
            cookie_jar: Vec::new(),
            collection: self.collection.clone(),
            folders: Vec::new(),
            globals: self.globals.clone(),
            certificates: Vec::new(),
            local: BTreeMap::new(),
            local_secrets: BTreeSet::new(),
            data: BTreeMap::new(),
        }
    }
}

/// Plain variables for every key/value pair that forms a valid variable.
fn plain_variables(values: &BTreeMap<String, String>) -> Vec<EnvironmentVariable> {
    values
        .iter()
        .filter_map(|(key, value)| EnvironmentVariable::plain(key, value.clone()).ok())
        .collect()
}

/// Runs store work on a blocking worker: the store is a synchronous SQLite connection behind a
/// mutex and must not be driven from an async runtime worker. The closure takes the store guard
/// itself, so no guard is ever held across an `.await`.
pub(super) async fn blocking<T: Send + 'static>(
    state: &AppState,
    work: impl FnOnce(&AppState) -> Result<T, AppError> + Send + 'static,
) -> Result<T, AppError> {
    let state = state.clone();
    spawn_blocking(move || work(&state))
        .await
        .map_err(|error| AppError::internal(format!("store task failed: {error}")))?
}

/// Loads the environment, cookies, collection chain, globals and certificates `request` runs with.
pub(crate) fn execution_context(
    store: &WorkspaceStore,
    request: &Request,
    environment_id: Option<&str>,
) -> Result<ExecutionContext, AppError> {
    execution_context_for(
        store,
        request.workspace_id.as_str(),
        request.collection_id.as_ref().map(CollectionId::as_str),
        request.folder_id.as_ref().map(FolderId::as_str),
        environment_id,
    )
}

/// Like [`execution_context`] for an explicit collection/folder, before a request exists.
pub fn execution_context_for(
    store: &WorkspaceStore,
    workspace_id: &str,
    collection_id: Option<&str>,
    folder_id: Option<&str>,
    environment_id: Option<&str>,
) -> Result<ExecutionContext, AppError> {
    let workspace = WorkspaceId::new(workspace_id).map_err(command_error)?;
    let folder_id = folder_id
        .map(FolderId::new)
        .transpose()
        .map_err(command_error)?;

    let mut collection = match collection_id
        .map(CollectionId::new)
        .transpose()
        .map_err(command_error)?
    {
        Some(collection_id) => Some(
            store
                .get_collection(&workspace, &collection_id)
                .map_err(command_error)?
                .ok_or_else(|| {
                    AppError::not_found(format!("collection {} not found", collection_id.as_str()))
                })?,
        ),
        None => None,
    };

    if let Some(folder_id) = folder_id.as_ref() {
        let folder = store
            .get_folder(&workspace, folder_id)
            .map_err(command_error)?
            .ok_or_else(|| {
                AppError::not_found(format!("folder {} not found", folder_id.as_str()))
            })?;
        if let Some(collection) = collection.as_ref() {
            if folder.collection_id != collection.id {
                return Err(AppError::conflict(format!(
                    "folder {} belongs to collection {}",
                    folder_id.as_str(),
                    folder.collection_id.as_str()
                )));
            }
        } else {
            collection = Some(
                store
                    .get_collection(&workspace, &folder.collection_id)
                    .map_err(command_error)?
                    .ok_or_else(|| {
                        AppError::not_found(format!(
                            "collection {} not found",
                            folder.collection_id.as_str()
                        ))
                    })?,
            );
        }
    }

    let folders = match collection.as_ref() {
        Some(collection) => store
            .list_folders(&workspace, &collection.id)
            .map_err(command_error)?,
        None => Vec::new(),
    };

    let environment = match environment_id
        .map(EnvironmentId::new)
        .transpose()
        .map_err(command_error)?
    {
        Some(environment_id) => Some(
            store
                .get_environment(&workspace, &environment_id)
                .map_err(command_error)?
                .ok_or_else(|| {
                    AppError::not_found(format!(
                        "environment {} not found",
                        environment_id.as_str()
                    ))
                })?,
        ),
        None => None,
    };

    let globals = store
        .list_global_variables(workspace_id)
        .map_err(command_error)?;
    let cookie_jar = store.list_cookie_jar(workspace_id).map_err(command_error)?;
    let certificates = enabled_certificates(store, workspace_id)?;

    Ok(ExecutionContext {
        environment,
        cookie_jar,
        collection,
        folders,
        globals,
        certificates,
        local: BTreeMap::new(),
        local_secrets: BTreeSet::new(),
        data: BTreeMap::new(),
    })
}

/// Lists the enabled TLS client certificates of a workspace.
fn enabled_certificates(
    store: &WorkspaceStore,
    workspace_id: &str,
) -> Result<Vec<TlsCertificate>, AppError> {
    Ok(store
        .list_tls_certificates(workspace_id)
        .map_err(command_error)?
        .into_iter()
        .filter(|certificate| certificate.enabled)
        .collect())
}
