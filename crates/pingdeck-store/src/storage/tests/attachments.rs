//! Storage tests for attachment rows and their message links.

use std::collections::HashSet;

use rusqlite::params;

use pingdeck_model::agent::{AgentAttachment, AttachmentKind};

use crate::storage::WorkspaceStore;

fn attachment(id: &str, sha: &str, name: &str) -> AgentAttachment {
    AgentAttachment {
        id: id.to_owned(),
        sha256: sha.to_owned(),
        name: name.to_owned(),
        media_type: "image/png".to_owned(),
        size_bytes: 12,
        kind: AttachmentKind::Image,
        width: Some(3),
        height: Some(4),
        thumb_data_url: None,
        lines: None,
        removed: false,
        created_at_ms: 10,
    }
}

fn message(store: &WorkspaceStore, session: &str, id: &str) {
    store
        .connection
        .execute(
            "INSERT INTO agent_messages (id, session_id, seq, branch_index, role, parts_json, created_at_ms)
             VALUES (?1, ?2, (SELECT COUNT(*) FROM agent_messages), (SELECT COUNT(*) FROM agent_messages), 'user', '[]', 1)",
            params![id, session],
        )
        .expect("message appends");
}

fn store_with_session() -> WorkspaceStore {
    let store = WorkspaceStore::open_in_memory().expect("store opens");
    store
        .connection
        .execute(
            "INSERT INTO agent_sessions (id, title, created_at_ms, updated_at_ms)
             VALUES ('s1', 't', 1, 1)",
            [],
        )
        .expect("session saves");
    store
}

#[test]
fn insert_dedupes_on_content_and_the_first_row_wins() {
    let store = store_with_session();
    let first = store
        .insert_attachment(&attachment("a1", "aa", "first.png"))
        .expect("insert");
    let second = store
        .insert_attachment(&attachment("a2", "aa", "second.png"))
        .expect("dedupe");
    assert_eq!(second, first);
    assert_eq!(second.id, "a1");
    assert!(store.get_attachment("a2").expect("get").is_none());
    assert_eq!(store.get_attachment_by_sha("aa").expect("get"), Some(first));
    assert_eq!(store.get_attachment("nope").expect("get"), None);
}

#[test]
fn rows_round_trip_every_kind_and_missing_dimensions() {
    let store = store_with_session();
    for (index, kind) in [
        AttachmentKind::Image,
        AttachmentKind::Pdf,
        AttachmentKind::Text,
    ]
    .into_iter()
    .enumerate()
    {
        let mut row = attachment(&format!("a{index}"), &format!("h{index}"), "f");
        row.kind = kind;
        row.width = None;
        row.height = None;
        assert_eq!(store.insert_attachment(&row).expect("insert"), row);
    }
}

#[test]
fn links_keep_order_and_group_by_message() {
    let store = store_with_session();
    for (id, sha) in [("a1", "h1"), ("a2", "h2"), ("a3", "h3")] {
        store
            .insert_attachment(&attachment(id, sha, id))
            .expect("insert");
    }
    message(&store, "s1", "m1");
    message(&store, "s1", "m2");
    message(&store, "s1", "m3");
    store
        .link_message_attachments("m1", &["a3".to_owned(), "a1".to_owned()])
        .expect("link");
    store
        .link_message_attachments("m2", &["a2".to_owned()])
        .expect("link");
    // Linking again is idempotent.
    store
        .link_message_attachments("m2", &["a2".to_owned()])
        .expect("relink");

    let listed = store
        .list_message_attachments(&["m1".to_owned(), "m2".to_owned(), "m3".to_owned()])
        .expect("list");
    let ids =
        |message: &str| -> Vec<&str> { listed[message].iter().map(|a| a.id.as_str()).collect() };
    assert_eq!(ids("m1"), ["a3", "a1"]);
    assert_eq!(ids("m2"), ["a2"]);
    assert!(!listed.contains_key("m3"));
    assert!(
        store
            .list_message_attachments(&[])
            .expect("empty")
            .is_empty()
    );
}

#[test]
fn unreferenced_rows_are_deletable_and_referenced_ones_are_protected() {
    let store = store_with_session();
    for (id, sha) in [("a1", "h1"), ("a2", "h2")] {
        store
            .insert_attachment(&attachment(id, sha, id))
            .expect("insert");
    }
    message(&store, "s1", "m1");
    store
        .link_message_attachments("m1", &["a1".to_owned()])
        .expect("link");

    let orphans = store.list_unreferenced_attachments().expect("list");
    assert_eq!(
        orphans.iter().map(|a| a.id.as_str()).collect::<Vec<_>>(),
        ["a2"]
    );
    assert!(store.delete_attachment_rows(&["a1".to_owned()]).is_err());
    assert_eq!(
        store
            .delete_attachment_rows(&["a2".to_owned()])
            .expect("delete"),
        1
    );
    assert_eq!(
        store.referenced_shas().expect("shas"),
        HashSet::from(["h1".to_owned()])
    );

    // Deleting the message frees its attachment.
    store
        .connection
        .execute("DELETE FROM agent_sessions WHERE id = 's1'", [])
        .expect("delete session");
    assert_eq!(
        store.list_unreferenced_attachments().expect("list").len(),
        1
    );
}

