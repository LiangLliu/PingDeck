//! [`SecretStore`] over the `secrets` table of the application database: plaintext, in the
//! `<db>.secrets` vault file when one is attached (never encrypted).

use std::sync::{Arc, Mutex, PoisonError};

use rusqlite::Connection;
use spanbay_base::{
    error::AppError,
    secrets::{MIN_REDACTION_CHARS, SecretHint, SecretStore},
};

use crate::storage::{DbHandle, StorageError, WorkspaceStore, secrets};

/// Where the secret statements run.
#[derive(Clone)]
enum Backend {
    /// Pooled connections of an on-disk database: the shared store lock is never taken, so a
    /// caller that holds it can still read and write secrets.
    Pool(DbHandle),
    /// The shared store connection (an in-memory store has no file to pool): every call takes the
    /// store lock, so it must not be used while the caller already holds that lock.
    Shared(Arc<Mutex<WorkspaceStore>>),
}

/// The default [`SecretStore`].
#[derive(Clone)]
pub struct SqliteSecretStore {
    backend: Backend,
}

impl SqliteSecretStore {
    /// A store over the shared store connection; see [`SqliteSecretStore::for_database`] for an
    /// on-disk database.
    #[must_use]
    pub fn new(store: Arc<Mutex<WorkspaceStore>>) -> Self {
        Self {
            backend: Backend::Shared(store),
        }
    }

    /// A store over `db`'s pooled connections when it is on disk, else over the shared `store`
    /// connection.
    #[must_use]
    pub fn for_database(store: Arc<Mutex<WorkspaceStore>>, db: &DbHandle) -> Self {
        match db {
            DbHandle::Path { .. } => Self {
                backend: Backend::Pool(db.clone()),
            },
            DbHandle::Unavailable => Self::new(store),
        }
    }

    /// A store over a private in-memory database, for headless tools and tests without an
    /// application state.
    ///
    /// # Errors
    /// [`AppError::Database`] when SQLite cannot open the in-memory database.
    pub fn in_memory() -> Result<Self, AppError> {
        Ok(Self::new(Arc::new(Mutex::new(
            WorkspaceStore::open_in_memory()?,
        ))))
    }

    /// Runs `work` on a pooled connection or under the shared store lock.
    fn with<T>(
        &self,
        work: impl FnOnce(&Connection) -> Result<T, StorageError>,
    ) -> Result<T, AppError> {
        match &self.backend {
            Backend::Pool(db) => {
                let connection = db.open()?;
                work(&connection).map_err(AppError::from)
            }
            Backend::Shared(store) => {
                let store = store.lock().unwrap_or_else(PoisonError::into_inner);
                work(store.connection()).map_err(AppError::from)
            }
        }
    }
}

impl SecretStore for SqliteSecretStore {
    fn get(&self, secret_ref: &str) -> Result<Option<String>, AppError> {
        self.with(|connection| secrets::get(connection, secret_ref))
    }

    fn put(&self, secret_ref: &str, plaintext: &str) -> Result<SecretHint, AppError> {
        self.with(|connection| {
            secrets::put(
                connection,
                secret_ref,
                plaintext,
                spanbay_base::time::now_ms(),
            )
        })
    }

    fn delete(&self, secret_ref: &str) -> Result<bool, AppError> {
        self.with(|connection| secrets::delete(connection, secret_ref))
    }

    fn hint(&self, secret_ref: &str) -> Result<SecretHint, AppError> {
        self.with(|connection| secrets::hint(connection, secret_ref))
    }

    /// Fails open: the trait cannot return an error, so a failed read yields no needles and the
    /// caller redacts nothing.
    fn active_redaction_needles(&self) -> Vec<String> {
        self.with(|connection| secrets::values_at_least(connection, MIN_REDACTION_CHARS))
            .unwrap_or_default()
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    #[test]
    fn an_on_disk_store_serves_secrets_while_the_store_lock_is_held() {
        let dir = std::env::temp_dir().join(format!(
            "spanbay-secrets-{}-{}",
            std::process::id(),
            spanbay_base::time::now_ms()
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("spanbay.sqlite3");
        let store = Arc::new(Mutex::new(WorkspaceStore::open(&path).unwrap()));
        let secrets = SqliteSecretStore::for_database(Arc::clone(&store), &DbHandle::path(&path));
        let held = store.lock().unwrap();
        secrets.put("env.e.token", "value-long-enough").unwrap();
        assert_eq!(
            secrets.get("env.e.token").unwrap().as_deref(),
            Some("value-long-enough")
        );
        // The shared connection sees what the pooled one wrote.
        assert_eq!(
            held.secret_get("env.e.token").unwrap().as_deref(),
            Some("value-long-enough")
        );
        assert_eq!(
            secrets.active_redaction_needles(),
            vec!["value-long-enough".to_owned()]
        );
        assert!(secrets.delete("env.e.token").unwrap());
        drop(held);
        let _ = std::fs::remove_dir_all(dir);
    }
}
