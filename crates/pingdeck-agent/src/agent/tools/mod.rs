//! What every agent tool, built-in or MCP, looks like to the runner: the [`AgentTool`] trait, the
//! [`ToolContext`] a call receives, its [`ToolOutput`] and the [`ToolRegistry`] a session resolves.
//!
//! Single responsibility: the contract between the run loop and the tools. Built-in tools live in
//! the submodules (`fs`, `shell`, `process`, `web`, `skill`, `todo`, `subagent`); MCP tools are
//! adapted in `agent::mcp`. The runner knows nothing about any tool's inner workings: it asks
//! [`AgentTool::inspect`] for a classification (read-only, destructive, protected, preview), lets
//! `agent::permissions` decide, then awaits [`AgentTool::call`].

use std::path::Path;
use std::sync::Arc;

use futures_util::future::BoxFuture;
use serde::{Deserialize, Serialize};
use serde_json::Value;

use pingdeck_base::cancel::CancelToken;
use pingdeck_base::error::AppError;
use pingdeck_llm::llm::hub::HubPart;
use pingdeck_model::settings::Settings;

use super::sandbox::SandboxKind;

pub mod ask_user;
pub mod fs;
pub mod fs_safe;
pub mod process;
pub mod shell_meta;
pub mod submit_plan;

pub use fs::ReadHashes;
pub use pingdeck_model::agent::tools::{
    FileChangeKind, OutputStream, ToolApprovalPreview, ToolClass, ToolResultMeta, ToolResultView,
    ToolSource,
};
pub use process::{BackgroundProcess, ProcessRegistry, ProcessState};
pub use shell_meta::ToolOutputChunk;
/// A tool the model may call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ToolSpec {
    /// Unique function name sent to the model (MCP tools are `mcp__<server name>__<tool>`).
    pub name: String,
    /// What the tool does, for the model.
    pub description: String,
    /// JSON Schema of the arguments object.
    pub input_schema: Value,
    /// The tool only reads (MCP `readOnlyHint`); drives `auto_read_only` approval.
    pub read_only: bool,
    /// The tool may destroy data (MCP `destructiveHint`).
    pub destructive: bool,
    /// Where the tool comes from.
    pub source: ToolSource,
    /// The capability-matrix row of the tool.
    pub class: ToolClass,
    /// Consecutive parallel-safe calls of one assistant turn may run concurrently:
    /// true only for reads and network reads.
    pub parallel_safe: bool,
}
/// The outcome of one tool call: content parts for the model (text, images, documents),
/// whether the tool failed, and the structured hint the UI reads.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolOutput {
    /// Result content, already size-capped by the tool.
    pub parts: Vec<HubPart>,
    /// The tool reported a failure.
    pub is_error: bool,
    /// The UI's link-out for this result; `None` when the result has none.
    pub meta: Option<ToolResultMeta>,
    /// The text comes from outside the app (a web page, search hits, an MCP server, a response
    /// body, a sub-agent's report): the runner stores it inside an `<external_data>` envelope
    /// ([`envelope_parts`]). The tool has already passed it through
    /// [`crate::agent::untrusted::sanitize_untrusted`].
    pub external: bool,
    /// The most bytes of the text the model sees before the `read_output` handle, when the tool
    /// wants less than the runner's cap (`get_request` shows 8 KiB of a body); `None` keeps it.
    pub cap: Option<usize>,
    /// The exit status of the command a tool ran (`shell`), when the process ended with one; the
    /// audit row records it (`agent_tool_calls.exit_code`). A signal, a timeout and a failure to
    /// start have none.
    pub exit_code: Option<i32>,
}

impl ToolOutput {
    /// A successful text result.
    #[must_use]
    pub fn text(text: impl Into<String>) -> Self {
        Self {
            parts: vec![HubPart::text(text)],
            is_error: false,
            meta: None,
            external: false,
            cap: None,
            exit_code: None,
        }
    }

