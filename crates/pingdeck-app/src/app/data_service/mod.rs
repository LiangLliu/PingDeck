//! Settings › Data and sync: storage sizes, chat retention, clearing attachments and captured
//! bodies, compacting the database and the in-app backup.
//!
//! Single responsibility: connect the size and age queries of the store to the files under the
//! data folder. The store lock is never held across file IO; deleting an attachment blob takes
//! the same lock as storing one, so a clear never removes the blob of a file being attached.
//!
//! Clearing attachments removes the blob and the thumbnail and keeps the row and the message
//! links: a message that showed the file stays valid, the model sees
//! `[attachment unavailable: <name>]` for it and `agent_attachment_get_bytes` answers
//! `NotFound`. The UI shows the "Attachment removed" chip for such a row; a stored flag on
//! `AgentAttachment` would need a schema change (see `docs/product.md` §7, `docs/decisions.md` §12 D-228).

use std::path::Path;
use std::sync::{Arc, Weak};
use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::error::AppError;

use pingdeck_store::attachments::blobs_lock;

use super::{AppState, AppStateInner, command_error, current_time_ms};

#[cfg(test)]
mod tests;

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Longest "older than" age accepted, in days (ten years).
const MAX_DAYS: u32 = 3650;

/// How often the retention job looks for chats to delete.
const RETENTION_INTERVAL: Duration = Duration::from_secs(24 * 60 * 60);

/// The sizes of the data folder's parts, for the Storage block. The database size
/// includes the chats and the captured bodies it holds.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct StorageUsageDto {
    /// The database file and its write-ahead log.
    pub database_bytes: u64,
    /// Free pages inside the database file: what "Compact database" gives back to the disk.
    /// Clears delete rows but leave the file at its size until it is compacted.
    #[serde(default)]
    pub database_free_bytes: u64,
    /// Chat text and tool results (inside the database) and spilled tool output (files).
    pub chats_bytes: u64,
    /// Chats that are not deleted.
    pub chats_count: u64,
    /// The attachment blobs.
    pub attachments_bytes: u64,
    /// Attachments the user can see (the text derived from a PDF is not counted).
    pub attachments_count: u64,
    /// The gateway's captured request and response bodies (inside the database).
    pub captured_bodies_bytes: u64,
    /// Stored captures.
    pub captured_bodies_count: u64,
    /// The diagnostic log files.
    pub logs_bytes: u64,
}

/// How many chats the chat-retention setting would delete.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ChatRetentionPreviewDto {
    /// Chats last used more than the given days ago that are not pinned and have no running or
    /// waiting run.
    pub chats: u64,
}

/// What a clear removed, or with `dry_run` would remove. Named apart from the gateway
/// log's `ClearReportDto`, which counts log rows.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataClearReportDto {
    /// Attachments or captures removed.
    pub removed: u64,
    /// Bytes given back.
    pub freed_bytes: u64,
    /// Chats with a message that showed a removed attachment.
    pub chats: u64,
}

/// What [`data_backup_impl`] wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataBackupDto {
    /// The copy of the database.
    pub path: String,
    /// The copy of the plaintext secrets vault next to it (owner-only); `None` without a vault.
    pub secrets_path: Option<String>,
    /// Bytes of both copies.
    pub bytes: u64,
}

/// What [`data_compact_impl`] changed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DataCompactDto {
    /// The database file and its write-ahead log before.
    pub before_bytes: u64,
    /// The same after.
    pub after_bytes: u64,
}

/// What [`clear_attachments_impl`] removes.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ClearAttachmentsOptions {
    /// Only attachments stored more than this many days ago; `None` clears all of them.
    pub older_than_days: Option<u32>,
    /// Delete nothing and report what would go.
    pub dry_run: bool,
}

