//! Atomic, hash-checked apply and restore of the config files.
//!
//! Apply refuses what it cannot write safely (a dangling symlink, a hard-linked file, a file
//! that changed after the reviewed preview), then stages everything (directories, backups in the
//! data folder, one `.<name>.spanbay-tmp` per changed file beside the resolved target) and only
//! then renames. A failure while staging replaces no target (new directories may stay behind);
//! a failed rename, or a file that changed under the write, rolls the
//! files already renamed back from the content read before the write. The caller persists the
//! returned [`AppliedFile`] entries (`llm_client_applied`) and hands them back to [`restore`].

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::backup::{
    KeptOriginal, backup_dir, clear_purged, create_backup, keep_original, put_original,
    read_original, refresh_original,
};
use super::fsio::{
    GROUP_OTHER_BITS, check_target, current_sha, existing_mode, replace_file, tmp_path,
    write_error, write_tmp,
};
#[cfg(doc)]
use super::restore::restore;
use super::{
    ClientApp, ClientConfigPlan, ClientConfigRequest, Edit,
    clock::Clock,
    display, display_in_home,
    mask::{OPEN_GATEWAY_TOKEN, gateway_tokens},
    plan_edits,
};
use spanbay_base::{crypto::sha256_hex, error::AppError, platform};

/// A file [`apply`] wrote; the caller stores one ledger row per entry.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AppliedFile {
    /// Absolute display path of the written file.
    pub path: String,
    /// Display path of the backup (in the data folder) of the previous content, when the file
    /// existed.
    pub backup_path: Option<String>,
    /// SHA-256 (lower-case hex) of the content written; [`restore`] compares it with the file.
    pub sha256_after: String,
    /// `false` when the file was created by the write (restoring deletes it).
    pub existed_before: bool,
}

/// Outcome of [`apply`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyResult {
    /// The tool.
    pub app: ClientApp,
    /// The gateway client the files were written for.
    pub client_name: String,
    /// Only the files that actually changed and were written.
    pub files: Vec<AppliedFile>,
    /// What to tell the user next.
    pub message: String,
}

/// The rename step, replaceable so tests can make one fail.
type Rename<'a> = &'a dyn Fn(&Path, &Path) -> io::Result<()>;

/// [`fs::rename`] as a plain function, to pass where a [`Rename`] is expected.
pub(super) fn fs_rename(from: &Path, to: &Path) -> io::Result<()> {
    fs::rename(from, to)
}

/// When a staged file loses its group and other permission bits.
#[derive(Debug, Clone, Copy)]
pub(super) enum ModePolicy<'a> {
    /// When the new content holds this gateway token (the open token is no secret).
    TightenIfHolds(&'a str),
    /// Always.
    AlwaysTighten,
}

impl ModePolicy<'_> {
    fn tightens(self, after: &str) -> bool {
        match self {
            Self::TightenIfHolds(token) => token != OPEN_GATEWAY_TOKEN && after.contains(token),
            Self::AlwaysTighten => true,
        }
    }
}

/// A changed file with everything the write needs: its real location, the staged temp file, the
/// content to roll back to, the hash it must still have, and the backup and original made.
struct Staged {
    /// Display path of the file as the user knows it.
    shown: String,
    /// The file to replace (symlinks resolved, so a dotfile link keeps pointing at its target).
    target: PathBuf,
    tmp: PathBuf,
    before: Option<String>,
    after: String,
    /// SHA-256 of `before`; `None` for a file that must not exist yet.
    base_sha: Option<String>,
    /// The permission bits the file had before this apply (Unix), restored by a roll-back.
    mode_before: Option<u32>,
    backup: Option<PathBuf>,
    /// The original this apply recorded (the first write to the file); removed again when the
    /// file ends up unchanged.
    kept_original: Option<PathBuf>,
}

