//! Environment use-cases: redacted wire DTOs, save, duplicate, delete with undo, compare.
//!
//! Single responsibility: everything the Environments module needs besides the Postman import
//! and export (see `import_export_service`) and the unresolved-variable scan
//! (see `unresolved_scan`). Secret values never leave the core in a DTO: a secret
//! variable is sent as `value: None` plus `has_secret`, and an editor row that sends no value
//! keeps the stored one.

use std::collections::{HashMap, HashSet};
use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::crypto::constant_time_eq;
use crate::error::AppError;
use crate::llm::events::LlmEvent;
use crate::secrets::mask_secret_hint;
use crate::storage::{collection_revision, environment_revision};
use crate::workspace::{
    AuthConfig, Collection, Environment, EnvironmentId, EnvironmentVariable, StorageError,
    VariableResolutionSource, VariableScopes, WorkspaceId, WorkspaceStore,
};

use super::factory_service::new_entity_id;
use super::secret_hosts::normalize_declared_hosts;
use super::undo_store::{UndoHeld, UndoStore};
use super::{
    AppState, EnvironmentVariableInput, SaveEnvironmentInput, command_error, deleted_or_not_found,
    ensure_workspace_exists,
};
use crate::error::ErrorCode;

/// One variable as the webview sees it: a secret's value is never included.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentVariableDto {
    /// Stable identity of the variable: the editor sends it back so a renamed secret keeps its
    /// value. A variable no save has named yet gets `<owner id>:<key>`.
    pub id: String,
    /// Variable name.
    pub key: String,
    /// The value of a plain variable; `None` for a secret.
    pub value: Option<String>,
    /// Whether the variable is a secret.
    pub is_secret: bool,
    /// Whether a non-empty secret value is stored.
    pub has_secret: bool,
    /// Masked tail of a stored secret (`mask_secret_hint`: `••••` below 12 characters, `…9f2a`
    /// below 24, `sk-…9f2a` above); absent for a plain variable or an empty secret.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub secret_hint: Option<String>,
    /// Whether the variable participates in resolution.
    pub enabled: bool,
    /// Free-text note.
    pub note: String,
}

/// The identity a variable answers to: its own id, or `<owner id>:<key>` while no save has
/// given it one. `owner` is the environment, collection or (for globals) workspace id.
#[must_use]
pub(crate) fn effective_variable_id(owner: &str, variable: &EnvironmentVariable) -> String {
    if variable.id.is_empty() {
        format!("{owner}:{}", variable.key)
    } else {
        variable.id.clone()
    }
}

fn variable_dto(owner: &str, variable: &EnvironmentVariable) -> EnvironmentVariableDto {
    let stored = variable
        .value
        .as_deref()
        .filter(|value| variable.is_secret && !value.is_empty());
    EnvironmentVariableDto {
        id: effective_variable_id(owner, variable),
        key: variable.key.clone(),
        value: if variable.is_secret {
            None
        } else {
            variable.value.clone()
        },
        is_secret: variable.is_secret,
        has_secret: stored.is_some(),
        secret_hint: stored.map(mask_secret_hint),
        enabled: variable.enabled,
        note: variable.note.clone(),
    }
}

/// Redacts the variables of `owner` (an environment, collection or workspace id) for the
/// webview.
#[must_use]
pub(crate) fn redact_variables(
    owner: &str,
    variables: &[EnvironmentVariable],
) -> Vec<EnvironmentVariableDto> {
    variables
        .iter()
        .map(|variable| variable_dto(owner, variable))
        .collect()
}

/// An environment as the webview sees it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EnvironmentDto {
    /// Environment id.
    pub id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Display name, unique in the workspace (case-insensitively).
    pub name: String,
    /// Variables in editor order, secrets redacted.
    pub variables: Vec<EnvironmentVariableDto>,
    /// Production guard.
    pub confirm_before_send: bool,
    /// Whether the guard also covers GET, HEAD and OPTIONS.
    pub confirm_safe_methods: bool,
    /// Hosts the user added by hand; see `environment_declared_hosts_impl` for the
    /// whole declared list.
    pub declared_hosts: Vec<String>,
    /// Opaque version of the saved environment: send it back as `base_revision` so a save
    /// made from a stale copy is refused instead of overwriting another writer.
    pub revision: String,
    /// When a guard flag last changed, in Unix milliseconds; absent before the first change.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub guard_changed_at: Option<i64>,
}

