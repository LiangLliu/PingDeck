//! Wire types of the Test bench use cases: run input, progress events, summaries and DTOs.
//!
//! Single responsibility: the serializable shapes the shell sends and receives. No logic beyond
//! small conversions from the engine's own types.

use serde::{Deserialize, Serialize};

use crate::llm::health::CircuitState;
use crate::llm::probe::apply::{ApplyPlan, ApplySelection};
use crate::llm::probe::estimate::{CurrencyAmount, ProbeEstimate};
use crate::llm::probe::plan::{CheckSelection, CheckSet, SkipReason};
use crate::llm::probe::run::{CellRecord, RunSummary};
use crate::llm::probe::{ProbeStatus, ProbeTest};
use crate::llm::store::{self, ProbeRunRow};
use crate::llm::wire::Protocol;

/// Most (key, model) pairs one probe run accepts.
pub(super) const MAX_PROBE_ITEMS: usize = 500;

/// One (key, model) pair to test. The model id may be any id the provider answers to, a model
/// that is not stored yet (a "From provider" ghost row) included.
#[derive(Debug, Clone, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct ProbeItem {
    /// Key to use.
    pub key_id: String,
    /// Model to test.
    pub model_id: String,
}

fn standard() -> CheckSelection {
    CheckSelection::Set {
        set: CheckSet::Standard,
    }
}

/// Input of a bench run and of its estimate. Exactly one mode applies: a new run over `items`,
/// "Retry failed" (`retry_of`), "Run remaining N" (`remaining_of`) or "Test alias" (`alias`);
/// `items` are read for a new run only, the other modes take their rows from the run or alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeRunInput {
    /// The (key, model) pairs of a new run, 1 to 500.
    #[serde(default)]
    pub items: Vec<ProbeItem>,
    /// What to check: a named set or an explicit list (the default is the Standard set).
    #[serde(default = "standard")]
    pub checks: CheckSelection,
    /// Force this chat protocol instead of the provider's preferred one.
    #[serde(default)]
    pub protocol: Option<Protocol>,
    /// Checks in flight for this run (1 to 16); the setting `probe_concurrency` when absent.
    #[serde(default)]
    pub concurrency: Option<u8>,
    /// Rerun the failed cells of this run as a new, linked run.
    #[serde(default)]
    pub retry_of: Option<String>,
    /// Run the Not tested and Stopped cells of this run as a new, linked run.
    #[serde(default)]
    pub remaining_of: Option<String>,
    /// Test this alias (id or name): the Quick set on each enabled target.
    #[serde(default)]
    pub alias: Option<String>,
    /// The user accepted the cost dialog; without it a run that needs the dialog is
    /// refused and nothing starts.
    #[serde(default)]
    pub confirmed_cost: bool,
}

/// The models of a scan that is not saved yet whose run `llm_probe_estimate` prices: the scan's
/// `report_id` (valid for ten minutes) and the model ids to test, all listed by that scan.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeSniffInput {
    /// `SniffReport.report_id`.
    pub report_id: String,
    /// The selected model ids, 1 to 500.
    pub model_ids: Vec<String>,
}

/// What an alias test found: the target routing would use now and each target's state.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasVerdict {
    /// The alias name.
    pub alias: String,
    /// "Alias would serve now: <model>": the model id, `None` when no target could serve.
    pub serves: Option<String>,
    /// The targets in alias order.
    pub targets: Vec<AliasTargetVerdict>,
}

/// One tested target of an alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasTargetVerdict {
    /// Provider of the target.
    pub provider_id: String,
    /// The key tested.
    pub key_id: String,
    /// The model tested.
    pub model_id: String,
    /// The circuit of the (key, model) pair; a probe never changes it.
    pub circuit: CircuitState,
    /// Status of the target's Chat check in this run.
    pub chat: Option<ProbeStatus>,
}

/// Totals of a finished (or stopped) probe run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeSummary {
    /// The run.
    pub run_id: String,
    /// Cells planned.
    pub total: u32,
    /// Cells finished, skipped ones included.
    pub done: u32,
    /// Cells that passed (Ready).
    pub passed: u32,
    /// Cells Rejected, Unreachable, Invalid response or Timed out.
    pub failed: u32,
    /// Cells Rate limited.
    pub rate_limited: u32,
    /// Cells Unsupported.
    pub unsupported: u32,
    /// Cells Not observed (neither passed nor failed, run again by Retry).
    #[serde(default)]
    pub not_observed: u32,
    /// Cells skipped.
    pub skipped: u32,
    /// Planned cells that never started.
    pub not_tested: u32,
    /// Whether the run was stopped before it finished.
    pub cancelled: bool,
    /// Whether the app quit while the run was going.
    pub interrupted: bool,
    /// Cells or request rows that could not be stored.
    pub persist_errors: u32,
    /// The history note ("Interrupted by quit").
    #[serde(default)]
    pub note: Option<String>,
    /// The verdict of an alias test.
    #[serde(default)]
    pub alias_verdict: Option<AliasVerdict>,
}

