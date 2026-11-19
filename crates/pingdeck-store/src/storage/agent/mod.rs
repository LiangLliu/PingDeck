//! Agent harness persistence: sessions and their PingDeck binding, the message tree, search, the
//! queue, runs and the run board, the tool-call audit, the file-change journal, tool policies,
//! permission rules, folder trust, MCP servers and their tool pins, skills.
//!
//! One submodule per table family, each an `impl WorkspaceStore` block; this file holds the small
//! tables (MCP servers, skill switches) and the helpers the submodules share.

mod binding;
mod board;
mod calls;
mod messages;
mod pins;
mod queue;
mod rules;
mod runs;
mod search;
mod sessions;
mod tree;
mod trust;

use std::collections::HashMap;

use rusqlite::{Connection, OptionalExtension, params};
use serde::Serialize;
use serde::de::DeserializeOwned;

use pingdeck_model::agent::AgentMcpServer;
use pingdeck_model::agent::mcp::McpTransportConfig;

use super::{StorageError, WorkspaceStore};
use crate::sqlite::to_i64;

pub use binding::PingDeckBinding;
pub use board::{BoardRunIds, PendingChanges, RunExtras, UnseenRun};

/// The review baseline of an MCP server that arrived through sync or import and was not saved by
/// the user yet.
pub const MCP_REVIEW_PENDING: &str = "pending";

/// A stored value that parses to nothing known.
fn invalid(column: &'static str, value: impl Into<String>) -> StorageError {
    StorageError::InvalidStoredValue {
        table: "agent",
        column,
        value: value.into(),
    }
}

/// Arguments the store refuses to write.
fn invalid_data(reason: impl Into<String>) -> StorageError {
    StorageError::InvalidAgentData {
        reason: reason.into(),
    }
}

/// The plain-text form of a fieldless serde enum (`"auto_read_only"`), as stored in a column.
///
/// Serialization can only fail for a variant that carries data; every enum stored this way is
/// fieldless, so the `InvalidAgentData` error (rather than `StorageError::Json`) is unreachable.
fn enum_text<T: Serialize>(value: &T) -> Result<String, StorageError> {
    crate::sqlite::enum_variant_name(value).map_err(invalid_data)
}

/// Parses [`enum_text`].
fn parse_enum<T: DeserializeOwned>(column: &'static str, text: &str) -> Result<T, StorageError> {
    crate::sqlite::enum_from_text(text).ok_or_else(|| invalid(column, text))
}

/// Fails with [`StorageError::InvalidAgentData`] unless chat session `id` exists (tombstoned or
/// not).
fn require_session(connection: &Connection, id: &str) -> Result<(), StorageError> {
    let exists: bool = connection.query_row(
        "SELECT EXISTS (SELECT 1 FROM agent_sessions WHERE id = ?1)",
        params![id],
        |row| row.get(0),
    )?;
    if !exists {
        return Err(invalid_data(format!("chat session {id} not found")));
    }
    Ok(())
}

impl WorkspaceStore {
    /// Ordered by name, then id.
    pub fn list_agent_mcp_servers(&self) -> Result<Vec<AgentMcpServer>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT id, name, transport_json, enabled, trusted, updated_at_ms
             FROM agent_mcp_servers ORDER BY name, id",
        )?;
        let rows = statement.query_map([], |row| {
            Ok((
                row.get::<_, String>(0)?,
                row.get::<_, String>(1)?,
                row.get::<_, String>(2)?,
                row.get::<_, bool>(3)?,
                row.get::<_, bool>(4)?,
                row.get::<_, i64>(5)?,
            ))
        })?;
        let mut servers = Vec::new();
        for row in rows {
            let (id, name, transport_json, enabled, trusted, updated_at_ms) = row?;
            servers.push(AgentMcpServer {
                id,
                name,
                transport: serde_json::from_str::<McpTransportConfig>(&transport_json)?,
                enabled,
                trusted,
                updated_at_ms,
            });
        }
        Ok(servers)
    }

    /// The configuration fingerprint the user last saved `id` with, [`MCP_REVIEW_PENDING`] for a
    /// server that arrived through sync or import, `None` when none was recorded yet.
    pub fn mcp_review_baseline(&self, id: &str) -> Result<Option<String>, StorageError> {
        Ok(self
            .connection
            .query_row(
                "SELECT review_fingerprint FROM agent_mcp_servers WHERE id = ?1",
                params![id],
                |row| row.get::<_, Option<String>>(0),
            )
            .optional()?
            .flatten())
    }

    /// Records the fingerprint of the configuration the user reviewed (saved), or
    /// [`MCP_REVIEW_PENDING`]; returns whether the server exists.
    pub fn set_mcp_review_baseline(
        &self,
        id: &str,
        fingerprint: &str,
    ) -> Result<bool, StorageError> {
        Ok(self.connection.execute(
            "UPDATE agent_mcp_servers SET review_fingerprint = ?2 WHERE id = ?1",
            params![id, fingerprint],
        )? > 0)
    }

    pub fn save_agent_mcp_server(&self, server: &AgentMcpServer) -> Result<(), StorageError> {
        self.connection.execute(
            "
            INSERT INTO agent_mcp_servers (id, name, transport_json, enabled, trusted, updated_at_ms)
            VALUES (?1, ?2, ?3, ?4, ?5, ?6)
            ON CONFLICT(id) DO UPDATE SET
                name = excluded.name,
                transport_json = excluded.transport_json,
                enabled = excluded.enabled,
                trusted = excluded.trusted,
                updated_at_ms = excluded.updated_at_ms
            ",
            params![
                server.id,
                server.name,
                serde_json::to_string(&server.transport)?,
                server.enabled,
                server.trusted,
                server.updated_at_ms,
            ],
        )?;
        Ok(())
    }

    /// Deletes an MCP server; returns whether it existed.
    pub fn delete_agent_mcp_server(&self, id: &str) -> Result<bool, StorageError> {
        Ok(self
            .connection
            .execute("DELETE FROM agent_mcp_servers WHERE id = ?1", params![id])?
            > 0)
    }

    /// Skill names the user switched on or off (skills not listed are enabled).
    pub fn list_agent_skill_flags(&self) -> Result<HashMap<String, bool>, StorageError> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT name, enabled FROM agent_skills")?;
        let rows = statement.query_map([], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    pub fn set_agent_skill_enabled(&self, name: &str, enabled: bool) -> Result<(), StorageError> {
        self.connection.execute(
            "INSERT INTO agent_skills (name, enabled) VALUES (?1, ?2)
             ON CONFLICT(name) DO UPDATE SET enabled = excluded.enabled",
            params![name, enabled],
        )?;
        Ok(())
    }
}
