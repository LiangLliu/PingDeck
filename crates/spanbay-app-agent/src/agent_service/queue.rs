//! Queue use-cases: the messages a chat holds back while a run is active. Whether the queue
//! runs on or waits for a resume is the run registry's state, not stored here.

use crate::agent::dto::ContextRef;
use crate::agent::model::{QueuedContent, QueuedTurn};
use crate::error::AppError;
use crate::llm::hub::HubPart;

use crate::agent::registry::SteerMessage;

use super::host::blocking;
use super::run::{dispatch_next, queue_changed, queued_parts, requeue_front};
use super::sessions::{load_session, validate_user_parts};
use crate::app::{AppState, command_error};

/// A queued message's content from the command's arguments.
fn content(parts: Vec<HubPart>, skills: Vec<String>, contexts: &[ContextRef]) -> QueuedContent {
    QueuedContent {
        parts,
        skills,
        contexts: contexts
            .iter()
            .filter_map(|context| serde_json::to_value(context).ok())
            .collect(),
    }
}

fn queue_of(state: &AppState, session_id: &str) -> Result<Vec<QueuedTurn>, AppError> {
    state.store().list_queue(session_id).map_err(command_error)
}

/// Queues `content` (its parts validated like any user message) behind the chat's active run and
/// returns the queue. A full queue (5 messages) is a `Conflict` reading `Queue is full (5)`.
fn add_queue(
    state: &AppState,
    session_id: &str,
    content: &QueuedContent,
) -> Result<Vec<QueuedTurn>, AppError> {
    validate_user_parts(&content.parts)?;
    load_session(state, session_id)?;
    let store = state.store();
    store
        .add_queued_turn(session_id, content)
        .map_err(command_error)?
        .ok_or_else(|| AppError::conflict("Queue is full (5)"))?;
    store.list_queue(session_id).map_err(command_error)
}

/// Replaces the content of queued message `queue_id` and returns the queue.
fn update_queue(
    state: &AppState,
    session_id: &str,
    queue_id: &str,
    content: &QueuedContent,
) -> Result<Vec<QueuedTurn>, AppError> {
    validate_user_parts(&content.parts)?;
    load_session(state, session_id)?;
    let store = state.store();
    if !store
        .update_queued_turn(session_id, queue_id, content)
        .map_err(command_error)?
    {
        return Err(AppError::not_found(format!(
            "queued message {queue_id} not found"
        )));
    }
    store.list_queue(session_id).map_err(command_error)
}

/// Removes queued message `queue_id` and returns the queue.
fn remove_queue(
    state: &AppState,
    session_id: &str,
    queue_id: &str,
) -> Result<Vec<QueuedTurn>, AppError> {
    load_session(state, session_id)?;
    let store = state.store();
    if !store
        .remove_queued_turn(session_id, queue_id)
        .map_err(command_error)?
    {
        return Err(AppError::not_found(format!(
            "queued message {queue_id} not found"
        )));
    }
    store.list_queue(session_id).map_err(command_error)
}

/// Empties the chat's queue and returns it (empty).
fn clear_queue(state: &AppState, session_id: &str) -> Result<Vec<QueuedTurn>, AppError> {
    load_session(state, session_id)?;
    let store = state.store();
    store.clear_queue(session_id).map_err(command_error)?;
    store.list_queue(session_id).map_err(command_error)
}

/// The chat's queued messages, first to run first.
pub fn agent_queue_list_impl(
    state: &AppState,
    session_id: &str,
) -> Result<Vec<QueuedTurn>, AppError> {
    load_session(state, session_id)?;
    queue_of(state, session_id)
}

