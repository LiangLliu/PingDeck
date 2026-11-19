//! Config sync use-cases: export, import preview, import and status.
//!
//! Single responsibility: gather local data under the store lock, then do the file work and the
//! diffing without it. The file format and merge rules live in `crate::sync`.

use std::collections::{BTreeSet, HashMap, HashSet};
use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::settings::Settings;
use crate::sync::{
    self, EntityId, Ledger, StoredStatus, SyncChange, SyncChoice, SyncErrorKind, SyncExportResult,
    SyncImportOptions, SyncImportResult, SyncKind, SyncKindCount, SyncPreview, SyncStatus,
};
use crate::workspace::WorkspaceStore;

use crate::agent::skills::discover;
use crate::app::{AppState, command_error, current_time_ms};

/// The directory to use: the explicit argument, else `settings.sync.directory`.
fn resolve_directory(directory: Option<String>, settings: &Settings) -> Result<PathBuf, AppError> {
    let chosen = directory
        .filter(|dir| !dir.trim().is_empty())
        .unwrap_or_else(|| settings.sync.directory.clone());
    if chosen.trim().is_empty() {
        return Err(AppError::validation(
            "no sync directory: choose one in the sync settings or pass a directory",
        ));
    }
    Ok(PathBuf::from(chosen))
}

/// Local data as sync documents. Skill folders are scanned before the store lock is taken.
fn snapshot(state: &AppState, settings: &Settings) -> Result<Vec<sync::Doc>, AppError> {
    let skills = discover(
        settings,
        state.agent.managed_skills_dir.as_deref(),
        None,
        false,
    );
    let store = state.store();
    sync::collect_local(&store, settings, &skills)
}

/// How an export reaches the database: the app's shared store (taken only for the moment of each
/// read or write, never across the file work) or the auto-exporter's own connection.
trait StoreAccess {
    fn with_store<T>(&self, f: impl FnOnce(&WorkspaceStore) -> T) -> T;
}

impl StoreAccess for AppState {
    fn with_store<T>(&self, f: impl FnOnce(&WorkspaceStore) -> T) -> T {
        f(&self.store())
    }
}

impl StoreAccess for WorkspaceStore {
    fn with_store<T>(&self, f: impl FnOnce(&WorkspaceStore) -> T) -> T {
        f(self)
    }
}

/// The entities an export puts into the folder: every local document but the skill flags and
/// the settings sections, which every machine has.
fn exported_entities(docs: &[sync::Doc]) -> HashSet<EntityId> {
    docs.iter()
        .filter(|doc| !matches!(doc.kind, SyncKind::Skills | SyncKind::Settings))
        .map(|doc| (doc.kind, doc.key.clone()))
        .collect()
}

/// Whether `item` is a change in the folder this machine has not taken in: an added, changed or
/// conflicting file that is not byte-identical to what this machine last wrote or imported (so a
/// newer local edit never silently overwrites another machine's file), or an entity another
/// machine deleted.
fn pending_incoming(item: &sync::Item, files: &HashMap<String, String>) -> bool {
    match item.entry.change {
        SyncChange::Added | SyncChange::Changed | SyncChange::Conflict => !item
            .incoming
            .as_ref()
            .and_then(|doc| Some((doc.file.as_ref()?, doc.fingerprint.as_ref()?)))
            .is_some_and(|(file, fp)| sync::fingerprint_unchanged(files.get(file), fp)),
        SyncChange::Removed => item.entry.deleted_in_folder,
    }
}

