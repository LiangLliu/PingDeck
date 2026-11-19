//! Run lists, the "Last run" line, a run's cells and recovery of interrupted runs.

use super::support::{empty_input, failing, healthy, open_state, quick, run_ok, upstream};
use crate::app::AppState;
use crate::{
    RecoveredRun, llm_probe_history_list_impl, llm_probe_last_run_impl, llm_probe_recover_impl,
    llm_probe_results_impl, llm_probe_run_get_impl, llm_probe_runs_list_impl,
};

fn interrupt(state: &AppState, run_id: &str, dropped: &[&str]) {
    let store = state.store();
    let connection = store.connection();
    let mut doc = crate::llm::store::get_probe_run(connection, run_id)
        .unwrap()
        .unwrap()
        .summary;
    doc.as_object_mut().unwrap().remove("totals");
    crate::llm::store::reopen_probe_run(connection, run_id, &doc, dropped).unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn runs_history_and_last_run_query_by_provider_and_limit() {
    let state = open_state();
    let a = upstream(&state, "a", healthy).await;
    let b = upstream(&state, "b", failing("b1", 403)).await;
    let (first, events) = run_ok(&state, quick(a.items(&["a1"]))).await;
    let (second, _) = run_ok(&state, quick(b.items(&["b1"]))).await;

    let runs = llm_probe_runs_list_impl(&state, None, None).unwrap();
    assert_eq!(runs[0].run_id, second.run_id);
    assert_eq!(runs[1].run_id, first.run_id);
    assert_eq!(
        llm_probe_last_run_impl(&state, Some("a"))
            .unwrap()
            .unwrap()
            .line,
        "4/4 ok"
    );
    assert_eq!(
        llm_probe_run_get_impl(&state, &first.run_id).unwrap().len(),
        events.cells().len()
    );
    assert_eq!(
        llm_probe_history_list_impl(&state, &a.key_id, "a1", Some(2))
            .unwrap()
            .len(),
        2
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn interrupted_runs_recover_and_resume_remaining_cells() {
    let state = open_state();
    let a = upstream(&state, "a", healthy).await;
    let (first, _) = run_ok(&state, quick(a.items(&["a1", "a2"]))).await;
    interrupt(&state, &first.run_id, &["stream"]);

    let recovered = llm_probe_recover_impl(&state).unwrap();
    assert_eq!(
        recovered,
        [RecoveredRun {
            run_id: first.run_id.clone(),
            provider_id: Some("a".to_owned()),
            not_tested: 2,
        }]
    );
    let run = &llm_probe_runs_list_impl(&state, None, None).unwrap()[0];
    assert!(run.stopped && run.interrupted && !run.running);
    assert!(llm_probe_recover_impl(&state).unwrap().is_empty());

    let mut input = empty_input();
    input.remaining_of = Some(first.run_id);
    let (second, _) = run_ok(&state, input).await;
    assert_eq!((second.total, second.passed), (2, 2));
    assert_eq!(
        llm_probe_results_impl(&state, Some(&a.key_id))
            .unwrap()
            .len(),
        6
    );
}
