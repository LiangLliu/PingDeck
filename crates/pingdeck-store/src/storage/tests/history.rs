//! Storage tests for execution history.

use pingdeck_model::{
    Collection, CollectionId, Environment, EnvironmentId, ExecutionHistoryEntry,
    ExecutionHistoryResultKind, Folder, FolderId, HISTORY_BODY_PREVIEW_LIMIT_BYTES,
    HistoryCheckSummary, HistoryEntryId, HistoryFilter, HistoryOrigin, HistoryOriginRef,
    HistorySource, HttpMethod, KeyValue, Request, RequestBody, Workspace,
};

use super::super::WorkspaceStore;

fn ok_entry(
    id: &str,
    request: &Request,
    status: u16,
    attempts: u32,
    at: &str,
) -> ExecutionHistoryEntry {
    ExecutionHistoryEntry::from_success(
        id,
        request,
        None,
        status,
        "OK",
        "https://api.example.com/ok",
        12,
        "{}",
        2,
        attempts,
        at,
    )
    .unwrap()
}

#[test]
fn execution_history_round_trips_snapshots_bounded_bodies_failures_and_attempts() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let workspace = Workspace::new("ws_billing", "Billing APIs").unwrap();
    let env = Environment::new("env_local", workspace.id.as_str(), "Local").unwrap();
    let mut request = Request::new(
        "req_snapshot",
        workspace.id.as_str(),
        "Snapshot request",
        "{{baseUrl}}/users/:id",
    )
    .unwrap();
    request.method = HttpMethod::Post;
    request.query_params = vec![KeyValue {
        key: "page".into(),
        value: "2".into(),
        enabled: true,
        description: None,
    }];
    request.headers = vec![KeyValue {
        key: "accept".into(),
        value: "application/json".into(),
        enabled: true,
        description: None,
    }];
    request.body = RequestBody::Json {
        content: r#"{"name":"snapshot"}"#.into(),
    };
    store.save_workspace(&workspace).unwrap();
    store.save_environment(&env).unwrap();
    store.save_request(&request).unwrap();

    let long_body = format!("{}尾", "a".repeat(HISTORY_BODY_PREVIEW_LIMIT_BYTES + 8));
    let success = ExecutionHistoryEntry::from_success(
        "hist_success",
        &request,
        Some("env_local"),
        201,
        "Created",
        "https://api.example.com/users/42",
        42,
        &long_body,
        long_body.len(),
        3,
        "1700000000000",
    )
    .unwrap()
    .with_request_snapshot(&request);
    store.save_execution_history(&success).unwrap();

    let failure = ExecutionHistoryEntry::from_failure(
        "hist_failure",
        &request,
        None,
        "Network",
        "network request failed: connection reset",
        17,
        1,
        "1700000000001",
    )
    .unwrap();
    store.save_execution_history(&failure).unwrap();

    let stored = store
        .get_execution_history(
            workspace.id.as_str(),
            &HistoryEntryId::new("hist_success").unwrap(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(stored.request_snapshot, Some(request.clone()));
    assert_eq!(stored.attempt_count, 3);
    assert!(stored.response_body_truncated);
    assert!(stored.response_body_preview.len() <= HISTORY_BODY_PREVIEW_LIMIT_BYTES);
    assert!(std::str::from_utf8(stored.response_body_preview.as_bytes()).is_ok());

    let listed = store
        .list_execution_history(workspace.id.as_str(), 10)
        .unwrap();
    assert_eq!(listed.len(), 2);
    assert_eq!(listed[0].result_kind, ExecutionHistoryResultKind::Failure);
    assert_eq!(listed[0].error_kind.as_deref(), Some("Network"));
    assert!(listed[1].request_snapshot.is_none());

    // Deleting request and environment nulls foreign keys without deleting history
    assert!(store.delete_request(&workspace.id, &request.id).unwrap());
    assert!(store.delete_environment(&workspace.id, &env.id).unwrap());
    let after = store
        .list_execution_history(workspace.id.as_str(), 10)
        .unwrap();
    assert_eq!(
        (after[1].request_id.clone(), after[1].environment_id.clone()),
        (None, None)
    );
}

#[test]
fn execution_history_paging_search_delete_restore_and_clear() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let ws = Workspace::new("ws_main", "Main").unwrap();
    let other_ws = Workspace::new("ws_other", "Other").unwrap();
    let req = Request::new(
        "req_main",
        ws.id.as_str(),
        "List invoices",
        "https://api.example.com/invoices",
    )
    .unwrap();
    let fail_req = Request::new(
        "req_fail",
        ws.id.as_str(),
        "Stream",
        "https://api.example.com/stream",
    )
    .unwrap();
    let other_req = Request::new(
        "req_other",
        other_ws.id.as_str(),
        "Other invoices",
        "https://api.example.com/invoices",
    )
    .unwrap();
    store.save_workspace(&ws).unwrap();
    store.save_workspace(&other_ws).unwrap();
    store.save_request(&req).unwrap();
    store.save_request(&fail_req).unwrap();
    store.save_request(&other_req).unwrap();

    let h1 = ok_entry("h1", &req, 200, 2, "1700000000000");
    let h2 = ExecutionHistoryEntry::from_failure(
        "h2",
        &fail_req,
        None,
        "Network",
        "connection refused",
        12,
        3,
        "1700000001000",
    )
    .unwrap();
    let h3 = ok_entry("h3", &req, 201, 1, "1700000002000");
    let h_other = ok_entry("h_other", &other_req, 200, 1, "1700000003000");
    for entry in [&h1, &h2, &h3, &h_other] {
        store.save_execution_history(entry).unwrap();
    }

    let text = |query: &str| HistoryFilter {
        query: query.to_owned(),
        ..HistoryFilter::default()
    };
    let p1 = store
        .query_execution_history(ws.id.as_str(), &text("invoice"), 1, None)
        .unwrap();
    assert_eq!((p1.entries[0].id.as_str(), p1.total), ("h3", Some(2)));
    // Late row while paging does not shift cursor
    store
        .save_execution_history(&ok_entry("h_late", &req, 200, 1, "1700000009000"))
        .unwrap();
    let p2 = store
        .query_execution_history(ws.id.as_str(), &text("invoice"), 1, p1.next_cursor.as_ref())
        .unwrap();
    assert_eq!(
        (p2.entries[0].id.as_str(), p2.total, p2.next_cursor),
        ("h1", None, None)
    );

    let fail_match = store
        .query_execution_history(
            ws.id.as_str(),
            &HistoryFilter {
                include_failed: true,
                ..text("connection refused")
            },
            10,
            None,
        )
        .unwrap();
    assert_eq!(fail_match.entries[0].id.as_str(), "h2");
    assert_eq!(
        store
            .query_execution_history(ws.id.as_str(), &text("2 attempts"), 10, None)
            .unwrap()
            .entries[0]
            .id
            .as_str(),
        "h1"
    );
    assert!(
        store
            .query_execution_history(ws.id.as_str(), &text("%"), 10, None)
            .unwrap()
            .entries
            .is_empty()
    );

    // Cross-workspace isolation and delete/restore
    let h1_id = h1.id.clone();
    let h2_id = h2.id.clone();
    assert!(
        store
            .get_execution_history(other_ws.id.as_str(), &h1_id)
            .unwrap()
            .is_none()
    );
    assert!(
        !store
            .delete_execution_history_entry(other_ws.id.as_str(), &h1_id)
            .unwrap()
    );
    assert!(
        store
            .delete_execution_history_entries(other_ws.id.as_str(), std::slice::from_ref(&h1_id))
            .unwrap()
            .is_empty()
    );

    assert!(
        store
            .delete_execution_history_entry(ws.id.as_str(), &HistoryEntryId::new("h_late").unwrap())
            .unwrap()
    );
    assert!(
        !store
            .delete_execution_history_entry(ws.id.as_str(), &HistoryEntryId::new("h_late").unwrap())
            .unwrap()
    );

    let deleted = store
        .delete_execution_history_entries(
            ws.id.as_str(),
            &[
                h2_id.clone(),
                HistoryEntryId::new("missing").unwrap(),
                h1_id.clone(),
            ],
        )
        .unwrap();
    assert_eq!(deleted, vec![h2_id.clone(), h1_id.clone()]);

    let restored = store
        .restore_execution_history_entries(vec![h2.clone(), h1.clone()])
        .unwrap();
    assert_eq!(restored, vec![h2_id, h1_id]);
    assert!(
        store
            .restore_execution_history_entries(vec![h1])
            .unwrap()
            .is_empty()
    );
    let mut overwritten = h3.clone();
    overwritten.status_text = "Overwritten".into();
    assert!(
        store
            .restore_execution_history_entries(vec![overwritten])
            .unwrap()
            .is_empty()
    );
    assert_eq!(
        store
            .get_execution_history(ws.id.as_str(), &h3.id)
            .unwrap()
            .unwrap()
            .status_text,
        "OK"
    );

    // Clear all keeps other workspace; retention caps per workspace
    assert_eq!(
        store
            .delete_all_execution_history(ws.id.as_str())
            .unwrap()
            .len(),
        3
    );
    assert_eq!(
        store
            .list_execution_history(other_ws.id.as_str(), 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn retention_caps_each_workspace_on_insert() {
    let mut store = WorkspaceStore::open_in_memory().unwrap();
    let ws = Workspace::new("ws_main", "Main").unwrap();
    let other_ws = Workspace::new("ws_other", "Other").unwrap();
    let req = Request::new(
        "req_main",
        ws.id.as_str(),
        "List invoices",
        "https://api.example.com/invoices",
    )
    .unwrap();
    let other_req = Request::new(
        "req_other",
        other_ws.id.as_str(),
        "Other invoices",
        "https://api.example.com/invoices",
    )
    .unwrap();
    store.save_workspace(&ws).unwrap();
    store.save_workspace(&other_ws).unwrap();
    store.save_request(&req).unwrap();
    store.save_request(&other_req).unwrap();
    store
        .save_execution_history(&ok_entry("h_other", &other_req, 200, 1, "1700000003000"))
        .unwrap();

    store.history_retention = 3;
    let total = 6;
    for index in 0..total {
        store
            .save_execution_history(&ok_entry(
                &format!("ret_{index:05}"),
                &req,
                200,
                1,
                &format!("{:013}", 1_700_000_000_000_i64 + index as i64),
            ))
            .unwrap();
    }
    let count: i64 = store
        .connection
        .query_row(
            "SELECT COUNT(*) FROM execution_history WHERE workspace_id = ?1",
            [ws.id.as_str()],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(count, 3);
    assert!(
        store
            .get_execution_history(ws.id.as_str(), &HistoryEntryId::new("ret_00002").unwrap())
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_execution_history(ws.id.as_str(), &HistoryEntryId::new("ret_00003").unwrap())
            .unwrap()
            .is_some()
    );
    assert_eq!(
        store
            .list_execution_history(other_ws.id.as_str(), 10)
            .unwrap()
            .len(),
        1
    );
}

#[test]
fn prune_count_ignores_mock_rows_and_sums_workspaces_and_take_returns_full_rows() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let mut reqs = Vec::new();
    for ws in ["ws_a", "ws_b"] {
        store
            .save_workspace(&Workspace::new(ws, ws).unwrap())
            .unwrap();
        let req = Request::new(format!("req_{ws}"), ws, "R", "https://api.example.com").unwrap();
        store.save_request(&req).unwrap();
        reqs.push(req);
    }
    let mut rows = Vec::new();
    for i in 0..5 {
        rows.push(
            ok_entry(
                &format!("a{i}"),
                &reqs[0],
                200,
                1,
                &format!("17000000{i:05}"),
            )
            .with_request_snapshot(&reqs[0]),
        );
    }
    for i in 0..3 {
        rows.push(ok_entry(
            &format!("b{i}"),
            &reqs[1],
            200,
            1,
            &format!("17000000{i:05}"),
        ));
    }
    let mut mock = ok_entry("a_mock", &reqs[0], 200, 1, "1700000099999");
    mock.source = pingdeck_model::HistorySource::Mock;
    rows.push(mock);
    store.save_execution_history_batch(&rows).unwrap();

    assert_eq!(store.history_prune_count(Some("ws_a"), 2).unwrap(), 3);
    assert_eq!(store.history_prune_count(Some("ws_b"), 2).unwrap(), 1);
    assert_eq!(store.history_prune_count(None, 2).unwrap(), 4);
    assert_eq!(store.history_prune_count(None, 10).unwrap(), 0);

    let id = |s: &str| HistoryEntryId::new(s).unwrap();
    let taken = store
        .take_execution_history_entries("ws_a", &[id("a1"), id("b1"), id("nope")])
        .unwrap();
    assert_eq!(taken.len(), 1, "a foreign or unknown id is skipped");
    assert!(taken[0].request_snapshot.is_some());
    assert!(
        store
            .get_execution_history("ws_a", &id("a1"))
            .unwrap()
            .is_none()
    );
    assert!(
        store
            .get_execution_history("ws_b", &id("b1"))
            .unwrap()
            .is_some()
    );
}

#[test]
fn filter_combines_environments_methods_and_duration_and_search_reads_the_environment_name() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let ws = Workspace::new("ws_main", "Main").unwrap();
    store.save_workspace(&ws).unwrap();
    for (id, name) in [("env_staging", "Staging"), ("env_prod", "Production")] {
        store
            .save_environment(&Environment::new(id, ws.id.as_str(), name).unwrap())
            .unwrap();
    }
    let mut get = Request::new("req_get", ws.id.as_str(), "List", "https://a.test/l").unwrap();
    get.method = HttpMethod::Get;
    let mut post = Request::new("req_post", ws.id.as_str(), "Create", "https://a.test/c").unwrap();
    post.method = HttpMethod::Post;
    store.save_request(&get).unwrap();
    store.save_request(&post).unwrap();
    let row = |id: &str, request: &Request, env: (&str, &str), elapsed: u128, at: &str| {
        let mut entry = ok_entry(id, request, 200, 1, at);
        entry.environment_id = Some(EnvironmentId::new(env.0).unwrap());
        entry.environment_name = env.1.to_owned();
        entry.elapsed_ms = elapsed;
        entry
    };
    let staging = ("env_staging", "Staging");
    let prod = ("env_prod", "Production");
    for entry in [
        row("slow_post_staging", &post, staging, 1500, "1700000000001"),
        row("fast_post_staging", &post, staging, 200, "1700000000002"),
        row("slow_get_staging", &get, staging, 1500, "1700000000003"),
        row("slow_post_prod", &post, prod, 1500, "1700000000004"),
    ] {
        store.save_execution_history(&entry).unwrap();
    }
    let ids = |filter: HistoryFilter| -> Vec<String> {
        store
            .query_execution_history(ws.id.as_str(), &filter, 50, None)
            .unwrap()
            .entries
            .iter()
            .map(|e| e.id.as_str().to_owned())
            .collect()
    };

    let narrowed = HistoryFilter {
        environment_ids: vec!["env_staging".into()],
        methods: vec![HttpMethod::Post],
        min_elapsed_ms: Some(1000),
        ..HistoryFilter::default()
    };
    assert_eq!(ids(narrowed), ["slow_post_staging"]);
    let either_method = HistoryFilter {
        methods: vec![HttpMethod::Get, HttpMethod::Post],
        environment_ids: vec!["env_prod".into(), "env_staging".into()],
        request_id: Some("req_get".into()),
        ..HistoryFilter::default()
    };
    assert_eq!(ids(either_method), ["slow_get_staging"]);
    let by_env_name = HistoryFilter {
        query: "producti".into(),
        ..HistoryFilter::default()
    };
    assert_eq!(ids(by_env_name), ["slow_post_prod"]);
}

#[test]
fn workspace_total_tells_an_empty_range_from_an_empty_workspace() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let ws = Workspace::new("ws_main", "Main").unwrap();
    let empty = Workspace::new("ws_empty", "Empty").unwrap();
    store.save_workspace(&ws).unwrap();
    store.save_workspace(&empty).unwrap();
    let req = Request::new("req_a", ws.id.as_str(), "A", "https://a.test/").unwrap();
    store.save_request(&req).unwrap();
    for (id, at) in [("old_1", "1700000000000"), ("old_2", "1700000001000")] {
        store
            .save_execution_history(&ok_entry(id, &req, 200, 1, at))
            .unwrap();
    }
    let recent = HistoryFilter {
        since_ms: Some(1_800_000_000_000),
        ..HistoryFilter::default()
    };

    let page = store
        .query_execution_history(ws.id.as_str(), &recent, 1, None)
        .unwrap();
    assert_eq!((page.total, page.workspace_total), (Some(0), Some(2)));
    let all = store
        .query_execution_history(ws.id.as_str(), &HistoryFilter::default(), 1, None)
        .unwrap();
    assert_eq!((all.total, all.workspace_total), (Some(2), Some(2)));
    let next = store
        .query_execution_history(
            ws.id.as_str(),
            &HistoryFilter::default(),
            1,
            all.next_cursor.as_ref(),
        )
        .unwrap();
    assert_eq!(next.workspace_total, None);
    let none = store
        .query_execution_history(empty.id.as_str(), &recent, 1, None)
        .unwrap();
    assert_eq!(none.workspace_total, Some(0));
}

#[test]
fn a_200_flow_step_with_a_failed_check_is_failed_and_its_run_stays_under_the_failed_filter() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let ws = Workspace::new("ws_main", "Main").unwrap();
    store.save_workspace(&ws).unwrap();
    let req = Request::new("req_a", ws.id.as_str(), "Create", "https://a.test/c").unwrap();
    store.save_request(&req).unwrap();
    let origin = HistoryOrigin::run(HistorySource::Flow, "Checkout", "flow_1", "run_1");
    let checked = |id: &str, at: &str, passed: u32, failed: u32| {
        ok_entry(id, &req, 200, 1, at)
            .with_origin(&origin)
            .with_check_summary(Some(HistoryCheckSummary { passed, failed }))
    };
    for entry in [
        checked("step_1", "1700000000001", 2, 0),
        checked("step_2", "1700000000002", 0, 1),
        // A row without checks (a mock hit, say) stays NULL: unchecked, not "0 passed".
        ok_entry("plain", &req, 200, 1, "1700000000003"),
    ] {
        store.save_execution_history(&entry).unwrap();
    }

    let all = store
        .query_execution_history(ws.id.as_str(), &HistoryFilter::default(), 50, None)
        .unwrap();
    let row = |id: &str| all.entries.iter().find(|e| e.id.as_str() == id).unwrap();
    assert_eq!(
        row("step_2").check_summary,
        Some(HistoryCheckSummary {
            passed: 0,
            failed: 1
        })
    );
    assert_eq!(row("plain").check_summary, None);
    assert_eq!((all.runs[0].total, all.runs[0].failed), (2, 1));

    let failed = HistoryFilter {
        include_failed: true,
        ..HistoryFilter::default()
    };
    let page = store
        .query_execution_history(ws.id.as_str(), &failed, 50, None)
        .unwrap();
    assert_eq!(page.entries.len(), 1);
    assert_eq!(
        (
            page.runs[0].run_id.as_str(),
            page.runs[0].total,
            page.runs[0].failed
        ),
        ("run_1", 1, 1)
    );
    let children = store
        .query_run_children(ws.id.as_str(), "run_1", &failed, 50, None)
        .unwrap();
    assert_eq!(children.entries.len(), 1);
    assert_eq!(children.entries[0].id.as_str(), "step_2");
}

