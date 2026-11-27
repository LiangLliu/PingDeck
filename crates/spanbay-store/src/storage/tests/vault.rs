//! Where the `secrets` table lives: the vault file for file databases, `main` in memory.

use rusqlite::Connection;

use crate::storage::vault::vault_path;
use crate::storage::{StorageError, WorkspaceStore};

use super::support::TestDb;

fn tables_named_secrets(connection: &Connection, schema: &str) -> i64 {
    connection
        .query_row(
            &format!("SELECT count(*) FROM {schema}.sqlite_master WHERE name = 'secrets'"),
            [],
            |row| row.get(0),
        )
        .unwrap()
}

#[test]
fn a_file_database_keeps_its_secrets_in_the_vault_file_only() {
    let db = TestDb::new("vault_file");
    let store = WorkspaceStore::open(&db.path).unwrap();
    store
        .secret_put("request.r1.proxy_password", "hunter22", 5)
        .unwrap();
    assert_eq!(tables_named_secrets(store.connection(), "main"), 0);
    drop(store);

    let vault = Connection::open(vault_path(&db.path)).unwrap();
    let value: String = vault
        .query_row(
            "SELECT value_text FROM secrets WHERE secret_ref = 'request.r1.proxy_password'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(value, "hunter22");
}

#[test]
fn a_file_database_with_secrets_in_main_is_refused() {
    let db = TestDb::new("vault_legacy");
    Connection::open(&db.path)
        .unwrap()
        .execute_batch("CREATE TABLE secrets (secret_ref TEXT PRIMARY KEY)")
        .unwrap();
    assert!(matches!(
        WorkspaceStore::open(&db.path),
        Err(StorageError::UnsupportedSchemaVersion { version: 0 })
    ));
}

#[test]
fn an_in_memory_store_keeps_its_secrets_in_main() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    assert_eq!(tables_named_secrets(store.connection(), "main"), 1);
}
