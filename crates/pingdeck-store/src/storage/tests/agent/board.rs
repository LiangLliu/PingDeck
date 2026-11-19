//! The run board and the attention feed: lanes, seen marks, unseen runs, pending changes of a run
//! and of abandoned branches, per-currency costs, run extras, the last answer and sub-agent
//! counts.

use super::*;
use pingdeck_model::agent::{CurrencyCost, CurrencyTotal};

/// A top-level run of `session_id` that ended at `finished_at_ms` (`None`: no end recorded).
fn ended(
    id: &str,
    session_id: &str,
    status: AgentRunStatus,
    started_at_ms: i64,
    finished_at_ms: Option<i64>,
) -> AgentRunSummary {
    AgentRunSummary {
        finished_at_ms,
        ..run(id, session_id, status, started_at_ms)
    }
}

/// A sub-agent run of `parent` in `session_id`.
fn child(id: &str, session_id: &str, parent: &str, status: AgentRunStatus) -> AgentRunSummary {
    AgentRunSummary {
        kind: AgentRunKind::Subagent,
        parent_run_id: Some(parent.to_owned()),
        ..run(id, session_id, status, 1)
    }
}

fn insert(store: &WorkspaceStore, runs: &[AgentRunSummary]) {
    for run in runs {
        store.insert_agent_run(run).unwrap();
    }
}

/// A session `id` that `parent` spawned.
fn sub_session(store: &WorkspaceStore, id: &str, parent: &str) {
    let mut sub = session(id, 1);
    sub.parent_session_id = Some(parent.to_owned());
    store.save_agent_session(&sub).unwrap();
}

fn seen_at(store: &WorkspaceStore, run_id: &str) -> Option<i64> {
    store.agent_run_extras(&[run_id.to_owned()]).unwrap()[run_id].seen_at_ms
}

#[test]
fn the_board_lanes_hold_top_level_runs_in_their_own_orders() {
    let store = store_with(&["s"]);
    insert(
        &store,
        &[
            run("q1", "s", AgentRunStatus::Queued, 10),
            run("r1", "s", AgentRunStatus::Running, 20),
            run("w1", "s", AgentRunStatus::WaitingApproval, 30),
            run("w2", "s", AgentRunStatus::WaitingApproval, 5),
            ended("c1", "s", AgentRunStatus::Completed, 1, Some(100)),
            ended("c2", "s", AgentRunStatus::Failed, 50, None),
            ended("done1", "s", AgentRunStatus::Completed, 1, Some(200)),
            ended("done2", "s", AgentRunStatus::Cancelled, 1, Some(150)),
            ended("done3", "s", AgentRunStatus::Completed, 1, Some(180)),
            ended("old", "s", AgentRunStatus::Completed, 1, Some(20)),
        ],
    );
    insert(
        &store,
        &[
            child("sub1", "s", "r1", AgentRunStatus::Running),
            child("sub2", "s", "r1", AgentRunStatus::Completed),
            AgentRunSummary {
                kind: AgentRunKind::Background,
                ..ended("bg", "s", AgentRunStatus::Completed, 1, Some(300))
            },
        ],
    );
    for id in ["done1", "done2", "done3", "old"] {
        assert!(store.mark_agent_run_seen(id, 400).unwrap());
    }

    let board = store.list_board_run_ids(100, 2).unwrap();
    assert_eq!(board.running, ["r1", "q1"]);
    assert_eq!(board.waiting, ["w2", "w1"]);
    // Ordered by end time, or start time while a run has none.
    assert_eq!(board.unseen, ["c1", "c2"]);
    // `old` ended before `since_ms`; `done2` is past `done_limit`.
    assert_eq!(board.done, ["done1", "done3"]);
}

#[test]
fn the_unseen_lane_holds_exactly_the_runs_that_are_not_active() {
    let store = store_with(&["s"]);
    let statuses = [
        AgentRunStatus::Queued,
        AgentRunStatus::Running,
        AgentRunStatus::WaitingApproval,
        AgentRunStatus::Completed,
        AgentRunStatus::Failed,
        AgentRunStatus::Cancelled,
        AgentRunStatus::Interrupted,
    ];
    for status in statuses {
        store
            .insert_agent_run(&run(status.as_str(), "s", status, 1))
            .unwrap();
    }
    let ended_ids: Vec<String> = statuses
        .iter()
        .filter(|status| !status.is_active())
        .map(|status| status.as_str().to_owned())
        .collect();
    let sorted = |mut ids: Vec<String>| {
        ids.sort();
        ids
    };

    let board = store.list_board_run_ids(0, 100).unwrap();
    assert_eq!(sorted(board.unseen), sorted(ended_ids.clone()));
    assert_eq!(
        sorted(
            store
                .list_unseen_agent_runs()
                .unwrap()
                .into_iter()
                .map(|run| run.run_id)
                .collect()
        ),
        sorted(ended_ids.clone())
    );
    assert_eq!(store.mark_agent_runs_seen("s", 5).unwrap(), ended_ids.len());
    let board = store.list_board_run_ids(0, 100).unwrap();
    assert_eq!(sorted(board.done), sorted(ended_ids));
    assert_eq!(board.running.len() + board.waiting.len(), 3);
}

