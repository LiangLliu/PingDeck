//! Input and output DTOs of the application service layer.
//!
//! Single responsibility: define the exact shapes a UI shell sends and
//! receives. They are plain `serde` types with public fields, so a Tauri
//! command or a test can build them directly.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use super::environment_service::effective_variable_id;
use super::factory_service::new_entity_id;
pub use pingdeck_model::runner_item::{CollectionRunStatus, CollectionRunSummary, RunnerItem};

use crate::error::AppError;
use crate::mock::MockLiveBucket;
use crate::workspace::ai_request::{AiRequestConfig, RequestMode};
use crate::workspace::models::RequestExample;
use crate::workspace::{
    Assertion, AuthConfig, EnvironmentVariable, HttpMethod, KeyValue, MockRoute, MockServer,
    NetworkOptions, ProtocolKind, RequestBody,
};

/// Input of `environment_save`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveEnvironmentInput {
    /// Environment being saved.
    pub id: String,
    /// Workspace that owns it.
    pub workspace_id: String,
    /// Display name.
    pub name: String,
    /// Variables in editor order.
    pub variables: Vec<EnvironmentVariableInput>,
    /// Production guard; `None` keeps the stored flag.
    #[serde(default)]
    pub confirm_before_send: Option<bool>,
    /// Widens the guard to safe methods; `None` keeps the stored flag.
    #[serde(default)]
    pub confirm_safe_methods: Option<bool>,
    /// Hosts the user added by hand; `None` keeps the stored list.
    #[serde(default)]
    pub declared_hosts: Option<Vec<String>>,
    /// The `revision` the editor loaded; a stored environment with another one refuses the save
    /// (`Conflict`). `None` saves unconditionally (import, sync, scripts).
    #[serde(default)]
    pub base_revision: Option<String>,
}

/// One environment variable as the editor sends it.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct EnvironmentVariableInput {
    /// Identity the list returned for this row; absent for a row the editor just added. A rename
    /// keeps a secret only when the row carries the id of the variable it renames.
    #[serde(default)]
    pub id: Option<String>,
    /// Variable name.
    pub key: String,
    /// Plain value, or a secret's new value. A secret row sends `None` to keep the stored value
    /// (secret values never reach the webview, so it cannot echo them back); `Some("")` clears
    /// it and `Some(text)` replaces it.
    pub value: Option<String>,
    /// Whether the value is stored masked.
    pub is_secret: bool,
    /// Whether the variable participates in resolution.
    pub enabled: bool,
    /// Free-text note shown next to the variable.
    #[serde(default)]
    pub note: String,
}

impl EnvironmentVariableInput {
    /// Validates and converts the editor row into a domain variable.
    ///
    /// The single validation path for environments, globals and collections. `owner` is the id
    /// of the environment, collection or workspace the list belongs to and `stored` the
    /// variables currently saved there. A plain variable needs a value. A secret row without
    /// one keeps the stored value of the variable its `id` names, or of the same key when it
    /// sends no id; a row that matches no stored secret is rejected. The variable keeps the
    /// stored identity it matched; a new one gets a fresh `var_…` id.
    pub fn into_domain(
        self,
        owner: &str,
        stored: &[EnvironmentVariable],
    ) -> Result<EnvironmentVariable, AppError> {
        let key = self.key.trim();
        let given_id = self
            .id
            .as_deref()
            .map(str::trim)
            .filter(|id| !id.is_empty());
        let matched = match given_id {
            Some(id) => stored
                .iter()
                .find(|variable| effective_variable_id(owner, variable) == id),
            None => stored.iter().find(|variable| variable.key == key),
        };
        let value = match self.value {
            Some(value) => value,
            None if self.is_secret => matched
                .filter(|variable| variable.is_secret)
                .map(|variable| variable.value.clone().unwrap_or_default())
                .ok_or_else(|| {
                    AppError::validation(format!("secret variable {} needs a value", self.key))
                })?,
            None => {
                return Err(AppError::validation(format!(
                    "plain variable {} is missing value",
                    self.key
                )));
            }
        };
        let created = if self.is_secret {
            EnvironmentVariable::masked(&self.key, value)
        } else {
            EnvironmentVariable::plain(&self.key, value)
        };

        let mut variable = created.map_err(|error| {
            AppError::validation(format!("invalid variable {}: {error}", self.key))
        })?;
        variable.enabled = self.enabled;
        variable.note = self.note;
        // A stored row without an id (a script or import wrote it) takes the effective id the
        // editor already holds for it, so the next list returns the same identity.
        variable.id = match (matched, given_id) {
            (Some(matched), _) if !matched.id.is_empty() => matched.id.clone(),
            (_, Some(id)) => id.to_owned(),
            (_, None) => new_entity_id("var"),
        };
        Ok(variable)
    }
}

