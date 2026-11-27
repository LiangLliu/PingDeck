//! Runner data parsing, history paging, save destinations, script catalog, validation messages.
//!
//! Single responsibility: the small pieces of UI-side logic that used to live in TypeScript
//! (runner data dialog, history panel, save-request dialog, script snippets, form pre-checks),
//! as plain use-cases any shell can call.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::{
    error::AppError,
    workspace::{
        ExecutionHistoryEntry, HistoryCursor, HistoryFilter, HistoryRunSummary, models::Folder,
    },
};

use super::{AppState, command_error};

mod history_remove;
mod runner_data;
mod snippets;
#[cfg(test)]
mod tests;
mod validation;

pub(crate) use history_remove::HistoryUndoStore;
pub use history_remove::{
    HISTORY_UNDO_MAX_ROWS, HistoryRemoveResult, history_entries_delete_impl,
    history_entries_undo_impl, history_prune_preview_impl,
};
#[cfg(test)]
pub(crate) use history_remove::{history_entries_delete_at, history_entries_undo_at};
pub use runner_data::{
    RUNNER_MAX_DELAY_MS, RUNNER_MAX_ITERATIONS, RunnerData, RunnerDataKind, RunnerLimits,
    runner_limits, runner_parse_data,
};
pub use snippets::{ScriptPhase, ScriptSnippet, script_snippet_catalog};
pub use validation::{
    IssueCode, ValidationIssue, validate_collection, validate_environment, validate_flow,
    validate_folder, validate_globals, validate_mock_server,
};
pub use validation::{first_flow_issue_error, first_issue_error};

/// Default history page size.
pub const HISTORY_PAGE_SIZE: u32 = 50;
/// Largest page the store serves; one extra row is fetched to find the continuation.
pub const HISTORY_MAX_PAGE_SIZE: u32 = 200;

/// One page of history, newest first.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct HistoryPageDto {
    /// At most `limit` entries, in store order (`executed_at` then `id`, both descending).
    pub entries: Vec<ExecutionHistoryEntry>,
    /// Opaque cursor of the next page; `None` on the last page.
    pub next_cursor: Option<String>,
    /// Rows the filter matches in total; only on the first page (no cursor).
    pub total: Option<u64>,
    /// Totals of the runs (collection runs, flow runs) that have rows on this page, over the rows
    /// the filter keeps: collapse a page's rows by `run_id` and read the counts from here.
    pub runs: Vec<HistoryRunSummary>,
    /// Rows of the whole workspace whatever the filter; first page of `history_page` only. Zero
    /// means nothing was ever recorded, as opposed to nothing in the chosen range.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub workspace_total: Option<u64>,
    /// Mock hits this process could not write (the writer's queue was full); first page of
    /// `history_page` only, and only when some were dropped.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub mock_dropped: Option<u64>,
}

/// Loads one history page under `filter` with a keyset cursor.
///
/// `limit` is the page size (default [`HISTORY_PAGE_SIZE`], clamped to
/// `1..=`[`HISTORY_MAX_PAGE_SIZE`]); `cursor` is the previous page's `next_cursor`, so rows
/// inserted while paging cannot repeat or skip. A cursor that is not one of ours is a
/// validation error.
pub fn history_page_impl(
    state: &AppState,
    workspace_id: String,
    filter: HistoryFilter,
    limit: Option<u32>,
    cursor: Option<String>,
) -> Result<HistoryPageDto, AppError> {
    let page_size = limit
        .unwrap_or(HISTORY_PAGE_SIZE)
        .clamp(1, HISTORY_MAX_PAGE_SIZE);
    let cursor = decode_cursor(cursor)?;
    let first = cursor.is_none();
    let rows = state
        .store()
        .query_execution_history(&workspace_id, &filter, page_size, cursor.as_ref())
        .map_err(command_error)?;
    let dropped = state.mock_history.dropped();
    Ok(HistoryPageDto {
        entries: rows.entries,
        next_cursor: rows.next_cursor.as_ref().map(HistoryCursor::encode),
        total: rows.total,
        runs: rows.runs,
        workspace_total: rows.workspace_total,
        mock_dropped: (first && dropped > 0).then_some(dropped),
    })
}

/// Parses a page cursor the shell sends back; one that is not one of ours is a validation error.
fn decode_cursor(cursor: Option<String>) -> Result<Option<HistoryCursor>, AppError> {
    cursor
        .map(|text| {
            HistoryCursor::decode(&text)
                .ok_or_else(|| AppError::validation("invalid history cursor"))
        })
        .transpose()
}

