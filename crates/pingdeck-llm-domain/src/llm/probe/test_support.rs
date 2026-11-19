//! Shared fixtures of the probe tests.

use super::listing::ModelKind;
use super::plan::{CheckSelection, CheckSet, PlanItem};
use super::{ProbeResult, ProbeTest};
use crate::llm::wire::Protocol;

/// A failed result of `test` for key `k`, model `m` on `openai_chat`; everything else default.
pub(crate) fn probe_result(test: ProbeTest) -> ProbeResult {
    ProbeResult {
        key_id: "k".to_owned(),
        model_id: "m".to_owned(),
        endpoint: "openai_chat".to_owned(),
        test: test.as_str().to_owned(),
        ..ProbeResult::default()
    }
}

/// A row of a run.
pub(crate) fn plan_item(
    provider: &str,
    key: &str,
    model: &str,
    kind: ModelKind,
    protocols: &[Protocol],
) -> PlanItem {
    PlanItem {
        provider_id: provider.to_owned(),
        key_id: key.to_owned(),
        model_id: model.to_owned(),
        kind,
        protocols: protocols.to_vec(),
    }
}

/// A chat model of provider `p`, key `k`, speaking OpenAI Chat.
pub(crate) fn chat_item(model: &str) -> PlanItem {
    plan_item("p", "k", model, ModelKind::Chat, &[Protocol::OpenaiChat])
}

/// An embedding model of provider `p`, key `k`, speaking Embeddings.
pub(crate) fn embedding_item(model: &str) -> PlanItem {
    plan_item(
        "p",
        "k",
        model,
        ModelKind::Embedding,
        &[Protocol::Embeddings],
    )
}

/// A named check set as a selection.
pub(crate) fn set(set: CheckSet) -> CheckSelection {
    CheckSelection::Set { set }
}