#[test]
fn agent_rows_read_their_chat_title_and_origin_refs_round_trip() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let workspace = Workspace::new("ws_origin", "Origin").unwrap();
    store.save_workspace(&workspace).unwrap();
    store
        .save_agent_session(&super::agent::session("chat_live", 1))
        .unwrap();
    store
        .save_agent_session(&super::agent::session("chat_gone", 1))
        .unwrap();
    assert!(store.soft_delete_session("chat_gone", 2).unwrap());
    for (id, at) in [("m_user", 3), ("m_reply", 4)] {
        let mut turn = super::agent::message("chat_live", id, "refund?", at);
        turn.run_id = Some("run_1".into());
        store.append_agent_message(&turn).unwrap();
    }
    let request = Request::new("req_o", "ws_origin", "Refund", "https://x.test/r").unwrap();
    let agent = |id: &str, chat: &str, at: &str| {
        ok_entry(id, &request, 200, 1, at).with_origin(&HistoryOrigin {
            source: HistorySource::Agent,
            source_name: "Agent chat".into(),
            source_id: chat.into(),
            run_id: Some("run_1".into()),
        })
    };
    let call = HistoryOriginRef::Call {
        id: "rq_00000000000000aa".into(),
    };
    let rows = [
        agent("h1", "chat_live", "1"),
        agent("h2", "chat_live", "2"),
        agent("h3", "chat_gone", "3"),
        ok_entry("h4", &request, 200, 1, "4").with_origin_ref(Some(call.clone())),
    ];
    let rows = rows.map(|mut row| {
        row.request_id = None; // the request itself is not stored here
        row
    });
    store.save_execution_history_batch(&rows).unwrap();

    let page = store
        .query_execution_history("ws_origin", &HistoryFilter::default(), 50, None)
        .unwrap();
    let by_id = |id: &str| page.entries.iter().find(|e| e.id.as_str() == id).unwrap();
    assert_eq!(
        by_id("h1").origin_label.as_deref(),
        Some("Session chat_live")
    );
    assert_eq!(
        by_id("h2").origin_label.as_deref(),
        Some("Session chat_live")
    );
    assert_eq!(
        by_id("h3").origin_label,
        None,
        "a deleted chat has no title"
    );
    assert_eq!(
        by_id("h1").origin_turn_id.as_deref(),
        Some("m_user"),
        "the run's first message is the turn to reveal"
    );
    assert_eq!(by_id("h3").origin_turn_id, None);
    assert_eq!(by_id("h4").origin_ref, Some(call));
    assert_eq!(by_id("h4").origin_label, None);

    let full = store
        .get_execution_history("ws_origin", &HistoryEntryId::new("h1").unwrap())
        .unwrap()
        .unwrap();
    assert_eq!(full.origin_label.as_deref(), Some("Session chat_live"));
}

