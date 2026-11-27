//! Mock server use-cases.
//!
//! Single responsibility: CRUD, lifecycle, logs, import and export of a workspace's mock servers,
//! delegating serving to [`crate::mock`].

use std::collections::HashMap;
use std::path::PathBuf;
use std::sync::Arc;

use crate::error::AppError;
use crate::events::{EventSink, MockLogEvent};
use crate::mock::{
    MockDataResetError, MockServerManager, MockSubscribeError, TransferError, export_json,
    file_body_paths, import_mockoon_environment, parse_export, reissue_ids,
    strip_literal_credentials, with_environment,
};
use crate::platform::{normalize_display_path, write_private_file};
use crate::workspace::{
    EnvironmentId, MockRoute, MockServer, MockTransactionLog, WorkspaceId, WorkspaceStore,
};

use super::factory_service::{
    first_free_port, mock_route_from_exchange, mock_route_from_log, mock_routes_from_recordings,
    mock_server_new, new_entity_id, port_is_bindable,
};
use super::misc_logic_service::{self, first_issue_error};
use super::{
    AppState, MockExportSaveResult, MockImportResultDto, MockLiveDataDto, MockRecordingRoutesDto,
    MockRecordingStatusDto, MockServerStatusDto, MockoonImportResultDto, command_error,
    current_time_ms, deleted_or_not_found, ensure_workspace_exists,
};

/// Rejects mock servers with blank IDs/names, invalid ports, routes, statuses or latencies
/// (the first of the issues `misc_logic_service::validate_mock_server` lists).
pub fn validate_mock_server(server: &MockServer) -> Result<(), AppError> {
    first_issue_error(misc_logic_service::validate_mock_server(server))
}

/// Reports whether a mock server is running, the port, prefix and URL it is bound to, its
/// counters and when it was last stopped.
#[must_use]
fn mock_server_status(server: &MockServer, manager: &MockServerManager) -> MockServerStatusDto {
    let bound = manager.bound_addr(&server.id);
    let stats = manager.stats(&server.id);
    let (recording, recorded_count) = manager.recording(&server.id).unwrap_or_default();
    let port = bound.map_or(server.port, |bound| bound.port());
    MockServerStatusDto {
        server_id: server.id.clone(),
        running: bound.is_some(),
        port,
        bound_url: format!("http://127.0.0.1:{port}"),
        endpoint_prefix: manager
            .endpoint_prefix(&server.id)
            .unwrap_or_else(|| crate::mock::normalized_prefix(&server.endpoint_prefix)),
        started_at_ms: stats.map(|stats| stats.started_at_ms),
        stopped_at_ms: manager.stopped_at_ms(&server.id),
        hit_count: stats.map_or(0, |stats| stats.hit_count),
        recording,
        recorded_count,
    }
}

/// The configuration a listener serves: `server` with the plain variables of its environment
/// filled into the proxy target, proxy headers and callback URLs and headers (step one of the environment bridge).
/// Secret variables are left as `{{name}}`: their values never leave for a proxy or callback
/// target until the declared-host rules cover the mock (step two). A missing or foreign
/// environment expands nothing.
pub fn mock_runtime_config(state: &AppState, server: &MockServer) -> Result<MockServer, AppError> {
    let Some(environment_id) = server.environment_id.as_deref() else {
        return Ok(server.clone());
    };
    let id = EnvironmentId::new(environment_id).map_err(command_error)?;
    let environment = state
        .store()
        .get_environment(&server.workspace_id, &id)
        .map_err(command_error)?;
    let variables: HashMap<String, String> = environment
        .into_iter()
        .flat_map(|environment| environment.variables)
        .filter(|variable| variable.enabled && !variable.is_secret)
        .filter_map(|variable| Some((variable.key, variable.value?)))
        .collect();
    Ok(with_environment(server.clone(), &variables))
}

/// Loads a mock server of `workspace_id`; one that is unknown or owned by another workspace is
/// `NotFound` (the foreign row is never returned).
fn owned_mock_server(
    state: &AppState,
    workspace_id: &str,
    server_id: &str,
) -> Result<MockServer, AppError> {
    state
        .store()
        .get_mock_server(server_id)
        .map_err(command_error)?
        .filter(|server| server.workspace_id.as_str() == workspace_id)
        .ok_or_else(|| AppError::not_found(format!("mock server {server_id} not found")))
}

