//! The per-session queue: the cap, edit, remove, pop, put back at the front and clear.

use super::*;
use spanbay_model::agent::{MAX_QUEUED_TURNS, QueuedContent};

#[test]
fn queue_caps_at_max_queued_turns_and_supports_edit_remove_pop_and_clear() {
    let store = store_with(&["a", "b"]);
    let parts = |t: &str| vec![HubPart::text(t)];
    let texts = |store: &WorkspaceStore| -> Vec<String> {
        store
            .list_queue("a")
            .unwrap()
            .into_iter()
            .map(|turn| match &turn.parts[0] {
                HubPart::Text { text, .. } => text.clone(),
                other => panic!("{other:?}"),
            })
            .collect()
    };

    let mut q_ids = Vec::new();
    for i in 0..MAX_QUEUED_TURNS {
        let turn = store
            .add_queued_turn("a", &parts(&format!("m{i}")).into())
            .unwrap()
            .unwrap();
        q_ids.push(turn.id);
    }
    assert_eq!(
        store.add_queued_turn("a", &parts("six").into()).unwrap(),
        None
    );
    assert!(store.add_queued_turn("ghost", &parts("x").into()).is_err());

    assert!(
        store
            .update_queued_turn("a", &q_ids[1], &parts("m1_ed").into())
            .unwrap()
    );
    assert!(
        !store
            .update_queued_turn("b", &q_ids[1], &parts("x").into())
            .unwrap()
    );
    assert!(store.remove_queued_turn("a", &q_ids[4]).unwrap());
    assert_eq!(texts(&store), ["m0", "m1_ed", "m2", "m3"]);

    let front = store.pop_queue_front("a").unwrap().unwrap();
    assert_eq!(front.id, q_ids[0]);
    // A turn put back goes first even into a full queue.
    store.add_queued_turn("a", &parts("m5").into()).unwrap();
    store.add_queued_turn("a", &parts("m6").into()).unwrap();
    assert_eq!(
        store.add_queued_turn("a", &parts("m7").into()).unwrap(),
        None
    );
    store.requeue_front("a", &front.parts.into()).unwrap();
    assert_eq!(texts(&store), ["m0", "m1_ed", "m2", "m3", "m5", "m6"]);
    assert_eq!(store.sessions_with_queue().unwrap(), ["a"]);
    assert_eq!(store.clear_queue("a").unwrap(), 6);
    assert_eq!(store.pop_queue_front("a").unwrap(), None);
    assert!(store.sessions_with_queue().unwrap().is_empty());
}

#[test]
fn queued_content_round_trips_skills_and_contexts() {
    let store = store_with(&["a"]);
    let content = QueuedContent {
        parts: vec![HubPart::text("new")],
        skills: vec!["review".to_owned()],
        contexts: vec![
            serde_json::json!({"kind": "request", "workspace_id": "w", "request_id": "r"}),
        ],
    };
    store.add_queued_turn("a", &content).unwrap().unwrap();
    let queue = store.list_queue("a").unwrap();
    assert_eq!(
        (
            queue[0].parts.clone(),
            queue[0].skills.clone(),
            queue[0].contexts.clone()
        ),
        (content.parts, content.skills, content.contexts)
    );
}

#[test]
fn taking_a_queued_turn_closes_the_gap_and_an_unknown_id_changes_nothing() {
    let store = store_with(&["a"]);
    let ids: Vec<String> = ["m0", "m1", "m2"]
        .iter()
        .map(|text| {
            store
                .add_queued_turn("a", &vec![HubPart::text(*text)].into())
                .unwrap()
                .unwrap()
                .id
        })
        .collect();
    let ords = |store: &WorkspaceStore| -> Vec<(String, u32)> {
        store
            .list_queue("a")
            .unwrap()
            .into_iter()
            .map(|turn| (turn.id, turn.ord))
            .collect()
    };

    let taken = store.take_queued_turn("a", &ids[1]).unwrap().unwrap();
    assert_eq!(taken.parts, [HubPart::text("m1")]);
    assert_eq!(ords(&store), [(ids[0].clone(), 0), (ids[2].clone(), 1)]);
    assert_eq!(store.take_queued_turn("a", "ghost").unwrap(), None);
    assert_eq!(store.take_queued_turn("b", &ids[0]).unwrap(), None);
    assert_eq!(ords(&store), [(ids[0].clone(), 0), (ids[2].clone(), 1)]);
}
