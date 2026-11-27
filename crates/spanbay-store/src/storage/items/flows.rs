//! Flow rows; steps and run options are JSON columns.

use rusqlite::{OptionalExtension, params};

use spanbay_model::items::Flow;
use spanbay_model::models::WorkspaceId;

use crate::storage::{StorageError, WorkspaceStore};

/// The `flows` columns [`stored_flow_row`] reads; callers append `WHERE`/`ORDER BY`.
const SELECT_FLOW: &str = "
            SELECT id, workspace_id, name, description, steps_json, updated_at_ms, options_json
            FROM flows";

impl WorkspaceStore {
    /// Upserts a flow, rejecting an id owned by another workspace.
    pub fn save_flow(&self, flow: &Flow) -> Result<(), StorageError> {
        self.atomic(|transaction| {
            self.ensure_item_workspace("flows", "flow", &flow.id, flow.workspace_id.as_str())?;
            transaction.execute(
                "
                INSERT INTO flows (
                    id, workspace_id, name, description, steps_json, updated_at_ms, options_json
                )
                VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)
                ON CONFLICT(id) DO UPDATE SET
                    workspace_id = excluded.workspace_id,
                    name = excluded.name,
                    description = excluded.description,
                    steps_json = excluded.steps_json,
                    updated_at_ms = excluded.updated_at_ms,
                    options_json = excluded.options_json
                ",
                params![
                    flow.id,
                    flow.workspace_id.as_str(),
                    flow.name,
                    flow.description,
                    serde_json::to_string(&flow.steps)?,
                    flow.updated_at_ms,
                    serde_json::to_string(&flow.options)?,
                ],
            )?;
            Ok(())
        })
    }

    /// Lists a workspace's flows ordered by name and id.
    pub fn list_flows(&self, workspace_id: &str) -> Result<Vec<Flow>, StorageError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "{SELECT_FLOW} WHERE workspace_id = ?1 ORDER BY name ASC, id ASC"
        ))?;
        let rows = statement.query_map(params![workspace_id], stored_flow_row)?;

        let mut flows = Vec::new();
        for row in rows {
            flows.push(row?.into_domain()?);
        }
        Ok(flows)
    }

    /// The flow with this id, in whichever workspace owns it.
    pub fn get_flow(&self, id: &str) -> Result<Option<Flow>, StorageError> {
        let stored = self
            .connection
            .query_row(
                &format!("{SELECT_FLOW} WHERE id = ?1"),
                params![id],
                stored_flow_row,
            )
            .optional()?;

        stored.map(StoredFlow::into_domain).transpose()
    }

    /// Deletes a flow scoped to its workspace; returns whether a row was removed.
    pub fn delete_flow(&self, id: &str, workspace_id: &str) -> Result<bool, StorageError> {
        let deleted = self.connection.execute(
            "DELETE FROM flows WHERE id = ?1 AND workspace_id = ?2",
            params![id, workspace_id],
        )?;

        Ok(deleted > 0)
    }
}

struct StoredFlow {
    id: String,
    workspace_id: String,
    name: String,
    description: String,
    steps_json: String,
    updated_at_ms: i64,
    options_json: String,
}

fn stored_flow_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<StoredFlow> {
    Ok(StoredFlow {
        id: row.get(0)?,
        workspace_id: row.get(1)?,
        name: row.get(2)?,
        description: row.get(3)?,
        steps_json: row.get(4)?,
        updated_at_ms: row.get(5)?,
        options_json: row.get(6)?,
    })
}

impl StoredFlow {
    fn into_domain(self) -> Result<Flow, StorageError> {
        Ok(Flow {
            id: self.id,
            workspace_id: WorkspaceId::new(self.workspace_id)?,
            name: self.name,
            description: self.description,
            steps: serde_json::from_str(&self.steps_json)?,
            updated_at_ms: self.updated_at_ms,
            options: serde_json::from_str(&self.options_json)?,
        })
    }
}
