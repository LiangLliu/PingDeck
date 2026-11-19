//! Backups of replaced config files and the kept originals. They live in the data folder, never
//! beside the tool's config: `<backup_root>/backups/<tool>/<name>.pingdeck-YYYYMMDD-HHMMSS.bak`
//! (a write replaced the content), `<name>.pingdeck-safety-YYYYMMDD-HHMMSS.bak` (a restore
//! replaced it) and `<name>.<folder digest>.pingdeck-original[-absent]` (the user's own file from
//! before PingDeck took the tool over). They hold the user's own config, credentials included.

use std::{
    fs, io,
    path::{Path, PathBuf},
};

use serde::{Deserialize, Serialize};

use super::{
    ClientApp,
    clock::{Clock, STAMP_DASH_AT, STAMP_LEN},
    display,
    fsio::{file_name, replace_file, write_synced},
};
use pingdeck_base::platform;

/// Backups of one file [`prune_kept`] keeps besides the ones the ledger still names; safety
/// backups (made before a restore) are counted apart, so restores never push out write backups.
pub(super) const BACKUPS_KEPT: usize = 5;
/// Age after which [`prune_kept`] deletes a backup the ledger no longer names (30 days).
pub(super) const BACKUP_MAX_AGE_MS: i64 = 30 * 24 * 60 * 60 * 1000;
/// Backup names tried (one second apart) before giving up on a free one.
const MAX_BACKUP_BUMPS: i64 = 1_000;
/// Between the file name and the stamp of a backup: `<name>.pingdeck-<stamp>.bak`.
const BACKUP_INFIX: &str = ".pingdeck-";
/// Between the file name and the stamp of a safety backup: `<name>.pingdeck-safety-<stamp>.bak`.
const SAFETY_INFIX: &str = ".pingdeck-safety-";
/// Extension of a backup.
const BACKUP_EXT: &str = ".bak";
/// Suffix of a kept original: `<key>.pingdeck-original`.
const ORIGINAL_SUFFIX: &str = ".pingdeck-original";
/// Added to [`ORIGINAL_SUFFIX`] when the file did not exist: `<key>.pingdeck-original-absent`.
const ABSENT_SUFFIX: &str = "-absent";

/// The folder that holds the backups of `app`'s files.
pub(super) fn backup_dir(backup_root: &Path, app: ClientApp) -> PathBuf {
    backup_root.join("backups").join(app.as_str())
}

/// Whether `stamp` is `YYYYMMDD-HHMMSS`.
fn is_stamp(stamp: &str) -> bool {
    stamp.len() == STAMP_LEN
        && stamp.bytes().enumerate().all(|(i, b)| {
            if i == STAMP_DASH_AT {
                b == b'-'
            } else {
                b.is_ascii_digit()
            }
        })
}

/// Whether `candidate` is `<name>.pingdeck-YYYYMMDD-HHMMSS.bak` (a write backup).
#[cfg(test)]
pub(super) fn is_backup_of(candidate: &str, name: &str) -> bool {
    matches!(classify(candidate), Some((KeptKind::Backup, of, _)) if of == name)
}

/// Copies `content` (owner-only) to a new backup of `target` in `dir`, creating the folder
/// privately; when the name is taken (two writes in one second) the stamp moves forward until a
/// free one is found.
pub(super) fn create_backup(
    dir: &Path,
    target: &Path,
    content: &str,
    clock: Clock,
) -> io::Result<PathBuf> {
    create_stamped(dir, target, content, clock, BACKUP_INFIX)
}

/// As [`create_backup`], for the copy a restore makes of the content it replaces.
pub(super) fn create_safety_backup(
    dir: &Path,
    target: &Path,
    content: &str,
    clock: Clock,
) -> io::Result<PathBuf> {
    create_stamped(dir, target, content, clock, SAFETY_INFIX)
}

fn create_stamped(
    dir: &Path,
    target: &Path,
    content: &str,
    clock: Clock,
    infix: &str,
) -> io::Result<PathBuf> {
    platform::create_private_dir_all(dir)?;
    let name = file_name(target)?;
    for bump in 0..MAX_BACKUP_BUMPS {
        let candidate = dir.join(format!("{name}{infix}{}{BACKUP_EXT}", clock.stamp(bump)));
        match write_synced(&candidate, content.as_bytes(), None) {
            Err(e) if e.kind() == io::ErrorKind::AlreadyExists => {}
            other => return other.map(|()| candidate),
        }
    }
    Err(io::Error::new(
        io::ErrorKind::AlreadyExists,
        "no free backup name",
    ))
}

