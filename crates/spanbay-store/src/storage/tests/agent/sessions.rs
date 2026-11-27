//! Session rows: round trip, what a settings save leaves alone, Plan mode, the tombstone and
//! purge, Full access at start-up and the one-time notice, the filtered, paged list, the stop
//! reason of a message and the SpanBay binding.

use super::*;
use crate::storage::{SpanBayBinding, StorageError};
use spanbay_model::agent::{AgentStopReason, AgentTodoItem, AgentTodoStatus, ReasoningEffort};

#[test]
fn sessions_round_trip_preserve_runtime_columns_and_validate_modes() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    store.save_agent_session(&session("parent", 1)).unwrap();
    let mut saved = session("s1", 10);
    saved.workspace_dir = Some("/work/dir".to_owned());
    saved.permission_mode = ToolApproval::AutoAll;
    saved.reasoning_effort = Some(ReasoningEffort::High);
    saved.disabled_tools = vec!["shell".to_owned()];
    saved.disabled_skills = vec!["pdf".to_owned()];
    saved.web_enabled = true;
    saved.title_locked = true;
    saved.pinned_at_ms = Some(11);
    saved.archived_at_ms = Some(12);
    saved.forked_from = Some("origin".to_owned());
    saved.forked_from_message_id = Some("m7".to_owned());
    saved.parent_session_id = Some("parent".to_owned());
    saved.parent_run_id = Some("r1".to_owned());
    saved.todos = vec![AgentTodoItem {
        id: "t1".to_owned(),
        content: "write".to_owned(),
        status: AgentTodoStatus::InProgress,
        ..AgentTodoItem::default()
    }];
    saved.total_usage = HubUsage {
        input_tokens: Some(5),
        ..HubUsage::default()
    };
    saved.total_cost_micro_usd = 99;
    saved.last_context_tokens = 1234;
    saved.instructions = Some("Answer in French.".to_owned());
    store.save_agent_session(&saved).unwrap();
    assert_eq!(store.get_agent_session("s1").unwrap(), Some(saved.clone()));
    assert_eq!(store.get_agent_session("nope").unwrap(), None);

    // Todos and usage rollup update without overwriting conversation timestamp or wrapping
    let todos = [AgentTodoItem {
        id: "t2".to_owned(),
        content: "done".to_owned(),
        status: AgentTodoStatus::Completed,
        ..AgentTodoItem::default()
    }];
    assert!(store.save_session_todos("s1", &todos).unwrap());
    assert!(!store.save_session_todos("ghost", &todos).unwrap());
    assert!(
        store
            .add_session_usage(
                "s1",
                &HubUsage {
                    input_tokens: Some(u64::MAX),
                    output_tokens: Some(3),
                    ..HubUsage::default()
                },
                i64::MAX,
                50,
            )
            .unwrap()
    );
    assert!(
        !store
            .add_session_usage("ghost", &HubUsage::default(), 1, 1)
            .unwrap()
    );
    store
        .append_agent_message(&message("s1", "m1", "hello", 5))
        .unwrap();
    assert!(store.soft_delete_session("s1", 77).unwrap());

    // Settings save preserves todos, usage, head, lineage, creation time, and tombstone
    let mut renamed = saved;
    renamed.title = "Renamed".to_owned();
    renamed.target = None;
    renamed.workspace_dir = None;
    renamed.permission_mode = ToolApproval::Ask;
    renamed.reasoning_effort = None;
    renamed.disabled_tools.clear();
    renamed.web_enabled = false;
    renamed.pinned_at_ms = None;
    renamed.archived_at_ms = None;
    renamed.instructions = None;
    renamed.forked_from = Some("elsewhere".to_owned());
    renamed.head_message_id = Some("ghost".to_owned());
    renamed.deleted_at_ms = None;
    renamed.created_at_ms = 999;
    renamed.updated_at_ms = 20;
    store.save_agent_session(&renamed).unwrap();

    let loaded = store.get_agent_session("s1").unwrap().unwrap();
    assert_eq!(
        (
            loaded.title.as_str(),
            loaded.created_at_ms,
            loaded.instructions
        ),
        ("Renamed", 1, None)
    );
    assert_eq!(loaded.todos, todos);
    assert_eq!(
        (
            loaded.total_usage.input_tokens,
            loaded.total_usage.output_tokens
        ),
        (Some(u64::MAX), Some(3))
    );
    assert_eq!(
        (loaded.total_cost_micro_usd, loaded.last_context_tokens),
        (i64::MAX, 50)
    );
    assert_eq!(loaded.forked_from.as_deref(), Some("origin"));
    assert_eq!(loaded.head_message_id.as_deref(), Some("m1"));
    assert_eq!(loaded.deleted_at_ms, Some(77));

    store
        .connection
        .execute(
            "UPDATE agent_sessions SET permission_mode = 'yolo' WHERE id = 's1'",
            [],
        )
        .unwrap();
    assert!(matches!(
        store.get_agent_session("s1"),
        Err(StorageError::InvalidStoredValue {
            column: "permission_mode",
            ..
        })
    ));
}