/// The total size of the regular files below `path` (0 when it does not exist); symbolic links
/// are not followed.
fn dir_bytes(path: &Path) -> u64 {
    let Ok(entries) = std::fs::read_dir(path) else {
        return 0;
    };
    entries
        .flatten()
        .map(|entry| match entry.file_type() {
            Ok(kind) if kind.is_dir() => dir_bytes(&entry.path()),
            Ok(kind) if kind.is_file() => entry.metadata().map_or(0, |meta| meta.len()),
            _ => 0,
        })
        .sum()
}

fn file_bytes(path: &Path) -> u64 {
    std::fs::metadata(path).map_or(0, |meta| meta.len())
}

/// The database file plus its write-ahead log.
fn database_file_bytes(db: &Path) -> u64 {
    let mut wal = db.as_os_str().to_owned();
    wal.push("-wal");
    file_bytes(db) + file_bytes(Path::new(&wal))
}

/// Writes a consistent copy of the database and its secrets vault to `destination` (and
/// `<destination>.secrets`), on a pooled connection so the app keeps working meanwhile; the copy
/// replaces an existing file only once complete. Blocking. The vault copy holds the secrets in
/// plaintext like the vault itself and is created owner-only. A destination inside the data
/// folder is refused: the final rename could replace the live database, its vault, a write-ahead
/// log or the instance lock, and a backup next to the data it copies protects nothing.
///
/// # Errors
/// [`AppError::Unsupported`] for an in-memory state, [`AppError::Validation`] when `destination`
/// is inside the data folder, [`AppError::Io`] or [`AppError::Database`] when the copy fails
/// (nothing half written is left).
pub fn data_backup_impl(state: &AppState, destination: &Path) -> Result<DataBackupDto, AppError> {
    let dirs = state
        .data_dirs()
        .ok_or_else(|| AppError::unsupported("a backup needs the on-disk database"))?;
    let canonical = |path: &Path| path.canonicalize().unwrap_or_else(|_| path.to_path_buf());
    let root = canonical(&dirs.root);
    // The file may not exist yet: resolve its folder (a relative name lives in the current one).
    let folder = destination
        .parent()
        .filter(|parent| !parent.as_os_str().is_empty())
        .unwrap_or_else(|| Path::new("."));
    if canonical(folder).starts_with(&root) {
        return Err(AppError::validation(
            "choose a folder outside PingDeck's data folder: a backup there could replace the live database",
        ));
    }
    let connection = state.llm().db().open()?;
    let files = pingdeck_store::storage::backup_database(&connection, destination)
        .map_err(command_error)?;
    state.note_user_file(&files.database);
    Ok(DataBackupDto {
        path: files.database.to_string_lossy().into_owned(),
        secrets_path: files
            .secrets
            .map(|path| path.to_string_lossy().into_owned()),
        bytes: files.bytes,
    })
}

/// Compacts the database and the vault (`VACUUM`) and truncates the write-ahead log, so the space
/// clears freed goes back to the disk. Blocking. It runs on the shared connection under the store
/// lock, so saves, history rows and everything else on that connection queue on the lock for as
/// long as it takes instead of running into SQLite's busy timeout; background writers on pooled
/// connections (gateway logs, usage, secrets) still wait only up to the busy timeout and can fail
/// while a long compaction runs.
///
/// # Errors
/// [`AppError::Unsupported`] for an in-memory state, [`AppError::Database`] when SQLite cannot
/// compact (for example while another connection holds a long write).
pub fn data_compact_impl(state: &AppState) -> Result<DataCompactDto, AppError> {
    let dirs = state
        .data_dirs()
        .ok_or_else(|| AppError::unsupported("compaction needs the on-disk database"))?;
    let before_bytes = database_file_bytes(&dirs.db);
    {
        let store = state.store();
        pingdeck_store::storage::compact_database(store.connection()).map_err(command_error)?;
    }
    Ok(DataCompactDto {
        before_bytes,
        after_bytes: database_file_bytes(&dirs.db),
    })
}

