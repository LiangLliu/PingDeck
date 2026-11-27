//! Cross-session message search (trigram index and short-query scan), the in-chat find and how
//! tool results reach the index.

use super::*;

fn found(store: &WorkspaceStore, query: &str) -> Vec<String> {
    store
        .search_agent_messages(query, 50)
        .unwrap()
        .into_iter()
        .map(|hit| hit.message_id)
        .collect()
}

#[test]
fn trigram_and_short_query_search_across_sessions_and_tools() {
    let store = store_with(&["a", "b"]);
    let mut sub = session("sub", 1);
    sub.parent_session_id = Some("a".to_owned());
    store.save_agent_session(&sub).unwrap();

    store
        .append_agent_message(&message(
            "a",
            "m1",
            "Please roll back the Kubernetes deployment",
            10,
        ))
        .unwrap();
    store
        .append_agent_message(&assistant(
            "b",
            "m2",
            "kubernetes is a scheduler with 50% off_sale",
            20,
        ))
        .unwrap();
    store
        .append_agent_message(&message(
            "a",
            "m3",
            r#"he said "hello" * -dash AND OR NOT"#,
            30,
        ))
        .unwrap();
    store
        .append_agent_message(&message("sub", "ms", "kubernetes in subagent", 40))
        .unwrap();

    assert_eq!(found(&store, "KUBERN"), ["m2", "m1"]);
    assert_eq!(found(&store, "ku"), ["m2", "m1"]);
    assert_eq!(found(&store, "%"), ["m2"]);
    assert_eq!(found(&store, "_"), ["m2"]);
    assert_eq!(found(&store, r#""hello""#), ["m3"]);
    assert_eq!(found(&store, "AND OR NOT"), ["m3"]);
    assert!(found(&store, "   ").is_empty());

    // Soft-deleted sessions hide from search and return on restore
    store.soft_delete_session("a", 9).unwrap();
    assert_eq!(found(&store, "kubernetes"), ["m2"]);
    store.restore_session("a").unwrap();
    assert_eq!(found(&store, "kubernetes"), ["m2", "m1"]);

    // Tool results are flagged
    let mut with_call = assistant_calling("a", "as1", &[("c1", "web_search")], 50);
    with_call.parts.push(HubPart::text("calling tool"));
    store
        .append_assistant_with_pending_results(&with_call, &tool_shell("a", "t1", 51))
        .unwrap();
    store
        .update_tool_result_part("t1", "c1", &result("c1", "RESULTBODY of search", false))
        .unwrap();
    let hits = store.search_agent_messages("RESULTBODY", 10).unwrap();
    assert_eq!(
        (hits[0].message_id.as_str(), hits[0].in_tool_output),
        ("t1", true)
    );
}

#[test]
fn fts_tracks_updates_and_deletes_and_bounds_cross_session_results() {
    let store = store_with(&["a"]);
    store
        .append_agent_message(&message("a", "m1", "original wording", 1))
        .unwrap();
    assert_eq!(found(&store, "original"), ["m1"]);
    store
        .connection
        .execute(
            "UPDATE agent_messages SET search_text = 'replacement wording' WHERE id = 'm1'",
            [],
        )
        .unwrap();
    assert!(found(&store, "original").is_empty());
    assert_eq!(found(&store, "replacement"), ["m1"]);
    store
        .connection
        .execute("DELETE FROM agent_messages WHERE id = 'm1'", [])
        .unwrap();
    assert!(found(&store, "replacement").is_empty());
    for i in 0..25 {
        let chat = format!("p{i}");
        store.save_agent_session(&session(&chat, 1)).unwrap();
        store
            .append_agent_message(&message(&chat, &format!("limit{i}"), "limit probe", 10 + i))
            .unwrap();
    }
    let hits = |limit| {
        store
            .search_agent_messages("limit probe", limit)
            .unwrap()
            .len()
    };
    assert_eq!((hits(3), hits(0), hits(u32::MAX)), (3, 1, 20));
}

#[test]
fn a_tool_result_is_indexed_by_its_head_and_tail_only() {
    let store = store_with(&["s"]);
    turns_of(&store, "s", 30);

    let big = format!("HEADWORD {} TAILWORD", "m".repeat(20_000));
    let with_middle = format!("{}MIDDLEWORD{}", &big[..10_000], &big[10_000..]);
    store
        .append_assistant_with_pending_results(
            &assistant_calling("s", "ac-1", &[("c1", "shell")], 400),
            &tool_shell("s", "tm-1", 401),
        )
        .unwrap();
    store
        .update_tool_result_part("tm-1", "c1", &result("c1", &with_middle, false))
        .unwrap();
    assert_eq!(store.search_agent_messages("headword", 5).unwrap().len(), 1);
    assert_eq!(store.search_agent_messages("tailword", 5).unwrap().len(), 1);
    assert!(
        store
            .search_agent_messages("middleword", 5)
            .unwrap()
            .is_empty()
    );

    let session_hits = store.find_in_session("s", "headword").unwrap();
    assert_eq!(session_hits.len(), 1);
    assert_eq!(
        (session_hits[0].turn_index, session_hits[0].in_tool_output),
        (29, true)
    );
}
