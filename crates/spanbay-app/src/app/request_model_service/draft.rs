//! Defaults, new-request templates, draft normalisation and history → draft.
//!
//! A draft is a plain [`Request`]: the former UI-only `RequestDraft` differed from the wire
//! DTO only by camelCase names, so nothing but the rules below needs a home here. Tab state
//! (dirty flag, tab id, execution, multipart validations) belongs to the shell.

use serde::{Deserialize, Serialize};

use super::effective_auth::{EffectiveAuthInput, request_effective_auth};
use super::url_sync::{merge_path_variables, query_params_from_url};
use crate::app::factory_service::new_entity_id;
use crate::app::{AppState, command_error};
use crate::error::AppError;
use crate::workspace::models::{
    AuthConfig, ExecutionHistoryEntry, HttpMethod, KeyValue, NetworkOptions, ProtocolKind, Request,
    RequestBody, RequestId, WorkspaceId,
};

/// Name of a request whose name is blank.
const UNTITLED_REQUEST: &str = "Untitled Request";

/// The starting point of a new request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestTemplate {
    /// `GET {{baseUrl}}/path` accepting JSON.
    HttpGet,
    /// `POST` with a JSON content type and body.
    HttpPostJson,
    /// A GraphQL `POST {{baseUrl}}/graphql` query.
    Graphql,
}

/// Fields the caller sets on top of a template, as `openNew(template, target, overrides)` did.
///
/// A `url` also derives the name (the trimmed URL), the query rows and the path variables,
/// unless those are given explicitly.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct RequestDraftOverrides {
    #[serde(default)]
    pub name: Option<String>,
    #[serde(default)]
    pub protocol: Option<ProtocolKind>,
    #[serde(default)]
    pub method: Option<HttpMethod>,
    #[serde(default)]
    pub url: Option<String>,
    #[serde(default)]
    pub query_params: Option<Vec<KeyValue>>,
    #[serde(default)]
    pub path_variables: Option<Vec<KeyValue>>,
    #[serde(default)]
    pub headers: Option<Vec<KeyValue>>,
    #[serde(default)]
    pub auth: Option<AuthConfig>,
    #[serde(default)]
    pub body: Option<RequestBody>,
}

/// A fresh, unsaved request id (the same generator as every other entity id).
fn new_request_id() -> String {
    new_entity_id("req")
}

fn header(key: &str, value: &str) -> KeyValue {
    KeyValue {
        key: key.to_owned(),
        value: value.to_owned(),
        enabled: true,
        description: None,
    }
}

/// Builds the unsaved request a template starts from, with a fresh id.
///
/// `collection_id` and `folder_id` place it in the tree (a blank id is rejected); the
/// shell chooses them (selected request's collection, first collection, …). `network` is what
/// the request starts with: the global network settings, as `NetworkOptions::from(&settings.network)`.
pub fn request_new_draft(
    network: &NetworkOptions,
    workspace_id: &str,
    collection_id: Option<&str>,
    folder_id: Option<&str>,
    template: RequestTemplate,
    overrides: Option<RequestDraftOverrides>,
) -> Result<Request, AppError> {
    let mut request = Request::new(
        new_request_id(),
        workspace_id,
        UNTITLED_REQUEST,
        "{{baseUrl}}/path",
    )
    .map_err(command_error)?;
    request.network_options = network.clone();
    if let Some(collection_id) = collection_id {
        request = request
            .with_collection(collection_id)
            .map_err(command_error)?;
    }
    if let Some(folder_id) = folder_id {
        request = request.with_folder(folder_id).map_err(command_error)?;
    }
    match template {
        RequestTemplate::HttpGet => {
            request.headers = vec![header("Accept", "application/json")];
        }
        RequestTemplate::HttpPostJson => {
            request.name = "Create resource".to_owned();
            request.method = HttpMethod::Post;
            request.headers = vec![header("Content-Type", "application/json")];
            request.body = RequestBody::Json {
                content: "{\n  \"name\": \"example\"\n}".to_owned(),
            };
        }
        RequestTemplate::Graphql => {
            request.name = "GraphQL Query".to_owned();
            request.protocol = ProtocolKind::Graphql;
            request.method = HttpMethod::Post;
            request.url = "{{baseUrl}}/graphql".to_owned();
            request.body = RequestBody::Graphql {
                query: "query SpanBay {\n  __typename\n}".to_owned(),
                variables_json: None,
                operation_name: None,
            };
        }
    }
    if let Some(overrides) = overrides {
        apply_overrides(&mut request, overrides);
    }
    Ok(request)
}

