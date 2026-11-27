//! Config sync: export the user's configuration to a folder of stable JSON files and import it back.
//!
//! Single responsibility: the file format, secret handling, diffing and merge rules of config sync.
//! The use-cases that tie it to the store and the settings live in `crate::sync_service`.
//!
//! Layout of the sync directory (every file pretty JSON, 2-space indent, trailing newline):
//! `manifest.json`, `settings.json`, `workspaces/<name>-<id>/{workspace,globals,requests}.json`,
//! `workspaces/<name>-<id>/{collections,environments,mocks,flows}/*.json`,
//! `llm/{providers,aliases}/*.json`, `agent/mcp-servers/*.json` and `agent/skills.json`.
//! Export is deterministic, so unchanged data re-exports byte for byte.

mod apply;
mod diff;
mod docs;
mod export;
mod folder;
mod ledger;
mod read;
mod secrets;

#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

use crate::app::sync_runtime::AUTO_EXPORT_DEBOUNCE;
pub(crate) use apply::apply;
pub(crate) use diff::{Item, compare};
pub(crate) use docs::{Doc, collect_local};
pub(crate) use export::{ExportOutcome, fingerprint_unchanged, write_dir};
pub use folder::SyncFolderRisk;
pub(crate) use folder::{classify, remove_secrets};
pub(crate) use ledger::{
    EntityId, Ledger, StoredStatus, acknowledge_files, replace_seen, save_status,
};
pub(crate) use read::read_dir;

/// Format name written to `manifest.json`.
pub const FORMAT_NAME: &str = "spanbay.config-sync";

/// Format version written to `manifest.json`. Version 2 adds key groups and protocol limits
/// and reads unknown fields; import also takes version 1 files and refuses newer ones.
pub const FORMAT_VERSION: u32 = 2;

/// The oldest format version import reads.
pub(crate) const OLDEST_FORMAT_VERSION: u32 = 1;

/// Largest file import reads, in bytes.
pub(crate) const MAX_FILE_BYTES: u64 = 5 * 1024 * 1024;

/// What a sync file holds. The declaration order is the order import applies kinds in: a
/// workspace before its contents, providers before the aliases that point at them.
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncKind {
    /// A workspace row.
    Workspace,
    /// A collection with its folders and requests.
    Collection,
    /// Requests of a workspace that are filed in no collection.
    LooseRequests,
    /// An environment.
    Environment,
    /// A workspace's global variables.
    Globals,
    /// A mock server.
    Mock,
    /// A flow.
    Flow,
    /// An LLM provider with its keys and models.
    LlmProvider,
    /// An LLM alias.
    LlmAlias,
    /// An MCP server (matched by name).
    McpServer,
    /// The enabled flags of the agent skills.
    Skills,
    /// One settings section (`sync` is never synced).
    Settings,
}

/// How an entity differs between the files and the local data.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncChange {
    /// Only in the files.
    Added,
    /// In both and different; the incoming copy wins (it is newer, or there is no timestamp).
    Changed,
    /// Only local. Reported, applied only with `apply_removals`. When the entity was in the
    /// folder at this machine's last export or import, another machine deleted it
    /// (`deleted_in_folder`).
    Removed,
    /// In both and different, and the local copy is newer: the local copy is kept.
    Conflict,
}

/// Which side an override keeps.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncChoice {
    /// Keep the local data.
    Local,
    /// Take the files' data.
    Incoming,
}

/// One entity that differs between the files and the local data. Unchanged entities are omitted.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncDiffEntry {
    /// Entity kind.
    pub kind: SyncKind,
    /// Stable identity: the entity id (the server name for MCP servers, the section for settings).
    pub key: String,
    /// Display name.
    pub name: String,
    /// How it differs.
    pub change: SyncChange,
    /// Local last-change time, when the entity has one.
    pub local_updated_at_ms: Option<i64>,
    /// Last-change time recorded in the file, when it has one.
    pub incoming_updated_at_ms: Option<i64>,
    /// What differs and what import would do.
    pub detail: String,
    /// Set for a provider whose base URL host or proxy changes, or that gains an outbound host (a
    /// custom balance URL elsewhere): its local keys are switched off on import (values kept)
    /// unless the preview's "Keep my key for the new host" is chosen. A new outbound host also
    /// clears the keys' console tokens, account ids and balance readings in every case.
    #[serde(default)]
    pub host_change: Option<HostChange>,
    /// The import would run or send something the user has not seen on this machine (a new or
    /// changed MCP command, a provider moving to another host): the preview shows an amber
    /// "Review" tag and `review_reason` says why.
    #[serde(default)]
    pub review: bool,
    /// One line naming what to review; `None` unless `review`.
    #[serde(default)]
    pub review_reason: Option<String>,
    /// A settings section whose machine-local fields (LAN binding, tool approvals, skill folders)
    /// differ in the files: those values are never imported and the local ones stay.
    #[serde(default)]
    pub local_setting_kept: bool,
    /// A `Removed` entity that was in the folder at this machine's last export or import: another
    /// machine deleted it. Auto-export stops until the removal is applied or the local copy is
    /// kept on purpose, so the deletion is never undone by a write-back.
    #[serde(default)]
    pub deleted_in_folder: bool,
}

/// A provider that the files move to another host.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostChange {
    /// The host of the local provider.
    pub from_host: String,
    /// The host the files name.
    pub to_host: String,
}

