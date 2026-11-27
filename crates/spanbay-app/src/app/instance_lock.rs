//! One SpanBay process per data directory (docs/architecture.md §3).
//!
//! Single responsibility: hold an exclusive advisory lock on `<root>/spanbay.lock` for the life
//! of the process. The desktop shell takes it before it opens the database, so the start-up
//! recovery (interrupted agent runs, Full access, the file journal, the process ledger) only ever
//! runs in the process that owns the directory: a second process would otherwise treat the first
//! one's live runs and child processes as a crash's leftovers and stop them. The operating system
//! drops the lock when the process ends, a crash included, so a stale lock file never blocks a
//! start.

use std::fs::{File, TryLockError};
use std::path::{Path, PathBuf};

use crate::error::AppError;
use crate::platform::{DataDirs, create_private_dir_all, restrict_file_to_owner};

/// Name of the lock file inside [`DataDirs::root`].
pub const LOCK_FILE_NAME: &str = "spanbay.lock";

/// The held lock; dropping it releases the directory.
#[derive(Debug)]
pub struct DataDirLock {
    _file: File,
    path: PathBuf,
}

impl DataDirLock {
    /// Locks `dirs.root` (creating the root owner-only when it is missing, and the lock file
    /// with mode `0600` on Unix).
    ///
    /// # Errors
    /// [`AppError::Conflict`] when another process holds the lock; [`AppError::Io`] when the
    /// root or the lock file cannot be created or the platform refuses the lock.
    pub fn acquire(dirs: &DataDirs) -> Result<Self, AppError> {
        if !dirs.root.exists() {
            create_private_dir_all(&dirs.root).map_err(|error| {
                AppError::io(format!("cannot create {}: {error}", dirs.root.display()))
            })?;
        }
        let path = dirs.root.join(LOCK_FILE_NAME);
        let file = open_lock_file(&path)
            .map_err(|error| AppError::io(format!("cannot open {}: {error}", path.display())))?;
        match file.try_lock() {
            Ok(()) => Ok(Self { _file: file, path }),
            Err(TryLockError::WouldBlock) => Err(AppError::conflict(format!(
                "another SpanBay process is using the data folder {}",
                dirs.root.display()
            ))),
            Err(TryLockError::Error(error)) => Err(AppError::io(format!(
                "cannot lock {}: {error}",
                path.display()
            ))),
        }
    }

    /// The lock file.
    #[must_use]
    pub fn path(&self) -> &Path {
        &self.path
    }
}

/// Opens (creating, never truncating) the lock file for reading and writing: Windows refuses
/// `LockFileEx` on a handle opened only for append.
fn open_lock_file(path: &Path) -> std::io::Result<File> {
    let mut options = std::fs::OpenOptions::new();
    options.read(true).write(true).create(true).truncate(false);
    #[cfg(unix)]
    {
        use std::os::unix::fs::OpenOptionsExt;
        options.mode(0o600);
    }
    let file = options.open(path)?;
    restrict_file_to_owner(path)?;
    Ok(file)
}

#[cfg(test)]
mod tests {
    use std::sync::atomic::{AtomicUsize, Ordering};

    use super::*;

    fn scratch() -> DataDirs {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let root = std::env::temp_dir().join(format!(
            "spanbay-instance-lock-{}-{}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        DataDirs::for_database(&root.join("spanbay.sqlite3"))
    }

    #[test]
    fn a_second_holder_is_refused_until_the_first_lets_go() {
        let dirs = scratch();
        let first = DataDirLock::acquire(&dirs).unwrap();
        let second = DataDirLock::acquire(&dirs).unwrap_err();
        assert!(matches!(second, AppError::Conflict { .. }), "{second:?}");
        assert!(second.message().contains(&dirs.root.display().to_string()));
        drop(first);
        let again = DataDirLock::acquire(&dirs).unwrap();
        assert_eq!(again.path(), dirs.root.join(LOCK_FILE_NAME));
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(again.path())
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o777, 0o600);
        }
        drop(again);
        let _ = std::fs::remove_dir_all(&dirs.root);
    }
}
