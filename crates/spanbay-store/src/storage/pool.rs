//! Pooled SQLite connections for concurrent readers/writers (such as the LLM gateway and probes).

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::Connection;

use super::schema::{CONNECTION_PRAGMAS, STATEMENT_CACHE_CAPACITY};
use super::vault::attach_vault;
use crate::sqlite::db;
use spanbay_base::error::AppError;

/// Maximum idle SQLite connections retained in [`DbHandle::Path`].
const MAX_POOL_SIZE: usize = 8;

/// A pooled SQLite connection that returns itself to [`DbHandle`] on drop.
#[derive(Debug)]
pub struct DbConn {
    conn: Option<Connection>,
    pool: Arc<Mutex<Vec<Connection>>>,
}

// `conn` is `Some` from construction until `Drop`; indexing the slice avoids an `expect`, which
// the workspace lints deny.
impl std::ops::Deref for DbConn {
    type Target = Connection;

    fn deref(&self) -> &Self::Target {
        &self.conn.as_slice()[0]
    }
}

impl std::ops::DerefMut for DbConn {
    fn deref_mut(&mut self) -> &mut Self::Target {
        &mut self.conn.as_mut_slice()[0]
    }
}

/// Rolls back a transaction left open, then returns the connection to the pool. A connection
/// that is still not in autocommit after the rollback is closed instead of pooled.
/// [`MAX_POOL_SIZE`] caps the idle connections kept, not how many may be open at once.
impl Drop for DbConn {
    fn drop(&mut self) {
        let Some(conn) = self.conn.take() else {
            return;
        };
        if !conn.is_autocommit() {
            let _ = conn.execute_batch("ROLLBACK;");
        }
        if conn.is_autocommit() {
            let mut pool = self.pool.lock().unwrap_or_else(PoisonError::into_inner);
            if pool.len() < MAX_POOL_SIZE {
                pool.push(conn);
            }
        }
    }
}

/// Where concurrent workers open their own SQLite connections.
#[derive(Debug, Clone)]
pub enum DbHandle {
    Path {
        path: PathBuf,
        pool: Arc<Mutex<Vec<Connection>>>,
    },
    /// No file (an in-memory app store): the gateway and the client cannot run.
    Unavailable,
}

impl DbHandle {
    #[must_use]
    pub fn path(path: impl Into<PathBuf>) -> Self {
        Self::Path {
            path: path.into(),
            pool: Arc::new(Mutex::new(Vec::with_capacity(MAX_POOL_SIZE))),
        }
    }

    /// Borrows a connection from the pool or opens a fresh one with the app's pragmas.
    ///
    /// # Errors
    /// [`AppError::Unsupported`] for an in-memory store, [`AppError::Database`] on SQLite failure.
    pub fn open(&self) -> Result<DbConn, AppError> {
        let Self::Path { path, pool } = self else {
            return Err(AppError::unsupported(
                "the LLM gateway needs the on-disk database; this store is in memory",
            ));
        };
        let pooled = pool.lock().unwrap_or_else(PoisonError::into_inner).pop();
        let connection = match pooled {
            Some(connection) => connection,
            None => {
                let connection = Connection::open(path).map_err(db)?;
                connection.execute_batch(CONNECTION_PRAGMAS).map_err(db)?;
                // A `StorageError` (I/O included) is reported as a database failure with its
                // own message, not converted through `From`.
                attach_vault(&connection, path)
                    .map_err(|error| AppError::database(error.to_string()))?;
                connection.set_prepared_statement_cache_capacity(STATEMENT_CACHE_CAPACITY);
                connection
            }
        };
        Ok(DbConn {
            conn: Some(connection),
            pool: Arc::clone(pool),
        })
    }
}