impl From<&Environment> for EnvironmentDto {
    fn from(environment: &Environment) -> Self {
        Self {
            id: environment.id.as_str().to_owned(),
            workspace_id: environment.workspace_id.as_str().to_owned(),
            name: environment.name.clone(),
            variables: redact_variables(environment.id.as_str(), &environment.variables),
            confirm_before_send: environment.confirm_before_send,
            confirm_safe_methods: environment.confirm_safe_methods,
            declared_hosts: environment.declared_hosts.clone(),
            revision: environment_revision(environment),
            guard_changed_at: environment.guard_changed_at,
        }
    }
}

/// The workspace globals as the webview sees them (`globals_get`, `globals_save`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct GlobalsDto {
    /// Variables in editor order, secrets redacted.
    pub variables: Vec<EnvironmentVariableDto>,
    /// Opaque version of the saved globals; send it back as `base_revision`.
    pub revision: String,
}

/// A collection as the webview sees it: the stored shape with redacted variables.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CollectionDto {
    /// Collection id.
    pub id: String,
    /// Owning workspace.
    pub workspace_id: String,
    /// Display name.
    pub name: String,
    /// Markdown description.
    pub description: String,
    /// Collection-level auth.
    pub auth: AuthConfig,
    /// Collection variables, secrets redacted.
    pub variables: Vec<EnvironmentVariableDto>,
    /// Script run before every request in the collection.
    pub pre_request_script: String,
    /// Script run after every response in the collection.
    pub test_script: String,
    /// Author-defined order.
    pub position: i64,
    /// Opaque version of the saved collection: pass it back as the save's base revision so a
    /// save over a collection changed since (a script, sync, another editor) is refused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub revision: Option<String>,
}

impl From<&Collection> for CollectionDto {
    fn from(collection: &Collection) -> Self {
        Self {
            id: collection.id.as_str().to_owned(),
            workspace_id: collection.workspace_id.as_str().to_owned(),
            name: collection.name.clone(),
            description: collection.description.clone(),
            auth: collection.auth.clone(),
            variables: redact_variables(collection.id.as_str(), &collection.variables),
            pre_request_script: collection.pre_request_script.clone(),
            test_script: collection.test_script.clone(),
            position: collection.position,
            revision: Some(collection_revision(collection)),
        }
    }
}

pub use super::undo_store::UndoToken;

/// How one key compares between two environments.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CompareState {
    /// Both sides resolve the key to the same value.
    Same,
    /// Both sides define the key with different values.
    Differs,
    /// Only the left environment defines the key.
    OnlyLeft,
    /// Only the right environment defines the key.
    OnlyRight,
}

/// One key of an environment comparison; a secret side never carries its text.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CompareRowDto {
    /// Variable name.
    pub key: String,
    /// How the two sides relate.
    pub state: CompareState,
    /// The left value; `None` when absent or secret.
    pub left: Option<String>,
    /// The right value; `None` when absent or secret.
    pub right: Option<String>,
    /// Whether the left variable is a secret.
    pub left_secret: bool,
    /// Whether the right variable is a secret.
    pub right_secret: bool,
    /// The left value comes from the workspace globals (the environment does not define it).
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub left_global: bool,
    /// The right value comes from the workspace globals.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub right_global: bool,
}

/// Deleted environments, secret values included, held for the Undo toast.
pub(crate) type EnvironmentUndoStore = UndoStore<Environment>;

impl UndoHeld for Environment {
    fn workspace_id(&self) -> &WorkspaceId {
        &self.workspace_id
    }
}

/// Whether `error` is SQLite refusing a duplicate row (the unique name index on a race).
fn is_unique_violation(error: &StorageError) -> bool {
    error.is_constraint_violation()
}

fn name_clash(name: &str) -> AppError {
    AppError::validation(format!("An environment named {name} already exists"))
}

/// Saves `environment`, mapping a name clash (found up front or raised by the unique index)
/// to the validation message the field shows.
pub(crate) fn save_environment_unique(
    store: &WorkspaceStore,
    environment: &Environment,
) -> Result<(), AppError> {
    let taken = |store: &WorkspaceStore| {
        store.environment_name_taken(
            &environment.workspace_id,
            &environment.name,
            &environment.id,
        )
    };
    if taken(store).map_err(command_error)? {
        return Err(name_clash(&environment.name));
    }
    match store.save_environment(environment) {
        Err(error) if is_unique_violation(&error) && taken(store).unwrap_or(false) => {
            Err(name_clash(&environment.name))
        }
        other => other.map_err(command_error),
    }
}

