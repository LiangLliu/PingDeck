//! Check sets, prerequisites and skipping as a pure planner.
//!
//! Single responsibility: turn the rows of a bench run and a check selection into a
//! [`ProbePlan`] (what runs, what is skipped up front and why), and decide at run time, from the
//! outcomes so far, which later checks to skip. "Retry failed" and "Run remaining" plans live in
//! the private `retry` submodule and are re-exported here. No I/O: the orchestrator executes the
//! plan.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use super::listing::{ModelKind, runs_decision};
use super::{ProbeStatus, ProbeTest};
use crate::llm::wire::Protocol;

mod retry;

pub use retry::{PreviousCell, RetryPlan, plan_remaining, plan_retry};

/// A named group of checks.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CheckSet {
    /// Connectivity, Auth, Chat, Stream.
    Quick,
    /// Quick plus Tools, JSON schema, Vision, Reasoning (the default).
    Standard,
    /// Standard plus Cache, Search and, on Messages or Gemini providers, PDF.
    Full,
}

/// What a run checks: a set, or the individual checks the "Checks" popover picked.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum CheckSelection {
    /// One of the named sets.
    Set {
        /// The set.
        set: CheckSet,
    },
    /// An explicit list (Test key runs Connectivity, Auth and Models). [`ProbeTest::Context`] is
    /// ignored: it is an opt-in action with its own warning.
    Custom {
        /// The checks, in any order.
        tests: Vec<ProbeTest>,
    },
}

/// Why a cell is skipped. The label is shown verbatim.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkipReason {
    /// The key's Connectivity check was Unreachable or Timed out.
    ConnectivityFailed,
    /// The key's Auth check was Rejected.
    KeyRejected,
    /// The model's Chat check was Rejected, Unreachable, Invalid response or Timed out.
    ChatFailed,
    /// An embedding model runs only Connectivity, Auth and Embeddings.
    EmbeddingModel,
    /// An image model has no content checks.
    ImageModel,
    /// An audio model has no content checks.
    AudioModel,
    /// A rerank model has no content checks.
    RerankModel,
    /// A moderation model has no content checks.
    ModerationModel,
    /// Any other model that is not a chat model.
    NotAChatModel,
    /// The user stopped the run while the check was in flight.
    Stopped,
}

impl SkipReason {
    /// The reason text of the cell.
    #[must_use]
    pub const fn label(self) -> &'static str {
        match self {
            Self::ConnectivityFailed => "Connectivity failed",
            Self::KeyRejected => "Key rejected",
            Self::ChatFailed => "Chat failed",
            Self::EmbeddingModel => "Embedding model",
            Self::ImageModel => "Image model",
            Self::AudioModel => "Audio model",
            Self::RerankModel => "Rerank model",
            Self::ModerationModel => "Moderation model",
            Self::NotAChatModel => "Not a chat model",
            Self::Stopped => "Stopped",
        }
    }
}

/// One row of the run: a model tested with one key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanItem {
    /// Provider of the key and model.
    pub provider_id: String,
    /// The key used.
    pub key_id: String,
    /// The model tested (any id: a model not yet stored is allowed).
    pub model_id: String,
    /// What the model is for ([`super::listing::model_kind`]).
    pub kind: ModelKind,
    /// Protocols the provider speaks.
    pub protocols: Vec<Protocol>,
}

impl PlanItem {
    /// The key of the row: `(provider_id, key_id)`.
    fn key(&self) -> (&str, &str) {
        (self.provider_id.as_str(), self.key_id.as_str())
    }
}

/// A planned check of one row.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct PlannedCell {
    /// The check.
    pub test: ProbeTest,
    /// `Some` when the cell is skipped before the run starts.
    pub skip: Option<SkipReason>,
}

/// The model-scoped checks of one row, in column order.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RowPlan {
    /// Index of the row's [`PlanItem`].
    pub item: usize,
    /// The row's cells (key-scoped checks are in [`KeyPlan`]).
    pub cells: Vec<PlannedCell>,
}

/// The key-scoped checks, run once per key.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct KeyPlan {
    /// Provider of the key.
    pub provider_id: String,
    /// The key.
    pub key_id: String,
    /// Index of the first [`PlanItem`] of this key: its target carries the key.
    pub first_item: usize,
    /// Connectivity, Auth and Models, in that order, as far as the selection has them.
    pub checks: Vec<ProbeTest>,
}

