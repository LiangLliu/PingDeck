//! Execution history and cookie jar use-case tests.

use std::fs;

use super::support::{history_list, temp_path};
use crate::app::misc_logic_service::history_page_impl;
use crate::app::{
    AppState, cookie_jar_delete_expired_impl, cookie_jar_delete_many_impl, cookie_jar_export_impl,
    cookie_jar_export_save_to_path_impl, cookie_jar_import_impl,
    cookie_jar_import_open_file_from_path_impl, cookie_jar_list_impl, cookie_jar_restore_many_impl,
    cookie_jar_timestamp, cookie_jar_update_expiry_impl, cookie_jar_update_flags_impl,
    cookie_jar_update_key_impl, cookie_jar_update_scope_impl, cookie_jar_update_value_impl,
    history_clear_impl, history_delete_many_impl, history_get_impl, history_restore_many_impl,
    request_delete_impl, workspace_create_impl,
};
use crate::error::AppError;
use crate::workspace::{
    CookieJarEntry, CookieJarExport, ExecutionHistoryEntry, HISTORY_BODY_PREVIEW_LIMIT_BYTES,
    HistoryFilter, KeyValue, Request,
};

fn state_with_workspaces(ids: &[&str]) -> AppState {
    let state = AppState::open_in_memory().expect("state should open");
    for id in ids {
        workspace_create_impl(&state, (*id).into(), (*id).into()).unwrap();
    }
    state
}

fn session_cookie(workspace_id: &str, source_request_id: Option<&str>) -> CookieJarEntry {
    CookieJarEntry::from_response_cookie(
        workspace_id,
        source_request_id,
        "https://api.example.com/account/profile",
        "session",
        "abc123",
        None,
        Some("/account"),
        None,
        None,
        true,
        true,
        Some("Lax"),
        "1700000000000",
    )
    .unwrap()
}

fn plain_cookie(
    workspace_id: &str,
    name: &str,
    expires: Option<&str>,
    max_age: Option<&str>,
    timestamp: &str,
) -> CookieJarEntry {
    CookieJarEntry::from_response_cookie(
        workspace_id,
        None,
        "https://api.example.com/a",
        name,
        "v",
        None,
        Some("/a"),
        expires,
        max_age,
        false,
        false,
        None,
        timestamp,
    )
    .unwrap()
}

fn success_history(
    id: &str,
    request: &Request,
    status_code: u16,
    final_url: &str,
    executed_at: &str,
) -> ExecutionHistoryEntry {
    ExecutionHistoryEntry::from_success(
        id,
        request,
        None,
        status_code,
        "OK",
        final_url,
        20,
        "{}",
        2,
        1,
        executed_at,
    )
    .unwrap()
}