/// Writes the config for `request` atomically: every changed file is staged as
/// `.<name>.spanbay-tmp` (the file's own mode, `0600` for a new file) beside its resolved
/// target, then all are renamed over their targets. An existing file is first copied to
/// `<backup_root>/backups/<tool>/<name>.spanbay-YYYYMMDD-HHMMSS.bak` (mode 0600); the caller
/// bounds the backups with [`super::prune_kept`] once the ledger knows which ones it still needs.
/// Nothing is written beside the tool's config. No target is replaced when any file fails to
/// merge, is refused or fails to stage (new directories may stay behind).
///
/// When no file of the tool holds a SpanBay connection yet (first connect, or the config is the
/// user's own again after a restore or edits by hand), the current content of every file becomes
/// the kept original that "Restore original config" brings back.
///
/// With `reviewed` (the plan the user saw) a file whose content differs from the plan's
/// `base_sha256` is refused, and every file's hash is checked again right before its rename.
///
/// # Errors
/// Same as [`super::preview`], plus [`AppError::validation`] for a refused file,
/// [`AppError::conflict`] for a file that changed after the preview or during the write, and
/// [`AppError::io`] when a write fails: permission denied reads "SpanBay cannot write `<path>`.
/// Check the file's permissions."; a failed rename lists the files that were rolled back and
/// those that were not changed.
pub fn apply(
    home: &Path,
    backup_root: &Path,
    request: &ClientConfigRequest,
    reviewed: Option<&ClientConfigPlan>,
    clock: Clock,
) -> Result<ApplyResult, AppError> {
    apply_with(home, backup_root, request, reviewed, clock, &fs_rename)
}

/// [`apply`] with an injectable rename (fault injection in tests).
fn apply_with(
    home: &Path,
    backup_root: &Path,
    request: &ClientConfigRequest,
    reviewed: Option<&ClientConfigPlan>,
    clock: Clock,
    rename: Rename<'_>,
) -> Result<ApplyResult, AppError> {
    let all = plan_edits(home, request)?;
    let fresh = !taken_over(home, request.app, &all);
    let (edits, unchanged): (Vec<Edit>, Vec<Edit>) = all.into_iter().partition(Edit::changed);
    if let Some(plan) = reviewed {
        check_reviewed(home, &edits, plan)?;
    }
    let backups = backup_dir(backup_root, request.app);
    let refreshed = if fresh && !edits.is_empty() {
        refresh_originals(&backups, edits.iter().chain(&unchanged))?
    } else {
        Vec::new()
    };
    let files = match write_edits(
        home,
        edits,
        ModePolicy::TightenIfHolds(request.token.trim()),
        &backups,
        clock,
        rename,
    ) {
        Ok(files) => files,
        Err(e) => {
            // Nothing was taken over: the originals are what they were before this apply.
            for (path, kept) in &refreshed {
                let _ = put_original(&backups, path, kept.as_ref().map(Option::as_deref));
            }
            return Err(e);
        }
    };
    if !refreshed.is_empty() {
        // The originals hold the user's own config again, so later writes may keep originals
        // again. Best effort: a marker left behind only means no original is kept.
        let _ = clear_purged(&backups);
    }
    let name = request.app.display_name();
    let message = if files.is_empty() {
        format!("{name} already uses this gateway; nothing was changed.")
    } else {
        format!("Restart {name} to use the new address.")
    };
    Ok(ApplyResult {
        app: request.app,
        client_name: request.client_name.clone(),
        files,
        message,
    })
}

/// Whether the tool's config already holds a SpanBay connection: a file holds a gateway-shaped
/// token, or the config points at a loopback address with a token (an open gateway's token is no
/// gateway-shaped one).
fn taken_over(home: &Path, app: ClientApp, edits: &[Edit]) -> bool {
    let loopback = |url: &str| {
        url::Url::parse(url).is_ok_and(|url| match url.host() {
            Some(url::Host::Ipv4(ip)) => ip.is_loopback(),
            Some(url::Host::Ipv6(ip)) => ip.is_loopback(),
            Some(url::Host::Domain(name)) => name.eq_ignore_ascii_case("localhost"),
            None => false,
        })
    };
    edits.iter().any(|edit| {
        edit.before
            .as_deref()
            .is_some_and(|text| !gateway_tokens(text).is_empty())
    }) || (app.writer().inspect)(home).is_ok_and(|found| {
        found.token_sha256.is_some() && found.base_url.as_deref().is_some_and(loopback)
    })
}

