//! The file-change journal of the agent.
//!
//! Single responsibility: record every successful `write_file` / `edit_file` / `delete_file` /
//! `move_file` of a run as `agent_file_changes` rows (a delete is an edit row without an after
//! hash; a move is a create row plus such a delete row; the bytes before the change go to the
//! attachment blob store under their hash), and offer the Changes tab its three use-cases: list,
//! keep, undo. Undo is
//! the fs layer's [`undo_file_change`]: it writes the stored bytes back (or deletes a created
//! file) only while the file still holds what the change produced, unless forced, and only inside
//! the folder the change was made in, as recorded on its row (the path guard is applied again to
//! that folder at undo time; a row without a recorded folder is refused). Undoing a change first
//! undoes the later pending changes of the same file, newest first, so the file walks back
//! through its own history. A saved request `create_request` wrote is journaled in the same table
//! (`request_changes`) and undone through the store instead of the disk.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde_json::Value;

use crate::agent::model::{
    AgentEvent, FileChange, FileChangeKind as JournalKind, FileChangeStatus,
};
use crate::agent::tools::fs::{
    FileChangeRecord, RevertHunkOutcome, UndoOutcome, apply_write, revert_file_hunk,
    undo_file_change,
};
use crate::agent::tools::{FileChangeKind, ToolContext, ToolOutput};
use crate::error::AppError;

use super::attachments::{agent_blob_put_impl, agent_blob_read_impl};
use super::request_changes::{is_request_change, undo_request_change};
use super::sessions::load_session;
use crate::app::factory_service::new_entity_id;
use crate::app::{AppState, command_error, current_time_ms};

/// Stores the pre-edit bytes and inserts the pending journal row of one applied write.
/// Blocking: the blob store and the database.
fn journal(
    state: &AppState,
    ctx: &JournalKey,
    change: &FileChangeRecord,
    status: FileChangeStatus,
) -> Result<FileChange, AppError> {
    let before_sha256 = change
        .before_bytes
        .as_deref()
        .map(|bytes| agent_blob_put_impl(state, bytes))
        .transpose()?;
    let row = FileChange {
        id: new_entity_id("fc"),
        session_id: ctx.session_id.clone(),
        run_id: ctx.run_id.clone(),
        call_id: ctx.call_id.clone(),
        path: change.path.clone(),
        folder: Some(ctx.folder.clone()),
        kind: match change.kind {
            FileChangeKind::Create => JournalKind::Create,
            FileChangeKind::Edit => JournalKind::Edit,
        },
        before_sha256,
        after_sha256: change.after_sha256.clone(),
        added: change.added,
        removed: change.removed,
        status,
        created_at_ms: current_time_ms(),
    };
    state
        .store()
        .insert_file_change(&row)
        .map_err(command_error)?;
    Ok(row)
}

/// Who made a change, and the canonical folder its path is relative to.
struct JournalKey {
    session_id: String,
    run_id: String,
    call_id: String,
    folder: String,
}

/// Whether the built-in tool `name` changes files and so runs through the journal.
pub(super) fn journals(name: &str) -> bool {
    matches!(
        name,
        "write_file" | "edit_file" | "delete_file" | "move_file"
    )
}