#[test]
fn cookie_jar_export_import_and_expiry_flags_obey_the_file_contract() {
    let state = state_with_workspaces(&["ws_billing", "ws_import", "ws_file"]);
    let session = session_cookie("ws_billing", None);
    let theme = CookieJarEntry::from_response_cookie(
        "ws_billing",
        None,
        "https://docs.example.com/",
        "theme",
        "dark",
        None,
        Some("/"),
        Some("Fri, 01 Jan 2100 00:00:00 GMT"),
        None,
        false,
        false,
        None,
        "1700000000001",
    )
    .unwrap();
    let mut old = plain_cookie(
        "ws_billing",
        "old",
        Some("Thu, 01 Jan 1970 00:00:01 GMT"),
        None,
        "1700000000",
    );
    old.updated_at = "1700000000".into();
    {
        let store = state.store();
        for entry in [&theme, &session, &old] {
            store.save_cookie_jar_entry(entry).unwrap();
        }
    }

    let listed = cookie_jar_list_impl(&state, "ws_billing".into()).unwrap();
    let expired: Vec<_> = listed
        .iter()
        .map(|entry| (entry.name.as_str(), entry.expired))
        .collect();
    assert!(expired.contains(&("old", true)) && expired.contains(&("theme", false)));
    assert!(expired.contains(&("session", false)));
    assert!(
        serde_json::to_string(&listed)
            .unwrap()
            .contains("\"expired\":true")
    );

    let export = cookie_jar_export_impl(&state, "ws_billing".into()).unwrap();
    assert_eq!(export.format, "pingdeck.cookie_jar");
    assert_eq!(export.version, 1);
    assert_eq!(export.workspace_id.as_str(), "ws_billing");
    assert_eq!(
        export
            .cookies
            .iter()
            .map(|entry| entry.name.as_str())
            .collect::<Vec<_>>(),
        vec!["old", "session", "theme"]
    );
    assert!(!serde_json::to_string(&export).unwrap().contains("expired"));

    let save_path = temp_path("cookie-jar-export-save.json");
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        fs::write(&save_path, "stale").unwrap();
        fs::set_permissions(&save_path, fs::Permissions::from_mode(0o644)).unwrap();
    }
    let saved =
        cookie_jar_export_save_to_path_impl(&state, "ws_billing".into(), Some(save_path.clone()))
            .unwrap();
    let expected_path = save_path.display().to_string();
    assert!(saved.saved && saved.cookie_count == 3);
    assert_eq!(saved.workspace_id.as_str(), "ws_billing");
    assert_eq!(saved.path.as_deref(), Some(expected_path.as_str()));
    #[cfg(unix)]
    {
        use std::os::unix::fs::PermissionsExt;
        let mode = fs::metadata(&save_path).unwrap().permissions().mode() & 0o777;
        assert_eq!(mode, 0o600, "cookie exports hold session secrets");
    }
    let written = fs::read_to_string(&save_path).unwrap();
    assert!(written.contains("\n  \"format\": \"pingdeck.cookie_jar\""));
    let decoded: CookieJarExport = serde_json::from_str(&written).unwrap();
    assert_eq!(decoded.cookies.len(), 3);

    let payload = serde_json::json!({
        "format": "pingdeck.cookie_jar", "version": 1, "workspace_id": "ws_import",
        "cookies": [{
            "id": "ignored", "workspace_id": "ws_import", "source_request_id": "req_missing",
            "name": "session", "value": "abc123", "domain": ".API.EXAMPLE.COM", "path": "account",
            "host_only": false, "expires": null, "max_age": null, "secure": true,
            "http_only": true, "same_site": "Lax",
            "created_at": "1700000000000", "updated_at": "1700000000000"
        }]
    })
    .to_string();
    let imported = cookie_jar_import_impl(&state, "ws_import".into(), payload).unwrap();
    assert_eq!(imported.imported_count, 1);
    assert_eq!(imported.workspace_id.as_str(), "ws_import");
    assert_eq!(
        imported.imported_ids[0].as_str(),
        "cookie:ws_import:api.example.com:/account:session"
    );
    let jar = cookie_jar_list_impl(&state, "ws_import".into()).unwrap();
    assert_eq!(jar.len(), 1);
    assert_eq!(jar[0].source_request_id, None);
    assert_eq!(
        (jar[0].domain.as_str(), jar[0].path.as_str()),
        ("api.example.com", "/account")
    );

    let opened = cookie_jar_import_open_file_from_path_impl(
        &state,
        "ws_billing".into(),
        Some(save_path.clone()),
    )
    .unwrap();
    assert!(opened.opened && opened.imported_count == 3);
    assert_eq!(opened.workspace_id.as_str(), "ws_billing");
    assert_eq!(opened.path.as_deref(), Some(expected_path.as_str()));
    let _ = fs::remove_file(&save_path);

    let huge = temp_path("huge-cookie-jar-import.json");
    fs::File::create(&huge)
        .unwrap()
        .set_len(crate::app::import_export_service::MAX_IMPORT_FILE_BYTES + 1)
        .unwrap();
    let error =
        cookie_jar_import_open_file_from_path_impl(&state, "ws_file".into(), Some(huge.clone()))
            .unwrap_err();
    assert!(matches!(error, AppError::Validation { .. }));
    assert!(error.message().contains("larger than"));
    let _ = fs::remove_file(huge);
}

