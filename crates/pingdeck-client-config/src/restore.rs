//! Undo of an apply: the files of one write back to their backups ([`restore`]), or every file of
//! a tool back to the user's own content from before PingDeck took it over
//! ([`restore_original`]). Both treat a tool's files as one group: either every file is put back
//! or none is (Gemini CLI's `settings.json` and `.env` only work together).

use std::{fs, io, path::Path};

use serde::{Deserialize, Serialize};

use super::backup::{Original, backup_dir, create_safety_backup, original_of};
use super::fsio::{check_target, existing_mode, replace_file, write_error};
use super::mask::{gateway_tokens, mask_secrets, mask_token};
use super::write::AppliedFile;
use super::{ClientApp, clock::Clock, display, read_optional};
use pingdeck_base::{crypto::sha256_hex, diff::unified_diff_text, error::AppError, platform};

/// A token a backup holds that no active client has.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RevokedToken {
    /// The masked form (`pd_cc_••••9d2f`) the dialog shows.
    pub hint: String,
    /// SHA-256 (lower-case hex) of the token, for the caller's revocation lookup.
    pub sha256: String,
}

/// Outcome of [`restore`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum RestoreOutcome {
    /// The file holds the previous content again.
    Restored {
        /// Backup of the content that was replaced, `None` when the file had vanished.
        safety_backup: Option<String>,
        /// The restored content holds a gateway token that no active client has.
        backup_has_revoked_token: bool,
        /// Those tokens, masked ("This restores token pd_cc_••••9d2f, revoked on …").
        revoked_tokens: Vec<RevokedToken>,
    },
    /// The file did not exist before the write and is gone again.
    Deleted {
        /// Backup of the content that was removed, `None` when the file had vanished.
        safety_backup: Option<String>,
    },
    /// Nothing was changed: a file of the group changed after PingDeck wrote it (or the restore
    /// goes back to the original); show `diff` and ask "Restore anyway?".
    NeedsConfirmation {
        /// Unified diff (tokens masked) from the current content to the content a restore would
        /// write.
        diff: String,
        /// The restore deletes the file (it did not exist before PingDeck wrote it).
        deletes_file: bool,
        /// The content a restore would write holds a gateway token that no active client has.
        backup_has_revoked_token: bool,
        /// Those tokens, masked.
        revoked_tokens: Vec<RevokedToken>,
    },
}

/// What [`restore`] needs besides the entry.
pub struct RestoreEnv<'a> {
    /// The tool the entry belongs to (picks the backup folder for the safety backup).
    pub app: ClientApp,
    /// The data folder that holds `backups/<tool>/`.
    pub backup_root: &'a Path,
    /// Time of the restore, for the safety backup's name.
    pub clock: Clock,
    /// Takes the SHA-256 (lower-case hex) of a token found in the backup; `true` when an active
    /// client has it.
    pub token_is_active: &'a dyn Fn(&str) -> bool,
}

/// The gateway tokens in `text` that no active client has, once each.
fn revoked_tokens(text: Option<&str>, token_is_active: &dyn Fn(&str) -> bool) -> Vec<RevokedToken> {
    let mut found: Vec<RevokedToken> = Vec::new();
    for token in text.map(gateway_tokens).unwrap_or_default() {
        let sha256 = sha256_hex(token.as_bytes());
        if !token_is_active(&sha256) && !found.iter().any(|t| t.sha256 == sha256) {
            found.push(RevokedToken {
                hint: mask_token(token),
                sha256,
            });
        }
    }
    found
}

/// One file of a group restore, read and checked before anything is written.
struct Planned<'a> {
    entry: &'a AppliedFile,
    target: std::path::PathBuf,
    current: Option<String>,
    previous: Option<String>,
    revoked_tokens: Vec<RevokedToken>,
}