/// Loads one page of the rows of run `run_id`, [`HISTORY_MAX_PAGE_SIZE`] rows at most.
///
/// `filter` applies to the children, the keyset `cursor` is the previous page's `next_cursor`,
/// and `total` (first page) counts the matching children. `runs` is empty: the page of the run
/// row already carries the totals. A run with no matching rows (or one pruned meanwhile) is an
/// empty page, not an error.
pub fn history_run_children_impl(
    state: &AppState,
    workspace_id: String,
    run_id: String,
    filter: HistoryFilter,
    cursor: Option<String>,
) -> Result<HistoryPageDto, AppError> {
    if run_id.trim().is_empty() {
        return Err(AppError::validation("run id cannot be blank"));
    }
    let cursor = decode_cursor(cursor)?;
    let rows = state
        .store()
        .query_run_children(
            &workspace_id,
            &run_id,
            &filter,
            HISTORY_MAX_PAGE_SIZE,
            cursor.as_ref(),
        )
        .map_err(command_error)?;
    Ok(HistoryPageDto {
        entries: rows.entries,
        next_cursor: rows.next_cursor.as_ref().map(HistoryCursor::encode),
        total: rows.total,
        runs: rows.runs,
        workspace_total: None,
        mock_dropped: None,
    })
}

/// Whether a save destination is a collection or a folder.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "lowercase")]
pub enum SaveDestinationKind {
    /// A collection root.
    Collection,
    /// A folder inside a collection.
    Folder,
}

/// One pickable "save request into…" target, in display order.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveDestinationRow {
    /// Stable list key: `collection:<id>` or `folder:<id>`.
    pub key: String,
    /// The collection or folder name.
    pub label: String,
    /// Collection or folder.
    pub kind: SaveDestinationKind,
    /// Nesting level: 0 for a collection, 1 for its top-level folders, and so on.
    pub depth: u32,
    /// The owning collection (the collection itself for a collection row).
    pub collection_id: String,
    /// The folder, or `None` for a collection row.
    pub folder_id: Option<String>,
}

/// Case-insensitive name order, uppercase after lowercase on a tie (close to `localeCompare`).
fn name_order(left: &str, right: &str) -> std::cmp::Ordering {
    left.to_lowercase()
        .cmp(&right.to_lowercase())
        .then_with(|| right.cmp(left))
}

fn push_folders(
    rows: &mut Vec<SaveDestinationRow>,
    children: &BTreeMap<Option<&str>, Vec<&Folder>>,
    collection_id: &str,
    parent: Option<&str>,
    depth: u32,
) {
    for folder in children.get(&parent).into_iter().flatten() {
        rows.push(SaveDestinationRow {
            key: format!("folder:{}", folder.id.as_str()),
            label: folder.name.clone(),
            kind: SaveDestinationKind::Folder,
            depth,
            collection_id: collection_id.to_owned(),
            folder_id: Some(folder.id.as_str().to_owned()),
        });
        push_folders(
            rows,
            children,
            collection_id,
            Some(folder.id.as_str()),
            depth + 1,
        );
    }
}

/// Lists every collection of the workspace followed by its folder tree, siblings ordered by
/// position then name; folders whose parent is missing are not reachable and are left out.
pub fn request_save_destinations(
    state: &AppState,
    workspace_id: String,
) -> Result<Vec<SaveDestinationRow>, AppError> {
    let (collections, folders) = {
        let store = state.store();
        let collections = store
            .list_collections(&workspace_id)
            .map_err(command_error)?;
        let mut folders = Vec::new();
        for collection in &collections {
            folders.push(
                store
                    .list_folders(&collection.workspace_id, &collection.id)
                    .map_err(command_error)?,
            );
        }
        (collections, folders)
    };
    let mut paired: Vec<_> = collections.into_iter().zip(folders).collect();
    paired.sort_by(|(a, _), (b, _)| {
        a.position
            .cmp(&b.position)
            .then_with(|| name_order(&a.name, &b.name))
    });

    let mut rows = Vec::new();
    for (collection, folders) in &paired {
        rows.push(SaveDestinationRow {
            key: format!("collection:{}", collection.id.as_str()),
            label: collection.name.clone(),
            kind: SaveDestinationKind::Collection,
            depth: 0,
            collection_id: collection.id.as_str().to_owned(),
            folder_id: None,
        });
        let mut children: BTreeMap<Option<&str>, Vec<&Folder>> = BTreeMap::new();
        for folder in folders {
            children
                .entry(folder.parent_folder_id.as_ref().map(|id| id.as_str()))
                .or_default()
                .push(folder);
        }
        for siblings in children.values_mut() {
            siblings.sort_by(|a, b| {
                a.position
                    .cmp(&b.position)
                    .then_with(|| name_order(&a.name, &b.name))
            });
        }
        push_folders(&mut rows, &children, collection.id.as_str(), None, 1);
    }
    Ok(rows)
}