/// Input of `collection_save`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveCollectionInput {
    /// Collection being saved.
    pub id: String,
    /// Workspace that owns it.
    pub workspace_id: String,
    /// Display name.
    pub name: String,
    /// Markdown description.
    #[serde(default)]
    pub description: String,
    /// Collection-level auth.
    #[serde(default)]
    pub auth: AuthConfig,
    /// Collection variables.
    #[serde(default)]
    pub variables: Vec<EnvironmentVariableInput>,
    /// Script run before every request in the collection.
    #[serde(default)]
    pub pre_request_script: String,
    /// Script run after every response in the collection.
    #[serde(default)]
    pub test_script: String,
    /// Author-defined order.
    #[serde(default)]
    pub position: i64,
}

/// Input of `folder_save`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct SaveFolderInput {
    /// Folder being saved.
    pub id: String,
    /// Owning collection.
    pub collection_id: String,
    /// Parent folder, for nesting.
    pub parent_folder_id: Option<String>,
    /// Display name.
    pub name: String,
    /// Markdown description.
    #[serde(default)]
    pub description: String,
    /// Folder-level auth.
    #[serde(default)]
    pub auth: AuthConfig,
    /// Script run before every request in the folder.
    #[serde(default)]
    pub pre_request_script: String,
    /// Script run after every response in the folder.
    #[serde(default)]
    pub test_script: String,
    /// Author-defined order.
    #[serde(default)]
    pub position: i64,
}

/// Input of `folder_move`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveFolderInput {
    /// Workspace that owns the folder and the destination.
    pub workspace_id: String,
    /// Folder being moved.
    pub folder_id: String,
    /// Destination collection.
    pub target_collection_id: String,
    /// Destination parent folder inside that collection.
    pub target_parent_folder_id: Option<String>,
}

/// Input of `request_move`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MoveRequestInput {
    /// Workspace that owns the request and the destination.
    pub workspace_id: String,
    /// Request being moved.
    pub request_id: String,
    /// Destination collection, `None` to unfile.
    pub target_collection_id: Option<String>,
    /// Destination folder inside that collection.
    pub target_folder_id: Option<String>,
    /// New author-defined order.
    pub position: Option<i64>,
}

/// Input of `globals_save`.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct GlobalsSaveInput {
    /// Workspace whose globals are replaced.
    pub workspace_id: String,
    /// Full replacement list, in editor order.
    pub variables: Vec<EnvironmentVariableInput>,
    /// The `revision` the editor loaded; other stored globals refuse the save (`Conflict`).
    /// `None` saves unconditionally.
    #[serde(default)]
    pub base_revision: Option<String>,
}

/// Input of `request_save`, and the draft body of `request_execute_draft`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct SaveRequestInput {
    /// Request id (stable across saves).
    pub id: String,
    /// Workspace that owns it.
    pub workspace_id: String,
    /// Owning collection, when filed.
    pub collection_id: Option<String>,
    /// Owning folder, when nested.
    pub folder_id: Option<String>,
    /// Display name.
    pub name: String,
    /// HTTP, GraphQL, WebSocket or gRPC.
    pub protocol: ProtocolKind,
    /// HTTP method.
    pub method: HttpMethod,
    /// URL template with `{{variables}}`.
    pub url: String,
    /// Query parameters.
    pub query_params: Vec<KeyValue>,
    /// Path variables substituted into the URL.
    #[serde(default)]
    pub path_variables: Vec<KeyValue>,
    /// Headers.
    pub headers: Vec<KeyValue>,
    /// Auth configuration.
    pub auth: AuthConfig,
    /// Request body.
    pub body: RequestBody,
    /// Timeouts, redirects, TLS and proxy options.
    pub network_options: NetworkOptions,
    /// Assertions evaluated on the response.
    #[serde(default)]
    pub assertions: Vec<Assertion>,
    /// Markdown description.
    #[serde(default)]
    pub description: String,
    /// Script run before the request is sent.
    #[serde(default)]
    pub pre_request_script: String,
    /// Script run after the response arrives.
    #[serde(default)]
    pub test_script: String,
    /// Author-defined order.
    #[serde(default)]
    pub position: i64,
    /// HTTP or AI API mode.
    #[serde(default)]
    pub mode: RequestMode,
    /// AI API settings; `None` for HTTP requests.
    #[serde(default)]
    pub ai: Option<AiRequestConfig>,
    /// Saved response examples, read by `request_save` only. Absent keeps the stored ones; a
    /// list, empty included, replaces them (the Docs tab deletes an example this way).
    #[serde(default)]
    pub examples: Option<Vec<RequestExample>>,
}