/// The first of `candidates` no other environment of the workspace uses.
pub fn first_free_environment_name(
    store: &WorkspaceStore,
    workspace_id: &WorkspaceId,
    own_id: &EnvironmentId,
    candidates: impl IntoIterator<Item = String>,
) -> Result<String, AppError> {
    for candidate in candidates {
        if !store
            .environment_name_taken(workspace_id, &candidate, own_id)
            .map_err(command_error)?
        {
            return Ok(candidate);
        }
    }
    Err(AppError::internal("exhausted environment name suffixes"))
}

/// Validates and saves an environment with its variables.
///
/// A secret row without a value keeps the stored value of the variable it names by id (so a
/// renamed key keeps it) or, with no id, of the same key; `Some("")` clears it, `Some(text)`
/// replaces it. `None` flags and `None` declared hosts keep the stored ones. Names are unique
/// within a workspace, case-insensitively; variable keys are unique within it.
///
/// With `base_revision`, the save is refused with `Conflict` (`stale_write`) when the stored
/// environment is no longer the version the caller read (a script, OAuth or sync wrote it
/// meanwhile); without it the save overwrites as before.
pub fn environment_save_impl(
    state: &AppState,
    input: SaveEnvironmentInput,
) -> Result<EnvironmentDto, AppError> {
    let mut environment =
        Environment::new(&input.id, &input.workspace_id, &input.name).map_err(command_error)?;
    let store = state.store();
    ensure_workspace_exists(&store, environment.workspace_id.as_str())?;
    let stored = store
        .get_environment(&environment.workspace_id, &environment.id)
        .map_err(command_error)?;
    if let Some(base) = input.base_revision.as_deref() {
        let current = stored.as_ref().map(environment_revision);
        ensure_revision(current.as_deref(), base, "environment")?;
    }
    let stored_variables = stored.as_ref().map_or(&[][..], |stored| &stored.variables);
    environment.variables =
        variables_from_rows(input.variables, environment.id.as_str(), stored_variables)?;
    environment.declared_hosts = match input.declared_hosts {
        Some(hosts) => normalize_declared_hosts(hosts)?,
        None => stored
            .as_ref()
            .map_or_else(Vec::new, |stored| stored.declared_hosts.clone()),
    };
    environment.confirm_before_send = input.confirm_before_send.unwrap_or_else(|| {
        stored
            .as_ref()
            .is_some_and(|stored| stored.confirm_before_send)
    });
    environment.confirm_safe_methods = input.confirm_safe_methods.unwrap_or_else(|| {
        stored
            .as_ref()
            .is_some_and(|stored| stored.confirm_safe_methods)
    });
    save_environment_unique(&store, &environment)?;
    // The store stamps `guard_changed_at` when a flag changed; read the row back for it.
    environment.guard_changed_at = store
        .get_environment(&environment.workspace_id, &environment.id)
        .map_err(command_error)?
        .and_then(|saved| saved.guard_changed_at);
    drop(store);
    state.sync.notify_changed();
    notify_environments_changed(
        state,
        environment.workspace_id.as_str(),
        vec![environment.id.as_str().to_owned()],
        "ui",
    );
    Ok(EnvironmentDto::from(&environment))
}

/// Refuses a save whose `base` is not the `current` revision of the stored owner (`None` when
/// it no longer exists).
pub(crate) fn ensure_revision(
    current: Option<&str>,
    base: &str,
    owner: &str,
) -> Result<(), AppError> {
    if current == Some(base) {
        return Ok(());
    }
    Err(
        AppError::conflict(format!("the {owner} changed since it was loaded"))
            .coded(ErrorCode::StaleWrite, None),
    )
}

/// Converts editor rows into domain variables, refusing a key used twice; errors come in row
/// order, matching the first issue `validate_environment` / `validate_globals` list.
pub(crate) fn variables_from_rows(
    rows: Vec<EnvironmentVariableInput>,
    owner: &str,
    stored: &[EnvironmentVariable],
) -> Result<Vec<EnvironmentVariable>, AppError> {
    let mut variables: Vec<EnvironmentVariable> = Vec::with_capacity(rows.len());
    for row in rows {
        let variable = row.into_domain(owner, stored)?;
        if variables.iter().any(|seen| seen.key == variable.key) {
            return Err(AppError::validation(duplicate_key_message(&variable.key)));
        }
        variables.push(variable);
    }
    Ok(variables)
}

