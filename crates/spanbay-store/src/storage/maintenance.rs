//! Whole-file database care: moving aside a database this build cannot open, consistent copies
//! (`VACUUM INTO`) for the in-app backup, compaction (`VACUUM`) and the free space a compaction
//! would give back. The functions take a [`Connection`] so the app can run the long ones on a
//! pooled connection instead of the shared store lock.

use std::path::{Path, PathBuf};

use rusqlite::Connection;
use spanbay_base::platform::DataDirs;

use super::vault::vault_path;
use super::{StorageError, WorkspaceStore};

/// A database moved aside at start because another SpanBay build created it.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SetAsideDatabase {
    /// The schema fingerprint the old database carried.
    pub version: i64,
    /// Where each moved file went, the database first and its `.secrets` vault next when it
    /// existed (write-ahead log and journal files and the attachment directory follow).
    pub moved: Vec<PathBuf>,
}

/// What [`backup_database`] wrote.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct BackupFiles {
    /// The copy of the main database.
    pub database: PathBuf,
    /// The copy of the vault next to it (`<database>.secrets`, plaintext, owner-only); `None` for a
    /// store without a vault.
    pub secrets: Option<PathBuf>,
    /// Bytes of both copies together.
    pub bytes: u64,
}

/// Suffixes of the files that belong to one SQLite database, besides the file itself.
const COMPANIONS: [&str; 3] = ["-wal", "-shm", "-journal"];

impl WorkspaceStore {
    /// [`WorkspaceStore::open`], except that a database created by another build (a schema
    /// fingerprint this build does not know) is moved aside, with its vault, journal files and
    /// the `attachments` directory next to it, to `<name>.bak-<UTC time>` and a fresh database is
    /// created in its place. The moved files
    /// are reported so the caller can tell the user where the old data is. Any other failure is
    /// returned as is.
    ///
    /// # Errors
    /// [`StorageError`] when the database cannot be opened, moved aside or created.
    pub fn open_or_set_aside(
        path: impl AsRef<Path>,
    ) -> Result<(Self, Option<SetAsideDatabase>), StorageError> {
        let path = path.as_ref();
        match Self::open(path) {
            Ok(store) => Ok((store, None)),
            Err(StorageError::UnsupportedSchemaVersion { version }) => {
                // The attachment blobs belong to the old database: left in place, the start's
                // attachment GC would find no reference to them in the fresh database and delete
                // them, leaving the moved copy with dangling attachments and undo journal.
                let attachments = DataDirs::for_database(path).attachments;
                let moved = set_aside(path, &[attachments], &backup_stamp()?)?;
                let store = Self::open(path)?;
                Ok((store, Some(SetAsideDatabase { version, moved })))
            }
            Err(error) => Err(error),
        }
    }
}

/// UTC time as `YYYYMMDD-HHMMSS`, from SQLite's clock so no time crate is needed.
fn backup_stamp() -> Result<String, StorageError> {
    Ok(Connection::open_in_memory()?.query_row(
        "SELECT strftime('%Y%m%d-%H%M%S', 'now')",
        [],
        |row| row.get(0),
    )?)
}

/// `file` with `suffix` appended to its name.
fn with_suffix(file: &Path, suffix: &str) -> PathBuf {
    let mut name = file.as_os_str().to_owned();
    name.push(suffix);
    PathBuf::from(name)
}

/// Renames the database at `path`, its vault, their companion files and the `extra` files or
/// directories that belong to it to `<name>.bak-<stamp>` (`-2`, `-3` … when that name is taken,
/// one suffix for all). All or nothing: when one rename fails, the files already moved are put
/// back, so a fresh database never starts next to a stale write-ahead log, vault or blob store.
fn set_aside(path: &Path, extra: &[PathBuf], stamp: &str) -> Result<Vec<PathBuf>, StorageError> {
    let vault = vault_path(path);
    let mut files = vec![path.to_path_buf(), vault.clone()];
    for base in [path, vault.as_path()] {
        files.extend(COMPANIONS.iter().map(|suffix| with_suffix(base, suffix)));
    }
    files.extend(extra.iter().cloned());
    let mut attempt = 1;
    let suffix = loop {
        let suffix = if attempt == 1 {
            format!(".bak-{stamp}")
        } else {
            format!(".bak-{stamp}-{attempt}")
        };
        if !files.iter().any(|file| with_suffix(file, &suffix).exists()) {
            break suffix;
        }
        attempt += 1;
    };
    let mut moved: Vec<(PathBuf, PathBuf)> = Vec::new();
    for file in files.into_iter().filter(|file| file.exists()) {
        let target = with_suffix(&file, &suffix);
        if let Err(error) = std::fs::rename(&file, &target) {
            for (from, to) in moved.iter().rev() {
                let _ = std::fs::rename(to, from);
            }
            return Err(StorageError::Io(std::io::Error::new(
                error.kind(),
                format!(
                    "could not move {} aside to {}: {error}",
                    file.display(),
                    target.display()
                ),
            )));
        }
        moved.push((file, target));
    }
    Ok(moved.into_iter().map(|(_, target)| target).collect())
}