fn apply_overrides(request: &mut Request, overrides: RequestDraftOverrides) {
    let RequestDraftOverrides {
        name,
        protocol,
        method,
        url,
        query_params,
        path_variables,
        headers,
        auth,
        body,
    } = overrides;
    if let Some(url) = url {
        request.name = name.unwrap_or_else(|| {
            let trimmed = url.trim();
            if trimmed.is_empty() {
                request.name.clone()
            } else {
                trimmed.to_owned()
            }
        });
        request.query_params = query_params.unwrap_or_else(|| query_params_from_url(&url));
        request.path_variables =
            path_variables.unwrap_or_else(|| merge_path_variables(&url, &request.path_variables));
        request.url = url;
    } else {
        if let Some(name) = name {
            request.name = name;
        }
        if let Some(query_params) = query_params {
            request.query_params = query_params;
        }
        if let Some(path_variables) = path_variables {
            request.path_variables = path_variables;
        }
    }
    if let Some(protocol) = protocol {
        request.protocol = protocol;
    }
    if let Some(method) = method {
        request.method = method;
    }
    if let Some(headers) = headers {
        request.headers = headers;
    }
    if let Some(auth) = auth {
        request.auth = auth;
    }
    if let Some(body) = body {
        request.body = body;
    }
}

/// Makes a request safe to edit and to save: it belongs to `workspace_id`, has an id, and has a
/// non-blank name ([`UNTITLED_REQUEST`] otherwise). Replaces `normalizeDraft`, `draftFromRequest`
/// and `requestFromDraft`; missing optional fields were already defaulted by deserialisation
/// (network options included), so nothing else needs coercing.
pub fn draft_normalize(mut request: Request, workspace_id: &str) -> Result<Request, AppError> {
    request.workspace_id = WorkspaceId::new(workspace_id).map_err(command_error)?;
    if request.id.as_str().trim().is_empty() {
        request.id = RequestId::new(new_request_id()).map_err(command_error)?;
    }
    if request.name.trim().is_empty() {
        UNTITLED_REQUEST.clone_into(&mut request.name);
    }
    Ok(request)
}

/// The draft that re-opens a history entry as a new request in `workspace_id`: its request
/// snapshot (the definition from before the scripts ran), else `linked_request`, else a request
/// with the recorded method rebuilt from the recorded URL. Always unsaved (fresh id, no
/// revision), outside any collection or folder, named like the entry. An `Inherit` auth left in
/// the source stays `Inherit` here; [`request_draft_from_history_resolved`] resolves it first.
pub fn request_draft_from_history(
    network: &NetworkOptions,
    entry: ExecutionHistoryEntry,
    workspace_id: &str,
    linked_request: Option<Request>,
) -> Result<Request, AppError> {
    let mut draft = match entry.request_snapshot.or(linked_request) {
        Some(snapshot) => draft_normalize(snapshot, workspace_id)?,
        None => {
            let mut rebuilt = request_new_draft(
                network,
                workspace_id,
                None,
                None,
                RequestTemplate::HttpGet,
                Some(RequestDraftOverrides {
                    url: Some(entry.request_url),
                    ..RequestDraftOverrides::default()
                }),
            )?;
            rebuilt.method = entry.method;
            rebuilt
        }
    };
    draft.id = RequestId::new(new_request_id()).map_err(command_error)?;
    draft.collection_id = None;
    draft.folder_id = None;
    draft.revision = None;
    draft.name = entry.request_name;
    draft_normalize(draft, workspace_id)
}

/// [`request_draft_from_history`] after resolving an `Inherit` auth of the source (snapshot, else
/// `linked_request`) through the collection and folders it was recorded in, as they are now.
/// History keeps inherited auth as `Inherit` so a collection credential is not copied into every
/// row; the draft leaves its collection, so it carries the resolved auth instead. A collection or
/// folder deleted since resolves to no auth.
///
/// # Errors
/// As [`request_draft_from_history`], plus [`AppError::Database`] when the parents cannot be
/// read.
pub fn request_draft_from_history_resolved(
    state: &AppState,
    mut entry: ExecutionHistoryEntry,
    workspace_id: &str,
    mut linked_request: Option<Request>,
) -> Result<Request, AppError> {
    let source = match entry.request_snapshot.as_mut() {
        Some(snapshot) => Some(snapshot),
        None => linked_request.as_mut(),
    };
    if let Some(source) = source
        && matches!(source.auth, AuthConfig::Inherit)
        && (source.collection_id.is_some() || source.folder_id.is_some())
    {
        let resolved = request_effective_auth(
            state,
            EffectiveAuthInput {
                workspace_id: source.workspace_id.as_str().to_owned(),
                request_id: None,
                collection_id: source
                    .collection_id
                    .as_ref()
                    .map(|id| id.as_str().to_owned()),
                folder_id: source.folder_id.as_ref().map(|id| id.as_str().to_owned()),
                auth: Some(AuthConfig::Inherit),
            },
        )?;
        source.auth = resolved.config;
    }
    let network = NetworkOptions::from(&state.settings().network);
    request_draft_from_history(&network, entry, workspace_id, linked_request)
}
