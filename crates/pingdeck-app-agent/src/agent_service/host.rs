//! The run loop's environment over a real `AppState`: persistence, permissions, approvals, tool
//! execution, events and the run's concurrency slot.
//!
//! Single responsibility: implement [`RunHost`] for one run. Every store call runs on the blocking
//! pool with a cloned `AppState` (the store lock is taken inside the blocking closure, never held
//! across an `.await`, and the secret store is never called while it is held). The run's status,
//! its summary row and the attention snapshot are kept in step by [`ServiceHost::set_status`].

use std::path::PathBuf;
use std::sync::{Arc, Mutex, PoisonError};
use std::time::Duration;

use serde_json::Value;

use crate::agent::approvals::{ApprovalOutcome, RuleContext};
use crate::agent::attachments::DocumentSupport;
use crate::agent::attention::PendingApprovalSummary;
use crate::agent::context::result_cap_bytes;
use crate::agent::failure::AgentFailure;
use crate::agent::fetch::SeenHosts;
use crate::agent::gate::{
    GateAnswer, GateKind, GateOutcome, GatePayload, GateRequest, PlanDecision,
};
use crate::agent::model::{
    AgentEvent, AgentMessage, AgentMessageMeta, AgentRole, AgentRunStatus, AgentRunSummary,
    AgentSession, AgentToolCallRecord, ApprovalDecision, ApprovalScope, CostShare, GateOutcomeDto,
    ToolErrorCode, coded_error_text,
};
use crate::agent::permissions::{
    Outcome, PermissionAction, PermissionEnv, PermissionQuery, approval_card, evaluate_permission,
    shell_network_allowed, shell_remote_reason,
};
use crate::agent::registry::{ActiveRun, CostDecision, SlotGuard};
use crate::agent::runner::{
    AskInfo, AssistantDraft, CallAudit, CallCost, CostPause, HistoryEntry, ModelInfo, RunClock,
    RunHost, RunOptions, RunTotals, StepConfig, SystemClock, ToolCall, ToolPlan, Verdict,
    add_usage, hub_role,
};
use crate::agent::sandbox::{SandboxKind, SandboxState, SandboxTag, sandbox_status};
use crate::agent::secret_guard::StreamRedactor;
use crate::agent::tool_output_files::{SpillCaps, write_spill};
use crate::agent::tools::shell_meta::SHELL;
use crate::agent::tools::submit_plan::PlanOutcomeKind;
use crate::agent::tools::{
    CallInspection, OutputStream, ToolApprovalPreview, ToolContext, ToolEvent, ToolOutput,
    ToolSource,
};
use crate::agent::tools::{ask_user, submit_plan};
use crate::error::AppError;
use crate::llm::hub::{HubMessage, HubPart, HubUsage};
use crate::secrets::redact_secrets;
use crate::settings::{Settings, ToolApproval};
use crate::workspace::WorkspaceStore;

use super::attachments::resolve_for_model;
use super::changes::{apply_journaled, journals};
use super::resolve::{Resolved, fingerprint, plan_allows, resolve};
use super::tools::Role;
use super::untracked::UntrackedWatch;
use crate::app::factory_service::new_entity_id;
use crate::app::{AppState, command_error, current_time_ms};

/// Why Plan mode refuses a call.
const PLAN_REFUSAL: &str = "Plan mode is read-only";
/// Bytes of one call's output the live view streams; the rest is in the call's result.
const LIVE_OUTPUT_BYTES: usize = 256 * 1024;
/// The note that ends a call's live output once [`LIVE_OUTPUT_BYTES`] streamed.
const LIVE_OUTPUT_CUT: &str = "\n[live output stops here; the full output is in the result]\n";

/// The part of a chunk of `text` the live view still streams when `before` bytes of the call
/// already went: the whole chunk under the budget, its head plus the note at the crossing, nothing
/// after.
fn live_output_share(before: usize, text: String) -> Option<String> {
    if before >= LIVE_OUTPUT_BYTES {
        return None;
    }
    let room = LIVE_OUTPUT_BYTES - before;
    if text.len() <= room {
        return Some(text);
    }
    let mut cut = room;
    while !text.is_char_boundary(cut) {
        cut -= 1;
    }
    Some(format!("{}{LIVE_OUTPUT_CUT}", &text[..cut]))
}

/// Whether a call of the tool `spec` can change the chat's folder without the file journal:
/// processes (`shell` and its kin) and MCP tools.
fn changes_files_unjournaled(spec: &crate::agent::tools::ToolSpec) -> bool {
    spec.class == crate::agent::tools::ToolClass::Exec
        || matches!(spec.source, ToolSource::Mcp { .. })
}

/// An attached image the chat's model cannot read, as a note telling it to hand the image to the
/// vision role; any other part as it is.
fn image_note(part: &HubPart) -> HubPart {
    match part {
        HubPart::Image {
            image: crate::llm::hub::HubImage::AttachmentRef { attachment_id, .. },
        } => HubPart::text(format!(
            "[Image {attachment_id}: you cannot see it. To read it, call task with role \"vision\" and images [\"{attachment_id}\"].]"
        )),
        other => other.clone(),
    }
}

/// How strict a mode is: the smaller, the more the user is asked.
const fn strictness(mode: ToolApproval) -> u8 {
    match mode {
        ToolApproval::Ask => 0,
        ToolApproval::AutoReadOnly => 1,
        ToolApproval::AutoAll => 2,
    }
}

/// The permission mode in force for `session_id` now. A sub-agent's chat is never looser than its
/// parent chat's current mode, so switching the parent to a stricter mode binds a
/// running child at its next call. `None` when the chat is gone.
pub(super) fn session_mode(store: &WorkspaceStore, session_id: &str) -> Option<ToolApproval> {
    let session = store.get_agent_session(session_id).ok().flatten()?;
    let parent = session
        .parent_session_id
        .as_deref()
        .and_then(|id| store.get_agent_session(id).ok().flatten());
    Some(parent.map_or(session.permission_mode, |parent| {
        [session.permission_mode, parent.permission_mode]
            .into_iter()
            .min_by_key(|mode| strictness(*mode))
            .unwrap_or(session.permission_mode)
    }))
}

/// Runs `work` on the blocking pool with a clone of the state.
pub(super) async fn blocking<T: Send + 'static>(
    state: &AppState,
    work: impl FnOnce(&AppState) -> T + Send + 'static,
) -> Result<T, AppError> {
    let state = state.clone();
    tokio::task::spawn_blocking(move || work(&state))
        .await
        .map_err(|error| AppError::internal(format!("store task failed: {error}")))
}

/// A new message with the defaults of a message nobody has stored yet.
pub(super) fn new_message(
    session_id: &str,
    run_id: Option<&str>,
    role: AgentRole,
    parts: Vec<HubPart>,
) -> AgentMessage {
    AgentMessage {
        id: new_entity_id("am"),
        session_id: session_id.to_owned(),
        seq: 0,
        role,
        parts,
        usage: None,
        model: None,
        error: None,
        run_id: run_id.map(str::to_owned),
        parent_id: None,
        branch_index: 0,
        partial: false,
        cost_micro_usd: 0,
        cost_currency: None,
        meta: AgentMessageMeta::default(),
        branch_siblings: Vec::new(),
        attachments: Vec::new(),
        created_at_ms: current_time_ms(),
    }
}

/// Appends a user message to the chat's active path and returns it with its attachments.
pub(super) fn store_user_message(
    state: &AppState,
    message: &AgentMessage,
) -> Result<AgentMessage, AppError> {
    let store = state.store();
    let mut stored = store.append_agent_message(message).map_err(command_error)?;
    let mut linked = store
        .list_message_attachments(std::slice::from_ref(&stored.id))
        .map_err(command_error)?;
    if let Some(attachments) = linked.remove(&stored.id) {
        stored.attachments = attachments;
    }
    Ok(stored)
}

/// Writes the run's summary row; a failure only loses a progress marker.
pub(super) async fn persist_run(state: &AppState, summary: AgentRunSummary) {
    let _ = blocking(state, move |state| state.store().update_agent_run(&summary)).await;
}

