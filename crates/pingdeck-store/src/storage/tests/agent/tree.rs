//! The message tree: append under the head, edit, retry, switch, delete, branch into a new chat,
//! and the plan of each branch.

use super::*;
use pingdeck_model::agent::{AgentTodoItem, AgentTodoStatus, MessageDeletion};

fn head(store: &WorkspaceStore, session_id: &str) -> Option<String> {
    store
        .get_agent_session(session_id)
        .unwrap()
        .unwrap()
        .head_message_id
}

fn path(store: &WorkspaceStore, session_id: &str) -> Vec<String> {
    store
        .list_active_path(session_id)
        .unwrap()
        .into_iter()
        .map(|m| m.id)
        .collect()
}

fn text_of(message: &AgentMessage) -> String {
    match &message.parts[0] {
        HubPart::Text { text, .. } => text.clone(),
        other => panic!("not text: {other:?}"),
    }
}

fn todo(id: &str) -> AgentTodoItem {
    AgentTodoItem {
        id: id.to_owned(),
        content: format!("task {id}"),
        status: AgentTodoStatus::Pending,
        ..AgentTodoItem::default()
    }
}

fn plan_ids(store: &WorkspaceStore, session_id: &str) -> Vec<String> {
    store
        .get_agent_session(session_id)
        .unwrap()
        .unwrap()
        .todos
        .into_iter()
        .map(|t| t.id)
        .collect()
}