#[test]
fn collection_path_walks_the_snapshot_folders_up_to_a_collection_of_the_workspace() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let ws = Workspace::new("ws_path", "Path").unwrap();
    let other = Workspace::new("ws_path_other", "Other").unwrap();
    store.save_workspace(&ws).unwrap();
    store.save_workspace(&other).unwrap();
    store
        .save_collection(&Collection::new("col_path", ws.id.as_str(), "Billing").unwrap())
        .unwrap();
    store
        .save_collection(&Collection::new("col_foreign", other.id.as_str(), "Foreign").unwrap())
        .unwrap();
    store
        .save_folder(&Folder::new("fold_outer", "col_path", "Outer").unwrap())
        .unwrap();
    store
        .save_folder(
            &Folder::new("fold_inner", "col_path", "Inner")
                .unwrap()
                .with_parent("fold_outer")
                .unwrap(),
        )
        .unwrap();
    let nested = Request::new("req_nested", ws.id.as_str(), "Nested", "https://a.test")
        .unwrap()
        .with_collection("col_path")
        .unwrap()
        .with_folder("fold_inner")
        .unwrap();
    store.save_request(&nested).unwrap();
    // A snapshot naming another workspace's collection resolves to nothing.
    let foreign = Request::new("req_foreign", ws.id.as_str(), "Foreign", "https://a.test")
        .unwrap()
        .with_collection("col_foreign")
        .unwrap();
    for entry in [
        ok_entry("hist_nested", &nested, 200, 1, "1700000000000").with_request_snapshot(&nested),
        ok_entry("hist_bare", &nested, 200, 1, "1700000000001"),
        ok_entry("hist_foreign", &foreign, 200, 1, "1700000000002").with_request_snapshot(&foreign),
    ] {
        let mut entry = entry;
        // `req_foreign` is never saved; history keeps no dangling request link.
        if entry.id.as_str() == "hist_foreign" {
            entry.request_id = None;
        }
        store.save_execution_history(&entry).unwrap();
    }
    let paths = |store: &WorkspaceStore| {
        let listed = store
            .query_execution_history(ws.id.as_str(), &HistoryFilter::default(), 10, None)
            .unwrap()
            .entries;
        let mut by_id: Vec<(String, Vec<String>)> = listed
            .into_iter()
            .map(|entry| (entry.id.as_str().to_owned(), entry.collection_path))
            .collect();
        by_id.sort();
        let got = store
            .get_execution_history(ws.id.as_str(), &HistoryEntryId::new("hist_nested").unwrap())
            .unwrap()
            .unwrap()
            .collection_path;
        (by_id, got)
    };
    let owned = |names: &[&str]| names.iter().map(|name| (*name).to_owned()).collect();

    let (listed, got) = paths(&store);
    assert_eq!(
        listed,
        vec![
            ("hist_bare".to_owned(), Vec::new()),
            ("hist_foreign".to_owned(), Vec::new()),
            (
                "hist_nested".to_owned(),
                owned(&["Billing", "Outer", "Inner"])
            ),
        ]
    );
    assert_eq!(got, owned(&["Billing", "Outer", "Inner"]));

    // A gone folder (the inner one cascades with its parent) ends the chain at the collection.
    assert!(
        store
            .delete_folder(&ws.id, &FolderId::new("fold_outer").unwrap())
            .unwrap()
    );
    let (listed, got) = paths(&store);
    assert_eq!(listed[2].1, owned(&["Billing"]));
    assert_eq!(got, owned(&["Billing"]));

    // A gone collection leaves the path empty.
    assert!(
        store
            .delete_collection(&ws.id, &CollectionId::new("col_path").unwrap())
            .unwrap()
    );
    let (listed, got) = paths(&store);
    assert!(listed.iter().all(|(_, path)| path.is_empty()));
    assert!(got.is_empty());
}

