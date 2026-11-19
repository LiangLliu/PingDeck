//! Running one `task` sub-agent from creation to its report.
//!
//! Single responsibility: turn a [`ChildRequest`] into a finished child run. The child is a chat of
//! its own (hidden, linked to the parent chat, run and call; it copies the parent's model, folder
//! and permission mode) with a run of kind `Subagent` whose cancellation hangs off the parent's, so
//! stopping the parent, deleting the parent chat or stopping the child alone all end it. It takes
//! no top-level concurrency slot (a parent waiting for its children would hold one). The loop,
//! approvals, cost and persistence are the ones of any run ([`prepare`], [`finish`]); this module
//! adds the parent's view: `SubagentStarted`, a summary of the child's events (no text deltas),
//! `SubagentFinished`. The child's cost already reaches the parent through the run host
//! (`ServiceHost::record_usage` adds it to the parent run as it is spent).

use std::sync::Arc;

use crate::agent::model::{
    AgentEvent, AgentMessage, AgentRole, AgentRunKind, AgentRunResult, AgentRunStatus,
    AgentSession, AgentStopReason, AgentTarget, AttachmentKind, SubagentRole,
};
use crate::agent::registry::ActiveRun;
use crate::agent::runner::run_agent;
use crate::agent_tools::subagent::{ChildEnd, ChildReport, ChildRequest, SubagentKind, TaskBrief};
use crate::error::AppError;
use crate::events::{EventSink, Sequenced};
use crate::llm::hub::{HubImage, HubPart, HubUsage, text_of};

use super::host::{blocking, new_message, persist_run, store_user_message};
use super::roles::role_target;
use super::run::{finish, initial_summary, prepare};
use super::sessions::load_session;
use super::tools::Role;
use crate::app::factory_service::new_entity_id;
use crate::app::{AppState, command_error, current_time_ms};

/// The model a child with a role runs on: the role, the session target and the model's name
/// form for the report's first line.
struct RoleModel {
    role: SubagentRole,
    target: AgentTarget,
    label: String,
}

/// The image parts of `ids`, each an image attachment of the chat `session_id` (a vision job may
/// look only at what the user put into this chat).
fn image_parts(
    state: &AppState,
    session_id: &str,
    ids: &[String],
) -> Result<Vec<HubPart>, AppError> {
    if ids.is_empty() {
        return Ok(Vec::new());
    }
    let attachments = state
        .store()
        .list_session_attachments(session_id)
        .map_err(command_error)?;
    ids.iter()
        .map(|id| {
            let found = attachments
                .iter()
                .find(|attachment| attachment.id == *id)
                .ok_or_else(|| AppError::validation(format!("this chat has no image {id}")))?;
            if found.kind != AttachmentKind::Image || found.removed {
                return Err(AppError::validation(format!(
                    "{} is not an image the sub-agent can see",
                    found.name
                )));
            }
            Ok(HubPart::Image {
                image: HubImage::AttachmentRef {
                    attachment_id: found.id.clone(),
                    media_type: found.media_type.clone(),
                },
            })
        })
        .collect()
}

/// `micro` millionths of `currency`, to four decimals (`USD 0.0021`).
fn spend_text(currency: &str, micro: i64) -> String {
    let micro = micro.max(0);
    format!(
        "{currency} {}.{:04}",
        micro / 1_000_000,
        (micro % 1_000_000) / 100
    )
}

/// The first line of a role's report: role, model and what the child spent per currency.
fn origin_line(role: &RoleModel, run: &ActiveRun) -> String {
    let spent: Vec<String> = run
        .spend()
        .into_iter()
        .filter(|(_, micro)| *micro > 0)
        .map(|(currency, micro)| spend_text(&currency, micro))
        .collect();
    let mut fields = vec![role.role.as_str().to_owned(), role.label.clone()];
    if !spent.is_empty() {
        fields.push(spent.join(" + "));
    }
    format!("[{}]", fields.join(" · "))
}

/// A child that is registered and stored but has not started.
struct Begun {
    run: Arc<ActiveRun>,
    session: AgentSession,
    user: AgentMessage,
}

/// The events of a child the parent's subscribers see: tool steps, approvals, file changes, notes,
/// the child's task list and its cost-cap pause. Never a text, reasoning or tool-output delta
/// (the full stream is the child's own), and a tool result without its content.
fn summarize(event: AgentEvent) -> Option<AgentEvent> {
    match event {
        AgentEvent::ToolResult {
            call_id,
            name,
            status,
            duration_ms,
            meta,
            exit_code,
            ..
        } => Some(AgentEvent::ToolResult {
            call_id,
            name,
            status,
            parts: Vec::new(),
            duration_ms,
            meta,
            exit_code,
        }),
        AgentEvent::ToolCallStarted { .. }
        | AgentEvent::ToolApprovalRequired { .. }
        | AgentEvent::ToolApprovalResolved { .. }
        | AgentEvent::FileChanged { .. }
        | AgentEvent::Notice { .. }
        | AgentEvent::TodosUpdated { .. }
        | AgentEvent::CostLimitReached { .. } => Some(event),
        _ => None,
    }
}

