//! The run lifecycle: start, subscribe, stop, steer, list, continue, edit, retry, compact, queue.
//!
//! Single responsibility: create runs in the registry, drive each on its own task and end each with
//! exactly one `Done` or `Error` event, persisting its row at every transition. A start returns at
//! once with the ids; everything the model does reaches the shell through the run's replayable
//! event stream (`agent_run_subscribe_impl`). Top-level runs wait `Queued` for one of
//! `ai.agent.max_concurrent_runs` slots; a run waiting for approval holds none. The per-chat queue
//! (persisted by the store) runs on when a run ends `Completed` and pauses (in memory) when one
//! ends any other way, until `agent_queue_resume_impl`.

use std::sync::Arc;

use futures_util::future::BoxFuture;

use crate::agent::attachments::attachment_parts;
use crate::agent::attention::FinishedRun;
use crate::agent::compaction;
use crate::agent::dto::{
    AgentRunInput, AgentRunListFilter, AgentRunStarted, AgentSubscribeTarget, ContextRef,
    ContextUse,
};
use crate::agent::failure::error_kind_name;
use crate::agent::gate::{GateAnswer, GateKind};
use crate::agent::model::{
    AgentEnvelope, AgentEvent, AgentMessage, AgentRole, AgentRunKind, AgentRunResult,
    AgentRunStatus, AgentRunSummary, AgentSession, AgentStopReason, AgentTarget, FileChange,
    FileChangeStatus, QueuedContent, QueuedTurn,
};
use crate::agent::prompt::PROMPT_VERSION;
use crate::agent::registry::{ActiveRun, CostDecision, SteerMessage};
use crate::agent::runner::{CompactionPolicy, CostCap, RunHost, RunInput, run_agent};
use crate::agent::secret_guard::expand_needles;
use crate::cancel::CancelToken;
use crate::error::AppError;
use crate::events::{EventSink, Sequenced};
use crate::llm::client::ChatTarget;
use crate::llm::hub::HubPart;
use crate::settings::AgentFallbacks;
use crate::workspace::storage::PingDeckBinding;

use super::attachments::load_sendable;
use super::host::{HostParts, ServiceHost, blocking, new_message, persist_run, store_user_message};
use super::model_info::resolve_model;
use super::resolve::{Resolved, resolve};
use super::sessions::{
    DEFAULT_TITLE, agent_message_prepare_edit_impl, agent_message_prepare_retry_impl, load_session,
    validate_user_parts,
};
use super::tools::Role;
use crate::app::factory_service::new_entity_id;
use crate::app::{AppState, command_error, current_time_ms};

/// Maximum characters retained when deriving a provisional title from the user's first question.
pub(super) const PROVISIONAL_TITLE_MAX_CHARS: usize = 30;
/// Runs `agent_run_list` returns when the caller names no limit: the chat store reads the end of
/// finished turns through it, so it must reach well past a long chat.
const RUN_LIST_DEFAULT: u32 = 500;
/// Most runs one `agent_run_list` call returns.
const RUN_LIST_MAX: u32 = 5_000;

/// The client target of a run on `target`: a named model brings the agent fallback chain of the
/// run's usage site (`ai.agent.fallbacks`: its sub-agent role, else the main chain); a pinned
/// model has none.
fn chat_target(target: &AgentTarget, fallbacks: &AgentFallbacks, run: &ActiveRun) -> ChatTarget {
    match target {
        AgentTarget::Named { name } => ChatTarget::Named {
            name: name.clone(),
            fallbacks: fallbacks.for_role(run.summary().role).to_vec(),
        },
        AgentTarget::Pinned {
            provider_id,
            key_id,
            model,
        } => ChatTarget::Pinned {
            provider_id: provider_id.clone(),
            key_id: key_id.clone(),
            model: model.clone(),
        },
    }
}

/// The automatic title of a chat's first message: the first words the user wrote. A message that
/// only hands over context objects ("Ask agent" without text) is named after the first one's
/// label; the tagged context text never reaches the title.
pub(super) fn title_of(parts: &[HubPart]) -> String {
    let mut own = String::new();
    let mut label = None;
    for part in parts {
        if let HubPart::Text { text, .. } = part {
            match super::context_payload::label_of(text) {
                Some(found) => {
                    label.get_or_insert(found);
                }
                None => {
                    own.push_str(text);
                    own.push(' ');
                }
            }
        }
    }
    let named = title_from(&own);
    if named.is_empty() {
        label.map(title_from).unwrap_or_default()
    } else {
        named
    }
}

/// Derives a single-line provisional title from the user's first question, collapsing whitespace
/// and truncating overly long questions to `PROVISIONAL_TITLE_MAX_CHARS` characters.
pub(super) fn title_from(text: &str) -> String {
    let collapsed = text.split_whitespace().collect::<Vec<_>>().join(" ");
    if collapsed.chars().count() <= PROVISIONAL_TITLE_MAX_CHARS {
        collapsed
    } else {
        let prefix: String = collapsed
            .chars()
            .take(PROVISIONAL_TITLE_MAX_CHARS.saturating_sub(1))
            .collect();
        format!("{}…", prefix.trim_end())
    }
}

