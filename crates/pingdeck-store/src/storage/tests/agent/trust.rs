//! Folder trust: the stored answer per canonical folder, its instructions hash and revoke.

use super::*;

#[test]
fn folder_trust_is_per_folder_replaced_by_a_new_answer_and_revocable() {
    let store = WorkspaceStore::open_in_memory().unwrap();

    assert!(!store.is_folder_trusted("/work/a").unwrap());
    store
        .folder_trust_set("/work/a", true, Some("h1"), 5)
        .unwrap();
    assert!(store.is_folder_trusted("/work/a").unwrap());
    assert!(!store.is_folder_trusted("/work/a/sub").unwrap());
    // Trusting again after a change replaces the stored instructions hash.
    store
        .folder_trust_set("/work/a", true, Some("h2"), 6)
        .unwrap();
    let stored = store.folder_trust_get("/work/a").unwrap().unwrap();
    assert_eq!(
        (stored.instructions_sha256.as_deref(), stored.decided_at_ms),
        (Some("h2"), 6)
    );
    assert_eq!(store.folder_trust_list().unwrap().len(), 1);
    assert!(store.folder_trust_revoke("/work/a").unwrap());
}