/// Returns `base`, or `base__N` with the smallest free suffix, as an unused mock server ID.
fn unique_mock_server_id(store: &WorkspaceStore, base: &str) -> Result<String, AppError> {
    let mut id = base.to_owned();
    let mut suffix = 0_u64;
    while store.get_mock_server(&id).map_err(command_error)?.is_some() {
        suffix = suffix
            .checked_add(1)
            .ok_or_else(|| AppError::internal("exhausted mock server IDs"))?;
        id = format!("{base}__{suffix}");
    }
    Ok(id)
}

/// Lists the mock servers of a workspace.
pub fn mock_server_list_impl(
    state: &AppState,
    workspace_id: String,
) -> Result<Vec<MockServer>, AppError> {
    let store = state.store();
    store
        .list_mock_servers(&workspace_id)
        .map_err(command_error)
}

/// Validates and persists a mock server, refusing port changes while it runs.
pub fn mock_server_save_impl(
    state: &AppState,
    mut server: MockServer,
) -> Result<MockServer, AppError> {
    server.workspace_id = WorkspaceId::new(server.workspace_id.as_str()).map_err(command_error)?;
    validate_mock_server(&server)?;
    if let Some(bound) = state.mocks.bound_addr(&server.id)
        && bound.port() != server.port
    {
        return Err(AppError::conflict(format!(
            "stop the mock server before changing its port (running on {})",
            bound.port()
        )));
    }
    server.updated_at_ms = current_time_ms();

    {
        let store = state.store();
        ensure_workspace_exists(&store, server.workspace_id.as_str())?;
        store.save_mock_server(&server).map_err(command_error)?;
    }
    if state.mocks.is_running(&server.id) {
        state.mocks.update(mock_runtime_config(state, &server)?);
    }
    state.sync.notify_changed();

    Ok(server)
}

/// Deletes a mock server and stops its listener if a row was actually removed.
pub fn mock_server_delete_impl(
    state: &AppState,
    workspace_id: String,
    server_id: String,
) -> Result<(), AppError> {
    let deleted = {
        let store = state.store();
        store
            .delete_mock_server(&server_id, &workspace_id)
            .map_err(command_error)?
    };
    // Only stop a listener whose row was really removed from this workspace.
    if deleted {
        state.mocks.stop(&server_id);
    }

    state.sync.notify_changed();
    deleted_or_not_found(deleted, "mock server", &server_id, &workspace_id)
}

/// Starts the listener of a stored mock server and returns its bound status.
pub fn mock_server_start_impl(
    state: &AppState,
    workspace_id: String,
    server_id: String,
) -> Result<MockServerStatusDto, AppError> {
    let server = {
        let store = state.store();
        let server = store
            .get_mock_server(&server_id)
            .map_err(command_error)?
            .ok_or_else(|| AppError::not_found(format!("mock server {server_id} not found")))?;
        if server.workspace_id.as_str() != workspace_id {
            return Err(AppError::not_found(format!(
                "mock server {server_id} not found"
            )));
        }
        server
    };
    validate_mock_server(&server)?;

    state
        .mocks
        .start(mock_runtime_config(state, &server)?)
        .map_err(command_error)?;

    Ok(mock_server_status(&server, &state.mocks))
}

/// Stops a mock server's listener and returns its stopped status; unknown or foreign servers
/// are `NotFound` and their listener is left running.
pub fn mock_server_stop_impl(
    state: &AppState,
    workspace_id: String,
    server_id: String,
) -> Result<MockServerStatusDto, AppError> {
    let server = owned_mock_server(state, &workspace_id, &server_id)?;
    state.mocks.stop(&server_id);

    Ok(mock_server_status(&server, &state.mocks))
}

/// Returns the running status of every mock server in a workspace.
pub fn mock_server_status_list_impl(
    state: &AppState,
    workspace_id: String,
) -> Result<Vec<MockServerStatusDto>, AppError> {
    let servers = {
        let store = state.store();
        store
            .list_mock_servers(&workspace_id)
            .map_err(command_error)?
    };

    Ok(servers
        .iter()
        .map(|server| mock_server_status(server, &state.mocks))
        .collect())
}