/// The message parts of a user message: text, then the attachments (checked for sending).
fn user_parts(
    state: &AppState,
    text: &str,
    attachment_ids: &[String],
) -> Result<Vec<HubPart>, AppError> {
    let attachments = if attachment_ids.is_empty() {
        Vec::new()
    } else {
        load_sendable(state, attachment_ids)?
    };
    let parts = attachment_parts(&attachments, text.trim());
    validate_user_parts(&parts)?;
    Ok(parts)
}

/// What starts a run's user message.
enum Turn {
    /// A new message, stored when the run is created.
    New {
        parts: Vec<HubPart>,
        skills: Vec<String>,
    },
    /// A message `prepare_edit` or `prepare_retry` already stored.
    Stored(Box<AgentMessage>),
}

/// A run that is registered and stored but has not started executing.
struct Begun {
    run: Arc<ActiveRun>,
    session: AgentSession,
    user: AgentMessage,
    title: Option<String>,
}

pub(super) fn initial_summary(
    session_id: &str,
    kind: AgentRunKind,
    target: &AgentTarget,
) -> AgentRunSummary {
    AgentRunSummary {
        run_id: new_entity_id("ar"),
        session_id: session_id.to_owned(),
        kind,
        parent_run_id: None,
        parent_call_id: None,
        status: AgentRunStatus::Queued,
        model_label: target.label(),
        target: Some(target.clone()),
        iterations: 0,
        usage: crate::llm::hub::HubUsage::default(),
        cost_micro_usd: 0,
        started_at_ms: current_time_ms(),
        finished_at_ms: None,
        last_event_ms: None,
        last_step: None,
        stop_reason: None,
        error_kind: None,
        error: None,
        prompt_version: PROMPT_VERSION,
        last_seq: 0,
        failure: None,
        role: None,
        cost_currency: None,
        cap_inapplicable: None,
        status_detail: None,
    }
}

/// One line per background process of the chat that ended while no run was going, for the first
/// message of the next run: the model learns what finished meanwhile.
fn background_exit_notes(state: &AppState, session_id: &str) -> Vec<HubPart> {
    state
        .agent
        .processes
        .take_finished_unreported(session_id)
        .into_iter()
        .map(|process| HubPart::text(background_exit_line(&process)))
        .collect()
}

/// What the model reads about a background process that ended (once, and how it ended).
pub(super) fn background_exit_line(process: &crate::agent::tools::BackgroundProcess) -> String {
    use crate::agent::tools::ProcessState;
    let how = match (process.state, process.exit_code) {
        (ProcessState::Exited, Some(code)) => format!("exited with code {code}"),
        (ProcessState::Exited, None) | (ProcessState::Running, _) => "exited".to_owned(),
        (ProcessState::Killed, _) => "was stopped".to_owned(),
        (ProcessState::Expired, _) => "was stopped at its lifetime limit".to_owned(),
    };
    format!(
        "[Background process {} (`{}`) {how}.]",
        process.id, process.command
    )
}

/// Registers a run for `session_id` and stores its row and (for a new turn) its user message.
/// Blocking (the store). The registry refuses a second active run of the chat before anything is
/// written.
fn begin(state: &AppState, session_id: &str, turn: Turn) -> Result<Begun, AppError> {
    let mut session = load_session(state, session_id)?;
    let target = session
        .target
        .clone()
        .ok_or_else(super::sessions::no_model)?;
    let run = ActiveRun::new(
        initial_summary(session_id, AgentRunKind::Main, &target),
        CancelToken::new(),
    );
    state.agent.runs.register(&run)?;
    let stored = (|| {
        let settings = state.settings();
        let mut title = None;
        let user = match turn {
            Turn::New { parts, skills } => {
                let parts = background_exit_notes(state, session_id)
                    .into_iter()
                    .chain(parts)
                    .collect::<Vec<_>>();
                if session.title == DEFAULT_TITLE
                    && !session.title_locked
                    && settings.ai.agent.auto_title
                {
                    let has_prior_user_turn = state
                        .store()
                        .list_agent_messages(session_id)
                        .map_err(command_error)?
                        .iter()
                        .any(|m| m.role == AgentRole::User);
                    if !has_prior_user_turn {
                        let named = title_of(&parts);
                        if !named.is_empty() {
                            session.title.clone_from(&named);
                            session.updated_at_ms = current_time_ms();
                            state
                                .store()
                                .save_agent_session(&session)
                                .map_err(command_error)?;
                            title = Some(named);
                        }
                    }
                }
                let mut message =
                    new_message(session_id, Some(&run.run_id), AgentRole::User, parts);
                message.meta.skills = skills;
                store_user_message(state, &message)?
            }
            Turn::Stored(message) => *message,
        };
        state
            .store()
            .insert_agent_run(&run.summary())
            .map_err(command_error)?;
        Ok::<_, AppError>((user, title))
    })();
    match stored {
        Ok((user, title)) => Ok(Begun {
            run,
            session,
            user,
            title,
        }),
        Err(error) => {
            state.agent.runs.discard(&run.run_id);
            Err(error)
        }
    }
}

/// The cost caps in force, one per currency whose setting is on, whatever the run's own model is
/// priced in: a sub-agent on another model spends in its own currency.
fn cost_caps(settings: &crate::settings::Settings) -> Vec<CostCap> {
    [
        ("USD", settings.ai.agent.cost_cap_usd),
        ("CNY", settings.ai.agent.cost_cap_cny),
    ]
    .into_iter()
    .filter_map(|(currency, cap)| {
        // `as` saturates; a cap below one millionth is no cap.
        let micro = (cap? * 1_000_000.0).round() as i64;
        (micro > 0).then(|| CostCap {
            micro,
            currency: currency.to_owned(),
        })
    })
    .collect()
}