/// Everything a run will do.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProbePlan {
    /// One entry per distinct key.
    pub keys: Vec<KeyPlan>,
    /// One entry per item, in item order.
    pub rows: Vec<RowPlan>,
    /// Keys (provider id, key id) that run one check at a time from the start of the run: the
    /// keys of the rate-limited cells a retry reruns.
    pub rate_limited_keys: Vec<(String, String)>,
}

impl ProbePlan {
    /// Cells that produce a result event: key-scoped checks plus every row cell (skips too).
    #[must_use]
    pub fn total_cells(&self) -> usize {
        self.keys.iter().map(|key| key.checks.len()).sum::<usize>()
            + self.rows.iter().map(|row| row.cells.len()).sum::<usize>()
    }

    /// Cells that may send a request (not skipped up front).
    #[must_use]
    pub fn runnable_cells(&self) -> usize {
        self.keys.iter().map(|key| key.checks.len()).sum::<usize>()
            + self
                .rows
                .iter()
                .flat_map(|row| &row.cells)
                .filter(|cell| cell.skip.is_none())
                .count()
    }
}

/// Whether the PDF check applies: the provider speaks Messages or Gemini.
fn speaks_documents(protocols: &[Protocol]) -> bool {
    protocols
        .iter()
        .any(|protocol| matches!(protocol, Protocol::AnthropicMessages | Protocol::Gemini))
}

/// The model-scoped checks a named set runs on a chat model (embedding models are handled by
/// [`plan_run`]).
fn set_checks(set: CheckSet, protocols: &[Protocol]) -> Vec<ProbeTest> {
    let mut tests = vec![ProbeTest::Chat, ProbeTest::Stream];
    if set != CheckSet::Quick {
        tests.extend([
            ProbeTest::Tools,
            ProbeTest::JsonSchema,
            ProbeTest::Vision,
            ProbeTest::Reasoning,
        ]);
    }
    if set == CheckSet::Full {
        tests.extend([ProbeTest::Cache, ProbeTest::Search]);
        if speaks_documents(protocols) {
            tests.push(ProbeTest::Pdf);
        }
    }
    tests
}

/// The key-scoped checks of a selection, in run order.
fn key_checks(selection: &CheckSelection) -> Vec<ProbeTest> {
    let wanted = |test: ProbeTest| match selection {
        CheckSelection::Set { .. } => test != ProbeTest::Models,
        CheckSelection::Custom { tests } => tests.contains(&test),
    };
    [ProbeTest::Connectivity, ProbeTest::Auth, ProbeTest::Models]
        .into_iter()
        .filter(|test| wanted(*test))
        .collect()
}

/// One [`KeyPlan`] per distinct key of `items`, in first-seen order; `first_item` is the index
/// of the key's first item and `checks` what `checks` returns for that item.
fn key_plans(items: &[PlanItem], checks: impl Fn(&PlanItem) -> Vec<ProbeTest>) -> Vec<KeyPlan> {
    let mut seen: HashSet<(&str, &str)> = HashSet::new();
    items
        .iter()
        .enumerate()
        .filter(|(_, item)| seen.insert(item.key()))
        .map(|(index, item)| KeyPlan {
            provider_id: item.provider_id.clone(),
            key_id: item.key_id.clone(),
            first_item: index,
            checks: checks(item),
        })
        .collect()
}

/// Whether the row runs the decision probe instead of a named set: a chat model of a provider
/// that speaks no chat protocol, or whose id is a decision model of a provider that speaks one
/// too.
fn runs_decision_probe(item: &PlanItem) -> bool {
    // A relay that speaks chat and decision routes answers chat for a jev model with "TypeSafe
    // models are only available through POST /v1/systemone", so the id decides.
    runs_decision(
        item.kind,
        &item.model_id,
        item.protocols.iter().any(|protocol| protocol.is_decision()),
        item.protocols.iter().any(|protocol| protocol.is_chat()),
    )
}

/// Why a planned `test` of a model of `kind` is skipped up front. Image, audio, rerank and
/// moderation models run only the free key-level checks.
const fn kind_skip(kind: ModelKind, test: ProbeTest) -> Option<SkipReason> {
    match kind {
        ModelKind::Chat => None,
        ModelKind::Embedding => match test {
            ProbeTest::Embeddings => None,
            _ => Some(SkipReason::EmbeddingModel),
        },
        ModelKind::Image => Some(SkipReason::ImageModel),
        ModelKind::Audio => Some(SkipReason::AudioModel),
        ModelKind::Rerank => Some(SkipReason::RerankModel),
        ModelKind::Moderation => Some(SkipReason::ModerationModel),
        ModelKind::Other => Some(SkipReason::NotAChatModel),
    }
}