/// Returns the recorded transaction log of a mock server (empty while it is stopped);
/// unknown or foreign servers are `NotFound`.
pub fn mock_server_logs_impl(
    state: &AppState,
    workspace_id: String,
    server_id: String,
) -> Result<Vec<MockTransactionLog>, AppError> {
    owned_mock_server(state, &workspace_id, &server_id)?;
    Ok(state.mocks.logs(&server_id))
}

/// Pushes every new transaction of a running mock server to `sink` and returns the subscription
/// id for [`mock_server_logs_unsubscribe_impl`].
///
/// Entries logged before subscribing are not replayed: call [`mock_server_logs_impl`] afterwards and
/// de-duplicate by entry id. Errors when the server is unknown, foreign to `workspace_id`, or stopped.
pub fn mock_server_logs_subscribe_impl(
    state: &AppState,
    workspace_id: String,
    server_id: String,
    sink: Arc<dyn EventSink<MockLogEvent>>,
) -> Result<u64, AppError> {
    {
        let store = state.store();
        let server = store
            .get_mock_server(&server_id)
            .map_err(command_error)?
            .ok_or_else(|| AppError::not_found(format!("mock server {server_id} not found")))?;
        if server.workspace_id.as_str() != workspace_id {
            return Err(AppError::not_found(format!(
                "mock server {server_id} not found"
            )));
        }
    }
    state
        .mocks
        .subscribe_logs(&server_id, sink)
        .map_err(|error| match error {
            MockSubscribeError::NotRunning => {
                AppError::not_found(format!("mock server {server_id} is not running"))
            }
            MockSubscribeError::TooMany => AppError::conflict(error.to_string()),
        })
}

/// Ends a log subscription; returns whether it was still active.
pub fn mock_server_logs_unsubscribe_impl(state: &AppState, subscription_id: u64) -> bool {
    state.mocks.unsubscribe_logs(subscription_id)
}

/// Resets the counters of a running server (sequential cursors, `RequestNumber` counts, hit
/// count) without touching its data buckets or log, and returns its status.
///
/// # Errors
/// `NotFound` for an unknown or foreign server, or one that is not running.
pub fn mock_server_state_reset_impl(
    state: &AppState,
    workspace_id: String,
    server_id: String,
) -> Result<MockServerStatusDto, AppError> {
    let server = owned_mock_server(state, &workspace_id, &server_id)?;
    if !state.mocks.reset_state(&server_id) {
        return Err(AppError::not_found(format!(
            "mock server {server_id} is not running"
        )));
    }
    Ok(mock_server_status(&server, &state.mocks))
}

/// Clears a running mock server's transaction log; unknown or foreign servers are `NotFound`,
/// and so is an owned server that is not running.
pub fn mock_server_clear_logs_impl(
    state: &AppState,
    workspace_id: String,
    server_id: String,
) -> Result<(), AppError> {
    owned_mock_server(state, &workspace_id, &server_id)?;
    if !state.mocks.clear_logs(&server_id) {
        return Err(AppError::not_found(format!(
            "mock server {server_id} is not running"
        )));
    }
    Ok(())
}

/// The live data of a mock server: its in-memory buckets next to whether CRUD routes
/// changed them. A stopped server answers `running: false` with no buckets: its live data equals
/// the saved data once it starts.
///
/// # Errors
/// `NotFound` for a server that is unknown or belongs to another workspace.
pub fn mock_server_data_get_impl(
    state: &AppState,
    workspace_id: String,
    server_id: String,
) -> Result<MockLiveDataDto, AppError> {
    owned_mock_server(state, &workspace_id, &server_id)?;
    let buckets = state.mocks.live_data(&server_id);
    Ok(MockLiveDataDto {
        running: buckets.is_some(),
        buckets: buckets.unwrap_or_default(),
        server_id,
    })
}

