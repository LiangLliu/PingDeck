//! MCP tool definition pins (`agent_mcp_tool_pins`): the fingerprint of each tool of a trusted
//! server as the user accepted it, so a changed definition goes back to asking.

use std::collections::HashMap;

use rusqlite::params;

use super::super::{StorageError, WorkspaceStore};

impl WorkspaceStore {
    /// The pinned fingerprints of `server_id`'s tools, by server-side tool name.
    pub fn mcp_tool_pins(&self, server_id: &str) -> Result<HashMap<String, String>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT tool_name, sha256 FROM agent_mcp_tool_pins WHERE server_id = ?1",
        )?;
        let rows = statement.query_map(params![server_id], |row| Ok((row.get(0)?, row.get(1)?)))?;
        rows.collect::<Result<_, _>>().map_err(Into::into)
    }

    /// Pins (or re-pins) each `(tool name, fingerprint)` of `server_id` in one transaction.
    pub fn pin_mcp_tools(
        &self,
        server_id: &str,
        pins: &[(String, String)],
    ) -> Result<(), StorageError> {
        self.atomic(|transaction| {
            let mut statement = transaction.prepare_cached(
                "INSERT INTO agent_mcp_tool_pins (server_id, tool_name, sha256)
                 VALUES (?1, ?2, ?3)
                 ON CONFLICT(server_id, tool_name) DO UPDATE SET sha256 = excluded.sha256",
            )?;
            for (tool, sha256) in pins {
                statement.execute(params![server_id, tool, sha256])?;
            }
            Ok(())
        })
    }

    /// Forgets every pin of `server_id` (the user stopped trusting it); returns how many.
    pub fn clear_mcp_tool_pins(&self, server_id: &str) -> Result<usize, StorageError> {
        Ok(self.connection.execute(
            "DELETE FROM agent_mcp_tool_pins WHERE server_id = ?1",
            params![server_id],
        )?)
    }
}