/// The checks of an export that must not overwrite anything: with secrets on, no git or
/// cloud-synced folder (writing secrets there needs the typed confirmation of the settings page
/// and stays a manual export); and no change in the folder this machine has not imported. Only
/// a missing directory or manifest means "no export yet"; any other read failure (a file with
/// conflict markers after a `git pull`, an oversized or special file, an unreadable folder)
/// fails the export, which then writes nothing.
fn guard(
    dir: &Path,
    docs: &[sync::Doc],
    settings: &Settings,
    ledger: &Ledger,
) -> Result<(), AppError> {
    if settings.sync.include_secrets && sync::classify(dir).risky {
        return Err(AppError::validation(
            "auto-export does not write secrets into a git or cloud-synced folder: turn off \"Include secrets\" or export by hand",
        ));
    }
    match sync::read_dir(dir) {
        Ok(incoming) => {
            let (items, _) = sync::compare(docs, incoming, settings, &[], &ledger.seen);
            let deleted = items
                .iter()
                .filter(|item| item.entry.deleted_in_folder)
                .count();
            let changed = items
                .iter()
                .filter(|item| !item.entry.deleted_in_folder)
                .filter(|item| pending_incoming(item, &ledger.files))
                .count();
            if deleted + changed > 0 {
                return Err(AppError::conflict(format!(
                    "incoming changes pending ({changed} changed, {deleted} deleted in the folder): preview and import first"
                )));
            }
            Ok(())
        }
        Err(AppError::NotFound { .. }) => Ok(()),
        Err(error) => Err(error),
    }
}

/// Writes `docs` into `dir` (after [`guard`] when `guarded`) and brings the ledger up to date:
/// the files written, the entities now in the folder.
fn export_docs(
    access: &impl StoreAccess,
    dir: &Path,
    docs: &[sync::Doc],
    settings: &Settings,
    guarded: bool,
) -> Result<SyncExportResult, AppError> {
    let ledger = access.with_store(|store| Ledger::load(store, dir))?;
    if guarded {
        guard(dir, docs, settings, &ledger)?;
    }
    let exported = sync::write_dir(
        dir,
        docs,
        settings.sync.include_secrets,
        current_time_ms(),
        &ledger.files,
    )?;
    access.with_store(|store| {
        remember(store, dir, &exported)?;
        sync::replace_seen(store, dir, &exported_entities(docs))
    })?;
    Ok(exported.result)
}

/// Keeps the outcome of an export of `dir` in the ledger (best effort: a failure to record it
/// must not turn a finished export into an error).
fn record_outcome<T>(access: &impl StoreAccess, dir: &Path, outcome: &Result<T, AppError>) {
    let now = current_time_ms();
    access.with_store(|store| {
        let mut status = Ledger::load(store, dir)
            .ok()
            .and_then(|ledger| ledger.status)
            .unwrap_or_default();
        match outcome {
            Ok(_) => {
                status.last_export_at_ms = Some(now);
                status.last_error = None;
                status.last_error_kind = None;
                status.last_error_at_ms = None;
            }
            Err(error) => {
                status.last_error = Some(error.message().to_owned());
                status.last_error_kind = Some(match error {
                    AppError::Conflict { .. } => SyncErrorKind::PendingIncoming,
                    _ => SyncErrorKind::Failed,
                });
                status.last_error_at_ms = Some(now);
            }
        }
        let _ = sync::save_status(store, dir, &status);
    });
}

/// Writes the whole configuration to `directory` (default: the configured sync directory).
///
/// Unchanged files are not rewritten, a file another machine changed more recently is left alone
/// (`skipped_newer`) and only stale files this machine wrote earlier are removed; secrets are
/// included only when `settings.sync.include_secrets` is on. "Export now" is the user keeping
/// the local copy on purpose and runs unguarded; `guarded` (the status line's Retry) runs the
/// auto-exporter's checks first and writes nothing while the folder holds changes this machine
/// has not imported.
pub fn sync_export_impl(
    state: &AppState,
    directory: Option<String>,
) -> Result<SyncExportResult, AppError> {
    sync_export_with_impl(state, directory, false)
}

/// [`sync_export_impl`], guarded or not.
pub fn sync_export_with_impl(
    state: &AppState,
    directory: Option<String>,
    guarded: bool,
) -> Result<SyncExportResult, AppError> {
    let settings = state.settings();
    let dir = resolve_directory(directory, &settings)?;
    let outcome = snapshot(state, &settings)
        .and_then(|docs| export_docs(state, &dir, &docs, &settings, guarded));
    record_outcome(state, &dir, &outcome);
    state.sync.record(
        &outcome
            .as_ref()
            .map(drop)
            .map_err(|e| e.message().to_owned()),
        current_time_ms(),
    );
    outcome
}