#[test]
fn thumbnails_are_set_and_a_duplicate_upload_fills_a_missing_one() {
    let store = store_with_session();
    let first = store
        .insert_attachment(&attachment("a1", "aa", "p.png"))
        .expect("insert");
    assert_eq!(first.thumb_data_url, None);

    let mut again = attachment("a2", "aa", "p2.png");
    again.thumb_data_url = Some("data:image/png;base64,AAAA".to_owned());
    let merged = store.insert_attachment(&again).expect("dedupe");
    assert_eq!(merged.id, "a1");
    assert_eq!(
        merged.thumb_data_url.as_deref(),
        Some("data:image/png;base64,AAAA")
    );

    // An existing thumbnail is kept by a later upload; only set_attachment_thumb replaces it.
    again.thumb_data_url = Some("data:image/png;base64,BBBB".to_owned());
    let kept = store.insert_attachment(&again).expect("dedupe");
    assert_eq!(
        kept.thumb_data_url.as_deref(),
        Some("data:image/png;base64,AAAA")
    );
    assert!(
        store
            .set_attachment_thumb("a1", "data:image/png;base64,CCCC")
            .expect("set")
    );
    assert!(!store.set_attachment_thumb("nope", "x").expect("unknown id"));
    assert_eq!(
        store
            .get_attachment("a1")
            .expect("get")
            .and_then(|row| row.thumb_data_url)
            .as_deref(),
        Some("data:image/png;base64,CCCC")
    );
}

#[test]
fn derived_text_is_found_by_its_source_and_lives_as_long_as_the_source_is_linked() {
    let store = store_with_session();
    let mut pdf = attachment("pdf", "h-pdf", "r.pdf");
    pdf.kind = AttachmentKind::Pdf;
    store.insert_attachment(&pdf).expect("pdf");
    let mut text = attachment("txt", "h-txt", "r.pdf.txt");
    text.kind = AttachmentKind::Text;
    store
        .insert_extracted_attachment(&text, "pdf")
        .expect("derived");

    assert_eq!(
        store
            .get_extracted_attachment("pdf")
            .expect("get")
            .map(|row| row.id),
        Some("txt".to_owned())
    );
    assert_eq!(store.get_extracted_attachment("txt").expect("get"), None);

    // The PDF is linked: its derived text is not an orphan, and neither is the PDF.
    message(&store, "s1", "m1");
    store
        .link_message_attachments("m1", &["pdf".to_owned()])
        .expect("link");
    assert!(
        store
            .list_unreferenced_attachments()
            .expect("list")
            .is_empty()
    );

    // Unlinked, both are orphans together.
    store
        .connection
        .execute("DELETE FROM agent_sessions WHERE id = 's1'", [])
        .expect("delete session");
    let orphans: Vec<String> = store
        .list_unreferenced_attachments()
        .expect("list")
        .into_iter()
        .map(|row| row.id)
        .collect();
    assert_eq!(orphans, ["pdf", "txt"]);
}

#[test]
fn journal_blobs_are_listed_from_both_columns() {
    let store = store_with_session();
    for (id, before, after) in [
        ("c1", Some("b1"), Some("a1")),
        ("c2", None, Some("a2")),
        ("c3", Some("b1"), None),
    ] {
        store
            .connection
            .execute(
                "INSERT INTO agent_file_changes
                     (id, session_id, run_id, call_id, path, kind, before_sha256,
                      after_sha256, created_ms)
                 VALUES (?1, 's1', 'r', 'c', '/p', 'edit', ?2, ?3, 1)",
                params![id, before, after],
            )
            .expect("change row");
    }
    assert_eq!(
        store.journal_blob_shas().expect("shas"),
        HashSet::from(["b1".to_owned(), "a1".to_owned(), "a2".to_owned()])
    );
}

#[test]
fn listing_many_messages_keeps_link_order_and_skips_messages_without_links() {
    let store = store_with_session();
    for (id, sha) in [("a1", "h1"), ("a2", "h2"), ("a3", "h3")] {
        store
            .insert_attachment(&attachment(id, sha, id))
            .expect("insert");
    }
    message(&store, "s1", "m_first");
    message(&store, "s1", "m_last");
    store
        .link_message_attachments("m_first", &["a2".to_owned(), "a1".to_owned()])
        .expect("link");
    store
        .link_message_attachments("m_last", &["a3".to_owned(), "a2".to_owned()])
        .expect("link");

    // 600 ids, most of them unknown, the linked ones repeated at both ends.
    let mut ids = vec!["m_first".to_owned(), "m_last".to_owned()];
    ids.extend((0..596).map(|n| format!("m_unknown_{n}")));
    ids.extend(["m_last".to_owned(), "m_first".to_owned()]);
    assert_eq!(ids.len(), 600);
    let listed = store.list_message_attachments(&ids).expect("list");
    let order =
        |message: &str| -> Vec<&str> { listed[message].iter().map(|a| a.id.as_str()).collect() };
    assert_eq!(listed.len(), 2);
    assert_eq!(order("m_first"), ["a2", "a1"]);
    assert_eq!(order("m_last"), ["a3", "a2"]);
}
