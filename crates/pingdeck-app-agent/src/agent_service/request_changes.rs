//! The agent's saved-request writes in the change journal.
//!
//! Single responsibility: record a `create_request` write as an `agent_file_changes` row in the
//! same transaction as the write, and take it back on undo. A request row is told apart from a file
//! row by its folder, `pingdeck-request://<workspace id>/<request id>`, which no file system path
//! can be; its path is the place the request is shown at (`PingDeck: Collection/Folder/Name`).
//! The before blob is the request as it was (none for a created one); the after blob holds the
//! request as stored after the write and the folders the write created. Undo restores the before
//! request (with its agent marker as it was), or deletes a created request and the created folders
//! that are empty again, only while the stored request still equals the after state, unless forced.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::agent::model::{FileChange, FileChangeKind, FileChangeStatus};
use crate::agent::tools::fs::unified_diff;
use crate::app::factory_service::new_entity_id;
use crate::app::{AppState, command_error, current_time_ms};
use crate::error::AppError;
use crate::workspace::models::Request;
use crate::workspace::{FolderId, RequestId, WorkspaceId, WorkspaceStore};

use super::attachments::{agent_blob_put_impl, agent_blob_read_impl};
use super::change_diff::ChangeDiff;

/// The folder prefix of a saved-request journal row.
const SCHEME: &str = "pingdeck-request://";
/// Largest diff text of a request change, in bytes.
const MAX_DIFF_BYTES: usize = 512 * 1024;

/// Who made a write: the journal key of the tool call.
pub(crate) struct RequestChangeKey<'a> {
    pub session_id: &'a str,
    pub run_id: &'a str,
    pub call_id: &'a str,
}

/// The after state of a request write.
#[derive(Serialize, Deserialize)]
struct AfterState {
    /// The request as the store holds it after the write (normalized, see [`normalized`]).
    request: Value,
    /// Folders the write created, deepest last.
    #[serde(default)]
    created_folders: Vec<String>,
}

/// Whether `row` journals a saved-request write rather than a file.
pub(crate) fn is_request_change(row: &FileChange) -> bool {
    row.folder
        .as_deref()
        .is_some_and(|folder| folder.starts_with(SCHEME))
}

/// The journal folder of a request.
fn folder_of(workspace_id: &str, request_id: &str) -> String {
    format!("{SCHEME}{workspace_id}/{request_id}")
}

/// Workspace and request id of a request row.
fn ids_of(row: &FileChange) -> Result<(WorkspaceId, RequestId), AppError> {
    let rest = row
        .folder
        .as_deref()
        .and_then(|folder| folder.strip_prefix(SCHEME))
        .ok_or_else(|| AppError::internal("not a saved-request change"))?;
    let (workspace, request) = rest
        .split_once('/')
        .ok_or_else(|| AppError::internal("a saved-request change without its request id"))?;
    Ok((
        WorkspaceId::new(workspace).map_err(command_error)?,
        RequestId::new(request).map_err(command_error)?,
    ))
}

/// `request` as compared and restored: without the editor revision and the agent marker, which a
/// save does not write.
fn normalized(request: &Request) -> Result<Value, AppError> {
    let mut copy = request.clone();
    copy.revision = None;
    copy.agent_touched_ms = None;
    serde_json::to_value(&copy).map_err(|error| AppError::internal(error.to_string()))
}

fn to_bytes(value: &impl Serialize) -> Result<Vec<u8>, AppError> {
    serde_json::to_vec_pretty(value).map_err(|error| AppError::internal(error.to_string()))
}

/// Stores the blobs of a write that is about to happen: the before request (`None` for a create)
/// and returns its hash. Blocking: the blob store. Called before the transaction, so a failed
/// write only leaves an unreferenced blob behind.
pub(crate) fn put_before(
    state: &AppState,
    before: Option<&Request>,
) -> Result<Option<String>, AppError> {
    before
        .map(|request| {
            let mut copy = request.clone();
            copy.revision = None;
            agent_blob_put_impl(state, &to_bytes(&copy)?)
        })
        .transpose()
}

/// Inserts the pending journal row of a request write, inside the caller's transaction on
/// `store`: `after` is the request as stored after the write, `created_folders` the folders it
/// created, `place` where the request is shown. The after blob is written first (outside the
/// database). When the blob store cannot take it, or the row cannot be stored, the write goes on
/// unjournaled (`Ok(None)`), as a file change does.
#[allow(clippy::too_many_arguments)]
pub(crate) fn journal_request_write(
    state: &AppState,
    store: &WorkspaceStore,
    key: &RequestChangeKey<'_>,
    before: Option<&Request>,
    before_sha256: Option<String>,
    after: &Request,
    created_folders: &[String],
    place: &str,
) -> Result<Option<FileChange>, AppError> {
    let after_state = AfterState {
        request: normalized(after)?,
        created_folders: created_folders.to_vec(),
    };
    let Ok(after_sha256) = agent_blob_put_impl(state, &to_bytes(&after_state)?) else {
        return Ok(None);
    };
    let (added, removed) = line_counts(before, after)?;
    let row = FileChange {
        id: new_entity_id("fc"),
        session_id: key.session_id.to_owned(),
        run_id: key.run_id.to_owned(),
        call_id: key.call_id.to_owned(),
        path: format!("PingDeck: {place}"),
        folder: Some(folder_of(after.workspace_id.as_str(), after.id.as_str())),
        kind: if before.is_some() {
            FileChangeKind::Edit
        } else {
            FileChangeKind::Create
        },
        before_sha256,
        after_sha256: Some(after_sha256),
        added,
        removed,
        status: FileChangeStatus::Pending,
        created_at_ms: current_time_ms(),
    };
    // A savepoint of the caller's transaction: a row that cannot be stored leaves the write
    // unjournaled instead of failing it, as a file change does.
    let inserted = store.with_transaction(
        |_| store.insert_file_change(&row).map_err(command_error),
        |error| AppError::database(error.to_string()),
    );
    Ok(inserted.is_ok().then_some(row))
}

