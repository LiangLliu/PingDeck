//! The journaled delete and move of the file tools: `delete_file` and `move_file`.
//!
//! Single responsibility: the blocking work behind those two tools, on paths that [`super::Env`]
//! already confined. A delete keeps the file's bytes in its journal record (an edit that leaves no
//! file), so the Changes tab can restore it; a move writes the destination through held directory
//! handles with the source's mode and then removes the source, recorded as a create and a delete
//! (the record keeps no mode, so undoing the delete recreates the source with the default mode).
//! Files larger than the write limit are refused: their bytes could not be kept for undo.

use std::path::Path;

use super::diff::change_counts;
use super::ops::{FOLDER_CHANGED, io_error, read_existing_bytes};
use super::{AppliedWrite, BeforeWrite, DeleteArgs, Env, FileChangeRecord, MoveArgs};
use crate::agent::tools::{FileChangeKind, ToolOutput, fs_safe};
use spanbay_base::cancel::CancelToken;
use spanbay_base::crypto::sha256_hex;
use spanbay_base::error::AppError;

/// Refuses a delete of anything but an existing regular file.
pub(super) fn check_deletable(path: &Path, rel: &str) -> Result<(), AppError> {
    let meta = std::fs::metadata(path).map_err(|error| io_error(rel, &error))?;
    if meta.is_dir() {
        return Err(AppError::validation(format!(
            "'{rel}' is a folder; delete_file deletes one file at a time"
        )));
    }
    if !meta.is_file() {
        return Err(AppError::validation(format!(
            "'{rel}' is not a regular file"
        )));
    }
    Ok(())
}

/// Refuses a move whose source is not a regular file or whose destination exists.
pub(super) fn check_movable(
    from: &Path,
    from_rel: &str,
    to: &Path,
    to_rel: &str,
) -> Result<(), AppError> {
    check_deletable(from, from_rel)?;
    if from == to {
        return Err(AppError::validation(format!(
            "'{from_rel}' and '{to_rel}' are the same file"
        )));
    }
    if std::fs::symlink_metadata(to).is_ok() {
        return Err(AppError::validation(format!(
            "'{to_rel}' already exists; move_file never overwrites a file"
        )));
    }
    Ok(())
}

/// The path of `path` inside the workspace, once more resolved to the same location (a folder
/// that changed between the check and the change refuses it).
fn inside<'a>(env: &Env, path: &'a Path, rel: &str) -> Result<&'a Path, AppError> {
    match env.resolve(rel, true) {
        Ok(now) if now == path => path.strip_prefix(&env.workspace).ok(),
        _ => None,
    }
    .ok_or_else(|| AppError::conflict(FOLDER_CHANGED))
}

/// The journal record of removing `rel`, whose content was `bytes`.
fn removal(rel: &str, bytes: Vec<u8>) -> FileChangeRecord {
    let text = String::from_utf8_lossy(&bytes);
    let (added, removed) = change_counts(Some(text.as_ref()), "");
    FileChangeRecord {
        path: rel.to_owned(),
        kind: FileChangeKind::Edit,
        before_sha256: Some(sha256_hex(&bytes)),
        before_bytes: Some(bytes),
        after_sha256: None,
        added,
        removed,
    }
}

/// `delete_file`: removes one regular file, keeping its bytes in the journal record handed to
/// `before_write` first (an error there aborts with nothing deleted).
pub(super) fn delete_file(
    env: &Env,
    args: &DeleteArgs,
    cancel: &CancelToken,
    before_write: BeforeWrite<'_>,
) -> Result<AppliedWrite, AppError> {
    let path = env.resolve(&args.path, true)?;
    let rel = env.relative(&path);
    check_deletable(&path, &rel)?;
    let seen = fs_safe::identity(&path)?;
    let bytes = read_existing_bytes(&path, &rel)?;
    if cancel.is_cancelled() {
        return Err(AppError::cancelled());
    }
    let size = bytes.len();
    let record = removal(&rel, bytes);
    before_write(&record)?;
    fs_safe::remove_in(&env.workspace, inside(env, &path, &rel)?, seen)?;
    Ok(AppliedWrite {
        output: ToolOutput::text(format!("Deleted {rel} ({size} bytes)")),
        changes: vec![record],
    })
}

/// `move_file`: writes the source's bytes to the destination (which must not exist; missing
/// folders are created), then removes the source. Both journal records go to `before_write`
/// before anything changes.
pub(super) fn move_file(
    env: &Env,
    args: &MoveArgs,
    cancel: &CancelToken,
    before_write: BeforeWrite<'_>,
) -> Result<AppliedWrite, AppError> {
    let from = env.resolve(&args.from, true)?;
    let to = env.resolve(&args.to, true)?;
    let (from_rel, to_rel) = (env.relative(&from), env.relative(&to));
    check_movable(&from, &from_rel, &to, &to_rel)?;
    let seen = fs_safe::identity(&from)?;
    // The moved file keeps its mode (an executable script stays executable).
    let permissions = std::fs::metadata(&from)
        .map_err(|error| io_error(&from_rel, &error))?
        .permissions();
    let bytes = read_existing_bytes(&from, &from_rel)?;
    if cancel.is_cancelled() {
        return Err(AppError::cancelled());
    }
    let (added, _) = change_counts(None, &String::from_utf8_lossy(&bytes));
    let created = FileChangeRecord {
        path: to_rel.clone(),
        kind: FileChangeKind::Create,
        before_bytes: None,
        before_sha256: None,
        after_sha256: Some(sha256_hex(&bytes)),
        added,
        removed: 0,
    };
    let removed = removal(&from_rel, bytes.clone());
    before_write(&created)?;
    before_write(&removed)?;
    let to_inside = inside(env, &to, &to_rel)?;
    let from_inside = inside(env, &from, &from_rel)?;
    fs_safe::create_in(&env.workspace, to_inside, &bytes, permissions).map_err(
        |error| match error {
            AppError::Conflict { .. } => AppError::conflict(FOLDER_CHANGED),
            other => other,
        },
    )?;
    if let Err(error) = fs_safe::remove_in(&env.workspace, from_inside, seen) {
        // The copy stays only while the source could not go: take it back.
        let copy = fs_safe::identity(&to).ok().flatten();
        let _ = fs_safe::remove_in(&env.workspace, to_inside, copy);
        return Err(error);
    }
    // What the session read at the old path is what it now reads at the new one.
    if let Some(sha256) = env.hashes.get(&env.session_id, &from) {
        env.hashes.record(&env.session_id, &to, sha256);
    }
    Ok(AppliedWrite {
        output: ToolOutput::text(format!("Moved {from_rel} to {to_rel}")),
        changes: vec![created, removed],
    })
}
