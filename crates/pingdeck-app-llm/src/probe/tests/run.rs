//! `llm_probe_run_impl`: whole runs against fake upstreams, the concurrency limits, Stop,
//! single-run refusal, the cost rule and the "probes never touch routing state" rules.

use std::sync::Arc;

use super::support::{
    add_model, attach, custom, healthy, open_state, quick, run, run_ok, standard, status_of,
    upstream,
};
use crate::error::AppError;
use crate::events::NoEvents;
use crate::llm::capabilities::{Capability, CapabilitySource};
use crate::llm::probe::test_support::{Reply, openai_fake, spawn_fake};
use crate::llm::probe::{ProbeStatus, ProbeTest};
use crate::llm::store::{self, LogFilter};
use crate::llm::wire::Quirks;
use crate::{
    ProbeEvent, ProbeItem, llm_model_list_impl, llm_probe_results_impl, llm_probe_run_get_impl,
    llm_probe_run_impl, llm_probe_runs_list_impl,
};

#[tokio::test(flavor = "multi_thread")]
async fn standard_run_records_cells_capabilities_and_logs_while_isolating_credentials() {
    let state = open_state();
    let mut quirks = Quirks::default();
    quirks
        .extra_headers
        .insert("x-team".to_owned(), "team-secret-123".to_owned());
    let a = attach(&state, "a", spawn_fake(healthy).await, quirks);
    let b = upstream(&state, "b", healthy).await;
    add_model(&state, "a", "a1", true);

    let mut items = a.items(&["a1"]);
    items.extend(b.items(&["b1"]));
    let (summary, events) = run_ok(&state, standard(items)).await;
    assert_eq!((summary.total, summary.passed), (4 + 12, 16));
    assert!(matches!(
        events.events().first(),
        Some(ProbeEvent::RunStarted { total: 16, .. })
    ));
    assert_eq!(
        status_of(&events.cells(), "a1", ProbeTest::Vision),
        ProbeStatus::Ready
    );

    // The run is evidence only: the matrix routing filters on waits for "Apply results".
    let models = llm_model_list_impl(&state, Some("a")).unwrap();
    assert_ne!(
        models[0].capabilities.get(Capability::Vision).source,
        CapabilitySource::Measured
    );
    assert_eq!(
        llm_probe_results_impl(&state, Some(&a.key_id))
            .unwrap()
            .len(),
        8
    );
    assert_eq!(
        llm_probe_run_get_impl(&state, &summary.run_id)
            .unwrap()
            .len(),
        16
    );

    // Extra headers resolved; keys never cross providers; probe logs do not pollute gateway rows.
    assert!(
        a.posts()
            .iter()
            .all(|seen| seen.headers.contains("x-team: team-secret-123")
                && !seen.headers.contains(&b.secret.to_lowercase()))
    );
    assert_eq!(
        store::count_log(
            state.store().connection(),
            &LogFilter {
                source: Some("gateway".to_owned()),
                ..LogFilter::default()
            },
        )
        .unwrap(),
        0
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn single_active_run_stop_cancellation_and_quit_counter_work() {
    let state = open_state();
    let a = upstream(&state, "a", |seen| {
        if seen.method == "POST" {
            Reply::Hang
        } else {
            openai_fake(seen)
        }
    })
    .await;
    let first = {
        let state = Arc::clone(&state);
        let items = a.items(&["a1"]);
        tokio::spawn(async move {
            llm_probe_run_impl(&state, quick(items), Some("stop-1".to_owned()), &NoEvents).await
        })
    };
    while a.posts().is_empty() {
        a.fake.arrived.notified().await;
    }
    assert_eq!(state.active_bench_runs(), 1);
    // A second run waits its turn: it does not start, and stopping it frees its place.
    let queued = {
        let (state, items) = (Arc::clone(&state), a.items(&["a1"]));
        tokio::spawn(async move {
            llm_probe_run_impl(&state, quick(items), Some("stop-2".to_owned()), &NoEvents).await
        })
    };
    while !state.cancellations.cancel("stop-2") {
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        queued.await.unwrap(),
        Err(AppError::Cancelled { .. })
    ));
    assert_eq!(state.active_bench_runs(), 1);
    assert!(state.cancellations.cancel("stop-1"));
    let summary = first.await.unwrap().unwrap();
    assert!(summary.cancelled);
    assert_eq!(state.active_bench_runs(), 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn cost_confirmation_and_input_validation_guard_runs() {
    let state = open_state();
    let a = upstream(&state, "a", healthy).await;
    assert!(matches!(
        run(&state, standard(Vec::new())).await,
        Err(AppError::Validation { .. })
    ));

    let names: Vec<String> = (0..51).map(|n| format!("g{n}")).collect();
    let items: Vec<ProbeItem> = a.items(&names.iter().map(String::as_str).collect::<Vec<_>>());
    assert!(matches!(
        run(&state, custom(items.clone(), &[ProbeTest::Chat])).await,
        Err(AppError::ConfirmationRequired { .. })
    ));
    assert!(
        llm_probe_runs_list_impl(&state, None, None)
            .unwrap()
            .is_empty()
    );

    let mut confirmed = custom(items, &[ProbeTest::Chat]);
    confirmed.confirmed_cost = true;
    let (summary, _) = run_ok(&state, confirmed).await;
    assert_eq!((summary.total, summary.passed), (51, 51));
}