    /// The same result with its UI link-out (`meta`).
    #[must_use]
    pub fn with_meta(mut self, meta: ToolResultMeta) -> Self {
        self.meta = Some(meta);
        self
    }

    /// A failed call described by `text` (the model reads it and may retry differently).
    #[must_use]
    pub fn error(text: impl Into<String>) -> Self {
        Self {
            parts: vec![HubPart::text(text)],
            is_error: true,
            meta: None,
            external: false,
            cap: None,
            exit_code: None,
        }
    }

    /// The same result with the exit status of the process behind it.
    #[must_use]
    pub fn with_exit_code(mut self, code: Option<i32>) -> Self {
        self.exit_code = code;
        self
    }

    /// The same result marked as external data (see [`ToolOutput::external`]).
    #[must_use]
    pub fn external(mut self) -> Self {
        self.external = true;
        self
    }

    /// The same result cut for the model at `bytes` at most (see [`ToolOutput::cap`]).
    #[must_use]
    pub fn capped(mut self, bytes: usize) -> Self {
        self.cap = Some(bytes);
        self
    }

    /// The text parts joined (for events, logs and tests).
    #[must_use]
    pub fn text_content(&self) -> String {
        pingdeck_llm::llm::hub::text_of(&self.parts)
    }
}
/// `parts` of a result of `source` inside one `<external_data>` envelope: the opening tag starts
/// the first text part and the closing tag ends the last one (a result without text gets one).
#[must_use]
pub fn envelope_parts(source: &str, mut parts: Vec<HubPart>) -> Vec<HubPart> {
    let (open, close) = crate::agent::untrusted::envelope_tags(source);
    let texts: Vec<usize> = parts
        .iter()
        .enumerate()
        .filter(|(_, part)| matches!(part, HubPart::Text { .. }))
        .map(|(index, _)| index)
        .collect();
    let (Some(&first), Some(&last)) = (texts.first(), texts.last()) else {
        parts.insert(0, HubPart::text(format!("{open}\n{close}")));
        return parts;
    };
    if let Some(HubPart::Text { text, .. }) = parts.get_mut(first) {
        *text = format!("{open}\n{text}");
    }
    if let Some(HubPart::Text { text, .. }) = parts.get_mut(last) {
        text.push('\n');
        text.push_str(&close);
    }
    parts
}

/// A tool's classification of one concrete call, computed from its arguments before approval.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct CallInspection {
    /// This call only reads (drives `auto_read_only` approval).
    pub read_only: bool,
    /// This call may destroy data.
    pub destructive: bool,
    /// What the approval card shows.
    pub preview: Option<ToolApprovalPreview>,
    /// Layer 1 of the permission decision: when set, the call is refused in every mode without a
    /// card and the text says which rule (a path outside the Folder, a write under `.git/`, a
    /// secret value in a URL). It ends as `blocked`.
    pub hard_block: Option<String>,
    /// When set, the call needs an explicit approval even under `auto_all` (a protected path, a
    /// Production environment, a probe cost); the text says why. `shell` without a usable sandbox
    /// is the permission decision's own forced Ask, not set here.
    pub always_ask: Option<String>,
    /// A stable code of `always_ask` (`protected_path`, `production`, `mcp_untrusted`, …) for the
    /// approval card and the audit (`forced:<code>`); `None` reads as `tool`.
    pub ask_code: Option<&'static str>,
    /// The approval can only ever be answered "once" (no stored rule, no chat grant): an MCP tool
    /// of a server that is not trusted.
    pub once_only: bool,
    /// A value rule patterns (`arg_pattern`) are matched against: the command line of a shell
    /// call, the workspace-relative path of a file call, the URL of a fetch.
    pub arg_subject: Option<String>,
    /// The key of the tool's per-tool policy when it is not the function name: an MCP tool is
    /// `mcp:<server id>:<server-side tool name>`, so renaming the server keeps its policies.
    pub policy_key: Option<String>,
}
/// Receives live output fragments of a running tool (`ToolOutputDelta` events).
pub type OutputSink<'a> = &'a (dyn Fn(OutputStream, &str) + Send + Sync);
/// Something a tool wants the run to publish besides its result; the runner maps it onto
/// `AgentEvent`s.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum ToolEvent {
    /// A file tool changed a file.
    FileChanged {
        /// Workspace-relative path.
        path: String,
        /// Created or modified.
        kind: FileChangeKind,
        /// Lines added.
        added: u32,
        /// Lines removed.
        removed: u32,
    },
    /// A short note for the user (for example "Output truncated").
    Notice(String),
}