/// Registers the run and spawns its driver; returns the ids.
async fn launch(
    state: &AppState,
    session_id: &str,
    turn: Turn,
) -> Result<AgentRunStarted, AppError> {
    let id = session_id.to_owned();
    let begun = blocking(state, move |state| begin(state, &id, turn)).await??;
    let started = AgentRunStarted {
        run_id: begun.run.run_id.clone(),
        session_id: session_id.to_owned(),
        user_message_id: begun.user.id.clone(),
    };
    state.agent.publish_attention();
    // A panic inside the run (a dependency's) must not leave the chat marked as answering.
    let (watched, run) = (state.clone(), Arc::clone(&begun.run));
    let task = tokio::spawn(drive(state.clone(), begun));
    tokio::spawn(async move {
        if task.await.is_err() {
            let error = AppError::internal("the run stopped unexpectedly");
            finish(&watched, &run, Err(error)).await;
        }
    });
    Ok(started)
}

/// Starts a run that answers a new message in a chat.
///
/// Returns as soon as the run is registered and the message stored; the run executes on its own
/// task (`Queued` until a concurrency slot is free) and reports through `agent_run_subscribe_impl`.
/// A blank message without attachments, a chat without a model and attachments the model cannot
/// take are validation errors; a chat that already has an active run is a conflict. The chat is
/// titled from the message's first words unless the user named it.
///
/// # Errors
/// [`AppError::NotFound`], [`AppError::Validation`], [`AppError::Conflict`] as above.
pub async fn agent_run_start_impl(
    state: &AppState,
    input: AgentRunInput,
) -> Result<AgentRunStarted, AppError> {
    let AgentRunInput {
        session_id,
        text,
        attachment_ids,
        skills,
        workspace_id,
        environment_id,
        contexts,
        context_use,
    } = input;
    let parts = {
        let (state, text, id) = (state.clone(), text, session_id.clone());
        blocking(&state.clone(), move |_| {
            // A message may be nothing but the objects it hands over ("Ask agent").
            let mut parts =
                if contexts.is_empty() || !text.trim().is_empty() || !attachment_ids.is_empty() {
                    user_parts(&state, &text, &attachment_ids)?
                } else {
                    Vec::new()
                };
            parts.extend(super::context_payload::context_parts(
                &state,
                &contexts,
                context_use,
            )?);
            bind_workspace(
                &state,
                &id,
                workspace_id.as_deref(),
                environment_id.as_deref(),
            )?;
            Ok::<_, AppError>(parts)
        })
        .await??
    };
    // The user is in this chat: what it finished earlier is no longer news.
    super::board::session_seen(state, &session_id).await;
    launch(state, &session_id, Turn::New { parts, skills }).await
}

/// Binds the chat to the PingDeck workspace and environment that are active now when it has none
/// yet (its first message). A bound chat keeps its binding whatever the user opens later; only
/// `agent_session_bind_pingdeck` changes it, while the chat is idle.
fn bind_workspace(
    state: &AppState,
    session_id: &str,
    workspace_id: Option<&str>,
    environment_id: Option<&str>,
) -> Result<(), AppError> {
    let Some(workspace_id) = workspace_id else {
        return Ok(());
    };
    let store = state.store();
    if store
        .session_pingdeck_binding(session_id)
        .map_err(command_error)?
        .is_some()
    {
        return Ok(());
    }
    let binding = PingDeckBinding {
        workspace_id: workspace_id.to_owned(),
        environment_id: environment_id.map(str::to_owned),
    };
    store
        .set_session_pingdeck_binding(session_id, Some(&binding))
        .map(|_| ())
        .map_err(command_error)
}

/// The file changes the turn that opened with `boundary_message_id` left pending, newest first:
/// what "This turn changed 3 files. Undo them first?" counts.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown chat.
pub fn agent_turn_changes_impl(
    state: &AppState,
    session_id: &str,
    boundary_message_id: &str,
) -> Result<Vec<FileChange>, AppError> {
    load_session(state, session_id)?;
    let mut rows: Vec<FileChange> = state
        .store()
        .turn_file_changes(session_id, boundary_message_id)
        .map_err(command_error)?
        .into_iter()
        .filter(|row| {
            matches!(
                row.status,
                FileChangeStatus::Pending | FileChangeStatus::Conflict
            )
        })
        .collect();
    rows.reverse();
    Ok(rows)
}

/// Undoes the pending changes of the turn first (the "Undo and retry" choice). Stops at a file
/// that changed since, naming it, so the retry never starts on a half-undone folder.
fn undo_turn_changes(
    state: &AppState,
    session_id: &str,
    boundary_message_id: &str,
) -> Result<(), AppError> {
    let ids: Vec<String> = agent_turn_changes_impl(state, session_id, boundary_message_id)?
        .into_iter()
        .map(|row| row.id)
        .collect();
    if ids.is_empty() {
        return Ok(());
    }
    let touched = super::changes::agent_change_undo_impl(state, &ids, false)?;
    match touched
        .iter()
        .find(|row| row.status == FileChangeStatus::Conflict)
    {
        Some(row) => Err(AppError::conflict(format!(
            "{} changed since the agent edited it; undo stopped there",
            row.path
        ))),
        None => Ok(()),
    }
}

