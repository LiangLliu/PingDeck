//! A dependency-free, sandboxed JavaScript runtime for Postman-style
//! pre-request and post-response scripts.
//!
//! The sandbox exposes a practical subset of the Postman `pm.*` API. Scripts
//! can read and mutate the request, read the response, manage variables, make
//! synchronous script requests, record tests, and write to the console. There
//! is no filesystem access, no Node API, and no ambient network access outside
//! the `pm.sendRequest` bridge.

mod body;
mod bridge;
mod compat;
mod cookies;
mod prelude;
mod runtime;

#[cfg(test)]
mod tests;

pub use compat::{CompatIssue, script_compat_issues};
pub use prelude::PRELUDE;
pub use runtime::run_scripts;

use std::{
    collections::{BTreeMap, BTreeSet},
    time::Duration,
};

use serde::{Deserialize, Serialize};

use pingdeck_model::script_result::{ConsoleEntry, ScriptError, ScriptTestResult};

use crate::executor::{AuxiliaryNetwork, ExecutionResult};
use pingdeck_base::cancel::CancelToken;
use pingdeck_model::{
    CookieJarEntry, CookieJarEntryId,
    ai_request::AiScriptResult,
    models::{Environment, EnvironmentVariable, Request},
    settings::Settings,
};

/// Which script event is running.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ScriptPhase {
    PreRequest,
    PostResponse,
}

impl ScriptPhase {
    /// The Postman event name (`prerequest` or `test`) for this phase.
    #[must_use]
    pub fn event_name(self) -> &'static str {
        match self {
            Self::PreRequest => "prerequest",
            Self::PostResponse => "test",
        }
    }
}

/// One script to run, labelled by where it came from (collection, folder, request).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptSource {
    pub label: String,
    pub source: String,
}

/// Metadata exposed to scripts as `pm.info`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ScriptInfo {
    pub request_name: String,
    pub request_id: String,
    pub iteration: u32,
    pub iteration_count: u32,
    pub event_name: String,
}

/// Everything a script phase needs to run.
#[derive(Debug, Clone)]
pub struct ScriptInput {
    pub phase: ScriptPhase,
    pub scripts: Vec<ScriptSource>,
    pub request: Request,
    pub response: Option<ExecutionResult>,
    /// The finished AI run behind `pm.response.ai`; `None` for HTTP requests.
    pub ai: Option<AiScriptResult>,
    pub environment: Option<Environment>,
    /// Cancels in-flight `pm.sendRequest` calls and stops the script at its next bridge call.
    pub cancel: Option<CancelToken>,
    pub collection_variables: Vec<EnvironmentVariable>,
    pub globals: Vec<EnvironmentVariable>,
    pub local: BTreeMap<String, String>,
    pub iteration_data: BTreeMap<String, String>,
    /// The workspace cookie jar the request sends from: `pm.cookies` reads it and
    /// `pm.cookies.jar()` reads and writes it (see `cookies`).
    pub cookie_jar: Vec<CookieJarEntry>,
    pub info: ScriptInfo,
    /// The caller confirmed the send against a guarded environment; `pm.sendRequest` to a method
    /// the environment guards is refused without it.
    pub confirmed: bool,
    /// Sandbox, output and `pm.sendRequest` limits (`advanced`, `data` and `network` settings).
    pub limits: ScriptLimits,
}

/// Limits one script run works under; the defaults are the sandbox's built-in values.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ScriptLimits {
    /// Loop iterations before the engine stops a script.
    pub loop_iteration_limit: u64,
    /// Call depth before the engine stops a script.
    pub recursion_limit: usize,
    /// `pm.sendRequest` calls one script may make.
    pub max_send_requests: u32,
    /// Console entries kept per run.
    pub max_console_entries: usize,
    /// Test results kept per run.
    pub max_test_results: usize,
    /// Bytes kept of one console message, test name or error.
    pub max_message_bytes: usize,
    /// Largest `pm.sendRequest` response body read.
    pub max_response_bytes: usize,
    /// Transport of `pm.sendRequest`.
    pub network: AuxiliaryNetwork,
    /// Wall-clock time one script phase may take before it is abandoned.
    pub wall_clock_limit: Duration,
}

/// Default for [`ScriptLimits::wall_clock_limit`]. Generous because `pm.sendRequest` waits count.
const DEFAULT_WALL_CLOCK_LIMIT: Duration = Duration::from_secs(60);

impl Default for ScriptLimits {
    fn default() -> Self {
        Self::from(&Settings::default())
    }
}

impl From<&Settings> for ScriptLimits {
    fn from(settings: &Settings) -> Self {
        let advanced = &settings.advanced;
        Self {
            loop_iteration_limit: advanced.script_loop_iteration_limit,
            recursion_limit: advanced.script_recursion_limit,
            max_send_requests: advanced.script_max_send_requests,
            max_console_entries: advanced.console_max_entries as usize,
            max_test_results: advanced.max_test_results as usize,
            max_message_bytes: advanced.console_max_message_bytes as usize,
            max_response_bytes: settings.data.response_body_cap(),
            network: AuxiliaryNetwork::from(&settings.network),
            wall_clock_limit: DEFAULT_WALL_CLOCK_LIMIT,
        }
    }
}

/// Variable keys a script set, unset or cleared in one scope.
#[derive(Debug, Clone, Default)]
pub struct ScopeMutations {
    pub keys: BTreeSet<String>,
    pub cleared: bool,
}

/// Variable mutations a script made, per persistent scope.
#[derive(Debug, Clone, Default)]
pub struct ScriptMutations {
    pub environment: ScopeMutations,
    pub collection: ScopeMutations,
    pub globals: ScopeMutations,
}

impl ScriptMutations {
    fn scope_mut(&mut self, scope: &str) -> Option<&mut ScopeMutations> {
        match scope {
            "environment" => Some(&mut self.environment),
            "collection" => Some(&mut self.collection),
            "globals" => Some(&mut self.globals),
            _ => None,
        }
    }
}

/// The request, variables, tests and console as left by a script phase.
#[derive(Debug, Clone)]
pub struct ScriptOutput {
    pub request: Request,
    pub response: Option<ExecutionResult>,
    pub environment: Option<Environment>,
    pub collection_variables: Vec<EnvironmentVariable>,
    pub globals: Vec<EnvironmentVariable>,
    pub mutations: ScriptMutations,
    pub local: BTreeMap<String, String>,
    pub tests: Vec<ScriptTestResult>,
    pub console: Vec<ConsoleEntry>,
    pub error: Option<ScriptError>,
    pub next_request: Option<Option<String>>,
    /// The production guard's message when a `pm.sendRequest` was refused for want of
    /// confirmation; the caller answers it like a refused main send.
    pub confirmation_required: Option<String>,
    /// The cookie jar after `pm.cookies.jar()` writes; the main request sends from it.
    pub cookie_jar: Vec<CookieJarEntry>,
    /// Cookies `pm.cookies.jar().set` stored, for the caller to persist.
    pub cookie_writes: Vec<CookieJarEntry>,
    /// Jar cookies `pm.cookies.jar().unset` or `.clear` removed, for the caller to delete.
    pub cookie_deletes: Vec<CookieJarEntryId>,
}