fn store_with_request(ws: &str) -> (WorkspaceStore, Request) {
    let store = WorkspaceStore::open_in_memory().unwrap();
    store
        .save_workspace(&Workspace::new(ws, ws).unwrap())
        .unwrap();
    let request = Request::new(
        format!("req_{ws}"),
        ws,
        "Invoices",
        "https://api.example.com/x",
    )
    .unwrap();
    store.save_request(&request).unwrap();
    (store, request)
}

fn ids_of(store: &WorkspaceStore, ws: &str, source: HistorySource) -> Vec<String> {
    let filter = HistoryFilter {
        sources: vec![source],
        ..HistoryFilter::default()
    };
    store
        .query_execution_history(ws, &filter, 500, None)
        .unwrap()
        .entries
        .into_iter()
        .map(|entry| entry.id.as_str().to_owned())
        .collect()
}

#[test]
fn each_source_keeps_its_own_budget_so_a_run_never_flushes_manual_sends() {
    let (mut store, request) = store_with_request("ws_b");
    store.set_history_retention(3);
    for i in 0..3 {
        store
            .save_execution_history(&ok_entry(
                &format!("manual{i}"),
                &request,
                200,
                1,
                &format!("17000000{i:05}"),
            ))
            .unwrap();
    }
    // One collection run of 10 rows, far over the budget, after the manual sends.
    let run: Vec<_> = (0..10)
        .map(|i| {
            let mut entry = ok_entry(
                &format!("run_a_{i:02}"),
                &request,
                200,
                1,
                &format!("17000001{i:05}"),
            );
            entry.source = HistorySource::Runner;
            entry.run_id = Some("run_a".into());
            entry
        })
        .collect();
    for entry in &run {
        store.save_execution_history(entry).unwrap();
    }
    assert_eq!(ids_of(&store, "ws_b", HistorySource::Request).len(), 3);
    // The newest run stays whole even over the budget.
    assert_eq!(ids_of(&store, "ws_b", HistorySource::Runner).len(), 10);

    // A second, small run evicts the first one as a whole, never a part of it.
    for i in 0..2 {
        let mut entry = ok_entry(
            &format!("run_b_{i}"),
            &request,
            200,
            1,
            &format!("17000002{i:05}"),
        );
        entry.source = HistorySource::Runner;
        entry.run_id = Some("run_b".into());
        store.save_execution_history(&entry).unwrap();
    }
    assert_eq!(
        ids_of(&store, "ws_b", HistorySource::Runner),
        ["run_b_1", "run_b_0"]
    );
    assert_eq!(ids_of(&store, "ws_b", HistorySource::Request).len(), 3);

    // The preview counts the same way: a budget of 1 row keeps the newest run (2 rows) and
    // 1 manual send.
    assert_eq!(store.history_prune_count(Some("ws_b"), 1).unwrap(), 2);
    assert_eq!(
        store.history_capacity(),
        3 * 4 + super::super::HISTORY_MOCK_RETENTION
    );
}