/// Receives the [`ToolEvent`]s of a running tool.
pub type ToolEventSink<'a> = &'a (dyn Fn(ToolEvent) + Send + Sync);

/// Everything a tool call may use, borrowed from the run for the duration of the call.
pub struct ToolContext<'a> {
    /// The application handle, type-erased: the tools that need the whole `AppState` (they live
    /// in `pingdeck-app`) downcast it. This crate never names a crate above itself.
    pub host: Option<&'a (dyn std::any::Any + Send + Sync)>,
    /// The agent runtime (live MCP connections, runs). Same crate, so it is typed.
    pub agent: Option<&'a crate::agent::runtime::AgentRuntime>,
    /// The data directory root (`<data>/`), for tools that spill files there.
    pub data_root: Option<&'a Path>,
    /// The settings in force for this run (one snapshot per run).
    pub settings: &'a Settings,
    /// The session the call belongs to.
    pub session_id: &'a str,
    /// The run the call belongs to.
    pub run_id: &'a str,
    /// The model's id of this call.
    pub call_id: &'a str,
    /// The session's bound folder; `None` disables the local workspace tools.
    pub workspace_dir: Option<&'a Path>,
    /// The OS sandbox available to a shell call.
    pub sandbox: SandboxKind,
    /// Fires when the run is cancelled; a call must stop promptly (a shell kills its process
    /// group).
    pub cancel: &'a CancelToken,
    /// Live output of the call, if the shell asked for it.
    pub output: Option<OutputSink<'a>>,
    /// Receives file-change and notice events of the call.
    pub events: Option<ToolEventSink<'a>>,
    /// The background processes of the app (`shell` with `background`, `shell_output`,
    /// `shell_kill`).
    pub processes: &'a ProcessRegistry,
    /// What the session's file tools last read, for the read-before-edit guard.
    pub read_hashes: &'a ReadHashes,
    /// Where full tool output is spilled (`<data_dir>/tool-output`); `None` keeps only the capped
    /// text.
    pub data_tool_output_dir: Option<&'a Path>,
    /// This call's shell may use the network: the permission mode allows it
    /// ([`crate::agent::permissions::shell_network_allowed`]).
    pub shell_network: bool,
    /// The chat is in Full access: the shell's sandbox profile also lets it write the toolchain
    /// caches (every read-deny still applies).
    pub shell_full_access: bool,
    /// A call with `sandbox` = [`SandboxKind::None`] may run: the chat acknowledged "Run without a
    /// sandbox", or the call's approval card already said that no sandbox applies. `shell`
    /// refuses to run unsandboxed without it (the sandbox state changed between the permission
    /// decision and the run, so no card covered this).
    pub unsandboxed_approved: bool,
    /// The most bytes of a result the model sees whole (the runner cuts and keeps the rest).
    pub result_cap: usize,
}

/// What a test needs next to the `AppState`, owned so a [`ToolContext`] can borrow it.
#[derive(Default)]
pub struct TestParts {
    /// Background processes.
    pub processes: ProcessRegistry,
    /// Read hashes.
    pub read_hashes: ReadHashes,
}