/// Edit: stores the new text as a sibling of `message_id`, makes it the head and starts the run
/// that answers it. With `undo_first` the turn's pending file changes are undone before the run
/// starts. A start that fails (an active run, no model, an undo conflict) restores the previous
/// branch.
///
/// # Errors
/// As [`agent_message_prepare_edit_impl`] and [`agent_run_start_impl`]; [`AppError::Conflict`]
/// when an undo stops at a changed file.
pub async fn agent_message_edit_impl(
    state: &AppState,
    session_id: &str,
    message_id: &str,
    text: String,
    attachment_ids: Vec<String>,
    undo_first: bool,
) -> Result<AgentRunStarted, AppError> {
    let (id, message, owner) = (session_id.to_owned(), message_id.to_owned(), state.clone());
    let (user, head) = blocking(state, move |state| {
        let head = load_session(state, &id)?.head_message_id;
        let parts = user_parts(state, &text, &attachment_ids)?;
        let user = agent_message_prepare_edit_impl(&owner, &id, &message, parts)?;
        if undo_first && let Err(error) = undo_turn_changes(state, &id, &message) {
            if let Some(head) = &head {
                let _ = state.store().set_head(&id, head);
            }
            return Err(error);
        }
        Ok((user, head))
    })
    .await??;
    launch_stored(state, session_id, user, head).await
}

/// Retry: makes the user message that opened the turn of `message_id` the head and starts the run
/// whose first answer becomes a sibling of the retried one. With `undo_first` the turn's pending
/// file changes are undone before the run starts.
///
/// # Errors
/// As [`agent_message_prepare_retry_impl`] and [`agent_run_start_impl`]; [`AppError::Conflict`]
/// when an undo stops at a changed file.
pub async fn agent_message_retry_impl(
    state: &AppState,
    session_id: &str,
    message_id: &str,
    undo_first: bool,
) -> Result<AgentRunStarted, AppError> {
    let (id, message, owner) = (session_id.to_owned(), message_id.to_owned(), state.clone());
    let (user, head) = blocking(state, move |state| {
        let head = load_session(state, &id)?.head_message_id;
        let user = agent_message_prepare_retry_impl(&owner, &id, &message)?;
        if undo_first && let Err(error) = undo_turn_changes(state, &id, &user.id) {
            if let Some(head) = &head {
                let _ = state.store().set_head(&id, head);
            }
            return Err(error);
        }
        Ok((user, head))
    })
    .await??;
    launch_stored(state, session_id, user, head).await
}

/// Starts the run for a message `prepare_*` stored; on failure the head goes back to where it was.
async fn launch_stored(
    state: &AppState,
    session_id: &str,
    user: AgentMessage,
    previous_head: Option<String>,
) -> Result<AgentRunStarted, AppError> {
    super::board::session_seen(state, session_id).await;
    match launch(state, session_id, Turn::Stored(Box::new(user))).await {
        Ok(started) => Ok(started),
        Err(error) => {
            if let Some(head) = previous_head {
                let id = session_id.to_owned();
                let _ = blocking(state, move |state| state.store().set_head(&id, &head)).await;
            }
            Err(error)
        }
    }
}

/// Subscribes `sink` to a run's events or a chat's runs; see [`crate::agent::registry`]. A chat
/// subscription clears the chat's unseen-failure mark.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown or expired run; see `RunRegistry::subscribe`.
pub fn agent_run_subscribe_impl(
    state: &AppState,
    target: &AgentSubscribeTarget,
    from_seq: Option<u64>,
    sink: Arc<dyn EventSink<AgentEnvelope>>,
) -> Result<u64, AppError> {
    let id = state.agent.runs.subscribe(target, from_seq, sink)?;
    super::board::target_seen(state, target);
    state.agent.publish_attention();
    Ok(id)
}

/// Ends a subscription; returns whether it existed.
pub fn agent_run_unsubscribe_impl(state: &AppState, subscription_id: u64) -> bool {
    let existed = state.agent.runs.unsubscribe(subscription_id);
    state.agent.publish_attention();
    existed
}

/// Subscribes `sink` to the attention snapshots; `from_seq: None` replays the latest one.
///
/// # Errors
/// [`AppError::Conflict`] when the hub has too many subscribers.
pub fn agent_attention_subscribe_impl(
    state: &AppState,
    from_seq: Option<u64>,
    sink: Arc<dyn EventSink<Sequenced<crate::agent::attention::AgentAttention>>>,
) -> Result<u64, AppError> {
    super::board::ensure_seeded(state);
    let hub = &state.agent.attention_hub;
    let from = from_seq.unwrap_or_else(|| hub.last_seq().saturating_sub(1));
    hub.subscribe(Some(from), sink)
}

/// Ends an attention subscription.
pub fn agent_attention_unsubscribe_impl(state: &AppState, subscription_id: u64) -> bool {
    state.agent.attention_hub.unsubscribe(subscription_id)
}

/// Stops a run (and its sub-agents); `false` when the run is unknown or already ended. The run
/// ends `Cancelled`: open tool calls are answered, shell commands are killed with their process
/// groups, a pending approval or cost pause is released.
pub fn agent_run_stop_impl(state: &AppState, run_id: &str) -> bool {
    let Some(run) = state
        .agent
        .runs
        .get(run_id)
        .filter(|run| run.status().is_active())
    else {
        return false;
    };
    run.cancel.cancel();
    true
}