/// Same as the queue use-case it wraps, and tells the chat's run stream the queue changed. In an
/// idle chat whose queue is not paused the message starts at once (a queue that is
/// neither paused nor empty always has a run going).
///
/// # Errors
/// As the wrapped operation.
pub async fn agent_queue_add_impl(
    state: &AppState,
    session_id: &str,
    parts: Vec<HubPart>,
    skills: Vec<String>,
    contexts: &[ContextRef],
) -> Result<Vec<QueuedTurn>, AppError> {
    let queued = content(parts, skills, contexts);
    let id = session_id.to_owned();
    let queue = blocking(state, move |state| {
        let queue = add_queue(state, &id, &queued)?;
        queue_changed(state, &id);
        Ok::<_, AppError>(queue)
    })
    .await??;
    if state.agent.runs.active_for_session(session_id).is_some()
        || state.agent.runs.queue_paused(session_id)
    {
        return Ok(queue);
    }
    dispatch_next(state, session_id).await;
    let id = session_id.to_owned();
    blocking(state, move |state| queue_of(state, &id)).await?
}

/// Same as the queue use-case it wraps, and tells the chat's run stream the queue changed.
///
/// # Errors
/// As the wrapped operation.
pub fn agent_queue_update_impl(
    state: &AppState,
    session_id: &str,
    queue_id: &str,
    parts: Vec<HubPart>,
    skills: Vec<String>,
    contexts: &[ContextRef],
) -> Result<Vec<QueuedTurn>, AppError> {
    let queue = update_queue(
        state,
        session_id,
        queue_id,
        &content(parts, skills, contexts),
    )?;
    queue_changed(state, session_id);
    Ok(queue)
}

/// Same as the queue use-case it wraps, and tells the chat's run stream the queue changed.
///
/// # Errors
/// As the wrapped operation.
pub fn agent_queue_remove_impl(
    state: &AppState,
    session_id: &str,
    queue_id: &str,
) -> Result<Vec<QueuedTurn>, AppError> {
    let queue = remove_queue(state, session_id, queue_id)?;
    queue_changed(state, session_id);
    Ok(queue)
}

/// Same as the queue use-case it wraps, and tells the chat's run stream the queue changed.
///
/// # Errors
/// As the wrapped operation.
pub fn agent_queue_clear_impl(
    state: &AppState,
    session_id: &str,
) -> Result<Vec<QueuedTurn>, AppError> {
    let queue = clear_queue(state, session_id)?;
    queue_changed(state, session_id);
    Ok(queue)
}

/// Moves queued message `queue_id` into the chat's active run as a steer message: it joins at the
/// next step boundary. A steer the run ends before taking goes back to the front of the queue
/// (paused). `false` when the chat has no active run or the message is gone.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown chat; the errors of reading the objects it mentions.
pub async fn agent_queue_steer_impl(
    state: &AppState,
    session_id: &str,
    queue_id: &str,
) -> Result<bool, AppError> {
    let Some(run) = state
        .agent
        .runs
        .active_for_session(session_id)
        .filter(|run| run.status().is_active())
    else {
        return Ok(false);
    };
    let (id, queued) = (session_id.to_owned(), queue_id.to_owned());
    let taken = blocking(state, move |state| {
        load_session(state, &id)?;
        let Some(turn) = queue_of(state, &id)?
            .into_iter()
            .find(|turn| turn.id == queued)
        else {
            return Ok(None);
        };
        let content = QueuedContent {
            parts: turn.parts,
            skills: turn.skills,
            contexts: turn.contexts,
        };
        // Mentions are read before the message leaves the queue, so a failure keeps it there.
        let parts = queued_parts(state, &content)?;
        let removed = state
            .store()
            .take_queued_turn(&id, &queued)
            .map_err(command_error)?;
        Ok::<_, AppError>(removed.map(|_| SteerMessage {
            parts,
            skills: content.skills,
        }))
    })
    .await??;
    let Some(message) = taken else {
        return Ok(false);
    };
    run.push_steer(message);
    queue_changed(state, session_id);
    if run.status().is_active() {
        return Ok(true);
    }
    // The run ended between the check and the push: whatever its finish did not take is put back.
    let late = run.take_steer();
    for steer in late.into_iter().rev() {
        let content = QueuedContent {
            parts: steer.parts,
            skills: steer.skills,
            // `parts` already carry the resolved mentions; re-reading them would add them twice.
            contexts: Vec::new(),
        };
        requeue_front(state, session_id, content).await;
    }
    queue_changed(state, session_id);
    Ok(false)
}