/// The key of `target`'s original: its file name plus a digest of its real folder, so the same
/// name in two config folders (`CLAUDE_CONFIG_DIR`, `CODEX_HOME`, `GEMINI_CLI_HOME`) keeps two
/// originals. Paths are canonicalized as the writer does, so writing and restoring agree.
fn original_key(target: &Path) -> io::Result<String> {
    let real = fs::canonicalize(target).ok().or_else(|| {
        let folder = fs::canonicalize(target.parent()?).ok()?;
        Some(folder.join(target.file_name()?))
    });
    let real = real.as_deref().unwrap_or(target);
    let name = file_name(real)?;
    let folder = real.parent().unwrap_or_else(|| Path::new(""));
    let digest = pingdeck_base::crypto::sha256_hex(folder.as_os_str().as_encoded_bytes());
    Ok(format!("{name}.{}", &digest[..12]))
}

/// The user's own file before PingDeck took the tool over: written on the first write and again
/// whenever PingDeck takes over a config that holds no PingDeck connection; never pruned.
fn original_path(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}{ORIGINAL_SUFFIX}"))
}

/// Marks that the file did not exist before PingDeck took the tool over.
fn absent_marker(dir: &Path, key: &str) -> PathBuf {
    dir.join(format!("{key}{ORIGINAL_SUFFIX}{ABSENT_SUFFIX}"))
}

/// Marks that [`purge_kept`] deleted the tool's originals while its config may still hold a
/// PingDeck connection: [`keep_original`] keeps nothing until PingDeck takes over the user's own
/// config again ([`clear_purged`]), so content PingDeck wrote never becomes the "original".
fn purged_marker(dir: &Path) -> PathBuf {
    dir.join(PURGED_MARKER)
}

/// Name of the marker [`purged_marker`] points at; [`classify`] does not list it as a copy.
const PURGED_MARKER: &str = ".pingdeck-purged";

/// Removes the marker [`purge_kept`] left: the tool's config was the user's own again and its
/// originals were just refreshed from it.
pub(super) fn clear_purged(dir: &Path) -> io::Result<()> {
    match fs::remove_file(purged_marker(dir)) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => Err(e),
        _ => Ok(()),
    }
}

/// Keeps `before` (the content PingDeck is about to replace; `None`: the file does not exist) as
/// the original of `target` unless an original is already kept or the originals were purged
/// (see [`purged_marker`]). Owner-only, like the backups. Returns the file it wrote, `None` when
/// it kept nothing; a write that fails must delete it again.
pub(super) fn keep_original(
    dir: &Path,
    target: &Path,
    before: Option<&str>,
) -> io::Result<Option<PathBuf>> {
    let key = original_key(target)?;
    let (original, absent) = (original_path(dir, &key), absent_marker(dir, &key));
    if original.exists() || absent.exists() || purged_marker(dir).exists() {
        return Ok(None);
    }
    platform::create_private_dir_all(dir)?;
    let (path, content) = match before {
        Some(text) => (original, text.as_bytes()),
        None => (absent, &[][..]),
    };
    match write_synced(&path, content, None) {
        Ok(()) => Ok(Some(path)),
        Err(e) if e.kind() == io::ErrorKind::AlreadyExists => Ok(None),
        Err(e) => Err(e),
    }
}

/// Makes `before` (`None`: the file does not exist) the original of `target`, replacing what was
/// kept: PingDeck takes over a config that is the user's own again (after a restore, or edited by
/// hand since), so "Restore original config" must bring back this content, not an older one.
pub(super) fn refresh_original(dir: &Path, target: &Path, before: Option<&str>) -> io::Result<()> {
    let key = original_key(target)?;
    let (original, absent) = (original_path(dir, &key), absent_marker(dir, &key));
    platform::create_private_dir_all(dir)?;
    let (keep, drop) = match before {
        Some(text) => {
            replace_file(&original, text.as_bytes(), None)?;
            (original, absent)
        }
        None => {
            replace_file(&absent, &[], None)?;
            (absent, original)
        }
    };
    match fs::remove_file(&drop) {
        Err(e) if e.kind() != io::ErrorKind::NotFound => {
            // Two kinds side by side would be ambiguous: undo the new one.
            let _ = fs::remove_file(&keep);
            Err(e)
        }
        _ => Ok(()),
    }
}