/// Stops every active top-level run; returns how many were stopped.
pub fn agent_run_stop_all_impl(state: &AppState) -> u32 {
    let mut stopped = 0;
    for run in state.agent.runs.active_runs() {
        if run.kind != AgentRunKind::Subagent && run.status().is_active() {
            run.cancel.cancel();
            stopped += 1;
        }
    }
    stopped
}

/// Sends a message into a running turn: it is stored as a steer message at the next step boundary
/// and the calls still waiting for approval are superseded. `false` when the run is unknown or
/// ended.
///
/// # Errors
/// [`AppError::Validation`] for a blank message or an attachment the model cannot take.
pub fn agent_run_steer_impl(
    state: &AppState,
    run_id: &str,
    text: &str,
    attachment_ids: &[String],
    skills: Vec<String>,
    contexts: &[ContextRef],
) -> Result<bool, AppError> {
    let mut parts = if contexts.is_empty() || !text.trim().is_empty() || !attachment_ids.is_empty()
    {
        user_parts(state, text, attachment_ids)?
    } else {
        Vec::new()
    };
    parts.extend(super::context_payload::context_parts(
        state,
        contexts,
        ContextUse::Mention,
    )?);
    let Some(run) = state
        .agent
        .runs
        .get(run_id)
        .filter(|run| run.status().is_active())
    else {
        return Ok(false);
    };
    run.push_steer(SteerMessage { parts, skills });
    // A sub-agent waiting for an approval ends with the steer: it is stopped and the parent's
    // `task` call ends superseded, so the parent reads the new message.
    for child in state.agent.runs.active_runs() {
        if child.parent_run_id.as_deref() == Some(run_id)
            && child.status() == AgentRunStatus::WaitingApproval
        {
            if let Some(call_id) = child.summary().parent_call_id {
                run.mark_superseded(&call_id);
            }
            child.cancel.cancel();
        }
    }
    Ok(true)
}

/// Answers the cost-cap pause of a run: `proceed` runs on (the next pause comes one more cap
/// later), otherwise the run ends. `false` when the run is not paused.
pub fn agent_run_continue_impl(state: &AppState, run_id: &str, proceed: bool) -> bool {
    let decision = if proceed {
        CostDecision::Continue
    } else {
        CostDecision::Stop
    };
    let gates = &state.agent.gates;
    gates
        .open_gate(run_id, GateKind::Cost)
        .is_some_and(|gate_id| gates.deliver(run_id, &gate_id, GateAnswer::Cost(decision)))
}

/// The runs matching `filter`, newest first: live runs from the registry (with their event
/// position) merged with the stored rows.
///
/// # Errors
/// [`AppError::Database`] when the store cannot be read.
pub fn agent_run_list_impl(
    state: &AppState,
    filter: Option<AgentRunListFilter>,
) -> Result<Vec<AgentRunSummary>, AppError> {
    let filter = filter.unwrap_or_default();
    let limit = filter
        .limit
        .unwrap_or(RUN_LIST_DEFAULT)
        .clamp(1, RUN_LIST_MAX);
    let mut rows = state.agent.runs.summaries(&filter);
    let stored = state
        .store()
        .list_agent_runs(filter.session_id.as_deref(), filter.active_only, limit)
        .map_err(command_error)?;
    for row in stored {
        if !rows.iter().any(|live| live.run_id == row.run_id) {
            rows.push(row);
        }
    }
    rows.sort_by(|a, b| (b.started_at_ms, &b.run_id).cmp(&(a.started_at_ms, &a.run_id)));
    rows.truncate(limit as usize);
    Ok(rows)
}

/// Lets a paused queue run on and starts its first message when the chat is idle; returns the
/// queue that remains.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown chat.
pub async fn agent_queue_resume_impl(
    state: &AppState,
    session_id: &str,
) -> Result<Vec<QueuedTurn>, AppError> {
    let id = session_id.to_owned();
    blocking(state, move |state| load_session(state, &id).map(|_| ())).await??;
    state.agent.runs.resume_queue(session_id);
    if state.agent.runs.active_for_session(session_id).is_none() {
        dispatch_next(state, session_id).await;
    } else {
        // The run's stream shows that the queue is no longer paused.
        let id = session_id.to_owned();
        blocking(state, move |state| queue_changed(state, &id)).await?;
    }
    let id = session_id.to_owned();
    blocking(state, move |state| {
        state.store().list_queue(&id).map_err(command_error)
    })
    .await?
}

/// Starts the chat's next queued message unless the queue is paused or empty. A start that fails
/// puts the message back and pauses the queue, so nothing is lost.
///
/// Boxed: a run's end starts the next run, and the box is what lets the compiler see that the
/// recursive chain is `Send`.
pub(super) fn dispatch_next(state: &AppState, session_id: &str) -> BoxFuture<'static, ()> {
    let (state, session_id) = (state.clone(), session_id.to_owned());
    Box::pin(async move {
        if state.agent.runs.queue_paused(&session_id) {
            return;
        }
        let id = session_id.clone();
        let popped = blocking(&state, move |state| state.store().pop_queue_front(&id))
            .await
            .ok()
            .and_then(Result::ok)
            .flatten();
        let Some(turn) = popped else { return };
        let content = QueuedContent {
            parts: turn.parts,
            skills: turn.skills,
            contexts: turn.contexts,
        };
        let (owner, queued) = (state.clone(), content.clone());
        let parts = blocking(&state, move |_| queued_parts(&owner, &queued))
            .await
            .and_then(|parts| parts);
        let started = match parts {
            Ok(parts) => {
                let skills = content.skills.clone();
                launch(&state, &session_id, Turn::New { parts, skills }).await
            }
            Err(error) => Err(error),
        };
        if started.is_err() {
            requeue_front(&state, &session_id, content).await;
        }
    })
}

