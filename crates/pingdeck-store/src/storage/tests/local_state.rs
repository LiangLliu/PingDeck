//! Machine-local state: the tab session and the process ledger.

use rusqlite::params;

use pingdeck_model::Workspace;
use pingdeck_model::agent::ledger::LedgerKind;

use crate::storage::{MAX_TAB_SESSION_BYTES, StorageError, WorkspaceStore};

#[test]
fn an_oversized_stored_tab_session_is_refused_before_it_is_read() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    store
        .save_workspace(&Workspace::new("ws", "Workspace").unwrap())
        .unwrap();
    // Written past `save_tab_session`, which would refuse it.
    store
        .connection()
        .execute(
            "INSERT INTO tab_sessions (workspace_id, payload) VALUES ('ws', ?1)",
            params!["x".repeat(MAX_TAB_SESSION_BYTES + 1)],
        )
        .unwrap();
    assert!(matches!(
        store.load_tab_session("ws"),
        Err(StorageError::TabSessionTooLarge { size_bytes, .. })
            if size_bytes == MAX_TAB_SESSION_BYTES + 1
    ));
}

#[test]
fn the_ledger_is_handed_over_oldest_first() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    for (pid, recorded_ms) in [(1_u32, 20_i64), (2, 10), (3, 10)] {
        store
            .connection()
            .execute(
                "INSERT INTO process_ledger (pid, pgid, started, kind, label, recorded_ms)
                 VALUES (?1, ?1, 's', 'tool', 'l', ?2)",
                params![pid, recorded_ms],
            )
            .unwrap();
    }
    let entries = store.ledger_take_all().unwrap();
    let pids: Vec<_> = entries.iter().map(|entry| entry.pid).collect();
    assert_eq!(pids, [2, 3, 1]);
    assert!(entries.iter().all(|entry| entry.kind == LedgerKind::Tool));
}