#[test]
fn plan_mode_survives_a_session_save_and_a_fork_starts_outside_it() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    store.save_agent_session(&session("s1", 1)).unwrap();
    assert!(store.set_session_plan_mode("s1", true).unwrap());
    assert!(!store.set_session_plan_mode("missing", true).unwrap());
    // Saving the row again (a title or mode change) keeps the switch.
    store.save_agent_session(&session("s1", 2)).unwrap();
    let mut fork = session("s2", 3);
    fork.forked_from = Some("s1".to_owned());
    store.save_agent_session(&fork).unwrap();
    assert_eq!(store.plan_mode_sessions().unwrap(), ["s1"]);
    store.soft_delete_session("s1", 4).unwrap();
    assert!(store.plan_mode_sessions().unwrap().is_empty());
    store.restore_session("s1").unwrap();
    store.set_session_plan_mode("s1", false).unwrap();
    assert!(store.plan_mode_sessions().unwrap().is_empty());
}

#[test]
fn session_tombstone_restore_and_purge_cascades() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    store.save_agent_session(&session("parent", 3)).unwrap();
    let mut child = session("child", 2);
    child.parent_session_id = Some("parent".to_owned());
    store.save_agent_session(&child).unwrap();
    let mut earlier = session("earlier", 1);
    earlier.parent_session_id = Some("parent".to_owned());
    store.save_agent_session(&earlier).unwrap();
    assert!(store.soft_delete_session("earlier", 50).unwrap());
    store.save_agent_session(&session("other", 1)).unwrap();

    assert!(store.soft_delete_session("parent", 100).unwrap());
    assert!(!store.soft_delete_session("parent", 200).unwrap());
    assert!(!store.soft_delete_session("ghost", 200).unwrap());
    let get_del = |id: &str| store.get_agent_session(id).unwrap().unwrap().deleted_at_ms;
    assert_eq!(
        (get_del("parent"), get_del("child"), get_del("earlier")),
        (Some(100), Some(100), Some(50))
    );

    assert!(store.restore_session("parent").unwrap());
    assert!(!store.restore_session("parent").unwrap());
    assert_eq!(
        (get_del("parent"), get_del("child"), get_del("earlier")),
        (None, None, Some(50))
    );

    // Populate child tables on "other" and verify cascade on purge
    add_attachment(&store, "att", "pic.png");
    store
        .append_agent_message(&with_attachment(
            message("other", "m-img", "words other", 2),
            "att",
        ))
        .unwrap();
    store
        .insert_agent_run(&run("r-o", "other", AgentRunStatus::Completed, 1))
        .unwrap();
    store
        .save_permission_rule(&PermissionRule {
            session_id: Some("other".to_owned()),
            arg_pattern: Some("rust*".to_owned()),
            ..rule("p-o", "web_search", PermissionAction::Allow)
        })
        .unwrap();
    store
        .add_queued_turn("other", &vec![HubPart::text("later")].into())
        .unwrap()
        .unwrap();
    store
        .upsert_tool_call(&tool_call("other", "c1", "r-o"))
        .unwrap();
    store
        .insert_file_change(&change("other", "fc-o", "r-o", FileChangeStatus::Pending))
        .unwrap();

    assert!(store.soft_delete_session("other", 300).unwrap());
    assert!(store.purge_deleted_sessions(50).unwrap().is_empty());
    assert_eq!(store.purge_deleted_sessions(51).unwrap(), ["earlier"]);
    assert_eq!(store.purge_deleted_sessions(301).unwrap(), ["other"]);
    for table in [
        "agent_messages",
        "agent_runs",
        "agent_permission_rules",
        "agent_queue",
        "agent_tool_calls",
        "agent_file_changes",
        "agent_message_attachments",
        "agent_messages_fts",
    ] {
        assert_eq!(count(&store, table), 0, "{table}");
    }
    assert_eq!(count(&store, "agent_attachments"), 1);
}

