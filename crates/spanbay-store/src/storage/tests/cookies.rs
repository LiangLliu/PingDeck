//! Storage tests for the cookie jar.

use spanbay_model::{CookieJarEntry, CookieJarEntryId, Request, Workspace, WorkspaceId};

use super::super::{COOKIE_JAR_MAX_ROWS, COOKIE_MAX_BYTES, WorkspaceStore};

fn cookie_on(workspace_id: &str, name: &str, value: &str, stamp: usize) -> CookieJarEntry {
    CookieJarEntry::from_response_cookie(
        workspace_id,
        None,
        "https://api.example.com/",
        name,
        value,
        None,
        None,
        None,
        None,
        false,
        false,
        None,
        format!("{stamp:013}"),
    )
    .unwrap()
}

#[test]
fn cookie_jar_round_trips_upserts_and_updates_fields_in_place() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let workspace = Workspace::new("ws_billing", "Billing APIs").unwrap();
    let request = Request::new(
        "req_cookie_source",
        workspace.id.as_str(),
        "Cookie source",
        "https://api.example.com/login",
    )
    .unwrap();
    store.save_workspace(&workspace).unwrap();
    store.save_request(&request).unwrap();

    let explicit = CookieJarEntry::from_response_cookie(
        "ws_billing",
        None,
        "https://api.example.com/account/profile",
        "theme",
        "dark",
        Some(".Example.COM"),
        Some("/"),
        Some("Wed, 21 Oct 2030 07:28:00 GMT"),
        None,
        false,
        false,
        None,
        "1700000000002",
    )
    .unwrap();
    assert_eq!(
        (
            explicit.domain.as_str(),
            explicit.path.as_str(),
            explicit.host_only
        ),
        ("example.com", "/", false)
    );

    let first = CookieJarEntry::from_response_cookie(
        workspace.id.as_str(),
        Some(request.id.as_str()),
        "https://api.example.com/account/profile",
        "session",
        "abc123",
        None,
        None,
        None,
        Some("3600"),
        true,
        true,
        Some("Lax"),
        "1700000000000",
    )
    .unwrap();
    let mut second = first.clone();
    second.value = "def456".into();
    second.max_age = Some("7200".into());
    second.updated_at = "1700000000001".into();

    store.save_cookie_jar_entry(&first).unwrap();
    store.save_cookie_jar_entry(&second).unwrap();
    let entries = store.list_cookie_jar(workspace.id.as_str()).unwrap();
    assert_eq!(entries.len(), 1);
    assert_eq!(entries[0], second);
    assert_eq!(
        entries[0].id.as_str(),
        "cookie:ws_billing:api.example.com:/account:session"
    );
    assert_eq!(entries[0].value, "def456");
    assert_eq!(
        entries[0].source_request_id.as_ref().map(|id| id.as_str()),
        Some("req_cookie_source")
    );

    let id = entries[0].id.clone();
    let ws = workspace.id.as_str();
    let missing = CookieJarEntryId::new("cookie_missing").unwrap();
    let mut expected = second;

    let v = store
        .update_cookie_jar_entry_value(ws, &id, "rotated", "1700000001000")
        .unwrap()
        .unwrap();
    expected.value = "rotated".into();
    expected.updated_at = "1700000001000".into();
    assert_eq!(v, expected);
    assert!(
        store
            .update_cookie_jar_entry_value(ws, &missing, "x", "1")
            .unwrap()
            .is_none()
    );

    let f = store
        .update_cookie_jar_entry_flags(ws, &id, false, false, Some("Strict"), "1700000002000")
        .unwrap()
        .unwrap();
    expected.secure = false;
    expected.http_only = false;
    expected.same_site = Some("Strict".into());
    expected.updated_at = "1700000002000".into();
    assert_eq!(f, expected);
    assert!(
        store
            .update_cookie_jar_entry_flags(ws, &missing, true, true, None, "1")
            .unwrap()
            .is_none()
    );

    let e = store
        .update_cookie_jar_entry_expiry(
            ws,
            &id,
            Some("Thu, 01 Jan 2037 00:00:00 GMT"),
            Some("60"),
            "1700000003000",
        )
        .unwrap()
        .unwrap();
    expected.expires = Some("Thu, 01 Jan 2037 00:00:00 GMT".into());
    expected.max_age = Some("60".into());
    expected.updated_at = "1700000003000".into();
    assert_eq!(e, expected);
    let c = store
        .update_cookie_jar_entry_expiry(ws, &id, None, None, "1700000004000")
        .unwrap()
        .unwrap();
    expected.expires = None;
    expected.max_age = None;
    expected.updated_at = "1700000004000".into();
    assert_eq!(c, expected);

    let s = store
        .update_cookie_jar_entry_scope(ws, &id, false, "1700000005000")
        .unwrap()
        .unwrap();
    expected.host_only = false;
    expected.updated_at = "1700000005000".into();
    assert_eq!(s, expected);

    // Rekey and conflict rejection
    let rekeyed = store
        .update_cookie_jar_entry_key(
            ws,
            &id,
            " auth ",
            ".API.EXAMPLE.COM ",
            "admin",
            "1700000006000",
        )
        .unwrap()
        .unwrap();
    expected.id = CookieJarEntryId::new("cookie:ws_billing:api.example.com:/admin:auth").unwrap();
    expected.name = "auth".into();
    expected.domain = "api.example.com".into();
    expected.path = "/admin".into();
    expected.updated_at = "1700000006000".into();
    assert_eq!(rekeyed, expected);
    store.save_cookie_jar_entry(&explicit).unwrap();
    let before_conflict = store.list_cookie_jar(ws).unwrap();
    assert!(
        store
            .update_cookie_jar_entry_key(
                ws,
                &rekeyed.id,
                "theme",
                "example.com",
                "/",
                "1700000007000"
            )
            .is_err()
    );
    assert_eq!(store.list_cookie_jar(ws).unwrap(), before_conflict);

    // Deleting request clears source_request_id
    assert!(store.delete_request(&workspace.id, &request.id).unwrap());
    let after_req_del = store.list_cookie_jar(ws).unwrap();
    assert!(
        after_req_del
            .iter()
            .all(|entry| entry.source_request_id.is_none())
    );
}