#[test]
fn search_folds_unicode_case_and_reads_bodies_and_headers() {
    let (store, request) = store_with_request("ws_s");
    let mut entry = ok_entry("h_body", &request, 200, 1, "1700000000000");
    entry.request_name = "Équipe".into();
    entry.response_body_preview = r#"{"customer":"ÉLAN Payments"}"#.into();
    entry.response_headers = vec![KeyValue {
        key: "X-Trace-Id".into(),
        value: "trace-7781".into(),
        enabled: true,
        description: None,
    }];
    store.save_execution_history(&entry).unwrap();
    store
        .save_execution_history(&ok_entry("h_other", &request, 200, 1, "1700000001000"))
        .unwrap();
    let hits = |query: &str| -> Vec<String> {
        store
            .query_execution_history(
                "ws_s",
                &HistoryFilter {
                    query: query.to_owned(),
                    ..HistoryFilter::default()
                },
                10,
                None,
            )
            .unwrap()
            .entries
            .into_iter()
            .map(|entry| entry.id.as_str().to_owned())
            .collect()
    };
    assert_eq!(hits("équipe"), ["h_body"], "a non-ASCII capital folds");
    assert_eq!(
        hits("ÉLAN"),
        ["h_body"],
        "an exact-case non-ASCII query matches a payload"
    );
    assert_eq!(
        hits("PAYMENTS"),
        ["h_body"],
        "a body matches ASCII case-insensitively"
    );
    assert_eq!(
        hits("trace-7781"),
        ["h_body"],
        "a response header value matches"
    );
    assert_eq!(
        hits("100%"),
        Vec::<String>::new(),
        "LIKE wildcards are literal"
    );
}