#[test]
fn a_netscape_cookies_txt_imports_with_its_scope_flags_and_expiry() {
    let state = state_with_workspaces(&["ws_txt"]);
    let text = "# Netscape HTTP Cookie File\n\
        .example.com\tTRUE\t/\tTRUE\t1445412480\tsid\tabc\n\
        #HttpOnly_api.example.com\tFALSE\t/v1\tFALSE\t0\ttoken\tt=1\n\
        localhost\tFALSE\t/\tFALSE\t0\tdev\n\
        \n";
    let imported = cookie_jar_import_impl(&state, "ws_txt".into(), text.into()).unwrap();
    assert_eq!(imported.imported_count, 3);
    let jar = cookie_jar_list_impl(&state, "ws_txt".into()).unwrap();
    let rows: Vec<_> = jar
        .iter()
        .map(|c| {
            (
                c.name.as_str(),
                c.value.as_str(),
                c.domain.as_str(),
                c.path.as_str(),
                c.host_only,
                c.secure,
                c.http_only,
                c.expires.as_deref(),
                c.expired,
            )
        })
        .collect();
    assert_eq!(
        rows,
        [
            (
                "token",
                "t=1",
                "api.example.com",
                "/v1",
                true,
                false,
                true,
                None,
                false
            ),
            (
                "sid",
                "abc",
                "example.com",
                "/",
                false,
                true,
                false,
                Some("Wed, 21 Oct 2015 07:28:00 GMT"),
                true
            ),
            ("dev", "", "localhost", "/", true, false, false, None, false),
        ]
    );

    for (bad, why) in [
        (
            "example.com\tMAYBE\t/\tFALSE\t0\ta\t1",
            "line 1: subdomains",
        ),
        ("x\n.com\tTRUE\t/\tFALSE\t0\ta\t1", "line 1: not 7 fields"),
        ("# only a comment\n", "No cookies found"),
    ] {
        let error = cookie_jar_import_impl(&state, "ws_txt".into(), bad.into()).unwrap_err();
        assert!(error.message().contains(why), "{bad}: {}", error.message());
    }
}

