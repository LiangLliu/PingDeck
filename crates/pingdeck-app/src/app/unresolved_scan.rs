//! Workspace scan for `{{variables}}` that nothing defines.
//!
//! Single responsibility: read the saved requests, collections, folders, flows and the chosen
//! environment, and report which `{{name}}` tokens would fail to resolve at send time. Nothing
//! is written. Names a script assigns or a flow step extracts are only known at run time, so
//! they count as defined; runner data columns are unknown and can show up here.

use std::collections::{BTreeMap, HashMap, HashSet};

use regex::Regex;
use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::executor::{auth_templates, body_templates, template_tokens};
use crate::workspace::{
    Collection, EnvironmentId, EnvironmentVariable, Flow, Folder, HttpMethod, Request, WorkspaceId,
    effective_auth,
};

use super::{AppState, command_error};

/// A saved request that uses an unresolved token: enough to list it without a second lookup.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvedRequestDto {
    /// Request id.
    pub id: String,
    /// Request name.
    pub name: String,
    /// HTTP method.
    pub method: HttpMethod,
}

/// A collection whose variables define an unresolved name: the read-only "where a value could
/// come from" row of the Unresolved help.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvedCollectionDto {
    /// Collection id.
    pub id: String,
    /// Collection name.
    pub name: String,
}

/// A `{{name}}` token no scope defines, with the saved requests that use it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UnresolvedVariableDto {
    /// The token name, trimmed.
    pub name: String,
    /// Ids of the requests using it, in workspace order.
    pub request_ids: Vec<String>,
    /// The same requests with their name and method, in the same order.
    pub requests: Vec<UnresolvedRequestDto>,
    /// `request_ids.len()`.
    pub request_count: u32,
    /// Collections whose variables define this name, in workspace order (empty when none does):
    /// the value exists, but only for the requests of those collections.
    pub collections: Vec<UnresolvedCollectionDto>,
}

/// Matches the variable name of a `.set("name"` call (`pm.environment.set`, `pm.globals.set`,
/// `pm.variables.set`, `pm.collectionVariables.set` and any other `.set` with a literal key).
const SCRIPT_SET_PATTERN: &str = r#"\.set\s*\(\s*(?:"([^"]+)"|'([^']+)'|`([^`]+)`)"#;

/// Lists the unresolved `{{name}}` tokens of the saved requests of a workspace for the given
/// environment (or none: then only the globals and collections define names), most used first
/// (then by name).
///
/// A token is resolved when it is `$…` (built-in), starts with `secret:`, is an enabled variable
/// of the globals, of the request's collection or of the environment, is assigned by any
/// script of the workspace, or is extracted by any flow step. Scanned: URL, path variables,
/// query, headers (enabled rows), the body, and the auth the request ends up with after
/// inheritance (so collection and folder auth count for the requests that use them).
pub fn environment_unresolved_scan_impl(
    state: &AppState,
    workspace_id: String,
    environment_id: Option<String>,
) -> Result<Vec<UnresolvedVariableDto>, AppError> {
    let workspace = WorkspaceId::new(&workspace_id).map_err(command_error)?;
    let environment_id = environment_id
        .map(|id| EnvironmentId::new(&id).map_err(command_error))
        .transpose()?;
    let (environment, globals, collections, folders, requests, flows) = {
        let store = state.store();
        // No environment: only the globals and the collections can define a name.
        let environment = match &environment_id {
            Some(environment_id) => {
                store
                    .get_environment(&workspace, environment_id)
                    .map_err(command_error)?
                    .ok_or_else(|| {
                        AppError::not_found(format!(
                            "environment {} not found in workspace {workspace_id}",
                            environment_id.as_str()
                        ))
                    })?
                    .variables
            }
            None => Vec::new(),
        };
        let collections = store
            .list_collections(&workspace_id)
            .map_err(command_error)?;
        let mut folders = Vec::new();
        for collection in &collections {
            folders.extend(
                store
                    .list_folders(&workspace, &collection.id)
                    .map_err(command_error)?,
            );
        }
        (
            environment,
            store
                .list_global_variables(&workspace_id)
                .map_err(command_error)?,
            collections,
            folders,
            store.list_requests(&workspace_id).map_err(command_error)?,
            store.list_flows(&workspace_id).map_err(command_error)?,
        )
    };

    let assigned = assigned_names(&requests, &folders, &collections, &flows)?;
    let globals = bound_keys(&globals);
    let environment = bound_keys(&environment);
    let collection_variables: HashMap<&str, HashMap<&str, bool>> = collections
        .iter()
        .map(|collection| (collection.id.as_str(), bound_keys(&collection.variables)))
        .collect();
    // Every collection that defines a name, in workspace order: an unresolved token used in
    // another collection still has a home the help can point at.
    let mut defining: HashMap<&str, Vec<UnresolvedCollectionDto>> = HashMap::new();
    for collection in &collections {
        for name in bound_keys(&collection.variables).into_keys() {
            defining
                .entry(name)
                .or_default()
                .push(UnresolvedCollectionDto {
                    id: collection.id.as_str().to_owned(),
                    name: collection.name.clone(),
                });
        }
    }
    let folder_collection: HashMap<&str, &str> = folders
        .iter()
        .map(|folder| (folder.id.as_str(), folder.collection_id.as_str()))
        .collect();

    let mut unresolved: BTreeMap<String, Vec<UnresolvedRequestDto>> = BTreeMap::new();
    for request in &requests {
        let collection_id = request
            .collection_id
            .as_ref()
            .map(|id| id.as_str())
            .or_else(|| {
                let folder_id = request.folder_id.as_ref()?;
                folder_collection.get(folder_id.as_str()).copied()
            });
        let collection = collection_id.and_then(|id| {
            collections
                .iter()
                .find(|collection| collection.id.as_str() == id)
        });
        let own = collection_id.and_then(|id| collection_variables.get(id));
        let mut resolved_request = request.clone();
        resolved_request.auth = effective_auth(request, collection, &folders);

        for name in request_tokens(&resolved_request) {
            // The send order (environment, collection, globals): the narrowest definition
            // decides, and one that needs a value (a cleared secret) leaves the name unresolved.
            let bound = environment
                .get(name.as_str())
                .or_else(|| own.and_then(|keys| keys.get(name.as_str())))
                .or_else(|| globals.get(name.as_str()))
                .copied();
            let defined = name.starts_with('$')
                || name.starts_with("secret:")
                || bound == Some(true)
                || assigned.contains(&name);
            if defined {
                continue;
            }
            let users = unresolved.entry(name).or_default();
            if !users.iter().any(|user| user.id == request.id.as_str()) {
                users.push(UnresolvedRequestDto {
                    id: request.id.as_str().to_owned(),
                    name: request.name.clone(),
                    method: request.method,
                });
            }
        }
    }

    let mut rows: Vec<UnresolvedVariableDto> = unresolved
        .into_iter()
        .map(|(name, requests)| UnresolvedVariableDto {
            collections: defining.remove(name.as_str()).unwrap_or_default(),
            name,
            request_count: u32::try_from(requests.len()).unwrap_or(u32::MAX),
            request_ids: requests.iter().map(|user| user.id.clone()).collect(),
            requests,
        })
        .collect();
    // `BTreeMap` already orders by name; the stable sort keeps that within equal counts.
    rows.sort_by_key(|row| std::cmp::Reverse(row.request_count));
    Ok(rows)
}

