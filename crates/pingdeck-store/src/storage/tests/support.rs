//! Shared helpers for storage tests.

use std::path::PathBuf;
use std::sync::atomic::{AtomicUsize, Ordering};

/// Prefix of the private directory each test database lives in.
const DIR_PREFIX: &str = "pingdeck-storage-";

/// A database path in a directory of its own (process id plus a counter, so no two tests share
/// one). The directory, with the `-wal`, `-shm` and `.secrets` siblings, is removed on drop.
pub(super) struct TestDb {
    dir: PathBuf,
    pub(super) path: PathBuf,
}

impl TestDb {
    pub(super) fn new(name: &str) -> Self {
        static NEXT: AtomicUsize = AtomicUsize::new(0);
        let dir = std::env::temp_dir().join(format!(
            "{DIR_PREFIX}{}-{}-{name}",
            std::process::id(),
            NEXT.fetch_add(1, Ordering::Relaxed)
        ));
        std::fs::create_dir_all(&dir).unwrap();
        let path = dir.join("workspace.sqlite");
        Self { dir, path }
    }
}

impl Drop for TestDb {
    fn drop(&mut self) {
        let _ = std::fs::remove_dir_all(&self.dir);
    }
}