/// The message of a key used by two rows of one list.
pub(crate) fn duplicate_key_message(key: &str) -> String {
    format!("variable {key} is defined twice")
}

/// Lists the environments of a workspace, secrets redacted.
pub fn environment_list_impl(
    state: &AppState,
    workspace_id: String,
) -> Result<Vec<EnvironmentDto>, AppError> {
    let store = state.store();
    let environments = store
        .list_environments(&workspace_id)
        .map_err(command_error)?;
    Ok(environments.iter().map(EnvironmentDto::from).collect())
}

fn load_environment(
    store: &WorkspaceStore,
    workspace_id: &WorkspaceId,
    id: &EnvironmentId,
) -> Result<Environment, AppError> {
    store
        .get_environment(workspace_id, id)
        .map_err(command_error)?
        .ok_or_else(|| {
            AppError::not_found(format!(
                "environment {} not found in workspace {}",
                id.as_str(),
                workspace_id.as_str()
            ))
        })
}

/// Hands the stored value of one secret variable to `write` (the desktop shell puts it on the
/// system clipboard) and never returns it, so the webview does not receive the text.
/// The variable is looked up in the workspace's environment `environment_id`, or in the
/// workspace globals when that is `None`. Ids from the webview are not authority: a variable of
/// another workspace or environment, a plain variable and a secret with no stored value are all
/// `NotFound`. The store lock is released before `write` runs.
///
/// # Errors
/// `NotFound` as above; whatever `write` returns.
pub fn environment_secret_copy_impl(
    state: &AppState,
    workspace_id: &str,
    environment_id: Option<&str>,
    variable_id: &str,
    write: impl FnOnce(&str) -> Result<(), AppError>,
) -> Result<(), AppError> {
    let workspace = WorkspaceId::new(workspace_id).map_err(command_error)?;
    let value = {
        let store = state.store();
        let (owner, variables) = match environment_id {
            Some(id) => {
                let id = EnvironmentId::new(id).map_err(command_error)?;
                let environment = load_environment(&store, &workspace, &id)?;
                (id.as_str().to_owned(), environment.variables)
            }
            None => (
                workspace.as_str().to_owned(),
                store
                    .list_global_variables(workspace.as_str())
                    .map_err(command_error)?,
            ),
        };
        variables
            .into_iter()
            .find(|v| v.is_secret && effective_variable_id(&owner, v) == variable_id)
            .and_then(|v| v.value)
            .filter(|value| !value.is_empty())
    };
    let value = value.ok_or_else(|| AppError::not_found("no stored secret with this id"))?;
    write(&value)
}

/// Copies an environment with its variables (fresh variable ids), secret values, flags and
/// declared hosts under a fresh id, named
/// `"<name> copy"` (`copy 2`, … while taken).
pub fn environment_duplicate_impl(
    state: &AppState,
    workspace_id: String,
    environment_id: String,
) -> Result<EnvironmentDto, AppError> {
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let source_id = EnvironmentId::new(&environment_id).map_err(command_error)?;
    let store = state.store();
    let mut copy = load_environment(&store, &workspace, &source_id)?;
    copy.id = EnvironmentId::new(new_entity_id("env")).map_err(command_error)?;
    let base = format!("{} copy", copy.name);
    copy.name = first_free_environment_name(
        &store,
        &workspace,
        &copy.id,
        std::iter::once(base.clone()).chain((2_u32..).map(|n| format!("{base} {n}"))),
    )?;
    copy.guard_changed_at = None;
    for variable in &mut copy.variables {
        variable.id = new_entity_id("var");
    }
    save_environment_unique(&store, &copy)?;
    state.sync.notify_changed();
    Ok(EnvironmentDto::from(&copy))
}

/// Deletes a workspace's environment and its secrets, holding the removed environment in
/// memory so [`environment_restore_impl`] can undo it until the toast is dismissed
/// ([`environment_undo_commit_impl`]) plus 1 s, and at most 60 s. An unknown id, or one owned by
/// another workspace, is `NotFound` and leaves every row untouched.
pub fn environment_delete_impl(
    state: &AppState,
    workspace_id: String,
    id: String,
) -> Result<UndoToken, AppError> {
    let token = environment_delete_at(state, &workspace_id, &id, Instant::now())?;
    state.env_undo.arm_timer();
    Ok(token)
}

/// The Undo toast of `token` was dismissed: the held environment stays restorable for 1 s more
/// (never past 60 s after the delete) and is then dropped with its secret values. Returns
/// `false` for an unknown or expired token.
pub fn environment_undo_commit_impl(state: &AppState, token: UndoToken) -> bool {
    let committed = state.env_undo.commit_at(Instant::now(), &token.0);
    if committed {
        state.env_undo.arm_timer();
    }
    committed
}