/// Each enabled key of a layer with whether its first enabled row has a value to send
/// ([`EnvironmentVariable::needs_value`]).
fn bound_keys(variables: &[EnvironmentVariable]) -> HashMap<&str, bool> {
    let mut keys = HashMap::new();
    for variable in variables.iter().filter(|variable| variable.enabled) {
        keys.entry(variable.key.as_str())
            .or_insert_with(|| !variable.needs_value());
    }
    keys
}

/// Every token name of the parts of `request` that are sent.
pub fn request_tokens(request: &Request) -> Vec<String> {
    let mut texts: Vec<&str> = vec![request.url.as_str()];
    for row in request
        .path_variables
        .iter()
        .chain(&request.query_params)
        .chain(&request.headers)
        .filter(|row| row.enabled)
    {
        texts.push(&row.key);
        texts.push(&row.value);
    }
    texts.extend(body_templates(&request.body));
    texts.extend(auth_templates(&request.auth));
    texts.into_iter().flat_map(template_tokens).collect()
}

/// Names a script assigns (any pre-request or test script of the workspace) or a flow step
/// extracts.
fn assigned_names(
    requests: &[Request],
    folders: &[Folder],
    collections: &[Collection],
    flows: &[Flow],
) -> Result<HashSet<String>, AppError> {
    let pattern = Regex::new(SCRIPT_SET_PATTERN)
        .map_err(|error| AppError::internal(format!("script scan pattern: {error}")))?;
    let scripts = requests
        .iter()
        .flat_map(|r| [&r.pre_request_script, &r.test_script])
        .chain(
            folders
                .iter()
                .flat_map(|f| [&f.pre_request_script, &f.test_script]),
        )
        .chain(
            collections
                .iter()
                .flat_map(|c| [&c.pre_request_script, &c.test_script]),
        );
    let mut names: HashSet<String> = scripts
        .flat_map(|script| pattern.captures_iter(script))
        .filter_map(|captures| {
            captures
                .iter()
                .skip(1)
                .flatten()
                .next()
                .map(|name| name.as_str().trim().to_owned())
        })
        .collect();
    names.extend(
        flows
            .iter()
            .flat_map(|flow| &flow.steps)
            .flat_map(|step| &step.extractions)
            .map(|extraction| extraction.variable_name.trim().to_owned()),
    );
    Ok(names)
}