#[test]
fn append_edit_retry_and_branch_switching() {
    let store = store_with(&["s"]);
    add_attachment(&store, "att1", "one.png");
    add_attachment(&store, "att2", "two.png");

    // Append links under head, preserves columns/attachments, and rejects missing attachments
    // or dupes
    let first = store
        .append_agent_message(&with_attachment(
            with_attachment(message("s", "u1", "q1", 10), "att2"),
            "att1",
        ))
        .unwrap();
    assert_eq!(
        (first.seq, first.parent_id, first.branch_index),
        (1, None, 0)
    );
    assert_eq!(links(&store, "u1"), ["att2", "att1"]);
    assert!(
        store
            .append_agent_message(&message("s", "u1", "dup", 10))
            .is_err()
    );
    assert!(
        store
            .append_agent_message(&with_attachment(message("s", "bad", "x", 11), "ghost"))
            .is_err()
    );

    store
        .append_agent_message(&assistant("s", "a1", "r1", 11))
        .unwrap();
    store
        .append_agent_message(&message("s", "u2", "q2", 20))
        .unwrap();
    store
        .append_agent_message(&assistant("s", "a2", "r2", 21))
        .unwrap();
    store
        .append_agent_message(&message("s", "u3", "q3", 30))
        .unwrap();
    store
        .append_agent_message(&assistant("s", "a3", "r3", 31))
        .unwrap();

    // Edit creates sibling under same parent and keeps old branch reachable
    let edited = store
        .create_sibling_user_message(
            "s",
            "u2",
            vec![HubPart::text("q2 edited")],
            AgentMessageMeta::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!(
        (edited.parent_id.as_deref(), edited.branch_index),
        (Some("a1"), 1)
    );
    assert_eq!(edited.branch_siblings, ["u2", edited.id.as_str()]);
    assert_eq!(path(&store, "s"), ["u1", "a1", edited.id.as_str()]);
    assert!(
        store
            .create_sibling_user_message("s", "a1", vec![], AgentMessageMeta::default())
            .is_err()
    );

    // Switch back to u2 returns to leaf a3
    assert_eq!(store.set_head("s", "u2").unwrap().as_deref(), Some("a3"));
    assert_eq!(path(&store, "s"), ["u1", "a1", "u2", "a2", "u3", "a3"]);
    assert_eq!(store.set_head("s", "ghost").unwrap(), None);

    // Retry moves head to turn-opening user message
    let retry_user = store.prepare_retry("s", "a3").unwrap().unwrap();
    assert_eq!(
        (retry_user.id.as_str(), head(&store, "s").as_deref()),
        ("u3", Some("u3"))
    );
    let a3b = store
        .append_agent_message(&assistant("s", "a3b", "r3 retry", 40))
        .unwrap();
    assert_eq!(a3b.branch_siblings, ["a3", "a3b"]);

    // Switch between branches remembers last active leaf
    store.set_head("s", &edited.id).unwrap();
    assert_eq!(store.set_head("s", "u2").unwrap().as_deref(), Some("a3b"));
}

#[test]
fn delete_prunes_subtrees_and_runs_and_branch_copies_path_with_compaction() {
    let store = store_with(&["s"]);
    add_attachment(&store, "att", "a.png");
    store
        .append_agent_message(&with_attachment(message("s", "u1", "look", 1), "att"))
        .unwrap();
    store
        .append_agent_message(&AgentMessage {
            run_id: Some("r1".to_owned()),
            cost_micro_usd: 5,
            ..assistant("s", "a1", "seen", 2)
        })
        .unwrap();
    store
        .append_agent_message(&AgentMessage {
            run_id: Some("r2".to_owned()),
            ..message("s", "u2", "more", 3)
        })
        .unwrap();
    store
        .append_agent_message(&AgentMessage {
            run_id: Some("r2".to_owned()),
            ..assistant("s", "a2", "ok", 4)
        })
        .unwrap();
    store
        .insert_agent_run(&run("r1", "s", AgentRunStatus::Completed, 1))
        .unwrap();
    store
        .insert_agent_run(&run("r2", "s", AgentRunStatus::Completed, 2))
        .unwrap();
    let mut call = tool_call("s", "c1", "r2");
    call.message_id = Some("a2".to_owned());
    store.upsert_tool_call(&call).unwrap();
    store
        .insert_file_change(&change("s", "fc2", "r2", FileChangeStatus::Pending))
        .unwrap();

    store
        .append_compaction_summary("s", None, vec![HubPart::text("gist")], "a1", Vec::new())
        .unwrap();
    store
        .append_agent_message(&message("s", "u3", "after", 5))
        .unwrap();

    // Branch session copies path and remaps compaction range
    let copy = store
        .branch_session("s", "u3", &session("copy", 100))
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            copy.forked_from.as_deref(),
            copy.forked_from_message_id.as_deref()
        ),
        (Some("s"), Some("u3"))
    );
    let copy_history = store.list_model_history("copy").unwrap();
    assert_eq!(copy_history[0].role, AgentRole::Summary);
    assert_eq!(text_of(&copy_history[1]), "more");
    assert_eq!(
        store
            .branch_session("s", "ghost", &session("c", 1))
            .unwrap(),
        None
    );
    assert!(
        store
            .branch_session("s", "u1", &session("copy", 1))
            .is_err()
    );

    // Preview and delete subtree u2..u3
    let preview = store.preview_message_deletion("s", "u2").unwrap().unwrap();
    assert_eq!(
        preview,
        MessageDeletion {
            messages: 4,
            runs: 1,
            tool_calls: 1,
            file_changes: 1,
            pending_file_changes: 1,
            branches: 1
        }
    );
    assert_eq!(store.delete_message("s", "u2").unwrap(), Some(preview));
    assert_eq!(head(&store, "s").as_deref(), Some("a1"));
    assert!(store.get_agent_run("r2").unwrap().is_none());
    assert!(store.get_agent_run("r1").unwrap().is_some());
}

#[test]
fn root_edits_keep_only_selected_attachments_and_confine_foreign_ids() {
    let store = store_with(&["s"]);
    store.save_agent_session(&session("o", 1)).unwrap();
    for id in ["keep", "drop"] {
        add_attachment(&store, id, "image.png");
    }
    let user = with_attachment(
        with_attachment(message("s", "u1", "look", 1), "keep"),
        "drop",
    );
    store.append_agent_message(&user).unwrap();
    let edited = store
        .create_sibling_user_message(
            "s",
            "u1",
            with_attachment(message("s", "x", "look again", 1), "keep").parts,
            AgentMessageMeta::default(),
        )
        .unwrap()
        .unwrap();
    assert_eq!((edited.parent_id.clone(), edited.branch_index), (None, 1));
    assert_eq!(links(&store, &edited.id), ["keep"]);
    assert_eq!(links(&store, "u1"), ["keep", "drop"]);
    assert!(
        store
            .create_sibling_user_message(
                "s",
                "u1",
                with_attachment(message("s", "x", "x", 1), "ghost").parts,
                AgentMessageMeta::default()
            )
            .is_err()
    );
    assert_eq!(count(&store, "agent_messages"), 2);
    assert_eq!(head(&store, "s").as_deref(), Some(edited.id.as_str()));
    assert_eq!(store.get_agent_message("o", "u1").unwrap(), None);
    assert_eq!(
        store
            .create_sibling_user_message("o", "u1", vec![], AgentMessageMeta::default())
            .unwrap(),
        None
    );
    assert_eq!(store.set_head("o", "u1").unwrap(), None);
    assert!(
        store
            .upsert_partial_assistant("o", "r", &edited.id, vec![])
            .is_err()
    );
    let mut steer = message("s", "steer", "also this", 2);
    steer.meta.steered = true;
    store.append_agent_message(&steer).unwrap();
    assert!(
        store
            .create_sibling_user_message("s", "steer", vec![], AgentMessageMeta::default())
            .is_err()
    );
    store
        .append_agent_message(&assistant("o", "orphan", "hi", 1))
        .unwrap();
    assert!(store.prepare_retry("o", "orphan").is_err());
}

