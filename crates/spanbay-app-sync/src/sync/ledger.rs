//! The local ledger of one sync folder: what this machine knows about the folder between two
//! exports or imports. It lives in the export-file table of the main database, never in the
//! folder, under three kinds of rows keyed by the folder path:
//!
//! - a relative file path with the SHA-256 of the bytes this machine wrote there, or found there
//!   and took in on import: the file is "ours", unchanged since we last saw it, and ours to remove
//!   once its entity is gone;
//! - `#entity/<kind>/<key>` (empty value): the entity was in the folder when this machine last
//!   exported to it or imported from it. An entity in this set that the folder no longer holds
//!   while it still exists here was deleted by another machine;
//! - `#status`: the outcome of the last export (JSON of [`StoredStatus`]), so a stopped
//!   auto-export is still reported after a restart.
//!
//! The reserved rows never match `is_known_path`, so stale-file cleanup never touches them.

use std::collections::{HashMap, HashSet};
use std::path::Path;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use crate::error::AppError;
use crate::workspace::WorkspaceStore;

use super::{SyncErrorKind, SyncKind};

/// Prefix of the rows naming an entity seen in the folder.
const ENTITY_PREFIX: &str = "#entity/";
/// Row holding the outcome of the last export.
const STATUS_PATH: &str = "#status";

/// An entity identity: its kind and key.
pub(crate) type EntityId = (SyncKind, String);

/// The outcome of the last export into a folder, as the ledger keeps it.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(crate) struct StoredStatus {
    #[serde(default)]
    pub(crate) last_export_at_ms: Option<i64>,
    #[serde(default)]
    pub(crate) last_error: Option<String>,
    #[serde(default)]
    pub(crate) last_error_kind: Option<SyncErrorKind>,
    #[serde(default)]
    pub(crate) last_error_at_ms: Option<i64>,
}

/// The ledger of one folder.
#[derive(Debug, Default)]
pub(crate) struct Ledger {
    /// Files this machine wrote or took in: relative path to SHA-256.
    pub(crate) files: HashMap<String, String>,
    /// Entities in the folder at this machine's last export or import.
    pub(crate) seen: HashSet<EntityId>,
    /// The last export's outcome.
    pub(crate) status: Option<StoredStatus>,
}

fn kind_name(kind: SyncKind) -> String {
    match serde_json::to_value(kind) {
        Ok(Value::String(name)) => name,
        _ => format!("{kind:?}"),
    }
}

fn entity_path((kind, key): &EntityId) -> String {
    format!("{ENTITY_PREFIX}{}/{key}", kind_name(*kind))
}

fn parse_entity(path: &str) -> Option<EntityId> {
    let (kind, key) = path.strip_prefix(ENTITY_PREFIX)?.split_once('/')?;
    let kind = serde_json::from_value(Value::String(kind.to_owned())).ok()?;
    Some((kind, key.to_owned()))
}

/// The key the ledger rows of `dir` are filed under.
pub(crate) fn folder_key(dir: &Path) -> String {
    dir.display().to_string()
}

impl Ledger {
    /// Reads the ledger of `dir`.
    pub(crate) fn load(store: &WorkspaceStore, dir: &Path) -> Result<Self, AppError> {
        let rows = store
            .list_sync_export_files(&folder_key(dir))
            .map_err(AppError::from)?;
        let mut ledger = Self::default();
        for (path, value) in rows {
            if path == STATUS_PATH {
                ledger.status = serde_json::from_str(&value).ok();
            } else if let Some(entity) = parse_entity(&path) {
                ledger.seen.insert(entity);
            } else if !path.starts_with('#') {
                ledger.files.insert(path, value);
            }
        }
        Ok(ledger)
    }
}

/// Makes `next` the set of entities seen in `dir`, writing only the difference from what the
/// ledger holds now.
pub(crate) fn replace_seen(
    store: &WorkspaceStore,
    dir: &Path,
    next: &HashSet<EntityId>,
) -> Result<(), AppError> {
    let before = Ledger::load(store, dir)?.seen;
    let added: Vec<(String, String)> = next
        .difference(&before)
        .map(|entity| (entity_path(entity), String::new()))
        .collect();
    let gone: Vec<String> = before.difference(next).map(entity_path).collect();
    if added.is_empty() && gone.is_empty() {
        return Ok(());
    }
    store
        .update_sync_export_files(&folder_key(dir), &added, &gone)
        .map_err(AppError::from)
}

/// Records `files` (relative path, SHA-256) as taken in from `dir`.
pub(crate) fn acknowledge_files(
    store: &WorkspaceStore,
    dir: &Path,
    files: &[(String, String)],
) -> Result<(), AppError> {
    if files.is_empty() {
        return Ok(());
    }
    store
        .update_sync_export_files(&folder_key(dir), files, &[])
        .map_err(AppError::from)
}

/// Writes the outcome of the last export into `dir`.
pub(crate) fn save_status(
    store: &WorkspaceStore,
    dir: &Path,
    status: &StoredStatus,
) -> Result<(), AppError> {
    let text =
        serde_json::to_string(status).map_err(|error| AppError::internal(error.to_string()))?;
    store
        .update_sync_export_files(&folder_key(dir), &[(STATUS_PATH.to_owned(), text)], &[])
        .map_err(AppError::from)
}

#[cfg(test)]
mod tests {
    #![allow(clippy::unwrap_used, clippy::expect_used)]

    use super::*;

    #[test]
    fn entity_rows_round_trip_and_never_look_like_export_files() {
        let entity = (SyncKind::LooseRequests, "ws/1".to_owned());
        let path = entity_path(&entity);
        assert_eq!(parse_entity(&path), Some(entity));
        assert!(!super::super::export::is_known_path(&path));
        assert!(!super::super::export::is_known_path(STATUS_PATH));
        assert_eq!(parse_entity("workspaces/a/collections/b.json"), None);
    }
}