#[test]
fn cookie_updates_reject_foreign_workspaces_and_persist_each_edited_field() {
    let state = state_with_workspaces(&["ws_billing", "ws_other"]);
    let session = session_cookie("ws_billing", None);
    state.store().save_cookie_jar_entry(&session).unwrap();

    let id = session.id.as_str().to_owned();
    fn not_found<T>(result: Result<T, AppError>) {
        assert!(matches!(result, Err(AppError::NotFound { .. })));
    }
    let other = || String::from("ws_other");
    not_found(cookie_jar_update_value_impl(
        &state,
        other(),
        id.clone(),
        "stolen".into(),
    ));
    not_found(cookie_jar_update_flags_impl(
        &state,
        other(),
        id.clone(),
        false,
        false,
        None,
    ));
    not_found(cookie_jar_update_expiry_impl(
        &state,
        other(),
        id.clone(),
        None,
        Some("1".into()),
    ));
    not_found(cookie_jar_update_scope_impl(
        &state,
        other(),
        id.clone(),
        false,
    ));
    not_found(cookie_jar_update_key_impl(
        &state,
        other(),
        id.clone(),
        "renamed".into(),
        "evil.example".into(),
        "/".into(),
    ));
    assert!(
        cookie_jar_delete_many_impl(&state, other(), vec![id.clone()])
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        state.store().list_cookie_jar("ws_billing").unwrap(),
        std::slice::from_ref(&session)
    );
    let reset = || state.store().save_cookie_jar_entry(&session).unwrap();
    let assert_saved = |entry: &CookieJarEntry| {
        let entries = cookie_jar_list_impl(&state, "ws_billing".into()).unwrap();
        assert_eq!(entries, std::slice::from_ref(entry));
        assert_ne!(entry.updated_at, "1700000000000");
        assert!(entry.updated_at.parse::<u128>().is_ok());
    };

    let val =
        cookie_jar_update_value_impl(&state, "ws_billing".into(), id.clone(), "rotated456".into())
            .unwrap();
    assert_eq!(val.value, "rotated456");
    assert!(val.updated_at != "1700000000000" && val.updated_at.parse::<u128>().is_ok());
    assert_saved(&val);
    reset();

    let flags = cookie_jar_update_flags_impl(
        &state,
        "ws_billing".into(),
        id.clone(),
        true,
        true,
        Some("None".into()),
    )
    .unwrap();
    assert!(flags.secure && flags.http_only && flags.same_site.as_deref() == Some("None"));
    assert_saved(&flags);
    let unset_flags = cookie_jar_update_flags_impl(
        &state,
        "ws_billing".into(),
        id.clone(),
        false,
        false,
        Some(" ".into()),
    )
    .unwrap();
    assert!(!unset_flags.secure && !unset_flags.http_only && unset_flags.same_site.is_none());
    assert_saved(&unset_flags);
    reset();

    // The dash form reads; the stored expiry is the canonical absolute date.
    let expiry = cookie_jar_update_expiry_impl(
        &state,
        "ws_billing".into(),
        id.clone(),
        Some("Mon, 21-Oct-2030 07:28:00 GMT".into()),
        None,
    )
    .unwrap();
    assert_eq!(
        (expiry.expires.as_deref(), expiry.max_age.as_deref()),
        (Some("Mon, 21 Oct 2030 07:28:00 GMT"), None)
    );
    assert_saved(&expiry);
    // A Max-Age wins and is fixed as an absolute date counted from the edit.
    let before = std::time::SystemTime::now();
    let aged = cookie_jar_update_expiry_impl(
        &state,
        "ws_billing".into(),
        id.clone(),
        Some("Mon, 21 Oct 2030 07:28:00 GMT".into()),
        Some("3600".into()),
    )
    .unwrap();
    assert_eq!(aged.max_age, None);
    let at = httpdate::parse_http_date(aged.expires.as_deref().unwrap()).unwrap();
    let lifetime = at.duration_since(before).unwrap_or_default().as_secs();
    assert!((3590..=3600).contains(&lifetime), "{lifetime}");
    assert_saved(&aged);
    let unset_expiry = cookie_jar_update_expiry_impl(
        &state,
        "ws_billing".into(),
        id.clone(),
        Some(" ".into()),
        Some(" ".into()),
    )
    .unwrap();
    assert_eq!(
        (
            unset_expiry.expires.as_deref(),
            unset_expiry.max_age.as_deref()
        ),
        (None, None)
    );
    assert_saved(&unset_expiry);
    reset();

    let scoped =
        cookie_jar_update_scope_impl(&state, "ws_billing".into(), id.clone(), false).unwrap();
    assert!(!scoped.host_only);
    assert_eq!(scoped.value, "abc123");
    assert_saved(&scoped);
    reset();

    let rekeyed = cookie_jar_update_key_impl(
        &state,
        "ws_billing".into(),
        id,
        " auth ".into(),
        ".API.EXAMPLE.COM ".into(),
        "admin".into(),
    )
    .unwrap();
    assert_eq!(
        rekeyed.id.as_str(),
        "cookie:ws_billing:api.example.com:/admin:auth"
    );
    assert!(
        rekeyed.name == "auth"
            && rekeyed.domain == "api.example.com"
            && rekeyed.path == "/admin"
            && rekeyed.host_only
    );
    assert_eq!(rekeyed.value, "abc123");
    assert_saved(&rekeyed);
}