/// What the model reads after `submit_plan`, how the gate resolved and whether the run ends after
/// this step. Every text but a cancellation starts with the outcome line the UI stamps the plan
/// block from ([`submit_plan::plan_outcome_line`]).
pub(super) fn plan_answer(outcome: GateOutcome) -> (String, GateOutcomeDto, bool) {
    let lined =
        |kind, mode, body: &str| format!("{}\n{body}", submit_plan::plan_outcome_line(kind, mode));
    match outcome {
        GateOutcome::Answered(GateAnswer::Plan {
            decision: PlanDecision::Approve,
            mode,
            ..
        }) => (
            lined(
                PlanOutcomeKind::Approved,
                mode,
                submit_plan::PLAN_APPROVED_RESULT,
            ),
            GateOutcomeDto::Answered,
            true,
        ),
        GateOutcome::Answered(GateAnswer::Plan { text, .. }) if !text.trim().is_empty() => (
            lined(
                PlanOutcomeKind::Revise,
                None,
                &format!(
                    "{}\n\nUser: {}",
                    submit_plan::PLAN_REVISE_RESULT,
                    text.trim()
                ),
            ),
            GateOutcomeDto::Answered,
            false,
        ),
        GateOutcome::Answered(_) | GateOutcome::Superseded => (
            lined(
                PlanOutcomeKind::Revise,
                None,
                submit_plan::PLAN_REVISE_RESULT,
            ),
            GateOutcomeDto::Answered,
            false,
        ),
        GateOutcome::TimedOut => (
            lined(
                PlanOutcomeKind::TimedOut,
                None,
                submit_plan::PLAN_TIMED_OUT_RESULT,
            ),
            GateOutcomeDto::TimedOut,
            true,
        ),
        GateOutcome::Cancelled => (
            "Cancelled while the plan waited for the user.".to_owned(),
            GateOutcomeDto::Moot,
            false,
        ),
    }
}

fn parse_arguments(text: &str) -> Result<Value, String> {
    if text.trim().is_empty() {
        return Ok(Value::Object(serde_json::Map::new()));
    }
    serde_json::from_str(text).map_err(|error| {
        coded_error_text(
            ToolErrorCode::InvalidArguments,
            &format!("The arguments are not valid JSON: {error}"),
        )
    })
}

/// `low`, `medium` or `high`.
fn risk_name(level: crate::agent::permissions::RiskLevel) -> &'static str {
    use crate::agent::permissions::RiskLevel;
    match level {
        RiskLevel::Low => "low",
        RiskLevel::Medium => "medium",
        RiskLevel::High => "high",
    }
}

/// Characters of a card's one-line summary.
const SUMMARY_CHARS: usize = 120;

/// The first line of `text`, cut to [`SUMMARY_CHARS`] characters.
fn first_line(text: &str) -> String {
    let line = text.lines().next().unwrap_or_default().trim();
    if line.chars().count() > SUMMARY_CHARS {
        let cut: String = line.chars().take(SUMMARY_CHARS).collect();
        format!("{cut}…")
    } else {
        line.to_owned()
    }
}

/// What the board shows of an approval: one plain line, and the whole command, URL or argument
/// text when the call is Low risk (the board offers an inline Allow only then).
fn card_text(call: &ToolCall, ask: &AskInfo) -> (String, Option<String>) {
    let line = match &ask.preview {
        Some(ToolApprovalPreview::Command { cmd, .. }) => format!("Run: {}", first_line(cmd)),
        Some(ToolApprovalPreview::Diff { path, .. }) => format!("Change {path}"),
        Some(ToolApprovalPreview::Url { url }) => format!("{}: {}", call.name, first_line(url)),
        Some(ToolApprovalPreview::Summary { text }) => first_line(text),
        None => format!("{}: {}", call.name, first_line(&call.arguments)),
    };
    let full_text = (ask.risk == "low").then(|| match &ask.preview {
        Some(ToolApprovalPreview::Command { cmd, .. }) => cmd.clone(),
        Some(ToolApprovalPreview::Url { url }) => url.clone(),
        Some(ToolApprovalPreview::Summary { text }) => text.clone(),
        _ => call.arguments.clone(),
    });
    (line, full_text)
}

fn enum_name<T: serde::Serialize>(value: &T) -> String {
    serde_json::to_value(value)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_default()
}

/// What the model may use and what the run knows about its place.
pub(super) struct HostParts {
    pub state: AppState,
    pub run: Arc<ActiveRun>,
    pub session: AgentSession,
    pub settings: Arc<Settings>,
    /// The run's tools, prompt and folder as resolved at its start.
    pub resolved: Resolved,
    /// Whose tools are built: the chat's own agent or a sub-agent.
    pub role: Role,
    pub model_label: String,
    pub model: ModelInfo,
    pub needles: Vec<String>,
    /// The top-level slot the run holds (`None` for a sub-agent or a background run).
    pub slot: Option<SlotGuard>,
}

/// Audit rows (`agent_tool_calls`) that could not be written since the app started.
pub(crate) static AUDIT_WRITE_FAILURES: std::sync::atomic::AtomicU64 =
    std::sync::atomic::AtomicU64::new(0);

/// [`RunHost`] over an `AppState`.
pub(super) struct ServiceHost {
    state: AppState,
    run: Arc<ActiveRun>,
    session: AgentSession,
    settings: Arc<Settings>,
    /// Replaced at a step boundary when the chat's folder, mode, switches or trust changed.
    resolved: Mutex<Arc<Resolved>>,
    role: Role,
    model_label: String,
    model: ModelInfo,
    needles: Vec<String>,
    tool_output_dir: Option<PathBuf>,
    /// The run's top-level slot was handed to this host (it lives in the run, see
    /// [`ActiveRun::begin_wait`]) and goes when the host does.
    owns_slot: bool,
    clock: SystemClock,
    /// The sandbox tier each planned `shell` call was decided under, by call id: the call runs
    /// under that tier, so a status change between the decision and the run cannot turn an
    /// allowed sandboxed call into an unsandboxed one without a card.
    planned_sandbox: Mutex<std::collections::HashMap<String, SandboxKind>>,
    /// Facts of fallback models that answered this run, by `provider_id/model`.
    served_models: Mutex<std::collections::HashMap<String, ModelInfo>>,
    /// The skills each message of the last loaded history used, for a summary to carry on.
    history_skills: Mutex<Vec<super::skill_delivery::UsedSkills>>,
    /// Hosts a `fetch_url` may reach without a card: named by the user, found by
    /// `web_search` or fetched with the user's approval during this run.
    seen_hosts: Mutex<SeenHosts>,
    /// A plan gate closed in a way that ends the run after this step (approved, timed out).
    end_requested: std::sync::atomic::AtomicBool,
    /// The folder listing taken before the run's first call that can change files outside the
    /// journal, reported on at the run's end.
    untracked: UntrackedWatch,
}

impl ServiceHost {
    /// At the run's end: says which files of the folder changed outside the file journal since
    /// the run's first command or MCP call (`untracked_changes`), which the Changes tab cannot
    /// undo.
    pub(super) async fn report_untracked_changes(&self) {
        let chat = self
            .session
            .parent_session_id
            .clone()
            .unwrap_or_else(|| self.session.id.clone());
        if let Some(notice) = self.untracked.report(&self.state, &chat).await {
            self.run.emit(notice);
        }
    }

    pub(super) fn new(parts: HostParts) -> Self {
        let tool_output_dir = parts
            .state
            .data_dirs()
            .map(|dirs| dirs.root.join("tool-output"));
        let owns_slot = parts.slot.is_some();
        if let Some(guard) = parts.slot {
            parts.run.hold_slot(guard);
        }
        Self {
            owns_slot,
            state: parts.state,
            run: parts.run,
            session: parts.session,
            settings: parts.settings,
            resolved: Mutex::new(Arc::new(parts.resolved)),
            role: parts.role,
            model_label: parts.model_label,
            model: parts.model,
            needles: parts.needles,
            tool_output_dir,
            clock: SystemClock,
            planned_sandbox: Mutex::default(),
            served_models: Mutex::default(),
            history_skills: Mutex::default(),
            seen_hosts: Mutex::default(),
            end_requested: std::sync::atomic::AtomicBool::new(false),
            untracked: UntrackedWatch::default(),
        }
    }

