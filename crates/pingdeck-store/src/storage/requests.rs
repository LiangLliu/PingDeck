//! Request persistence and placement validation within the workspace hierarchy. The row
//! mapping lives in `row`; the protocol and method codecs here are shared with the tree and
//! history modules.

mod row;

use std::collections::HashSet;

use rusqlite::{OptionalExtension, params};

use pingdeck_base::proxy_url;
use pingdeck_model::ai_request::RequestMode;
use pingdeck_model::models::{
    CollectionId, FolderId, HttpMethod, NetworkOptions, ProtocolKind, Request, RequestId,
    WorkspaceId,
};

use crate::storage::{PlacementFault, StorageError, WorkspaceStore, secrets};
use row::{LIST_REQUESTS, SELECT_REQUEST_BY_ID, StoredRequest, stored_request_from_row};

/// Longest request timeout a save keeps: the upper bound of the `network.timeout_ms` setting.
const MAX_REQUEST_TIMEOUT_MS: u64 = 3_600_000;

/// `options` as stored: a timeout of `0` (reqwest would fire it at once; Postman means "none")
/// becomes the default and one above [`MAX_REQUEST_TIMEOUT_MS`] is clamped (W-21).
fn stored_network_options(options: &NetworkOptions) -> NetworkOptions {
    let timeout_ms = match options.timeout_ms {
        0 => NetworkOptions::default().timeout_ms,
        ms => ms.min(MAX_REQUEST_TIMEOUT_MS),
    };
    NetworkOptions {
        timeout_ms,
        ..options.clone()
    }
}

/// Key of the saved examples inside `requests.network_options_json`: the examples ride in that
/// JSON object so the baseline schema needs no column for them.
pub(super) const EXAMPLES_KEY: &str = "examples";

/// The `network_options_json` text of `request`: its stored network options plus, when it has
/// any, its saved examples under [`EXAMPLES_KEY`].
fn network_options_json(request: &Request) -> Result<String, StorageError> {
    let mut value = serde_json::to_value(stored_network_options(&request.network_options))?;
    if !request.examples.is_empty()
        && let Some(object) = value.as_object_mut()
    {
        object.insert(
            EXAMPLES_KEY.to_owned(),
            serde_json::to_value(&request.examples)?,
        );
    }
    Ok(serde_json::to_string(&value)?)
}

impl WorkspaceStore {
    /// Upserts a request after validating its assertions and collection/folder placement.
    pub fn save_request(&self, request: &Request) -> Result<(), StorageError> {
        request.validate_assertions()?;
        request.validate_examples()?;
        request.validate_ai()?;
        // Stored `ai_json` is `Some` exactly in AI API mode, whatever the caller passed.
        let ai_json = match (request.mode, &request.ai) {
            (RequestMode::AiApi, Some(ai)) => Some(serde_json::to_string(ai)?),
            _ => None,
        };
        self.validate_request_placement(request)?;

        let changed = self.connection.execute(
            "
            INSERT INTO requests (
                id,
                workspace_id,
                collection_id,
                folder_id,
                name,
                protocol,
                method,
                url,
                query_params_json,
                path_variables_json,
                headers_json,
                auth_json,
                body_json,
                network_options_json,
                assertions_json,
                description,
                pre_request_script,
                test_script,
                position,
                mode,
                ai_json,
                updated_at
            )
            VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, ?18, ?19, ?20, ?21, strftime('%Y-%m-%dT%H:%M:%fZ', 'now'))
            ON CONFLICT(id) DO UPDATE SET
                collection_id = excluded.collection_id,
                folder_id = excluded.folder_id,
                name = excluded.name,
                protocol = excluded.protocol,
                method = excluded.method,
                url = excluded.url,
                query_params_json = excluded.query_params_json,
                path_variables_json = excluded.path_variables_json,
                headers_json = excluded.headers_json,
                auth_json = excluded.auth_json,
                body_json = excluded.body_json,
                network_options_json = excluded.network_options_json,
                assertions_json = excluded.assertions_json,
                description = excluded.description,
                pre_request_script = excluded.pre_request_script,
                test_script = excluded.test_script,
                position = excluded.position,
                mode = excluded.mode,
                ai_json = excluded.ai_json,
                updated_at = excluded.updated_at
            WHERE requests.workspace_id = excluded.workspace_id
            ",
            params![
                request.id.as_str(),
                request.workspace_id.as_str(),
                request.collection_id.as_ref().map(CollectionId::as_str),
                request.folder_id.as_ref().map(FolderId::as_str),
                request.name,
                protocol_to_storage(request.protocol),
                http_method_to_storage(request.method),
                request.url,
                serde_json::to_string(&request.query_params)?,
                serde_json::to_string(&request.path_variables)?,
                serde_json::to_string(&request.headers)?,
                serde_json::to_string(&request.auth)?,
                serde_json::to_string(&request.body)?,
                network_options_json(request)?,
                serde_json::to_string(&request.assertions)?,
                request.description,
                request.pre_request_script,
                request.test_script,
                request.position,
                request.mode.as_str(),
                ai_json,
            ],
        )?;
        // An id owned by another workspace matches the conflict but not the WHERE guard.
        if changed == 0 {
            return Err(StorageError::ItemWorkspaceMismatch {
                kind: "request",
                id: request.id.as_str().to_owned(),
            });
        }
        // After the row: a failure here leaves a masked URL without its password, which the
        // caller sees as the error it is.
        self.store_request_proxy_password(request)?;