impl ProbeSummary {
    pub(super) fn new(run_id: &str, summary: &RunSummary) -> Self {
        Self {
            run_id: run_id.to_owned(),
            total: summary.total,
            done: summary.done,
            passed: summary.ready,
            failed: summary.failed,
            rate_limited: summary.rate_limited,
            unsupported: summary.unsupported,
            not_observed: summary.not_observed,
            skipped: summary.skipped,
            not_tested: summary.not_tested,
            cancelled: summary.cancelled,
            interrupted: summary.interrupted,
            persist_errors: summary.persist_errors,
            note: summary.note().map(str::to_owned),
            alias_verdict: None,
        }
    }
}

/// One finished or skipped cell of the grid.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProbeCellDto {
    /// Provider of the row.
    pub provider_id: String,
    /// Key of the row.
    pub key_id: String,
    /// Model of the row (a key-scoped check carries the key's first model).
    pub model_id: String,
    /// The check.
    pub test: ProbeTest,
    /// The status word.
    pub status: ProbeStatus,
    /// One-line reason; empty for a pass.
    pub reason: String,
    /// Why the cell was skipped.
    pub skip: Option<SkipReason>,
    /// The measurement; `None` for a skipped cell.
    pub result: Option<store::ProbeResult>,
}

impl ProbeCellDto {
    pub(super) fn new(run_id: &str, cell: CellRecord) -> Self {
        let mut result = cell.result;
        if let Some(result) = result.as_mut() {
            result.run_id = Some(run_id.to_owned());
            result.status_word = cell.status.as_str().to_owned();
        }
        Self {
            provider_id: cell.provider_id,
            key_id: cell.key_id,
            model_id: cell.model_id,
            test: cell.test,
            status: cell.status,
            reason: cell.reason,
            skip: cell.skip,
            result,
        }
    }
}

/// Progress of a probe run. The command layer coalesces these to at most 30 per second.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
// Transient push values: boxing would only add an allocation.
#[allow(clippy::large_enum_variant)]
pub enum ProbeEvent {
    /// The run exists and its plan is known.
    RunStarted {
        /// The run.
        run_id: String,
        /// Cells planned.
        total: u32,
        /// The run this one retries or resumes.
        previous_run_id: Option<String>,
    },
    /// One cell finished (or was skipped) and was stored.
    Cell {
        /// The run.
        run_id: String,
        /// The cell.
        cell: ProbeCellDto,
    },
    /// Cells finished so far, sent after every cell.
    Progress {
        /// Cells finished.
        done: u32,
        /// Cells planned.
        total: u32,
    },
    /// The run is over.
    RunFinished {
        /// Totals.
        summary: ProbeSummary,
    },
}

/// The estimate of a run: the numbers and the one string every surface shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeEstimateDto {
    /// Input plus output tokens of every planned check.
    pub tokens: u64,
    /// Models that spend tokens.
    pub models: u32,
    /// Estimated cost per currency.
    pub cost: Vec<CurrencyAmount>,
    /// Models that spend tokens and have no price.
    pub unknown_price_models: u32,
    /// Whether the cost dialog must come first.
    pub needs_confirmation: bool,
    /// "about 22k tokens, ≈ $0.07 for 10 models".
    pub summary: String,
    /// The distinct checks the run would send, for the dialog's list.
    pub checks: Vec<ProbeTest>,
    /// Cells that may send a request.
    pub runnable_cells: u32,
}

impl ProbeEstimateDto {
    pub(super) fn new(estimate: &ProbeEstimate, checks: Vec<ProbeTest>, runnable: usize) -> Self {
        Self {
            summary: estimate.summary(),
            tokens: estimate.tokens,
            models: estimate.models,
            cost: estimate.cost.clone(),
            unknown_price_models: estimate.unknown_price_models,
            needs_confirmation: estimate.needs_confirmation,
            checks,
            runnable_cells: u32::try_from(runnable).unwrap_or(u32::MAX),
        }
    }
}

