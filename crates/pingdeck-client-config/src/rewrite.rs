//! Re-tokening of files an earlier connect wrote ("Rotate and update N files"): only the
//! token value changes, so the new token never has to leave the core for file writers.

use std::path::{Path, PathBuf};

use serde::{Deserialize, Serialize};

use super::backup::backup_dir;
use super::write::{AppliedFile, ModePolicy, fs_rename, write_edits};
use super::{ClientApp, Edit, check_token, clock::Clock, display, non_blank, read_optional};
use pingdeck_base::{crypto::sha256_hex, error::AppError};

/// What a writer's `retoken` decided about one file.
pub(super) enum Swap {
    /// The new file content.
    Replaced(String),
    /// Nothing to do: the file already holds the new token or never holds one.
    Nothing,
    /// The token field holds something else (the user edited it); the file is left alone.
    NotHeld,
}

/// The decision about a token field.
pub(super) enum Verdict {
    /// It holds the old token.
    Replace,
    /// It already holds the new token.
    Keep,
    /// It is missing or holds another value.
    NotHeld,
}

/// Compares the value found in a file with the old token's hash and the new token.
pub(super) fn decide(current: Option<&str>, old_sha256: &str, new_token: &str) -> Verdict {
    match non_blank(current) {
        Some(value) => {
            let sha = sha256_hex(value.as_bytes());
            if sha == old_sha256 {
                Verdict::Replace
            } else if sha == sha256_hex(new_token.as_bytes()) {
                Verdict::Keep
            } else {
                Verdict::NotHeld
            }
        }
        None => Verdict::NotHeld,
    }
}

impl Verdict {
    /// Turns the verdict into a [`Swap`], building the new content only for [`Verdict::Replace`].
    pub(super) fn into_swap(
        self,
        build: impl FnOnce() -> Result<String, AppError>,
    ) -> Result<Swap, AppError> {
        match self {
            Self::Replace => build().map(Swap::Replaced),
            Self::Keep => Ok(Swap::Nothing),
            Self::NotHeld => Ok(Swap::NotHeld),
        }
    }
}

/// The outcome of [`rewrite_token`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RewriteReport {
    /// The files that now hold the new token, as [`super::apply`] would report them (the caller
    /// records new ledger rows).
    pub updated: Vec<AppliedFile>,
    /// `(path, reason)` of every ledger file that could not be updated although it may still
    /// hold the old token (it cannot be read or parsed, or the write failed).
    pub failed: Vec<(String, String)>,
    /// `(path, reason)` of every ledger file that no longer holds the old token: it is gone, holds
    /// another value (the user or another client changed it), or is no file PingDeck manages for
    /// the tool any more (its config folder moved). Its ledger row is stale.
    pub stale: Vec<(String, String)>,
}

/// Why a ledger file is not re-tokened.
enum Skip {
    /// It no longer holds the old token; see [`RewriteReport::stale`].
    Stale(String),
    /// It may still hold it; see [`RewriteReport::failed`].
    Failed(String),
}

/// Replaces the token inside the files the ledger names for `app`: a file is updated only when
/// its token field holds the token whose SHA-256 is `old_token_sha256`; nothing else in the file
/// changes. A file that already holds the new token (a retry) or never holds one (Gemini CLI's
/// `settings.json`) is skipped silently; one whose token is something else, that is missing or
/// that PingDeck no longer manages is reported in `stale`, one that cannot be read or parsed in
/// `failed`; both are left alone. All updated files are written
/// atomically together (see [`super::apply`]) with backups in `<backup_root>/backups/<tool>/`;
/// when that write fails every file that was to be updated is reported in `failed`.
///
/// # Errors
/// [`AppError::validation`] when `new_token` is empty or holds whitespace, control characters or
/// quotes.
pub fn rewrite_token(
    home: &Path,
    backup_root: &Path,
    app: ClientApp,
    ledger_files: &[AppliedFile],
    old_token_sha256: &str,
    new_token: &str,
    clock: Clock,
) -> Result<RewriteReport, AppError> {
    let new_token = check_token(new_token)?;
    let managed = managed_paths(home, app);
    let mut failed: Vec<(String, String)> = Vec::new();
    let mut stale: Vec<(String, String)> = Vec::new();
    let mut edits: Vec<Edit> = Vec::new();
    for file in ledger_files {
        let edit = if managed.contains(&file.path) {
            retoken_file(
                home,
                app,
                Path::new(&file.path),
                old_token_sha256,
                new_token,
            )
        } else {
            Err(Skip::Stale(unmanaged(app)))
        };
        match edit {
            Ok(Some(edit)) => edits.push(edit),
            Ok(None) => {}
            Err(Skip::Stale(reason)) => stale.push((file.path.clone(), reason)),
            Err(Skip::Failed(reason)) => failed.push((file.path.clone(), reason)),
        }
    }
    let policy = ModePolicy::TightenIfHolds(new_token);
    let mut report = finish(home, backup_root, app, edits, policy, clock, failed);
    report.stale = stale;
    Ok(report)
}

