//! The plan of a run as it is stored with the run row, and the cells of a run read back.
//!
//! Single responsibility: make a [`ProbePlan`] survive in `llm_probe_runs.summary_json`, so
//! "Retry failed" and "Run remaining N" can rebuild what the run was meant to do, also after an
//! app quit, and turn stored result rows into the planner's input.

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::types::ProbeSummary;
use crate::llm::probe::listing::ModelKind;
use crate::llm::probe::plan::{
    KeyPlan, PlanItem, PlannedCell, PreviousCell, ProbePlan, RowPlan, SkipReason,
};
use crate::llm::probe::run::CellRecord;
use crate::llm::probe::{ProbeStatus, ProbeTest};
use crate::llm::router::RoutingSnapshot;
use crate::llm::store::{ProbeResult, ProbeRunRow};
use crate::llm::wire::Protocol;

/// What `summary_json` holds: the plan from the start of the run, the totals once it ended.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RunDoc {
    /// The plan the run executes.
    #[serde(default)]
    pub spec: RunSpec,
    /// The totals; `None` until the run ended.
    #[serde(default)]
    pub totals: Option<ProbeSummary>,
}

impl RunDoc {
    /// The document of a stored run; a row without one (written before plans were kept) reads
    /// as empty.
    pub(super) fn of(row: &ProbeRunRow) -> Self {
        serde_json::from_value(row.summary.clone()).unwrap_or_default()
    }