/// The parts a queued message is sent with: its own, then the objects it mentions, read now.
pub(super) fn queued_parts(
    state: &AppState,
    content: &QueuedContent,
) -> Result<Vec<HubPart>, AppError> {
    let contexts: Vec<ContextRef> = content
        .contexts
        .iter()
        .filter_map(|value| serde_json::from_value(value.clone()).ok())
        .collect();
    let mut parts = content.parts.clone();
    parts.extend(super::context_payload::context_parts(
        state,
        &contexts,
        ContextUse::Mention,
    )?);
    Ok(parts)
}

/// Puts `content` back at the front of the chat's queue and pauses it: a start that failed, or a
/// steer the run ended before taking, waits for the user instead of being lost or sent on its own.
pub(super) async fn requeue_front(state: &AppState, session_id: &str, content: QueuedContent) {
    let id = session_id.to_owned();
    let _ = blocking(state, move |state| {
        state.store().requeue_front(&id, &content)
    })
    .await;
    state.agent.runs.pause_queue(session_id);
}

/// The chat's queue, for the `QueuedUpdated` event.
fn queue_of(state: &AppState, session_id: &str) -> Vec<QueuedTurn> {
    state.store().list_queue(session_id).unwrap_or_default()
}

/// The chat's queue changed (a message added, edited, removed, moved or cleared): tells the
/// subscribers of the chat's current run. Call it with the store released. A chat with no run has
/// no stream; its next read of the queue shows the change.
pub fn queue_changed(state: &AppState, session_id: &str) {
    if let Some(run) = state.agent.runs.active_for_session(session_id) {
        run.emit(AgentEvent::QueuedUpdated {
            session_id: session_id.to_owned(),
            queued: queue_of(state, session_id),
            paused: state.agent.runs.queue_paused(session_id),
        });
    }
}

/// Everything a run needs besides the loop, built once at its start.
pub(super) struct Prepared {
    pub(super) host: ServiceHost,
    pub(super) input: RunInput,
}

/// Resolves the model, the tools and the prompt of a run for `session`; `role` says whether the
/// chat's own agent or a `task` sub-agent runs (it changes the tools and the prompt).
pub(super) async fn prepare(
    state: &AppState,
    run: &Arc<ActiveRun>,
    session: &AgentSession,
    slot: Option<crate::agent::registry::SlotGuard>,
    role: Role,
) -> Result<Prepared, AppError> {
    let target = session
        .target
        .clone()
        .ok_or_else(super::sessions::no_model)?;
    let settings = state.settings();
    let model = resolve_model(state, &target).await;
    let resolved = resolve(state, session, &model, role, true).await;
    super::branch_state::on_run_start(state, &session.id, resolved.branch.as_deref());
    if !resolved.connecting.is_empty() {
        run.emit(AgentEvent::McpConnecting {
            server_names: resolved.connecting.clone(),
        });
    }
    let secrets = Arc::clone(state.secrets());
    let needles =
        expand_needles(blocking(state, move |_| secrets.active_redaction_needles()).await?);
    let agent = &settings.ai.agent;
    let input = RunInput {
        target: chat_target(&target, &agent.fallbacks, run),
        model_label: target.label(),
        system: Some(resolved.system.clone()),
        tools: resolved.specs.clone(),
        native_web_search: resolved.native_web_search,
        reasoning: session.reasoning_effort,
        max_iterations: agent.max_tool_iterations,
        compaction: CompactionPolicy {
            auto: agent.auto_compact,
            threshold_pct: agent.compact_threshold_pct,
            keep_turns: usize::from(agent.keep_recent_turns),
        },
        cost_caps: cost_caps(&settings),
        model: model.clone(),
        wrap_up_last_step: agent.wrap_up_last_step,
        prompt_cache: (agent.prompt_cache != pingdeck_model::settings::PromptCacheMode::Off).then(
            || match (&session.parent_session_id, role) {
                // Parallel sub-agents of the same kind share a stable cache key so their identical
                // system prompt + tool definitions hit provider KV prompt caches.
                (Some(parent_id), Role::Subagent(kind)) => {
                    let tag = match kind {
                        crate::agent_tools::subagent::SubagentKind::Explore => "explore",
                        crate::agent_tools::subagent::SubagentKind::General => "general",
                    };
                    format!("{parent_id}:sub:{tag}")
                }
                _ => session.id.clone(),
            },
        ),
        max_run_tokens: agent.max_run_tokens,
    };
    let host = ServiceHost::new(HostParts {
        state: state.clone(),
        run: Arc::clone(run),
        session: session.clone(),
        settings,
        resolved,
        role,
        model_label: target.label(),
        model,
        needles,
        slot,
    });
    Ok(Prepared { host, input })
}