/// The sizes of the data folder's parts. Blocking: one pass over the attachment and log folders
/// and a few aggregate queries.
///
/// # Errors
/// [`AppError::Database`] when the counts cannot be read.
pub fn storage_usage_impl(state: &AppState) -> Result<StorageUsageDto, AppError> {
    let (chats, captures, attachments_count, page_bytes, free) = {
        let store = state.store();
        (
            store.chat_usage().map_err(command_error)?,
            store.capture_usage().map_err(command_error)?,
            store.attachment_count().map_err(command_error)?,
            store.database_page_bytes().map_err(command_error)?,
            pingdeck_store::storage::free_bytes(store.connection()).map_err(command_error)?,
        )
    };
    let mut usage = StorageUsageDto {
        database_bytes: page_bytes,
        database_free_bytes: free,
        chats_bytes: chats.bytes,
        chats_count: chats.chats,
        attachments_count,
        captured_bodies_bytes: captures.bytes,
        captured_bodies_count: captures.count,
        ..StorageUsageDto::default()
    };
    if let Some(dirs) = state.data_dirs() {
        usage.database_bytes = database_file_bytes(&dirs.db);
        usage.attachments_bytes = dir_bytes(&dirs.attachments);
        usage.logs_bytes = dir_bytes(&dirs.logs);
        usage.chats_bytes += dir_bytes(&dirs.root.join("tool-output"));
    }
    Ok(usage)
}

fn retention_cutoff(now_ms: i64, days: u32) -> i64 {
    now_ms.saturating_sub(i64::from(days) * DAY_MS)
}

fn checked_days(days: u32) -> Result<u32, AppError> {
    if days > MAX_DAYS {
        return Err(AppError::validation(format!(
            "days must be between 1 and {MAX_DAYS}"
        )));
    }
    Ok(days)
}

/// How many chats deleting those last used more than `days` ago would remove; 0 days is "Off".
///
/// # Errors
/// [`AppError::Validation`] for more than 3650 days, [`AppError::Database`] on a read failure.
pub fn chat_retention_preview_impl(
    state: &AppState,
    days: u32,
) -> Result<ChatRetentionPreviewDto, AppError> {
    if checked_days(days)? == 0 {
        return Ok(ChatRetentionPreviewDto::default());
    }
    let cutoff = retention_cutoff(current_time_ms(), days);
    let chats = state
        .store()
        .retention_candidates(cutoff)
        .map_err(command_error)?;
    Ok(ChatRetentionPreviewDto {
        chats: u64::try_from(chats.len()).unwrap_or(u64::MAX),
    })
}

/// Deletes the chats last used more than `days` ago at `now_ms` (0 days is "Off": nothing). A
/// pinned chat, a sub-agent chat and a chat with a queued, running or waiting run stay. Deleted
/// chats are restorable for a day like any deleted chat, and their attachments go with the
/// startup purge. Returns how many chats were deleted.
///
/// # Errors
/// [`AppError::Database`] on a read or write failure.
pub fn chat_retention_apply_impl(
    state: &AppState,
    days: u32,
    now_ms: i64,
) -> Result<u64, AppError> {
    if checked_days(days)? == 0 {
        return Ok(0);
    }
    let candidates = state
        .store()
        .retention_candidates(retention_cutoff(now_ms, days))
        .map_err(command_error)?;
    let mut deleted = 0;
    for id in candidates {
        if state.agent.runs.active_for_session(&id).is_some() {
            continue;
        }
        match state
            .agent
            .cancel_and_soft_delete(&state.store(), &id, now_ms)
        {
            Ok(true) => deleted += 1,
            // Deleted by the user in the meantime.
            Ok(false) => {}
            Err(error) => return Err(command_error(error)),
        }
    }
    Ok(deleted)
}

/// The configured `data.chat_retention_days` (0: Off).
fn configured_retention_days(state: &AppState) -> u32 {
    u32::from(state.settings().data.chat_retention_days)
}

/// Runs the chat retention once with the configured age (the launch and daily job).
///
/// # Errors
/// As [`chat_retention_apply_impl`].
pub fn chat_retention_run_impl(state: &AppState) -> Result<u64, AppError> {
    chat_retention_apply_impl(state, configured_retention_days(state), current_time_ms())
}