    /// `submit_plan`: opens the `Plan` gate with the call's own plan text and waits for the user
    /// under the gate's timeout rule. Approval and a timeout end the run after this step; a
    /// steer is the user's additions and the run goes on.
    async fn submit_plan(&self, call: &ToolCall, arguments: &Value) -> ToolOutput {
        let (plan, title) = match submit_plan::parse(arguments) {
            Ok(parsed) => parsed,
            Err(error) => return ToolOutput::error(error.message()),
        };
        let runtime = &self.state.agent;
        let card =
            submit_plan::plan_card(&self.run.run_id, &call.id, plan, title, current_time_ms());
        let Some(ticket) = runtime.gates.open(GateRequest {
            gate_id: call.id.clone(),
            run_id: self.run.run_id.clone(),
            session_id: self.session.id.clone(),
            kind: GateKind::Plan,
            opened_ms: card.created_at_ms,
            payload: GatePayload::Plan(card.clone()),
        }) else {
            return ToolOutput::error("This plan already waits for the user.");
        };
        self.release_slot();
        self.set_waiting(GateKind::Plan, submit_plan::SUBMIT_PLAN)
            .await;
        self.run.emit(AgentEvent::GateOpened {
            run_id: self.run.run_id.clone(),
            gate: card,
        });
        let timeout = Duration::from_secs(u64::from(self.approval_timeout_s()));
        let outcome = ticket
            .wait(&runtime.gate(&self.run), &self.run.cancel, timeout)
            .await;
        let (text, resolved, ends) = plan_answer(outcome);
        self.run.emit(AgentEvent::GateResolved {
            run_id: self.run.run_id.clone(),
            gate_id: call.id.clone(),
            outcome: resolved,
        });
        if resolved == GateOutcomeDto::Moot {
            runtime.publish_attention();
            return ToolOutput::error(text);
        }
        if ends {
            // The run ends after this step: it gives its wait back without queueing for a slot.
            let _ = self.root().end_wait();
            self.end_requested
                .store(true, std::sync::atomic::Ordering::SeqCst);
        } else if !self.reacquire_slot().await {
            runtime.publish_attention();
            return ToolOutput::error("Cancelled while the plan waited for the user.");
        }
        self.set_status(AgentRunStatus::Running, Some(submit_plan::SUBMIT_PLAN))
            .await;
        ToolOutput::text(text)
    }

    /// `ask_user`: opens the `Question` gate and waits for the user under the gate's timeout rule.
    /// The answer, a skip, a steer (the user wrote instead) or a timeout all go back to the model
    /// as the tool result and the run goes on; none of them approves anything.
    async fn ask_user(&self, call: &ToolCall, arguments: &Value) -> ToolOutput {
        let parsed = match ask_user::parse(arguments) {
            Ok(parsed) => parsed,
            Err(error) => return ToolOutput::error(error.message()),
        };
        let runtime = &self.state.agent;
        let card = ask_user::question_card(&self.run.run_id, &call.id, parsed, current_time_ms());
        let Some(ticket) = runtime.gates.open(GateRequest {
            gate_id: call.id.clone(),
            run_id: self.run.run_id.clone(),
            session_id: self.session.id.clone(),
            kind: GateKind::Question,
            opened_ms: card.created_at_ms,
            payload: GatePayload::Question(card.clone()),
        }) else {
            return ToolOutput::error("This question already waits for the user.");
        };
        let options = match &card.body {
            crate::agent::model::HumanGateBody::Question { options, .. } => options.clone(),
            crate::agent::model::HumanGateBody::Plan { .. } => Vec::new(),
        };
        self.release_slot();
        self.set_waiting(GateKind::Question, ask_user::ASK_USER)
            .await;
        self.run.emit(AgentEvent::GateOpened {
            run_id: self.run.run_id.clone(),
            gate: card,
        });
        let timeout = Duration::from_secs(u64::from(self.approval_timeout_s()));
        let outcome = ticket
            .wait(&runtime.gate(&self.run), &self.run.cancel, timeout)
            .await;
        let (end, resolved) = match &outcome {
            GateOutcome::Answered(GateAnswer::Question(answer)) => (
                Some(ask_user::QuestionEnd::Answered(answer)),
                if answer.skipped {
                    GateOutcomeDto::Skipped
                } else {
                    GateOutcomeDto::Answered
                },
            ),
            GateOutcome::Answered(_) | GateOutcome::Superseded => {
                (Some(ask_user::QuestionEnd::Wrote), GateOutcomeDto::Answered)
            }
            GateOutcome::TimedOut => (
                Some(ask_user::QuestionEnd::TimedOut),
                GateOutcomeDto::TimedOut,
            ),
            GateOutcome::Cancelled => (None, GateOutcomeDto::Moot),
        };
        self.run.emit(AgentEvent::GateResolved {
            run_id: self.run.run_id.clone(),
            gate_id: call.id.clone(),
            outcome: resolved,
        });
        let cancelled = "Cancelled while the question waited for the user.";
        let Some(end) = end else {
            runtime.publish_attention();
            return ToolOutput::error(cancelled);
        };
        if !self.reacquire_slot().await {
            runtime.publish_attention();
            return ToolOutput::error(cancelled);
        }
        self.set_status(AgentRunStatus::Running, Some(ask_user::ASK_USER))
            .await;
        ToolOutput::text(ask_user::result_text(&options, end))
    }

    /// `fetch_url` checks that need the run: a URL holding a known secret is refused before any
    /// card, and outside Ask every time a host no trusted source named asks first.
    fn guard_fetch(&self, inspection: &mut CallInspection, mode: ToolApproval) {
        let Some(url) = inspection.arg_subject.clone() else {
            return;
        };
        if let Some(refusal) = crate::agent::secret_guard::refusal_if_secret(&url, &self.needles) {
            inspection.hard_block = Some(refusal.to_owned());
            return;
        }
        let seen = self
            .seen_hosts
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .covers(&url);
        if mode != ToolApproval::Ask && inspection.always_ask.is_none() && !seen {
            inspection.always_ask = Some(
                "this host did not appear in your messages, attachments or this run's web search results"
                    .to_owned(),
            );
            inspection.ask_code = Some("url_provenance");
        }
    }

    /// `shell` with network (Full access): a command that contacts a remote host the user's
    /// messages, attachments or this run's web search results never named asks first, as a
    /// fetch of that host would.
    fn guard_shell_remote(&self, inspection: &mut CallInspection, mode: ToolApproval) {
        if !shell_network_allowed(mode, false) || inspection.always_ask.is_some() {
            return;
        }
        let Some(command) = inspection.arg_subject.as_deref() else {
            return;
        };
        let reason = {
            let seen = self
                .seen_hosts
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            shell_remote_reason(command, &seen)
        };
        if let Some(reason) = reason {
            inspection.always_ask = Some(reason);
            inspection.ask_code = Some("shell_remote");
        }
    }

    /// Remembers the hosts a finished call may vouch for: the results of `web_search` and the
    /// URL of a fetch that ran (it was seen already or the user approved it).
    fn note_seen(&self, call: &ToolCall, arguments: &Value, output: &ToolOutput) {
        let mut seen = self
            .seen_hosts
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if call.name == crate::agent_tools::web::WEB_SEARCH && !output.is_error {
            for part in &output.parts {
                if let HubPart::Text { text, .. } = part {
                    seen.note_text(text);
                }
            }
        } else if call.name == crate::agent_tools::web::FETCH_URL
            && let Some(url) = arguments.get("url").and_then(Value::as_str)
        {
            seen.note_url(url);
        } else if call.name == SHELL
            && let Some(command) = arguments.get("command").and_then(Value::as_str)
        {
            // A networked command that ran was seen already or approved, like a fetch.
            for host in crate::agent::permissions::remote_hosts(command).unwrap_or_default() {
                seen.note_host(&host);
            }
        }
    }

    /// What the run is built from now.
    fn resolved(&self) -> Arc<Resolved> {
        Arc::clone(&self.resolved.lock().unwrap_or_else(PoisonError::into_inner))
    }

    /// Moves the run to `status`, persists its row and publishes the attention snapshot.
    pub(super) async fn set_status(&self, status: AgentRunStatus, step: Option<&str>) {
        let summary = self.run.update(|summary| {
            summary.status = status;
            summary.status_detail = None;
            if let Some(step) = step {
                summary.last_step = Some(step.to_owned());
            }
        });
        persist_run(&self.state, summary).await;
        self.state.agent.publish_attention();
    }

    /// The run waits on a gate of `kind` (`WaitingApproval` with its `status_detail`).
    async fn set_waiting(&self, kind: GateKind, step: &str) {
        let summary = self.run.update(|summary| {
            summary.status = AgentRunStatus::WaitingApproval;
            summary.status_detail = Some(kind.status_detail().to_owned());
            summary.last_step = Some(step.to_owned());
        });
        persist_run(&self.state, summary).await;
        self.state.agent.publish_attention();
    }