/// Runs one file-changing call (`tool`: `write_file`, `edit_file`, `delete_file`, `move_file`)
/// with the journal: the files are changed, the pre-change bytes and a pending row per file are
/// stored, and the events of the call come back for the run to publish (`FileChanged` with the
/// row id; a `Notice` when the journal could not be written, which leaves the change applied but
/// not undoable). The result for the model is the same as the plain file tool's.
pub(super) async fn apply_journaled(
    ctx: &ToolContext<'_>,
    tool: &str,
    arguments: Value,
) -> (ToolOutput, Vec<AgentEvent>) {
    let Some(workspace) = ctx.workspace_dir.map(Path::to_path_buf) else {
        return (
            ToolOutput::error(
                "This session has no workspace folder, so file tools are unavailable. Ask the user to choose a folder for the session",
            ),
            Vec::new(),
        );
    };
    let state = crate::agent_tools::app_state(ctx).clone();
    let data_root = state.data_dirs().map(|dirs| dirs.root.clone());
    let key = JournalKey {
        session_id: ctx.session_id.to_owned(),
        run_id: ctx.run_id.to_owned(),
        call_id: ctx.call_id.to_owned(),
        folder: workspace.to_string_lossy().into_owned(),
    };
    let (tool, hashes, cancel) = (tool.to_owned(), ctx.read_hashes.clone(), ctx.cancel.clone());
    let work = tokio::task::spawn_blocking(move || {
        // Serialize file mutations per canonical workspace folder so parallel `General`
        // sub-agents or concurrent runs cannot race past read-hash/target checks and clobber
        // each other's writes or corrupt the undo journal.
        let folder_lock = state.agent.write_locks.lock_for(&workspace);
        let _write_guard = folder_lock
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        // The row (and the bytes it undoes with) is stored as `Applying` BEFORE the file is
        // replaced and turns `Pending` after it: a crash in between leaves a row that startup
        // recovery completes or drops by comparing the file's hash.
        let applying: std::sync::Mutex<Vec<String>> = std::sync::Mutex::new(Vec::new());
        let applied = apply_write(
            &tool,
            &workspace,
            data_root.as_deref(),
            &key.session_id,
            &hashes,
            &arguments,
            &cancel,
            &|record| {
                let row = journal(&state, &key, record, FileChangeStatus::Applying)?;
                applying
                    .lock()
                    .unwrap_or_else(std::sync::PoisonError::into_inner)
                    .push(row.id);
                Ok(())
            },
        );
        let row_ids = applying
            .into_inner()
            .unwrap_or_else(std::sync::PoisonError::into_inner);
        let applied = match applied {
            Ok(applied) => applied,
            Err(error) => {
                // Nothing was changed: the intent rows go.
                for id in &row_ids {
                    let _ = state.store().delete_file_change(id);
                }
                return Err(error);
            }
        };
        // One row per change, in the order the records were handed over.
        let rows: Vec<Result<String, AppError>> = (0..applied.changes.len())
            .map(|index| match row_ids.get(index) {
                Some(id) => state
                    .store()
                    .finish_file_change(id)
                    .map_err(command_error)
                    .map(|_| id.clone()),
                None => Err(AppError::internal("the change was not journaled")),
            })
            .collect();
        Ok::<_, AppError>((applied, rows, key))
    })
    .await;
    match work {
        Ok(Ok((applied, rows, key))) => {
            let mut events = Vec::new();
            for (change, row) in applied.changes.into_iter().zip(rows) {
                let change_id = match row {
                    Ok(id) => Some(id),
                    Err(error) => {
                        events.push(AgentEvent::notice(format!(
                            "{} was changed, but the change could not be recorded for undo: {}",
                            change.path,
                            error.message()
                        )));
                        None
                    }
                };
                events.push(AgentEvent::FileChanged {
                    call_id: key.call_id.clone(),
                    path: change.path,
                    change: change.kind,
                    added: change.added,
                    removed: change.removed,
                    change_id,
                });
            }
            (applied.output, events)
        }
        Ok(Err(error)) => (ToolOutput::error(error.to_string()), Vec::new()),
        Err(error) => (
            ToolOutput::error(format!("The file tool failed: {error}")),
            Vec::new(),
        ),
    }
}

/// The journal of a chat (and of the sub-agents it spawned), newest change first.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown chat.
pub fn agent_changes_list_impl(
    state: &AppState,
    session_id: &str,
) -> Result<Vec<FileChange>, AppError> {
    load_session(state, session_id)?;
    let mut rows = state
        .store()
        .list_chat_file_changes(session_id)
        .map_err(command_error)?;
    rows.reverse();
    Ok(rows)
}

/// Marks the pending (or conflicting) changes among `change_ids` as kept: no disk operation.
/// Returns how many changed state; ids that are unknown, already kept or already undone count for
/// nothing.
///
/// # Errors
/// [`AppError::Database`] when the journal cannot be read or written.
pub fn agent_change_keep_impl(state: &AppState, change_ids: &[String]) -> Result<u32, AppError> {
    let store = state.store();
    let mut by_session: HashMap<String, Vec<String>> = HashMap::new();
    for row in store.get_file_changes(change_ids).map_err(command_error)? {
        if matches!(
            row.status,
            FileChangeStatus::Pending | FileChangeStatus::Conflict
        ) {
            by_session.entry(row.session_id).or_default().push(row.id);
        }
    }
    let mut kept = 0;
    for (session_id, ids) in by_session {
        kept += store
            .set_file_change_status(&session_id, &ids, FileChangeStatus::Kept)
            .map_err(command_error)?;
    }
    Ok(u32::try_from(kept).unwrap_or(u32::MAX))
}