#[test]
fn cookie_batch_delete_restore_and_expired_cleanup_keep_atomicity_and_stale_refs() {
    let state = state_with_workspaces(&["ws_billing"]);
    let theme = plain_cookie("ws_billing", "theme", None, None, "1700000000001");
    let request =
        Request::new("req_src", "ws_billing", "Src", "https://api.example.com/a").unwrap();
    let first = session_cookie("ws_billing", Some(request.id.as_str()));
    let (first_id, second_id) = (first.id.as_str().to_owned(), theme.id.as_str().to_owned());
    {
        let store = state.store();
        store.save_request(&request).unwrap();
        store.save_cookie_jar_entry(&first).unwrap();
        store.save_cookie_jar_entry(&theme).unwrap();
    }
    assert!(
        cookie_jar_delete_many_impl(
            &state,
            "ws_billing".into(),
            vec![first_id.clone(), " ".into(), second_id.clone()],
        )
        .is_err()
    );
    assert_eq!(
        cookie_jar_list_impl(&state, "ws_billing".into())
            .unwrap()
            .len(),
        2
    );
    let deleted = cookie_jar_delete_many_impl(
        &state,
        "ws_billing".into(),
        vec![first_id.clone(), "cookie_missing".into(), second_id.clone()],
    )
    .unwrap();
    assert_eq!(deleted, vec![first_id.clone(), second_id]);
    assert!(
        cookie_jar_list_impl(&state, "ws_billing".into())
            .unwrap()
            .is_empty()
    );

    request_delete_impl(&state, "ws_billing".into(), "req_src".into()).unwrap();
    let restored = cookie_jar_restore_many_impl(&state, vec![first]).unwrap();
    assert_eq!(restored, vec![first_id.clone()]);
    assert_eq!(
        cookie_jar_list_impl(&state, "ws_billing".into()).unwrap()[0].source_request_id,
        None
    );
    cookie_jar_delete_many_impl(&state, "ws_billing".into(), vec![first_id]).unwrap();

    let expired_entry = plain_cookie("ws_billing", "expired", None, Some("0"), "1700000000000");
    let active_ts = cookie_jar_timestamp();
    let active = plain_cookie("ws_billing", "session", None, Some("3600"), &active_ts);
    let expired_id = expired_entry.id.as_str().to_owned();
    {
        let store = state.store();
        store.save_cookie_jar_entry(&expired_entry).unwrap();
        store.save_cookie_jar_entry(&active).unwrap();
    }
    let pruned = cookie_jar_delete_expired_impl(&state, "ws_billing".into()).unwrap();
    assert_eq!(pruned, vec![expired_id]);
    let remaining = cookie_jar_list_impl(&state, "ws_billing".into()).unwrap();
    assert_eq!(remaining.len(), 1);
    assert_eq!(remaining[0].name, "session");
    assert!(cookie_jar_delete_expired_impl(&state, " ".into()).is_err());
}