/// A run in a list, with the line the pane shows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeRunSummaryDto {
    /// Run id.
    pub run_id: String,
    /// The provider the run covered; `None` for a run over several providers.
    pub provider_id: Option<String>,
    /// `quick`, `standard`, `full` or `custom` (a retry or resume keeps the set of its origin).
    pub check_set: String,
    /// When it started, Unix ms.
    pub started_at_ms: i64,
    /// When it ended; `None` while it runs.
    pub finished_at_ms: Option<i64>,
    /// Cells that passed.
    pub passed: u32,
    /// Cells planned.
    pub total: u32,
    /// "36/38 ok".
    pub line: String,
    /// The run it retries or resumes.
    pub previous_run_id: Option<String>,
    /// No end time yet.
    pub running: bool,
    /// Stopped by the user or by quit.
    pub stopped: bool,
    /// Stopped by quit.
    pub interrupted: bool,
    /// Cells that never started: "Run remaining N".
    pub not_tested: u32,
    /// The history note ("Interrupted by quit").
    pub note: Option<String>,
}

impl ProbeRunSummaryDto {
    pub(super) fn new(row: ProbeRunRow, totals: Option<&ProbeSummary>) -> Self {
        Self {
            line: format!("{}/{} ok", row.passed, row.total),
            running: row.finished_at_ms.is_none(),
            stopped: totals.is_some_and(|totals| totals.cancelled),
            interrupted: totals.is_some_and(|totals| totals.interrupted),
            not_tested: totals.map_or(0, |totals| totals.not_tested),
            note: totals.and_then(|totals| totals.note.clone()),
            run_id: row.run_id,
            provider_id: row.provider_id,
            check_set: row.check_set,
            started_at_ms: row.started_at_ms,
            finished_at_ms: row.finished_at_ms,
            passed: row.passed,
            total: row.total,
            previous_run_id: row.previous_run_id,
        }
    }
}

/// A run found unfinished at launch and marked Stopped.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RecoveredRun {
    /// The run.
    pub run_id: String,
    /// Its provider.
    pub provider_id: Option<String>,
    /// Cells left: the shell offers "Run remaining N" for it.
    pub not_tested: u32,
}

/// The preview of "Apply results".
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeApplyPreviewDto {
    /// The run whose results would be applied.
    pub run_id: String,
    /// The provider whose models would change.
    pub provider_id: String,
    /// The provider's display name ("Apply results to relay.acme.dev").
    pub provider_name: String,
    /// When the run started, Unix ms (the title "Apply the results of the 14:32 run").
    pub started_at_ms: i64,
    /// The enable, disable and create lists and the capability facts.
    pub plan: ApplyPlan,
    /// Every row checked as the dialog opens it.
    pub default_selection: ApplySelection,
    /// Why there is nothing to apply ("Nothing to apply, the key failed"); `None` otherwise.
    pub empty_reason: Option<String>,
}

/// What an apply did.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ApplyReportDto {
    /// The id the undo takes; `None` when nothing changed and nothing was logged.
    pub apply_id: Option<String>,
    /// The run applied.
    pub run_id: String,
    /// The provider whose models changed.
    pub provider_id: String,
    /// Models enabled.
    pub enabled: Vec<String>,
    /// Models disabled.
    pub disabled: Vec<String>,
    /// Models created (and enabled).
    pub created: Vec<String>,
    /// When it was applied, Unix ms.
    pub applied_at_ms: i64,
}

/// "Copy as curl" and "Open in API client" of one cell. The key is never in it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeRequestPreviewDto {
    /// The provider called.
    pub provider_id: String,
    /// The model tested.
    pub model_id: String,
    /// The check.
    pub test: ProbeTest,
    /// The command, with `$PROVIDER_API_KEY` for the credential.
    pub curl: String,
    /// Visible warnings about the command.
    pub warnings: Vec<String>,
    /// The unsaved request tab for Requests in AI API mode; `None` for the GET checks
    /// (Connectivity, Auth, Models), which have no AI body.
    pub api_client: Option<crate::llm::capture::ApiClientDraft>,
}

/// The rows an export keeps (the grid's visible rows); an absent list keeps everything.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeExportFilter {
    /// Only these models.
    #[serde(default)]
    pub models: Option<Vec<String>>,
    /// Only these checks.
    #[serde(default)]
    pub tests: Option<Vec<ProbeTest>>,
    /// Only these status words.
    #[serde(default)]
    pub statuses: Option<Vec<ProbeStatus>>,
    /// Only this key.
    #[serde(default)]
    pub key_id: Option<String>,
}

/// The export format.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ProbeExportFormat {
    /// UTF-8 CSV without BOM.
    Csv,
    /// A JSON document.
    Json,
}