/// Puts one data bucket (all when `bucket_id` is `None`) of a running server back to the saved
/// data and returns the new snapshot.
///
/// # Errors
/// `NotFound` for an unknown or foreign server, a stopped server or an unknown bucket.
pub fn mock_server_data_reset_impl(
    state: &AppState,
    workspace_id: String,
    server_id: String,
    bucket_id: Option<String>,
) -> Result<MockLiveDataDto, AppError> {
    owned_mock_server(state, &workspace_id, &server_id)?;
    state
        .mocks
        .reset_live_data(&server_id, bucket_id.as_deref())
        .map_err(|error| match error {
            MockDataResetError::NotRunning => {
                AppError::not_found(format!("mock server {server_id} is not running"))
            }
            MockDataResetError::UnknownBucket => AppError::not_found(format!(
                "data bucket {} not found",
                bucket_id.as_deref().unwrap_or_default()
            )),
        })?;
    mock_server_data_get_impl(state, workspace_id, server_id)
}

/// Imports a Mockoon environment as a new mock server with a collision-free ID.
pub fn mock_server_import_mockoon_impl(
    state: &AppState,
    workspace_id: String,
    json_text: String,
) -> Result<MockoonImportResultDto, AppError> {
    let workspace_id = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let (mut server, warnings) =
        import_mockoon_environment(&workspace_id, &json_text).map_err(AppError::validation)?;
    server.updated_at_ms = current_time_ms();

    {
        let store = state.store();
        ensure_workspace_exists(&store, workspace_id.as_str())?;
        server.id = unique_mock_server_id(&store, &server.id)?;
        validate_mock_server(&server)?;
        store.save_mock_server(&server).map_err(command_error)?;
    }
    state.sync.notify_changed();

    Ok(MockoonImportResultDto { server, warnings })
}

/// Creates a stopped mock server from an OpenAPI document: one route per operation with
/// example responses, the base URL path as the endpoint prefix, the first free port from
/// `mock.first_port` and new ids. No proxy, no callbacks.
pub fn mock_openapi_import_impl(
    state: &AppState,
    workspace_id: String,
    text: String,
) -> Result<MockoonImportResultDto, AppError> {
    let generated = crate::importers::openapi_mock_routes(&text).map_err(command_error)?;
    let mut server = mock_server_new(state, &workspace_id)?;
    server.name = generated.name;
    server.endpoint_prefix = generated.endpoint_prefix;
    server.routes = generated.routes;
    reissue_ids(&mut server, new_entity_id);
    server.updated_at_ms = current_time_ms();
    validate_mock_server(&server)?;
    state
        .store()
        .save_mock_server(&server)
        .map_err(command_error)?;
    state.sync.notify_changed();
    Ok(MockoonImportResultDto {
        server,
        warnings: generated.warnings,
    })
}

/// The first loopback port from `from_port` up that no saved mock server of any workspace uses
/// and that binds right now. The probe is a hint: the port may be taken before the
/// server starts, and starting then reports a port in use.
pub fn mock_port_suggest_impl(
    state: &AppState,
    workspace_id: String,
    from_port: u16,
) -> Result<u16, AppError> {
    mock_port_suggest_with(state, &workspace_id, from_port, port_is_bindable)
}

/// [`mock_port_suggest_impl`] with the bind probe injected, so tests do not depend on which
/// ports the machine has free.
pub(crate) fn mock_port_suggest_with(
    state: &AppState,
    workspace_id: &str,
    from_port: u16,
    is_bindable: impl Fn(u16) -> bool,
) -> Result<u16, AppError> {
    let ports = {
        let store = state.store();
        ensure_workspace_exists(&store, workspace_id)?;
        store.list_mock_server_ports().map_err(command_error)?
    };
    first_free_port(from_port.max(1), &ports, is_bindable)
}

/// Copies a mock server: every id is new and the references between its parts are
/// rewritten, the name is `"<name> copy"` (`copy 2`, ... when taken), the port is the first free
/// one above the original's, and the copy is saved stopped.
pub fn mock_server_duplicate_impl(
    state: &AppState,
    workspace_id: String,
    server_id: String,
) -> Result<MockServer, AppError> {
    mock_server_duplicate_with(state, &workspace_id, &server_id, port_is_bindable)
}

