//! Baseline schema creation and version checks.

use rusqlite::Connection;

use crate::storage::schema::{CURRENT_SCHEMA_VERSION, table_exists};
use crate::storage::{StorageError, WorkspaceStore};

use super::support::TestDb;

#[test]
fn fresh_database_initializes_schema_and_reopens_cleanly() {
    let db = TestDb::new("fresh_schema");
    let store = WorkspaceStore::open(&db.path).unwrap();
    assert_eq!(store.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
    for table in [
        "workspaces",
        "requests",
        "agent_sessions",
        "agent_file_changes",
        "llm_providers",
    ] {
        assert!(table_exists(store.connection(), table).unwrap(), "{table}");
    }
    drop(store);

    let reopened = WorkspaceStore::open(&db.path).unwrap();
    assert_eq!(reopened.schema_version().unwrap(), CURRENT_SCHEMA_VERSION);
}

#[test]
fn unsupported_schema_versions_are_rejected() {
    let db = TestDb::new("unsupported_schema");
    {
        let conn = Connection::open(&db.path).unwrap();
        conn.pragma_update(None, "user_version", CURRENT_SCHEMA_VERSION + 1)
            .unwrap();
    }
    let err = WorkspaceStore::open(&db.path).err().unwrap();
    assert!(matches!(
        err,
        StorageError::UnsupportedSchemaVersion {
            version
        } if version == CURRENT_SCHEMA_VERSION + 1
    ));
}

#[test]
fn concurrent_first_opens_of_one_path_both_succeed() {
    let db = TestDb::new("concurrent_first_open");
    // Switching a new file to WAL needs an exclusive lock that SQLite refuses at once (no busy
    // wait) while another connection reads it; production never opens one path twice at once,
    // so the race exercised here starts with the empty database already in WAL mode.
    Connection::open(&db.path)
        .unwrap()
        .pragma_update(None, "journal_mode", "WAL")
        .unwrap();
    let barrier = std::sync::Barrier::new(2);
    std::thread::scope(|scope| {
        let opens: Vec<_> = (0..2)
            .map(|index| {
                let barrier = &barrier;
                let path = &db.path;
                scope.spawn(move || {
                    barrier.wait();
                    let store = WorkspaceStore::open(path).unwrap();
                    store
                        .secret_put(&format!("llm.p{index}.header.x"), "value", 1)
                        .unwrap();
                    store.schema_version().unwrap()
                })
            })
            .collect();
        for open in opens {
            assert_eq!(open.join().unwrap(), CURRENT_SCHEMA_VERSION);
        }
    });
    let store = WorkspaceStore::open(&db.path).unwrap();
    for index in 0..2 {
        let secret_ref = format!("llm.p{index}.header.x");
        assert!(
            store.secret_get(&secret_ref).unwrap().is_some(),
            "{secret_ref}"
        );
    }
}

#[test]
fn schema_fingerprint_ignores_layout_but_not_definitions() {
    use crate::storage::schema::fingerprint;
    let base = fingerprint(&["CREATE TABLE t (a TEXT DEFAULT 'x  y');"]);
    let relaid =
        fingerprint(&["-- note\nCREATE   TABLE t (\n    a TEXT DEFAULT 'x  y'); -- end\n"]);
    assert_eq!(base, relaid);
    assert_ne!(
        base,
        fingerprint(&["CREATE TABLE t (a TEXT DEFAULT 'x y');"])
    );
    assert_ne!(
        base,
        fingerprint(&["CREATE TABLE t (a INTEGER DEFAULT 'x  y');"])
    );
}
