//! Storage tests for stored collection runs.

use pingdeck_model::runner_item::{CollectionRunStatus, CollectionRunSummary};
use pingdeck_model::{Collection, Workspace};

use super::super::WorkspaceStore;
use crate::storage::collection_runs::COLLECTION_RUN_RETENTION;

fn run(id: &str, collection_id: &str, started_at_ms: i64) -> CollectionRunSummary {
    CollectionRunSummary {
        run_id: id.into(),
        workspace_id: "ws_cr".into(),
        collection_id: collection_id.into(),
        collection_name: "Billing".into(),
        folder_id: None,
        request_ids: None,
        environment_id: Some("env_gone".into()),
        environment_name: Some("Staging".into()),
        started_at_ms,
        finished_at_ms: started_at_ms + 1,
        status: CollectionRunStatus::Done,
        iterations: 1,
        planned: 0,
        total: 0,
        passed: 0,
        failed: 0,
        items: Vec::new(),
    }
}

#[test]
fn collection_runs_keep_twenty_per_collection_scoped_to_workspace() {
    let store = WorkspaceStore::open_in_memory().unwrap();
    store
        .save_workspace(&Workspace::new("ws_cr", "Runs").unwrap())
        .unwrap();
    for id in ["col_a", "col_b"] {
        store
            .save_collection(&Collection::new(id, "ws_cr", id).unwrap())
            .unwrap();
    }
    for n in 1..=25 {
        assert!(
            store
                .save_collection_run(&run(&format!("run_{n}"), "col_a", n))
                .unwrap()
        );
    }
    assert!(
        store
            .save_collection_run(&run("run_b", "col_b", 1))
            .unwrap()
    );

    assert!(
        store
            .get_collection_run("ws_cr", "run_5")
            .unwrap()
            .is_none()
    );
    let newest = store
        .get_collection_run("ws_cr", "run_25")
        .unwrap()
        .unwrap();
    assert_eq!(
        (newest.started_at_ms, newest.status),
        (25, CollectionRunStatus::Done)
    );
    assert!(
        store
            .get_collection_run("ws_cr", "run_6")
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_collection_run("ws_cr", "run_b")
            .unwrap()
            .is_some()
    );
    assert!(
        store
            .get_collection_run("ws_other", "run_25")
            .unwrap()
            .is_none()
    );
    let kept = (1..=25)
        .filter(|n| {
            store
                .get_collection_run("ws_cr", &format!("run_{n}"))
                .unwrap()
                .is_some()
        })
        .count();
    assert_eq!(kept, COLLECTION_RUN_RETENTION);

    let mut gone = run("run_x", "col_missing", 30);
    assert!(!store.save_collection_run(&gone).unwrap());
    gone.collection_id = "col_b".into();
    gone.workspace_id = "ws_other".into();
    assert!(!store.save_collection_run(&gone).unwrap());
}
