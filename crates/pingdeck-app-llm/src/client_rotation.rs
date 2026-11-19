//! Rotating a connected client's token without ever leaving a dead token behind.
//!
//! Single responsibility: the all-or-nothing rotation. The new token is minted in memory
//! (`store::mint_token`), every file that holds the old one is written with it (atomic per tool,
//! backups in the data folder), and only when every file is written the store swaps the token
//! hash and records the ledger rows in one transaction (`store::commit_rotation`). Any failure
//! before that commit restores the files already written from their backups and leaves the old
//! token valid in the store, so the old token keeps working and the new one never did. A failure
//! of the commit itself restores the files the same way.
//!
//! The ledger is reconciled with the files first: a row whose file no longer holds the old token
//! (gone, overwritten by another client or by hand, or outside the tool's config folder now) is
//! skipped, listed in the result and deleted after the commit, so it cannot block every later
//! rotation. A file that may still hold the token but cannot be read, parsed or written still
//! fails the rotation.

use std::collections::BTreeMap;
use std::path::Path;

use super::client_writers::{RotateUpdateResult, RotatedFiles, SkippedFile, backup_root};
use super::overview::token_hint;
use super::{current_time_ms, with_connection};
use crate::app::AppState;
use crate::client_config::{
    self, AppliedFile, ApplyResult, ClientApp, ClientConfigPlan, ClientConfigRequest, Clock,
    RestoreEnv,
};
use crate::error::AppError;
use crate::llm::store::{self, GatewayClient, MintedToken};
use crate::logging::{LogLevel, log_line};

/// The files one rotation wrote, per tool.
pub(super) type Written = Vec<(ClientApp, Vec<AppliedFile>)>;

/// What [`rewrite_others`] did: the files it wrote, and the stale ledger rows it skipped.
#[derive(Default)]
pub(super) struct Rewritten {
    pub(super) written: Written,
    /// The skipped files, as the result lists them.
    pub(super) skipped: Vec<SkippedFile>,
    /// The ids of their ledger rows, deleted once the rotation is committed.
    pub(super) stale_ids: Vec<String>,
}

/// How a tool's config is written; the system's [`client_config::apply`], or a test's double that
/// fails part-way.
pub(super) type Applier<'a> = &'a dyn Fn(
    &Path,
    &Path,
    &ClientConfigRequest,
    Option<&ClientConfigPlan>,
    Clock,
) -> Result<ApplyResult, AppError>;

/// [`client_config::apply`] as an [`Applier`].
pub(super) fn system_apply(
    home: &Path,
    backup_root: &Path,
    request: &ClientConfigRequest,
    reviewed: Option<&ClientConfigPlan>,
    clock: Clock,
) -> Result<ApplyResult, AppError> {
    client_config::apply(home, backup_root, request, reviewed, clock)
}

/// Puts every file of `written` back to its pre-write content (the backup, or deleted when the
/// write created it), whatever it holds now. Each file is restored on its own, not as one group:
/// a file that cannot be put back must not drag the others back to the uncommitted new token.
/// Returns the paths that could not be restored.
pub(super) fn restore_all(state: &AppState, home: &Path, written: &Written) -> Vec<String> {
    let root = backup_root(state, home);
    let any = |_: &str| true;
    let mut left = Vec::new();
    for (app, files) in written {
        let env = RestoreEnv {
            app: *app,
            backup_root: &root,
            clock: Clock::system(),
            token_is_active: &any,
        };
        for file in files {
            if let Err(error) = client_config::restore(std::slice::from_ref(file), &env, true) {
                log_line(
                    LogLevel::Warn,
                    "llm.clients",
                    &format!(
                        "could not restore {} ({}): {}",
                        file.path,
                        app.display_name(),
                        error.message()
                    ),
                );
                left.push(file.path.clone());
            }
        }
    }
    left
}

/// Keeps the ledger history and the backups of `apps` bounded: older writes beyond the history go
/// (each client's latest stays), then the backups no remaining row names are pruned. Best effort.
pub(super) fn tidy_kept(state: &AppState, home: &Path, apps: &[ClientApp]) {
    let root = backup_root(state, home);
    for app in apps {
        let referenced = with_connection(state, |connection| {
            store::prune_history(connection, *app, store::HISTORY_WRITES_KEPT)?;
            store::referenced_backups(connection, *app)
        });
        match referenced {
            Ok(referenced) => {
                let keep = |path: &str| referenced.iter().any(|known| known == path);
                client_config::prune_kept(&root, *app, &keep, Clock::system());
            }
            Err(error) => log_line(
                LogLevel::Warn,
                "llm.clients",
                &format!(
                    "could not tidy the {} backups: {}",
                    app.display_name(),
                    error.message()
                ),
            ),
        }
    }
}

/// Deletes the stale ledger rows a committed rotation skipped. Best effort: a row left behind is
/// found stale again next time.
pub(super) fn drop_stale(state: &AppState, stale_ids: &[String]) {
    if stale_ids.is_empty() {
        return;
    }
    if let Err(error) = with_connection(state, |connection| {
        store::delete_applied_ids(connection, stale_ids)
    }) {
        log_line(
            LogLevel::Warn,
            "llm.clients",
            &format!("could not delete stale ledger rows: {}", error.message()),
        );
    }
}

/// `error`, plus the files a rollback could not restore.
pub(super) fn with_leftovers(error: AppError, left: &[String]) -> AppError {
    if left.is_empty() {
        error
    } else {
        AppError::io(format!(
            "{} These files could not be restored: {}",
            error.message(),
            left.join(", ")
        ))
    }
}