/// Drives one run from `Queued` to its end.
async fn drive(state: AppState, begun: Begun) {
    let run = Arc::clone(&begun.run);
    let outcome = execute(&state, &begun).await;
    finish(&state, &run, outcome).await;
}

async fn execute(state: &AppState, begun: &Begun) -> Result<AgentRunResult, AppError> {
    let run = &begun.run;
    let limit = || usize::from(state.settings().ai.agent.max_concurrent_runs);
    let slot = state
        .agent
        .runs
        .acquire_slot(limit, &run.cancel)
        .await
        .ok_or_else(AppError::cancelled)?;
    // A cap the model's price cannot reach is said up front.
    let cap_inapplicable = match &begun.session.target {
        Some(target) if !cost_caps(&state.settings()).is_empty() => matches!(
            resolve_model(state, target).await.price,
            crate::llm::usage::PriceResolution::Unknown(_)
        )
        .then_some(true),
        _ => None,
    };
    let summary = run.update(|summary| {
        summary.status = AgentRunStatus::Running;
        summary.cap_inapplicable = cap_inapplicable;
    });
    persist_run(state, summary.clone()).await;
    run.emit(AgentEvent::RunStarted {
        run: Box::new(summary),
    });
    let (session, names) = (begun.session.clone(), begun.user.meta.skills.clone());
    let skills_undelivered = blocking(state, move |state| {
        super::resolve::undelivered_skills(state, &session, &names)
    })
    .await?;
    run.emit(AgentEvent::UserMessage {
        message: Box::new(begun.user.clone()),
        skills_undelivered,
    });
    if let Some(title) = &begun.title {
        run.emit(AgentEvent::SessionTitleUpdated {
            session_id: begun.session.id.clone(),
            title: title.clone(),
        });
    }
    let session_id = begun.session.id.clone();
    let queued = blocking(state, move |state| queue_of(state, &session_id)).await?;
    run.emit(AgentEvent::QueuedUpdated {
        session_id: begun.session.id.clone(),
        queued,
        paused: state.agent.runs.queue_paused(&begun.session.id),
    });
    state.agent.publish_attention();
    let prepared = prepare(state, run, &begun.session, Some(slot), Role::Main).await?;
    let outcome = run_agent(
        &state.llm_client().for_session(&begun.session.id),
        &prepared.host,
        &prepared.input,
        &run.cancel,
    )
    .await;
    prepared.host.report_untracked_changes().await;
    outcome
}

/// Ends a run: final status in the row, the queue's fate, the one terminal event, the registry.
pub(super) async fn finish(
    state: &AppState,
    run: &Arc<ActiveRun>,
    outcome: Result<AgentRunResult, AppError>,
) {
    let (status, stop_reason, error) = match &outcome {
        Ok(result) if result.stop_reason == AgentStopReason::CostLimit => (
            AgentRunStatus::Cancelled,
            result.stop_reason.as_str(),
            Some((
                "Cancelled".to_owned(),
                "Stopped at the cost limit".to_owned(),
            )),
        ),
        Ok(result) => (AgentRunStatus::Completed, result.stop_reason.as_str(), None),
        Err(error) if error.is_cancelled() => (
            AgentRunStatus::Cancelled,
            "cancelled",
            Some((error_kind_name(error), error.message().to_owned())),
        ),
        Err(error) => (
            AgentRunStatus::Failed,
            "failed",
            Some((error_kind_name(error), error.message().to_owned())),
        ),
    };
    let failure = run.take_failure();
    let summary = run.update(|summary| {
        summary.status = status;
        summary.finished_at_ms = Some(current_time_ms());
        summary.stop_reason = Some(stop_reason.to_owned());
        summary.failure.clone_from(&failure);
        if let Some((kind, message)) = &error {
            summary.error_kind = Some(kind.clone());
            summary.error = Some(message.clone());
        }
    });
    persist_run(state, summary).await;
    if let Ok(result) = &outcome
        && !result.message_id.is_empty()
    {
        // The chat shows how the turn ended on its last answer, also after a relaunch.
        let (message_id, reason) = (result.message_id.clone(), result.stop_reason);
        let _ = blocking(state, move |state| {
            state.store().set_message_stop_reason(&message_id, reason)
        })
        .await;
    }
    let (awake_run, flagged) = (Arc::clone(run), run.approval_timed_out());
    let _ = blocking(state, move |state| {
        state
            .store()
            .set_agent_run_extras(&awake_run.run_id, None, awake_run.awake_ms(), flagged)
    })
    .await;

    let main = run.kind == AgentRunKind::Main;
    if main {
        // Steers the run ended before taking wait at the front of the queue.
        for steer in run.take_steer().into_iter().rev() {
            let (id, content) = (
                run.session_id.clone(),
                QueuedContent {
                    parts: steer.parts,
                    skills: steer.skills,
                    contexts: Vec::new(),
                },
            );
            let _ = blocking(state, move |state| {
                state.store().requeue_front(&id, &content)
            })
            .await;
        }
    }
    // A turn that did not end with a finished answer (failed, stopped, the step limit, the output
    // cap) leaves the queue waiting for the user.
    let held = matches!(&outcome, Ok(result) if result.stop_reason.pauses_queue());
    if main && (status != AgentRunStatus::Completed || held) {
        let id = run.session_id.clone();
        let queued = blocking(state, move |state| queue_of(state, &id))
            .await
            .unwrap_or_default();
        if !queued.is_empty() {
            state.agent.runs.pause_queue(&run.session_id);
            run.emit(AgentEvent::QueuedUpdated {
                session_id: run.session_id.clone(),
                queued,
                paused: true,
            });
        }
    }
    let final_summary = run.summary();
    if let Some((kind, message)) = &error {
        run.emit(AgentEvent::Error {
            run_id: run.run_id.clone(),
            error_kind: kind.clone(),
            message: message.clone(),
            failure: final_summary.failure.clone(),
        });
    } else if let Ok(result) = &outcome {
        run.emit(AgentEvent::Done {
            run: Box::new(final_summary),
            stop_reason: result.stop_reason,
        });
    }
    state.agent.runs.finish(&run.run_id);
    if main {
        record_end(state, run, status).await;
    }
    state.agent.publish_attention();
    if main && status == AgentRunStatus::Completed && !held && !run.handed_over() {
        dispatch_next(state, &run.session_id).await;
    }
}

