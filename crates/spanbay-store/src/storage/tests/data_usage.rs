//! Storage tests for the Settings › Data usage and retention queries.

use std::collections::HashSet;

use rusqlite::params;

use crate::storage::{CaptureUsage, ChatUsage, WorkspaceStore};

fn store() -> WorkspaceStore {
    WorkspaceStore::open_in_memory().unwrap()
}

fn session(store: &WorkspaceStore, id: &str, updated_ms: i64, pinned: bool, parent: Option<&str>) {
    store
        .connection
        .execute(
            "INSERT INTO agent_sessions (id, title, created_at_ms, updated_at_ms, pinned_at_ms,
                                         parent_session_id)
             VALUES (?1, 't', ?2, ?2, ?3, ?4)",
            params![id, updated_ms, pinned.then_some(1_i64), parent],
        )
        .unwrap();
}

fn message(
    store: &WorkspaceStore,
    id: &str,
    session: &str,
    seq: i64,
    created_ms: i64,
    parts: &str,
) {
    store
        .connection
        .execute(
            "INSERT INTO agent_messages (id, session_id, seq, role, parts_json, created_at_ms)
             VALUES (?1, ?2, ?3, 'user', ?4, ?5)",
            params![id, session, seq, parts, created_ms],
        )
        .unwrap();
}

fn attachment(store: &WorkspaceStore, id: &str, created_ms: i64, extracted_of: Option<&str>) {
    store
        .connection
        .execute(
            "INSERT INTO agent_attachments
                 (id, sha256, name, media_type, size_bytes, kind, created_at_ms, extracted_of)
             VALUES (?1, ?2, 'f', 'text/plain', 10, 'text', ?3, ?4)",
            params![id, format!("{id:0>64}"), created_ms, extracted_of],
        )
        .unwrap();
}

#[test]
fn chat_usage_counts_live_chats_and_message_bytes() {
    let store = store();
    session(&store, "a", 1, false, None);
    session(&store, "b", 1, false, None);
    message(&store, "m1", "a", 1, 1, "[\"héllo\"]");
    store
        .connection
        .execute(
            "UPDATE agent_sessions SET deleted_at_ms = 5 WHERE id = 'b'",
            [],
        )
        .unwrap();
    let usage = store.chat_usage().unwrap();
    // "héllo" is 6 bytes in UTF-8, plus the brackets and quotes.
    assert_eq!(
        usage,
        ChatUsage {
            chats: 1,
            bytes: 10
        }
    );
}

#[test]
fn retention_keeps_pinned_active_children_and_recent_chats() {
    let store = store();
    session(&store, "old", 10, false, None);
    session(&store, "pinned", 10, true, None);
    session(&store, "running", 10, false, None);
    session(&store, "child", 10, false, Some("old"));
    session(&store, "recent", 100, false, None);
    session(&store, "recent-message", 10, false, None);
    message(&store, "m1", "recent-message", 1, 100, "[]");
    store
        .connection
        .execute(
            "INSERT INTO agent_runs (id, session_id, status, model_label, started_at_ms)
             VALUES ('r1', 'running', 'waiting_approval', 'm', 1)",
            [],
        )
        .unwrap();
    assert_eq!(
        store.retention_candidates(50).unwrap(),
        vec!["old".to_owned()]
    );
}

#[test]
fn clearable_attachments_follow_age_journal_and_derived_text() {
    let store = store();
    attachment(&store, "old", 10, None);
    attachment(&store, "new", 100, None);
    attachment(&store, "old-text", 100, Some("old"));
    attachment(&store, "journal", 10, None);
    session(&store, "s", 1, false, None);
    message(&store, "m1", "s", 1, 1, "[]");
    store
        .link_message_attachments("m1", &["old".to_owned()])
        .unwrap();
    let keep: HashSet<String> = [format!("{:0>64}", "journal")].into_iter().collect();

    let some = store.clearable_attachments(Some(50), &keep).unwrap();
    let ids: Vec<(&str, bool, bool)> = some
        .iter()
        .map(|a| (a.id.as_str(), a.derived, a.linked))
        .collect();
    assert_eq!(ids, vec![("old", false, true), ("old-text", true, false)]);

    let all = store.clearable_attachments(None, &keep).unwrap();
    assert_eq!(all.len(), 3);
    assert_eq!(
        store.chats_with_attachments(&["old".to_owned()]).unwrap(),
        1
    );
    assert_eq!(store.attachment_count().unwrap(), 3);
}

#[test]
fn forgetting_previews_clears_only_the_thumbnail() {
    let store = store();
    attachment(&store, "a", 1, None);
    store
        .connection
        .execute("UPDATE agent_attachments SET thumb_data_url = 'data:x'", [])
        .unwrap();
    store.forget_attachment_previews(&["a".to_owned()]).unwrap();
    let row = store.get_attachment("a").unwrap().unwrap();
    assert_eq!(row.thumb_data_url, None);
    assert_eq!(row.size_bytes, 10);
}

#[test]
fn capture_usage_and_page_bytes_read_on_an_empty_database() {
    let store = store();
    assert_eq!(store.capture_usage().unwrap(), CaptureUsage::default());
    assert!(store.database_page_bytes().unwrap() > 0);
}

#[test]
fn chats_with_attachments_counts_each_live_chat_once() {
    let store = store();
    session(&store, "live", 1, false, None);
    session(&store, "other", 1, false, None);
    session(&store, "gone", 1, false, None);
    message(&store, "m1", "live", 1, 1, "[]");
    message(&store, "m3", "other", 1, 1, "[]");
    message(&store, "m4", "gone", 1, 1, "[]");
    for id in ["a", "b"] {
        attachment(&store, id, 1, None);
    }
    store
        .link_message_attachments("m1", &["a".to_owned(), "b".to_owned()])
        .unwrap();
    store
        .link_message_attachments("m3", &["b".to_owned()])
        .unwrap();
    store
        .link_message_attachments("m4", &["a".to_owned()])
        .unwrap();
    store
        .connection
        .execute(
            "UPDATE agent_sessions SET deleted_at_ms = 5 WHERE id = 'gone'",
            [],
        )
        .unwrap();

    // Duplicates, two links in one chat, a tombstoned chat and 596 unknown ids.
    let mut ids = vec![
        "a".to_owned(),
        "b".to_owned(),
        "a".to_owned(),
        "b".to_owned(),
    ];
    ids.extend((0..596).map(|n| format!("unknown_{n}")));
    assert_eq!(ids.len(), 600);
    assert_eq!(store.chats_with_attachments(&ids).unwrap(), 2);
    assert_eq!(store.chats_with_attachments(&["a".to_owned()]).unwrap(), 1);
    assert_eq!(store.chats_with_attachments(&[]).unwrap(), 0);
}
