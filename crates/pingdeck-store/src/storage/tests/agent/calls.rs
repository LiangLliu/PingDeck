//! The tool-call audit, the file-change journal (applying rows, turn changes) and the global
//! per-tool policy.

use super::*;
use crate::storage::StorageError;
use pingdeck_model::agent::AgentToolPolicy;

#[test]
fn tool_calls_upsert_in_first_seen_order_and_need_their_session() {
    let store = store_with(&["a", "b"]);

    let mut tc = tool_call("a", "c2", "r1");
    store.upsert_tool_call(&tc).unwrap();
    store.upsert_tool_call(&tool_call("a", "c1", "r1")).unwrap();
    tc.state = "approval-timed-out".to_owned();
    tc.decided_by = Some("timeout".to_owned());
    tc.decision = Some("none".to_owned());
    tc.decided_ms = Some(7);
    tc.started_ms = Some(8);
    tc.ended_ms = Some(9);
    tc.exit_code = Some(-1);
    tc.sandbox = Some("seatbelt".to_owned());
    tc.output_bytes = 1 << 40;
    tc.spill_ref = Some("tool-output/c2".to_owned());
    tc.message_id = Some("m1".to_owned());
    tc.risk = "high".to_owned();
    tc.truncated = true;
    store.upsert_tool_call(&tc).unwrap();
    let listed_tc = store.list_tool_calls("a").unwrap();
    assert_eq!((&listed_tc[0], listed_tc[1].call_id.as_str()), (&tc, "c1"));
    assert!(
        store
            .upsert_tool_call(&tool_call("ghost", "c", "r"))
            .is_err()
    );
}

#[test]
fn a_partial_tool_call_write_keeps_the_audit_columns_and_a_later_value_wins() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    store.save_agent_session(&session("a", 1)).unwrap();
    let mut full = tool_call("a", "c1", "r1");
    full.decided_by = Some("forced:production".to_owned());
    full.exit_code = Some(2);
    full.truncated = true;
    full.output_bytes = 99;
    full.spill_ref = Some("c1".to_owned());
    full.requested_ms = Some(7);
    store.upsert_tool_call(&full).unwrap();
    let mut partial = tool_call("a", "c1", "r1");
    partial.state = "output-available".to_owned();
    store.upsert_tool_call(&partial).unwrap();
    let kept = store.get_tool_call("a", "c1").unwrap().unwrap();
    assert_eq!(kept.state, "output-available");
    assert_eq!(kept.decided_by.as_deref(), Some("forced:production"));
    assert_eq!(
        (
            kept.exit_code,
            kept.truncated,
            kept.output_bytes,
            kept.spill_ref.as_deref()
        ),
        (Some(2), true, 99, Some("c1"))
    );
    let mut later = partial;
    later.decided_by = Some("user".to_owned());
    later.output_bytes = 5;
    later.requested_ms = Some(9);
    store.upsert_tool_call(&later).unwrap();
    let after = store.get_tool_call("a", "c1").unwrap().unwrap();
    assert_eq!(
        (after.decided_by.as_deref(), after.output_bytes),
        (Some("user"), 5)
    );
    // The card's first appearance is the wait's start: a later write never moves it.
    assert_eq!(after.requested_ms, Some(7));
}

#[test]
fn the_file_journal_lists_a_session_oldest_first_and_sets_status_within_it() {
    let store = store_with(&["a", "b"]);

    let mut late = change("a", "late", "r", FileChangeStatus::Pending);
    late.created_at_ms = 30;
    late.kind = FileChangeKind::Create;
    store.insert_file_change(&late).unwrap();
    store
        .insert_file_change(&change("a", "early", "r", FileChangeStatus::Pending))
        .unwrap();
    store
        .insert_file_change(&change("b", "other", "r", FileChangeStatus::Pending))
        .unwrap();
    assert!(
        store
            .insert_file_change(&change("a", "early", "r", FileChangeStatus::Pending))
            .is_err()
    );
    assert_eq!(store.list_chat_file_changes("a").unwrap()[1], late);
    assert_eq!(
        store
            .set_file_change_status(
                "a",
                &["early".into(), "other".into()],
                FileChangeStatus::Kept
            )
            .unwrap(),
        1
    );
    assert_eq!(
        store.list_chat_file_changes("a").unwrap()[0].status,
        FileChangeStatus::Kept
    );
    assert_eq!(
        store.list_chat_file_changes("b").unwrap()[0].status,
        FileChangeStatus::Pending
    );
}