/// Records in the local ledger which files an export wrote into `dir`.
fn remember(
    store: &WorkspaceStore,
    dir: &Path,
    exported: &sync::ExportOutcome,
) -> Result<(), AppError> {
    store
        .update_sync_export_files(
            &dir.display().to_string(),
            &exported.owned,
            &exported.forgotten,
        )
        .map_err(command_error)
}

/// What the background exporter runs: exports when `auto_export` is on and a directory is set.
/// It reads the database through its own connection, so it never contends for the app's store
/// lock. It runs the [`guard`] checks first: it never exports while the folder holds changes or
/// deletions this machine has not imported. The outcome is kept in the ledger, so the status
/// line and the attention list still show a stopped auto-export after a restart. Returns
/// whether it exported.
pub(crate) fn auto_export_job(
    db_path: &Path,
    managed_skills_dir: Option<&Path>,
) -> Result<bool, AppError> {
    let store = WorkspaceStore::open(db_path).map_err(command_error)?;
    let settings = Settings::from_rows(&store.list_app_settings().map_err(command_error)?);
    if !settings.sync.auto_export || settings.sync.directory.is_empty() {
        return Ok(false);
    }
    let dir = Path::new(&settings.sync.directory);
    let skills = discover(&settings, managed_skills_dir, None, false);
    let outcome = sync::collect_local(&store, &settings, &skills)
        .and_then(|docs| export_docs(&store, dir, &docs, &settings, true));
    record_outcome(&store, dir, &outcome);
    outcome.map(|_| true)
}

/// What reading a folder for a preview or an import found.
struct Diff {
    directory: PathBuf,
    items: Vec<sync::Item>,
    warnings: Vec<String>,
    /// `(entity, file, fingerprint)` of every incoming document.
    incoming: Vec<(EntityId, String, String)>,
    token: String,
    ledger: Ledger,
}

/// The fingerprint of the folder content `incoming` was read from.
fn content_token(incoming: &[sync::Doc]) -> String {
    let lines: BTreeSet<String> = incoming
        .iter()
        .filter_map(|doc| {
            Some(format!(
                "{}\t{}\n",
                doc.file.as_ref()?,
                doc.fingerprint.as_ref()?
            ))
        })
        .collect();
    crate::crypto::sha256_hex(lines.into_iter().collect::<String>().as_bytes())
}

/// Reads `directory` and compares it with the local data.
fn diff(
    state: &AppState,
    directory: Option<String>,
    keep_key_providers: &[String],
) -> Result<Diff, AppError> {
    let settings = state.settings();
    let dir = resolve_directory(directory, &settings)?;
    let incoming = sync::read_dir(&dir)?;
    let local = snapshot(state, &settings)?;
    let ledger = state.with_store(|store| Ledger::load(store, &dir))?;
    let token = content_token(&incoming);
    let files = incoming
        .iter()
        .filter_map(|doc| {
            Some((
                (doc.kind, doc.key.clone()),
                doc.file.clone()?,
                doc.fingerprint.clone()?,
            ))
        })
        .collect();
    let (items, warnings) = sync::compare(
        &local,
        incoming,
        &settings,
        keep_key_providers,
        &ledger.seen,
    );
    Ok(Diff {
        directory: dir,
        items,
        warnings,
        incoming: files,
        token,
        ledger,
    })
}

/// Shows what importing `directory` would do, without changing anything.
pub fn sync_preview_impl(
    state: &AppState,
    directory: Option<String>,
) -> Result<SyncPreview, AppError> {
    let Diff {
        directory,
        items,
        warnings,
        token,
        ..
    } = diff(state, directory, &[])?;
    let mut counts: Vec<SyncKindCount> = Vec::new();
    for item in &items {
        let kind = item.entry.kind;
        if counts.last().is_none_or(|count| count.kind != kind) {
            counts.push(SyncKindCount {
                kind,
                added: 0,
                changed: 0,
                removed: 0,
                conflicts: 0,
            });
        }
        if let Some(count) = counts.last_mut() {
            match item.entry.change {
                SyncChange::Added => count.added += 1,
                SyncChange::Changed => count.changed += 1,
                SyncChange::Removed => count.removed += 1,
                SyncChange::Conflict => count.conflicts += 1,
            }
        }
    }
    let entries: Vec<_> = items.into_iter().map(|item| item.entry).collect();
    let conflicts = entries
        .iter()
        .filter(|entry| entry.change == SyncChange::Conflict)
        .cloned()
        .collect();
    Ok(SyncPreview {
        directory: directory.display().to_string(),
        entries,
        counts,
        conflicts,
        warnings,
        token,
    })
}