    /// The user-facing chat id: the parent chat for a sub-agent, or this session itself.
    /// Permission rules, sandbox acknowledgements and session-level rollups live here so
    /// sub-agents inherit parent Block/Allow rules and never persist rules to hidden child rows.
    fn effective_chat_id(&self) -> &str {
        self.session
            .parent_session_id
            .as_deref()
            .unwrap_or(&self.session.id)
    }

    /// The top-level run of this run's tree (itself, or a sub-agent's parent).
    fn root(&self) -> Arc<ActiveRun> {
        self.run
            .parent_run_id
            .as_deref()
            .and_then(|id| self.state.agent.runs.get(id))
            .unwrap_or_else(|| Arc::clone(&self.run))
    }

    /// The run waits for the user: the tree's top-level slot is free while all active branches wait.
    fn release_slot(&self) {
        self.root().begin_wait();
    }

    /// Ends a wait when the gate was cancelled so `root.waits` never leaks across cancellations.
    fn end_wait_cancelled(&self) {
        let _ = self.root().end_wait();
    }

    /// Waits for a top-level slot again after a pause; `false` when the run was cancelled meanwhile.
    async fn reacquire_slot(&self) -> bool {
        let root = self.root();
        if !root.end_wait() {
            return true;
        }
        let limit = usize::from(self.settings.ai.agent.max_concurrent_runs);
        match self
            .state
            .agent
            .runs
            .acquire_slot(|| limit, &self.run.cancel)
            .await
        {
            Some(guard) => {
                root.hold_slot(guard);
                true
            }
            None => false,
        }
    }

    /// Writes the run's currency (that of the calls it booked, never the main model's by
    /// default), awake time and timed-out flag; a failure only loses a marker.
    async fn save_extras(&self) {
        let (run, currency) = (Arc::clone(&self.run), self.run.summary().cost_currency);
        let _ = blocking(&self.state, move |state| {
            state.store().set_agent_run_extras(
                &run.run_id,
                currency.as_deref(),
                run.awake_ms(),
                run.approval_timed_out(),
            )
        })
        .await;
    }

    /// The chat's permission mode now (the user may change it while a run is going).
    async fn mode(&self) -> ToolApproval {
        let id = self.session.id.clone();
        blocking(&self.state, move |state| session_mode(&state.store(), &id))
            .await
            .ok()
            .flatten()
            .unwrap_or(self.session.permission_mode)
    }

    fn context<'a>(
        &'a self,
        resolved: &'a Resolved,
        call_id: &'a str,
        output: Option<crate::agent::tools::OutputSink<'a>>,
        events: Option<crate::agent::tools::ToolEventSink<'a>>,
        shell_network: bool,
    ) -> ToolContext<'a> {
        ToolContext {
            host: Some(&self.state as &(dyn std::any::Any + Send + Sync)),
            data_root: self.state.data_dirs().map(|dirs| dirs.root.as_path()),
            agent: Some(&self.state.agent),
            settings: &self.settings,
            session_id: &self.session.id,
            run_id: &self.run.run_id,
            call_id,
            workspace_dir: resolved.workspace.as_deref(),
            sandbox: sandbox_status().usable(),
            cancel: &self.run.cancel,
            output,
            events,
            processes: &self.state.agent.processes,
            read_hashes: &self.state.agent.read_hashes,
            data_tool_output_dir: self.tool_output_dir.as_deref(),
            shell_network,
            shell_full_access: false,
            // `run_tool` sets it from the plan and the acknowledgement.
            unsandboxed_approved: false,
            result_cap: self.result_cap_bytes(),
        }
    }

    /// Whether the chat chose "Run without a sandbox" (this app run only). It skips an available
    /// sandbox too: `nix`, `xcodebuild` and other tools that sandbox themselves fail inside
    /// Seatbelt, so this is the only way to run them.
    fn sandbox_acknowledged(&self) -> bool {
        self.state
            .agent
            .approvals
            .sandbox_acknowledged(self.effective_chat_id())
    }

    /// How the call stood to the sandbox, recorded with it: `skipped_by_choice` when the chat
    /// acknowledged "Run without a sandbox", `unavailable` when the self-test found none, else the
    /// tier.
    fn sandbox_record(&self) -> String {
        let status = sandbox_status();
        if self.sandbox_acknowledged() {
            enum_name(&SandboxTag::SkippedByChoice)
        } else if status.state != SandboxState::Available {
            status.state.recorded().to_owned()
        } else {
            enum_name(&status.usable())
        }
    }
}

impl Drop for ServiceHost {
    fn drop(&mut self) {
        if self.owns_slot {
            self.run.drop_slot();
        }
    }
}

impl RunHost for ServiceHost {
    fn emit(&self, event: AgentEvent) {
        self.run.emit(event);
    }

    fn clock(&self) -> &dyn RunClock {
        &self.clock
    }

    fn run_key(&self) -> (String, crate::llm::client::CallPurpose) {
        let summary = self.run.summary();
        match summary.parent_run_id {
            Some(root) => (root, crate::llm::client::CallPurpose::Subagent),
            None => (summary.run_id, crate::llm::client::CallPurpose::Step),
        }
    }

    fn tool_source(&self, name: &str) -> ToolSource {
        self.resolved()
            .registry
            .get(name)
            .map_or(ToolSource::Builtin, |tool| tool.spec().source.clone())
    }

    fn redact(&self, parts: Vec<HubPart>) -> Vec<HubPart> {
        parts
            .into_iter()
            .map(|part| match part {
                HubPart::Text {
                    text,
                    signature,
                    origin,
                    citations,
                } => HubPart::Text {
                    text: redact_secrets(&text, &self.needles),
                    signature,
                    origin,
                    citations,
                },
                other => other,
            })
            .collect()
    }

    async fn load_history(&self) -> Result<Vec<HistoryEntry>, AppError> {
        let (id, parent_id, skills) = (
            self.session.id.clone(),
            self.session.parent_session_id.clone(),
            self.resolved().skills.clone(),
        );
        let (messages, parent_user_texts, used) = blocking(&self.state, move |state| {
            let store = state.store();
            let mut messages = store.list_model_history(&id).map_err(command_error)?;
            let parent_user_texts: Vec<String> = match parent_id {
                Some(pid) => store
                    .list_model_history(&pid)
                    .unwrap_or_default()
                    .into_iter()
                    .filter(|m| m.role == AgentRole::User)
                    .flat_map(|m| m.parts)
                    .filter_map(|p| match p {
                        HubPart::Text { text, .. } => Some(text),
                        _ => None,
                    })
                    .collect(),
                None => Vec::new(),
            };
            let used = super::skill_delivery::attach_skills(&mut messages, &skills);
            Ok::<_, AppError>((messages, parent_user_texts, used))
        })
        .await??;
        *self
            .history_skills
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = used;
        {
            let mut seen = self
                .seen_hosts
                .lock()
                .unwrap_or_else(PoisonError::into_inner);
            for text in &parent_user_texts {
                seen.note_text(text);
            }
            for message in messages.iter().filter(|m| m.role == AgentRole::User) {
                for part in &message.parts {
                    if let HubPart::Text { text, .. } = part {
                        seen.note_text(text);
                    }
                }
            }
        }
        Ok(messages
            .into_iter()
            .map(|message| HistoryEntry {
                id: message.id,
                summary: message.role == AgentRole::Summary,
                message: HubMessage {
                    role: hub_role(message.role),
                    content: message.parts,
                },
            })
            .collect())
    }

    async fn resolve_attachments(&self, messages: Vec<HubMessage>) -> Vec<HubMessage> {
        // A model that has not been measured gets PDFs natively; a measured "no" gets the text.
        let document = self.model.capabilities.document;
        let caps = DocumentSupport {
            pdf: document.supported
                || document.source == crate::llm::capabilities::CapabilitySource::Unknown,
        };
        // The chat's own model is known not to read images, but the vision role can: the model
        // gets a note per image instead, saying how to hand it over.
        let delegate_images = self.role == Role::Main
            && self
                .model
                .capabilities
                .known_absent(crate::llm::capabilities::Capability::Vision)
            && self
                .state
                .settings()
                .ai
                .agent
                .role_models
                .get(crate::agent::model::SubagentRole::Vision)
                .is_some();
        let mut messages = messages;
        if delegate_images {
            for message in &mut messages {
                for part in &mut message.content {
                    *part = image_note(part);
                }
            }
        }
        let fallback = messages.clone();
        blocking(&self.state, move |state| {
            let mut messages = messages;
            resolve_for_model(state, caps, &mut messages);
            messages
        })
        .await
        .unwrap_or(fallback)
    }