    pub(super) fn to_value(&self) -> Value {
        serde_json::to_value(self).unwrap_or(Value::Null)
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SpecItem {
    provider_id: String,
    key_id: String,
    model_id: String,
    kind: ModelKind,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SpecKey {
    provider_id: String,
    key_id: String,
    first_item: usize,
    checks: Vec<ProbeTest>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SpecCell {
    test: ProbeTest,
    #[serde(default)]
    skip: Option<SkipReason>,
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
struct SpecRow {
    item: usize,
    cells: Vec<SpecCell>,
}

/// A serializable [`ProbePlan`] with its rows.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub(super) struct RunSpec {
    items: Vec<SpecItem>,
    keys: Vec<SpecKey>,
    rows: Vec<SpecRow>,
    rate_limited_keys: Vec<(String, String)>,
    /// The chat protocol the run forced.
    #[serde(default)]
    pub protocol: Option<Protocol>,
}

impl RunSpec {
    pub(super) fn new(items: &[PlanItem], plan: &ProbePlan, protocol: Option<Protocol>) -> Self {
        Self {
            items: items
                .iter()
                .map(|item| SpecItem {
                    provider_id: item.provider_id.clone(),
                    key_id: item.key_id.clone(),
                    model_id: item.model_id.clone(),
                    kind: item.kind,
                })
                .collect(),
            keys: plan
                .keys
                .iter()
                .map(|key| SpecKey {
                    provider_id: key.provider_id.clone(),
                    key_id: key.key_id.clone(),
                    first_item: key.first_item,
                    checks: key.checks.clone(),
                })
                .collect(),
            rows: plan
                .rows
                .iter()
                .map(|row| SpecRow {
                    item: row.item,
                    cells: row
                        .cells
                        .iter()
                        .map(|cell| SpecCell {
                            test: cell.test,
                            skip: cell.skip,
                        })
                        .collect(),
                })
                .collect(),
            rate_limited_keys: plan.rate_limited_keys.clone(),
            protocol,
        }
    }

    /// Whether the spec holds a plan at all.
    pub(super) fn is_empty(&self) -> bool {
        self.items.is_empty()
    }

    /// Whether the run's row for (`key_id`, `model_id`) is an embedding model: its decisive
    /// check is Embeddings instead of Chat.
    pub(super) fn is_embedding(&self, key_id: &str, model_id: &str) -> bool {
        self.item_of(key_id, model_id)
            .is_some_and(|index| self.items[index].kind == ModelKind::Embedding)
    }

    /// Whether the run's row for (`key_id`, `model_id`) is a decision model: its decisive check
    /// is Decision instead of Chat.
    pub(super) fn is_decision(&self, key_id: &str, model_id: &str) -> bool {
        let Some(index) = self.item_of(key_id, model_id) else {
            return false;
        };
        self.rows.iter().any(|row| {
            row.item == index
                && row
                    .cells
                    .iter()
                    .any(|cell| cell.test == ProbeTest::Decision)
        })
    }

    /// The plan, as it was made.
    pub(super) fn plan(&self) -> ProbePlan {
        ProbePlan {
            keys: self
                .keys
                .iter()
                .map(|key| KeyPlan {
                    provider_id: key.provider_id.clone(),
                    key_id: key.key_id.clone(),
                    first_item: key.first_item,
                    checks: key.checks.clone(),
                })
                .collect(),
            rows: self
                .rows
                .iter()
                .map(|row| RowPlan {
                    item: row.item,
                    cells: row
                        .cells
                        .iter()
                        .map(|cell| PlannedCell {
                            test: cell.test,
                            skip: cell.skip,
                        })
                        .collect(),
                })
                .collect(),
            rate_limited_keys: self.rate_limited_keys.clone(),
        }
    }

    /// The rows, with the protocols the providers speak now.
    pub(super) fn plan_items(&self, snapshot: &RoutingSnapshot) -> Vec<PlanItem> {
        self.items
            .iter()
            .map(|item| PlanItem {
                provider_id: item.provider_id.clone(),
                key_id: item.key_id.clone(),
                model_id: item.model_id.clone(),
                kind: item.kind,
                protocols: protocols_of(snapshot, &item.provider_id),
            })
            .collect()
    }

    fn item_of(&self, key_id: &str, model_id: &str) -> Option<usize> {
        self.items
            .iter()
            .position(|item| item.key_id == key_id && item.model_id == model_id)
    }

    fn provider_of(&self, key_id: &str, model_id: &str) -> Option<&str> {
        self.item_of(key_id, model_id)
            .map(|index| self.items[index].provider_id.as_str())
    }

    /// The stored cells of the run as the planner reads them. Rows of another run shape (a
    /// model the spec does not know) and rows without a status are ignored.
    pub(super) fn previous_cells(&self, results: &[ProbeResult]) -> Vec<PreviousCell> {
        results
            .iter()
            .filter_map(|result| {
                Some(PreviousCell {
                    item: self.item_of(&result.key_id, &result.model_id)?,
                    test: ProbeTest::parse(&result.test)?,
                    status: ProbeStatus::parse(&result.status_word)?,
                    skip: skip_of(result.skip_reason.as_deref()),
                })
            })
            .collect()
    }

    /// The stored cells of the run as [`CellRecord`]s, for the summary of an interrupted run.
    pub(super) fn recorded_cells(&self, results: &[ProbeResult]) -> Vec<CellRecord> {
        results
            .iter()
            .filter_map(|result| {
                Some(CellRecord {
                    provider_id: self
                        .provider_of(&result.key_id, &result.model_id)?
                        .to_owned(),
                    key_id: result.key_id.clone(),
                    model_id: result.model_id.clone(),
                    test: ProbeTest::parse(&result.test)?,
                    status: ProbeStatus::parse(&result.status_word)?,
                    reason: result.message.clone(),
                    skip: skip_of(result.skip_reason.as_deref()),
                    result: None,
                    capability: None,
                })
            })
            .collect()
    }
}

/// The protocols a provider speaks, empty for a provider that is gone.
pub(super) fn protocols_of(snapshot: &RoutingSnapshot, provider_id: &str) -> Vec<Protocol> {
    snapshot
        .providers
        .get(provider_id)
        .map(|provider| {
            provider
                .endpoints
                .iter()
                .filter(|endpoint| endpoint.enabled)
                .map(|endpoint| endpoint.protocol)
                .collect()
        })
        .unwrap_or_default()
}

fn skip_of(text: Option<&str>) -> Option<SkipReason> {
    serde_json::from_value(Value::String(text?.to_owned())).ok()
}
