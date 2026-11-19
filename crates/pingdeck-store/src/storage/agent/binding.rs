//! The PingDeck workspace and environment an agent chat is bound to: reads and writes
//! `agent_sessions.pingdeck_workspace_id` and `pingdeck_environment_id`, the scope the
//! PingDeck-native tools act in whichever workspace the user has open when a call runs.

use rusqlite::{OptionalExtension, params};

use super::super::{StorageError, WorkspaceStore};

/// The PingDeck scope of one chat, fixed when the chat started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PingDeckBinding {
    /// The workspace the chat acts in.
    pub workspace_id: String,
    /// The environment that was active then (the default of `send_request`), if any.
    pub environment_id: Option<String>,
}

impl WorkspaceStore {
    /// Binds the session to a PingDeck workspace and, optionally, its active environment;
    /// `None` clears the binding. Returns whether the session exists.
    pub fn set_session_pingdeck_binding(
        &self,
        session_id: &str,
        binding: Option<&PingDeckBinding>,
    ) -> Result<bool, StorageError> {
        let changed = self.connection.execute(
            "UPDATE agent_sessions
             SET pingdeck_workspace_id = ?2, pingdeck_environment_id = ?3
             WHERE id = ?1",
            params![
                session_id,
                binding.map(|binding| binding.workspace_id.as_str()),
                binding.and_then(|binding| binding.environment_id.as_deref()),
            ],
        )?;

        Ok(changed > 0)
    }

    /// The PingDeck scope of the session; `None` when the session is unbound or unknown.
    pub fn session_pingdeck_binding(
        &self,
        session_id: &str,
    ) -> Result<Option<PingDeckBinding>, StorageError> {
        let row = self
            .connection
            .query_row(
                "SELECT pingdeck_workspace_id, pingdeck_environment_id
                 FROM agent_sessions WHERE id = ?1",
                params![session_id],
                |row| {
                    Ok((
                        row.get::<_, Option<String>>(0)?,
                        row.get::<_, Option<String>>(1)?,
                    ))
                },
            )
            .optional()?;

        Ok(row.and_then(|(workspace_id, environment_id)| {
            workspace_id.map(|workspace_id| PingDeckBinding {
                workspace_id,
                environment_id,
            })
        }))
    }
}