    fn steer_pending(&self) -> bool {
        self.run.has_steer()
    }

    async fn drain_steer(&self) -> Result<bool, AppError> {
        let steers = self.run.take_steer();
        if steers.is_empty() {
            return Ok(false);
        }
        let available = self.resolved().skills.clone();
        for steer in steers {
            let mut message = new_message(
                &self.session.id,
                Some(&self.run.run_id),
                AgentRole::User,
                steer.parts,
            );
            message.meta.steered = true;
            message.meta.skills = steer.skills;
            let skills = available.clone();
            let (stored, skills_undelivered) = blocking(&self.state, move |state| {
                let undelivered = crate::agent::skills::render_skill_blocks(
                    &crate::agent::skills::skill_bodies(&skills, &message.meta.skills),
                )
                .1;
                store_user_message(state, &message).map(|stored| (stored, undelivered))
            })
            .await??;
            self.run.emit(AgentEvent::UserMessage {
                message: Box::new(stored),
                skills_undelivered,
            });
        }
        Ok(true)
    }

    async fn drain_process_exits(&self) -> Result<(), AppError> {
        let ended = self
            .state
            .agent
            .processes
            .take_finished_unreported(&self.session.id);
        for process in ended {
            let line = super::run::background_exit_line(&process);
            let mut message = new_message(
                &self.session.id,
                Some(&self.run.run_id),
                AgentRole::User,
                vec![HubPart::text(line.clone())],
            );
            message.meta.system = true;
            blocking(&self.state, move |state| {
                store_user_message(state, &message)
            })
            .await??;
            self.run.emit(AgentEvent::coded_notice(
                "process_exited",
                line,
                &[
                    ("process", process.id.clone()),
                    ("command", process.command.clone()),
                ],
            ));
        }
        Ok(())
    }

    async fn save_partial(&self, message_id: &str, parts: Vec<HubPart>) {
        let (session, run, id) = (
            self.session.id.clone(),
            self.run.run_id.clone(),
            message_id.to_owned(),
        );
        let _ = blocking(&self.state, move |state| {
            state
                .store()
                .upsert_partial_assistant(&session, &run, &id, parts)
        })
        .await;
    }

    async fn save_assistant(&self, draft: AssistantDraft) -> Result<AgentMessage, AppError> {
        // A known cost (0 included) carries the currency of the model that answered; an unknown
        // one is 0 without a currency.
        let currency = draft.cost.micro.and(draft.cost.currency.clone());
        let message = self.assistant_message(draft);
        blocking(&self.state, move |state| {
            let store = state.store();
            let saved = store
                .append_agent_message(&message)
                .map_err(command_error)?;
            if let Some(currency) = &currency {
                // A lost currency tag only leaves the cost unlabelled.
                let _ = store.set_message_cost_currency(&saved.id, currency);
            }
            Ok(saved)
        })
        .await?
    }

    async fn save_tool_turn(
        &self,
        draft: AssistantDraft,
    ) -> Result<(AgentMessage, AgentMessage), AppError> {
        let currency = draft.cost.micro.and(draft.cost.currency.clone());
        let assistant = self.assistant_message(draft);
        let tool = new_message(
            &self.session.id,
            Some(&self.run.run_id),
            AgentRole::Tool,
            Vec::new(),
        );
        blocking(&self.state, move |state| {
            let store = state.store();
            let saved = store
                .append_assistant_with_pending_results(&assistant, &tool)
                .map_err(command_error)?;
            if let Some(currency) = &currency {
                let _ = store.set_message_cost_currency(&saved.0.id, currency);
            }
            Ok(saved)
        })
        .await?
    }

    async fn save_tool_result(
        &self,
        tool_message_id: &str,
        result: HubPart,
    ) -> Result<(), AppError> {
        let HubPart::ToolResult { call_id, .. } = &result else {
            return Err(AppError::internal("a tool result part was expected"));
        };
        let (message_id, call_id) = (tool_message_id.to_owned(), call_id.clone());
        // A message deleted meanwhile leaves no slot to fill; the result is simply not stored.
        blocking(&self.state, move |state| {
            state
                .store()
                .update_tool_result_part(&message_id, &call_id, &result)
                .map(|_| ())
                .map_err(command_error)
        })
        .await?
    }

    async fn save_summary(
        &self,
        text: String,
        compacted_through: &str,
    ) -> Result<AgentMessage, AppError> {
        let (session, run, through) = (
            self.session.id.clone(),
            self.run.run_id.clone(),
            compacted_through.to_owned(),
        );
        let skills = super::skill_delivery::used_through(
            &self
                .history_skills
                .lock()
                .unwrap_or_else(PoisonError::into_inner),
            compacted_through,
        );
        blocking(&self.state, move |state| {
            state
                .store()
                .append_compaction_summary(
                    &session,
                    Some(&run),
                    vec![HubPart::text(text)],
                    &through,
                    skills,
                )
                .map_err(command_error)
        })
        .await?
    }

    async fn record_call(&self, audit: CallAudit) {
        // The wait for an answer starts when the card appears (AI-observe-5).
        let requested_ms = (audit.state == "approval-requested").then(current_time_ms);
        let record = AgentToolCallRecord {
            session_id: self.session.id.clone(),
            call_id: audit.call_id,
            run_id: self.run.run_id.clone(),
            message_id: Some(audit.message_id),
            name: audit.name,
            class: enum_name(&audit.class),
            state: audit.state.to_owned(),
            args_json: audit.arguments,
            risk: audit.risk.to_owned(),
            decided_by: audit.decided_by,
            decision: audit.decision.map(str::to_owned),
            decided_ms: audit.decided_ms,
            started_ms: audit.started_ms,
            ended_ms: audit.ended_ms,
            exit_code: audit.exit_code,
            sandbox: Some(self.sandbox_record()),
            truncated: audit.truncated,
            output_bytes: audit.output_bytes,
            spill_ref: audit.spill_ref,
            requested_ms,
        };
        let written = blocking(&self.state, move |state| {
            state.store().upsert_tool_call(&record)
        })
        .await;
        if let Err(error) = written
            .map_err(|e| e.to_string())
            .and_then(|r| r.map_err(|e| e.to_string()))
        {
            let failures =
                AUDIT_WRITE_FAILURES.fetch_add(1, std::sync::atomic::Ordering::Relaxed) + 1;
            pingdeck_base::logging::log_line(
                pingdeck_base::logging::LogLevel::Error,
                "agent.audit",
                &format!("audit row of a tool call not written ({failures} so far): {error}"),
            );
        }
    }

    async fn model_info(&self, served: &crate::agent::runner::ServedModel) -> ModelInfo {
        let key = served.id_form();
        let cached = self
            .served_models
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .get(&key)
            .cloned();
        if let Some(info) = cached {
            return info;
        }
        let info = super::model_info::model_facts(
            &self.state,
            served.provider_id.clone(),
            served.model.clone(),
        )
        .await;
        self.served_models
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .insert(key, info.clone());
        info
    }

