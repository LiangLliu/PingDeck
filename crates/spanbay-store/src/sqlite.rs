//! Shared SQLite primitives for every repository module: composable `BEGIN IMMEDIATE`
//! transactions with `SAVEPOINT` nesting, integer saturating casts, `LIKE` escaping, and JSON /
//! enum helpers.
//!
//! [`atomic_with`] is the crate's transaction primitive: repository writes that must be
//! all-or-nothing go through it (directly or through [`atomic`] / [`atomic_app`]); schema
//! creation opens its own transaction. Known multi-statement writes that are not atomic:
//! `WorkspaceStore::prune_orphan_transport_secrets` (two independent best-effort deletes),
//! `WorkspaceStore::save_request` (the proxy-password secret is written after the row) and
//! `llm::store::upsert_rollup` (atomic only when its caller already holds a transaction).

use rusqlite::{Connection, TransactionBehavior};
use serde::Serialize;
use serde::de::DeserializeOwned;

use spanbay_base::error::AppError;

/// Runs `body` as one all-or-nothing unit that composes with the caller's own transaction.
///
/// * On an autocommit connection it starts `BEGIN IMMEDIATE`, taking the write lock up front.
///   A deferred transaction that reads first and writes later fails at once with
///   `SQLITE_BUSY_SNAPSHOT` when another connection (the gateway's log writer, a pooled
///   connection) committed in between; an immediate one waits for the busy timeout instead.
/// * When a transaction is already open on `connection` it runs inside `SAVEPOINT pd_atomic`, so
///   nesting never fails with "cannot start a transaction within a transaction". Nested
///   savepoints share the name; SQLite resolves `ROLLBACK TO` / `RELEASE` to the innermost one.
/// * `Ok` from `body` (an early return included) commits, or releases the savepoint into the
///   outer transaction. `Err` rolls back to where this call began; an outer transaction stays
///   open and usable. A failure while cleaning up the savepoint is deliberately swallowed so the
///   caller sees the error of `body`.
///
/// # Errors
/// The error of `body`, or the SQLite error of `BEGIN`, `SAVEPOINT`, `COMMIT` or `RELEASE`
/// converted through `map_err`.
pub(crate) fn atomic_with<T, E>(
    connection: &Connection,
    map_err: impl Fn(rusqlite::Error) -> E,
    body: impl FnOnce(&Connection) -> Result<T, E>,
) -> Result<T, E> {
    if connection.is_autocommit() {
        let transaction =
            rusqlite::Transaction::new_unchecked(connection, TransactionBehavior::Immediate)
                .map_err(&map_err)?;
        let value = body(&transaction)?;
        transaction.commit().map_err(&map_err)?;
        return Ok(value);
    }
    connection
        .execute_batch("SAVEPOINT pd_atomic")
        .map_err(&map_err)?;
    match body(connection) {
        Ok(value) => {
            connection
                .execute_batch("RELEASE pd_atomic")
                .map_err(&map_err)?;
            Ok(value)
        }
        Err(error) => {
            // Best effort: surface the error from `body` rather than a cleanup failure.
            let _ = connection.execute_batch("ROLLBACK TO pd_atomic; RELEASE pd_atomic");
            Err(error)
        }
    }
}

/// [`atomic_with`] for callers whose error type converts from [`rusqlite::Error`] (such as
/// [`crate::storage::StorageError`] or `rusqlite::Error` itself). Same transaction contract.
pub(crate) fn atomic<T, E: From<rusqlite::Error>>(
    connection: &Connection,
    body: impl FnOnce(&Connection) -> Result<T, E>,
) -> Result<T, E> {
    atomic_with(connection, E::from, body)
}

/// [`atomic_with`] for LLM repository functions returning [`AppError`]; SQLite failures become
/// [`AppError::Database`] through [`db`]. Same transaction contract.
pub(crate) fn atomic_app<T>(
    connection: &Connection,
    body: impl FnOnce(&Connection) -> Result<T, AppError>,
) -> Result<T, AppError> {
    atomic_with(connection, db, body)
}

/// A SQLite failure as [`AppError::Database`], keeping the SQLite message.
pub(crate) fn db(error: rusqlite::Error) -> AppError {
    AppError::database(error.to_string())
}

/// `value` serialized as JSON text for a `*_json` column.
///
/// # Errors
/// [`AppError::Internal`] when `value` cannot be serialized.
pub(crate) fn json_text<T: Serialize>(value: &T) -> Result<String, AppError> {
    serde_json::to_string(value).map_err(|error| AppError::internal(error.to_string()))
}

/// Parses a stored JSON column; `what` names it in the error.
///
/// # Errors
/// [`AppError::Database`] ("stored {what} is invalid") when `text` does not parse as `T`.
pub(crate) fn from_json<T: DeserializeOwned>(text: &str, what: &str) -> Result<T, AppError> {
    serde_json::from_str(text)
        .map_err(|error| AppError::database(format!("stored {what} is invalid: {error}")))
}