/// The chat a journal row belongs to: its session, or the parent chat of a sub-agent session.
fn chat_of(state: &AppState, session_id: &str) -> Result<String, AppError> {
    let session = state
        .store()
        .get_agent_session(session_id)
        .map_err(command_error)?
        .ok_or_else(|| AppError::not_found(format!("chat session {session_id} not found")))?;
    Ok(session.parent_session_id.unwrap_or(session.id))
}

/// Undoes `change_ids` and the later pending changes of the same files, newest first. A file is
/// a path in the folder its change recorded, so the same path in another folder is another file.
/// A change whose file no longer holds what it produced ends `conflict` and stops that file's
/// chain, unless `force` overwrites it. Returns the rows it touched, newest first, with their new
/// state.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown change or a missing stored blob,
/// [`AppError::Conflict`] while the chat has an active run, [`AppError::Validation`] for a path
/// outside the recorded folder or a change without a recorded folder (refused before any file is
/// written), I/O errors of the file system.
pub fn agent_change_undo_impl(
    state: &AppState,
    change_ids: &[String],
    force: bool,
) -> Result<Vec<FileChange>, AppError> {
    let requested = state
        .store()
        .get_file_changes(change_ids)
        .map_err(command_error)?;
    if let Some(missing) = change_ids
        .iter()
        .find(|id| !requested.iter().any(|row| &row.id == *id))
    {
        return Err(AppError::not_found(format!("change {missing} not found")));
    }
    let mut chats = Vec::new();
    for row in &requested {
        let chat = chat_of(state, &row.session_id)?;
        if !chats.contains(&chat) {
            chats.push(chat);
        }
    }
    let mut touched = Vec::new();
    for chat in chats {
        touched.extend(undo_in_chat(state, &chat, &requested, force)?);
    }
    touched.sort_by(|a: &FileChange, b: &FileChange| {
        (b.created_at_ms, &b.id).cmp(&(a.created_at_ms, &a.id))
    });
    Ok(touched)
}

/// The file a change is to: its path in its recorded folder. A saved request is one "file"
/// whatever it was renamed to, so its folder (which names the request) alone identifies it.
fn file(row: &FileChange) -> (Option<&str>, &str) {
    if is_request_change(row) {
        return (row.folder.as_deref(), "");
    }
    (row.folder.as_deref(), row.path.as_str())
}

/// One chat's share of an undo.
fn undo_in_chat(
    state: &AppState,
    chat: &str,
    requested: &[FileChange],
    force: bool,
) -> Result<Vec<FileChange>, AppError> {
    if state.agent.runs.active_for_session(chat).is_some() {
        return Err(AppError::conflict(
            "Stop this chat's run before undoing its changes",
        ));
    }
    let journal = state
        .store()
        .list_chat_file_changes(chat)
        .map_err(command_error)?;
    let undoable = |row: &FileChange| {
        matches!(
            row.status,
            FileChangeStatus::Pending | FileChangeStatus::Conflict
        )
    };
    // Per file, the position in the journal of the earliest change asked for.
    let mut first_asked: HashMap<(Option<&str>, &str), usize> = HashMap::new();
    for (position, row) in journal.iter().enumerate() {
        if undoable(row) && requested.iter().any(|asked| asked.id == row.id) {
            first_asked.entry(file(row)).or_insert(position);
        }
    }
    let chain: Vec<&FileChange> = journal
        .iter()
        .enumerate()
        .rev()
        .filter(|(position, row)| {
            undoable(row)
                && first_asked
                    .get(&file(row))
                    .is_some_and(|first| position >= first)
        })
        .map(|(_, row)| row)
        .collect();
    if let Some(row) = chain.iter().find(|row| row.folder.is_none()) {
        return Err(unknown_folder(&row.path));
    }
    let mut blocked: HashSet<(Option<&str>, &str)> = HashSet::new();
    let mut touched = Vec::new();
    for row in chain {
        if blocked.contains(&file(row)) {
            continue;
        }
        let status = undo_one(state, row, force)?;
        if status == FileChangeStatus::Conflict {
            blocked.insert(file(row));
        }
        state
            .store()
            .set_file_change_status(&row.session_id, std::slice::from_ref(&row.id), status)
            .map_err(command_error)?;
        touched.push(FileChange {
            status,
            ..row.clone()
        });
    }
    Ok(touched)
}

/// The refusal for a row recorded before changes named their folder, whose folder is unknown.
fn unknown_folder(path: &str) -> AppError {
    AppError::validation(format!(
        "{path}: the folder this change was made in is unknown, so it cannot be undone"
    ))
}

