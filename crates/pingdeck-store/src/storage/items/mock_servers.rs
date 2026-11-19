//! Mock server rows; routes, headers, data buckets and callbacks are JSON columns.

use rusqlite::{OptionalExtension, params};

use pingdeck_model::items::MockServer;
use pingdeck_model::models::WorkspaceId;

use super::stored_u64;
use crate::sqlite::to_i64;
use crate::storage::{StorageError, WorkspaceStore};

/// The `mock_servers` columns [`stored_mock_server_row`] reads; callers append `WHERE`/`ORDER BY`.
const SELECT_MOCK_SERVER: &str = "
            SELECT id, workspace_id, name, port, endpoint_prefix, latency_ms, cors_enabled,
                   headers_json, routes_json, data_buckets_json, callbacks_json, proxy_enabled,
                   proxy_base_url, proxy_remove_prefix, proxy_req_headers_json,
                   proxy_res_headers_json, updated_at_ms, environment_id
            FROM mock_servers";

impl WorkspaceStore {
    /// Upserts a mock server, rejecting an id owned by another workspace.
    pub fn save_mock_server(&self, server: &MockServer) -> Result<(), StorageError> {
        self.atomic(|transaction| {
            self.ensure_item_workspace(
                "mock_servers",
                "mock server",
                &server.id,
                server.workspace_id.as_str(),
            )?;
            transaction.execute(
                "
                INSERT INTO mock_servers (
                    id, workspace_id, name, port, endpoint_prefix, latency_ms, cors_enabled,
                    headers_json, routes_json, data_buckets_json, callbacks_json, proxy_enabled,
                    proxy_base_url, proxy_remove_prefix, proxy_req_headers_json,
                    proxy_res_headers_json, updated_at_ms, environment_id
                )
                VALUES (
                    ?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17,
                    (SELECT id FROM environments WHERE id = ?18 AND workspace_id = ?2)
                )
                ON CONFLICT(id) DO UPDATE SET
                    workspace_id = excluded.workspace_id,
                    name = excluded.name,
                    port = excluded.port,
                    endpoint_prefix = excluded.endpoint_prefix,
                    latency_ms = excluded.latency_ms,
                    cors_enabled = excluded.cors_enabled,
                    headers_json = excluded.headers_json,
                    routes_json = excluded.routes_json,
                    data_buckets_json = excluded.data_buckets_json,
                    callbacks_json = excluded.callbacks_json,
                    proxy_enabled = excluded.proxy_enabled,
                    proxy_base_url = excluded.proxy_base_url,
                    proxy_remove_prefix = excluded.proxy_remove_prefix,
                    proxy_req_headers_json = excluded.proxy_req_headers_json,
                    proxy_res_headers_json = excluded.proxy_res_headers_json,
                    updated_at_ms = excluded.updated_at_ms,
                    environment_id = excluded.environment_id
                ",
                params![
                    server.id,
                    server.workspace_id.as_str(),
                    server.name,
                    i64::from(server.port),
                    server.endpoint_prefix,
                    to_i64(server.latency_ms),
                    server.cors_enabled,
                    serde_json::to_string(&server.headers)?,
                    serde_json::to_string(&server.routes)?,
                    serde_json::to_string(&server.data_buckets)?,
                    serde_json::to_string(&server.callbacks)?,
                    server.proxy_enabled,
                    server.proxy_base_url,
                    server.proxy_remove_prefix,
                    serde_json::to_string(&server.proxy_req_headers)?,
                    serde_json::to_string(&server.proxy_res_headers)?,
                    server.updated_at_ms,
                    server.environment_id,
                ],
            )?;
            Ok(())
        })
    }

