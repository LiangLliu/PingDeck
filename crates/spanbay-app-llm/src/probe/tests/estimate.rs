//! `llm_probe_estimate_impl`: the one estimate every surface shows and the run's cost rule.

use super::support::{
    add_model, custom, empty_input, failing, healthy, open_state, quick, run, run_ok, standard,
    upstream,
};
use crate::error::AppError;
use crate::llm::probe::ProbeTest;
use crate::llm::usage::PriceOverride;
use crate::{
    ProbeItem, llm_price_override_save_impl, llm_probe_estimate_impl, llm_probe_runs_list_impl,
};

fn price(model: &str, input: f64, output: f64) -> PriceOverride {
    PriceOverride {
        provider_id: "a".to_owned(),
        model_id: model.to_owned(),
        input_per_mtok: input,
        output_per_mtok: output,
        cache_read_per_mtok: None,
        cache_write_per_mtok: None,
        reasoning_per_mtok: None,
        currency: "USD".to_owned(),
        updated_at_ms: 0,
    }
}

fn names(count: usize) -> Vec<String> {
    (0..count).map(|n| format!("m{n}")).collect()
}

fn refs(names: &[String]) -> Vec<&str> {
    names.iter().map(String::as_str).collect()
}

#[tokio::test(flavor = "multi_thread")]
async fn ten_priced_chat_models_read_about_22k_tokens_and_seven_cents() {
    let state = open_state();
    let a = upstream(&state, "a", healthy).await;
    let names = names(10);
    for name in &names {
        llm_price_override_save_impl(&state, price(name, 1.0, 4.0)).unwrap();
    }
    let estimate = llm_probe_estimate_impl(&state, standard(a.items(&refs(&names))))
        .await
        .unwrap();
    assert_eq!(estimate.summary, "about 22k tokens, ≈ $0.07 for 10 models");
    assert_eq!((estimate.tokens, estimate.models), (22_300, 10));
    assert!(!estimate.needs_confirmation);
    assert_eq!(estimate.unknown_price_models, 0);
}

#[tokio::test(flavor = "multi_thread")]
async fn models_without_a_price_are_named_in_the_line() {
    let state = open_state();
    let a = upstream(&state, "a", healthy).await;
    let estimate = llm_probe_estimate_impl(&state, standard(a.items(&["unpriced-model"])))
        .await
        .unwrap();
    assert_eq!(
        estimate.summary,
        "about 2.2k tokens, price unknown for 1 model"
    );
    assert_eq!(estimate.unknown_price_models, 1);
}

#[tokio::test(flavor = "multi_thread")]
async fn the_refusal_of_a_run_carries_the_same_line_as_the_estimate() {
    let state = open_state();
    let a = upstream(&state, "a", healthy).await;
    let names = names(51);
    let input = custom(a.items(&refs(&names)), &[ProbeTest::Chat]);
    let estimate = llm_probe_estimate_impl(&state, input.clone())
        .await
        .unwrap();
    match run(&state, input).await {
        Err(AppError::ConfirmationRequired { message, .. }) => {
            assert!(message.ends_with(&estimate.summary), "{message}");
        }
        other => panic!("{:?}", other.map(|(summary, _)| summary)),
    }
}

#[tokio::test(flavor = "multi_thread")]
async fn estimating_sends_and_stores_nothing() {
    let state = open_state();
    let a = upstream(&state, "a", healthy).await;
    llm_probe_estimate_impl(&state, standard(a.items(&["a1"])))
        .await
        .unwrap();
    // Only the key's own model list may have been read (queued when the key was saved).
    assert!(a.fake.requests().iter().all(|seen| seen.method == "GET"));
    assert!(
        llm_probe_runs_list_impl(&state, None, None)
            .unwrap()
            .is_empty()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn the_estimate_of_a_retry_counts_only_what_would_rerun() {
    let state = open_state();
    let a = upstream(&state, "a", failing("a2", 403)).await;
    add_model(&state, "a", "a2", true);
    let (first, _) = run_ok(&state, standard(a.items(&["a1", "a2"]))).await;
    let mut input = empty_input();
    input.retry_of = Some(first.run_id);
    let estimate = llm_probe_estimate_impl(&state, input).await.unwrap();
    assert_eq!((estimate.tokens, estimate.models), (2_230, 1));
    assert_eq!(estimate.runnable_cells, 6);
}

#[tokio::test(flavor = "multi_thread")]
async fn estimate_errors_match_the_run_errors() {
    let state = open_state();
    let a = upstream(&state, "a", healthy).await;
    assert!(matches!(
        llm_probe_estimate_impl(&state, standard(Vec::new())).await,
        Err(AppError::Validation { .. })
    ));
    let ghost = vec![ProbeItem {
        key_id: "nope".to_owned(),
        model_id: "m".to_owned(),
    }];
    assert!(matches!(
        llm_probe_estimate_impl(&state, standard(ghost)).await,
        Err(AppError::NotFound { .. })
    ));
    // Aliases are gone: a probe that names one is refused (as the run refuses it).
    let mut by_alias = empty_input();
    by_alias.alias = Some("nope".to_owned());
    assert!(matches!(
        llm_probe_estimate_impl(&state, by_alias).await,
        Err(AppError::Validation { .. })
    ));
    // A run that was never confirmed still estimates; a Quick run of one model is cheap.
    let cheap = llm_probe_estimate_impl(&state, quick(a.items(&["a1"])))
        .await
        .unwrap();
    assert!(!cheap.needs_confirmation);
}