/// Input of `request_execute`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExecuteRequestInput {
    /// Workspace that owns the request and environment.
    pub workspace_id: String,
    /// Saved request to execute.
    pub request_id: String,
    /// Environment whose variables apply, if any.
    pub environment_id: Option<String>,
    /// The user confirmed sending into a guarded environment: without it, a guarded
    /// environment fails with `AppError::ConfirmationRequired` before anything is sent.
    #[serde(default)]
    pub confirmed: Option<bool>,
}

/// Input of `request_execute_draft`: an unsaved editor buffer.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ExecuteDraftRequestInput {
    /// The draft request.
    pub request: SaveRequestInput,
    /// Environment whose variables apply, if any.
    pub environment_id: Option<String>,
    /// The user confirmed sending into a guarded environment.
    #[serde(default)]
    pub confirmed: Option<bool>,
    /// `false` writes no history row and no cookies (the Mock "Try it" drawer: the mock's own
    /// hit row is the record). Absent means `true`.
    #[serde(default)]
    pub save_to_history: Option<bool>,
}

/// Result of importing a collection from any supported format.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostmanImportResult {
    /// Workspace the collection landed in.
    pub workspace_id: String,
    /// Id of the saved collection (unique on import).
    pub collection_id: String,
    /// Collection name.
    pub collection_name: String,
    /// Folders of the file (created, or for an update or replace written into the collection).
    pub folder_count: usize,
    /// Requests of the file (created, or for an update or replace written into the collection).
    pub request_count: usize,
    /// Environment created from the import, when the payload carried one.
    pub environment_id: Option<String>,
    /// Collection variables imported.
    pub variable_count: usize,
    /// Non-fatal notes about data that could not be imported.
    pub warnings: Vec<String>,
    /// `true` when `on_conflict` was `skip` and a collection of that name exists: nothing was
    /// imported, and `collection_id` names the existing one.
    #[serde(default)]
    pub skipped: bool,
    /// Other collections of the same name that `on_conflict: replace` deleted.
    #[serde(default)]
    pub replaced: usize,
    /// Requests of the file that `update` or `replace` paired with stored ones: they kept their
    /// ids (`request_count` counts the file's requests).
    #[serde(default)]
    pub updated: usize,
    /// After an update or replace: `restore_deleted` with this token puts the collection back as
    /// it was (and the replaced collections); `commit_delete_undo` once the undo is no longer
    /// offered.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_token: Option<super::undo_store::UndoToken>,
}

/// Live status of one mock server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockServerStatusDto {
    /// Server id.
    pub server_id: String,
    /// Whether a listener is bound right now.
    pub running: bool,
    /// Bound port (the configured port when stopped).
    pub port: u16,
    /// Loopback URL the listener answers on.
    pub bound_url: String,
    /// The endpoint prefix the listener serves, normalized (empty or `/segment`): the one fixed
    /// when it started while running, the saved one while stopped. A saved prefix edit applies
    /// only after a restart, so the two differ in between.
    pub endpoint_prefix: String,
    /// When the listener was bound, in Unix milliseconds; `None` while stopped.
    pub started_at_ms: Option<i64>,
    /// When the server was last stopped in this session (Unix milliseconds); `None` while it runs
    /// and for a server not stopped since the app started (kept in memory only).
    pub stopped_at_ms: Option<i64>,
    /// Requests served since the listener started (0 while stopped). Counts every request,
    /// including those the bounded log has already dropped.
    pub hit_count: u64,
    /// Whether the running server records proxied exchanges (in memory, until it stops).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub recording: bool,
    /// Proxied exchanges recorded so far (one per method and path).
    #[serde(default, skip_serializing_if = "is_zero")]
    pub recorded_count: usize,
}

fn is_zero(value: &usize) -> bool {
    *value == 0
}

/// Recording state of a running mock server after switching it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MockRecordingStatusDto {
    /// Whether proxied exchanges are recorded.
    pub enabled: bool,
    /// Proxied exchanges recorded so far.
    pub count: usize,
}