    async fn record_usage(
        &self,
        usage: &HubUsage,
        cost: &CallCost,
        context_tokens: u64,
        purpose: crate::llm::client::CallPurpose,
    ) -> Result<RunTotals, AppError> {
        use crate::llm::client::CallPurpose;
        let cost_micro = cost.micro_or_zero();
        let (session, parent_session, call_usage) = (
            self.session.id.clone(),
            self.session.parent_session_id.clone(),
            usage.clone(),
        );
        blocking(&self.state, move |state| {
            let store = state.store();
            store
                .add_session_usage(&session, &call_usage, cost_micro, context_tokens)
                .map_err(command_error)?;
            if let Some(parent_id) = parent_session {
                let _ = store.add_session_usage_rollup(&parent_id, &call_usage, cost_micro);
            }
            Ok::<(), AppError>(())
        })
        .await??;
        // An unknown cost counts in no currency; a known one counts for the run and, as it
        // happens, for its parent, in the currency of the model that answered.
        let currency = cost.micro.and(cost.currency.as_deref());
        let summary = self.run.update(|summary| {
            // A summary is not a step.
            if purpose != CallPurpose::Compaction {
                summary.iterations += 1;
            }
            add_usage(&mut summary.usage, usage);
            summary.cost_micro_usd = summary.cost_micro_usd.saturating_add(cost_micro);
            summary.model_label.clone_from(&self.model_label);
            summary.last_step = Some("model".to_owned());
            if let Some(currency) =
                currency.filter(|_| cost_micro > 0 || summary.cost_currency.is_none())
            {
                summary.cost_currency = Some(currency.to_owned());
            }
        });
        if let Some(currency) = currency {
            self.run.add_cost(currency, cost_micro);
            if let Some(parent) = self
                .run
                .parent_run_id
                .as_deref()
                .and_then(|id| self.state.agent.runs.get(id))
            {
                parent.add_cost(currency, cost_micro);
            }
        }
        let part = match purpose {
            CallPurpose::Compaction => "compaction",
            CallPurpose::Subagent => "subagent",
            CallPurpose::Step | CallPurpose::Title => "main",
        };
        let tokens = usage
            .input_tokens
            .unwrap_or(0)
            .saturating_add(usage.output_tokens.unwrap_or(0));
        let reported = usage.input_tokens.is_some() || usage.output_tokens.is_some();
        let priced = currency.map(|currency| (currency, cost_micro));
        let model = if priced.is_none() && reported {
            self.model_label.as_str()
        } else {
            ""
        };
        self.root().note_call(part, priced, model, tokens);
        let totals = RunTotals {
            usage: summary.usage.clone(),
            // The `Usage` event's figure is in this call's currency: the tree's spend in it.
            cost_micro: currency.map_or(0, |currency| self.run.total_cost_micro(currency)),
        };
        persist_run(&self.state, summary).await;
        self.save_extras().await;
        Ok(totals)
    }

    fn run_tokens(&self) -> u64 {
        self.root().tokens_used()
    }

    fn cap_passes(&self, key: &str) -> Option<i64> {
        Some(self.root().cap_passes(key))
    }

    fn set_cap_passes(&self, key: &str, passes: i64) {
        self.root().set_cap_passes(key, passes);
    }

    fn run_cost_micro(&self, currency: &str) -> i64 {
        // A sub-agent is capped with the whole tree: its parent's total already holds the
        // child's spend as it happens (`record_usage`).
        self.run
            .parent_run_id
            .as_deref()
            .and_then(|id| self.state.agent.runs.get(id))
            .map_or_else(
                || self.run.total_cost_micro(currency),
                |parent| parent.total_cost_micro(currency),
            )
    }

    fn parallel_safe(&self, name: &str) -> bool {
        self.resolved()
            .registry
            .get(name)
            .is_some_and(|tool| tool.spec().parallel_safe)
    }

    async fn plan_call(&self, call: &ToolCall) -> ToolPlan {
        let invalid = |text: String| ToolPlan {
            class: crate::agent::tools::ToolClass::Internal,
            risk: "low",
            parallel_safe: false,
            verdict: Verdict::Invalid(text),
            decided_by: None,
        };
        let resolved = self.resolved();
        if call.name == ask_user::ASK_USER && self.role != Role::Main {
            return invalid(crate::agent_tools::subagent::ASK_REFUSAL.to_owned());
        }
        let Some(tool) = resolved.registry.get(&call.name) else {
            // Plan mode removes the write tools from the list; a call to one is refused, not
            // "unknown".
            if self.state.agent.plan_active(&self.session.id) {
                return ToolPlan {
                    class: crate::agent::tools::ToolClass::Internal,
                    risk: "low",
                    parallel_safe: false,
                    verdict: Verdict::Refuse(PLAN_REFUSAL.to_owned()),
                    decided_by: Some("plan".to_owned()),
                };
            }
            return invalid(super::tools::unknown_tool_text(
                &call.name,
                &resolved.registry,
                &self.session.disabled_tools,
                resolved.fingerprint.bound,
            ));
        };
        let spec = tool.spec();
        if self.role == Role::Main
            && self.state.agent.plan_active(&self.session.id)
            && !plan_allows(spec)
        {
            return ToolPlan {
                class: spec.class,
                risk: "low",
                parallel_safe: false,
                verdict: Verdict::Refuse(PLAN_REFUSAL.to_owned()),
                decided_by: Some("plan".to_owned()),
            };
        }
        let arguments = match parse_arguments(&call.arguments) {
            Ok(arguments) => arguments,
            Err(text) => return invalid(text),
        };
        let inspection = {
            let ctx = self.context(&resolved, &call.id, None, None, false);
            tool.inspect(&ctx, &arguments)
        };
        let mut inspection = match inspection {
            Ok(inspection) => inspection,
            Err(error) => {
                return ToolPlan {
                    class: spec.class,
                    risk: "low",
                    parallel_safe: spec.parallel_safe,
                    verdict: Verdict::Invalid(error.message().to_owned()),
                    decided_by: None,
                };
            }
        };
        let mut risk = if inspection.destructive {
            "high"
        } else if inspection.read_only {
            "low"
        } else {
            "medium"
        };
        // The safety list and the auto-run switch are read now, not from the run's snapshot: a
        // pattern added mid-run applies to the next call. Sub-agents evaluate chat-scoped rules
        // against their parent chat so parent Block/Allow rules always apply.
        let live = self.state.settings();
        let (session_id, rules_chat_id, blocked) = (
            self.session.id.clone(),
            self.effective_chat_id().to_owned(),
            live.ai.agent.blocked_commands.clone(),
        );
        let loaded = blocking(&self.state, move |state| {
            let store = state.store();
            (
                store
                    .list_permission_rules(Some(&rules_chat_id))
                    .unwrap_or_default(),
                store.list_tool_policy().unwrap_or_default(),
                session_mode(&store, &session_id),
            )
        })
        .await;
        let (rules, policies, loaded_mode) = loaded.unwrap_or_default();
        let mode = loaded_mode.unwrap_or(self.session.permission_mode);
        if call.name == crate::agent_tools::web::FETCH_URL {
            self.guard_fetch(&mut inspection, mode);
        } else if call.name == SHELL {
            self.guard_shell_remote(&mut inspection, mode);
        }
        let policies: std::collections::HashMap<String, PermissionAction> = policies
            .into_iter()
            .map(|policy| (policy.tool_key, policy.policy))
            .collect();
        let data_dir = self.state.data_dirs().map(|dirs| dirs.root.clone());
        let home = std::env::var_os("HOME").map(PathBuf::from);
        let tree_sessions: std::collections::HashSet<String> = {
            let root = self.root();
            let mut ids: std::collections::HashSet<String> = self
                .state
                .agent
                .runs
                .active_runs()
                .into_iter()
                .filter(|r| {
                    r.run_id == root.run_id || r.parent_run_id.as_deref() == Some(&root.run_id)
                })
                .map(|r| r.session_id.clone())
                .collect();
            ids.insert(self.session.id.clone());
            ids.insert(self.effective_chat_id().to_owned());
            ids
        };
        let folder_busy = self.state.agent.processes.list(None).iter().any(|process| {
            process.state == crate::agent::tools::process::ProcessState::Running
                && tree_sessions.contains(&process.session_id)
        });
        let status = sandbox_status();
        let query = PermissionQuery {
            session_id: self.effective_chat_id(),
            workspace_dir: resolved.workspace.as_deref(),
            tool_name: &call.name,
            spec,
            inspection: &inspection,
            policy: mode,
            sandbox: status.usable(),
            env: PermissionEnv {
                sandbox_acknowledged: self.sandbox_acknowledged(),
                folder_busy,
                data_dir: data_dir.as_deref(),
                home: home.as_deref(),
                sandbox_auto_run: live.ai.agent.sandbox_auto_run,
            },
        };
        let decision = evaluate_permission(&rules, &policies, &blocked, &query);
        if call.name == SHELL {
            self.planned_sandbox
                .lock()
                .unwrap_or_else(PoisonError::into_inner)
                .insert(call.id.clone(), query.sandbox);
        }
        let decided_by = Some(decision.decided_by.clone());
        let verdict = match decision.outcome {
            Outcome::Allow => Verdict::Run,
            Outcome::Block => Verdict::Refuse(decision.reason),
            Outcome::Ask => {
                let card = approval_card(&query, &decision, status);
                risk = risk_name(card.risk);
                Verdict::Ask(AskInfo {
                    reason: decision.reason,
                    class: spec.class,
                    read_only: inspection.read_only,
                    destructive: inspection.destructive,
                    preview: inspection.preview.clone(),
                    network: call.name == SHELL && !shell_network_allowed(mode, false),
                    card: Some(Box::new(card)),
                    risk,
                })
            }
        };
        ToolPlan {
            class: spec.class,
            risk,
            parallel_safe: spec.parallel_safe,
            verdict,
            decided_by,
        }
    }