/// Reads one ledger file and swaps its token: `Ok(None)` when there is nothing to do, `Err` with
/// the reason when the file is left alone.
fn retoken_file(
    home: &Path,
    app: ClientApp,
    path: &Path,
    old_sha256: &str,
    new_token: &str,
) -> Result<Option<Edit>, Skip> {
    let text = match read_optional(path) {
        Ok(Some(text)) => text,
        Ok(None) => return Err(Skip::Stale("the file no longer exists".to_owned())),
        Err(e) => return Err(Skip::Failed(e.message().to_owned())),
    };
    let swap = (app.writer().retoken)(home, path, &text, old_sha256, new_token);
    match swap.map_err(|e| Skip::Failed(e.message().to_owned()))? {
        Swap::Replaced(after) => Ok(Some(Edit {
            path: path.to_owned(),
            before: Some(text),
            after,
        })),
        Swap::Nothing => Ok(None),
        Swap::NotHeld => Err(Skip::Stale(
            "the file no longer holds the old token, so it was left untouched".to_owned(),
        )),
    }
}

/// Replaces the model reference `old` with `new` in the files the ledger names for `app` (after a
/// provider rename): every JSON/TOML string that is exactly `old`, or `pingdeck/old` (OpenCode's
/// provider-qualified form), becomes the same string with `new`. A file without the reference is
/// skipped silently; one that cannot be read is reported in `failed`. Written atomically with
/// backups, as [`rewrite_token`]; every written file is left owner-only, as a token file is.
///
/// The two replacements run in that order, so `new` must not be `pingdeck/<old>` (the provider
/// name rules guarantee this for a rename).
///
/// # Errors
/// None today; the `Result` keeps the signature stable for callers.
pub fn rewrite_model(
    home: &Path,
    backup_root: &Path,
    app: ClientApp,
    ledger_files: &[AppliedFile],
    old: &str,
    new: &str,
    clock: Clock,
) -> Result<RewriteReport, AppError> {
    let quote = |text: String| serde_json::Value::String(text).to_string();
    let pairs = [
        (quote(old.to_owned()), quote(new.to_owned())),
        (
            quote(format!("pingdeck/{old}")),
            quote(format!("pingdeck/{new}")),
        ),
    ];
    let managed = managed_paths(home, app);
    let mut failed: Vec<(String, String)> = Vec::new();
    let mut edits: Vec<Edit> = Vec::new();
    for file in ledger_files {
        if !managed.contains(&file.path) {
            failed.push((file.path.clone(), unmanaged(app)));
            continue;
        }
        let path = PathBuf::from(&file.path);
        match read_optional(&path) {
            Ok(Some(text)) if pairs.iter().any(|(from, _)| text.contains(from.as_str())) => {
                let after = pairs
                    .iter()
                    .fold(text.clone(), |after, (from, to)| after.replace(from, to));
                edits.push(Edit {
                    path,
                    before: Some(text),
                    after,
                });
            }
            Ok(_) => {}
            Err(e) => failed.push((file.path.clone(), e.message().to_owned())),
        }
    }
    Ok(finish(
        home,
        backup_root,
        app,
        edits,
        ModePolicy::AlwaysTighten,
        clock,
        failed,
    ))
}

/// Display paths of every file PingDeck manages for `app`.
fn managed_paths(home: &Path, app: ClientApp) -> Vec<String> {
    app.files(home).iter().map(|path| display(path)).collect()
}

/// Why a ledger file outside `app`'s managed files is left alone.
fn unmanaged(app: ClientApp) -> String {
    format!("not a file PingDeck manages for {}", app.display_name())
}

/// Writes the changed `edits` together and reports them after the per-file `failed`; when the
/// write fails, every file that was to be updated is added to `failed` with the reason.
fn finish(
    home: &Path,
    backup_root: &Path,
    app: ClientApp,
    mut edits: Vec<Edit>,
    policy: ModePolicy<'_>,
    clock: Clock,
    mut failed: Vec<(String, String)>,
) -> RewriteReport {
    edits.retain(Edit::changed);
    let paths: Vec<String> = edits.iter().map(|edit| display(&edit.path)).collect();
    let backups = backup_dir(backup_root, app);
    let updated = match write_edits(home, edits, policy, &backups, clock, &fs_rename) {
        Ok(updated) => updated,
        Err(e) => {
            failed.extend(paths.into_iter().map(|path| (path, e.message().to_owned())));
            Vec::new()
        }
    };
    RewriteReport {
        updated,
        failed,
        stale: Vec::new(),
    }
}

#[cfg(test)]
mod tests;