/// [`mock_server_duplicate_impl`] with the bind probe injected.
pub(crate) fn mock_server_duplicate_with(
    state: &AppState,
    workspace_id: &str,
    server_id: &str,
    is_bindable: impl Fn(u16) -> bool,
) -> Result<MockServer, AppError> {
    let mut copy = owned_mock_server(state, workspace_id, server_id)?;
    let (names, ports) = {
        let store = state.store();
        let names: Vec<String> = store
            .list_mock_servers(workspace_id)
            .map_err(command_error)?
            .into_iter()
            .map(|server| server.name)
            .collect();
        (
            names,
            store.list_mock_server_ports().map_err(command_error)?,
        )
    };
    reissue_ids(&mut copy, new_entity_id);
    copy.name = copy_name(&copy.name, &names);
    copy.port = first_free_port(copy.port, &ports, is_bindable)?;
    copy.updated_at_ms = current_time_ms();
    validate_mock_server(&copy)?;

    state
        .store()
        .save_mock_server(&copy)
        .map_err(command_error)?;
    state.sync.notify_changed();
    Ok(copy)
}

/// `"<name> copy"`, or `"<name> copy N"` with the smallest `N ≥ 2` that no name in `taken` has.
fn copy_name(name: &str, taken: &[String]) -> String {
    let first = format!("{name} copy");
    if !taken.contains(&first) {
        return first;
    }
    (2_u64..)
        .map(|number| format!("{first} {number}"))
        .find(|candidate| !taken.contains(candidate))
        .unwrap_or(first)
}

/// File name an export dialog should suggest.
#[must_use]
pub fn mock_server_export_suggested_filename() -> String {
    "spanbay-mock-servers.json".to_owned()
}

/// Writes `server_ids` (all servers of the workspace when `None`) as a SpanBay mock export to
/// the chosen path, owner-only because headers and callbacks can carry secrets. Literal values
/// of credential headers (proxy request and callback headers) are left empty, with a warning
/// that counts them, unless `include_credentials` asks for them (D-272). The path is checked
/// first, so a canceled dialog (`None`) builds and writes nothing. A server that is unknown or
/// belongs to another workspace is `NotFound`; an empty selection is a validation error.
pub fn mock_server_export_save_to_path_impl(
    state: &AppState,
    workspace_id: String,
    server_ids: Option<Vec<String>>,
    include_credentials: bool,
    selected_path: Option<PathBuf>,
) -> Result<MockExportSaveResult, AppError> {
    let Some(path) = selected_path else {
        return Ok(MockExportSaveResult {
            saved: false,
            path: None,
            server_count: 0,
            warnings: Vec::new(),
        });
    };

    let mut servers = {
        let store = state.store();
        ensure_workspace_exists(&store, &workspace_id)?;
        let all = store
            .list_mock_servers(&workspace_id)
            .map_err(command_error)?;
        match server_ids {
            None => all,
            Some(ids) => {
                let mut chosen: Vec<MockServer> = Vec::with_capacity(ids.len());
                for id in &ids {
                    if chosen.iter().any(|server| &server.id == id) {
                        continue;
                    }
                    let server = all.iter().find(|server| &server.id == id).ok_or_else(|| {
                        AppError::not_found(format!("mock server {id} not found"))
                    })?;
                    chosen.push(server.clone());
                }
                chosen
            }
        }
    };
    if servers.is_empty() {
        return Err(AppError::validation("there are no mock servers to export"));
    }

    let server_count = servers.len();
    let stripped = if include_credentials {
        0
    } else {
        strip_literal_credentials(&mut servers)
    };
    let json = export_json(servers, current_time_ms()).map_err(command_error)?;
    write_private_file(&path, json)
        .map_err(|error| AppError::io(format!("failed to write mock server export: {error}")))?;
    state.note_user_file(&path);

    let warnings = match stripped {
        0 => Vec::new(),
        1 => vec![
            "1 literal credential header was left empty; include literal credentials to export it"
                .to_owned(),
        ],
        count => vec![format!(
            "{count} literal credential headers were left empty; include literal credentials to export them"
        )],
    };
    Ok(MockExportSaveResult {
        saved: true,
        path: Some(normalize_display_path(&path)),
        server_count,
        warnings,
    })
}

/// Switches recording of proxied exchanges on a running server of the workspace; switching it
/// on starts an empty session. Recordings live in memory until the server stops. A stopped
/// server is a validation error.
pub fn mock_recording_set_impl(
    state: &AppState,
    workspace_id: String,
    server_id: String,
    enabled: bool,
) -> Result<MockRecordingStatusDto, AppError> {
    owned_mock_server(state, &workspace_id, &server_id)?;
    let count = state
        .mocks
        .set_recording(&server_id, enabled)
        .ok_or_else(|| AppError::validation("start the mock server to record proxied requests"))?;
    Ok(MockRecordingStatusDto { enabled, count })
}