#[test]
fn seen_marks_cover_sub_agent_chats_and_skip_active_runs() {
    let store = store_with(&["s", "other"]);
    sub_session(&store, "sub", "s");
    insert(
        &store,
        &[
            run("a", "s", AgentRunStatus::Completed, 1),
            run("b", "s", AgentRunStatus::Running, 2),
            run("c", "sub", AgentRunStatus::Failed, 3),
            run("d", "other", AgentRunStatus::Completed, 4),
        ],
    );

    assert_eq!(store.mark_agent_runs_seen("s", 9).unwrap(), 2);
    assert_eq!(store.mark_agent_runs_seen("s", 10).unwrap(), 0);
    assert_eq!(
        (
            seen_at(&store, "a"),
            seen_at(&store, "b"),
            seen_at(&store, "c"),
            seen_at(&store, "d")
        ),
        (Some(9), None, Some(9), None)
    );

    assert!(!store.mark_agent_run_seen("b", 11).unwrap());
    assert!(store.mark_agent_run_seen("d", 12).unwrap());
    assert!(!store.mark_agent_run_seen("d", 13).unwrap());
    assert_eq!(seen_at(&store, "d"), Some(12));
}

#[test]
fn unseen_runs_are_ended_top_level_runs_oldest_first() {
    let store = store_with(&["s", "t"]);
    insert(
        &store,
        &[
            run("m2", "s", AgentRunStatus::Completed, 20),
            run("m1", "t", AgentRunStatus::Failed, 10),
            run("active", "s", AgentRunStatus::Running, 1),
            run("seen", "s", AgentRunStatus::Completed, 2),
            child("sub", "s", "m2", AgentRunStatus::Completed),
        ],
    );
    store.mark_agent_run_seen("seen", 3).unwrap();

    let unseen: Vec<(String, String)> = store
        .list_unseen_agent_runs()
        .unwrap()
        .into_iter()
        .map(|run| (run.run_id, run.session_id))
        .collect();
    assert_eq!(
        unseen,
        [
            ("m1".to_owned(), "t".to_owned()),
            ("m2".to_owned(), "s".to_owned())
        ]
    );
}

#[test]
fn pending_changes_count_distinct_files_of_a_run_and_its_sub_agents() {
    let store = store_with(&["s"]);
    insert(
        &store,
        &[
            run("p", "s", AgentRunStatus::Completed, 1),
            run("o", "s", AgentRunStatus::Completed, 1),
            child("c", "s", "p", AgentRunStatus::Completed),
        ],
    );
    let at = |id: &str, run_id: &str, path: &str, status, added, removed| FileChange {
        path: path.to_owned(),
        added,
        removed,
        ..change("s", id, run_id, status)
    };
    for change in [
        at("x1", "p", "x.rs", FileChangeStatus::Pending, 3, 1),
        at("x2", "p", "x.rs", FileChangeStatus::Conflict, 2, 2),
        at("y", "c", "y.rs", FileChangeStatus::Pending, 1, 0),
        at("z", "p", "z.rs", FileChangeStatus::Kept, 9, 9),
        at("w", "o", "w.rs", FileChangeStatus::Pending, 9, 9),
    ] {
        store.insert_file_change(&change).unwrap();
    }

    let pending = store.agent_pending_changes("p").unwrap();
    assert_eq!((pending.files, pending.added, pending.removed), (2, 6, 3));
    assert_eq!(
        store.agent_pending_changes("ghost").unwrap(),
        crate::storage::PendingChanges::default()
    );
}

#[test]
fn off_branch_changes_are_those_of_runs_off_the_active_path_with_their_sub_agents() {
    let store = store_with(&["s"]);
    sub_session(&store, "sub", "s");
    store
        .append_agent_message(&message("s", "u1", "q", 1))
        .unwrap();
    store
        .append_agent_message(&AgentMessage {
            run_id: Some("r1".to_owned()),
            ..assistant("s", "a1", "first", 2)
        })
        .unwrap();
    store.prepare_retry("s", "a1").unwrap();
    store
        .append_agent_message(&AgentMessage {
            run_id: Some("r2".to_owned()),
            ..assistant("s", "a1b", "second", 3)
        })
        .unwrap();
    insert(
        &store,
        &[
            run("r1", "s", AgentRunStatus::Completed, 1),
            run("r2", "s", AgentRunStatus::Completed, 2),
            child("k1", "sub", "r1", AgentRunStatus::Completed),
            child("k2", "sub", "r2", AgentRunStatus::Completed),
        ],
    );
    let at = |session_id: &str, id: &str, run_id: &str, status, created_at_ms| FileChange {
        created_at_ms,
        ..change(session_id, id, run_id, status)
    };
    for change in [
        at("s", "c1", "r1", FileChangeStatus::Pending, 10),
        at("s", "c2", "r2", FileChangeStatus::Pending, 30),
        at("sub", "c3", "k1", FileChangeStatus::Conflict, 20),
        at("sub", "c4", "k2", FileChangeStatus::Pending, 40),
        at("s", "c5", "r1", FileChangeStatus::Kept, 50),
    ] {
        store.insert_file_change(&change).unwrap();
    }

    assert_eq!(
        store.off_branch_pending_change_ids("s").unwrap(),
        ["c3", "c1"]
    );
    store.set_head("s", "a1").unwrap();
    assert_eq!(
        store.off_branch_pending_change_ids("s").unwrap(),
        ["c4", "c2"]
    );
}