    async fn request_approval(&self, call: &ToolCall, ask: &AskInfo) -> ApprovalOutcome {
        let runtime = &self.state.agent;
        let (line, full_text) = card_text(call, ask);
        let summary = PendingApprovalSummary {
            run_id: self.run.run_id.clone(),
            session_id: self.session.id.clone(),
            call_id: call.id.clone(),
            tool: call.name.clone(),
            reason: ask.reason.clone(),
            preview: ask.preview.clone(),
            requested_ms: current_time_ms(),
            risk: ask.risk.to_owned(),
            summary: line.clone(),
            full_text: full_text.clone(),
            card: ask.card.as_deref().cloned(),
        };
        let rule_chat_id = self.effective_chat_id();
        let context = ask.card.as_ref().map_or_else(
            || RuleContext {
                session_id: rule_chat_id.to_owned(),
                workspace_dir: self.session.workspace_dir.clone(),
                tool: call.name.clone(),
                pattern: None,
                offered: vec![ApprovalScope::Once],
            },
            |card| {
                card.rule_context(
                    rule_chat_id,
                    self.session.workspace_dir.as_deref(),
                    &call.name,
                )
            },
        );
        // Registered before the card is announced, so an instant answer is never lost.
        let Some(ticket) = runtime.approvals.register(summary, context) else {
            return ApprovalOutcome::Cancelled;
        };
        self.release_slot();
        self.set_waiting(GateKind::Approval, &call.name).await;
        // The card shows the profile that will actually run: an acknowledged chat
        // shells without the sandbox, so the card must not claim one.
        let sandbox = if call.name == SHELL && self.sandbox_acknowledged() {
            SandboxKind::None
        } else {
            sandbox_status().usable()
        };
        self.run.emit(AgentEvent::ToolApprovalRequired {
            run_id: self.run.run_id.clone(),
            call_id: call.id.clone(),
            name: call.name.clone(),
            arguments: call.arguments.clone(),
            class: ask.class,
            read_only: ask.read_only,
            destructive: ask.destructive,
            sandbox,
            reason: ask.reason.clone(),
            preview: ask.preview.clone(),
            network: ask.network,
            risk: ask.risk.to_owned(),
            summary: line,
            full_text,
            card: ask.card.clone(),
        });
        runtime.publish_attention();
        let timeout = Duration::from_secs(u64::from(self.approval_timeout_s()));
        let outcome = ticket
            .wait(&runtime.gate(&self.run), &self.run.cancel, timeout)
            .await;
        match &outcome {
            ApprovalOutcome::Answered(answer) => {
                self.run.emit(AgentEvent::ToolApprovalResolved {
                    call_id: call.id.clone(),
                    decision: answer.decision,
                    scope: answer.scope,
                    timed_out: false,
                });
            }
            ApprovalOutcome::TimedOut | ApprovalOutcome::Superseded => {
                self.run.emit(AgentEvent::ToolApprovalResolved {
                    call_id: call.id.clone(),
                    decision: ApprovalDecision::Deny,
                    scope: ApprovalScope::Once,
                    timed_out: outcome == ApprovalOutcome::TimedOut,
                });
            }
            ApprovalOutcome::Cancelled => {}
        }
        if outcome == ApprovalOutcome::TimedOut {
            self.run.mark_approval_timed_out();
            self.save_extras().await;
        }
        if outcome == ApprovalOutcome::Cancelled {
            self.end_wait_cancelled();
            runtime.publish_attention();
            return ApprovalOutcome::Cancelled;
        }
        if !self.reacquire_slot().await {
            runtime.publish_attention();
            return ApprovalOutcome::Cancelled;
        }
        self.set_status(AgentRunStatus::Running, Some(&call.name))
            .await;
        outcome
    }