        Ok(())
    }

    /// Keeps the request's proxy password in the secret store under `request.<id>.proxy_password`
    /// (the row holds only the masked URL): a URL with a new password stores it, one that carries
    /// the mask is untouched and keeps the stored password, anything else clears it.
    fn store_request_proxy_password(&self, request: &Request) -> Result<(), StorageError> {
        let reference = proxy_url::request_secret(request.id.as_str());
        match request
            .network_options
            .proxy_url
            .as_deref()
            .map(proxy_url::split)
        {
            Some(proxy_url::Split {
                password: Some(password),
                ..
            }) => {
                secrets::put(
                    &self.connection,
                    &reference,
                    &password,
                    pingdeck_base::time::now_ms(),
                )?;
            }
            Some(proxy_url::Split {
                is_masked: true, ..
            }) => {}
            _ => {
                secrets::delete(&self.connection, &reference)?;
            }
        }
        Ok(())
    }

    /// Puts the stored proxy password back into a loaded request whose URL carries the mask.
    fn hydrate_request_proxy(&self, request: &mut Request) -> Result<(), StorageError> {
        let Some(url) = request.network_options.proxy_url.as_deref() else {
            return Ok(());
        };
        if !proxy_url::split(url).is_masked {
            return Ok(());
        }
        let reference = proxy_url::request_secret(request.id.as_str());
        if let Some(password) = secrets::get(&self.connection, &reference)? {
            request.network_options.proxy_url = Some(proxy_url::restore(url, &password));
        }
        Ok(())
    }

    fn validate_request_placement(&self, request: &Request) -> Result<(), StorageError> {
        if let Some(collection_id) = request.collection_id.as_ref() {
            let collection_workspace_id = self
                .connection
                .query_row(
                    "SELECT workspace_id FROM collections WHERE id = ?1",
                    params![collection_id.as_str()],
                    |row| row.get::<_, String>(0),
                )
                .optional()?
                .ok_or_else(|| StorageError::InvalidRequestPlacement {
                    reason: format!("collection {} does not exist", collection_id.as_str()),
                    fault: PlacementFault::NotFound,
                })?;

            if collection_workspace_id != request.workspace_id.as_str() {
                return Err(StorageError::InvalidRequestPlacement {
                    reason: format!(
                        "collection {} belongs to workspace {collection_workspace_id}",
                        collection_id.as_str()
                    ),
                    fault: PlacementFault::Conflict,
                });
            }
        }

        let Some(folder_id) = request.folder_id.as_ref() else {
            return Ok(());
        };

        let (folder_collection_id, folder_workspace_id) = self
            .connection
            .query_row(
                "
                SELECT folders.collection_id, collections.workspace_id
                FROM folders
                JOIN collections ON collections.id = folders.collection_id
                WHERE folders.id = ?1
                ",
                params![folder_id.as_str()],
                |row| Ok((row.get::<_, String>(0)?, row.get::<_, String>(1)?)),
            )
            .optional()?
            .ok_or_else(|| StorageError::InvalidRequestPlacement {
                reason: format!("folder {} does not exist", folder_id.as_str()),
                fault: PlacementFault::NotFound,
            })?;

        match request.collection_id.as_ref() {
            Some(collection_id) if folder_collection_id != collection_id.as_str() => {
                Err(StorageError::InvalidRequestPlacement {
                    reason: format!(
                        "folder {} belongs to collection {folder_collection_id}",
                        folder_id.as_str()
                    ),
                    fault: PlacementFault::Conflict,
                })
            }
            None if folder_workspace_id != request.workspace_id.as_str() => {
                Err(StorageError::InvalidRequestPlacement {
                    reason: format!(
                        "folder {} belongs to workspace {folder_workspace_id}",
                        folder_id.as_str()
                    ),
                    fault: PlacementFault::Conflict,
                })
            }
            _ => Ok(()),
        }
    }

    /// Loads one request of a workspace with its proxy password, or `None` when it does not
    /// exist there. Cached: every send and move reads a request.
    pub fn get_request(
        &self,
        workspace_id: &WorkspaceId,
        id: &RequestId,
    ) -> Result<Option<Request>, StorageError> {
        let row = self
            .connection
            .prepare_cached(SELECT_REQUEST_BY_ID)?
            .query_row(
                params![workspace_id.as_str(), id.as_str()],
                stored_request_from_row,
            )
            .optional()?;

        let mut request = row.map(StoredRequest::into_domain).transpose()?;
        if let Some(request) = request.as_mut() {
            self.hydrate_request_proxy(request)?;
        }
        Ok(request)
    }

    /// Lists a workspace's requests ordered by position, creation time, and id.
    pub fn list_requests(&self, workspace_id: &str) -> Result<Vec<Request>, StorageError> {
        let mut statement = self.connection.prepare_cached(LIST_REQUESTS)?;
        let rows = statement.query_map(params![workspace_id], stored_request_from_row)?;

        let mut requests = Vec::new();
        for row in rows {
            let mut request = row?.into_domain()?;
            self.hydrate_request_proxy(&mut request)?;
            requests.push(request);
        }

        Ok(requests)
    }

    /// Moves a workspace's request to a collection/folder, appending after its siblings by
    /// default; `None` when the request does not exist in that workspace.
    pub fn move_request(
        &self,
        workspace_id: &WorkspaceId,
        id: &RequestId,
        target_collection_id: Option<&CollectionId>,
        target_folder_id: Option<&FolderId>,
        position: Option<i64>,
    ) -> Result<Option<Request>, StorageError> {
        let Some(mut request) = self.get_request(workspace_id, id)? else {
            return Ok(None);
        };

        request.collection_id = target_collection_id.cloned();
        request.folder_id = target_folder_id.cloned();
        self.validate_request_placement(&request)?;

        let position = match position {
            Some(position) => position,
            None => {
                let sibling_max = self.connection.query_row(
                    "
                    SELECT COALESCE(MAX(position), 0)
                    FROM requests
                    WHERE workspace_id = ?1
                      AND collection_id IS ?2
                      AND folder_id IS ?3
                    ",
                    params![
                        request.workspace_id.as_str(),
                        target_collection_id.map(CollectionId::as_str),
                        target_folder_id.map(FolderId::as_str),
                    ],
                    |row| row.get::<_, i64>(0),
                )?;
                sibling_max.saturating_add(1)
            }
        };

        self.connection.execute(
            "
            UPDATE requests
            SET collection_id = ?2, folder_id = ?3, position = ?4,
                updated_at = strftime('%Y-%m-%dT%H:%M:%fZ', 'now')
            WHERE workspace_id = ?5 AND id = ?1
            ",
            params![
                request.id.as_str(),
                target_collection_id.map(CollectionId::as_str),
                target_folder_id.map(FolderId::as_str),
                position,
                request.workspace_id.as_str(),
            ],
        )?;

        self.get_request(&request.workspace_id, id)
    }

    /// Deletes a workspace's request by id. Returns whether a row was removed; its proxy
    /// password leaves the secret store with it.
    pub fn delete_request(
        &self,
        workspace_id: &WorkspaceId,
        id: &RequestId,
    ) -> Result<bool, StorageError> {
        let affected_rows = self.connection.execute(
            "DELETE FROM requests WHERE workspace_id = ?1 AND id = ?2",
            params![workspace_id.as_str(), id.as_str()],
        )?;
        if affected_rows > 0 {
            secrets::delete(&self.connection, &proxy_url::request_secret(id.as_str()))?;
        }

        Ok(affected_rows > 0)
    }

    /// Stamps a saved request as created or edited by the agent at `now_ms`: it declares no host
    /// for secrets until the user sends it from the UI. Returns whether the request exists.
    pub fn mark_request_agent_touched(
        &self,
        workspace_id: &str,
        request_id: &str,
        now_ms: i64,
    ) -> Result<bool, StorageError> {
        let changed = self.connection.execute(
            "UPDATE requests SET agent_touched_ms = ?3 WHERE workspace_id = ?1 AND id = ?2",
            params![workspace_id, request_id, now_ms],
        )?;

        Ok(changed > 0)
    }

    /// Clears the agent marker of a request the user sent from the UI. Returns whether a marker
    /// was cleared.
    pub fn clear_request_agent_touched(
        &self,
        workspace_id: &str,
        request_id: &str,
    ) -> Result<bool, StorageError> {
        let changed = self.connection.execute(
            "UPDATE requests SET agent_touched_ms = NULL
             WHERE workspace_id = ?1 AND id = ?2 AND agent_touched_ms IS NOT NULL",
            params![workspace_id, request_id],
        )?;

        Ok(changed > 0)
    }

    /// The ids of the workspace's saved requests the agent created or edited and the user has
    /// not sent from the UI since: the `untrusted_request_ids` of the declared-host rules.
    pub fn agent_touched_request_ids(
        &self,
        workspace_id: &str,
    ) -> Result<HashSet<String>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id FROM requests WHERE workspace_id = ?1 AND agent_touched_ms IS NOT NULL",
        )?;
        let ids = statement
            .query_map(params![workspace_id], |row| row.get::<_, String>(0))?
            .collect::<Result<HashSet<_>, _>>()?;

        Ok(ids)
    }
}