/// Differences of one entity kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncKindCount {
    /// Entity kind.
    pub kind: SyncKind,
    /// Entities only in the files.
    pub added: u32,
    /// Entities the incoming copy would replace.
    pub changed: u32,
    /// Entities only local.
    pub removed: u32,
    /// Entities where the newer local copy is kept.
    pub conflicts: u32,
}

/// What importing a directory would do.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncPreview {
    /// The directory that was read.
    pub directory: String,
    /// Every differing entity, ordered by kind then key.
    pub entries: Vec<SyncDiffEntry>,
    /// Counts per kind that has differences.
    pub counts: Vec<SyncKindCount>,
    /// The `Conflict` entries again: what the merge keeps local.
    pub conflicts: Vec<SyncDiffEntry>,
    /// Things worth knowing that are not differences (placeholders without a local value, invalid
    /// settings sections).
    pub warnings: Vec<String>,
    /// Fingerprint of the folder content this preview read. Passed back as
    /// `SyncImportOptions::preview_token`, it makes the import refuse a folder that changed since.
    pub token: String,
}

/// A per-entity decision that replaces the merge rule.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncOverride {
    /// Entity kind.
    pub kind: SyncKind,
    /// The entry's `key`.
    pub key: String,
    /// The side to keep.
    pub choice: SyncChoice,
}

/// Options of an import.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct SyncImportOptions {
    /// Delete local entities that are not in the files. Off by default.
    pub apply_removals: bool,
    /// Keep a provider's local keys that the files do not list. On by default (and then left out
    /// of the serialized form); off deletes them.
    #[serde(skip_serializing_if = "is_true")]
    pub keep_local_keys: bool,
    /// Overrides of the merge rule, by entity.
    pub overrides: Vec<SyncOverride>,
    /// Ids of providers whose base URL host changes and whose local keys the user chose to keep
    /// switched on for the new host ("Keep my key for the new host"); every other such provider's
    /// keys are switched off, their values kept.
    pub keep_key_providers: Vec<String>,
    /// The `token` of the preview the user decided on: when set and the folder no longer reads the
    /// same, the import refuses and asks for a new preview.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub preview_token: Option<String>,
}

#[allow(clippy::trivially_copy_pass_by_ref)]
const fn is_true(value: &bool) -> bool {
    *value
}

impl Default for SyncImportOptions {
    fn default() -> Self {
        Self {
            apply_removals: false,
            keep_local_keys: true,
            overrides: Vec::new(),
            keep_key_providers: Vec::new(),
            preview_token: None,
        }
    }
}

/// An entity import left alone, and why.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncSkipped {
    /// Entity kind.
    pub kind: SyncKind,
    /// The entry's `key`.
    pub key: String,
    /// Why nothing was written.
    pub reason: String,
}

/// What an import changed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncImportResult {
    /// Entities created.
    pub added: u32,
    /// Entities replaced by the incoming copy.
    pub updated: u32,
    /// Entities deleted (only with `apply_removals`).
    pub removed: u32,
    /// Entities where the local copy was kept (conflicts and `Local` overrides).
    pub kept_local: u32,
    /// Entities that could not be applied.
    pub skipped_with_reason: Vec<SyncSkipped>,
    /// Notes such as secret placeholders imported empty.
    pub warnings: Vec<String>,
}

/// What an export wrote.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncExportResult {
    /// The directory written.
    pub directory: String,
    /// Files created or changed (unchanged files are left untouched).
    pub files_written: u32,
    /// Stale files this machine wrote in an earlier export that were removed.
    pub files_removed: u32,
    /// Relative paths left untouched because the file in the directory records a newer
    /// `updated_at_ms` than the local entity (another machine's newer edit): import first.
    pub skipped_newer: Vec<String>,
    /// Whether secret values are in the files.
    pub secrets_included: bool,
}

/// Why the last export into the folder did not happen.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncErrorKind {
    /// The folder holds changes or deletions this machine has not imported: preview and import.
    PendingIncoming,
    /// Anything else (an unreadable folder, a file error, secrets refused for a shared folder).
    Failed,
}

/// The sync settings and the last export into the configured folder. The outcome is kept in the
/// local ledger, so a stopped auto-export is still reported after a restart.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncStatus {
    /// The configured directory (empty when unset).
    pub directory: String,
    /// Whether exports include secret values.
    pub include_secrets: bool,
    /// Whether changes are exported automatically.
    pub auto_export: bool,
    /// When the last export into the folder finished.
    pub last_export_at_ms: Option<i64>,
    /// Why the last export failed, if it did.
    pub last_error: Option<String>,
    /// What kind of failure `last_error` is.
    #[serde(default)]
    pub last_error_kind: Option<SyncErrorKind>,
    /// When the export failed.
    #[serde(default)]
    pub last_error_at_ms: Option<i64>,
}

/// Wires the debounced background auto-exporter into `state.sync` for disk-backed databases.
pub fn attach_auto_export(state: &crate::app::AppState) {
    if let crate::llm::runtime::DbHandle::Path { path, .. } = state.llm().db() {
        let path = path.clone();
        let skills_dir = state.data_dirs().map(|dirs| dirs.skills.clone());
        state
            .sync
            .install_auto_exporter(AUTO_EXPORT_DEBOUNCE, move || {
                crate::sync_service::auto_export_job(&path, skills_dir.as_deref())
            });
    }
}
