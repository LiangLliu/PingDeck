//! The connection pool's return contract.

use crate::storage::{DbHandle, WorkspaceStore};

use super::support::TestDb;

#[test]
fn a_connection_dropped_mid_transaction_returns_rolled_back() {
    let db = TestDb::new("pool_rollback");
    drop(WorkspaceStore::open(&db.path).unwrap());
    let handle = DbHandle::path(&db.path);

    let connection = handle.open().unwrap();
    connection
        .execute_batch(
            "BEGIN IMMEDIATE;
             INSERT INTO app_settings (section, version, value_json, updated_at_ms)
             VALUES ('dropped', 1, '{}', 0);",
        )
        .unwrap();
    drop(connection);

    let connection = handle.open().unwrap();
    assert!(connection.is_autocommit());
    let rows: i64 = connection
        .query_row(
            "SELECT count(*) FROM app_settings WHERE section = 'dropped'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(rows, 0);
}