/// The cells of one row, in column order ([`ProbeTest::ALL`]). A decision model runs the
/// decision probe instead of the named set.
fn row_cells(selection: &CheckSelection, item: &PlanItem) -> Vec<PlannedCell> {
    let mut wanted: Vec<ProbeTest> = match selection {
        CheckSelection::Set { .. } if runs_decision_probe(item) => vec![ProbeTest::Decision],
        CheckSelection::Set { set } => {
            let mut tests = set_checks(*set, &item.protocols);
            if item.kind == ModelKind::Embedding {
                tests.push(ProbeTest::Embeddings);
            }
            tests
        }
        CheckSelection::Custom { tests } => tests
            .iter()
            .copied()
            .filter(|test| {
                !test.is_key_scoped()
                    && *test != ProbeTest::Context
                    && (*test != ProbeTest::Pdf || speaks_documents(&item.protocols))
            })
            .collect(),
    };
    // The derived order of `ProbeTest` is the column order of `ProbeTest::ALL`.
    wanted.sort_unstable();
    wanted.dedup();
    wanted
        .into_iter()
        .map(|test| PlannedCell {
            test,
            skip: kind_skip(item.kind, test),
        })
        .collect()
}

/// Plans a run over `items`: the key-scoped checks once per distinct key, and per row the
/// model-scoped checks of the selection. An embedding model runs only Embeddings; its chat
/// checks are skipped with "Embedding model". A decision model (a provider that speaks no chat
/// protocol, or an id [`super::listing::is_decision_model`] recognises) runs the decision probe
/// instead of a named set. Checks that do not apply to a provider (PDF without Messages or
/// Gemini) are not planned.
#[must_use]
pub fn plan_run(items: &[PlanItem], selection: &CheckSelection) -> ProbePlan {
    let checks = key_checks(selection);
    let keys = key_plans(items, |_| checks.clone());
    let rows = items
        .iter()
        .enumerate()
        .map(|(index, item)| RowPlan {
            item: index,
            cells: row_cells(selection, item),
        })
        .collect();
    ProbePlan {
        keys,
        rows,
        rate_limited_keys: Vec::new(),
    }
}

/// What the key-scoped checks found so far.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct KeyOutcome {
    /// Result of the key's Connectivity check, once known.
    pub connectivity: Option<ProbeStatus>,
    /// Result of the key's Auth check, once known.
    pub auth: Option<ProbeStatus>,
}

/// Why `test` must not run given the key's outcomes: a failed Connectivity (Unreachable or Timed
/// out) skips every other check of the key; a rejected Auth skips every check except
/// Connectivity and Auth.
#[must_use]
pub fn key_gate(test: ProbeTest, key: &KeyOutcome) -> Option<SkipReason> {
    if test == ProbeTest::Connectivity {
        return None;
    }
    if matches!(
        key.connectivity,
        Some(ProbeStatus::Unreachable | ProbeStatus::TimedOut)
    ) {
        return Some(SkipReason::ConnectivityFailed);
    }
    (test != ProbeTest::Auth && key.auth == Some(ProbeStatus::Rejected))
        .then_some(SkipReason::KeyRejected)
}

/// What the model's Chat result means for a later check of the same model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum ChatGate {
    /// Run it.
    Open,
    /// Skip it with this reason.
    Skip(SkipReason),
    /// Leave it "Not tested" (no cell, no event): the Chat check was rate limited, so nothing is
    /// known yet and a later run can take the cell.
    NotTested,
}

/// What the model's Chat result means for `test`: a Rejected, Unreachable, Invalid response or
/// Timed out Chat skips every other chat-family check with "Chat failed"; a Rate limited Chat
/// leaves them Not tested; Unsupported and Ready do not gate.
#[must_use]
pub fn chat_gate(test: ProbeTest, chat: Option<ProbeStatus>) -> ChatGate {
    if !test.is_chat_family() || test == ProbeTest::Chat {
        return ChatGate::Open;
    }
    // Rate limited is a failure too, so it must be matched first.
    match chat {
        Some(ProbeStatus::RateLimited) => ChatGate::NotTested,
        Some(status) if status.is_failure() => ChatGate::Skip(SkipReason::ChatFailed),
        _ => ChatGate::Open,
    }
}

#[cfg(test)]
mod tests;