#[test]
fn cookie_jar_import_delete_restore_and_workspace_isolation() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let owner = Workspace::new("ws_owner", "Owner").unwrap();
    let other = Workspace::new("ws_other", "Other").unwrap();
    store.save_workspace(&owner).unwrap();
    store.save_workspace(&other).unwrap();
    let ws = owner.id.as_str();

    let c1 = cookie_on(ws, "c1", "v1", 10);
    let c2 = cookie_on(ws, "c2", "v2", 20);
    let c3 = cookie_on(ws, "c3", "v3", 30);
    store.save_cookie_jar_entry(&c1).unwrap();

    let mut c1_updated = c1.clone();
    c1_updated.value = "v1_new".into();
    let imported = store
        .import_cookie_jar_entries(&[c2.clone(), c1_updated, c3.clone()])
        .unwrap();
    assert_eq!(imported.len(), 3);

    // Rollback when one entry has an invalid workspace
    let mut invalid = cookie_on(ws, "bad", "v", 40);
    invalid.workspace_id = WorkspaceId::new("ws_missing").unwrap();
    assert!(
        store
            .import_cookie_jar_entries(&[cookie_on(ws, "ok", "v", 41), invalid])
            .is_err()
    );
    assert_eq!(store.list_cookie_jar(ws).unwrap().len(), 3);

    // Cross-workspace operations are isolated
    let other_ws = other.id.as_str();
    assert!(
        store
            .update_cookie_jar_entry_value(other_ws, &c1.id, "x", "1")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .update_cookie_jar_entry_flags(other_ws, &c1.id, true, true, None, "1")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .update_cookie_jar_entry_expiry(other_ws, &c1.id, None, None, "1")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .update_cookie_jar_entry_scope(other_ws, &c1.id, false, "1")
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .update_cookie_jar_entry_key(other_ws, &c1.id, "x", "a.com", "/", "1")
            .unwrap()
            .is_none()
    );
    assert!(!store.delete_cookie_jar_entry(other_ws, &c1.id).unwrap());
    assert!(
        store
            .delete_cookie_jar_entries(other_ws, std::slice::from_ref(&c1.id))
            .unwrap()
            .is_empty()
    );

    // Delete single, delete many, and restore without overwriting existing
    assert!(store.delete_cookie_jar_entry(ws, &c3.id).unwrap());
    assert!(!store.delete_cookie_jar_entry(ws, &c3.id).unwrap());
    let deleted = store
        .delete_cookie_jar_entries(
            ws,
            &[
                c2.id.clone(),
                CookieJarEntryId::new("cookie_missing").unwrap(),
                c1.id.clone(),
            ],
        )
        .unwrap();
    assert_eq!(deleted, vec![c2.id.clone(), c1.id.clone()]);

    let mut c2_replacement = c2.clone();
    c2_replacement.value = "v2_kept".into();
    store.save_cookie_jar_entry(&c2_replacement).unwrap();
    let restored = store
        .restore_cookie_jar_entries(&[c1.clone(), c2.clone()])
        .unwrap();
    assert_eq!(restored, vec![c1.id]);
    let jar = store.list_cookie_jar(ws).unwrap();
    assert_eq!(jar.iter().find(|e| e.id == c2.id).unwrap().value, "v2_kept");
}