/// Merges `directory` into the local data: last writer wins on `updated_at`, the incoming copy
/// wins ties, local removals happen only with `options.apply_removals`, secrets stay local. With
/// `options.preview_token` the folder must still read as it did for that preview.
///
/// Afterwards the ledger learns what this machine took in: the files of every entity that now
/// exists here as the folder has it or as the user kept it (an export may then replace them), and
/// the entities now in the folder. An entity another machine deleted stays "seen" (auto-export
/// keeps stopping) until the removal is applied, or the user keeps the local copy (a `Local`
/// override, or Export now).
pub fn sync_import_impl(
    state: &AppState,
    directory: Option<String>,
    options: &SyncImportOptions,
) -> Result<SyncImportResult, AppError> {
    let Diff {
        directory,
        items,
        warnings,
        incoming,
        token,
        ledger,
    } = diff(state, directory, &options.keep_key_providers)?;
    if options
        .preview_token
        .as_ref()
        .is_some_and(|expected| *expected != token)
    {
        return Err(AppError::conflict(
            "the sync folder changed since the preview: preview it again before importing",
        ));
    }
    let choices: HashMap<EntityId, SyncChoice> = options
        .overrides
        .iter()
        .map(|choice| ((choice.kind, choice.key.clone()), choice.choice))
        .collect();
    let changes: HashMap<EntityId, (SyncChange, bool)> = items
        .iter()
        .map(|item| {
            let id = (item.entry.kind, item.entry.key.clone());
            let taken = match item.entry.change {
                SyncChange::Conflict => choices.get(&id) == Some(&SyncChoice::Incoming),
                _ => choices.get(&id) != Some(&SyncChoice::Local),
            };
            (id, (item.entry.change, taken))
        })
        .collect();
    let (result, failed) = sync::apply(state, items, options, warnings);

    let mut acknowledged: HashMap<String, String> = HashMap::new();
    let mut vetoed: HashSet<String> = HashSet::new();
    let mut seen: HashSet<EntityId> = HashSet::new();
    for (id, file, fingerprint) in incoming {
        let present_here = match changes.get(&id) {
            None => true,
            Some((SyncChange::Added, taken)) => *taken,
            Some(_) => true,
        };
        if failed.contains(&id) || !present_here {
            vetoed.insert(file.clone());
        } else {
            acknowledged.insert(file, fingerprint);
        }
        if !matches!(id.0, SyncKind::Skills | SyncKind::Settings) {
            seen.insert(id);
        }
    }
    for (id, (change, taken)) in &changes {
        // Deleted elsewhere, kept here without the user choosing to keep it: still "seen".
        let removed_here = *taken && options.apply_removals && !failed.contains(id);
        let kept_on_purpose = choices.get(id) == Some(&SyncChoice::Local);
        if *change == SyncChange::Removed
            && ledger.seen.contains(id)
            && !removed_here
            && !kept_on_purpose
        {
            seen.insert(id.clone());
        }
    }
    let files: Vec<(String, String)> = acknowledged
        .into_iter()
        .filter(|(file, _)| !vetoed.contains(file))
        .collect();
    state.with_store(|store| -> Result<(), AppError> {
        sync::acknowledge_files(store, &directory, &files)?;
        sync::replace_seen(store, &directory, &seen)?;
        // A stop for pending changes is resolved by importing them; the next export says
        // whether anything is still pending.
        if let Some(mut status) = ledger.status.clone()
            && status.last_error_kind == Some(SyncErrorKind::PendingIncoming)
        {
            status.last_error = None;
            status.last_error_kind = None;
            status.last_error_at_ms = None;
            sync::save_status(store, &directory, &status)?;
        }
        Ok(())
    })?;
    state.sync.notify_changed();
    Ok(result)
}

