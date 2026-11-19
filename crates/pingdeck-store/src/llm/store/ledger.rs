//! The applied-config ledger (`llm_client_applied`): which files the "Connect a client"
//! workflow wrote for which gateway client.
//!
//! Single responsibility: one row per written file, the lists the rotate dialog and the restore
//! flow read, deletion, and the bounded history of older writes ("Restore previous config" goes
//! back at most [`HISTORY_WRITES_KEPT`] writes per tool). Writing the files themselves is
//! `pingdeck_client_config`; a row names a backup there, which is kept while the row exists.

use rusqlite::{Connection, Row, params};

use super::{atomic, db};
use crate::llm::client_config::{AppliedFile, ClientApp};
use pingdeck_base::crypto::random_hex;
use pingdeck_base::error::AppError;

pub use pingdeck_llm_domain::llm::records::LedgerEntry;

const LEDGER_COLUMNS: &str = "id, client_id, app, path, backup_path, sha256_after, applied_at_ms";

/// Writes per tool the ledger keeps for "Restore previous config"; every client's latest write of
/// a tool stays besides (its files hold the client's current token).
pub const HISTORY_WRITES_KEPT: usize = 5;

fn entry_from_row(row: &Row<'_>) -> rusqlite::Result<LedgerEntry> {
    let app: String = row.get(2)?;
    let app = ClientApp::parse(&app).map_err(|error| {
        rusqlite::Error::FromSqlConversionFailure(2, rusqlite::types::Type::Text, Box::new(error))
    })?;
    let backup: String = row.get(4)?;
    let backup_path = (!backup.is_empty()).then_some(backup);
    Ok(LedgerEntry {
        id: row.get(0)?,
        client_id: row.get(1)?,
        app,
        file: AppliedFile {
            path: row.get(3)?,
            existed_before: backup_path.is_some(),
            backup_path,
            sha256_after: row.get(5)?,
        },
        applied_at_ms: row.get(6)?,
    })
}

/// The rows of `sql`, whose one parameter `?1` is bound to `param`.
fn query_entries(
    connection: &Connection,
    sql: &str,
    param: &str,
) -> Result<Vec<LedgerEntry>, AppError> {
    let mut statement = connection.prepare_cached(sql).map_err(db)?;
    let rows = statement
        .query_map(params![param], entry_from_row)
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// Records `files` as written for `client_id` and `app` at `applied_at_ms`, one row per file,
/// atomically: in its own immediate transaction, or as a savepoint inside the caller's
/// transaction. Returns the rows.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure, [`AppError::Internal`] without OS randomness.
pub fn record_applied(
    connection: &Connection,
    client_id: &str,
    app: ClientApp,
    files: &[AppliedFile],
    applied_at_ms: i64,
) -> Result<Vec<LedgerEntry>, AppError> {
    atomic(connection, |transaction| {
        insert_rows(transaction, client_id, app, files, applied_at_ms)
    })
}

/// The inserts of [`record_applied`] inside a transaction the caller owns.
pub(super) fn insert_rows(
    connection: &Connection,
    client_id: &str,
    app: ClientApp,
    files: &[AppliedFile],
    applied_at_ms: i64,
) -> Result<Vec<LedgerEntry>, AppError> {
    let mut insert = connection
        .prepare_cached(
            "INSERT INTO llm_client_applied \
                 (id, client_id, app, path, backup_path, sha256_after, applied_at_ms) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7)",
        )
        .map_err(db)?;
    let mut entries = Vec::with_capacity(files.len());
    for file in files {
        let id = format!("ap_{}", random_hex::<6>()?);
        insert
            .execute(params![
                id,
                client_id,
                app.as_str(),
                file.path,
                file.backup_path.as_deref().unwrap_or(""),
                file.sha256_after,
                applied_at_ms,
            ])
            .map_err(db)?;
        entries.push(LedgerEntry {
            id,
            client_id: client_id.to_owned(),
            app,
            file: AppliedFile {
                existed_before: file.backup_path.is_some(),
                ..file.clone()
            },
            applied_at_ms,
        });
    }
    Ok(entries)
}

/// Every row of `client_id`, newest write first.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
#[cfg(test)]
fn list_applied(connection: &Connection, client_id: &str) -> Result<Vec<LedgerEntry>, AppError> {
    query_entries(
        connection,
        &format!(
            "SELECT {LEDGER_COLUMNS} FROM llm_client_applied WHERE client_id = ?1 \
             ORDER BY applied_at_ms DESC, path"
        ),
        client_id,
    )
}

/// The files of the latest write per tool for `client_id`: for each `app` the rows with that
/// app's newest `applied_at_ms`. These are the files that hold the client's current token.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn latest_applied(
    connection: &Connection,
    client_id: &str,
) -> Result<Vec<LedgerEntry>, AppError> {
    query_entries(
        connection,
        &format!(
            "SELECT {LEDGER_COLUMNS} FROM llm_client_applied AS entry WHERE client_id = ?1 \
               AND applied_at_ms = (SELECT MAX(applied_at_ms) FROM llm_client_applied \
                                    WHERE client_id = entry.client_id AND app = entry.app) \
             ORDER BY app, path"
        ),
        client_id,
    )
}