/// Makes the current content of every file of `edits` its kept original; returns what was kept
/// before, to put back when the write fails. A failure puts back the ones already refreshed.
fn refresh_originals<'a>(
    backups: &Path,
    edits: impl Iterator<Item = &'a Edit>,
) -> Result<Vec<(PathBuf, KeptOriginal)>, AppError> {
    let mut refreshed: Vec<(PathBuf, KeptOriginal)> = Vec::new();
    for edit in edits {
        let step = read_original(backups, &edit.path).and_then(|kept| {
            refresh_original(backups, &edit.path, edit.before.as_deref()).map(|()| kept)
        });
        match step {
            Ok(kept) => refreshed.push((edit.path.clone(), kept)),
            Err(e) => {
                for (path, kept) in &refreshed {
                    let _ = put_original(backups, path, kept.as_ref().map(Option::as_deref));
                }
                return Err(write_error(&display(&edit.path), &e));
            }
        }
    }
    Ok(refreshed)
}

/// The message for a file that changed after the user reviewed it.
fn changed_while_reviewing(home: &Path, path: &Path) -> String {
    format!(
        "`{}` changed while you were reviewing. Review again.",
        display_in_home(home, path)
    )
}

/// Refuses a write whose files differ from the ones the user reviewed.
fn check_reviewed(home: &Path, edits: &[Edit], plan: &ClientConfigPlan) -> Result<(), AppError> {
    for edit in edits {
        let shown = display(&edit.path);
        let base = edit.base_sha256();
        if !plan
            .files
            .iter()
            .any(|file| file.path == shown && file.base_sha256 == base)
        {
            return Err(AppError::conflict(changed_while_reviewing(
                home, &edit.path,
            )));
        }
    }
    Ok(())
}

/// Writes the changed `edits` atomically (the steps of [`apply`]) with their backups in
/// `backups`; each file must still hold `edit.before` right before its rename. `policy` says
/// which files must not stay readable by group or others (a file that holds a gateway token).
pub(super) fn write_edits(
    home: &Path,
    edits: Vec<Edit>,
    policy: ModePolicy<'_>,
    backups: &Path,
    clock: Clock,
    rename: Rename<'_>,
) -> Result<Vec<AppliedFile>, AppError> {
    let mut checked = Vec::with_capacity(edits.len());
    for edit in edits {
        let shown = display(&edit.path);
        let target = check_target(&shown, &edit.path)?;
        checked.push((edit, target, shown));
    }
    let mut staged: Vec<Staged> = Vec::with_capacity(checked.len());
    if let Err(e) = stage(checked, policy, backups, clock, &mut staged) {
        discard(&staged);
        return Err(e);
    }
    for (index, file) in staged.iter().enumerate() {
        match current_sha(&file.target) {
            Ok(sha) if sha == file.base_sha => {}
            Ok(_) => {
                let headline = changed_while_reviewing(home, Path::new(&file.shown));
                return Err(AppError::conflict(roll_back(&staged, index, &headline)));
            }
            Err(e) => {
                let headline = write_error(&file.shown, &e).message().to_owned();
                return Err(AppError::io(roll_back(&staged, index, &headline)));
            }
        }
        if let Err(e) = rename(&file.tmp, &file.target) {
            let headline = write_error(&file.shown, &e).message().to_owned();
            return Err(AppError::io(roll_back(&staged, index, &headline)));
        }
    }
    let mut files = Vec::with_capacity(staged.len());
    for file in &staged {
        files.push(AppliedFile {
            path: file.shown.clone(),
            backup_path: file.backup.as_deref().map(display),
            sha256_after: sha256_hex(file.after.as_bytes()),
            existed_before: file.before.is_some(),
        });
    }
    Ok(files)
}