/// Notes that a top-level run ended: the away-from-app signal and, unless the user watches the
/// chat, the "Review" mark that lasts until the chat is opened.
async fn record_end(state: &AppState, run: &Arc<ActiveRun>, status: AgentRunStatus) {
    state.agent.runs.push_finished(FinishedRun {
        run_id: run.run_id.clone(),
        session_id: run.session_id.clone(),
        status,
        duration_ms: run.awake_ms(),
        finished_at_ms: current_time_ms(),
    });
    if state.agent.runs.watchers(&run.session_id) == 0 {
        state.agent.runs.mark_unseen(&run.run_id, &run.session_id);
    } else {
        let id = run.run_id.clone();
        let _ = blocking(state, move |state| {
            state.store().mark_agent_run_seen(&id, current_time_ms())
        })
        .await;
    }
}

/// Summarises the chat's older history now. The run is a `Background` run of its own:
/// it appears in the run list and its events (`RunStarted`, `ContextCompacted`, `Done`) stream to
/// subscribers. Returns its final summary.
///
/// # Errors
/// [`AppError::Conflict`] while the chat has an active run, [`AppError::Validation`] when there
/// is nothing older to summarise, the summarising call's error otherwise.
pub async fn agent_context_compact_impl(
    state: &AppState,
    session_id: &str,
) -> Result<AgentRunSummary, AppError> {
    let id = session_id.to_owned();
    let (run, session) = blocking(state, move |state| {
        let session = load_session(state, &id)?;
        let target = session
            .target
            .clone()
            .ok_or_else(super::sessions::no_model)?;
        let mut summary = initial_summary(&id, AgentRunKind::Background, &target);
        summary.status = AgentRunStatus::Running;
        let run = ActiveRun::new(summary, CancelToken::new());
        state.agent.runs.register(&run)?;
        if let Err(error) = state.store().insert_agent_run(&run.summary()) {
            state.agent.runs.discard(&run.run_id);
            return Err(command_error(error));
        }
        Ok((run, session))
    })
    .await??;
    run.emit(AgentEvent::RunStarted {
        run: Box::new(run.summary()),
    });
    state.agent.publish_attention();
    let outcome = compact_now(state, &run, &session).await;
    let result = outcome.map(|compacted| AgentRunResult {
        message_id: compacted.summary_message_id,
        stop_reason: AgentStopReason::Completed,
        iterations: 0,
        usage: crate::llm::hub::HubUsage::default(),
    });
    let failure = result.as_ref().err().cloned();
    finish(state, &run, result).await;
    failure.map_or_else(|| Ok(run.summary()), Err)
}

async fn compact_now(
    state: &AppState,
    run: &Arc<ActiveRun>,
    session: &AgentSession,
) -> Result<compaction::Compacted, AppError> {
    let target = session
        .target
        .clone()
        .ok_or_else(super::sessions::no_model)?;
    let settings = state.settings();
    let model = resolve_model(state, &target).await;
    let agent = &settings.ai.agent;
    // A compaction request carries no tools and no prompt of its own.
    let input = RunInput {
        target: chat_target(&target, &agent.fallbacks, run),
        model_label: target.label(),
        system: None,
        tools: Vec::new(),
        native_web_search: false,
        reasoning: None,
        max_iterations: 1,
        compaction: CompactionPolicy {
            auto: false,
            threshold_pct: agent.compact_threshold_pct,
            keep_turns: usize::from(agent.keep_recent_turns),
        },
        cost_caps: Vec::new(),
        model: model.clone(),
        wrap_up_last_step: false,
        prompt_cache: None,
        max_run_tokens: None,
    };
    let host = ServiceHost::new(HostParts {
        state: state.clone(),
        run: Arc::clone(run),
        session: session.clone(),
        settings,
        // The skills too, or the summariser reads attached ones as unavailable.
        resolved: Resolved {
            skills: super::resolve::session_skills(state, session).await,
            ..Resolved::default()
        },
        role: Role::Main,
        model_label: target.label(),
        model,
        needles: Vec::new(),
        slot: None,
    });
    let entries = host.load_history().await?;
    compaction::compact(
        &state.llm_client().for_session(&session.id),
        &host,
        &input,
        &entries,
        &run.cancel,
    )
    .await?
    .ok_or_else(|| AppError::validation("There is nothing older to summarise yet"))
}