fn nested_branches() -> WorkspaceStore {
    let store = store_with(&["s"]);
    for i in 1..=3 {
        store
            .append_agent_message(&message("s", &format!("u{i}"), "q", i * 10))
            .unwrap();
        store
            .append_agent_message(&assistant("s", &format!("a{i}"), "r", i * 10 + 1))
            .unwrap();
    }
    store.prepare_retry("s", "a2").unwrap();
    store
        .append_agent_message(&assistant("s", "a2b", "alt", 50))
        .unwrap();
    store
        .append_agent_message(&message("s", "u4", "next", 51))
        .unwrap();
    store
        .append_agent_message(&assistant("s", "a4", "last", 52))
        .unwrap();
    store.set_head("s", "a2").unwrap();
    store.prepare_retry("s", "a3").unwrap();
    store
        .append_agent_message(&assistant("s", "a3b", "alt3", 60))
        .unwrap();
    store
}

#[test]
fn deleting_active_and_inactive_branches_selects_the_right_leaf() {
    let store = nested_branches();
    assert_eq!(
        store.delete_message("s", "a3b").unwrap().unwrap().messages,
        1
    );
    assert_eq!(head(&store, "s").as_deref(), Some("a3"));
    assert!(
        store
            .get_agent_message("s", "a3")
            .unwrap()
            .unwrap()
            .branch_siblings
            .is_empty()
    );
    assert_eq!(
        store.delete_message("s", "a2").unwrap().unwrap().messages,
        3
    );
    assert_eq!(path(&store, "s"), ["u1", "a1", "u2", "a2b", "u4", "a4"]);
    let store = nested_branches();
    assert_eq!(
        store.delete_message("s", "a2b").unwrap().unwrap().messages,
        3
    );
    assert_eq!(head(&store, "s").as_deref(), Some("a3b"));
    store.delete_message("s", "u1").unwrap().unwrap();
    assert_eq!(head(&store, "s"), None);
    assert!(store.list_active_path("s").unwrap().is_empty());
    let fresh = store
        .append_agent_message(&message("s", "new", "again", 99))
        .unwrap();
    assert_eq!((fresh.parent_id, fresh.branch_index), (None, 0));
}

#[test]
fn branching_copies_only_selected_path_and_rolls_back_on_insert_failure() {
    let store = nested_branches();
    add_attachment(&store, "att", "a.png");
    store
        .append_agent_message(&with_attachment(message("s", "image", "seen", 70), "att"))
        .unwrap();
    store
        .add_session_usage("s", &HubUsage::default(), 500, 20)
        .unwrap();
    let mut wanted = session("copy", 100);
    wanted.total_cost_micro_usd = 12345;
    wanted.deleted_at_ms = Some(1);
    wanted.parent_session_id = Some("s".to_owned());
    wanted.instructions = Some("source-private".to_owned());
    let copy = store
        .branch_session("s", "image", &wanted)
        .unwrap()
        .unwrap();
    assert_eq!(
        (
            copy.total_cost_micro_usd,
            copy.last_context_tokens,
            copy.deleted_at_ms,
            copy.parent_session_id,
            copy.instructions
        ),
        (0, 0, None, None, None)
    );
    let messages = store.list_agent_messages("copy").unwrap();
    assert_eq!(messages.len(), 7);
    assert!(
        messages
            .iter()
            .all(|m| m.branch_index == 0 && m.run_id.is_none())
    );
    assert_eq!(links(&store, &messages[6].id), ["att"]);
    assert_eq!(links(&store, "image"), ["att"]);
    assert_eq!(
        store
            .get_agent_session("s")
            .unwrap()
            .unwrap()
            .total_cost_micro_usd,
        500
    );

    store
        .connection
        .execute_batch(
            "CREATE TRIGGER fail_copy BEFORE INSERT ON agent_messages
         WHEN NEW.session_id = 'fail' AND NEW.seq = 3
         BEGIN SELECT RAISE(ABORT, 'boom'); END;",
        )
        .unwrap();
    assert!(
        store
            .branch_session("s", "image", &session("fail", 1))
            .is_err()
    );
    assert_eq!(store.get_agent_session("fail").unwrap(), None);
    assert!(store.list_agent_messages("fail").unwrap().is_empty());
}