pub(crate) fn environment_delete_at(
    state: &AppState,
    workspace_id: &str,
    id: &str,
    now: Instant,
) -> Result<UndoToken, AppError> {
    let environment_id = EnvironmentId::new(id).map_err(command_error)?;
    let workspace = WorkspaceId::new(workspace_id).map_err(command_error)?;
    state.env_undo.purge_at(now);
    let store = state.store();
    let environment = store
        .get_environment(&workspace, &environment_id)
        .map_err(command_error)?;
    let deleted = store
        .delete_environment(&workspace, &environment_id)
        .map_err(command_error)?;
    drop(store);
    state.sync.notify_changed();
    deleted_or_not_found(deleted, "environment", id, workspace_id)?;
    let environment = environment
        .ok_or_else(|| AppError::internal("a deleted environment vanished before it was read"))?;
    Ok(state.env_undo.insert_at(now, environment))
}

/// Deletes an environment and its secrets for good, without the 7 s undo hold: used where the
/// removal comes from a sync import, so no secret value is kept in memory for it.
pub fn environment_delete_discarding_secrets(
    state: &AppState,
    workspace_id: &str,
    id: &str,
) -> Result<(), AppError> {
    let environment_id = EnvironmentId::new(id).map_err(command_error)?;
    let workspace = WorkspaceId::new(workspace_id).map_err(command_error)?;
    let deleted = state
        .store()
        .delete_environment(&workspace, &environment_id)
        .map_err(command_error)?;
    state.sync.notify_changed();
    deleted_or_not_found(deleted, "environment", id, workspace_id)
}

/// Re-creates the environment a [`environment_delete_impl`] token names, with its id,
/// variables, secrets and flags. An expired, unknown or foreign-workspace token is `NotFound`;
/// a name or id taken meanwhile is `Conflict` and keeps the token usable until it expires.
pub fn environment_restore_impl(
    state: &AppState,
    workspace_id: String,
    token: UndoToken,
) -> Result<EnvironmentDto, AppError> {
    environment_restore_at(state, &workspace_id, &token, Instant::now())
}

pub(crate) fn environment_restore_at(
    state: &AppState,
    workspace_id: &str,
    token: &UndoToken,
    now: Instant,
) -> Result<EnvironmentDto, AppError> {
    let workspace = WorkspaceId::new(workspace_id).map_err(command_error)?;
    let environment = state
        .env_undo
        .peek_at(now, &workspace, &token.0)
        .ok_or_else(|| AppError::not_found("the deleted environment can no longer be restored"))?;
    let store = state.store();
    ensure_workspace_exists(&store, workspace.as_str())?;
    if store
        .environment_id_occupied(&environment.id)
        .map_err(command_error)?
    {
        return Err(AppError::conflict(format!(
            "environment {} already exists",
            environment.id.as_str()
        )));
    }
    save_environment_unique(&store, &environment).map_err(|error| match error {
        AppError::Validation { message, .. } => AppError::conflict(message),
        other => other,
    })?;
    drop(store);
    state.env_undo.discard(&token.0);
    state.sync.notify_changed();
    Ok(EnvironmentDto::from(&environment))
}