    /// Ports of every saved mock server across all workspaces (for free-port picking).
    pub fn list_mock_server_ports(&self) -> Result<Vec<u16>, StorageError> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT port FROM mock_servers")?;
        let rows = statement.query_map([], |row| row.get::<_, u16>(0))?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Lists a workspace's mock servers ordered by name and id.
    pub fn list_mock_servers(&self, workspace_id: &str) -> Result<Vec<MockServer>, StorageError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "{SELECT_MOCK_SERVER} WHERE workspace_id = ?1 ORDER BY name ASC, id ASC"
        ))?;
        let rows = statement.query_map(params![workspace_id], stored_mock_server_row)?;

        let mut servers = Vec::new();
        for row in rows {
            servers.push(row?.into_domain()?);
        }
        Ok(servers)
    }

    /// The mock server with this id, in whichever workspace owns it.
    pub fn get_mock_server(&self, id: &str) -> Result<Option<MockServer>, StorageError> {
        let stored = self
            .connection
            .query_row(
                &format!("{SELECT_MOCK_SERVER} WHERE id = ?1"),
                params![id],
                stored_mock_server_row,
            )
            .optional()?;

        stored.map(StoredMockServer::into_domain).transpose()
    }

    /// Deletes a mock server scoped to its workspace; returns whether a row was removed.
    pub fn delete_mock_server(&self, id: &str, workspace_id: &str) -> Result<bool, StorageError> {
        let deleted = self.connection.execute(
            "DELETE FROM mock_servers WHERE id = ?1 AND workspace_id = ?2",
            params![id, workspace_id],
        )?;

        Ok(deleted > 0)
    }
}

struct StoredMockServer {
    id: String,
    workspace_id: String,
    name: String,
    port: u16,
    endpoint_prefix: String,
    latency_ms: i64,
    cors_enabled: bool,
    headers_json: String,
    routes_json: String,
    data_buckets_json: String,
    callbacks_json: String,
    proxy_enabled: bool,
    proxy_base_url: String,
    proxy_remove_prefix: bool,
    proxy_req_headers_json: String,
    proxy_res_headers_json: String,
    updated_at_ms: i64,
    environment_id: Option<String>,
}

fn stored_mock_server_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredMockServer> {
    Ok(StoredMockServer {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        name: row.get(2)?,
        port: row.get(3)?,
        endpoint_prefix: row.get(4)?,
        latency_ms: row.get(5)?,
        cors_enabled: row.get(6)?,
        headers_json: row.get(7)?,
        routes_json: row.get(8)?,
        data_buckets_json: row.get(9)?,
        callbacks_json: row.get(10)?,
        proxy_enabled: row.get(11)?,
        proxy_base_url: row.get(12)?,
        proxy_remove_prefix: row.get(13)?,
        proxy_req_headers_json: row.get(14)?,
        proxy_res_headers_json: row.get(15)?,
        updated_at_ms: row.get(16)?,
        environment_id: row.get(17)?,
    })
}

impl StoredMockServer {
    fn into_domain(self) -> Result<MockServer, StorageError> {
        Ok(MockServer {
            id: self.id,
            workspace_id: WorkspaceId::new(self.workspace_id)?,
            name: self.name,
            port: self.port,
            endpoint_prefix: self.endpoint_prefix,
            latency_ms: stored_u64("mock_servers", "latency_ms", self.latency_ms)?,
            cors_enabled: self.cors_enabled,
            headers: serde_json::from_str(&self.headers_json)?,
            routes: serde_json::from_str(&self.routes_json)?,
            data_buckets: serde_json::from_str(&self.data_buckets_json)?,
            callbacks: serde_json::from_str(&self.callbacks_json)?,
            proxy_enabled: self.proxy_enabled,
            proxy_base_url: self.proxy_base_url,
            proxy_remove_prefix: self.proxy_remove_prefix,
            proxy_req_headers: serde_json::from_str(&self.proxy_req_headers_json)?,
            proxy_res_headers: serde_json::from_str(&self.proxy_res_headers_json)?,
            updated_at_ms: self.updated_at_ms,
            environment_id: self.environment_id,
        })
    }
}
