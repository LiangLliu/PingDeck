//! Test bench use cases: estimate and run (new, retry, remaining, alias), stored runs and
//! history, apply and undo, request preview, export and the context-test warning.
//!
//! Single responsibility: the UI-agnostic entry points of the bench over the engine
//! (`llm::probe`) and the store (`llm::store`). One run is active at a time; probes never touch
//! the circuit breaker or the health book. Nothing here holds the store lock across an `.await`;
//! the submodules split by use case (`run`, `estimate`, `history`, `apply`, `inspect`), `plan`
//! and `spec` are what the run and the estimate share.

mod apply;
mod context;
mod estimate;
mod history;
mod inspect;
mod plan;
mod run;
mod spec;
#[cfg(test)]
mod tests;
mod types;

pub use crate::llm::probe::ExportFile;
pub use apply::{
    llm_probe_apply_impl, llm_probe_apply_latest_impl, llm_probe_apply_preview_impl,
    llm_probe_apply_undo_impl,
};
pub use context::llm_context_probe_run_impl;
pub use estimate::{llm_probe_estimate_impl, llm_probe_estimate_sniff_impl};
pub use history::{
    llm_probe_history_list_impl, llm_probe_last_run_impl, llm_probe_recover_impl,
    llm_probe_results_impl, llm_probe_run_get_impl, llm_probe_runs_list_impl,
};
pub use inspect::{
    llm_context_probe_info_impl, llm_probe_export_impl, llm_probe_request_preview_impl,
};
pub use run::llm_probe_run_impl;
pub use types::{
    AliasTargetVerdict, AliasVerdict, ApplyReportDto, ProbeApplyPreviewDto, ProbeCellDto,
    ProbeEstimateDto, ProbeEvent, ProbeExportFilter, ProbeExportFormat, ProbeItem,
    ProbeRequestPreviewDto, ProbeRunInput, ProbeRunSummaryDto, ProbeSniffInput, ProbeSummary,
    RecoveredRun,
};