    async fn run_tool(&self, call: &ToolCall, options: RunOptions) -> ToolOutput {
        let resolved = self.resolved();
        let Some(tool) = resolved.registry.get(&call.name) else {
            return ToolOutput::error(format!("There is no tool named {}.", call.name));
        };
        let arguments = match parse_arguments(&call.arguments) {
            Ok(arguments) => arguments,
            Err(text) => return ToolOutput::error(text),
        };
        if call.name == submit_plan::SUBMIT_PLAN {
            return self.submit_plan(call, &arguments).await;
        }
        if call.name == ask_user::ASK_USER {
            return self.ask_user(call, &arguments).await;
        }
        let mode = self.mode().await;
        let network = shell_network_allowed(mode, options.allow_network_once);
        let call_id = call.id.clone();
        // The live view streams the first `LIVE_OUTPUT_BYTES` of a call's output, then one note;
        // the whole output stays in the call's result and spill file. A runaway command cannot
        // flood the replay ring and push the run's approval cards and usage out of it.
        let streamed = std::sync::atomic::AtomicUsize::new(0);
        let emit_delta = |stream: OutputStream, text: String| {
            if text.is_empty() {
                return;
            }
            let before = streamed.fetch_add(text.len(), std::sync::atomic::Ordering::Relaxed);
            let text = match live_output_share(before, text) {
                Some(text) => text,
                None => return,
            };
            self.run.emit(AgentEvent::ToolOutputDelta {
                call_id: call_id.clone(),
                stream,
                text,
            });
        };
        let lanes = Mutex::new((StreamRedactor::default(), StreamRedactor::default()));
        let output = |stream: OutputStream, text: &str| {
            let mut lanes = lanes.lock().unwrap_or_else(PoisonError::into_inner);
            let lane = match stream {
                OutputStream::Stdout => &mut lanes.0,
                OutputStream::Stderr => &mut lanes.1,
            };
            emit_delta(stream, lane.push(text, &self.needles));
        };
        let events = |event: ToolEvent| match event {
            ToolEvent::FileChanged {
                path,
                kind,
                added,
                removed,
            } => {
                self.run.emit(AgentEvent::FileChanged {
                    call_id: call_id.clone(),
                    path,
                    change: kind,
                    added,
                    removed,
                    change_id: None,
                });
            }
            ToolEvent::Notice(text) => {
                self.run.emit(AgentEvent::notice(text));
            }
        };
        let mut ctx = self.context(&resolved, &call.id, Some(&output), Some(&events), network);
        ctx.shell_full_access = mode == ToolApproval::AutoAll;
        let planned = self
            .planned_sandbox
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&call.id);
        if let Some(kind) = planned {
            ctx.sandbox = kind;
        }
        let acknowledged = self.sandbox_acknowledged();
        if acknowledged {
            // "Run without a sandbox": the call carries `SandboxTag::SkippedByChoice`.
            ctx.sandbox = SandboxKind::None;
        }
        // Unsandboxed only when a card or the acknowledgement covered it: a plan that said
        // "sandboxed" and a run that finds none (the status changed meanwhile) is refused by `shell`.
        ctx.unsandboxed_approved = acknowledged || planned == Some(SandboxKind::None);
        self.run
            .update(|summary| summary.last_step = Some(call.name.clone()));
        if changes_files_unjournaled(tool.spec()) {
            self.untracked.arm(resolved.workspace.as_deref()).await;
        }
        let result = if journals(&call.name) {
            let (output, events) = apply_journaled(&ctx, &call.name, arguments).await;
            for event in events {
                self.run.emit(event);
            }
            output
        } else {
            let output = tool.call(&ctx, arguments.clone()).await;
            self.note_seen(call, &arguments, &output);
            output
        };
        // The unfinished last line of each stream was held back so a secret split over two
        // chunks is still masked; the call is over, release it.
        let mut lanes = lanes.lock().unwrap_or_else(PoisonError::into_inner);
        emit_delta(OutputStream::Stdout, lanes.0.finish(&self.needles));
        emit_delta(OutputStream::Stderr, lanes.1.finish(&self.needles));
        result
    }

    async fn await_cost_decision(&self, pause: &CostPause<'_>) -> Option<CostDecision> {
        let runtime = &self.state.agent;
        let ticket = runtime.gates.open(GateRequest {
            gate_id: format!("cost:{}:{}", pause.currency.unwrap_or("tokens"), pause.cap),
            run_id: self.run.run_id.clone(),
            session_id: self.session.id.clone(),
            kind: GateKind::Cost,
            payload: GatePayload::Cost,
            opened_ms: current_time_ms(),
        })?;
        self.release_slot();
        self.set_waiting(GateKind::Cost, "cost limit").await;
        let root = self.root();
        let breakdown: Vec<CostShare> = match pause.currency {
            Some(currency) => root
                .spend_by_part(currency)
                .into_iter()
                .map(|(part, micro)| CostShare {
                    part: part.to_owned(),
                    micro,
                })
                .collect(),
            None => vec![CostShare {
                part: "main".to_owned(),
                micro: pause.spent,
            }],
        };
        let unpriced = root.unpriced_models();
        self.run.emit(AgentEvent::CostLimitReached {
            run_id: self.run.run_id.clone(),
            cost_micro: pause.spent,
            cap_micro: pause.cap,
            currency: pause.currency.unwrap_or_default().to_owned(),
            breakdown: (!breakdown.is_empty()).then_some(breakdown),
            next_cap_micro: Some(pause.next_cap),
            unpriced_models: (!unpriced.is_empty()).then_some(unpriced),
            tokens: pause.currency.is_none().then_some(true),
        });
        let timeout = Duration::from_secs(u64::from(self.approval_timeout_s()));
        let outcome = ticket
            .wait(&runtime.gate(&self.run), &self.run.cancel, timeout)
            .await;
        // Nobody answered in time: the run ends at the cap and the queue waits (AI-chatux-14).
        let decision = match outcome {
            GateOutcome::Answered(GateAnswer::Cost(decision)) => decision,
            GateOutcome::TimedOut => CostDecision::Stop,
            _ => {
                self.end_wait_cancelled();
                runtime.publish_attention();
                return None;
            }
        };
        if !self.reacquire_slot().await {
            return None;
        }
        self.set_status(AgentRunStatus::Running, Some("model"))
            .await;
        Some(decision)
    }

    fn approval_timeout_s(&self) -> u32 {
        self.settings.ai.agent.approval_timeout_s
    }

    fn take_end_request(&self) -> bool {
        self.end_requested
            .swap(false, std::sync::atomic::Ordering::SeqCst)
    }

    fn superseded(&self, call_id: &str) -> bool {
        self.run.take_superseded(call_id)
    }

    fn result_cap_bytes(&self) -> usize {
        result_cap_bytes(self.model.context_limit)
    }

    fn folder_untrusted(&self) -> bool {
        let resolved = self.resolved();
        resolved.workspace.is_some() && !resolved.fingerprint.folder_trusted()
    }

    async fn spill_output(&self, call_id: &str, text: String) -> Option<u64> {
        let root = self.tool_output_dir.clone()?;
        let busy: Vec<String> = self
            .state
            .agent
            .runs
            .active_runs()
            .iter()
            .map(|run| run.summary().session_id)
            .collect();
        let (state, session, call) = (
            self.state.clone(),
            self.session.id.clone(),
            call_id.to_owned(),
        );
        tokio::task::spawn_blocking(move || {
            let written =
                write_spill(&root, &session, &call, &text, SpillCaps::DEFAULT, &busy).ok()?;
            // Persist `spill_ref` on the tool-call audit row so watermark-pruned handles also
            // resolve in `agent_tool_output_read_impl` (`read_output`).
            let _ = state
                .store()
                .set_tool_call_spill_ref(&session, &call, &call);
            Some(written)
        })
        .await
        .ok()
        .flatten()
    }

    async fn working_state_summary(&self) -> Option<String> {
        let session_id = self.session.id.clone();
        let chat_id = self.effective_chat_id().to_owned();
        let state = self.state.clone();
        let (todos, changes) = blocking(&state, move |state| {
            let store = state.store();
            let todos = store
                .get_agent_session(&session_id)
                .ok()
                .flatten()
                .map(|s| s.todos)
                .unwrap_or_default();
            let changes = store.list_chat_file_changes(&chat_id).unwrap_or_default();
            (todos, changes)
        })
        .await
        .ok()?;
        let mut lines = Vec::new();
        let active_todos: Vec<String> = todos
            .into_iter()
            .filter(|t| {
                !matches!(
                    t.status,
                    crate::agent::model::AgentTodoStatus::Completed
                        | crate::agent::model::AgentTodoStatus::Cancelled
                )
            })
            .take(15)
            .map(|t| {
                let tag = match t.status {
                    crate::agent::model::AgentTodoStatus::InProgress => "in_progress",
                    crate::agent::model::AgentTodoStatus::Pending => "pending",
                    crate::agent::model::AgentTodoStatus::Completed => "completed",
                    crate::agent::model::AgentTodoStatus::Cancelled => "cancelled",
                };
                format!("- [{tag}] {}", t.content)
            })
            .collect();
        if !active_todos.is_empty() {
            lines.push(format!("Active todos:\n{}", active_todos.join("\n")));
        }
        let mut touched: Vec<String> = Vec::new();
        for ch in changes
            .into_iter()
            .filter(|c| c.status == crate::agent::model::FileChangeStatus::Pending)
        {
            if !touched.contains(&ch.path) {
                touched.push(ch.path);
                if touched.len() >= 20 {
                    break;
                }
            }
        }
        if !touched.is_empty() {
            lines.push(format!("Pending modified files: {}", touched.join(", ")));
        }
        let running_procs: Vec<String> = self
            .state
            .agent
            .processes
            .list(Some(&self.session.id))
            .into_iter()
            .filter(|p| p.state == crate::agent::tools::process::ProcessState::Running)
            .map(|p| format!("{} ({})", p.id, p.command))
            .collect();
        if !running_procs.is_empty() {
            lines.push(format!(
                "Running background processes: {}",
                running_procs.join("; ")
            ));
        }
        (!lines.is_empty()).then(|| lines.join("\n"))
    }

    fn record_failure(&self, failure: AgentFailure) {
        self.run.set_failure(failure);
    }

    fn context_state(&self) -> crate::agent::context::ContextState {
        self.state.agent.runs.context_state(&self.session.id)
    }

    fn save_context_state(&self, state: &crate::agent::context::ContextState) {
        self.state
            .agent
            .runs
            .set_context_state(&self.session.id, state);
    }

    async fn step_config(&self) -> Option<StepConfig> {
        let id = self.session.id.clone();
        let plan = self.role == Role::Main && self.state.agent.plan_active(&id);
        let current = self.resolved();
        let session = blocking(&self.state, move |state| {
            let session = state.store().get_agent_session(&id).ok().flatten()?;
            let fresh = fingerprint(state, &session, plan);
            Some((session, fresh))
        })
        .await
        .ok()
        .flatten()?;
        let (session, fresh) = session;
        if fresh == current.fingerprint {
            return None;
        }
        let next = resolve(&self.state, &session, &self.model, self.role, true).await;
        let config = StepConfig {
            system: Some(next.system.clone()),
            tools: next.specs.clone(),
            native_web_search: next.native_web_search,
        };
        *self.resolved.lock().unwrap_or_else(PoisonError::into_inner) = Arc::new(next);
        Some(config)
    }
}

impl ServiceHost {
    fn assistant_message(&self, draft: AssistantDraft) -> AgentMessage {
        AgentMessage {
            id: draft.id,
            usage: draft.usage,
            model: Some(draft.model),
            error: draft.error,
            cost_micro_usd: draft.cost.micro_or_zero(),
            meta: AgentMessageMeta {
                served_model: draft.served_model,
                reasoning_effort: draft.reasoning_effort,
                ttft_ms: draft.ttft_ms,
                stream_ms: draft.stream_ms,
                error_kind: draft.error_kind,
                failure: draft.failure,
                ..AgentMessageMeta::default()
            },
            ..new_message(
                &self.session.id,
                Some(&self.run.run_id),
                AgentRole::Assistant,
                draft.parts,
            )
        }
    }
}

#[cfg(test)]
mod live_output_tests {
    #![allow(clippy::unwrap_used)]

    use super::*;

    #[test]
    fn a_call_streams_its_first_bytes_then_one_note_and_nothing_more() {
        assert_eq!(live_output_share(0, "abc".into()).unwrap(), "abc");
        let crossing = live_output_share(LIVE_OUTPUT_BYTES - 2, "héllo".into()).unwrap();
        // The cut never splits a character: "h" plus the note (é is two bytes).
        assert_eq!(crossing, format!("h{LIVE_OUTPUT_CUT}"));
        assert_eq!(live_output_share(LIVE_OUTPUT_BYTES, "more".into()), None);
    }
}