/// Where the sync folder sits and whether it holds secret values: inside a git working tree or a
/// known cloud-sync folder (the dialog of "Include secrets in exports" names the location and asks
/// for a typed phrase), and whether "Remove secrets from this folder" has anything to do.
/// `directory` defaults to the configured sync folder.
pub fn sync_folder_risk_impl(
    state: &AppState,
    directory: Option<String>,
) -> Result<sync::SyncFolderRisk, AppError> {
    let dir = resolve_directory(directory, &state.settings())?;
    Ok(sync::classify(&dir))
}

/// What "Remove secrets from this folder" did.
#[derive(Debug, Clone, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
pub struct SyncRemoveSecretsResult {
    /// Files rewritten with placeholders in place of values.
    pub files_rewritten: u32,
}

/// Rewrites the exported files of the sync folder that hold secret values with placeholders
/// and tells the local ledger about the new bytes, so the next export does not read
/// them as another machine's edit.
///
/// # Errors
/// [`AppError::Io`] when a file cannot be rewritten.
pub fn sync_remove_secrets_impl(
    state: &AppState,
    directory: Option<String>,
) -> Result<SyncRemoveSecretsResult, AppError> {
    let dir = resolve_directory(directory, &state.settings())?;
    let written = sync::remove_secrets(&dir)?;
    let key = dir.display().to_string();
    let store = state.store();
    let owned = store.list_sync_export_files(&key).map_err(command_error)?;
    let known: Vec<(String, String)> = written
        .iter()
        .filter(|(path, _)| owned.contains_key(path))
        .cloned()
        .collect();
    store
        .update_sync_export_files(&key, &known, &[])
        .map_err(command_error)?;
    Ok(SyncRemoveSecretsResult {
        files_rewritten: u32::try_from(written.len()).unwrap_or(u32::MAX),
    })
}

/// The sync settings and the outcome of the last export into the configured folder, as the
/// ledger keeps it (so it survives a restart); this session's in-memory record when the ledger
/// has none.
#[must_use]
pub fn sync_status_impl(state: &AppState) -> SyncStatus {
    let settings = state.settings();
    let stored = (!settings.sync.directory.trim().is_empty())
        .then(|| state.with_store(|store| Ledger::load(store, Path::new(&settings.sync.directory))))
        .and_then(Result::ok)
        .and_then(|ledger| ledger.status);
    let status = stored.unwrap_or_else(|| {
        let (last_export_at_ms, last_error) = state.sync.last();
        StoredStatus {
            last_export_at_ms,
            last_error_kind: last_error.as_ref().map(|_| SyncErrorKind::Failed),
            last_error,
            last_error_at_ms: None,
        }
    });
    SyncStatus {
        directory: settings.sync.directory.clone(),
        include_secrets: settings.sync.include_secrets,
        auto_export: settings.sync.auto_export,
        last_export_at_ms: status.last_export_at_ms,
        last_error: status.last_error,
        last_error_kind: status.last_error_kind,
        last_error_at_ms: status.last_error_at_ms,
    }
}

/// Where a sync folder sits, as the sync settings ask.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SyncFolderKind {
    /// Neither a git working tree nor a cloud-synced folder.
    Plain,
    /// Inside a git working tree (a `.git` above the folder).
    Git,
    /// Inside a cloud-synced folder (`cloud_name` says which).
    Cloud,
}

/// What a folder is and whether exported files in it already hold secret values.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SyncFolderInfoDto {
    /// The folder exists.
    pub exists: bool,
    /// Where it sits; a folder in both a cloud folder and a git tree is `cloud`.
    pub kind: SyncFolderKind,
    /// iCloud Drive, Dropbox, Google Drive, OneDrive or Syncthing; set only for `cloud`.
    pub cloud_name: Option<String>,
    /// Some exported file holds a secret value instead of a placeholder.
    pub holds_secrets: bool,
    /// How many files hold one.
    pub files_with_secrets: u32,
}