#[test]
fn costs_group_by_currency_and_a_run_without_one_counts_as_usd() {
    let store = store_with(&["s", "o"]);
    sub_session(&store, "sub", "s");
    let priced = |id: &str, session_id: &str, started: i64, cost: i64, currency: Option<&str>| {
        AgentRunSummary {
            cost_micro_usd: cost,
            cost_currency: currency.map(str::to_owned),
            ..run(id, session_id, AgentRunStatus::Completed, started)
        }
    };
    insert(
        &store,
        &[
            priced("a", "s", 10, 100, None),
            priced("b", "s", 20, 50, Some("USD")),
            priced("c", "s", 30, 7, Some("CNY")),
            priced("z", "s", 40, 0, Some("EUR")),
            priced("o1", "o", 50, 1000, Some("USD")),
        ],
    );
    insert(
        &store,
        &[AgentRunSummary {
            kind: AgentRunKind::Subagent,
            parent_run_id: Some("a".to_owned()),
            ..priced("k", "sub", 5, 5, Some("CNY"))
        }],
    );

    let total = |currency: &str, cost_micro| CurrencyTotal {
        currency: currency.to_owned(),
        cost_micro,
    };
    assert_eq!(
        store.agent_cost_since(10).unwrap(),
        [total("CNY", 7), total("USD", 1150)]
    );
    let cost = |currency: &str, cost_micro| CurrencyCost {
        currency: currency.to_owned(),
        cost_micro,
    };
    assert_eq!(
        store.agent_session_cost_by_currency("s").unwrap(),
        [cost("CNY", 12), cost("USD", 150)]
    );
}

#[test]
fn run_extras_keep_a_known_currency_and_a_timed_out_approval() {
    let store = store_with(&["s"]);
    store
        .insert_agent_run(&run("r", "s", AgentRunStatus::Running, 1))
        .unwrap();
    let extras =
        |store: &WorkspaceStore| store.agent_run_extras(&["r".to_owned()]).unwrap()["r"].clone();

    store
        .set_agent_run_extras("r", Some("CNY"), 10, true)
        .unwrap();
    store.set_agent_run_extras("r", None, 20, false).unwrap();
    let kept = extras(&store);
    assert_eq!(
        (
            kept.cost_currency.as_deref(),
            kept.awake_ms,
            kept.approval_timed_out
        ),
        (Some("CNY"), 20, true)
    );

    store
        .set_agent_run_extras("r", Some("USD"), u64::MAX, false)
        .unwrap();
    let saturated = extras(&store);
    assert_eq!(
        (saturated.cost_currency.as_deref(), saturated.awake_ms),
        (Some("USD"), u64::try_from(i64::MAX).unwrap())
    );
    assert!(
        store
            .agent_run_extras(&["ghost".to_owned()])
            .unwrap()
            .is_empty()
    );
}

#[test]
fn the_last_answer_skips_unfinished_rows_and_answers_without_text() {
    let store = store_with(&["s"]);
    let of_run = |message: AgentMessage| AgentMessage {
        run_id: Some("r".to_owned()),
        ..message
    };
    store
        .append_agent_message(&of_run(message("s", "u1", "question", 1)))
        .unwrap();
    store
        .append_agent_message(&of_run(assistant("s", "a1", "\nFirst answer\nmore", 2)))
        .unwrap();
    store
        .append_agent_message(&of_run(assistant_calling("s", "a2", &[("c1", "t")], 3)))
        .unwrap();
    store
        .upsert_partial_assistant("s", "r", "a3", vec![HubPart::text("streaming")])
        .unwrap();

    assert_eq!(
        store.agent_run_last_answer("r", 100).unwrap().as_deref(),
        Some("First answer")
    );
    assert_eq!(store.agent_run_last_answer("ghost", 100).unwrap(), None);
}

#[test]
fn sub_agent_counts_are_per_parent_run() {
    let store = store_with(&["s"]);
    insert(
        &store,
        &[
            run("p1", "s", AgentRunStatus::Completed, 1),
            run("p2", "s", AgentRunStatus::Completed, 1),
            run("p3", "s", AgentRunStatus::Completed, 1),
        ],
    );
    insert(
        &store,
        &[
            child("c1", "s", "p1", AgentRunStatus::Completed),
            child("c2", "s", "p1", AgentRunStatus::Failed),
            child("c3", "s", "p2", AgentRunStatus::Completed),
        ],
    );

    let counts = store
        .agent_subagent_counts(&["p1".to_owned(), "p2".to_owned(), "p3".to_owned()])
        .unwrap();
    assert_eq!(
        counts,
        std::collections::HashMap::from([("p1".to_owned(), 2), ("p2".to_owned(), 1)])
    );
}
