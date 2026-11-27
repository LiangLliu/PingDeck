//! What a write, edit, delete or move changed, and how to take it back.
//!
//! Single responsibility: the data the run's file-change journal needs ([`FileChangeRecord`]:
//! path, kind, the bytes before, the hash after (none when the change removed the file), line
//! counts), the blocking entry point it calls ([`apply_write`]) and the undo of one recorded
//! change ([`undo_file_change`]), which refuses when the file changed since unless forced. A
//! delete is an edit that leaves no file; a move is a delete of the source and a create of the
//! destination, two records. The journal itself (SQLite rows, attachment storage of the
//! before-bytes) lives in the run loop.

use std::fs;
use std::path::Path;

use serde_json::Value;

use super::paths::PathGuard;
use super::{DeleteArgs, EditArgs, Env, MoveArgs, WriteArgs, manage, ops, parse};
use crate::agent::tools::{FileChangeKind, ReadHashes, ToolOutput, fs_safe};
use spanbay_base::cancel::CancelToken;
use spanbay_base::crypto::sha256_hex;
use spanbay_base::error::AppError;

/// Called with the finished journal record right before the file is written; an error aborts the
/// write with nothing changed. The run loop stores the previous bytes and an `applying` row here.
pub type BeforeWrite<'a> = &'a dyn Fn(&FileChangeRecord) -> Result<(), AppError>;

/// One file change, with what is needed to journal and undo it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct FileChangeRecord {
    /// Workspace-relative path.
    pub path: String,
    /// Created or modified.
    pub kind: FileChangeKind,
    /// The file's bytes before the change (`None` for a created file).
    pub before_bytes: Option<Vec<u8>>,
    /// Hex SHA-256 of `before_bytes`.
    pub before_sha256: Option<String>,
    /// Hex SHA-256 of the content the tool wrote; `None` when the change removed the file.
    pub after_sha256: Option<String>,
    /// Lines added.
    pub added: u32,
    /// Lines removed.
    pub removed: u32,
}

/// The result of one applied `write_file` / `edit_file` / `delete_file` / `move_file` call.
#[derive(Debug, Clone, PartialEq)]
pub struct AppliedWrite {
    /// What the model reads.
    pub output: ToolOutput,
    /// What the journal stores, one record per file (a move has two).
    pub changes: Vec<FileChangeRecord>,
}

/// Runs one `write_file`, `edit_file`, `delete_file` or `move_file` call (`tool`) on the files of
/// `workspace_dir`, enforcing the read-before-edit guard against `read_hashes`. `before_write`
/// runs with each journal record just before its file is changed ([`BeforeWrite`]). Blocking:
/// call it from `spawn_blocking`.
///
/// # Errors
/// [`AppError::Validation`] for bad arguments, an unknown `tool` or an unsafe path,
/// [`AppError::Conflict`] when the file was not read in this session or changed since,
/// [`AppError::Io`]-style errors and [`AppError::Cancelled`].
#[allow(clippy::too_many_arguments)] // one production call site; a struct would only rename the arguments
pub fn apply_write(
    tool: &str,
    workspace_dir: &Path,
    data_root: Option<&Path>,
    session_id: &str,
    read_hashes: &ReadHashes,
    arguments: &Value,
    cancel: &CancelToken,
    before_write: BeforeWrite<'_>,
) -> Result<AppliedWrite, AppError> {
    let env = Env::open(
        Some(workspace_dir),
        data_root,
        session_id,
        read_hashes.clone(),
    )?;
    match tool {
        "write_file" => {
            ops::write_file(&env, &parse::<WriteArgs>(arguments)?, cancel, before_write)
        }
        "edit_file" => ops::edit_file(&env, &parse::<EditArgs>(arguments)?, cancel, before_write),
        "delete_file" => {
            manage::delete_file(&env, &parse::<DeleteArgs>(arguments)?, cancel, before_write)
        }
        "move_file" => {
            manage::move_file(&env, &parse::<MoveArgs>(arguments)?, cancel, before_write)
        }
        other => Err(AppError::validation(format!(
            "{other} does not write files"
        ))),
    }
}

/// What [`undo_file_change`] did.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UndoOutcome {
    /// The previous content was written back.
    Restored,
    /// The file the change created was deleted (or was already gone).
    Deleted,
    /// The file changed after the recorded change; nothing was touched (use `force`).
    Conflict,
}

/// Takes one recorded change back: writes `before` over the file (recreating it when the change
/// deleted it), or deletes the file when `before` is `None` (the change created it). Unless
/// `force`, the file must still hold exactly the content whose hash is `expected_after_sha256`,
/// or, when that is `None` (the change removed the file), still be absent.
///
/// # Errors
/// [`AppError::Validation`] for a path outside the workspace, [`AppError::Io`]-style errors from
/// the file system.
pub fn undo_file_change(
    workspace: &Path,
    path: &str,
    before: Option<&[u8]>,
    expected_after_sha256: Option<&str>,
    force: bool,
) -> Result<UndoOutcome, AppError> {
    let workspace = super::paths::canonical_workspace(workspace)?;
    // Plain message: this runs from the undo command, whose error the user reads as text.
    let target = PathGuard::for_process(None).resolve_plain(&workspace, path, true)?;
    let inside = target
        .strip_prefix(&workspace)
        .map_err(|_| AppError::validation(format!("{path} is outside the workspace")))?;
    let io = |error: std::io::Error| AppError::io(format!("{path}: {error}"));
    // Read at the check: the write or delete below goes through held directory handles and
    // refuses when a parent or the file was swapped since.
    let seen = fs_safe::identity(&target)?;
    let current = match fs::read(&target) {
        Ok(bytes) => Some(bytes),
        Err(error) if error.kind() == std::io::ErrorKind::NotFound => None,
        Err(error) => return Err(io(error)),
    };
    let unchanged = match (current.as_deref(), expected_after_sha256) {
        (Some(bytes), Some(expected)) => sha256_hex(bytes) == expected,
        (None, None) => true,
        _ => false,
    };
    // An already deleted created file is already undone.
    let already_undone = before.is_none() && current.is_none();
    if !force && !unchanged && !already_undone {
        return Ok(UndoOutcome::Conflict);
    }
    match before {
        Some(bytes) => {
            fs_safe::write_in(&workspace, inside, seen, bytes)?;
            Ok(UndoOutcome::Restored)
        }
        None => {
            if current.is_some() {
                fs_safe::remove_in(&workspace, inside, seen)?;
            }
            Ok(UndoOutcome::Deleted)
        }
    }
}