/// Undoes one write of a tool: puts every file of `entries` back to its backup, or deletes a file
/// the write created. When any file no longer holds what PingDeck wrote (its SHA-256 differs from
/// `sha256_after`) and `force` is not set, nothing is changed and every file comes back as
/// [`RestoreOutcome::NeedsConfirmation`] with its diff. Otherwise the current content of each
/// file is copied to a safety backup, then the files are put back; a file that fails rolls back
/// the files already put back. Each outcome says when the content that comes back holds a token
/// no active client has (`env.token_is_active`). A file keeps its current permission bits.
///
/// # Errors
/// [`AppError::validation`] for a refused file (see [`super::apply`]); [`AppError::io`] when a
/// backup is missing (before anything is changed) or a file cannot be read or written.
pub fn restore(
    entries: &[AppliedFile],
    env: &RestoreEnv<'_>,
    force: bool,
) -> Result<Vec<(String, RestoreOutcome)>, AppError> {
    let mut planned = Vec::with_capacity(entries.len());
    for entry in entries {
        let target = check_target(&entry.path, Path::new(&entry.path))?;
        let current = read_optional(&target)?;
        let previous = read_previous(entry)?;
        let revoked_tokens = revoked_tokens(previous.as_deref(), env.token_is_active);
        planned.push(Planned {
            entry,
            target,
            current,
            previous,
            revoked_tokens,
        });
    }
    let confirm = !force
        && planned.iter().any(|file| {
            !holds_the_write(file.entry, file.current.as_deref())
                && !(file.current.is_none() && file.previous.is_none())
        });
    if confirm {
        return Ok(planned.into_iter().map(confirmation).collect());
    }
    put_back_all(planned, env)
}

/// Safety backups of every file, then every file put back; see [`restore`].
fn put_back_all(
    planned: Vec<Planned<'_>>,
    env: &RestoreEnv<'_>,
) -> Result<Vec<(String, RestoreOutcome)>, AppError> {
    let backups = backup_dir(env.backup_root, env.app);
    let mut safety: Vec<Option<String>> = Vec::with_capacity(planned.len());
    for file in &planned {
        match safety_backup(
            &backups,
            &file.target,
            file.current.as_deref(),
            env.clock,
            &file.entry.path,
        ) {
            Ok(made) => safety.push(made),
            Err(e) => {
                for made in safety.iter().flatten() {
                    let _ = fs::remove_file(made);
                }
                return Err(e);
            }
        }
    }
    for (index, file) in planned.iter().enumerate() {
        if let Err(e) = put_back(&file.target, file.previous.as_deref(), &file.entry.path) {
            let mut stuck = Vec::new();
            for done in planned[..index].iter().rev() {
                if put_back(&done.target, done.current.as_deref(), &done.entry.path).is_err() {
                    stuck.push(done.entry.path.clone());
                }
            }
            let mut message = format!("{} Nothing was restored.", e.message());
            if !stuck.is_empty() {
                message = format!(
                    "{} These files could not be put back as they were: {} (their content is in the safety backups).",
                    e.message(),
                    stuck.join(", ")
                );
            }
            return Err(AppError::io(message));
        }
    }
    Ok(planned
        .into_iter()
        .zip(safety)
        .map(|(file, safety_backup)| {
            let outcome = match file.previous {
                Some(_) => RestoreOutcome::Restored {
                    safety_backup,
                    backup_has_revoked_token: !file.revoked_tokens.is_empty(),
                    revoked_tokens: file.revoked_tokens,
                },
                None => RestoreOutcome::Deleted { safety_backup },
            };
            (file.entry.path.clone(), outcome)
        })
        .collect())
}

/// The content `entry` replaced, read from its backup; `None` when the write created the file.
fn read_previous(entry: &AppliedFile) -> Result<Option<String>, AppError> {
    if !entry.existed_before {
        return Ok(None);
    }
    let backup = entry
        .backup_path
        .as_deref()
        .ok_or_else(|| AppError::io(format!("the ledger holds no backup for {}", entry.path)))?;
    fs::read_to_string(backup)
        .map(Some)
        .map_err(|e| AppError::io(format!("failed to read the backup {backup}: {e}")))
}

