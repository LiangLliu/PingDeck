//! Retry failed, Run remaining and alias tests: the modes that derive their rows from a stored
//! run or from an alias.

use std::sync::Arc;
use std::sync::atomic::{AtomicBool, Ordering};

use super::support::{
    Collect, empty_input, healthy, model_of, open_state, quick, run, run_ok, standard, upstream,
};
use crate::error::AppError;
use crate::llm::probe::test_support::{Reply, Seen, openai_fake};
use crate::{ProbeEvent, llm_probe_results_impl, llm_probe_run_impl, llm_probe_runs_list_impl};

fn switchable(broken: &Arc<AtomicBool>) -> impl Fn(&Seen) -> Reply + Send + Sync + 'static {
    let broken = Arc::clone(broken);
    move |seen| {
        if seen.method == "POST" && model_of(seen) == "a2" && broken.load(Ordering::SeqCst) {
            Reply::json(403, &serde_json::json!({"error": {"message": "no access"}}))
        } else {
            openai_fake(seen)
        }
    }
}

fn retry(run_id: &str) -> crate::ProbeRunInput {
    let mut input = empty_input();
    input.retry_of = Some(run_id.to_owned());
    input
}

fn remaining(run_id: &str) -> crate::ProbeRunInput {
    let mut input = empty_input();
    input.remaining_of = Some(run_id.to_owned());
    input
}

#[tokio::test(flavor = "multi_thread")]
async fn retry_failed_reruns_only_failed_cells_and_links_previous_run() {
    let state = open_state();
    let broken = Arc::new(AtomicBool::new(true));
    let a = upstream(&state, "a", switchable(&broken)).await;
    let (first, _) = run_ok(&state, standard(a.items(&["a1", "a2"]))).await;
    assert_eq!((first.failed, first.skipped), (1, 5));

    broken.store(false, Ordering::SeqCst);
    let gets = a.gets().len();
    let (second, _) = run_ok(&state, retry(&first.run_id)).await;
    assert_eq!((second.total, second.passed), (6, 6));
    assert_eq!(a.gets().len(), gets);
    let runs = llm_probe_runs_list_impl(&state, None, None).unwrap();
    assert_eq!(
        runs[0].previous_run_id.as_deref(),
        Some(first.run_id.as_str())
    );
    assert!(matches!(
        run(&state, retry(&second.run_id)).await,
        Err(AppError::Validation { .. })
    ));
}

#[tokio::test(flavor = "multi_thread")]
async fn stop_then_run_remaining_finishes_untested_cells_and_rejects_aliases() {
    let state = open_state();
    let a = upstream(&state, "a", healthy).await;
    let finished = std::sync::atomic::AtomicU32::new(0);
    let events = Collect::default();
    let first = {
        let sink = |event: ProbeEvent| {
            if matches!(event, ProbeEvent::Cell { .. })
                && finished.fetch_add(1, Ordering::SeqCst) + 1 == 4
            {
                state.cancellations.cancel("stop");
            }
            crate::events::EventSink::emit(&events, event)
        };
        let mut input = quick(a.items(&["m1", "m2", "m3"]));
        input.concurrency = Some(1);
        llm_probe_run_impl(&state, input, Some("stop".to_owned()), &sink)
            .await
            .unwrap()
    };
    assert!(first.cancelled && first.not_tested > 0);

    let (second, _) = run_ok(&state, remaining(&first.run_id)).await;
    assert_eq!(second.passed, second.total);
    let latest = llm_probe_results_impl(&state, Some(&a.key_id)).unwrap();
    assert_eq!(latest.len(), 8);
    assert!(latest.iter().all(|row| row.status_word == "ready"));

    let mut alias_input = empty_input();
    alias_input.alias = Some("smart".to_owned());
    assert!(matches!(
        run(&state, alias_input).await,
        Err(AppError::Validation { .. })
    ));
}
