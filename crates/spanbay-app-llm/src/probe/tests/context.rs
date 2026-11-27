//! `llm_context_probe_run_impl`: the acknowledged, persisted, stoppable context-window test.

use std::sync::Arc;

use serde_json::json;

use super::support::{Collect, healthy, open_state, quick, upstream};
use crate::app::AppState;
use crate::error::AppError;
use crate::events::NoEvents;
use crate::llm::probe::test_support::{Reply, Seen, oa_message};
use crate::llm::probe::{ProbeStatus, ProbeTest};
use crate::llm::store::{FactSource, ModelSource, ProviderModel};
use crate::{
    llm_context_probe_run_impl, llm_model_list_impl, llm_model_save_impl, llm_probe_run_impl,
    llm_probe_runs_list_impl,
};

fn fails_over(limit: usize) -> impl Fn(&Seen) -> Reply + Send + Sync + 'static {
    move |seen| {
        if seen.method != "POST" {
            return healthy(seen);
        }
        if seen.body.len() > limit {
            Reply::json(
                400,
                &json!({"error": {"message": "context length exceeded"}}),
            )
        } else {
            oa_message(
                &json!({"role": "assistant", "content": "OK"}),
                "stop",
                &json!({"prompt_tokens": seen.body.len() / 4, "completion_tokens": 1}),
            )
        }
    }
}

fn model_with_context(state: &AppState, model_id: &str, context: u64) {
    llm_model_save_impl(
        state,
        ProviderModel {
            provider_id: "a".to_owned(),
            model_id: model_id.to_owned(),
            source: ModelSource::Live,
            context: Some(context),
            ..ProviderModel::default()
        },
    )
    .unwrap();
}

#[tokio::test(flavor = "multi_thread")]
async fn context_probe_requires_acknowledgment_and_persists_measured_window() {
    let state = open_state();
    let a = upstream(&state, "a", fails_over(100_000)).await;
    model_with_context(&state, "big", 200_000);

    let refused =
        llm_context_probe_run_impl(&state, &a.key_id, "big", false, None, &NoEvents).await;
    assert!(matches!(refused, Err(AppError::Validation { .. })));
    // Only the key's own model list may have been read (queued when the key was saved).
    assert!(a.fake.requests().iter().all(|seen| seen.method == "GET"));

    let events = Collect::default();
    let result = llm_context_probe_run_impl(&state, &a.key_id, "big", true, None, &events)
        .await
        .unwrap();
    assert!(result.ok);
    assert_eq!(result.detail["max_ok_tokens"], 8000);
    // A step failed: the ladder writes nothing back, the listed window stays.
    let stored = &llm_model_list_impl(&state, Some("a")).unwrap()[0];
    assert_eq!(
        (
            stored.context,
            stored.context_source,
            stored.measured_context
        ),
        (Some(200_000), Some(FactSource::Listed), None)
    );
    let cells = events.cells();
    assert_eq!(
        (cells.len(), cells[0].test, cells[0].status),
        (1, ProbeTest::Context, ProbeStatus::Ready)
    );

    // Every step passed: the confirmed window beats the listed one.
    model_with_context(&state, "small", 8_000);
    llm_context_probe_run_impl(&state, &a.key_id, "small", true, None, &NoEvents)
        .await
        .unwrap();
    let small = llm_model_list_impl(&state, Some("a"))
        .unwrap()
        .into_iter()
        .find(|m| m.model_id == "small")
        .unwrap();
    assert_eq!(small.context_source, Some(FactSource::Measured));
}

#[tokio::test(flavor = "multi_thread")]
async fn stopping_context_probe_cancels_cleanly_and_releases_active_slot() {
    let state = open_state();
    let a = upstream(&state, "a", |seen| {
        if seen.method == "POST" {
            Reply::Hang
        } else {
            healthy(seen)
        }
    })
    .await;
    model_with_context(&state, "big", 200_000);
    let handle = {
        let (state, key) = (Arc::clone(&state), a.key_id.clone());
        tokio::spawn(async move {
            llm_context_probe_run_impl(&state, &key, "big", true, Some("ctx".to_owned()), &NoEvents)
                .await
        })
    };
    while a.posts().is_empty() {
        a.fake.arrived.notified().await;
    }
    // A run started meanwhile waits its turn and gives up its place when stopped.
    let queued = {
        let (state, items) = (Arc::clone(&state), a.items(&["big"]));
        tokio::spawn(async move {
            llm_probe_run_impl(&state, quick(items), Some("queued".to_owned()), &NoEvents).await
        })
    };
    while !state.cancellations.cancel("queued") {
        tokio::task::yield_now().await;
    }
    assert!(matches!(
        queued.await.unwrap(),
        Err(AppError::Cancelled { .. })
    ));
    assert!(state.cancellations.cancel("ctx"));
    assert!(matches!(
        handle.await.unwrap(),
        Err(AppError::Cancelled { .. })
    ));
    let runs = llm_probe_runs_list_impl(&state, None, None).unwrap();
    assert!(runs[0].stopped && !runs[0].running);
}