/// Compares the enabled variables of two environments by key.
///
/// A disabled variable is not resolved, so it counts as absent. With `include_globals` a key a
/// side lacks falls back to the enabled global of that name, as a send would resolve it, and
/// the row says so (`left_global`, `right_global`). Rows follow the left environment's order,
/// then the keys only the right one has, then the keys only the globals have. Two plain values
/// compare as text. When a side is a secret the core compares in-core (constant time) and that
/// side's text is never returned; a secret against a plain value always `Differs`.
pub fn environment_compare_impl(
    state: &AppState,
    workspace_id: String,
    env_a: String,
    env_b: String,
    include_globals: Option<bool>,
) -> Result<Vec<CompareRowDto>, AppError> {
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let left_id = EnvironmentId::new(&env_a).map_err(command_error)?;
    let right_id = EnvironmentId::new(&env_b).map_err(command_error)?;
    let (left, right, globals) = {
        let store = state.store();
        let globals = if include_globals.unwrap_or(false) {
            store
                .list_global_variables(workspace.as_str())
                .map_err(command_error)?
        } else {
            Vec::new()
        };
        (
            load_environment(&store, &workspace, &left_id)?.variables,
            load_environment(&store, &workspace, &right_id)?.variables,
            globals,
        )
    };
    fn enabled(variables: &[EnvironmentVariable]) -> HashMap<&str, &EnvironmentVariable> {
        variables
            .iter()
            .filter(|variable| variable.enabled)
            .map(|variable| (variable.key.as_str(), variable))
            .collect()
    }
    /// A side's variable for `key`, and whether it came from the globals.
    fn pick<'a>(
        own: &HashMap<&str, &'a EnvironmentVariable>,
        globals: &HashMap<&str, &'a EnvironmentVariable>,
        key: &str,
    ) -> Option<(&'a EnvironmentVariable, bool)> {
        own.get(key)
            .map(|variable| (*variable, false))
            .or_else(|| globals.get(key).map(|variable| (*variable, true)))
    }
    let (left_map, right_map, global_map) = (enabled(&left), enabled(&right), enabled(&globals));
    let text = |variable: Option<&EnvironmentVariable>| {
        variable
            .filter(|variable| !variable.is_secret)
            .and_then(|variable| variable.value.clone())
    };
    let value = |variable: &EnvironmentVariable| {
        variable
            .value
            .as_deref()
            .unwrap_or_default()
            .as_bytes()
            .to_vec()
    };
    let row = |key: &str| {
        let (l, r) = (
            pick(&left_map, &global_map, key),
            pick(&right_map, &global_map, key),
        );
        let state = match (l.map(|(v, _)| v), r.map(|(v, _)| v)) {
            (Some(_), None) => CompareState::OnlyLeft,
            (None, Some(_)) => CompareState::OnlyRight,
            (Some(a), Some(b)) if a.is_secret != b.is_secret => CompareState::Differs,
            (Some(a), Some(b)) if a.is_secret => {
                if constant_time_eq(&value(a), &value(b)) {
                    CompareState::Same
                } else {
                    CompareState::Differs
                }
            }
            (Some(a), Some(b)) if a.value == b.value => CompareState::Same,
            _ => CompareState::Differs,
        };
        CompareRowDto {
            key: key.to_owned(),
            state,
            left: text(l.map(|(v, _)| v)),
            right: text(r.map(|(v, _)| v)),
            left_secret: l.is_some_and(|(v, _)| v.is_secret),
            right_secret: r.is_some_and(|(v, _)| v.is_secret),
            left_global: l.is_some_and(|(_, global)| global),
            right_global: r.is_some_and(|(_, global)| global),
        }
    };

    // Each key once, in order: the left's rows, then the right's, then the globals'.
    let mut seen = HashSet::new();
    Ok(left
        .iter()
        .chain(&right)
        .chain(&globals)
        .filter(|variable| variable.enabled && seen.insert(variable.key.as_str()))
        .map(|variable| row(&variable.key))
        .collect())
}

/// Tells open pages that environments (or, with the workspace id in `ids`, the globals) of
/// `workspace_id` were written; `source` is `ui`, `oauth` or `sync`.
pub fn notify_environments_changed(
    state: &AppState,
    workspace_id: &str,
    ids: Vec<String>,
    source: &str,
) {
    state.llm.emit(LlmEvent::EnvironmentsChanged {
        workspace_id: workspace_id.to_owned(),
        ids,
        source: source.to_owned(),
    });
}

/// Where `environment_variable_save` writes the variable.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum VariableScope {
    /// An environment (`owner_id`), or a new one when `owner_id` is missing.
    Environment,
    /// A collection (`owner_id`).
    Collection,
    /// The workspace globals.
    Global,
}

/// Input of `environment_variable_save`: define one variable in place (the Define variable
/// dialog, OAuth "Use"), without the editor's snapshot of the other rows.
#[derive(Debug, Clone, Serialize, Deserialize)]
pub struct DefineVariableInput {
    /// Owning workspace.
    pub workspace_id: String,
    /// Which layer gets the variable.
    pub scope: VariableScope,
    /// The environment or collection. An `environment` scope without it creates an
    /// environment named `Local` (or the first free `Local N`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub owner_id: Option<String>,
    /// Variable name.
    pub key: String,
    /// The value; a secret's value is stored in the vault and never returned.
    pub value: String,
    /// Store the value as a secret.
    pub is_secret: bool,
}

/// What `environment_variable_save` wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DefineVariableResult {
    /// The layer written.
    pub scope: VariableScope,
    /// The environment or collection written; the workspace id for the globals.
    pub owner_id: String,
    /// Whether the environment was created for this definition.
    pub created: bool,
}