#[test]
fn session_list_filters_queries_previews_and_cursor_paging() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let mut plain = session("plain", 100);
    plain.workspace_dir = Some("/w/a".to_owned());
    plain.title = "Quarterly report".to_owned();
    plain.spanbay_workspace_id = Some("ws-1".to_owned());
    let mut pinned_old = session("pinned_old", 10);
    pinned_old.pinned_at_ms = Some(5);
    pinned_old.title = "100% done_ok".to_owned();
    let mut pinned_new = session("pinned_new", 50);
    pinned_new.pinned_at_ms = Some(6);
    let mut archived = session("archived", 200);
    archived.archived_at_ms = Some(7);
    let mut sub = session("sub", 300);
    sub.parent_session_id = Some("plain".to_owned());
    let mut gone = session("gone", 400);
    gone.deleted_at_ms = Some(8);
    for s in [&plain, &pinned_old, &pinned_new, &archived, &sub, &gone] {
        store.save_agent_session(s).unwrap();
    }
    store
        .insert_agent_run(&run(
            "live",
            "pinned_old",
            AgentRunStatus::WaitingApproval,
            1,
        ))
        .unwrap();
    store
        .append_agent_message(&message("plain", "m1", "the NEEDLE is here", 1))
        .unwrap();
    let long = "é".repeat(300);
    store
        .append_agent_message(&assistant("pinned_new", "m2", &long, 2))
        .unwrap();

    let with = |f: AgentSessionFilter| page(&store, &f);
    let def = AgentSessionFilter::default;
    assert_eq!(with(def()), ["pinned_new", "pinned_old", "plain"]);
    assert_eq!(
        with(AgentSessionFilter {
            include_archived: true,
            ..def()
        }),
        ["pinned_new", "pinned_old", "archived", "plain"]
    );
    assert_eq!(
        with(AgentSessionFilter {
            pinned_only: true,
            ..def()
        }),
        ["pinned_new", "pinned_old"]
    );
    assert_eq!(
        with(AgentSessionFilter {
            workspace_dir: Some("/w/a".to_owned()),
            ..def()
        }),
        ["plain"]
    );
    assert_eq!(
        with(AgentSessionFilter {
            only_running: true,
            ..def()
        }),
        ["pinned_old"]
    );
    assert_eq!(
        with(AgentSessionFilter {
            spanbay_workspace_id: Some("ws-1".to_owned()),
            ..def()
        }),
        ["plain"]
    );
    assert_eq!(
        with(AgentSessionFilter {
            no_folder: true,
            ..def()
        }),
        ["pinned_new", "pinned_old"]
    );
    assert_eq!(
        with(AgentSessionFilter {
            deleted_only: true,
            ..def()
        }),
        ["gone"]
    );
    assert_eq!(
        with(AgentSessionFilter {
            parent_session_id: Some("plain".to_owned()),
            ..def()
        }),
        ["sub"]
    );
    assert_eq!(
        with(AgentSessionFilter {
            query: Some("needle".to_owned()),
            ..def()
        }),
        ["plain"]
    );
    assert_eq!(
        with(AgentSessionFilter {
            query: Some("ee".to_owned()),
            ..def()
        }),
        ["plain"]
    );
    assert_eq!(
        with(AgentSessionFilter {
            query: Some("%".to_owned()),
            ..def()
        }),
        ["pinned_old"]
    );

    let summaries = store
        .list_agent_sessions(&def(), None, None)
        .unwrap()
        .sessions;
    assert_eq!(summaries[0].preview.chars().count(), 160);

    // Cursor paging and bad cursor rejection
    let p1 = store.list_agent_sessions(&def(), Some(2), None).unwrap();
    let p2 = store
        .list_agent_sessions(&def(), Some(2), p1.next_cursor.as_deref())
        .unwrap();
    assert_eq!(
        (p1.sessions.len(), p2.sessions.len(), p2.next_cursor),
        (2, 1, None)
    );
    assert!(matches!(
        store.list_agent_sessions(&def(), None, Some("bad")),
        Err(StorageError::InvalidAgentData { .. })
    ));
}