/// Routes for the exchanges `server` (the caller's working copy of a running server of the
/// workspace) recorded that none of its routes answers yet; the caller appends and saves them.
/// The recordings stay, so a save that fails can be retried.
pub fn mock_recording_routes_impl(
    state: &AppState,
    workspace_id: String,
    server: MockServer,
) -> Result<MockRecordingRoutesDto, AppError> {
    owned_mock_server(state, &workspace_id, &server.id)?;
    let recordings = state.mocks.recordings(&server.id);
    let (routes, skipped) = mock_routes_from_recordings(&server, &recordings);
    Ok(MockRecordingRoutesDto { routes, skipped })
}

/// A route made from a log row of `server`: from the recorded exchange with its whole body when
/// the server recorded that row, else from the row (whose body may be cut at 64 KiB).
#[must_use]
pub fn mock_route_from_log_impl(
    state: &AppState,
    server: &MockServer,
    log: &MockTransactionLog,
) -> MockRoute {
    match state.mocks.recorded_exchange(&server.id, &log.id) {
        Some(exchange) => mock_route_from_exchange(server, &exchange, server.routes.len()),
        None => mock_route_from_log(server, log),
    }
}

/// Imports a SpanBay mock export into `workspace_id`. Every server gets a fresh id
/// and the workspace, is saved stopped, keeps its port unless another saved server holds it
/// (then the first free port above is used), and has proxying switched off: an import must not
/// start outbound traffic by itself. A warning names every local file a response serves as its
/// body. Callbacks are kept. Nothing is saved when any server is
/// invalid. A file of a newer version is `Unsupported`.
pub fn mock_server_import_json_impl(
    state: &AppState,
    workspace_id: String,
    json_text: String,
) -> Result<MockImportResultDto, AppError> {
    mock_server_import_json_with(state, &workspace_id, &json_text, port_is_bindable)
}

/// [`mock_server_import_json_impl`] with the bind probe injected.
pub(crate) fn mock_server_import_json_with(
    state: &AppState,
    workspace_id: &str,
    json_text: &str,
    is_bindable: impl Fn(u16) -> bool,
) -> Result<MockImportResultDto, AppError> {
    let workspace_id = WorkspaceId::new(workspace_id).map_err(command_error)?;
    let mut servers = parse_export(json_text).map_err(|error| match error {
        TransferError::Invalid(_) => AppError::validation(error.to_string()),
        TransferError::Newer { .. } => AppError::unsupported(error.to_string()),
    })?;
    if servers.is_empty() {
        return Err(AppError::validation("the export contains no mock servers"));
    }
    for server in &servers {
        validate_mock_server(server).map_err(|error| {
            AppError::validation(format!("mock server {}: {error}", server.name))
        })?;
    }

    let mut warnings = Vec::new();
    let store = state.store();
    ensure_workspace_exists(&store, workspace_id.as_str())?;
    let mut ports = store.list_mock_server_ports().map_err(command_error)?;
    let now = current_time_ms();
    for server in &mut servers {
        server.id = new_entity_id("mock");
        server.workspace_id = workspace_id.clone();
        server.updated_at_ms = now;
        if ports.contains(&server.port) {
            let free = first_free_port(server.port, &ports, &is_bindable)?;
            warnings.push(format!(
                "{}: port {} is used by another mock server; using {free}",
                server.name, server.port
            ));
            server.port = free;
        }
        ports.push(server.port);
        if server.proxy_enabled {
            server.proxy_enabled = false;
            warnings.push(format!(
                "{}: proxying was switched off on import; review the target URL and enable it manually",
                server.name
            ));
        }
        let files = file_body_paths(server);
        if !files.is_empty() {
            warnings.push(format!(
                "{}: responses serve these local files to the mock's clients once started: {}; check the paths before starting the server",
                server.name,
                files.join(", ")
            ));
        }
    }
    for server in &servers {
        store.save_mock_server(server).map_err(command_error)?;
    }
    drop(store);
    state.sync.notify_changed();

    Ok(MockImportResultDto { servers, warnings })
}