/// Sets `key` in `variables`: an existing row keeps its id and note and becomes enabled with
/// the new value; it becomes a secret when asked, and an existing secret stays one (turning a
/// secret plain is an explicit edit in the editor, never a side effect of defining it); a
/// missing one is appended with a fresh id.
fn upsert_variable(
    variables: &mut Vec<EnvironmentVariable>,
    key: &str,
    value: String,
    is_secret: bool,
) -> Result<(), AppError> {
    match variables.iter_mut().find(|existing| existing.key == key) {
        Some(existing) => {
            existing.value = Some(value);
            existing.is_secret |= is_secret;
            existing.enabled = true;
        }
        None => {
            let mut variable = if is_secret {
                EnvironmentVariable::masked(key, value)
            } else {
                EnvironmentVariable::plain(key, value)
            }
            .map_err(command_error)?;
            variable.id = new_entity_id("var");
            variables.push(variable);
        }
    }
    Ok(())
}

/// Defines one variable: reads the owner, sets the variable and saves it while holding
/// the store, so a concurrent editor save is refused by its `base_revision` instead of losing
/// this write, and the other rows are written back exactly as stored. A secret's value is then
/// rewritten to `{{key}}` in the workspace's history (best effort: a failure there is logged and
/// the define still succeeds). `source` names the writer in the
/// `EnvironmentsChanged` event.
pub fn environment_variable_define(
    state: &AppState,
    input: DefineVariableInput,
    source: &str,
) -> Result<DefineVariableResult, AppError> {
    let key = input.key.trim().to_owned();
    if key.is_empty() {
        return Err(AppError::validation("Name the variable"));
    }
    let workspace = WorkspaceId::new(&input.workspace_id).map_err(command_error)?;
    // A value defined as a secret ("Store as secret…") must stop living in plaintext in the
    // history rows recorded before (see `scrub_history_value`).
    let scrub = input.is_secret.then(|| input.value.clone());
    let store = state.store();
    ensure_workspace_exists(&store, workspace.as_str())?;
    let (owner_id, created) = match (input.scope, input.owner_id.as_deref()) {
        (VariableScope::Environment, owner) => {
            let (mut environment, created) = match owner {
                Some(id) => {
                    let id = EnvironmentId::new(id).map_err(command_error)?;
                    (load_environment(&store, &workspace, &id)?, false)
                }
                None => {
                    let id = EnvironmentId::new(new_entity_id("env")).map_err(command_error)?;
                    let candidates = std::iter::once("Local".to_owned())
                        .chain((2..100).map(|n| format!("Local {n}")));
                    let name = first_free_environment_name(&store, &workspace, &id, candidates)?;
                    let environment = Environment::new(id.as_str(), workspace.as_str(), &name)
                        .map_err(command_error)?;
                    (environment, true)
                }
            };
            upsert_variable(
                &mut environment.variables,
                &key,
                input.value,
                input.is_secret,
            )?;
            save_environment_unique(&store, &environment)?;
            (environment.id.as_str().to_owned(), created)
        }
        (VariableScope::Collection, Some(id)) => {
            let id = crate::workspace::CollectionId::new(id).map_err(command_error)?;
            let mut collection = store
                .get_collection(&workspace, &id)
                .map_err(command_error)?
                .ok_or_else(|| {
                    AppError::not_found(format!("collection {} not found", id.as_str()))
                })?;
            upsert_variable(
                &mut collection.variables,
                &key,
                input.value,
                input.is_secret,
            )?;
            store.save_collection(&collection).map_err(command_error)?;
            (id.as_str().to_owned(), false)
        }
        (VariableScope::Collection, None) => {
            return Err(AppError::validation(
                "Choose the collection to define the variable in",
            ));
        }
        (VariableScope::Global, _) => {
            let mut globals = store
                .list_global_variables(workspace.as_str())
                .map_err(command_error)?;
            upsert_variable(&mut globals, &key, input.value, input.is_secret)?;
            store
                .save_global_variables(workspace.as_str(), &globals)
                .map_err(command_error)?;
            (workspace.as_str().to_owned(), false)
        }
    };
    // The variable is committed: a failed scrub is logged, not returned, so the caller still
    // swaps the literal for `{{key}}` and the environments UI still hears of the variable.
    if let Some(value) = scrub
        && let Err(error) =
            store.scrub_history_value(workspace.as_str(), &value, &format!("{{{{{key}}}}}"))
    {
        crate::logging::log_line(
            crate::logging::LogLevel::Warn,
            "environment.secret",
            &format!("could not rewrite the new secret {key} in the history: {error}"),
        );
    }
    drop(store);
    state.sync.notify_changed();
    if input.scope != VariableScope::Collection {
        notify_environments_changed(state, workspace.as_str(), vec![owner_id.clone()], source);
    }
    Ok(DefineVariableResult {
        scope: input.scope,
        owner_id,
        created,
    })
}