impl ToolContext<'_> {
    /// Counts `micro` of `currency` a tool spent outside the model calls (a probe's real
    /// requests) against the cost cap of the call's run tree, like a model call's cost: the run,
    /// its parent while it is a sub-agent, and the root's per-part ledger. Nothing happens
    /// without a runtime or for a run that already ended.
    pub fn charge_run(&self, currency: &str, micro: i64) {
        let (Some(agent), true) = (self.agent, micro > 0) else {
            return;
        };
        let Some(run) = agent.runs.get(self.run_id) else {
            return;
        };
        run.add_cost(currency, micro);
        let parent = run
            .parent_run_id
            .as_deref()
            .and_then(|id| agent.runs.get(id));
        if let Some(parent) = &parent {
            parent.add_cost(currency, micro);
        }
        let part = if parent.is_some() { "subagent" } else { "main" };
        parent
            .as_ref()
            .unwrap_or(&run)
            .note_call(part, Some((currency, micro)), "", 0);
    }
}

impl<'a> ToolContext<'a> {
    /// A context for session `s`, run `r`, call `c` without a workspace, sandbox, sinks or
    /// network; tests set the fields they exercise.
    pub fn for_test(
        host: Option<&'a (dyn std::any::Any + Send + Sync)>,
        settings: &'a Settings,
        cancel: &'a CancelToken,
        parts: &'a TestParts,
    ) -> Self {
        Self {
            host,
            settings,
            session_id: "s",
            run_id: "r",
            call_id: "c",
            workspace_dir: None,
            sandbox: SandboxKind::None,
            cancel,
            output: None,
            events: None,
            processes: &parts.processes,
            read_hashes: &parts.read_hashes,
            data_tool_output_dir: None,
            data_root: None,
            agent: None,
            shell_network: false,
            shell_full_access: false,
            unsandboxed_approved: false,
            result_cap: crate::agent::context::MAX_RESULT_BYTES,
        }
    }
}

/// One tool the model may call.
pub trait AgentTool: Send + Sync {
    /// Name, description, schema and static hints.
    fn spec(&self) -> &ToolSpec;

    /// Validates `arguments` and classifies this call. An `Err` becomes an error result for the
    /// model without asking the user (bad path, missing argument, protected-and-denied path).
    ///
    /// # Errors
    /// [`AppError::Validation`] naming the faulty argument.
    fn inspect(&self, ctx: &ToolContext<'_>, arguments: &Value)
    -> Result<CallInspection, AppError>;

    /// Executes the call.
    fn call<'a>(&'a self, ctx: &'a ToolContext<'a>, arguments: Value) -> BoxFuture<'a, ToolOutput>;
}

/// The tools one run may call, by function name.
#[derive(Clone, Default)]
pub struct ToolRegistry {
    tools: Vec<Arc<dyn AgentTool>>,
}

impl ToolRegistry {
    /// An empty registry.
    #[must_use]
    pub fn new() -> Self {
        Self::default()
    }

    /// Adds a tool; a later tool with the same name replaces the earlier one.
    pub fn register(&mut self, tool: Arc<dyn AgentTool>) {
        self.tools
            .retain(|existing| existing.spec().name != tool.spec().name);
        self.tools.push(tool);
    }

    /// Adds a tool unless another already holds its name; `false` (and the tool dropped) then. For
    /// tools whose names come from outside (MCP servers): a server must never shadow another
    /// tool, including its trust flags.
    pub fn register_unique(&mut self, tool: Arc<dyn AgentTool>) -> bool {
        if self.get(&tool.spec().name).is_some() {
            return false;
        }
        self.tools.push(tool);
        true
    }

    /// The tool called `name`.
    #[must_use]
    pub fn get(&self, name: &str) -> Option<&Arc<dyn AgentTool>> {
        self.tools.iter().find(|tool| tool.spec().name == name)
    }

    /// The specs of every registered tool, in registration order.
    #[must_use]
    pub fn specs(&self) -> Vec<ToolSpec> {
        self.tools.iter().map(|tool| tool.spec().clone()).collect()
    }

    /// How many tools are registered.
    #[must_use]
    pub fn len(&self) -> usize {
        self.tools.len()
    }

    /// Whether no tool is registered.
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.tools.is_empty()
    }
}