/// Whether `connection` has the vault attached.
fn has_vault(connection: &Connection) -> Result<bool, StorageError> {
    Ok(connection.query_row(
        "SELECT count(*) FROM pragma_database_list WHERE name = 'vault'",
        [],
        |row| row.get::<_, i64>(0),
    )? > 0)
}

/// Writes a consistent copy of the database to `destination` and, when a vault is attached, of
/// the vault to `<destination>.secrets`, each through `VACUUM INTO` (one read transaction, so
/// concurrent writers neither block nor tear the copy). Each copy is built in an owner-only temp
/// file next to its destination and renamed over it at the end, so an existing file is replaced
/// only by a complete copy.
///
/// # Errors
/// [`StorageError`] when a file cannot be created or SQLite fails; nothing is left half written.
pub fn backup_database(
    connection: &Connection,
    destination: &Path,
) -> Result<BackupFiles, StorageError> {
    let secrets = has_vault(connection)?.then(|| vault_path(destination));
    let mut targets = vec![("main", destination.to_path_buf())];
    if let Some(secrets) = &secrets {
        targets.push(("vault", secrets.clone()));
    }
    let mut written: Vec<(PathBuf, PathBuf)> = Vec::new();
    let result = (|| {
        for (schema, target) in &targets {
            let temp = with_suffix(target, ".partial");
            // `VACUUM INTO` needs an absent or empty file; an empty owner-only one keeps the copy
            // (the vault holds secrets in plaintext) unreadable to others from the first byte.
            let _ = std::fs::remove_file(&temp);
            spanbay_base::platform::open_private_append(&temp).map(drop)?;
            written.push((temp.clone(), target.clone()));
            connection.execute(
                &format!("VACUUM {schema} INTO ?1"),
                [temp.to_string_lossy().as_ref()],
            )?;
        }
        Ok::<(), StorageError>(())
    })();
    if let Err(error) = result {
        for (temp, _) in &written {
            let _ = std::fs::remove_file(temp);
        }
        return Err(error);
    }
    let mut bytes = 0;
    for (temp, target) in &written {
        std::fs::rename(temp, target)?;
        bytes += std::fs::metadata(target).map_or(0, |meta| meta.len());
    }
    Ok(BackupFiles {
        database: destination.to_path_buf(),
        secrets,
        bytes,
    })
}

/// Rebuilds the database (and the vault when attached) without free pages and truncates the
/// write-ahead log, so space freed by clears goes back to the file system. Writers on other
/// connections wait while it runs (up to the busy timeout).
///
/// # Errors
/// [`StorageError::Sqlite`] when SQLite cannot vacuum (for example inside a transaction).
pub fn compact_database(connection: &Connection) -> Result<(), StorageError> {
    connection.execute_batch("VACUUM main;")?;
    if has_vault(connection)? {
        connection.execute_batch("VACUUM vault;")?;
    }
    // The pragma answers a row (busy, log, checkpointed): read it rather than execute it.
    connection.query_row("PRAGMA main.wal_checkpoint(TRUNCATE)", [], |_| Ok(()))?;
    Ok(())
}