#[test]
fn subtree_deletion_spares_a_run_with_messages_outside_that_subtree() {
    let store = store_with(&["s"]);
    for entry in [
        message("s", "u1", "q1", 1),
        AgentMessage {
            run_id: Some("r1".to_owned()),
            ..assistant("s", "a1", "ans1", 2)
        },
        AgentMessage {
            run_id: Some("r2".to_owned()),
            ..message("s", "u2", "q2", 3)
        },
        AgentMessage {
            run_id: Some("r2".to_owned()),
            ..assistant("s", "a2", "ans2", 4)
        },
        AgentMessage {
            run_id: Some("r3".to_owned()),
            ..assistant("s", "a3", "ans3", 5)
        },
    ] {
        store.append_agent_message(&entry).unwrap();
    }
    store.prepare_retry("s", "a1").unwrap();
    store
        .append_agent_message(&AgentMessage {
            run_id: Some("r3".to_owned()),
            ..message("s", "u9", "q9", 9)
        })
        .unwrap();
    for id in ["r1", "r2", "r3"] {
        store
            .insert_agent_run(&run(id, "s", AgentRunStatus::Completed, 1))
            .unwrap();
    }
    for id in ["r1", "r2"] {
        store.upsert_tool_call(&tool_call("s", id, id)).unwrap();
        store
            .insert_file_change(&change("s", id, id, FileChangeStatus::Pending))
            .unwrap();
    }
    // From the root: the retry's sibling branch and both runs' unreviewed changes go too.
    let whole = store.preview_message_deletion("s", "u1").unwrap().unwrap();
    assert_eq!((whole.branches, whole.pending_file_changes), (2, 2));
    let preview = store.preview_message_deletion("s", "u2").unwrap().unwrap();
    assert_eq!(
        preview,
        MessageDeletion {
            messages: 3,
            runs: 1,
            tool_calls: 1,
            file_changes: 1,
            pending_file_changes: 1,
            branches: 1
        }
    );
    assert_eq!(count(&store, "agent_messages"), 6);
    assert_eq!(store.delete_message("s", "u2").unwrap(), Some(preview));
    assert_eq!(store.get_agent_run("r2").unwrap(), None);
    assert!(
        store.get_agent_run("r1").unwrap().is_some()
            && store.get_agent_run("r3").unwrap().is_some()
    );
    assert_eq!(
        store
            .list_tool_calls("s")
            .unwrap()
            .iter()
            .map(|c| c.call_id.as_str())
            .collect::<Vec<_>>(),
        ["r1"]
    );
    assert_eq!(
        store
            .list_chat_file_changes("s")
            .unwrap()
            .iter()
            .map(|c| c.id.as_str())
            .collect::<Vec<_>>(),
        ["r1"]
    );
    assert_eq!(store.preview_message_deletion("s", "ghost").unwrap(), None);
    assert_eq!(store.delete_message("s", "ghost").unwrap(), None);
}

#[test]
fn each_branch_keeps_its_own_plan() {
    let store = store_with(&["s"]);
    turns_of(&store, "s", 30);
    store
        .save_session_todos("s", &[todo("1"), todo("2")])
        .unwrap();

    let edited = store
        .create_sibling_user_message(
            "s",
            "u30",
            vec![HubPart::text("other")],
            AgentMessageMeta::default(),
        )
        .unwrap()
        .unwrap();
    store.save_session_todos("s", &[todo("9")]).unwrap();
    store.set_head("s", "u30").unwrap();
    assert_eq!(plan_ids(&store, "s"), ["1", "2"]);
    store.set_head("s", &edited.id).unwrap();
    assert_eq!(plan_ids(&store, "s"), ["9"]);
}
