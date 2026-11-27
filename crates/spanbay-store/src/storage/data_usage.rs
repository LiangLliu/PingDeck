//! Queries behind Settings › Data and sync: how much the stored chats, attachments and captured
//! bodies hold, which chats the retention job may delete and which attachments a clear removes.
//!
//! Read-only except [`WorkspaceStore::forget_attachment_previews`]; attachment rows and blobs are
//! deleted by the caller (`delete_attachment_rows`, the attachment store).

use std::collections::HashSet;

use rusqlite::{Connection, params};

use crate::sqlite::to_u64;
use crate::storage::{StorageError, WorkspaceStore};

/// Chats and the bytes their messages occupy in the database.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct ChatUsage {
    /// Chats that are not deleted.
    pub chats: u64,
    /// Bytes of the stored message content (text, reasoning, tool calls and results).
    pub bytes: u64,
}

/// What the gateway's captured request and response bodies occupy.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct CaptureUsage {
    pub count: u64,
    /// Bytes of the stored captures (the sum of `bytes_total`).
    pub bytes: u64,
}

/// One attachment row a clear would remove the blob of.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ClearableAttachment {
    pub id: String,
    /// Hash of the content (the blob's name).
    pub sha256: String,
    pub size_bytes: u64,
    /// The text of a PDF this row was derived from; such a row goes with its source.
    pub derived: bool,
    /// A message still links to it, so the row stays and the message shows it as removed.
    pub linked: bool,
}

/// The single integer `sql` selects, a negative one read as `0`. Uncached: Settings › Data reads
/// these rarely, and they would only push hot statements out of the cache.
fn count(connection: &Connection, sql: &str) -> Result<u64, StorageError> {
    let value: i64 = connection.query_row(sql, [], |row| row.get(0))?;
    Ok(to_u64(value))
}

impl WorkspaceStore {
    /// Chats that are not deleted and the bytes of every stored message (tombstoned chats still
    /// occupy the database until they are purged, so their messages count).
    pub fn chat_usage(&self) -> Result<ChatUsage, StorageError> {
        Ok(ChatUsage {
            chats: count(
                &self.connection,
                "SELECT COUNT(*) FROM agent_sessions WHERE deleted_at_ms IS NULL",
            )?,
            bytes: count(
                &self.connection,
                "SELECT COALESCE(SUM(LENGTH(CAST(parts_json AS BLOB))), 0) FROM agent_messages",
            )?,
        })
    }

    /// How many gateway captures are stored and the bytes they hold.
    pub fn capture_usage(&self) -> Result<CaptureUsage, StorageError> {
        Ok(CaptureUsage {
            count: count(&self.connection, "SELECT COUNT(*) FROM llm_debug_captures")?,
            bytes: count(
                &self.connection,
                "SELECT COALESCE(SUM(bytes_total), 0) FROM llm_debug_captures",
            )?,
        })
    }

    /// Attachments the user can see: every row except the text derived from a PDF.
    pub fn attachment_count(&self) -> Result<u64, StorageError> {
        count(
            &self.connection,
            "SELECT COUNT(*) FROM agent_attachments WHERE extracted_of IS NULL",
        )
    }

    /// The size of the database as SQLite counts it (pages in use), for a state without a file.
    pub fn database_page_bytes(&self) -> Result<u64, StorageError> {
        let pages = count(&self.connection, "PRAGMA page_count")?;
        let page_size = count(&self.connection, "PRAGMA page_size")?;
        Ok(pages.saturating_mul(page_size))
    }

