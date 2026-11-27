//! The SpanBay workspace and environment an agent chat is bound to: reads and writes
//! `agent_sessions.spanbay_workspace_id` and `spanbay_environment_id`, the scope the
//! SpanBay-native tools act in whichever workspace the user has open when a call runs.

use rusqlite::{OptionalExtension, params};

use super::super::{StorageError, WorkspaceStore};

/// The SpanBay scope of one chat, fixed when the chat started.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SpanBayBinding {
    /// The workspace the chat acts in.
    pub workspace_id: String,
    /// The environment that was active then (the default of `send_request`), if any.
    pub environment_id: Option<String>,
}

impl WorkspaceStore {
    /// Binds the session to a SpanBay workspace and, optionally, its active environment;
    /// `None` clears the binding. Returns whether the session exists.
    pub fn set_session_spanbay_binding(
        &self,
        session_id: &str,
        binding: Option<&SpanBayBinding>,
    ) -> Result<bool, StorageError> {
        let changed = self.connection.execute(
            "UPDATE agent_sessions
             SET spanbay_workspace_id = ?2, spanbay_environment_id = ?3
             WHERE id = ?1",
            params![
                session_id,
                binding.map(|binding| binding.workspace_id.as_str()),
                binding.and_then(|binding| binding.environment_id.as_deref()),
            ],
        )?;

        Ok(changed > 0)
    }

    /// The SpanBay scope of the session; `None` when the session is unbound or unknown.
    pub fn session_spanbay_binding(
        &self,
        session_id: &str,
    ) -> Result<Option<SpanBayBinding>, StorageError> {
        let row = self
            .connection
            .query_row(
                "SELECT spanbay_workspace_id, spanbay_environment_id
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
            workspace_id.map(|workspace_id| SpanBayBinding {
                workspace_id,
                environment_id,
            })
        }))
    }
}
