//! Row mapping between `requests` rows and [`Request`].

use spanbay_model::ai_request::{AiRequestConfig, RequestMode};
use spanbay_model::models::{
    Assertion, AuthConfig, KeyValue, NetworkOptions, Request, RequestBody, RequestExample,
};
use spanbay_model::validation::WorkspaceError;

use super::{http_method_from_storage, protocol_from_storage};
use crate::storage::StorageError;

/// The `requests` columns [`stored_request_from_row`] reads, in its order.
macro_rules! request_columns {
    () => {
        "id, workspace_id, collection_id, folder_id, name, protocol, \
    method, url, query_params_json, headers_json, auth_json, body_json, network_options_json, \
    assertions_json, description, pre_request_script, test_script, position, path_variables_json, \
    mode, ai_json, agent_touched_ms"
    };
}

/// One request of `workspace_id` (`?1`) by id (`?2`).
pub(super) const SELECT_REQUEST_BY_ID: &str = concat!(
    "SELECT ",
    request_columns!(),
    " FROM requests WHERE workspace_id = ?1 AND id = ?2"
);

/// Every request of `workspace_id` (`?1`) in list order.
pub(super) const LIST_REQUESTS: &str = concat!(
    "SELECT ",
    request_columns!(),
    " FROM requests WHERE workspace_id = ?1 ORDER BY position ASC, created_at ASC, id ASC"
);

pub(super) fn stored_request_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredRequest> {
    Ok(StoredRequest {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        collection_id: row.get(2)?,
        folder_id: row.get(3)?,
        name: row.get(4)?,
        protocol: row.get(5)?,
        method: row.get(6)?,
        url: row.get(7)?,
        query_params_json: row.get(8)?,
        headers_json: row.get(9)?,
        auth_json: row.get(10)?,
        body_json: row.get(11)?,
        network_options_json: row.get(12)?,
        assertions_json: row.get(13)?,
        description: row.get(14)?,
        pre_request_script: row.get(15)?,
        test_script: row.get(16)?,
        position: row.get(17)?,
        path_variables_json: row.get(18)?,
        mode: row.get(19)?,
        ai_json: row.get(20)?,
        agent_touched_ms: row.get(21)?,
    })
}

pub(super) struct StoredRequest {
    id: String,
    workspace_id: String,
    collection_id: Option<String>,
    folder_id: Option<String>,
    name: String,
    protocol: String,
    method: String,
    url: String,
    query_params_json: String,
    path_variables_json: String,
    headers_json: String,
    auth_json: String,
    body_json: String,
    network_options_json: String,
    assertions_json: String,
    description: String,
    pre_request_script: String,
    test_script: String,
    position: i64,
    mode: String,
    ai_json: Option<String>,
    agent_touched_ms: Option<i64>,
}

impl StoredRequest {
    pub(super) fn into_domain(self) -> Result<Request, StorageError> {
        let protocol = protocol_from_storage(&self.id, &self.protocol)?;
        let method = http_method_from_storage(&self.id, &self.method)?;
        let mut request = Request::new(&self.id, &self.workspace_id, &self.name, &self.url)?;

        if let Some(collection_id) = self.collection_id {
            request = request.with_collection(collection_id)?;
        }

        if let Some(folder_id) = self.folder_id {
            request = request.with_folder(folder_id)?;
        }

        request.protocol = protocol;
        request.method = method;
        request.query_params = serde_json::from_str::<Vec<KeyValue>>(&self.query_params_json)?;
        request.path_variables = serde_json::from_str::<Vec<KeyValue>>(&self.path_variables_json)?;
        request.headers = serde_json::from_str::<Vec<KeyValue>>(&self.headers_json)?;
        request.auth = serde_json::from_str::<AuthConfig>(&self.auth_json)?;
        request.body = serde_json::from_str::<RequestBody>(&self.body_json)?;
        let mut network_options =
            serde_json::from_str::<serde_json::Value>(&self.network_options_json)?;
        if let Some(examples) = network_options
            .as_object_mut()
            .and_then(|object| object.remove(super::EXAMPLES_KEY))
        {
            request.examples = serde_json::from_value::<Vec<RequestExample>>(examples)?;
        }
        request.network_options = serde_json::from_value::<NetworkOptions>(network_options)?;
        request.assertions = serde_json::from_str::<Vec<Assertion>>(&self.assertions_json)?;
        request.description = self.description;
        request.pre_request_script = self.pre_request_script;
        request.test_script = self.test_script;
        request.position = self.position;
        request.agent_touched_ms = self.agent_touched_ms;
        request.mode =
            RequestMode::parse(&self.mode).ok_or_else(|| WorkspaceError::InvalidAiRequest {
                reason: format!("request {} has unknown mode {:?}", self.id, self.mode),
            })?;
        if request.mode == RequestMode::AiApi {
            request.ai = self
                .ai_json
                .as_deref()
                .map(serde_json::from_str::<AiRequestConfig>)
                .transpose()?;
        }

        Ok(request)
    }
}
