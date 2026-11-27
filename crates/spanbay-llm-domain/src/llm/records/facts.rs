//! Pure balance and decisive-probe rules used when assembling provider status facts.

use super::keys::ProviderKey;
use super::models::ProviderModel;
use crate::llm::balance_state::{BalanceErrorKind, BalanceScope};
use crate::llm::presets::ModelRole;
use crate::llm::probe::ProbeTest;
use crate::llm::probe::listing::{ModelKind, model_kind, role_kind, runs_decision};
use crate::llm::providers::Provider;
use crate::llm::status::{BALANCE_EVIDENCE_MS, BalanceSignal};
use crate::llm::wire::Protocol;

/// The balance flags of a provider from its enabled keys at `now_ms`: an exhausted Account-scope
/// reading, how many enabled keys have an exhausted Key-scope quota (only readings younger than
/// a day count: an old "exhausted" says nothing about a balance topped up since), when the newest
/// of those readings was taken, and whether a refresh was rejected.
#[must_use]
pub fn balance_signal(keys: &[&ProviderKey], now_ms: i64) -> BalanceSignal {
    let mut signal = BalanceSignal::default();
    for balance in keys
        .iter()
        .filter(|key| key.enabled)
        .filter_map(|key| key.balance.as_ref())
    {
        if let Some(report) = balance.last_good.as_ref().filter(|report| {
            report.exhausted && now_ms.saturating_sub(report.checked_at_ms) <= BALANCE_EVIDENCE_MS
        }) {
            match report.scope {
                BalanceScope::Account => signal.account_exhausted = true,
                BalanceScope::Key => signal.exhausted_keys += 1,
            }
            signal.exhausted_at_ms = signal.exhausted_at_ms.max(Some(report.checked_at_ms));
        }
        signal.auth_failed |= balance
            .error
            .as_ref()
            .is_some_and(|error| error.kind == BalanceErrorKind::Auth);
    }
    signal
}

/// The check that decides whether `model` answers, the rule the probe planner uses: an embedding
/// model runs Embeddings; a decision model (`probe::listing::runs_decision` over the enabled
/// endpoints) runs Decision; everything else runs Chat.
#[must_use]
pub fn decisive_test(provider: &Provider, model: &ProviderModel) -> ProbeTest {
    // A preset role is only a catalogue mode for embedding models; every other role falls
    // through to the id patterns, as `plan::row_cells` does.
    let mode = (model.role == Some(ModelRole::Embedding)).then_some("embedding");
    let kind = role_kind(model.role).unwrap_or_else(|| model_kind(&model.model_id, mode, None));
    if kind == ModelKind::Embedding {
        return ProbeTest::Embeddings;
    }
    let speaks = |is: fn(Protocol) -> bool| {
        provider
            .endpoints
            .iter()
            .any(|endpoint| endpoint.enabled && is(endpoint.protocol))
    };
    if runs_decision(
        kind,
        &model.model_id,
        speaks(Protocol::is_decision),
        speaks(Protocol::is_chat),
    ) {
        ProbeTest::Decision
    } else {
        ProbeTest::Chat
    }
}
