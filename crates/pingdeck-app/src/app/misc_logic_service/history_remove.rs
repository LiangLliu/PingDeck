//! Deleting history rows with a core-held Undo, and the retention preview.
//!
//! The full rows a delete removes stay in memory behind an opaque token for at most 60 s; the
//! webview only ever holds the token, so it cannot hand back a row the core did not delete.

use std::time::Instant;

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::workspace::{ExecutionHistoryEntry, HistoryEntryId, WorkspaceId};

use super::super::history_cookies_service::clear_stale_history_refs;
use super::super::undo_store::{UndoHeld, UndoStore, UndoToken};
use super::super::{AppState, command_error};

/// The most rows one delete keeps for Undo; a larger delete gets no token.
pub const HISTORY_UNDO_MAX_ROWS: usize = 200;

/// Rows of one delete, held for its Undo.
#[derive(Clone)]
pub(crate) struct HeldHistory {
    workspace_id: WorkspaceId,
    entries: Vec<ExecutionHistoryEntry>,
}

impl UndoHeld for HeldHistory {
    fn workspace_id(&self) -> &WorkspaceId {
        &self.workspace_id
    }
}

pub(crate) type HistoryUndoStore = UndoStore<HeldHistory>;

/// What `history_entries_delete` answers: the ids really deleted and, when Undo is possible,
/// its token.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HistoryRemoveResult {
    pub removed: Vec<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub undo_token: Option<UndoToken>,
}

/// Deletes `ids` of `workspace_id` (unknown or foreign ids are skipped) and holds the full rows
/// for Undo, unless more than [`HISTORY_UNDO_MAX_ROWS`] went.
pub fn history_entries_delete_impl(
    state: &AppState,
    workspace_id: String,
    ids: Vec<String>,
) -> Result<HistoryRemoveResult, AppError> {
    let result = history_entries_delete_at(state, &workspace_id, &ids, Instant::now())?;
    state.history_undo.arm_timer();
    Ok(result)
}

pub(crate) fn history_entries_delete_at(
    state: &AppState,
    workspace_id: &str,
    ids: &[String],
    now: Instant,
) -> Result<HistoryRemoveResult, AppError> {
    let workspace = WorkspaceId::new(workspace_id).map_err(command_error)?;
    let ids = ids
        .iter()
        .map(|id| HistoryEntryId::new(id).map_err(command_error))
        .collect::<Result<Vec<_>, _>>()?;
    state.history_undo.purge_at(now);
    let entries = state
        .store()
        .take_execution_history_entries(workspace.as_str(), &ids)
        .map_err(command_error)?;
    let removed = entries.iter().map(|e| e.id.as_str().to_owned()).collect();
    let undo_token = (!entries.is_empty() && entries.len() <= HISTORY_UNDO_MAX_ROWS).then(|| {
        state.history_undo.insert_at(
            now,
            HeldHistory {
                workspace_id: workspace,
                entries,
            },
        )
    });
    Ok(HistoryRemoveResult {
        removed,
        undo_token,
    })
}

/// Puts back the rows of `token`, which must belong to `workspace_id`; returns the restored ids.
/// An unknown, expired or foreign token is `NotFound` and restores nothing.
pub fn history_entries_undo_impl(
    state: &AppState,
    workspace_id: String,
    token: UndoToken,
) -> Result<Vec<String>, AppError> {
    history_entries_undo_at(state, &workspace_id, &token, Instant::now())
}

pub(crate) fn history_entries_undo_at(
    state: &AppState,
    workspace_id: &str,
    token: &UndoToken,
    now: Instant,
) -> Result<Vec<String>, AppError> {
    let workspace = WorkspaceId::new(workspace_id).map_err(command_error)?;
    let held = state
        .history_undo
        .peek_at(now, &workspace, &token.0)
        .ok_or_else(|| AppError::not_found("this delete can no longer be undone"))?;
    let restored = {
        let store = state.store();
        let mut entries = held.entries;
        for entry in &mut entries {
            clear_stale_history_refs(&store, entry)?;
        }
        store
            .restore_execution_history_entries(entries)
            .map_err(command_error)?
    };
    state.history_undo.discard(&token.0);
    Ok(restored
        .into_iter()
        .map(|id| id.as_str().to_owned())
        .collect())
}

/// How many history rows a retention of `keep` would delete: in `workspace_id`, or summed over
/// every workspace when it is `None`.
pub fn history_prune_preview_impl(
    state: &AppState,
    workspace_id: Option<String>,
    keep: u32,
) -> Result<u64, AppError> {
    let workspace = workspace_id
        .map(|id| WorkspaceId::new(&id).map_err(command_error))
        .transpose()?;
    state
        .store()
        .history_prune_count(workspace.as_ref().map(WorkspaceId::as_str), keep as usize)
        .map_err(command_error)
}