/// Bytes of the main database's free pages: what a compaction would give back to the disk.
///
/// # Errors
/// [`StorageError::Sqlite`] when the pragmas cannot be read.
pub fn free_bytes(connection: &Connection) -> Result<u64, StorageError> {
    let pages: i64 = connection.query_row("PRAGMA main.freelist_count", [], |row| row.get(0))?;
    let size: i64 = connection.query_row("PRAGMA main.page_size", [], |row| row.get(0))?;
    Ok(u64::try_from(pages.saturating_mul(size)).unwrap_or(0))
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::storage::schema::CURRENT_SCHEMA_VERSION;
    use spanbay_model::models::Workspace;

    /// A directory of its own per call: the tests run in parallel and each removes its directory.
    fn temp_dir() -> PathBuf {
        static COUNTER: std::sync::atomic::AtomicUsize = std::sync::atomic::AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "spanbay-maint-{}-{}-{}",
            std::process::id(),
            spanbay_base::time::now_ms(),
            COUNTER.fetch_add(1, std::sync::atomic::Ordering::SeqCst)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        dir
    }

    #[test]
    fn a_database_of_another_build_is_moved_aside_with_its_vault_and_a_fresh_one_opens() {
        let dir = temp_dir();
        let db = dir.join("spanbay.sqlite3");
        {
            let store = WorkspaceStore::open(&db).unwrap();
            store
                .save_workspace(&Workspace::new("ws_old", "Old").unwrap())
                .unwrap();
            store
                .connection
                .pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION + 1)
                .unwrap();
        }
        let blob = dir.join("attachments").join("ab").join("ab12");
        std::fs::create_dir_all(blob.parent().unwrap()).unwrap();
        std::fs::write(&blob, b"old attachment").unwrap();
        let (store, aside) = WorkspaceStore::open_or_set_aside(&db).unwrap();
        let aside = aside.expect("the old database is moved aside");
        // The old database's blobs go with it, so the start's attachment GC cannot reach them.
        let moved_blobs = aside
            .moved
            .iter()
            .find(|path| {
                path.file_name()
                    .is_some_and(|name| name.to_string_lossy().starts_with("attachments.bak-"))
            })
            .expect("the attachment directory is moved aside");
        assert_eq!(
            std::fs::read(moved_blobs.join("ab").join("ab12")).unwrap(),
            b"old attachment"
        );
        assert!(!dir.join("attachments").exists());
        assert_eq!(aside.version, CURRENT_SCHEMA_VERSION + 1);
        let names: Vec<String> = aside
            .moved
            .iter()
            .map(|path| path.file_name().unwrap().to_string_lossy().into_owned())
            .collect();
        assert!(names[0].starts_with("spanbay.sqlite3.bak-"), "{names:?}");
        assert!(
            names
                .iter()
                .any(|name| name.starts_with("spanbay.sqlite3.secrets.bak-")),
            "{names:?}"
        );
        assert!(store.list_workspaces().unwrap().is_empty());
        assert_eq!(store.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
        // The moved copy still holds the old data.
        let old = Connection::open(&aside.moved[0]).unwrap();
        let count: i64 = old
            .query_row("SELECT count(*) FROM workspaces", [], |row| row.get(0))
            .unwrap();
        assert_eq!(count, 1);
        drop(store);
        // A current database opens without moving anything.
        let (_, again) = WorkspaceStore::open_or_set_aside(&db).unwrap();
        assert!(again.is_none());
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn a_taken_backup_name_gets_a_counter() {
        let dir = temp_dir();
        let db = dir.join("a.sqlite3");
        std::fs::write(&db, b"x").unwrap();
        std::fs::write(with_suffix(&db, ".bak-stamp"), b"older").unwrap();
        let moved = set_aside(&db, &[dir.join("attachments")], "stamp").unwrap();
        assert_eq!(moved, vec![with_suffix(&db, ".bak-stamp-2")]);
        assert_eq!(
            std::fs::read(with_suffix(&db, ".bak-stamp")).unwrap(),
            b"older"
        );
        let _ = std::fs::remove_dir_all(dir);
    }

    #[test]
    fn backup_copies_database_and_vault_and_compaction_returns_free_pages() {
        let dir = temp_dir();
        let db = dir.join("live.sqlite3");
        let store = WorkspaceStore::open(&db).unwrap();
        store
            .save_workspace(&Workspace::new("ws_1", "One").unwrap())
            .unwrap();
        store.secret_put("env.e.token", "s3cr3t-value", 1).unwrap();
        store
            .connection
            .execute_batch(
                "CREATE TABLE filler(x BLOB); \
                 WITH RECURSIVE n(i) AS (SELECT 1 UNION ALL SELECT i + 1 FROM n WHERE i < 64) \
                 INSERT INTO filler SELECT zeroblob(4096) FROM n; \
                 DROP TABLE filler;",
            )
            .unwrap();
        assert!(free_bytes(&store.connection).unwrap() > 0);

        let target = dir.join("copy.sqlite3");
        std::fs::write(&target, b"an older backup").unwrap();
        let files = backup_database(&store.connection, &target).unwrap();
        assert_eq!(files.secrets, Some(dir.join("copy.sqlite3.secrets")));
        assert!(files.bytes > 0);
        let copy = Connection::open(&target).unwrap();
        let workspaces: i64 = copy
            .query_row("SELECT count(*) FROM workspaces", [], |row| row.get(0))
            .unwrap();
        assert_eq!(workspaces, 1);
        let vault = Connection::open(dir.join("copy.sqlite3.secrets")).unwrap();
        let secret: String = vault
            .query_row("SELECT value_text FROM secrets", [], |row| row.get(0))
            .unwrap();
        assert_eq!(secret, "s3cr3t-value");
        assert!(!with_suffix(&target, ".partial").exists());
        #[cfg(unix)]
        {
            use std::os::unix::fs::PermissionsExt;
            let mode = std::fs::metadata(dir.join("copy.sqlite3.secrets"))
                .unwrap()
                .permissions()
                .mode();
            assert_eq!(mode & 0o077, 0, "the vault copy is owner-only");
        }

        compact_database(&store.connection).unwrap();
        assert_eq!(free_bytes(&store.connection).unwrap(), 0);
        let _ = std::fs::remove_dir_all(dir);
    }
}
