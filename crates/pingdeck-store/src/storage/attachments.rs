//! Attachment rows: the metadata of files attached to agent messages and their links to messages.
//!
//! The bytes live in [`crate::attachments::AttachmentStore`]; here are only names, kinds, the
//! stored thumbnail, the `extracted_of` link of a PDF's derived text and the `agent_message_attachments`
//! links that decide which blobs are still referenced. A derived text attachment has no message
//! link of its own: it stays as long as its source does.

use std::collections::{HashMap, HashSet};

use rusqlite::{Connection, OptionalExtension, Row, params};

use pingdeck_model::agent::{AgentAttachment, AttachmentKind};

use crate::sqlite::{i64_from_usize, to_i64};
use crate::storage::{StorageError, WorkspaceStore};

/// The `agent_attachments` columns (alias `a`) [`raw_from_row`] reads, in its order.
const COLUMNS: &str = "a.id, a.sha256, a.name, a.media_type, a.size_bytes, a.kind, a.width, \
                       a.height, a.created_at_ms, a.thumb_data_url, a.line_count";

fn invalid(column: &'static str, value: impl Into<String>) -> StorageError {
    StorageError::InvalidStoredValue {
        table: "agent_attachments",
        column,
        value: value.into(),
    }
}

fn kind_name(kind: AttachmentKind) -> &'static str {
    match kind {
        AttachmentKind::Image => "image",
        AttachmentKind::Pdf => "pdf",
        AttachmentKind::Text => "text",
    }
}

fn parse_kind(name: &str) -> Result<AttachmentKind, StorageError> {
    match name {
        "image" => Ok(AttachmentKind::Image),
        "pdf" => Ok(AttachmentKind::Pdf),
        "text" => Ok(AttachmentKind::Text),
        other => Err(invalid("kind", other)),
    }
}

/// Raw column values; the checked conversion to the domain type happens after the row borrow.
struct RawAttachment {
    attachment: AgentAttachment,
    kind: String,
    size: i64,
    width: Option<i64>,
    height: Option<i64>,
}

/// Reads the [`COLUMNS`] starting at column `base`.
fn raw_from_row(row: &Row<'_>, base: usize) -> rusqlite::Result<RawAttachment> {
    Ok(RawAttachment {
        attachment: AgentAttachment {
            id: row.get(base)?,
            sha256: row.get(base + 1)?,
            name: row.get(base + 2)?,
            media_type: row.get(base + 3)?,
            size_bytes: 0,
            kind: AttachmentKind::Text,
            width: None,
            height: None,
            thumb_data_url: row.get(base + 9)?,
            removed: false,
            lines: row
                .get::<_, Option<i64>>(base + 10)?
                .and_then(|lines| u32::try_from(lines).ok()),
            created_at_ms: row.get(base + 8)?,
        },
        size: row.get(base + 4)?,
        kind: row.get(base + 5)?,
        width: row.get(base + 6)?,
        height: row.get(base + 7)?,
    })
}

impl RawAttachment {
    fn into_domain(self) -> Result<AgentAttachment, StorageError> {
        let dimension = |column, value: Option<i64>| {
            value
                .map(|value| u32::try_from(value).map_err(|_| invalid(column, value.to_string())))
                .transpose()
        };
        Ok(AgentAttachment {
            size_bytes: u64::try_from(self.size)
                .map_err(|_| invalid("size_bytes", self.size.to_string()))?,
            kind: parse_kind(&self.kind)?,
            width: dimension("width", self.width)?,
            height: dimension("height", self.height)?,
            ..self.attachment
        })
    }
}

impl WorkspaceStore {
    /// Stores an attachment row. Content is unique: when a row with the same `sha256` exists it
    /// wins (keeping its id and first name) and is returned instead; a thumbnail the existing row
    /// lacks is filled in from `attachment`.
    pub fn insert_attachment(
        &self,
        attachment: &AgentAttachment,
    ) -> Result<AgentAttachment, StorageError> {
        self.insert(attachment, None)
    }

    /// Stores the text extracted from the PDF `source_id` as a row linked to it (same dedupe as
    /// [`WorkspaceStore::insert_attachment`]).
    pub fn insert_extracted_attachment(
        &self,
        attachment: &AgentAttachment,
        source_id: &str,
    ) -> Result<AgentAttachment, StorageError> {
        self.insert(attachment, Some(source_id))
    }