/// Whether the file still holds exactly what PingDeck wrote.
fn holds_the_write(entry: &AppliedFile, current: Option<&str>) -> bool {
    current.is_some_and(|text| sha256_hex(text.as_bytes()) == entry.sha256_after)
}

/// What to ask about one file of a group that needs confirmation: the diff from its current
/// content to the content a restore would write.
fn confirmation(file: Planned<'_>) -> (String, RestoreOutcome) {
    let current = file.current.as_deref().unwrap_or("");
    let previous = file.previous.as_deref().unwrap_or("");
    let diff = unified_diff_text(&file.entry.path, current, previous);
    (
        file.entry.path.clone(),
        RestoreOutcome::NeedsConfirmation {
            diff: mask_secrets(&diff, ""),
            deletes_file: file.previous.is_none() && file.current.is_some(),
            backup_has_revoked_token: !file.revoked_tokens.is_empty(),
            revoked_tokens: file.revoked_tokens,
        },
    )
}

/// Copies the `current` content of `target` to a new safety backup in `backups`; `None` when the
/// file is gone.
fn safety_backup(
    backups: &Path,
    target: &Path,
    current: Option<&str>,
    clock: Clock,
    shown: &str,
) -> Result<Option<String>, AppError> {
    current
        .map(|text| {
            create_safety_backup(backups, target, text, clock)
                .map(|backup| display(&backup))
                .map_err(|e| write_error(shown, &e))
        })
        .transpose()
}

/// Writes `previous` over `target` (creating its folder privately), or deletes `target` when
/// there is no previous content; a file that is already gone counts as deleted.
fn put_back(target: &Path, previous: Option<&str>, shown: &str) -> Result<(), AppError> {
    match previous {
        Some(text) => {
            if let Some(dir) = target.parent().filter(|dir| !dir.exists()) {
                platform::create_private_dir_all(dir).map_err(|e| write_error(shown, &e))?;
            }
            replace_file(target, text.as_bytes(), existing_mode(target))
                .map_err(|e| write_error(shown, &e))
        }
        None => match fs::remove_file(target) {
            Err(e) if e.kind() != io::ErrorKind::NotFound => Err(write_error(shown, &e)),
            _ => Ok(()),
        },
    }
}

/// "Restore original config": puts every file of `app` back to what it was before PingDeck took
/// the tool over (or deletes a file PingDeck created), as one group. Files PingDeck never wrote,
/// and files that already hold their original, are left out. Without `force` nothing is changed
/// and every file comes back as [`RestoreOutcome::NeedsConfirmation`] with the diff (and whether
/// the file would be deleted), for the user to review; with `force` the current content goes to
/// a safety backup first. The originals are kept, so this can be done again.
///
/// # Errors
/// As [`restore`].
pub fn restore_original(
    home: &Path,
    env: &RestoreEnv<'_>,
    force: bool,
) -> Result<Vec<(String, RestoreOutcome)>, AppError> {
    let backups = backup_dir(env.backup_root, env.app);
    let mut entries = Vec::new();
    for target in env.app.files(home) {
        let Some(original) = original_of(&backups, &target) else {
            continue;
        };
        let current = read_optional(&check_target(&display(&target), &target)?)?;
        let (backup_path, existed_before) = match original {
            Original::Content(path) => {
                let kept = fs::read_to_string(&path).map_err(|e| {
                    AppError::io(format!(
                        "failed to read the original {}: {e}",
                        display(&path)
                    ))
                })?;
                if current.as_deref() == Some(kept.as_str()) {
                    continue;
                }
                (Some(display(&path)), true)
            }
            Original::Absent if current.is_none() => continue,
            Original::Absent => (None, false),
        };
        entries.push(AppliedFile {
            path: display(&target),
            backup_path,
            sha256_after: String::new(),
            existed_before,
        });
    }
    if entries.is_empty() {
        return Ok(Vec::new());
    }
    restore(&entries, env, force)
}

#[cfg(test)]
mod tests;
