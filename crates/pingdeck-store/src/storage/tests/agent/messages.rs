//! Message rows: partial streaming rows, crash-safe tool turns, model history and compaction,
//! the paged active path and message attachments.

use super::*;

#[test]
fn partial_rows_model_history_compaction_and_turn_paging() {
    let store = store_with(&["s"]);
    store
        .append_agent_message(&message("s", "u1", "q1", 1))
        .unwrap();

    // Partial assistant row is not indexed until finalized
    let part1 = store
        .upsert_partial_assistant("s", "r1", "a1", vec![HubPart::text("Hel")])
        .unwrap();
    assert!(part1.partial);
    let part2 = store
        .upsert_partial_assistant("s", "r1", "a1", vec![HubPart::text("Hello world")])
        .unwrap();
    assert_eq!((part2.seq, count(&store, "agent_messages_fts")), (2, 1));

    let done = store
        .append_agent_message(&AgentMessage {
            run_id: Some("r1".to_owned()),
            ..assistant("s", "a1", "Hello world", 7)
        })
        .unwrap();
    assert!(!done.partial);
    assert_eq!(count(&store, "agent_messages_fts"), 2);
    assert!(
        store
            .upsert_partial_assistant("s", "r1", "a1", vec![])
            .is_err()
    );

    // Steered message stays inside turn
    let mut steer = message("s", "steer", "also", 8);
    steer.meta.steered = true;
    store.append_agent_message(&steer).unwrap();
    store
        .append_agent_message(&assistant("s", "a1b", "done", 9))
        .unwrap();
    store
        .append_agent_message(&message("s", "u2", "q2", 10))
        .unwrap();
    store
        .append_agent_message(&assistant("s", "a2", "r2", 11))
        .unwrap();

    let (p1, more1) = store.list_active_path_page("s", None, 1).unwrap();
    assert_eq!((ids(&p1), more1), (vec!["u2", "a2"], true));
    let (p2, more2) = store
        .list_active_path_page("s", Some(p1[0].seq), 1)
        .unwrap();
    assert_eq!((ids(&p2), more2), (vec!["u1", "a1", "steer", "a1b"], false));

    // Compaction summary replaces earlier turns in model history
    let sum = store
        .append_compaction_summary("s", None, vec![HubPart::text("gist")], "a1b", Vec::new())
        .unwrap();
    assert_eq!(
        ids(&store.list_model_history("s").unwrap()),
        [sum.id.as_str(), "u2", "a2"]
    );
    assert!(
        store
            .append_compaction_summary("s", None, vec![], "ghost", Vec::new())
            .is_err()
    );
}

#[test]
fn a_tool_turn_stores_one_placeholder_per_call_and_results_replace_them() {
    let store = store_with(&["a", "b"]);
    store
        .append_agent_message(&message("a", "u1", "go", 1))
        .unwrap();

    // Validation and rollback on clash
    let calling = assistant_calling("a", "as1", &[("c1", "t"), ("c2", "t")], 5);
    assert!(
        store
            .append_assistant_with_pending_results(&calling, &tool_shell("a", "as1", 6))
            .is_err()
    );
    assert!(
        store
            .append_assistant_with_pending_results(&calling, &tool_shell("b", "tm1", 6))
            .is_err()
    );

    let (_, tool) = store
        .append_assistant_with_pending_results(&calling, &tool_shell("a", "tm1", 6))
        .unwrap();
    assert!(
        store
            .update_tool_result_part(&tool.id, "c1", &result("c1", "first done", false))
            .unwrap()
    );
    assert!(
        !store
            .update_tool_result_part("ghost", "c1", &result("c1", "x", false))
            .unwrap()
    );
    assert!(
        store
            .update_tool_result_part(&tool.id, "c1", &result("c2", "wrong", false))
            .is_err()
    );

    let history = history_for_provider(&store, "a");
    assert_eq!(
        answered_calls(&history),
        [vec!["c1".to_owned(), "c2".to_owned()]]
    );
}

#[test]
fn a_page_opens_around_a_turn_and_scrolls_forward_from_it() {
    let store = store_with(&["s"]);
    turns_of(&store, "s", 30);

    let window = store
        .list_active_path_around("s", "a12", 10)
        .unwrap()
        .unwrap();
    assert!(window.has_before && window.has_after);
    let next = store
        .list_active_path_after("s", window.messages.last().unwrap().seq, 5)
        .unwrap();
    assert!(next.messages[0].seq > window.messages.last().unwrap().seq);
    assert!(
        store
            .list_active_path_around("s", "missing", 10)
            .unwrap()
            .is_none()
    );
}

#[test]
fn forgetting_attachment_previews_keeps_the_attachment_rows() {
    let store = store_with(&["s"]);
    add_attachment(&store, "a1", "pic.png");
    store
        .append_agent_message(&with_attachment(message("s", "u2", "look", 2), "a1"))
        .unwrap();
    assert_eq!(store.list_session_attachments("s").unwrap().len(), 1);
    store
        .forget_attachment_previews(&["a1".to_owned()])
        .unwrap();
    assert!(
        store.list_message_attachments(&["u2".to_owned()]).unwrap()["u2"][0]
            .thumb_data_url
            .is_none()
    );
}