#[test]
fn history_crud_paging_and_workspace_isolation_preserve_snapshots() {
    let state = state_with_workspaces(&["ws_billing", "ws_other"]);
    let request = Request::new(
        "req_history",
        "ws_billing",
        "List invoices",
        "https://api.example.com/invoices",
    )
    .unwrap();
    let fail_req = Request::new(
        "req_fail",
        "ws_billing",
        "Realtime stream",
        "https://api.example.com/stream",
    )
    .unwrap();
    let first = success_history(
        "hist_first",
        &request,
        200,
        "https://api.example.com/invoices",
        "1700000000000",
    )
    .with_request_snapshot(&request);
    let second = success_history(
        "hist_second",
        &request,
        201,
        "https://api.example.com/invoices",
        "1700000002000",
    );
    let failure = ExecutionHistoryEntry::from_failure(
        "hist_fail",
        &fail_req,
        None,
        "Network",
        "network request failed: connection refused",
        10,
        3,
        "1700000001000",
    )
    .unwrap();
    {
        let store = state.store();
        store.save_request(&request).unwrap();
        store.save_request(&fail_req).unwrap();
        for entry in [&first, &second, &failure] {
            store.save_execution_history(entry).unwrap();
        }
    }

    let page = |text: &str, include_failed, limit, cursor| {
        history_page_impl(
            &state,
            "ws_billing".into(),
            HistoryFilter {
                query: text.into(),
                include_failed,
                ..HistoryFilter::default()
            },
            limit,
            cursor,
        )
        .unwrap()
    };
    let p1 = page("invoice", false, Some(1), None);
    let p2 = page("invoice", false, Some(1), p1.next_cursor.clone());
    let pf = page("connection refused", true, Some(10), None);
    assert_eq!(p1.entries[0].id.as_str(), "hist_second");
    assert!(p1.next_cursor.is_some());
    assert_eq!(p2.entries[0].id.as_str(), "hist_first");
    assert!(p2.next_cursor.is_none());
    assert_eq!(pf.entries[0].id.as_str(), "hist_fail");

    assert!(matches!(
        history_get_impl(&state, "ws_other".into(), "hist_first".into()),
        Err(AppError::NotFound { .. })
    ));
    assert!(
        history_delete_many_impl(
            &state,
            "ws_other".into(),
            vec!["hist_first".into(), "hist_second".into()],
        )
        .unwrap()
        .is_empty()
    );
    assert_eq!(
        history_list(&state, "ws_billing".into(), None, None)
            .unwrap()
            .len(),
        3
    );

    let loaded = history_get_impl(&state, "ws_billing".into(), "hist_first".into()).unwrap();
    assert_eq!(loaded.request_snapshot, Some(request));
    assert!(matches!(
        history_get_impl(&state, "ws_billing".into(), "hist_missing".into()),
        Err(AppError::NotFound { .. })
    ));
    assert!(history_get_impl(&state, "ws_billing".into(), " ".into()).is_err());

    history_delete_many_impl(&state, "ws_billing".into(), vec!["hist_fail".into()]).unwrap();

    assert!(
        history_delete_many_impl(
            &state,
            "ws_billing".into(),
            vec!["hist_first".into(), " ".into()],
        )
        .is_err()
    );
    assert_eq!(
        history_list(&state, "ws_billing".into(), None, None)
            .unwrap()
            .len(),
        2
    );
    let deleted = history_delete_many_impl(
        &state,
        "ws_billing".into(),
        vec![
            "hist_first".into(),
            "hist_second".into(),
            "hist_missing".into(),
        ],
    )
    .unwrap();
    assert_eq!(
        deleted,
        vec!["hist_first".to_string(), "hist_second".to_string()]
    );
    assert!(
        history_list(&state, "ws_billing".into(), None, None)
            .unwrap()
            .is_empty()
    );
    let restored = history_restore_many_impl(&state, vec![second, first]).unwrap();
    assert_eq!(
        restored,
        vec!["hist_second".to_string(), "hist_first".to_string()]
    );
    let restored_rows = history_list(&state, "ws_billing".into(), None, None).unwrap();
    assert_eq!(
        restored_rows
            .iter()
            .map(|entry| entry.id.as_str())
            .collect::<Vec<_>>(),
        ["hist_second", "hist_first"]
    );
    let cleared = history_clear_impl(&state, "ws_billing".into()).unwrap();
    assert_eq!(
        cleared,
        vec!["hist_first".to_string(), "hist_second".to_string()]
    );
    assert!(
        history_list(&state, "ws_billing".into(), None, None)
            .unwrap()
            .is_empty()
    );
    assert!(history_clear_impl(&state, " ".into()).is_err());
}

