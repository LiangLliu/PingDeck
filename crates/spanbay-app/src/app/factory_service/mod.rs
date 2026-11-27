//! Entity factories, ids and reorder/duplicate helpers.
//!
//! Single responsibility: everything a shell needs to *create* or *rearrange* workspace
//! entities without re-implementing the rules: the one id generator, the starter workspace,
//! the unsaved `*_new` defaults, pure reorder/duplicate/remove over DTO lists, and the mock
//! route helpers (from a log entry, test request, CRUD endpoint listing).
//!
//! The `*_new` functions never persist. Store-aware ones (`mock_server_new`, `flow_new`) only read the
//! store under a short lock to pick a free port or an unused name; the shell saves the result with the regular `*_save` use-case.

mod defaults;
mod lists;
mod mock_routes;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::workspace::models::Workspace;
use crate::workspace::variables::uuid_v4;
use crate::workspace::{Collection, Environment, EnvironmentVariable, Request, StorageError};

use super::{AppState, command_error};

pub use defaults::{
    AssertionKindInfo, assertion_kinds, assertion_new, environment_new, flow_new, flow_step_new,
    mock_bucket_new, mock_callback_new, mock_response_new, mock_route_new, mock_rule_new,
    mock_server_new,
};
pub(crate) use defaults::{first_free_port, port_is_bindable};
pub use lists::{
    flow_duplicate, flow_step_duplicate, flow_step_remove, flow_steps_reorder,
    mock_response_duplicate, mock_route_duplicate, request_duplicate,
};
pub use mock_routes::{
    MockCrudEndpoint, MockRouteTestRequest, mock_crud_endpoints, mock_route_from_exchange,
    mock_route_from_log, mock_route_test_request, mock_routes_from_recordings,
    mock_server_add_route,
};

/// Returns a collision-resistant id `<prefix>_<random UUID v4>`: the one generator for every
/// workspace entity id (mock servers, routes, flows, environments, ...).
#[must_use]
pub fn new_entity_id(prefix: &str) -> String {
    format!("{prefix}_{}", uuid_v4())
}

/// Everything [`workspace_create_starter`] saved.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct StarterWorkspace {
    /// The new workspace.
    pub workspace: Workspace,
    /// `Local Environment` with `baseUrl` and a disabled secret `apiToken` placeholder.
    pub environment: Environment,
    /// `Starter Collection`.
    pub collection: Collection,
    /// The `Health check` request (`GET {{baseUrl}}/health`) inside the collection.
    pub request: Request,
}

/// Creates a workspace with a starter environment, collection and `Health check` request.
///
/// Fails with `Conflict` when `workspace_id` already exists (nothing is overwritten); when a
/// later row cannot be written, the half-created workspace is deleted again.
pub fn workspace_create_starter(
    state: &AppState,
    workspace_id: &str,
    name: &str,
) -> Result<StarterWorkspace, AppError> {
    let workspace = Workspace::new(workspace_id, name).map_err(command_error)?;
    let mut environment = Environment::new(
        new_entity_id("env"),
        workspace.id.as_str(),
        "Local Environment",
    )
    .map_err(command_error)?;
    let mut api_token = EnvironmentVariable::masked("apiToken", "replace-with-your-token")
        .map_err(command_error)?;
    api_token.enabled = false;
    environment.variables = vec![
        EnvironmentVariable::plain("baseUrl", "https://api.example.com").map_err(command_error)?,
        api_token,
    ];
    let collection = Collection::new(
        new_entity_id("col"),
        workspace.id.as_str(),
        "Starter Collection",
    )
    .map_err(command_error)?;
    let request = Request::new(
        new_entity_id("req"),
        workspace.id.as_str(),
        "Health check",
        "{{baseUrl}}/health",
    )
    .and_then(|request| request.with_collection(collection.id.as_str()))
    .map_err(command_error)?;

    let store = state.store();
    if store
        .get_workspace(&workspace.id)
        .map_err(command_error)?
        .is_some()
    {
        return Err(AppError::conflict(format!(
            "workspace {} already exists",
            workspace.id.as_str()
        )));
    }
    store.save_workspace(&workspace).map_err(command_error)?;
    let saved: Result<(), StorageError> = (|| {
        store.save_environment(&environment)?;
        store.save_collection(&collection)?;
        store.save_request(&request)
    })();
    if let Err(error) = saved {
        // The workspace row cascades its children away; a failed cleanup changes nothing.
        let _ = store.delete_workspace(&workspace.id);
        return Err(command_error(error));
    }
    Ok(StarterWorkspace {
        workspace,
        environment,
        collection,
        request,
    })
}