    fn insert(
        &self,
        attachment: &AgentAttachment,
        extracted_of: Option<&str>,
    ) -> Result<AgentAttachment, StorageError> {
        self.connection.prepare_cached(
            "INSERT INTO agent_attachments
                 (id, sha256, name, media_type, size_bytes, kind, width, height, created_at_ms,
                  thumb_data_url, extracted_of, line_count)
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12)
             ON CONFLICT(sha256) DO UPDATE SET
                 thumb_data_url = COALESCE(agent_attachments.thumb_data_url, excluded.thumb_data_url)",
        )?
        .execute(params![
                attachment.id,
                attachment.sha256,
                attachment.name,
                attachment.media_type,
                to_i64(attachment.size_bytes),
                kind_name(attachment.kind),
                attachment.width,
                attachment.height,
                attachment.created_at_ms,
                attachment.thumb_data_url,
                extracted_of,
                attachment.lines,
            ])?;
        self.get_attachment_by_sha(&attachment.sha256)?
            .ok_or_else(|| invalid("sha256", attachment.sha256.clone()))
    }

    /// Sets the thumbnail of an attachment; `false` when there is no such id.
    pub fn set_attachment_thumb(
        &self,
        id: &str,
        thumb_data_url: &str,
    ) -> Result<bool, StorageError> {
        Ok(self.connection.execute(
            "UPDATE agent_attachments SET thumb_data_url = ?2 WHERE id = ?1",
            params![id, thumb_data_url],
        )? > 0)
    }

    /// Backdates an attachment row, for tests of age-based clearing.
    #[cfg(any(test, feature = "test-support"))]
    pub fn set_attachment_created_at_ms(
        &self,
        id: &str,
        created_at_ms: i64,
    ) -> Result<(), StorageError> {
        self.connection.execute(
            "UPDATE agent_attachments SET created_at_ms = ?1 WHERE id = ?2",
            params![created_at_ms, id],
        )?;
        Ok(())
    }

    /// The text attachment derived from the PDF `source_id`, if one was made.
    pub fn get_extracted_attachment(
        &self,
        source_id: &str,
    ) -> Result<Option<AgentAttachment>, StorageError> {
        self.query_attachment("a.extracted_of", source_id)
    }

    /// The attachment row with this id.
    pub fn get_attachment(&self, id: &str) -> Result<Option<AgentAttachment>, StorageError> {
        self.query_attachment("a.id", id)
    }

    /// The attachment row holding this content.
    pub(super) fn get_attachment_by_sha(
        &self,
        sha256: &str,
    ) -> Result<Option<AgentAttachment>, StorageError> {
        self.query_attachment("a.sha256", sha256)
    }

    /// The row whose `column` (a literal from this module) equals `value`; three fixed shapes, so
    /// the statement is cached.
    fn query_attachment(
        &self,
        column: &'static str,
        value: &str,
    ) -> Result<Option<AgentAttachment>, StorageError> {
        self.connection
            .prepare_cached(&format!(
                "SELECT {COLUMNS} FROM agent_attachments a WHERE {column} = ?1"
            ))?
            .query_row(params![value], |row| raw_from_row(row, 0))
            .optional()?
            .map(RawAttachment::into_domain)
            .transpose()
    }

    /// Links attachments to a message in the given order (`ord` is the index). Linking the same
    /// pair again is a no-op.
    #[cfg(any(test, feature = "test-support"))]
    pub fn link_message_attachments(
        &self,
        message_id: &str,
        attachment_ids: &[String],
    ) -> Result<(), StorageError> {
        self.atomic(|transaction| link_attachments_on(transaction, message_id, attachment_ids))
    }

    /// The attachments of each given message, in link order; messages without any are absent.
    pub fn list_message_attachments(
        &self,
        message_ids: &[String],
    ) -> Result<HashMap<String, Vec<AgentAttachment>>, StorageError> {
        let mut grouped: HashMap<String, Vec<AgentAttachment>> = HashMap::new();
        if message_ids.is_empty() {
            return Ok(grouped);
        }
        let ids_json = serde_json::to_string(message_ids)?;
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT l.message_id, {COLUMNS}
             FROM agent_message_attachments l
             JOIN agent_attachments a ON a.id = l.attachment_id
             WHERE l.message_id IN (SELECT value FROM json_each(?1))
             ORDER BY l.message_id, l.ord"
        ))?;
        let rows = statement.query_map(params![ids_json], |row| {
            Ok((row.get::<_, String>(0)?, raw_from_row(row, 1)?))
        })?;
        for row in rows {
            let (message_id, raw) = row?;
            grouped
                .entry(message_id)
                .or_default()
                .push(raw.into_domain()?);
        }
        Ok(grouped)
    }

    /// Attachments no message links to (uploaded but never sent, or whose messages are gone). The
    /// text derived from a PDF counts as linked while its source PDF is.
    pub fn list_unreferenced_attachments(&self) -> Result<Vec<AgentAttachment>, StorageError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM agent_attachments a
             WHERE NOT EXISTS (
                 SELECT 1 FROM agent_message_attachments l WHERE l.attachment_id = a.id
             ) AND NOT EXISTS (
                 SELECT 1 FROM agent_message_attachments l WHERE l.attachment_id = a.extracted_of
             )
             ORDER BY a.created_at_ms, a.id"
        ))?;
        let raws = statement
            .query_map([], |row| raw_from_row(row, 0))?
            .collect::<Result<Vec<_>, _>>()?;
        raws.into_iter().map(RawAttachment::into_domain).collect()
    }

    /// Deletes attachment rows by id; returns how many existed. A row a message still links to
    /// is refused by the foreign key.
    pub fn delete_attachment_rows(&self, ids: &[String]) -> Result<usize, StorageError> {
        self.atomic(|transaction| {
            let mut delete = transaction.prepare("DELETE FROM agent_attachments WHERE id = ?1")?;
            let mut deleted = 0;
            for id in ids {
                deleted += delete.execute([id])?;
            }
            Ok(deleted)
        })
    }

    /// Every attachment of the chat's messages on every branch (and the text derived from a PDF
    /// of the chat), oldest first: what `read_attachment` may read.
    pub fn list_session_attachments(
        &self,
        session_id: &str,
    ) -> Result<Vec<AgentAttachment>, StorageError> {
        let mut statement = self.connection.prepare_cached(&format!(
            "SELECT {COLUMNS} FROM agent_attachments a
             WHERE a.id IN (SELECT l.attachment_id FROM agent_message_attachments l
                            JOIN agent_messages m ON m.id = l.message_id
                            WHERE m.session_id = ?1)
                OR a.extracted_of IN (SELECT l.attachment_id FROM agent_message_attachments l
                                      JOIN agent_messages m ON m.id = l.message_id
                                      WHERE m.session_id = ?1)
             ORDER BY a.created_at_ms, a.id"
        ))?;
        let raws = statement
            .query_map(params![session_id], |row| raw_from_row(row, 0))?
            .collect::<Result<Vec<_>, _>>()?;
        raws.into_iter().map(RawAttachment::into_domain).collect()
    }

    /// The content hash of every attachment row: the blobs that must stay on disk.
    pub fn referenced_shas(&self) -> Result<HashSet<String>, StorageError> {
        let mut statement = self
            .connection
            .prepare_cached("SELECT sha256 FROM agent_attachments")?;
        Ok(statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<HashSet<String>, _>>()?)
    }

    /// The hash of every blob the file-change journal references (before and after bytes): they
    /// have no attachment row but must stay on disk.
    pub fn journal_blob_shas(&self) -> Result<HashSet<String>, StorageError> {
        let mut statement = self.connection.prepare_cached(
            "SELECT before_sha256 FROM agent_file_changes WHERE before_sha256 IS NOT NULL
             UNION
             SELECT after_sha256 FROM agent_file_changes WHERE after_sha256 IS NOT NULL",
        )?;
        Ok(statement
            .query_map([], |row| row.get(0))?
            .collect::<Result<HashSet<String>, _>>()?)
    }
}

/// Links `attachment_ids` to `message_id` in order (`ord` is the index) on `connection`, which the
/// caller has made atomic; an existing pair is left as is. No ids, no statement.
pub(in crate::storage) fn link_attachments_on(
    connection: &Connection,
    message_id: &str,
    attachment_ids: &[impl AsRef<str>],
) -> Result<(), StorageError> {
    if attachment_ids.is_empty() {
        return Ok(());
    }
    let mut insert = connection.prepare_cached(
        "INSERT OR IGNORE INTO agent_message_attachments (message_id, attachment_id, ord)
         VALUES (?1, ?2, ?3)",
    )?;
    for (ord, attachment_id) in attachment_ids.iter().enumerate() {
        insert.execute(params![
            message_id,
            attachment_id.as_ref(),
            i64_from_usize(ord)
        ])?;
    }
    Ok(())
}