#[test]
fn applying_rows_are_hidden_until_finished_and_a_turn_lists_its_runs_changes() {
    let store = store_with(&["s"]);

    let mut u1 = message("s", "u1", "edit a", 1);
    u1.run_id = Some("r1".to_owned());
    store.append_agent_message(&u1).unwrap();
    store
        .insert_file_change(&change("s", "c-apply", "r1", FileChangeStatus::Applying))
        .unwrap();
    store
        .insert_file_change(&change("s", "c-done", "r1", FileChangeStatus::Pending))
        .unwrap();
    assert_eq!(store.list_chat_file_changes("s").unwrap().len(), 1);
    assert_eq!(store.list_applying_file_changes().unwrap().len(), 1);
    assert!(store.finish_file_change("c-apply").unwrap());
    assert_eq!(store.turn_file_changes("s", "u1").unwrap().len(), 2);
    assert!(store.delete_file_change("c-done").unwrap());
}

#[test]
fn a_tool_policy_replaces_the_previous_one_and_its_key_is_validated() {
    let store = WorkspaceStore::open_in_memory().unwrap();

    let pol = |k: &str, p| AgentToolPolicy {
        tool_key: k.to_owned(),
        policy: p,
    };
    store
        .set_tool_policy(&pol("shell", PermissionAction::Block))
        .unwrap();
    store
        .set_tool_policy(&pol("shell", PermissionAction::Ask))
        .unwrap();
    assert_eq!(
        store.list_tool_policy().unwrap(),
        [pol("shell", PermissionAction::Ask)]
    );
    assert!(store.delete_tool_policy("shell").unwrap());
    assert!(matches!(
        store.set_tool_policy(&pol("bad key", PermissionAction::Allow)),
        Err(StorageError::InvalidAgentData { .. })
    ));
}

#[test]
fn recent_blocked_calls_are_block_decisions_newest_first() {
    let store = store_with(&["a"]);
    for (call_id, decided_by) in [
        ("c1", Some("block:rule")),
        ("c2", Some("user")),
        ("c3", Some("hard_block:x")),
        ("c4", Some("block:y")),
        ("c5", None),
        ("c6", Some("unblock:z")),
    ] {
        let mut call = tool_call("a", call_id, "r");
        call.decided_by = decided_by.map(str::to_owned);
        store.upsert_tool_call(&call).unwrap();
    }
    let blocked = |limit| -> Vec<String> {
        store
            .recent_blocked_calls(limit)
            .unwrap()
            .into_iter()
            .map(|call| call.call_id)
            .collect()
    };

    assert_eq!(blocked(10), ["c4", "c3", "c1"]);
    assert_eq!(blocked(2), ["c4", "c3"]);
}

#[test]
fn a_chats_journal_includes_its_sub_agents_and_lookups_skip_applying_rows() {
    let store = store_with(&["a", "b"]);
    let mut sub = session("sub", 1);
    sub.parent_session_id = Some("a".to_owned());
    store.save_agent_session(&sub).unwrap();
    let at = |session_id: &str, id: &str, status, created_at_ms| FileChange {
        created_at_ms,
        ..change(session_id, id, "r", status)
    };
    for change in [
        at("a", "x", FileChangeStatus::Pending, 10),
        at("sub", "y", FileChangeStatus::Pending, 5),
        at("a", "z", FileChangeStatus::Applying, 1),
        at("b", "w", FileChangeStatus::Pending, 20),
    ] {
        store.insert_file_change(&change).unwrap();
    }
    let ids = |changes: Vec<FileChange>| -> Vec<String> {
        changes.into_iter().map(|change| change.id).collect()
    };

    assert_eq!(ids(store.list_chat_file_changes("a").unwrap()), ["y", "x"]);
    assert_eq!(
        ids(store
            .get_file_changes(&["w".into(), "z".into(), "x".into(), "ghost".into()])
            .unwrap()),
        ["x", "w"]
    );
}