fn protocol_to_storage(protocol: ProtocolKind) -> &'static str {
    match protocol {
        ProtocolKind::HttpRest => "http_rest",
        ProtocolKind::Graphql => "graphql",
        ProtocolKind::WebSocket => "websocket",
        ProtocolKind::Grpc => "grpc",
    }
}

pub(super) fn protocol_from_storage(
    request_id: &str,
    protocol: &str,
) -> Result<ProtocolKind, StorageError> {
    match protocol {
        "http_rest" => Ok(ProtocolKind::HttpRest),
        "graphql" => Ok(ProtocolKind::Graphql),
        "websocket" => Ok(ProtocolKind::WebSocket),
        "grpc" => Ok(ProtocolKind::Grpc),
        _ => Err(StorageError::InvalidProtocol {
            request_id: request_id.to_owned(),
            protocol: protocol.to_owned(),
        }),
    }
}

pub(super) fn http_method_to_storage(method: HttpMethod) -> &'static str {
    match method {
        HttpMethod::Get => "GET",
        HttpMethod::Post => "POST",
        HttpMethod::Put => "PUT",
        HttpMethod::Patch => "PATCH",
        HttpMethod::Delete => "DELETE",
        HttpMethod::Head => "HEAD",
        HttpMethod::Options => "OPTIONS",
    }
}

pub(super) fn http_method_from_storage(
    request_id: &str,
    method: &str,
) -> Result<HttpMethod, StorageError> {
    match method {
        "GET" => Ok(HttpMethod::Get),
        "POST" => Ok(HttpMethod::Post),
        "PUT" => Ok(HttpMethod::Put),
        "PATCH" => Ok(HttpMethod::Patch),
        "DELETE" => Ok(HttpMethod::Delete),
        "HEAD" => Ok(HttpMethod::Head),
        "OPTIONS" => Ok(HttpMethod::Options),
        _ => Err(StorageError::InvalidHttpMethod {
            request_id: request_id.to_owned(),
            method: method.to_owned(),
        }),
    }
}