#[test]
fn scrubbing_a_value_rewrites_snapshots_sent_requests_and_urls_but_not_short_values() {
    let (store, mut request) = store_with_request("ws_x");
    let clean = request.clone();
    request.url = "https://api.example.com/x?key=sk-live-123456".into();
    request.headers = vec![KeyValue {
        key: "Authorization".into(),
        value: "Bearer sk-live-123456".into(),
        enabled: true,
        description: None,
    }];
    let mut entry =
        ok_entry("h_secret", &request, 200, 1, "1700000000000").with_request_snapshot(&request);
    entry.request_url = request.url.clone();
    store.save_execution_history(&entry).unwrap();
    let untouched =
        ok_entry("h_plain", &clean, 200, 1, "1700000001000").with_request_snapshot(&clean);
    store.save_execution_history(&untouched).unwrap();

    assert_eq!(
        store.scrub_history_value("ws_x", "short", "{{k}}").unwrap(),
        0
    );
    assert_eq!(
        store
            .scrub_history_value("ws_x", "sk-live-123456", "{{api_key}}")
            .unwrap(),
        1
    );
    let row = store
        .get_execution_history("ws_x", &HistoryEntryId::new("h_secret").unwrap())
        .unwrap()
        .unwrap();
    let snapshot = row.request_snapshot.unwrap();
    assert_eq!(snapshot.headers[0].value, "Bearer {{api_key}}");
    assert_eq!(snapshot.url, "https://api.example.com/x?key={{api_key}}");
    assert_eq!(row.request_url, "https://api.example.com/x?key={{api_key}}");
    // Another workspace's rows are never touched.
    assert_eq!(
        store
            .scrub_history_value("ws_other", "{{api_key}}", "x")
            .unwrap(),
        0
    );

    // A column that does not parse is scrubbed in its raw text instead of failing the scrub.
    store
        .connection()
        .execute(
            "UPDATE execution_history SET sent_request_json = ?1 WHERE id = 'h_plain'",
            ["{\"url\": \"https://x.test/?t=tok-abcdef-99\" broken"],
        )
        .unwrap();
    assert_eq!(
        store
            .scrub_history_value("ws_x", "tok-abcdef-99", "{{tok}}")
            .unwrap(),
        1
    );
    let raw: String = store
        .connection()
        .query_row(
            "SELECT sent_request_json FROM execution_history WHERE id = 'h_plain'",
            [],
            |row| row.get(0),
        )
        .unwrap();
    assert_eq!(raw, "{\"url\": \"https://x.test/?t={{tok}}\" broken");
}