#[test]
fn history_restore_rebounds_hostile_payloads_and_prunes_to_retention() {
    let state = state_with_workspaces(&["ws_restore_cap"]);

    state.store().set_history_retention(3);
    let cap_req =
        Request::new("req_cap", "ws_restore_cap", "Cap", "https://api.example/c").unwrap();
    let make_cap = |n: u32| {
        let mut entry = success_history(
            &format!("hist_restore_cap_{n}"),
            &cap_req,
            200,
            "https://api.example/c",
            &format!("17000000000{n:02}"),
        );
        entry.response_body_preview = "é".repeat(HISTORY_BODY_PREVIEW_LIMIT_BYTES);
        entry.response_headers = (0..2000)
            .map(|i| KeyValue {
                key: format!("x-h{i}"),
                value: "v".repeat(64),
                enabled: true,
                description: None,
            })
            .chain(std::iter::once(KeyValue {
                key: "Authorization".into(),
                value: "Bearer secret-token".into(),
                enabled: true,
                description: None,
            }))
            .collect();
        entry
    };
    let restored_cap = history_restore_many_impl(&state, (0..5).map(make_cap).collect()).unwrap();
    assert_eq!(
        restored_cap,
        [
            "hist_restore_cap_2",
            "hist_restore_cap_3",
            "hist_restore_cap_4"
        ]
    );
    let store = state.store();
    let rows = store.list_execution_history("ws_restore_cap", 20).unwrap();
    assert_eq!(rows.len(), 3);
    let full = store
        .get_execution_history("ws_restore_cap", &rows[0].id)
        .unwrap()
        .unwrap();
    assert!(
        full.response_body_preview.len() <= HISTORY_BODY_PREVIEW_LIMIT_BYTES
            && full.response_body_truncated
            && full.response_headers.len() < 2000
            && full.response_headers_truncated
    );
}

/// `count` rows of `ws` with a request snapshot each, saved through the store.
fn seed_rows(state: &AppState, ws: &str, count: usize) -> Vec<String> {
    let request = Request::new(format!("req_{ws}"), ws, "R", "https://api.example.com").unwrap();
    let store = state.store();
    store.save_request(&request).unwrap();
    (0..count)
        .map(|i| {
            let id = format!("h_{ws}_{i:04}");
            let at = format!("{}", 1_700_000_000_000_u64 + i as u64);
            let entry = success_history(&id, &request, 200, "https://api.example.com", &at)
                .with_request_snapshot(&request);
            store.save_execution_history(&entry).unwrap();
            id
        })
        .collect()
}

#[test]
fn history_delete_undo_restores_full_rows_and_rejects_foreign_expired_or_forged_tokens() {
    use crate::app::misc_logic_service::{
        history_entries_delete_at, history_entries_delete_impl, history_entries_undo_at,
        history_entries_undo_impl,
    };
    use crate::app::undo_store::UndoToken;
    use std::time::{Duration, Instant};

    let state = state_with_workspaces(&["ws_a", "ws_b"]);
    let ids = seed_rows(&state, "ws_a", 3);
    let full = history_get_impl(&state, "ws_a".into(), ids[0].clone()).unwrap();

    let removed = history_entries_delete_impl(&state, "ws_a".into(), ids.clone()).unwrap();
    assert_eq!(removed.removed.len(), 3);
    let token = removed
        .undo_token
        .clone()
        .expect("a small delete gets a token");
    assert_eq!(
        serde_json::to_value(&removed).unwrap()["undo_token"],
        serde_json::json!(token.0)
    );
    // Another workspace cannot use the token; a made-up token restores nothing.
    for (ws, t) in [
        ("ws_b", token.clone()),
        ("ws_a", UndoToken("undo_forged".into())),
    ] {
        let err = history_entries_undo_impl(&state, ws.into(), t).unwrap_err();
        assert!(matches!(err, AppError::NotFound { .. }), "{err:?}");
    }
    let mut restored = history_entries_undo_impl(&state, "ws_a".into(), token.clone()).unwrap();
    restored.sort();
    assert_eq!(restored, ids);
    assert_eq!(
        history_get_impl(&state, "ws_a".into(), ids[0].clone()).unwrap(),
        full,
        "the heavy parts come back"
    );
    assert!(history_entries_undo_impl(&state, "ws_a".into(), token).is_err());

    // A token expires after 60 s.
    let t0 = Instant::now();
    let held = history_entries_delete_at(&state, "ws_a", &ids, t0)
        .unwrap()
        .undo_token
        .unwrap();
    let late = history_entries_undo_at(&state, "ws_a", &held, t0 + Duration::from_secs(60));
    assert!(late.is_err());
}