    /// Chats last used before `cutoff_ms` that retention may delete: not tombstoned, not pinned,
    /// not a sub-agent chat (it goes with its parent) and without a queued, running or
    /// approval-waiting run. "Last used" is the newest message, else the chat's own update time.
    pub fn retention_candidates(&self, cutoff_ms: i64) -> Result<Vec<String>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT s.id FROM agent_sessions s
             WHERE s.deleted_at_ms IS NULL
               AND s.pinned_at_ms IS NULL
               AND s.parent_session_id IS NULL
               AND MAX(s.updated_at_ms,
                       COALESCE((SELECT MAX(m.created_at_ms) FROM agent_messages m
                                 WHERE m.session_id = s.id), 0)) < ?1
               AND NOT EXISTS (SELECT 1 FROM agent_runs r WHERE r.session_id = s.id
                               AND r.status IN ('queued', 'running', 'waiting_approval'))
             ORDER BY s.id",
        )?;
        Ok(statement
            .query_map(params![cutoff_ms], |row| row.get(0))?
            .collect::<Result<_, _>>()?)
    }

    /// The attachments a clear removes the blob of: user attachments created before
    /// `older_than_ms` (all when `None`) and the PDF text derived from each of them. Hashes in
    /// `keep` (blobs the file-change journal also uses) are left out.
    pub fn clearable_attachments(
        &self,
        older_than_ms: Option<i64>,
        keep: &HashSet<String>,
    ) -> Result<Vec<ClearableAttachment>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT a.id, a.sha256, a.size_bytes, a.extracted_of, a.created_at_ms,
                    EXISTS (SELECT 1 FROM agent_message_attachments l WHERE l.attachment_id = a.id)
             FROM agent_attachments a ORDER BY a.created_at_ms, a.id",
        )?;
        let rows = statement
            .query_map([], |row| {
                Ok((
                    row.get::<_, String>(0)?,
                    row.get::<_, String>(1)?,
                    row.get::<_, i64>(2)?,
                    row.get::<_, Option<String>>(3)?,
                    row.get::<_, i64>(4)?,
                    row.get::<_, bool>(5)?,
                ))
            })?
            .collect::<Result<Vec<_>, _>>()?;
        let cleared_sources: HashSet<&str> = rows
            .iter()
            .filter(|(_, _, _, source, created, _)| {
                source.is_none() && older_than_ms.is_none_or(|cutoff| *created < cutoff)
            })
            .map(|(id, ..)| id.as_str())
            .collect();
        Ok(rows
            .iter()
            .filter(|(id, sha, _, source, ..)| {
                !keep.contains(sha)
                    && match source {
                        Some(source) => cleared_sources.contains(source.as_str()),
                        None => cleared_sources.contains(id.as_str()),
                    }
            })
            .map(
                |(id, sha256, size, source, _, linked)| ClearableAttachment {
                    id: id.clone(),
                    sha256: sha256.clone(),
                    size_bytes: to_u64(*size),
                    derived: source.is_some(),
                    linked: *linked,
                },
            )
            .collect())
    }

    /// How many live chats have a message that links one of `attachment_ids`.
    pub fn chats_with_attachments(&self, attachment_ids: &[String]) -> Result<u64, StorageError> {
        if attachment_ids.is_empty() {
            return Ok(0);
        }
        let ids_json = serde_json::to_string(attachment_ids)?;
        let count: i64 = self
            .connection
            .prepare_cached(
                "SELECT COUNT(DISTINCT m.session_id)
                 FROM agent_message_attachments l
                 JOIN agent_messages m ON m.id = l.message_id
                 JOIN agent_sessions s ON s.id = m.session_id
                 WHERE s.deleted_at_ms IS NULL
                   AND l.attachment_id IN (SELECT value FROM json_each(?1))",
            )?
            .query_row(params![ids_json], |row| row.get(0))?;
        Ok(to_u64(count))
    }

    /// Drops the stored thumbnail of the given attachments (their content is gone).
    pub fn forget_attachment_previews(&self, ids: &[String]) -> Result<(), StorageError> {
        self.atomic(|transaction| {
            let mut forget = transaction
                .prepare("UPDATE agent_attachments SET thumb_data_url = NULL WHERE id = ?1")?;
            for id in ids {
                forget.execute(params![id])?;
            }
            Ok(())
        })
    }
}
