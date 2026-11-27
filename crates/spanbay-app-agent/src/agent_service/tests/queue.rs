//! Queue use-case tests: the messages a chat holds back behind its active run.

use super::*;
use crate::agent::model::QueuedTurn;
use crate::agent_service::{
    agent_queue_add_impl, agent_queue_clear_impl, agent_queue_list_impl, agent_queue_remove_impl,
    agent_queue_update_impl, agent_session_restore_impl,
};
use crate::llm::hub::HubImage;

fn parts(text: &str) -> Vec<HubPart> {
    vec![HubPart::text(text)]
}

/// Adds behind a held-back queue (as behind a busy run), so nothing starts.
fn add(
    state: &AppState,
    session_id: &str,
    parts: Vec<HubPart>,
) -> Result<Vec<QueuedTurn>, AppError> {
    state.agent.runs.pause_queue(session_id);
    tokio::runtime::Builder::new_current_thread()
        .enable_all()
        .build()
        .expect("runtime")
        .block_on(agent_queue_add_impl(
            state,
            session_id,
            parts,
            Vec::new(),
            &[],
        ))
}

fn update(
    state: &AppState,
    session_id: &str,
    id: &str,
    parts: Vec<HubPart>,
) -> Result<Vec<QueuedTurn>, AppError> {
    agent_queue_update_impl(state, session_id, id, parts, Vec::new(), &[])
}

fn texts(queue: &[QueuedTurn]) -> Vec<String> {
    queue
        .iter()
        .map(|turn| match &turn.parts[0] {
            HubPart::Text { text, .. } => text.clone(),
            other => panic!("text expected, got {other:?}"),
        })
        .collect()
}

#[test]
fn add_returns_the_queue_in_order_and_the_sixth_message_is_a_conflict() {
    let state = state();
    let chat = session(&state, "Queued");
    for i in 1..=5 {
        let queue = add(&state, &chat.id, parts(&format!("m{i}"))).expect("adds");
        assert_eq!(queue.len(), i);
        assert_eq!(
            queue.last().map(|turn| turn.ord),
            Some(u32::try_from(i - 1).expect("small"))
        );
    }
    let error = add(&state, &chat.id, parts("six")).expect_err("full");
    assert!(matches!(&error, AppError::Conflict { .. }));
    assert_eq!(error.message(), "Queue is full (5)");
    assert_eq!(
        texts(&agent_queue_list_impl(&state, &chat.id).expect("list")).len(),
        5
    );
    // Another chat has its own queue.
    let other = session(&state, "Other");
    assert_eq!(add(&state, &other.id, parts("x")).expect("adds").len(), 1);
}

#[test]
fn update_remove_and_clear_return_the_new_queue_and_report_unknown_ids() {
    let state = state();
    let chat = session(&state, "Queued");
    let other = session(&state, "Other");
    let mut queue = Vec::new();
    for text in ["a", "b", "c"] {
        queue = add(&state, &chat.id, parts(text)).expect("adds");
    }
    let foreign = add(&state, &other.id, parts("theirs")).expect("adds");

    let updated = update(&state, &chat.id, &queue[1].id, parts("B!")).expect("updates");
    assert_eq!(texts(&updated), ["a", "B!", "c"]);
    let removed = agent_queue_remove_impl(&state, &chat.id, &queue[0].id).expect("removes");
    assert_eq!(texts(&removed), ["B!", "c"]);
    assert_eq!(
        removed.iter().map(|turn| turn.ord).collect::<Vec<_>>(),
        [0, 1],
        "no gap"
    );

    for result in [
        update(&state, &chat.id, "ghost", parts("x")).map(drop),
        agent_queue_remove_impl(&state, &chat.id, "ghost").map(drop),
        // Another chat's queue item is not reachable through this chat.
        update(&state, &chat.id, &foreign[0].id, parts("x")).map(drop),
        agent_queue_remove_impl(&state, &chat.id, &foreign[0].id).map(drop),
        add(&state, "ghost", parts("x")).map(drop),
        agent_queue_list_impl(&state, "ghost").map(drop),
        agent_queue_clear_impl(&state, "ghost").map(drop),
    ] {
        assert!(
            matches!(result, Err(AppError::NotFound { .. })),
            "{result:?}"
        );
    }
    assert_eq!(
        texts(&agent_queue_list_impl(&state, &other.id).expect("list")),
        ["theirs"]
    );

    assert!(
        agent_queue_clear_impl(&state, &chat.id)
            .expect("clears")
            .is_empty()
    );
    assert!(
        agent_queue_list_impl(&state, &chat.id)
            .expect("list")
            .is_empty()
    );
}

#[test]
fn queued_content_is_validated_like_a_user_message() {
    let state = state();
    let chat = session(&state, "Queued");
    let queue = add(&state, &chat.id, parts("ok")).expect("adds");
    let inline_image = vec![HubPart::Image {
        image: HubImage::Base64 {
            media_type: "image/png".to_owned(),
            data: "AAAA".to_owned(),
        },
    }];
    for bad in [vec![], parts("  "), inline_image] {
        let error = add(&state, &chat.id, bad.clone()).expect_err("rejected");
        assert!(matches!(error, AppError::Validation { .. }));
        let error = update(&state, &chat.id, &queue[0].id, bad).expect_err("rejected");
        assert!(matches!(error, AppError::Validation { .. }));
    }
    assert_eq!(
        texts(&agent_queue_list_impl(&state, &chat.id).expect("list")),
        ["ok"]
    );
    // A reference to an attachment may be queued (a missing one fails when it is sent).
    let with_ref = vec![HubPart::Image {
        image: HubImage::AttachmentRef {
            attachment_id: "att-later".to_owned(),
            media_type: "image/png".to_owned(),
        },
    }];
    assert_eq!(add(&state, &chat.id, with_ref).expect("adds").len(), 2);
}

#[test]
fn a_deleted_chat_keeps_its_queue_for_restore_and_loses_it_on_purge() {
    let state = state();
    let chat = session(&state, "Queued");
    add(&state, &chat.id, parts("waiting")).expect("adds");
    agent_session_delete_impl(&state, &chat.id).expect("deletes");
    assert!(matches!(
        add(&state, &chat.id, parts("x")),
        Err(AppError::NotFound { .. })
    ));
    agent_session_restore_impl(&state, &chat.id).expect("restores");
    assert_eq!(
        texts(&agent_queue_list_impl(&state, &chat.id).expect("list")),
        ["waiting"]
    );

    agent_session_delete_impl(&state, &chat.id).expect("deletes");
    state
        .store()
        .purge_deleted_sessions(i64::MAX)
        .expect("purge");
    assert!(state.store().list_queue(&chat.id).expect("rows").is_empty());
}
