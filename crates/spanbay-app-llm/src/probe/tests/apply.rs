//! Apply results, its preview and its undo.

use std::sync::Arc;

use super::support::{
    Upstream, add_model, healthy, model_of, open_state, quick, run_ok, standard, upstream,
};
use crate::app::AppState;
use crate::error::AppError;
use crate::llm::capabilities::{Capability, CapabilitySource};
use crate::llm::probe::apply::ApplySelection;
use crate::llm::probe::test_support::{Reply, Seen, openai_fake};
use crate::llm::store::ModelSource;
use crate::{
    ProbeSummary, llm_model_list_impl, llm_model_save_impl, llm_probe_apply_impl,
    llm_probe_apply_latest_impl, llm_probe_apply_preview_impl, llm_probe_apply_undo_impl,
};

fn scenario(seen: &Seen) -> Reply {
    let refusal = |status| {
        Reply::json(
            status,
            &serde_json::json!({"error": {"message": "refused"}}),
        )
    };
    if seen.method == "POST" {
        match model_of(seen).as_str() {
            "bad" | "ghost-bad" => return refusal(403),
            "refused" => return refusal(400),
            "ratey" => return refusal(429),
            _ => {}
        }
    }
    openai_fake(seen)
}

const MODELS: [&str; 7] = [
    "dis",
    "bad",
    "ghost",
    "ghost-bad",
    "refused",
    "ratey",
    "fine",
];

async fn applied_scenario() -> (Arc<AppState>, Upstream, ProbeSummary) {
    let state = open_state();
    let a = upstream(&state, "a", scenario).await;
    add_model(&state, "a", "dis", false);
    for model in ["bad", "refused", "ratey", "fine"] {
        add_model(&state, "a", model, true);
    }
    let (summary, _) = run_ok(&state, standard(a.items(&MODELS))).await;
    (state, a, summary)
}

fn flag(state: &AppState, model: &str) -> Option<bool> {
    llm_model_list_impl(state, Some("a"))
        .unwrap()
        .into_iter()
        .find(|m| m.model_id == model)
        .map(|m| m.enabled)
}

#[tokio::test(flavor = "multi_thread")]
async fn preview_apply_and_undo_update_models_and_preserve_manual_edits() {
    let (state, _a, summary) = applied_scenario().await;
    let preview = llm_probe_apply_preview_impl(&state, &summary.run_id).unwrap();
    assert_eq!(preview.plan.enable[0].model_id, "dis");
    assert_eq!(preview.plan.disable[0].model_id, "bad");
    assert_eq!(preview.plan.create[0].model_id, "ghost");

    // A failure alone never takes a model out of routing: disabling is opt-in.
    let mut selection = preview.default_selection.clone();
    assert!(selection.disable.is_empty());
    selection.disable = vec!["bad".to_owned()];
    let report = llm_probe_apply_impl(&state, &summary.run_id, &selection).unwrap();
    assert_eq!(report.enabled, ["dis"]);
    assert_eq!(report.disabled, ["bad"]);
    assert_eq!(report.created, ["ghost"]);
    assert_eq!(flag(&state, "ghost-bad"), None);

    let models = llm_model_list_impl(&state, Some("a")).unwrap();
    let ghost = models.iter().find(|m| m.model_id == "ghost").unwrap();
    assert_eq!(ghost.source, ModelSource::Live);
    assert_eq!(
        ghost.capabilities.get(Capability::Vision).source,
        CapabilitySource::Measured
    );

    // Manual edit on "dis" survives undo; "bad" and "ghost" revert.
    let mut dis = models.into_iter().find(|m| m.model_id == "dis").unwrap();
    dis.enabled = false;
    llm_model_save_impl(&state, dis).unwrap();

    let apply_id = report.apply_id.unwrap();
    assert_eq!(
        llm_probe_apply_latest_impl(&state, Some("a"))
            .unwrap()
            .unwrap()
            .apply_id,
        apply_id
    );
    let undone = llm_probe_apply_undo_impl(&state, &apply_id).unwrap();
    assert_eq!(undone.kept, ["dis"]);
    assert_eq!(flag(&state, "bad"), Some(true));
    assert_eq!(flag(&state, "ghost"), Some(false));
    assert!(
        llm_probe_apply_latest_impl(&state, Some("a"))
            .unwrap()
            .is_none()
    );
}

#[tokio::test(flavor = "multi_thread")]
async fn apply_refuses_multi_provider_or_unknown_runs() {
    let state = open_state();
    let a = upstream(&state, "a", healthy).await;
    let b = upstream(&state, "b", healthy).await;
    let mut items = a.items(&["a1"]);
    items.extend(b.items(&["b1"]));
    let (summary, _) = run_ok(&state, quick(items)).await;
    assert!(matches!(
        llm_probe_apply_preview_impl(&state, &summary.run_id),
        Err(AppError::Validation { .. })
    ));
    assert!(matches!(
        llm_probe_apply_impl(&state, "nope", &ApplySelection::default()),
        Err(AppError::NotFound { .. })
    ));
}