#[test]
fn a_delete_of_more_than_200_rows_gets_no_token_and_prune_preview_counts_per_workspace_or_all() {
    use crate::app::misc_logic_service::{
        HISTORY_UNDO_MAX_ROWS, history_entries_delete_impl, history_prune_preview_impl,
    };

    let state = state_with_workspaces(&["ws_a", "ws_b"]);
    let ids = seed_rows(&state, "ws_a", HISTORY_UNDO_MAX_ROWS + 1);
    seed_rows(&state, "ws_b", 30);
    let preview = |ws: Option<&str>, keep| {
        history_prune_preview_impl(&state, ws.map(str::to_owned), keep).unwrap()
    };
    assert_eq!(preview(Some("ws_a"), 10), 191);
    assert_eq!(preview(Some("ws_b"), 10), 20);
    assert_eq!(preview(None, 10), 211);
    assert_eq!(preview(None, 2000), 0);

    let removed = history_entries_delete_impl(&state, "ws_a".into(), ids).unwrap();
    assert_eq!(removed.removed.len(), HISTORY_UNDO_MAX_ROWS + 1);
    assert_eq!(removed.undo_token, None);
    assert!(
        serde_json::to_value(&removed)
            .unwrap()
            .get("undo_token")
            .is_none()
    );
}

#[test]
fn history_export_har_takes_only_the_filtered_rows_masks_credentials_and_signs_the_entries() {
    use crate::app::{HistoryExportScope, history_export_har_impl};
    use crate::workspace::HttpMethod;

    let state = state_with_workspaces(&["ws_har"]);
    let mut post = Request::new(
        "req_post",
        "ws_har",
        "Create",
        "https://alice:pw-1@api.test/v1?key=k-123&page=2",
    )
    .unwrap();
    post.method = HttpMethod::Post;
    post.headers.push(KeyValue {
        key: "Authorization".into(),
        value: "Bearer tok-abcdefgh".into(),
        enabled: true,
        description: None,
    });
    let get = Request::new("req_get", "ws_har", "List", "https://api.test/v1").unwrap();
    {
        let store = state.store();
        store.save_request(&post).unwrap();
        store.save_request(&get).unwrap();
        for (id, request, at) in [
            ("h_post", &post, "1700000000002"),
            ("h_get", &get, "1700000000001"),
        ] {
            let entry =
                success_history(id, request, 200, &request.url, at).with_request_snapshot(request);
            store.save_execution_history(&entry).unwrap();
        }
    }
    let filter = HistoryFilter {
        methods: vec![HttpMethod::Post],
        ..HistoryFilter::default()
    };
    let (json, count) =
        history_export_har_impl(&state, "ws_har".into(), HistoryExportScope::Filter(filter))
            .unwrap();
    let har: serde_json::Value = serde_json::from_str(&json).unwrap();
    let entries = har["log"]["entries"].as_array().unwrap();

    assert_eq!((count, entries.len()), (1, 1));
    assert_eq!(entries[0]["_pingdeck"]["id"], "h_post");
    assert!(
        !json.contains("pw-1") && !json.contains("k-123") && !json.contains("tok-abcdefgh"),
        "{json}"
    );
    assert!(
        entries[0]["request"]["url"]
            .as_str()
            .unwrap()
            .contains("page=2")
    );
    let manifest = &har["log"]["_pingdeck"]["manifest"];
    assert_eq!(manifest["entries"], 1);
    assert_eq!(
        manifest["sha256"],
        pingdeck_base::crypto::sha256_hex(&serde_json::to_vec(entries).unwrap()).as_str()
    );
}