#[test]
fn full_access_ends_at_start_with_a_note_and_other_modes_stay() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let mut full = session("full", 1);
    full.permission_mode = ToolApproval::AutoAll;
    store.save_agent_session(&full).unwrap();
    store.save_agent_session(&session("reads", 1)).unwrap();
    assert_eq!(store.end_full_access().unwrap(), 1);
    let ended = store.get_agent_session("full").unwrap().unwrap();
    assert_eq!(ended.permission_mode, ToolApproval::AutoReadOnly);
    assert_eq!(ended.notice.as_deref(), Some("full_access_off"));
    let other = store.get_agent_session("reads").unwrap().unwrap();
    assert_eq!(other.notice, None);
    assert_eq!(store.end_full_access().unwrap(), 0);
}

#[test]
fn the_list_cursor_round_trips_an_id_that_contains_a_colon() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    for (id, updated_at_ms) in [("a:b", 30), ("a:c", 20), ("z", 10)] {
        store
            .save_agent_session(&session(id, updated_at_ms))
            .unwrap();
    }
    let filter = AgentSessionFilter::default();

    let mut cursor: Option<String> = None;
    let mut seen = Vec::new();
    loop {
        let page = store
            .list_agent_sessions(&filter, Some(1), cursor.as_deref())
            .unwrap();
        seen.extend(page.sessions.into_iter().map(|s| s.id));
        cursor = page.next_cursor;
        if cursor.is_none() {
            break;
        }
    }
    assert_eq!(seen, ["a:b", "a:c", "z"]);
}

#[test]
fn clearing_a_notice_reports_whether_there_was_one() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    let mut full = session("full", 1);
    full.permission_mode = ToolApproval::AutoAll;
    store.save_agent_session(&full).unwrap();
    store.end_full_access().unwrap();

    assert!(store.clear_session_notice("full").unwrap());
    assert_eq!(
        store.get_agent_session("full").unwrap().unwrap().notice,
        None
    );
    assert!(!store.clear_session_notice("full").unwrap());
    assert!(!store.clear_session_notice("ghost").unwrap());
}

#[test]
fn the_stop_reason_is_recorded_in_the_message_meta() {
    let store = store_with(&["s"]);
    store
        .append_agent_message(&assistant("s", "a1", "done", 1))
        .unwrap();

    store
        .set_message_stop_reason("a1", AgentStopReason::MaxIterations)
        .unwrap();
    assert_eq!(
        store
            .get_agent_message("s", "a1")
            .unwrap()
            .unwrap()
            .meta
            .stop_reason,
        Some(AgentStopReason::MaxIterations)
    );
}

#[test]
fn the_spanbay_binding_is_set_cleared_and_absent_for_an_unknown_chat() {
    let store = store_with(&["s"]);
    let binding = SpanBayBinding {
        workspace_id: "ws-1".to_owned(),
        environment_id: Some("env-1".to_owned()),
    };

    assert!(
        store
            .set_session_spanbay_binding("s", Some(&binding))
            .unwrap()
    );
    assert_eq!(store.session_spanbay_binding("s").unwrap(), Some(binding));
    assert!(store.set_session_spanbay_binding("s", None).unwrap());
    assert_eq!(store.session_spanbay_binding("s").unwrap(), None);
    assert!(!store.set_session_spanbay_binding("ghost", None).unwrap());
    assert_eq!(store.session_spanbay_binding("ghost").unwrap(), None);
}