/// Starts the chat-retention job: one run now, then one every 24 hours, until the state is
/// dropped. A failed run (a database error) is tried again at the next tick. Needs a Tokio
/// runtime; the work itself runs on the blocking pool.
///
/// # Errors
/// [`AppError::Internal`] when there is no Tokio runtime to spawn on.
pub fn start_chat_retention_job(state: &AppState) -> Result<(), AppError> {
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|_| AppError::internal("the chat retention job needs a Tokio runtime"))?;
    let weak: Weak<AppStateInner> = Arc::downgrade(&state.0);
    runtime.spawn(async move {
        while let Some(inner) = weak.upgrade() {
            let state = AppState(inner);
            // The closure owns the only strong handle, so it is gone before the sleep.
            let _ = tokio::task::spawn_blocking(move || chat_retention_run_impl(&state)).await;
            tokio::time::sleep(RETENTION_INTERVAL).await;
        }
    });
    Ok(())
}

/// Removes the stored files of attachments older than `options.older_than_days` (all with
/// `None`) together with the text derived from their PDFs; with `dry_run` it only reports. The
/// rows and message links stay (see the module docs); blobs the file-change journal also uses
/// stay. `chats` counts the chats with a message that showed a removed attachment.
///
/// # Errors
/// [`AppError::Unsupported`] without a data directory, [`AppError::Validation`] for an age over
/// 3650 days, [`AppError::Io`] when a blob cannot be deleted.
pub fn clear_attachments_impl(
    state: &AppState,
    options: ClearAttachmentsOptions,
) -> Result<DataClearReportDto, AppError> {
    let cutoff = options
        .older_than_days
        .map(|days| checked_days(days).map(|days| retention_cutoff(current_time_ms(), days)))
        .transpose()?;
    let store = state.attachment_store()?;
    let _blobs = blobs_lock();
    let keep = state.store().journal_blob_shas().map_err(command_error)?;
    let rows = state
        .store()
        .clearable_attachments(cutoff, &keep)
        .map_err(command_error)?;
    // A row cleared before has no blob left and counts for nothing.
    let present: Vec<_> = rows
        .into_iter()
        .filter(|row| store.exists(&row.sha256))
        .collect();
    let linked: Vec<String> = present
        .iter()
        .filter(|row| row.linked)
        .map(|row| row.id.clone())
        .collect();
    let report = DataClearReportDto {
        removed: u64::try_from(present.iter().filter(|row| !row.derived).count())
            .unwrap_or(u64::MAX),
        freed_bytes: present.iter().map(|row| row.size_bytes).sum(),
        chats: state
            .store()
            .chats_with_attachments(&linked)
            .map_err(command_error)?,
    };
    if options.dry_run {
        return Ok(report);
    }
    for row in &present {
        store.remove(&row.sha256)?;
    }
    let unlinked: Vec<String> = present
        .iter()
        .filter(|row| !row.linked)
        .map(|row| row.id.clone())
        .collect();
    let workspace = state.store();
    workspace
        .forget_attachment_previews(&linked)
        .map_err(command_error)?;
    workspace
        .delete_attachment_rows(&unlinked)
        .map_err(command_error)?;
    Ok(report)
}

/// Deletes every captured gateway body and keeps the log rows ("Clear captured bodies").
/// `freed_bytes` is the size of the bodies removed from the database; the file keeps its size
/// until "Compact database" runs.
///
/// # Errors
/// [`AppError::Database`] on a read or write failure.
pub async fn clear_captured_bodies_impl(state: &AppState) -> Result<DataClearReportDto, AppError> {
    let captures = state.store().capture_usage().map_err(command_error)?;
    let freed_bytes = state
        .with_llm_db_blocking(crate::llm::store::delete_captures)
        .await?;
    Ok(DataClearReportDto {
        removed: captures.count,
        freed_bytes,
        chats: 0,
    })
}