/// What is kept as the original of a file: `None` when nothing is kept, `Some(None)` for "did not
/// exist", `Some(Some(text))` for its content.
pub(super) type KeptOriginal = Option<Option<String>>;

/// What is kept as the original of `target`, to put back with [`put_original`] when a write that
/// refreshed it fails.
pub(super) fn read_original(dir: &Path, target: &Path) -> io::Result<KeptOriginal> {
    match original_of(dir, target) {
        None => Ok(None),
        Some(Original::Absent) => Ok(Some(None)),
        Some(Original::Content(path)) => fs::read_to_string(path).map(|text| Some(Some(text))),
    }
}

/// Puts back what [`read_original`] read: the same original, or none at all.
pub(super) fn put_original(
    dir: &Path,
    target: &Path,
    kept: Option<Option<&str>>,
) -> io::Result<()> {
    match kept {
        Some(before) => refresh_original(dir, target, before),
        None => {
            let key = original_key(target)?;
            for path in [original_path(dir, &key), absent_marker(dir, &key)] {
                match fs::remove_file(path) {
                    Err(e) if e.kind() != io::ErrorKind::NotFound => return Err(e),
                    _ => {}
                }
            }
            Ok(())
        }
    }
}

/// What PingDeck kept of a file before it took the tool over.
#[derive(Debug, PartialEq, Eq)]
pub(super) enum Original {
    /// The file existed; its content is kept at this path.
    Content(PathBuf),
    /// The file did not exist.
    Absent,
}

/// The kept original of `target`; `None` when PingDeck never wrote it.
pub(super) fn original_of(dir: &Path, target: &Path) -> Option<Original> {
    let key = original_key(target).ok()?;
    let original = original_path(dir, &key);
    if original.is_file() {
        Some(Original::Content(original))
    } else {
        absent_marker(dir, &key)
            .is_file()
            .then_some(Original::Absent)
    }
}

/// What a file in a tool's backup folder is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeptKind {
    /// The user's own file from before PingDeck took the tool over.
    Original,
    /// A marker that the file did not exist then (holds nothing).
    AbsentMarker,
    /// The content a write replaced.
    Backup,
    /// The content a restore replaced.
    SafetyBackup,
}

/// One copy PingDeck keeps in a tool's backup folder. Originals and backups hold the user's own
/// config, credentials included, in plaintext (owner-only).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeptCopy {
    /// Display path of the copy in the data folder.
    pub path: String,
    /// What it is.
    pub kind: KeptKind,
    /// Name of the config file it is a copy of (`settings.json`).
    pub file_name: String,
    /// When it was made (Unix ms), from its stamp; `None` for an original.
    pub made_at_ms: Option<i64>,
}

/// What the file called `candidate` in a backup folder is, with the name of the config file and
/// its stamp; `None` for anything else (a temp file, a foreign file).
fn classify(candidate: &str) -> Option<(KeptKind, &str, Option<&str>)> {
    // An original's key is `<name>.<12 hex digits>`, see `original_key`.
    let name_of_key = |key: &'_ str| -> Option<usize> {
        let at = key.rfind('.')?;
        let digest = &key[at + 1..];
        (digest.len() == 12 && digest.bytes().all(|b| b.is_ascii_hexdigit())).then_some(at)
    };
    if let Some(key) = candidate
        .strip_suffix(ABSENT_SUFFIX)
        .and_then(|rest| rest.strip_suffix(ORIGINAL_SUFFIX))
    {
        let at = name_of_key(key)?;
        return Some((KeptKind::AbsentMarker, &candidate[..at], None));
    }
    if let Some(key) = candidate.strip_suffix(ORIGINAL_SUFFIX) {
        let at = name_of_key(key)?;
        return Some((KeptKind::Original, &candidate[..at], None));
    }
    let rest = candidate.strip_suffix(BACKUP_EXT)?;
    let at = rest.len().checked_sub(STAMP_LEN)?;
    let (head, stamp) = (rest.get(..at)?, rest.get(at..)?);
    if !is_stamp(stamp) {
        return None;
    }
    if let Some(name) = head.strip_suffix(SAFETY_INFIX) {
        return Some((KeptKind::SafetyBackup, name, Some(stamp)));
    }
    head.strip_suffix(BACKUP_INFIX)
        .map(|name| (KeptKind::Backup, name, Some(stamp)))
}

