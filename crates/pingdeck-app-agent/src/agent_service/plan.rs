//! Plan (read-only) mode of a chat.
//!
//! Single responsibility: switch a chat into Plan, answer whether it is, and approve the plan. In
//! Plan only Read and PingDeck-read tools, `read_skill`, `read_attachment`, `todo_write` and
//! `submit_plan` are offered (the rest are absent from the tool list and `blocked` when called
//! anyway: `app::agent_service::resolve`, `host::plan_call`). The model hands its plan over with
//! `submit_plan`, which waits on a `Plan` gate. Approving answers that gate (the run ends
//! `Completed`), leaves Plan, sets the chosen mode and starts the run that carries the plan out
//! with the user's localised text. Plan is stored on the chat (`agent_sessions.plan_mode`) and
//! survives a relaunch; a branch or a sub-agent never inherits it.

use crate::agent::dto::{AgentRunInput, AgentRunStarted};
use crate::agent::gate::{GateAnswer, GateKind, PlanDecision};
use crate::agent::model::AgentSessionPatch;
use crate::error::AppError;
use crate::settings::ToolApproval;

use super::host::blocking;
use super::run::{agent_run_start_impl, dispatch_next};
use super::sessions::{agent_session_update_impl, load_session};
use crate::app::{AppState, command_error};

/// The first message of the run after Approve plan when the caller sent no text.
pub const PLAN_APPROVED_TEXT: &str = "The plan is approved. Carry it out.";

/// Whether `session_id` is in Plan mode.
#[must_use]
pub fn agent_plan_get_impl(state: &AppState, session_id: &str) -> bool {
    state.agent.plan_active(session_id)
}

/// Puts the chat in or out of Plan mode, stores it, and returns the mode now. Takes effect at
/// the next step boundary of a run in progress.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown chat; [`AppError::Database`] when it cannot be stored.
pub async fn agent_plan_set_impl(
    state: &AppState,
    session_id: &str,
    on: bool,
) -> Result<bool, AppError> {
    let id = session_id.to_owned();
    blocking(state, move |state| load_session(state, &id).map(|_| ())).await??;
    store_plan(state, session_id, on).await?;
    Ok(on)
}

/// Approves the plan: answers the run's `Plan` gate when one is open and waits for that run to
/// end, leaves Plan, applies `mode` (none: the mode the chat had, Full access returning as
/// Approve reads) and starts the run that carries the plan out with `text` (the localised
/// approval and the plan as edited; [`PLAN_APPROVED_TEXT`] when empty). A start that fails puts
/// the chat back in Plan with its mode.
///
/// # Errors
/// [`AppError::Conflict`] when the chat is not in Plan, or a run is still writing the plan; the
/// errors of [`agent_run_start_impl`] otherwise.
pub async fn agent_plan_approve_impl(
    state: &AppState,
    session_id: &str,
    text: Option<String>,
    mode: Option<ToolApproval>,
) -> Result<AgentRunStarted, AppError> {
    let id = session_id.to_owned();
    let session = blocking(state, move |state| load_session(state, &id)).await??;
    if !state.agent.plan_active(session_id) {
        return Err(AppError::conflict("this chat is not in Plan mode"));
    }
    let waiting = state.agent.runs.active_for_session(session_id).map(|run| {
        let gate = state.agent.gates.open_gate(&run.run_id, GateKind::Plan);
        (run, gate)
    });
    let previous = session.permission_mode;
    let next = mode.unwrap_or(match previous {
        ToolApproval::AutoAll => ToolApproval::AutoReadOnly,
        other => other,
    });
    let text = text
        .map(|text| text.trim().to_owned())
        .filter(|text| !text.is_empty())
        .unwrap_or_else(|| PLAN_APPROVED_TEXT.to_owned());
    match waiting {
        Some((_, None)) => return Err(AppError::conflict("wait for the plan to be written first")),
        Some((run, Some(gate_id))) => {
            run.hand_over();
            let answer = GateAnswer::Plan {
                decision: PlanDecision::Approve,
                mode: Some(next),
                text: text.clone(),
            };
            if !state.agent.gates.deliver(&run.run_id, &gate_id, answer) {
                return Err(AppError::conflict("the plan no longer waits for approval"));
            }
            run.ended().await;
        }
        None => {}
    }
    if next != previous {
        set_mode(state, session_id, next).await?;
    }
    store_plan(state, session_id, false).await?;
    let started = agent_run_start_impl(
        state,
        AgentRunInput {
            session_id: session_id.to_owned(),
            text,
            ..AgentRunInput::default()
        },
    )
    .await;
    if started.is_err() {
        // The chat is back in Plan with the mode it had; the queue the plan held goes on.
        let _ = store_plan(state, session_id, true).await;
        if next != previous {
            let _ = set_mode(state, session_id, previous).await;
        }
        dispatch_next(state, session_id).await;
    }
    started
}

async fn store_plan(state: &AppState, session_id: &str, on: bool) -> Result<(), AppError> {
    let id = session_id.to_owned();
    blocking(state, move |state| {
        state
            .store()
            .set_session_plan_mode(&id, on)
            .map_err(command_error)
    })
    .await??;
    state.agent.set_plan(session_id, on);
    Ok(())
}

async fn set_mode(state: &AppState, session_id: &str, mode: ToolApproval) -> Result<(), AppError> {
    let id = session_id.to_owned();
    blocking(state, move |state| {
        agent_session_update_impl(
            state,
            &id,
            AgentSessionPatch {
                permission_mode: Some(mode),
                ..AgentSessionPatch::default()
            },
        )
        .map(|_| ())
    })
    .await?
}
