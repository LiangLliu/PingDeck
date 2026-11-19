//! Folder trust (`agent_folder_trust`): which folders may have their `AGENTS.md`, `CLAUDE.md`
//! and folder skills loaded into a chat.
//!
//! The key is the canonical path the caller passes (the service canonicalizes, so a symlink and
//! its target share one answer); nothing here walks the file system. A row exists once the user
//! answered the prompt; revoking deletes it.

use rusqlite::{OptionalExtension, params};

use pingdeck_model::agent::AgentFolderTrust;

use super::super::{StorageError, WorkspaceStore};

/// Refuses a path that cannot be a canonical folder: an empty one or one holding a NUL byte (both
/// with the same message).
fn check_path(path: &str) -> Result<(), StorageError> {
    if path.is_empty() || path.contains('\0') {
        return Err(super::invalid_data("a folder path must not be empty"));
    }
    Ok(())
}

fn trust_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<AgentFolderTrust> {
    Ok(AgentFolderTrust {
        path: row.get(0)?,
        trusted: row.get(1)?,
        instructions_sha256: row.get(2)?,
        decided_at_ms: row.get(3)?,
    })
}

impl WorkspaceStore {
    /// The stored answer for `canonical_path`, or `None` when the user was never asked.
    pub fn folder_trust_get(
        &self,
        canonical_path: &str,
    ) -> Result<Option<AgentFolderTrust>, StorageError> {
        Ok(self
            .connection
            .query_row(
                "SELECT path, trusted, instructions_sha256, decided_at_ms
                 FROM agent_folder_trust WHERE path = ?1",
                params![canonical_path],
                trust_from_row,
            )
            .optional()?)
    }

    /// Records the answer for `canonical_path`; a later call replaces it (trusting again after a
    /// change stores the new instructions hash).
    pub fn folder_trust_set(
        &self,
        canonical_path: &str,
        trusted: bool,
        instructions_sha256: Option<&str>,
        now_ms: i64,
    ) -> Result<AgentFolderTrust, StorageError> {
        check_path(canonical_path)?;
        self.connection.execute(
            "INSERT INTO agent_folder_trust (path, trusted, instructions_sha256, decided_at_ms)
             VALUES (?1, ?2, ?3, ?4)
             ON CONFLICT(path) DO UPDATE SET
                 trusted = excluded.trusted,
                 instructions_sha256 = excluded.instructions_sha256,
                 decided_at_ms = excluded.decided_at_ms",
            params![canonical_path, trusted, instructions_sha256, now_ms],
        )?;
        Ok(AgentFolderTrust {
            path: canonical_path.to_owned(),
            trusted,
            instructions_sha256: instructions_sha256.map(str::to_owned),
            decided_at_ms: now_ms,
        })
    }

    /// Forgets the answer for `canonical_path` (Settings "Revoke"); returns whether there was one.
    pub fn folder_trust_revoke(&self, canonical_path: &str) -> Result<bool, StorageError> {
        Ok(self.connection.execute(
            "DELETE FROM agent_folder_trust WHERE path = ?1",
            params![canonical_path],
        )? > 0)
    }

    /// Every stored answer, the trusted folders first and then by path (the Settings list).
    pub fn folder_trust_list(&self) -> Result<Vec<AgentFolderTrust>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT path, trusted, instructions_sha256, decided_at_ms
             FROM agent_folder_trust ORDER BY trusted DESC, path",
        )?;
        let rows = statement.query_map([], trust_from_row)?;
        Ok(rows.collect::<Result<_, _>>()?)
    }

    /// Whether the user trusted `canonical_path` (the folder itself, not a parent).
    pub fn is_folder_trusted(&self, canonical_path: &str) -> Result<bool, StorageError> {
        Ok(self
            .folder_trust_get(canonical_path)?
            .is_some_and(|trust| trust.trusted))
    }
}