/// Forwards the child's summarised events to the parent's stream as `SubagentEvent`s.
fn forwarder(
    parent: Arc<ActiveRun>,
    parent_call_id: String,
    child_run_id: String,
) -> Arc<dyn EventSink<Sequenced<AgentEvent>>> {
    Arc::new(move |envelope: Sequenced<AgentEvent>| {
        if let Some(event) = summarize(envelope.event) {
            parent.emit(AgentEvent::SubagentEvent {
                parent_call_id: parent_call_id.clone(),
                child_run_id: child_run_id.clone(),
                event: Box::new(event),
            });
        }
        true
    })
}

/// Creates the child chat, registers its run under the parent's cancellation and stores the
/// brief as its first message. Blocking (the store).
fn begin(
    state: &AppState,
    parent: &ActiveRun,
    call_id: &str,
    brief: &TaskBrief,
    role: Option<&RoleModel>,
) -> Result<Begun, AppError> {
    let source = load_session(state, &parent.session_id)?;
    let target = match role {
        Some(role) => role.target.clone(),
        None => source
            .target
            .clone()
            .ok_or_else(|| AppError::validation("choose a model for this chat first"))?,
    };
    let images = image_parts(state, &source.id, &brief.images)?;
    let now = current_time_ms();
    let session = AgentSession {
        id: new_entity_id("as"),
        title: brief.description.clone(),
        target: Some(target.clone()),
        workspace_dir: source.workspace_dir.clone(),
        permission_mode: source.permission_mode,
        reasoning_effort: source.reasoning_effort,
        disabled_tools: source.disabled_tools.clone(),
        disabled_skills: source.disabled_skills.clone(),
        web_enabled: source.web_enabled,
        title_locked: true,
        head_message_id: None,
        pinned_at_ms: None,
        archived_at_ms: None,
        deleted_at_ms: None,
        forked_from: None,
        forked_from_message_id: None,
        parent_session_id: Some(source.id.clone()),
        parent_run_id: Some(parent.run_id.clone()),
        todos: Vec::new(),
        total_usage: HubUsage::default(),
        total_cost_micro_usd: 0,
        last_context_tokens: 0,
        pingdeck_workspace_id: source.pingdeck_workspace_id.clone(),
        pingdeck_environment_id: source.pingdeck_environment_id.clone(),
        pingdeck_workspace_name: None,
        notice: None,
        // A sub-agent sees only its task, not the chat; it gets the user's global instructions
        // from the settings but not the chat's own.
        instructions: None,
        created_at_ms: now,
        updated_at_ms: now,
    };
    state
        .store()
        .save_agent_session(&session)
        .map_err(command_error)?;
    let started = (|| {
        let mut summary = initial_summary(&session.id, AgentRunKind::Subagent, &target);
        summary.parent_run_id = Some(parent.run_id.clone());
        summary.parent_call_id = Some(call_id.to_owned());
        summary.role = role.map(|role| role.role);
        let run = ActiveRun::new(summary, parent.cancel.child());
        state.agent.runs.register(&run)?;
        let user = new_message(
            &session.id,
            Some(&run.run_id),
            AgentRole::User,
            std::iter::once(HubPart::text(brief.prompt.clone()))
                .chain(images)
                .collect(),
        );
        // The run row first, with the store released before the message takes it again.
        let inserted = state.store().insert_agent_run(&run.summary());
        let stored = inserted
            .map_err(command_error)
            .and_then(|()| store_user_message(state, &user));
        match stored {
            Ok(user) => Ok((run, user)),
            Err(error) => {
                state.agent.runs.discard(&run.run_id);
                Err(error)
            }
        }
    })();
    match started {
        Ok((run, user)) => Ok(Begun { run, session, user }),
        Err(error) => {
            // The chat never ran; it goes with the next purge instead of lingering unseen.
            let _ = state.store().soft_delete_session(&session.id, now);
            Err(error)
        }
    }
}