/// The serde name of a fieldless `rename_all = "snake_case"` enum variant, as stored in a text
/// column.
///
/// Only meant for such enums: serializing them cannot fail and always yields a string, so the
/// `Err` arm is defensive (it reports a struct-like or numeric value it was never given).
pub(crate) fn enum_variant_name<T: Serialize>(value: &T) -> Result<String, String> {
    match serde_json::to_value(value) {
        Ok(serde_json::Value::String(text)) => Ok(text),
        Ok(other) => Err(format!("{other} is not a plain enum value")),
        Err(error) => Err(error.to_string()),
    }
}

/// The variant of a fieldless `snake_case` enum stored as `text`; `None` for an unknown name.
pub(crate) fn enum_from_text<T: DeserializeOwned>(text: &str) -> Option<T> {
    serde_json::from_value(serde_json::Value::String(text.to_owned())).ok()
}

/// A boolean as the `0` / `1` SQLite stores.
pub(crate) fn flag(value: bool) -> i64 {
    i64::from(value)
}

/// A count or size as the signed integer SQLite stores; saturates at `i64::MAX` instead of
/// wrapping.
pub(crate) fn to_i64(value: u64) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// A stored integer as an unsigned count; a negative value reads as `0`.
pub(crate) fn to_u64(value: i64) -> u64 {
    u64::try_from(value).unwrap_or(0)
}

/// A stored integer as a `u32` count; a negative or out-of-range value reads as `0`.
pub(crate) fn to_u32(value: i64) -> u32 {
    u32::try_from(value).unwrap_or(0)
}

/// A length or index as the signed integer SQLite stores; saturates at `i64::MAX`.
pub(crate) fn i64_from_usize(value: usize) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// A duration in milliseconds as the signed integer SQLite stores; saturates at `i64::MAX`.
pub(crate) fn i64_from_u128(value: u128) -> i64 {
    i64::try_from(value).unwrap_or(i64::MAX)
}

/// Escapes `\`, `%` and `_` with a backslash so `text` matches literally inside a `LIKE`
/// pattern written with `ESCAPE '\'`.
pub(crate) fn escape_like(text: &str) -> String {
    text.chars()
        .fold(String::with_capacity(text.len()), |mut out, ch| {
            if matches!(ch, '\\' | '%' | '_') {
                out.push('\\');
            }
            out.push(ch);
            out
        })
}

#[cfg(test)]
mod tests {
    use super::*;

    fn connection() -> Connection {
        let connection = Connection::open_in_memory().unwrap();
        connection
            .execute_batch("CREATE TABLE t (v INTEGER)")
            .unwrap();
        connection
    }

    fn insert(connection: &Connection, value: i64) -> rusqlite::Result<()> {
        connection.execute("INSERT INTO t (v) VALUES (?1)", [value])?;
        Ok(())
    }

    fn values(connection: &Connection) -> Vec<i64> {
        let mut statement = connection.prepare("SELECT v FROM t ORDER BY v").unwrap();
        statement
            .query_map([], |row| row.get(0))
            .unwrap()
            .collect::<Result<_, _>>()
            .unwrap()
    }

    fn failed() -> rusqlite::Error {
        rusqlite::Error::QueryReturnedNoRows
    }

    #[test]
    fn a_top_level_body_commits_on_ok_and_rolls_back_on_err() {
        let c = connection();
        atomic(&c, |tx| insert(tx, 1)).unwrap();
        assert!(c.is_autocommit());
        let result: rusqlite::Result<()> = atomic(&c, |tx| {
            insert(tx, 2)?;
            Err(failed())
        });
        assert!(result.is_err());
        assert!(c.is_autocommit());
        assert_eq!(values(&c), [1]);
    }

    #[test]
    fn a_failed_nested_body_undoes_only_its_own_writes() {
        let c = connection();
        atomic(&c, |outer| {
            insert(outer, 1)?;
            let inner: rusqlite::Result<()> = atomic(outer, |tx| {
                insert(tx, 2)?;
                Err(failed())
            });
            assert!(inner.is_err());
            assert!(!outer.is_autocommit(), "the outer transaction stays open");
            insert(outer, 3)
        })
        .unwrap();
        assert_eq!(values(&c), [1, 3]);
    }

    #[test]
    fn a_released_nested_body_rolls_back_with_its_failed_outer_transaction() {
        let c = connection();
        let result: rusqlite::Result<()> = atomic(&c, |outer| {
            insert(outer, 1)?;
            atomic(outer, |tx| insert(tx, 2))?;
            Err(failed())
        });
        assert!(result.is_err());
        assert!(c.is_autocommit());
        assert!(values(&c).is_empty());
    }

    #[test]
    fn same_named_savepoints_roll_back_the_innermost_one_only() {
        let c = connection();
        atomic(&c, |outer| {
            insert(outer, 1)?;
            atomic(outer, |middle| {
                insert(middle, 2)?;
                let inner: rusqlite::Result<()> = atomic(middle, |tx| {
                    insert(tx, 3)?;
                    Err(failed())
                });
                assert!(inner.is_err());
                insert(middle, 4)
            })
        })
        .unwrap();
        assert_eq!(values(&c), [1, 2, 4]);
    }

    #[test]
    fn like_escaping_prefixes_backslash_percent_and_underscore() {
        assert_eq!(escape_like(r"a\b%c_d"), r"a\\b\%c\_d");
        assert_eq!(escape_like("plain é"), "plain é");
    }
}