/// `environment_variable_save`: [`environment_variable_define`] for the webview.
pub fn environment_variable_save_impl(
    state: &AppState,
    input: DefineVariableInput,
) -> Result<DefineVariableResult, AppError> {
    environment_variable_define(state, input, "ui")
}

/// One layer that holds a name, as the Resolved tab lists it under the winner.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedLayer {
    /// `Environment`, `Collection` or `Global`.
    pub source: VariableResolutionSource,
    /// The plain value; `None` for a secret.
    pub value: Option<String>,
    /// Whether the row is a secret.
    pub is_secret: bool,
    /// Whether the row has a value (an empty secret has none and does not resolve).
    pub has_value: bool,
    /// Whether the row takes part in resolution.
    pub enabled: bool,
}

/// One name of the Resolved tab: the layer that wins at send time and every layer holding it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ResolvedVariable {
    /// Variable name.
    pub name: String,
    /// The winning layer, or `Unresolved` when no enabled row with a value has the name.
    pub source: VariableResolutionSource,
    /// The winner's plain value; `None` for a secret or an unresolved name.
    pub value: Option<String>,
    /// Whether the winner is a secret.
    pub is_secret: bool,
    /// Every layer holding the name, narrowest first; the winner is one of them.
    pub layers: Vec<ResolvedLayer>,
}

/// Lists the names the environment, the collection and the globals define, each with the layer
/// that wins: the order is the core's (`VariableScopes::resolve_trace`), so the tab cannot drift
/// from what a send does. Runtime layers (script locals, runner data) exist only during a run
/// and are not listed. Secret values never leave the core.
pub fn environment_resolved_list_impl(
    state: &AppState,
    workspace_id: String,
    environment_id: Option<String>,
    collection_id: Option<String>,
) -> Result<Vec<ResolvedVariable>, AppError> {
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let (environment, collection, globals) = {
        let store = state.store();
        let environment = match environment_id.as_deref() {
            Some(id) => {
                let id = EnvironmentId::new(id).map_err(command_error)?;
                load_environment(&store, &workspace, &id)?.variables
            }
            None => Vec::new(),
        };
        let collection = match collection_id.as_deref() {
            Some(id) => {
                let id = crate::workspace::CollectionId::new(id).map_err(command_error)?;
                store
                    .get_collection(&workspace, &id)
                    .map_err(command_error)?
                    .map(|collection| collection.variables)
                    .unwrap_or_default()
            }
            None => Vec::new(),
        };
        let globals = store
            .list_global_variables(workspace.as_str())
            .map_err(command_error)?;
        (environment, collection, globals)
    };
    let layers = [
        (VariableResolutionSource::Environment, &environment),
        (VariableResolutionSource::Collection, &collection),
        (VariableResolutionSource::Global, &globals),
    ];
    let mut names: Vec<&str> = Vec::new();
    for (_, variables) in layers {
        for variable in variables.iter() {
            if !names.contains(&variable.key.as_str()) {
                names.push(&variable.key);
            }
        }
    }
    let scopes = VariableScopes::new()
        .with_globals(globals.clone())
        .with_collection_vars(collection.clone())
        .with_environment_vars(environment.clone());
    Ok(names
        .into_iter()
        .map(|name| {
            let (value, source, is_secret) = scopes.resolve_trace(name);
            let layers = layers
                .iter()
                .flat_map(|(source, variables)| {
                    variables
                        .iter()
                        .filter(|variable| variable.key == name)
                        .map(|variable| ResolvedLayer {
                            source: *source,
                            value: variable.value.clone().filter(|_| !variable.is_secret),
                            is_secret: variable.is_secret,
                            has_value: variable.value.as_deref().is_some_and(|v| !v.is_empty()),
                            enabled: variable.enabled,
                        })
                })
                .collect();
            ResolvedVariable {
                name: name.to_owned(),
                source,
                value: value.filter(|_| !is_secret),
                is_secret,
                layers,
            }
        })
        .collect())
}