/// Lines added and removed between the pretty JSON of `before` and `after`.
fn line_counts(before: Option<&Request>, after: &Request) -> Result<(u32, u32), AppError> {
    let before = before
        .map(|request| normalized(request).and_then(|value| to_bytes(&value)))
        .transpose()?
        .map(|bytes| String::from_utf8_lossy(&bytes).into_owned());
    let after = String::from_utf8_lossy(&to_bytes(&normalized(after)?)?).into_owned();
    Ok(pingdeck_base::diff::change_counts(
        before.as_deref(),
        &after,
    ))
}

fn read_after(state: &AppState, row: &FileChange) -> Result<AfterState, AppError> {
    let sha256 = row
        .after_sha256
        .as_deref()
        .ok_or_else(|| AppError::not_found("the change has no recorded after state"))?;
    serde_json::from_slice(&agent_blob_read_impl(state, sha256)?)
        .map_err(|error| AppError::internal(format!("the recorded change is unreadable: {error}")))
}

fn read_before(state: &AppState, row: &FileChange) -> Result<Option<Request>, AppError> {
    row.before_sha256
        .as_deref()
        .map(|sha256| {
            serde_json::from_slice::<Request>(&agent_blob_read_impl(state, sha256)?).map_err(
                |error| AppError::internal(format!("the recorded request is unreadable: {error}")),
            )
        })
        .transpose()
}

/// Takes one request write back; the state it ends in. Blocking: the store and the blob store.
///
/// # Errors
/// [`AppError::NotFound`] for a missing blob, store errors (a restored request whose folder was
/// deleted meanwhile is refused by the store).
pub(crate) fn undo_request_change(
    state: &AppState,
    row: &FileChange,
    force: bool,
) -> Result<FileChangeStatus, AppError> {
    let (workspace_id, request_id) = ids_of(row)?;
    let after = read_after(state, row)?;
    let before = read_before(state, row)?;
    let store = state.store();
    store.with_transaction(
        |_| {
            let current = store
                .get_request(&workspace_id, &request_id)
                .map_err(command_error)?;
            let unchanged = match &current {
                Some(current) => normalized(current)? == after.request,
                None => false,
            };
            if !unchanged && !force {
                return Ok(FileChangeStatus::Conflict);
            }
            match &before {
                Some(before) => {
                    store.save_request(before).map_err(command_error)?;
                    match before.agent_touched_ms {
                        Some(touched) => store
                            .mark_request_agent_touched(
                                workspace_id.as_str(),
                                request_id.as_str(),
                                touched,
                            )
                            .map_err(command_error)?,
                        None => store
                            .clear_request_agent_touched(workspace_id.as_str(), request_id.as_str())
                            .map_err(command_error)?,
                    };
                }
                None => {
                    if current.is_some() {
                        store
                            .delete_request(&workspace_id, &request_id)
                            .map_err(command_error)?;
                    }
                    delete_empty_folders(&store, &workspace_id, &after.created_folders)?;
                }
            }
            Ok(FileChangeStatus::Undone)
        },
        |error| AppError::database(error.to_string()),
    )
}

/// Deletes the folders a write created, deepest first, while they hold no request and no folder.
fn delete_empty_folders(
    store: &WorkspaceStore,
    workspace_id: &WorkspaceId,
    created: &[String],
) -> Result<(), AppError> {
    if created.is_empty() {
        return Ok(());
    }
    let requests = store
        .list_requests(workspace_id.as_str())
        .map_err(command_error)?;
    for id in created.iter().rev() {
        let folder_id = FolderId::new(id).map_err(command_error)?;
        let Some(folder) = store
            .get_folder(workspace_id, &folder_id)
            .map_err(command_error)?
        else {
            continue;
        };
        let holds_request = requests
            .iter()
            .any(|request| request.folder_id.as_ref() == Some(&folder_id));
        let holds_folder = store
            .list_folders(workspace_id, &folder.collection_id)
            .map_err(command_error)?
            .iter()
            .any(|other| other.parent_folder_id.as_ref() == Some(&folder_id));
        if !holds_request && !holds_folder {
            store
                .delete_folder(workspace_id, &folder_id)
                .map_err(command_error)?;
        }
    }
    Ok(())
}

/// The diff of a request write: the pretty JSON of the request before and after.
///
/// # Errors
/// [`AppError::NotFound`] for a missing blob.
pub(crate) fn request_change_diff(
    state: &AppState,
    row: &FileChange,
) -> Result<ChangeDiff, AppError> {
    let after = read_after(state, row)?;
    let before = match read_before(state, row)? {
        Some(request) => String::from_utf8_lossy(&to_bytes(&normalized(&request)?)?).into_owned(),
        None => String::new(),
    };
    let after = String::from_utf8_lossy(&to_bytes(&after.request)?).into_owned();
    Ok(ChangeDiff {
        unified_diff: unified_diff(
            &row.path,
            (!before.is_empty()).then_some(before.as_str()),
            &after,
            MAX_DIFF_BYTES,
        ),
        binary: false,
    })
}