#[test]
fn cookie_jar_expiry_pruning_and_batch_caps() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let workspace = Workspace::new("ws_cap", "Cap").unwrap();
    let other = Workspace::new("ws_other", "Other").unwrap();
    store.save_workspace(&workspace).unwrap();
    store.save_workspace(&other).unwrap();
    let ws = workspace.id.as_str();

    let mut expired_age = cookie_on(ws, "expired_age", "v", 1_700_000_000_000);
    expired_age.max_age = Some("2".into());
    let mut expired_date = cookie_on(ws, "expired_date", "v", 1_700_000_000_000);
    expired_date.expires = Some("Wed, 21 Oct 2015 07:28:00 GMT".into());
    let mut active_age = cookie_on(ws, "active_age", "v", 1_700_000_000_000);
    active_age.max_age = Some("3600".into());
    let mut other_expired = cookie_on(other.id.as_str(), "expired_age", "v", 1_700_000_000_000);
    other_expired.max_age = Some("2".into());
    for entry in [&expired_age, &expired_date, &active_age, &other_expired] {
        store.save_cookie_jar_entry(entry).unwrap();
    }

    let now = std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_700_000_003_000);
    let pruned = store.delete_expired_cookie_jar_entries(ws, now).unwrap();
    assert_eq!(pruned, vec![expired_age.id, expired_date.id]);
    assert_eq!(store.list_cookie_jar(other.id.as_str()).unwrap().len(), 1);
    assert!(
        store
            .delete_expired_cookie_jar_entries(ws, now)
            .unwrap()
            .is_empty()
    );
    assert!(store.delete_cookie_jar_entry(ws, &active_age.id).unwrap());

    // Batch cap eviction, oversized drop, foreign drop, and max_age=0 drop
    let first: Vec<_> = (0..COOKIE_JAR_MAX_ROWS)
        .map(|n| cookie_on(ws, &format!("c{n}"), "v", n))
        .collect();
    store.save_cookie_jar_entries(ws, &first).unwrap();
    assert_eq!(
        store.list_cookie_jar(ws).unwrap().len(),
        COOKIE_JAR_MAX_ROWS
    );
    let mut dead = cookie_on(ws, "c2", "v", 10_004);
    dead.max_age = Some("0".into());
    let next = [
        cookie_on(ws, "new-a", "v", 10_000),
        cookie_on(ws, "new-b", "v", 10_001),
        cookie_on(ws, "huge", &"x".repeat(COOKIE_MAX_BYTES), 10_002),
        cookie_on(other.id.as_str(), "foreign", "v", 10_003),
        dead,
    ];
    store.save_cookie_jar_entries(ws, &next).unwrap();
    let names: Vec<_> = store
        .list_cookie_jar(ws)
        .unwrap()
        .into_iter()
        .map(|e| e.name)
        .collect();
    assert!(names.contains(&"new-a".to_owned()) && names.contains(&"new-b".to_owned()));
    assert!(!names.contains(&"c0".to_owned()) && !names.contains(&"c2".to_owned()));
    assert!(!names.contains(&"huge".to_owned()));
}

#[test]
fn invalid_expiry_attributes_are_preserved_and_not_deleted_as_expired() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    store
        .save_workspace(&Workspace::new("ws", "W").unwrap())
        .unwrap();
    for (name, expires, max_age, updated_at) in [
        ("invalid_age", None, Some("abc"), "1700000000000"),
        ("invalid_updated", None, Some("3600"), "not-a-timestamp"),
        ("invalid_expires", Some("2020-01-01"), None, "1700000000000"),
        ("session", None, None, "1700000000000"),
    ] {
        let mut entry = cookie_on("ws", name, "v", 1_700_000_000_000);
        entry.expires = expires.map(str::to_owned);
        entry.max_age = max_age.map(str::to_owned);
        entry.updated_at = updated_at.to_owned();
        store.save_cookie_jar_entry(&entry).unwrap();
    }
    let now = std::time::UNIX_EPOCH + std::time::Duration::from_millis(1_700_000_003_000);
    assert!(
        store
            .delete_expired_cookie_jar_entries("ws", now)
            .unwrap()
            .is_empty()
    );
    assert_eq!(store.list_cookie_jar("ws").unwrap().len(), 4);
}

#[test]
fn a_saved_cookie_keeps_its_first_creation_time_and_an_import_replaces_it() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    store
        .save_workspace(&Workspace::new("ws_created", "Created").unwrap())
        .unwrap();
    let first = cookie_on("ws_created", "sid", "one", 1);
    store.save_cookie_jar_entry(&first).unwrap();
    let mut again = cookie_on("ws_created", "sid", "two", 2);
    again.created_at = "0000000000009".into();
    store.save_cookie_jar_entry(&again).unwrap();
    let saved = store.list_cookie_jar("ws_created").unwrap();
    assert_eq!(saved.len(), 1);
    assert_eq!(saved[0].value, "two");
    assert_eq!(saved[0].created_at, first.created_at);

    store
        .import_cookie_jar_entries(std::slice::from_ref(&again))
        .unwrap();
    let imported = store.list_cookie_jar("ws_created").unwrap();
    assert_eq!(imported.len(), 1);
    assert_eq!(imported[0].created_at, "0000000000009");
}