/// Rewrites the token whose hash is `old_sha256` to `new_token` in the files the ledger names for
/// `client_id`, for every tool except `except`. Files that no longer hold the old token are
/// skipped and their rows reported stale (see the module docs). All or nothing for the rest: when
/// any file cannot be updated (it cannot be parsed or written) the files already written are
/// restored and the error names what failed. Nothing is stored; the caller commits.
pub(super) fn rewrite_others(
    state: &AppState,
    home: &Path,
    client_id: &str,
    old_sha256: &str,
    new_token: &str,
    except: Option<ClientApp>,
) -> Result<Rewritten, AppError> {
    let entries = with_connection(state, |connection| {
        store::latest_applied(connection, client_id)
    })?;
    let mut by_app: BTreeMap<&'static str, (ClientApp, Vec<AppliedFile>)> = BTreeMap::new();
    let mut ids: Vec<(ClientApp, String, String)> = Vec::new();
    for entry in entries
        .into_iter()
        .filter(|entry| Some(entry.app) != except)
    {
        ids.push((entry.app, entry.file.path.clone(), entry.id));
        by_app
            .entry(entry.app.as_str())
            .or_insert_with(|| (entry.app, Vec::new()))
            .1
            .push(entry.file);
    }
    let root = backup_root(state, home);
    let mut done = Rewritten::default();
    let mut failed: Vec<(String, String)> = Vec::new();
    for (app, files) in by_app.into_values() {
        match client_config::rewrite_token(
            home,
            &root,
            app,
            &files,
            old_sha256,
            new_token,
            Clock::system(),
        ) {
            Ok(report) => {
                if !report.updated.is_empty() {
                    done.written.push((app, report.updated));
                }
                failed.extend(report.failed);
                for (path, reason) in report.stale {
                    done.stale_ids.extend(
                        ids.iter()
                            .filter(|(of, known, _)| *of == app && *known == path)
                            .map(|(_, _, id)| id.clone()),
                    );
                    done.skipped.push(SkippedFile { path, reason });
                }
            }
            Err(error) => failed.push((app.display_name().to_owned(), error.message().to_owned())),
        }
        if !failed.is_empty() {
            break;
        }
    }
    if failed.is_empty() {
        return Ok(done);
    }
    let left = restore_all(state, home, &done.written);
    let what: Vec<String> = failed
        .iter()
        .map(|(path, reason)| format!("{path}: {reason}"))
        .collect();
    Err(with_leftovers(
        AppError::conflict(format!(
            "Nothing was changed: {}. Use Rotate and copy token instead.",
            what.join("; ")
        )),
        &left,
    ))
}

/// Commits a rotation whose files are written: swaps the token hash and records the ledger rows
/// in one transaction. When that fails the files are restored. Refreshes the gateway's token
/// cache on success.
pub(super) fn commit(
    state: &AppState,
    home: &Path,
    client_id: &str,
    old_sha256: &str,
    minted: &MintedToken,
    written: &Written,
    grant: &store::ModelGrant,
) -> Result<GatewayClient, AppError> {
    let committed = with_connection(state, |connection| {
        store::commit_rotation(
            connection,
            client_id,
            old_sha256,
            minted,
            written,
            grant,
            current_time_ms(),
        )
    });
    match committed {
        Ok(client) => {
            state.llm.touch();
            Ok(client)
        }
        Err(error) => {
            let left = restore_all(state, home, written);
            Err(with_leftovers(error, &left))
        }
    }
}

/// The files of a rotation as the DTO lists them.
pub(super) fn rotated_files(done: &Rewritten) -> RotatedFiles {
    RotatedFiles {
        updated: done
            .written
            .iter()
            .flat_map(|(_, files)| files.iter().cloned())
            .collect(),
        skipped: done.skipped.clone(),
    }
}

/// The tools of `written`.
pub(super) fn apps_of(written: &Written) -> Vec<ClientApp> {
    written.iter().map(|(app, _)| *app).collect()
}

/// "Rotate and update N files": mints a new token, writes it into
/// every file the ledger names, and only then commits the rotation. See the module docs for what
/// a failure leaves. The token is not returned.
pub(super) fn rotate_and_update(
    state: &AppState,
    client_id: &str,
    home: &Path,
) -> Result<RotateUpdateResult, AppError> {
    let client = with_connection(state, |connection| store::get_client(connection, client_id))?;
    if !client.is_active() {
        return Err(AppError::validation("A revoked client cannot be rotated"));
    }
    let has_files = with_connection(state, |connection| {
        Ok(!store::latest_applied(connection, client_id)?.is_empty())
    })?;
    if !has_files {
        return Err(AppError::validation(
            "This client has no config files written by PingDeck; use Rotate and copy token",
        ));
    }
    let old = with_connection(state, store::client_hashes_snapshot)?
        .into_iter()
        .find(|entry| entry.client.id == client_id)
        .map(|entry| entry.token_sha256)
        .ok_or_else(|| AppError::not_found(format!("gateway client {client_id} not found")))?;
    let minted = store::mint_token(&client.name)?;
    let done = rewrite_others(state, home, client_id, &old, &minted.token, None)?;
    let client = commit(
        state,
        home,
        client_id,
        &old,
        &minted,
        &done.written,
        &store::ModelGrant::default(),
    )?;
    drop_stale(state, &done.stale_ids);
    tidy_kept(state, home, &apps_of(&done.written));
    state
        .llm
        .services
        .client_watch
        .first_requests
        .arm(client_id);
    super::clients::clients_changed(state);
    Ok(RotateUpdateResult {
        token_hint: token_hint(&client),
        client,
        files: rotated_files(&done),
    })
}
