//! AI API mode use cases: run a request, cancel it, run it on several models, detect an endpoint
//!
//! Single responsibility: glue the HTTP pipeline's pieces (variable resolution, pre-request and
//! test scripts, assertions, the production guard, history) to the AI run core
//! (`llm::ai_run`): only the transport and the result differ from a plain Send. The core fixes
//! URL and credential of a provider target after scripts ran, so a script cannot redirect the
//! injected key. The HTTP pipeline (`execute_request_pipeline`) hands an AI request
//! of a collection run, flow step or draft send to [`run::run_in_pipeline`].

mod compare;
mod compare_frames;
mod detect;
mod history;
mod prepare;
mod run;
#[cfg(test)]
mod tests;

use serde::{Deserialize, Serialize};

use crate::app::dto::SaveRequestInput;
use crate::workspace::ai_request::CompareModel;

pub use compare::{
    AiCompareEstimateDto, AiCompareEvent, AiCompareExportFormat, AiCompareResultDto, AiCompareRow,
    ai_compare_estimate_impl, ai_compare_export_impl, ai_compare_run_impl,
};
pub use detect::{AiDetectInput, ai_detect_impl};
pub(crate) use run::run_in_pipeline;
pub use run::{ai_request_cancel_impl, ai_request_run_impl};

fn keep_by_default() -> bool {
    true
}

fn default_concurrency() -> u8 {
    3
}

/// Input of `ai_request_run`: a saved request or an unsaved draft, run in AI API mode.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiRequestRunInput {
    /// Workspace that owns the request and the environment.
    pub workspace_id: String,
    /// The saved request to run (give this or `request`).
    #[serde(default)]
    pub request_id: Option<String>,
    /// An unsaved draft to run (give this or `request_id`).
    #[serde(default)]
    pub request: Option<SaveRequestInput>,
    /// Environment whose variables apply, and whose guard the call answers to.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// The user confirmed sending into a guarded environment; it covers this call only.
    #[serde(default)]
    pub confirmed: Option<bool>,
    /// "Keep this exchange in the log": a capture is written under every policy.
    #[serde(default = "keep_by_default")]
    pub keep_exchange: bool,
}

/// Input of `ai_compare_run` and `ai_compare_estimate`: one request on several models.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AiCompareRunInput {
    /// Workspace that owns the request and the environment.
    pub workspace_id: String,
    /// The saved request (give this or `request`).
    #[serde(default)]
    pub request_id: Option<String>,
    /// An unsaved draft (give this or `request_id`).
    #[serde(default)]
    pub request: Option<SaveRequestInput>,
    /// Environment whose variables apply, and whose guard the run answers to.
    #[serde(default)]
    pub environment_id: Option<String>,
    /// The user confirmed sending into a guarded environment; it covers the whole run.
    #[serde(default)]
    pub confirmed: Option<bool>,
    /// "Keep this exchange in the log" for every row.
    #[serde(default = "keep_by_default")]
    pub keep_exchange: bool,
    /// The models (at most 12); each runs the same body with only `model` replaced.
    pub models: Vec<CompareModel>,
    /// Calls in flight at once, 1 to 6 (default 3).
    #[serde(default = "default_concurrency")]
    pub concurrency: u8,
}

impl AiCompareRunInput {
    /// The run input of one row of the compare.
    fn single(&self) -> AiRequestRunInput {
        AiRequestRunInput {
            workspace_id: self.workspace_id.clone(),
            request_id: self.request_id.clone(),
            request: self.request.clone(),
            environment_id: self.environment_id.clone(),
            confirmed: self.confirmed,
            keep_exchange: self.keep_exchange,
        }
    }
}