/// Looks at `path` before "Include secrets in exports" is turned on. Blocking: it reads the
/// exported files of the folder.
///
/// # Errors
/// [`AppError::Validation`] for a blank path.
pub fn inspect_sync_folder_impl(
    state: &AppState,
    path: &str,
) -> Result<SyncFolderInfoDto, AppError> {
    if path.trim().is_empty() {
        return Err(AppError::validation("the sync folder path is empty"));
    }
    let risk = sync_folder_risk_impl(state, Some(path.to_owned()))?;
    let kind = match (&risk.cloud_service, risk.in_git) {
        (Some(_), _) => SyncFolderKind::Cloud,
        (None, true) => SyncFolderKind::Git,
        (None, false) => SyncFolderKind::Plain,
    };
    Ok(SyncFolderInfoDto {
        exists: Path::new(&risk.directory).is_dir(),
        kind,
        cloud_name: risk.cloud_service,
        holds_secrets: risk.holds_secret_values,
        files_with_secrets: risk.files_with_secrets,
    })
}

/// "Remove secrets from this folder": rewrites the configured sync folder's files with
/// placeholders; returns how many files were rewritten.
///
/// # Errors
/// [`AppError::Validation`] without a sync folder, [`AppError::Io`] when a file cannot be written.
pub fn remove_secrets_from_folder_impl(state: &AppState) -> Result<u32, AppError> {
    sync_remove_secrets_impl(state, None).map(|result| result.files_rewritten)
}

#[cfg(test)]
mod folder_tests {
    use super::*;
    use std::fs;
    use std::sync::atomic::{AtomicUsize, Ordering};

    struct TempDir(PathBuf);
    impl TempDir {
        fn new() -> Self {
            static COUNTER: AtomicUsize = AtomicUsize::new(0);
            let path = std::env::temp_dir().join(format!(
                "pingdeck-sync-folder-{}-{}",
                std::process::id(),
                COUNTER.fetch_add(1, Ordering::SeqCst)
            ));
            fs::create_dir_all(&path).expect("temp dir");
            Self(path)
        }
        fn path(&self) -> &Path {
            &self.0
        }
    }
    impl Drop for TempDir {
        fn drop(&mut self) {
            let _ = fs::remove_dir_all(&self.0);
        }
    }

    #[test]
    fn inspecting_a_sync_folder_names_git_and_cloud_places() {
        let dir = TempDir::new();
        let state = AppState::open_in_memory().expect("state opens");
        let plain = dir.path().join("plain");
        let repo = dir.path().join("repo/sub");
        let dropbox = dir.path().join("Dropbox/pingdeck");
        for folder in [&plain, &repo, &dropbox] {
            fs::create_dir_all(folder).expect("folder");
        }
        fs::create_dir_all(dir.path().join("repo/.git")).expect("git dir");
        let inspect = |path: &Path| {
            inspect_sync_folder_impl(&state, &path.display().to_string()).expect("inspection")
        };

        assert_eq!(
            inspect(&plain),
            SyncFolderInfoDto {
                exists: true,
                kind: SyncFolderKind::Plain,
                cloud_name: None,
                holds_secrets: false,
                files_with_secrets: 0,
            }
        );
        assert_eq!(inspect(&repo).kind, SyncFolderKind::Git);
        let cloud = inspect(&dropbox);
        assert_eq!(cloud.kind, SyncFolderKind::Cloud);
        assert_eq!(cloud.cloud_name.as_deref(), Some("Dropbox"));
        assert!(!inspect(&plain.join("not-yet")).exists);
        assert!(matches!(
            inspect_sync_folder_impl(&state, "  "),
            Err(AppError::Validation { .. })
        ));
    }

    #[test]
    fn removing_secrets_needs_a_sync_folder() {
        let dir = TempDir::new();
        let state = AppState::open(dir.path().join("pingdeck.db")).expect("state opens");
        state.on_start().expect("start");
        assert!(matches!(
            remove_secrets_from_folder_impl(&state),
            Err(AppError::Validation { .. })
        ));
    }
}