/// The files of the newest write for `app` by any client (what "Restore previous config" of a
/// tool undoes): the rows with the app's newest `applied_at_ms`.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn latest_applied_for_app(
    connection: &Connection,
    app: ClientApp,
) -> Result<Vec<LedgerEntry>, AppError> {
    query_entries(
        connection,
        &format!(
            "SELECT {LEDGER_COLUMNS} FROM llm_client_applied WHERE app = ?1 \
               AND applied_at_ms = (SELECT MAX(applied_at_ms) FROM llm_client_applied \
                                    WHERE app = ?1) \
             ORDER BY path"
        ),
        app.as_str(),
    )
}

/// Deletes one row (after its file was restored); `true` when it existed.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn delete_applied(connection: &Connection, id: &str) -> Result<bool, AppError> {
    let deleted = connection
        .execute("DELETE FROM llm_client_applied WHERE id = ?1", params![id])
        .map_err(db)?;
    Ok(deleted > 0)
}

/// Deletes the rows `ids` (files that no longer hold what the row says); returns how many.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn delete_applied_ids(connection: &Connection, ids: &[String]) -> Result<u64, AppError> {
    atomic(connection, |transaction| {
        let mut delete = transaction
            .prepare_cached("DELETE FROM llm_client_applied WHERE id = ?1")
            .map_err(db)?;
        let mut deleted = 0;
        for id in ids {
            deleted += delete.execute(params![id]).map_err(db)? as u64;
        }
        Ok(deleted)
    })
}

/// Deletes every row of `app` (its files are back to the user's original); returns how many.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn delete_applied_for_app(connection: &Connection, app: ClientApp) -> Result<u64, AppError> {
    let deleted = connection
        .execute(
            "DELETE FROM llm_client_applied WHERE app = ?1",
            params![app.as_str()],
        )
        .map_err(db)?;
    Ok(deleted as u64)
}

/// Deletes every row of `client_id` (the client is deleted); returns how many.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn delete_applied_for_client(
    connection: &Connection,
    client_id: &str,
) -> Result<u64, AppError> {
    let deleted = connection
        .execute(
            "DELETE FROM llm_client_applied WHERE client_id = ?1",
            params![client_id],
        )
        .map_err(db)?;
    Ok(deleted as u64)
}

/// Deletes the rows of `app` older than its newest `keep_writes` writes, except each client's
/// latest write of the app; returns how many.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn prune_history(
    connection: &Connection,
    app: ClientApp,
    keep_writes: usize,
) -> Result<u64, AppError> {
    let keep = i64::try_from(keep_writes).unwrap_or(i64::MAX);
    let mut statement = connection
        .prepare_cached(
            "DELETE FROM llm_client_applied AS entry WHERE app = ?1 \
               AND applied_at_ms < (SELECT coalesce(min(at), 0) FROM \
                     (SELECT DISTINCT applied_at_ms AS at FROM llm_client_applied \
                       WHERE app = ?1 ORDER BY at DESC LIMIT ?2)) \
               AND applied_at_ms < (SELECT max(applied_at_ms) FROM llm_client_applied \
                     WHERE app = ?1 AND client_id = entry.client_id)",
        )
        .map_err(db)?;
    let deleted = statement.execute(params![app.as_str(), keep]).map_err(db)?;
    Ok(deleted as u64)
}

/// The backups the rows of `app` name (display paths); they must not be pruned.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn referenced_backups(
    connection: &Connection,
    app: ClientApp,
) -> Result<Vec<String>, AppError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT DISTINCT backup_path FROM llm_client_applied \
             WHERE app = ?1 AND backup_path <> ''",
        )
        .map_err(db)?;
    let rows = statement
        .query_map(params![app.as_str()], |row| row.get::<_, String>(0))
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

#[cfg(test)]
mod tests;
