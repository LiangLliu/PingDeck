//! Background shell processes: the `agent_process_list` and `agent_process_kill` use-cases
//!
//! Single responsibility: show and end the processes of `AgentRuntime.processes`. The registry
//! itself (limits, output rings, process-group kill, the 8-hour lifetime) is
//! `crate::agent::tools::process`.

use crate::agent::tools::{BackgroundProcess, ProcessState};
use crate::error::AppError;

use crate::app::AppState;

/// The tracked processes (running ones, and finished ones for 30 minutes), oldest first, of one
/// chat or of every chat.
///
/// # Errors
/// Never fails today; the `Result` is the shell contract of every use-case.
pub fn agent_process_list_impl(
    state: &AppState,
    session_id: Option<&str>,
) -> Result<Vec<BackgroundProcess>, AppError> {
    Ok(state.agent.processes.list(session_id))
}

/// Asks the process to end: `SIGTERM` to its process group, `SIGKILL` after a short grace. The
/// call returns at once (a process can take a second to go); the list shows `killed` when it is
/// gone. `true` when a running process was told to stop, `false` when it had already ended.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown or already forgotten process, [`AppError::Internal`]
/// when called outside an async runtime.
pub fn agent_process_kill_impl(state: &AppState, process_id: &str) -> Result<bool, AppError> {
    let process = state
        .agent
        .processes
        .list(None)
        .into_iter()
        .find(|process| process.id == process_id)
        .ok_or_else(|| AppError::not_found("No such process"))?;
    if process.state != ProcessState::Running {
        return Ok(false);
    }
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|_| AppError::internal("stopping a process needs the async runtime"))?;
    let (processes, id) = (state.agent.processes.clone(), process.id);
    runtime.spawn(async move {
        // The process may end by itself first; either way it is gone afterwards.
        let _ = processes.kill(None, &id).await;
    });
    Ok(true)
}