/// Routes built from the recorded proxied exchanges of a running mock server.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockRecordingRoutesDto {
    /// New routes, in recording order, positioned after the server's routes.
    pub routes: Vec<MockRoute>,
    /// Recorded exchanges a route of the server already answers.
    pub skipped: usize,
}

/// The live data of a mock server: the in-memory buckets CRUD routes changed, next to
/// the data the user saved.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct MockLiveDataDto {
    /// Server id.
    pub server_id: String,
    /// Whether the server is running; a stopped one has no live data (it equals the saved data
    /// once started).
    pub running: bool,
    /// Buckets in the order of the server's data buckets; empty while stopped.
    pub buckets: Vec<MockLiveBucket>,
}

/// Result of importing a PingDeck mock-server export file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockImportResultDto {
    /// The saved servers: stopped, with fresh ids.
    pub servers: Vec<MockServer>,
    /// Non-fatal import notes (proxying switched off, a port that had to change).
    pub warnings: Vec<String>,
}

/// Result of saving a mock-server export to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockExportSaveResult {
    /// Whether a file was written.
    pub saved: bool,
    /// Destination path, for the toast.
    pub path: Option<String>,
    /// Servers in the file (0 when canceled).
    pub server_count: usize,
    /// Non-fatal notes, such as how many literal credentials were left empty.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub warnings: Vec<String>,
}

/// Result of importing a Mockoon environment file.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct MockoonImportResultDto {
    /// The saved mock server.
    pub server: MockServer,
    /// Non-fatal import notes.
    pub warnings: Vec<String>,
}

/// Result of saving a response body to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct ResponseSaveResult {
    /// Whether the user picked a path and bytes were written.
    pub saved: bool,
    /// Destination path, for the toast.
    pub path: Option<String>,
}

/// Result of saving an exported Postman collection to disk.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct PostmanExportSaveResult {
    /// Whether a file was written.
    pub saved: bool,
    /// Destination path, for the toast.
    pub path: Option<String>,
    /// What the export left out or could not resolve, one line each (empty when nothing).
    #[serde(default)]
    pub warnings: Vec<String>,
}

/// An exported collection and what the user should know about it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PostmanExportDto {
    /// Postman v2.1 JSON.
    pub json: String,
    /// What the export left out or could not resolve, one line each ("N attachments are not
    /// exported"); empty when nothing.
    pub warnings: Vec<String>,
}

/// Input of `run_collection`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RunCollectionInput {
    /// Workspace that owns the collection and environment.
    pub workspace_id: String,
    /// Collection to run.
    pub collection_id: String,
    /// Environment whose variables apply.
    pub environment_id: Option<String>,
    /// Iteration count; defaults to the data row count.
    #[serde(default)]
    pub iterations: Option<u32>,
    /// Delay between requests, in milliseconds.
    #[serde(default)]
    pub delay_ms: Option<u64>,
    /// Stop the run after the first failed request.
    #[serde(default)]
    pub stop_on_error: Option<bool>,
    /// Data rows, one per iteration.
    #[serde(default)]
    pub data: Option<Vec<BTreeMap<String, String>>>,
    /// The user confirmed running against a guarded environment; without it a guarded
    /// run fails with `AppError::ConfirmationRequired` before anything is sent.
    #[serde(default)]
    pub confirmed: Option<bool>,
    /// Run only this folder (and its sub-folders) of the collection; unknown ids are `NotFound`.
    #[serde(default)]
    pub folder_id: Option<String>,
    /// Run only these requests, always in collection order (not the order given). Every id must
    /// belong to the collection (and to `folder_id` when set) or the run is refused; an empty
    /// list is a validation error.
    #[serde(default)]
    pub request_ids: Option<Vec<String>>,
}

/// Result of one collection run.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct RunnerResult {
    /// Run id: the `run_id` of the history rows the run wrote and of its stored summary.
    pub run_id: String,
    /// Collection that ran.
    pub collection_id: String,
    /// Iterations requested.
    pub iterations: u32,
    /// Requests executed.
    pub total: usize,
    /// Requests that passed every check.
    pub passed: usize,
    /// Requests that failed.
    pub failed: usize,
    /// Whether the run stopped early (`stop_on_error`, `postman.setNextRequest(null)`
    /// or the step ceiling).
    pub stopped: bool,
    /// Per-request outcomes in execution order.
    pub items: Vec<RunnerItem>,
}

/// How a caller supplied response bytes to save.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum ResponseBodyEncoding {
    /// The body is already UTF-8 text.
    Text,
    /// The body is Base64 of the captured bytes.
    Base64,
}
