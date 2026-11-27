//! What is running, for the quit and close confirm, and the ordered stop of agent work
//!
//! Single responsibility: [`app_running_work_impl`] counts, at any time, the work a quit would
//! cut off: agent runs (waiting ones, background ones and sub-agents included), background shell
//! processes, MCP servers, mock servers, the gateway, flow runs, collection runs and Test-bench
//! runs. [`agent_work_stop_impl`] is the agent half of a confirmed quit: cancel the runs and give
//! them up to [`RUN_DRAIN`] to store their tool results, then close the MCP children and kill the
//! background process groups, in that order. The shell stops mock servers and the gateway after
//! it (`AppState::shutdown`).

use std::time::Duration;

use serde::{Deserialize, Serialize};

use crate::agent::mcp::McpServerStatus;
use crate::agent::model::AgentRunKind;
use crate::agent::tools::ProcessState;
use crate::error::AppError;

use super::board::ensure_seeded;
use crate::app::AppState;

/// How long cancelled runs may take to store what they have before the stop goes on.
const RUN_DRAIN: Duration = Duration::from_secs(2);
/// How often the drain looks at the registry.
const DRAIN_POLL: Duration = Duration::from_millis(25);

/// The live counts a quit or close confirm names.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RunningWork {
    /// Top-level agent runs that have not ended (waiting, queued and background ones included).
    pub agent_runs: u32,
    /// Sub-agent runs that have not ended.
    pub subagents: u32,
    /// Background shell processes still running.
    pub background_processes: u32,
    /// MCP servers with a live connection.
    pub mcp_servers: u32,
    /// Running mock servers.
    pub mock_servers: u32,
    /// The local gateway is running.
    pub gateway_running: bool,
    /// Flow runs in flight.
    pub flow_runs: u32,
    /// Collection runs in flight.
    pub collection_runs: u32,
    /// Test-bench runs in flight.
    pub bench_runs: u32,
}

impl RunningWork {
    /// Whether anything is running: the confirm is shown exactly then.
    #[must_use]
    pub fn any(&self) -> bool {
        self.agent_runs
            + self.subagents
            + self.background_processes
            + self.mcp_servers
            + self.mock_servers
            + self.flow_runs
            + self.collection_runs
            + self.bench_runs
            > 0
            || self.gateway_running
    }
}

fn count(len: usize) -> u32 {
    u32::try_from(len).unwrap_or(u32::MAX)
}

/// Background shell processes still running, over all chats.
pub(super) fn running_process_count(state: &AppState) -> u32 {
    count(
        state
            .agent
            .processes
            .list(None)
            .iter()
            .filter(|process| process.state == ProcessState::Running)
            .count(),
    )
}

/// Drops the run subscriptions whose consumer is gone (a reloaded or closed page) so the
/// approval-timeout watcher count falls at once instead of at the next emit.
pub fn agent_reap_gone_sinks_impl(state: &AppState) {
    state.agent.runs.reap_gone();
}

/// The work a quit would cut off right now.
#[must_use]
pub fn app_running_work_impl(state: &AppState) -> RunningWork {
    let runs = state.agent.runs.active_runs();
    let subagents = runs
        .iter()
        .filter(|run| run.kind == AgentRunKind::Subagent)
        .count();
    let processes = running_process_count(state);
    let mcp = state
        .agent
        .mcp
        .status_list()
        .iter()
        .filter(|event| event.status == McpServerStatus::Running)
        .count();
    RunningWork {
        agent_runs: count(runs.len() - subagents),
        subagents: count(subagents),
        background_processes: processes,
        mcp_servers: count(mcp),
        mock_servers: count(state.mocks.running_count()),
        gateway_running: state.llm().gateway.status(state.llm()).running,
        flow_runs: state.active_flow_runs(),
        collection_runs: state.active_collection_runs(),
        bench_runs: state.active_bench_runs(),
    }
}

/// Stops the agent's work for a confirmed quit: cancels every run and waits up to two seconds for
/// them to end (their tool calls answered, results stored), then closes the MCP children and
/// kills the background process groups.
pub async fn agent_work_stop_impl(state: &AppState) {
    super::run::agent_run_stop_all_impl(state);
    let deadline = tokio::time::Instant::now() + RUN_DRAIN;
    while !state.agent.runs.active_runs().is_empty() && tokio::time::Instant::now() < deadline {
        tokio::time::sleep(DRAIN_POLL).await;
    }
    state.agent.mcp.disconnect_all();
    state.agent.processes.kill_all().await;
}

/// Completes or drops the journal rows a crash left `Applying` and seeds unopened runs.
///
/// # Errors
/// [`AppError::Database`] when the journal cannot be read or written.
pub fn agent_runtime_start_impl(state: &AppState) -> Result<(), AppError> {
    ensure_seeded(state);
    crate::app::recovery::recover_file_journal(&state.store())
}