/// Stages every `(edit, target, shown)` into `staged`. A file's parent directory is created
/// before the file is pushed (and is not removed on failure); the file is pushed even when
/// [`prepare`] fails half-way, so the caller can clean up every backup and temp file made.
fn stage(
    checked: Vec<(Edit, PathBuf, String)>,
    policy: ModePolicy<'_>,
    backups: &Path,
    clock: Clock,
    staged: &mut Vec<Staged>,
) -> Result<(), AppError> {
    for (edit, target, shown) in checked {
        if let Some(dir) = target.parent().filter(|dir| !dir.exists()) {
            platform::create_private_dir_all(dir).map_err(|e| write_error(&shown, &e))?;
        }
        let tmp = tmp_path(&target).map_err(|e| write_error(&shown, &e))?;
        let tighten = policy.tightens(&edit.after);
        let mode_before = existing_mode(&target);
        let mode = mode_before.map(|mode| {
            if tighten {
                mode & !GROUP_OTHER_BITS
            } else {
                mode
            }
        });
        let mut file = Staged {
            base_sha: edit.base_sha256(),
            mode_before,
            shown,
            target,
            tmp,
            before: edit.before,
            after: edit.after,
            backup: None,
            kept_original: None,
        };
        let prepared =
            prepare(&mut file, backups, clock, mode).map_err(|e| write_error(&file.shown, &e));
        staged.push(file);
        prepared?;
    }
    Ok(())
}

/// Keeps the original, backs up the current content and writes the temp file of `file`, in
/// that order.
fn prepare(file: &mut Staged, backups: &Path, clock: Clock, mode: Option<u32>) -> io::Result<()> {
    file.kept_original = keep_original(backups, &file.target, file.before.as_deref())?;
    if let Some(before) = &file.before {
        file.backup = Some(create_backup(backups, &file.target, before, clock)?);
    }
    write_tmp(&file.tmp, file.after.as_bytes(), mode)
}

/// Removes the temp files, then the backups, then the originals this apply recorded, of
/// `staged`: files that were never changed.
fn discard(staged: &[Staged]) {
    for file in staged {
        let _ = fs::remove_file(&file.tmp);
    }
    for backup in staged.iter().filter_map(|file| file.backup.as_ref()) {
        let _ = fs::remove_file(backup);
    }
    for original in staged.iter().filter_map(|file| file.kept_original.as_ref()) {
        let _ = fs::remove_file(original);
    }
}

/// Puts the file back to the content and mode it had before this apply.
fn revert(file: &Staged) -> io::Result<()> {
    match &file.before {
        Some(before) => replace_file(&file.target, before.as_bytes(), file.mode_before),
        None => fs::remove_file(&file.target),
    }
}

/// Handles the stop at `staged[failed]`: reverts the files already renamed (content and mode),
/// removes the temp files, backups and recorded originals of every file that ends up unchanged,
/// and describes the outcome after `headline`.
fn roll_back(staged: &[Staged], failed: usize, headline: &str) -> String {
    let mut restored = Vec::new();
    let mut stuck = Vec::new();
    for file in staged[..failed].iter().rev() {
        match revert(file) {
            Ok(()) => {
                restored.push(file.shown.clone());
                for made in [&file.backup, &file.kept_original].into_iter().flatten() {
                    let _ = fs::remove_file(made);
                }
            }
            Err(e) => stuck.push(match &file.backup {
                Some(backup) => format!(
                    "{} ({e}; the previous content is in {})",
                    file.shown,
                    display(backup)
                ),
                None => format!("{} ({e})", file.shown),
            }),
        }
    }
    discard(&staged[failed..]);
    let list = |items: &[String]| {
        if items.is_empty() {
            "none".to_owned()
        } else {
            items.join(", ")
        }
    };
    let untouched: Vec<String> = staged[failed..].iter().map(|f| f.shown.clone()).collect();
    let mut message = format!(
        "{headline} The write was rolled back. Changed, then restored: {}. Not changed: {}.",
        list(&restored),
        list(&untouched),
    );
    if !stuck.is_empty() {
        message.push_str(&format!(" Could not be restored: {}.", stuck.join(", ")));
    }
    message
}

#[cfg(test)]
mod tests;