/// The child's loop without a top-level slot.
async fn execute(
    state: &AppState,
    begun: &Begun,
    kind: SubagentKind,
) -> Result<AgentRunResult, AppError> {
    let run = &begun.run;
    let summary = run.update(|summary| summary.status = AgentRunStatus::Running);
    persist_run(state, summary.clone()).await;
    run.emit(AgentEvent::RunStarted {
        run: Box::new(summary),
    });
    run.emit(AgentEvent::UserMessage {
        message: Box::new(begun.user.clone()),
        skills_undelivered: Vec::new(),
    });
    state.agent.publish_attention();
    if run.cancel.is_cancelled() {
        return Err(AppError::cancelled());
    }
    let prepared = prepare(state, run, &begun.session, None, Role::Subagent(kind)).await?;
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

fn end_of(outcome: &Result<AgentRunResult, AppError>) -> ChildEnd {
    match outcome {
        Ok(result) => match result.stop_reason {
            // A report cut short by the output cap is still the child's answer.
            AgentStopReason::Completed
            | AgentStopReason::OutputLimit
            | AgentStopReason::ContextFull => ChildEnd::Completed,
            AgentStopReason::MaxIterations => ChildEnd::StepLimit,
            AgentStopReason::NoProgress => {
                ChildEnd::Failed("it kept repeating the same call without progress".to_owned())
            }
            AgentStopReason::CostLimit => ChildEnd::CostLimit,
        },
        Err(error) if error.is_cancelled() => ChildEnd::Cancelled,
        Err(error) => ChildEnd::Failed(error.message().to_owned()),
    }
}

/// The child's last assistant message that has text; empty when it wrote none.
async fn last_text(state: &AppState, session_id: &str) -> String {
    let id = session_id.to_owned();
    let history = blocking(state, move |state| state.store().list_model_history(&id))
        .await
        .ok()
        .and_then(Result::ok)
        .unwrap_or_default();
    history
        .iter()
        .rev()
        .filter(|message| message.role == AgentRole::Assistant)
        .map(|message| text_of(&message.parts))
        .find(|text| !text.trim().is_empty())
        .unwrap_or_default()
}

/// Runs the child to its end and reports to the parent. Spawned so that it also finishes (row,
/// terminal event, `SubagentFinished`) when the parent's call is abandoned after a stop.
async fn drive(
    state: AppState,
    parent: Arc<ActiveRun>,
    call_id: String,
    kind: SubagentKind,
    begun: Begun,
    role: Option<RoleModel>,
) -> ChildReport {
    let run = Arc::clone(&begun.run);
    parent.emit(AgentEvent::SubagentStarted {
        parent_call_id: call_id.clone(),
        child_run: Box::new(run.summary()),
        child_session_id: begun.session.id.clone(),
    });
    let forwarding = run
        .hub
        .subscribe(
            None,
            forwarder(Arc::clone(&parent), call_id.clone(), run.run_id.clone()),
        )
        .ok();
    let session_id = begun.session.id.clone();
    // A panic inside the loop (a dependency's) must still end the child's run and report.
    let loop_state = state.clone();
    let looped = tokio::spawn(async move { execute(&loop_state, &begun, kind).await }).await;
    let outcome =
        looped.unwrap_or_else(|_| Err(AppError::internal("the sub-agent stopped unexpectedly")));
    let end = end_of(&outcome);
    finish(&state, &run, outcome).await;
    if let Some(id) = forwarding {
        run.hub.unsubscribe(id);
    }
    // What the child left running goes with it, however it ended.
    state.agent.processes.kill_session(&session_id).await;
    parent.emit(AgentEvent::SubagentFinished {
        parent_call_id: call_id,
        child_run: Box::new(run.summary()),
    });
    ChildReport {
        end,
        text: last_text(&state, &session_id).await,
        origin: role.as_ref().map(|role| origin_line(role, &run)),
    }
}

/// Runs the sub-agent `request` describes and waits for its report. A child with a role runs on
/// the role's model with the role's agent fallback chain (`ai.agent.fallbacks.<role>`); the role
/// model or one entry of its chain must be callable.
///
/// # Errors
/// The store's error when the child chat or run cannot be created ([`AppError::NotFound`] when the
/// parent chat is gone, [`AppError::Validation`] when it has no model, the role's model cannot be
/// called or an image is not one of the chat's).
pub(crate) async fn run_subagent(
    state: &AppState,
    request: ChildRequest,
) -> Result<ChildReport, AppError> {
    let ChildRequest {
        parent,
        call_id,
        brief,
        role,
    } = request;
    let role = match role {
        Some(role) => {
            let (target, label) = role_target(state, role).await?;
            Some(RoleModel {
                role,
                target,
                label,
            })
        }
        None => None,
    };
    let (begun, role) = {
        let (parent, call_id, brief) = (Arc::clone(&parent), call_id.clone(), brief.clone());
        blocking(state, move |state| {
            begin(state, &parent, &call_id, &brief, role.as_ref()).map(|begun| (begun, role))
        })
        .await??
    };
    tokio::spawn(drive(
        state.clone(),
        parent,
        call_id,
        brief.kind,
        begun,
        role,
    ))
    .await
    .map_err(|_| AppError::internal("the sub-agent stopped unexpectedly"))
}