/// Every copy PingDeck keeps for `app` (originals, backups, safety backups, absent markers),
/// newest first within a file; `clock` reads the stamps in the zone they were made in.
#[must_use]
pub fn kept_copies(backup_root: &Path, app: ClientApp, clock: Clock) -> Vec<KeptCopy> {
    let dir = backup_dir(backup_root, app);
    let Ok(entries) = fs::read_dir(&dir) else {
        return Vec::new();
    };
    let mut copies: Vec<KeptCopy> = entries
        .filter_map(|entry| {
            let entry = entry.ok()?;
            let name = entry.file_name().into_string().ok()?;
            let (kind, file_name, stamp) = classify(&name)?;
            Some(KeptCopy {
                path: display(&entry.path()),
                kind,
                file_name: file_name.to_owned(),
                made_at_ms: stamp.and_then(|stamp| clock.stamp_ms(stamp)),
            })
        })
        .collect();
    copies.sort_by(|a, b| (&a.file_name, b.made_at_ms).cmp(&(&b.file_name, a.made_at_ms)));
    copies
}

/// Bounds the backups of `app`: per file and kind (write backup, safety backup) the newest
/// [`BACKUPS_KEPT`] stay and none older than [`BACKUP_MAX_AGE_MS`]; a backup `referenced` names
/// (by display path: the ledger still needs it for "Restore previous config") always stays and is
/// not counted. Originals are never pruned. Best effort; returns how many were deleted.
pub fn prune_kept(
    backup_root: &Path,
    app: ClientApp,
    referenced: &dyn Fn(&str) -> bool,
    clock: Clock,
) -> usize {
    let mut deleted = 0;
    let mut seen: Vec<(KeptKind, String, usize)> = Vec::new();
    for copy in kept_copies(backup_root, app, clock) {
        if !matches!(copy.kind, KeptKind::Backup | KeptKind::SafetyBackup) || referenced(&copy.path)
        {
            continue;
        }
        let rank = match seen
            .iter_mut()
            .find(|(kind, name, _)| *kind == copy.kind && *name == copy.file_name)
        {
            Some(entry) => {
                entry.2 += 1;
                entry.2
            }
            None => {
                seen.push((copy.kind, copy.file_name.clone(), 0));
                0
            }
        };
        let too_old = copy
            .made_at_ms
            .is_some_and(|made| clock.now_ms.saturating_sub(made) > BACKUP_MAX_AGE_MS);
        if (rank >= BACKUPS_KEPT || too_old) && fs::remove_file(&copy.path).is_ok() {
            deleted += 1;
        }
    }
    deleted
}

/// Deletes every copy PingDeck keeps for `app` (originals included) except the backups
/// `referenced` names; "Restore original config" is not possible afterwards, and later writes keep
/// no original until PingDeck takes over the user's own config again (see [`purged_marker`]).
/// Returns how many were deleted.
///
/// # Errors
/// [`AppError::io`] naming the first copy that could not be deleted (the others are tried), or
/// when the purge marker cannot be written (nothing is deleted then).
pub fn purge_kept(
    backup_root: &Path,
    app: ClientApp,
    referenced: &dyn Fn(&str) -> bool,
    clock: Clock,
) -> Result<usize, pingdeck_base::error::AppError> {
    let dir = backup_dir(backup_root, app);
    // The marker goes first: without it the next write would keep PingDeck's own content as the
    // original once the real one is gone.
    platform::create_private_dir_all(&dir)
        .and_then(|()| replace_file(&purged_marker(&dir), &[], None))
        .map_err(|e| {
            pingdeck_base::error::AppError::io(format!(
                "could not write {}: {e}",
                display(&purged_marker(&dir))
            ))
        })?;
    let mut deleted = 0;
    let mut failed: Option<String> = None;
    for copy in kept_copies(backup_root, app, clock) {
        if referenced(&copy.path) {
            continue;
        }
        match fs::remove_file(&copy.path) {
            Ok(()) => deleted += 1,
            Err(e) if e.kind() == io::ErrorKind::NotFound => {}
            Err(e) => {
                failed.get_or_insert_with(|| format!("{}: {e}", copy.path));
            }
        }
    }
    match failed {
        Some(what) => Err(pingdeck_base::error::AppError::io(format!(
            "could not delete {what}"
        ))),
        None => Ok(deleted),
    }
}

#[cfg(test)]
mod tests;