/// Takes one change back on disk, in the folder it recorded (or in the store, for a saved
/// request the agent wrote); the state it ends in.
fn undo_one(state: &AppState, row: &FileChange, force: bool) -> Result<FileChangeStatus, AppError> {
    if is_request_change(row) {
        return undo_request_change(state, row, force);
    }
    let workspace = row
        .folder
        .as_deref()
        .ok_or_else(|| unknown_folder(&row.path))?;
    let before = row
        .before_sha256
        .as_deref()
        .map(|sha256| agent_blob_read_impl(state, sha256))
        .transpose()?;
    let outcome = undo_file_change(
        Path::new(workspace),
        &row.path,
        before.as_deref(),
        row.after_sha256.as_deref(),
        force,
    )?;
    Ok(match outcome {
        UndoOutcome::Restored | UndoOutcome::Deleted => FileChangeStatus::Undone,
        UndoOutcome::Conflict => FileChangeStatus::Conflict,
    })
}

/// Reverts a single diff hunk (`hunk_index`, 0-based) of `change_id` on disk and updates the
/// journal row's `after_sha256`, `added`, `removed`, and `status`. Refuses if the chat has an
/// active run, the path escapes the recorded workspace folder, or the file on disk was modified
/// since `change_id` was recorded (marking the row [`FileChangeStatus::Conflict`] and returning
/// [`AppError::Conflict`]).
///
/// # Errors
/// [`AppError::NotFound`] for an unknown change or missing blob, [`AppError::Conflict`] while a
/// run is active or when the file on disk changed since the diff was computed,
/// [`AppError::Validation`] for an unsafe path, missing folder, binary file, or out-of-range hunk.
pub fn agent_change_revert_hunk_impl(
    state: &AppState,
    change_id: &str,
    hunk_index: usize,
) -> Result<FileChange, AppError> {
    let row = state
        .store()
        .get_file_changes(&[change_id.to_owned()])
        .map_err(command_error)?
        .into_iter()
        .next()
        .ok_or_else(|| AppError::not_found(format!("change {change_id} not found")))?;
    if is_request_change(&row) {
        return Err(AppError::validation(
            "Saved request changes can only be undone as a whole",
        ));
    }
    let chat = chat_of(state, &row.session_id)?;
    if state.agent.runs.active_for_session(&chat).is_some() {
        return Err(AppError::conflict(
            "Stop this chat's run before reverting its changes",
        ));
    }
    if !matches!(
        row.status,
        FileChangeStatus::Pending | FileChangeStatus::Conflict
    ) {
        return Err(AppError::conflict("This change is no longer pending"));
    }
    if row.after_sha256.is_none() {
        return Err(AppError::validation(
            "Deleted files can only be restored as a whole",
        ));
    }
    let workspace = row
        .folder
        .as_deref()
        .ok_or_else(|| unknown_folder(&row.path))?;
    let folder_lock = state.agent.write_locks.lock_for(Path::new(workspace));
    let _write_guard = folder_lock
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner);
    let before = row
        .before_sha256
        .as_deref()
        .map(|sha256| agent_blob_read_impl(state, sha256))
        .transpose()?;
    let outcome = revert_file_hunk(
        Path::new(workspace),
        &row.path,
        before.as_deref(),
        row.after_sha256.as_deref(),
        hunk_index,
    )?;
    match outcome {
        RevertHunkOutcome::Updated {
            after_sha256,
            added,
            removed,
        } => {
            state
                .store()
                .update_file_change_after_revert(
                    &row.session_id,
                    &row.id,
                    Some(&after_sha256),
                    added,
                    removed,
                    FileChangeStatus::Pending,
                )
                .map_err(command_error)?;
            Ok(FileChange {
                after_sha256: Some(after_sha256),
                added,
                removed,
                status: FileChangeStatus::Pending,
                ..row
            })
        }
        RevertHunkOutcome::Undone { after_sha256 } => {
            state
                .store()
                .update_file_change_after_revert(
                    &row.session_id,
                    &row.id,
                    after_sha256.as_deref(),
                    0,
                    0,
                    FileChangeStatus::Undone,
                )
                .map_err(command_error)?;
            Ok(FileChange {
                after_sha256,
                added: 0,
                removed: 0,
                status: FileChangeStatus::Undone,
                ..row
            })
        }
        RevertHunkOutcome::Conflict => {
            state
                .store()
                .set_file_change_status(
                    &row.session_id,
                    std::slice::from_ref(&row.id),
                    FileChangeStatus::Conflict,
                )
                .map_err(command_error)?;
            Err(AppError::conflict(format!(
                "{}: the file changed after this edit, so the hunk cannot be reverted",
                row.path
            )))
        }
    }
}
