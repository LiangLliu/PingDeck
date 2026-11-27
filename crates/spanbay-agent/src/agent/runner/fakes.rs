//! Test doubles of the run loop: a scripted model, an in-memory host and a clock that moves only
//! when a test says so.

#![allow(clippy::unwrap_used, clippy::expect_used, clippy::panic)]

use std::collections::{HashMap, VecDeque};
use std::sync::atomic::{AtomicUsize, Ordering};
use std::sync::{Arc, Mutex};
use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use futures_util::stream::{self, BoxStream};
use futures_util::{FutureExt, StreamExt};
use tokio::sync::oneshot;

use crate::agent::approvals::ApprovalOutcome;
use crate::agent::failure::AgentFailure;
use crate::agent::model::{AgentEvent, AgentMessage, AgentMessageMeta, AgentRole, SandboxKind};
use crate::agent::registry::CostDecision;
use crate::agent::tools::{ToolClass, ToolOutput, ToolSource};
use spanbay_base::cancel::CancelToken;
use spanbay_base::error::AppError;
use spanbay_base::secrets::redact_secrets;
use spanbay_llm::llm::client::{
    CallMeta, ChatError, ChatErrorKind, ChatTarget, Reshape, TargetFacts, shape_for,
};
use spanbay_llm::llm::hub::{
    HubAttempt, HubEvent, HubMessage, HubPart, HubRequest, HubResponse, HubUsage,
};

use super::{
    AskInfo, AssistantDraft, CallAudit, ChatBackend, HistoryEntry, ModelInfo, RunClock, RunHost,
    RunOptions, RunTotals, ServedModel, ToolCall, ToolPlan, Verdict, hub_role,
};

// ---------------------------------------------------------------------------------- the clock

/// Timers waiting for the clock: `(deadline, wake-up)`.
type Timers = Vec<(Duration, oneshot::Sender<()>)>;

/// A clock that only moves on [`ManualClock::advance`]; timers fire when it passes them.
pub(super) struct ManualClock {
    base: Instant,
    state: Mutex<(Duration, Timers)>,
}

impl ManualClock {
    pub(super) fn new() -> Arc<Self> {
        Arc::new(Self {
            base: Instant::now(),
            state: Mutex::new((Duration::ZERO, Vec::new())),
        })
    }

    /// Moves time forward and fires every timer that is due.
    pub(super) fn advance(&self, by: Duration) {
        let due = {
            let mut state = self.state.lock().unwrap();
            state.0 += by;
            let now = state.0;
            let (due, later): (Vec<_>, Vec<_>) = std::mem::take(&mut state.1)
                .into_iter()
                .partition(|(deadline, _)| *deadline <= now);
            state.1 = later;
            due
        };
        for (_, sender) in due {
            let _ = sender.send(());
        }
    }

    /// Timers somebody still waits for.
    pub(super) fn waiting(&self) -> usize {
        self.state
            .lock()
            .unwrap()
            .1
            .iter()
            .filter(|(_, sender)| !sender.is_closed())
            .count()
    }
}

impl RunClock for ManualClock {
    fn now(&self) -> Instant {
        self.base + self.state.lock().unwrap().0
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        let (sender, receiver) = oneshot::channel();
        {
            let mut state = self.state.lock().unwrap();
            let deadline = state.0 + duration;
            state.1.push((deadline, sender));
        }
        async move {
            if receiver.await.is_err() {
                std::future::pending::<()>().await;
            }
        }
        .boxed()
    }
}

/// Lets other tasks run until `done` holds; panics when it never does.
pub(super) async fn until(mut done: impl FnMut() -> bool) {
    for _ in 0..2_000 {
        if done() {
            return;
        }
        tokio::task::yield_now().await;
    }
    panic!("the condition never became true");
}

// ---------------------------------------------------------------------------------- the model

/// One step of a scripted model stream.
pub(super) enum Step {
    Event(HubEvent),
    Fail(ChatError),
    /// Moves the clock forward when the stream reaches this point.
    Advance(Duration),
    /// The stream stays open and silent from here on.
    Hang,
}

/// A scripted model: each call takes the next script; every request is recorded (as the target
/// that answered got it).
pub(super) struct FakeBackend {
    pub clock: Arc<ManualClock>,
    /// Each script, with the fallback that answers it when the run's model failed first.
    streams: Mutex<VecDeque<(Option<TargetFacts>, Vec<Step>)>>,
    completions: Mutex<VecDeque<Result<String, AppError>>>,
    pub requests: Mutex<Vec<HubRequest>>,
    pub summaries: Mutex<Vec<HubRequest>>,
    /// The run key of every call, streamed or not, in call order.
    pub metas: Mutex<Vec<CallMeta>>,
}

impl FakeBackend {
    pub(super) fn new(clock: &Arc<ManualClock>) -> Self {
        Self {
            clock: Arc::clone(clock),
            streams: Mutex::default(),
            completions: Mutex::default(),
            requests: Mutex::default(),
            summaries: Mutex::default(),
            metas: Mutex::default(),
        }
    }

    pub(super) fn script(self, steps: Vec<Step>) -> Self {
        self.streams.lock().unwrap().push_back((None, steps));
        self
    }

    /// A call the run's model fails with 503 before any content and `fallback` answers with
    /// `steps`, getting the request reshaped for it like the client does.
    pub(super) fn script_fallback(self, fallback: TargetFacts, steps: Vec<Step>) -> Self {
        self.streams
            .lock()
            .unwrap()
            .push_back((Some(fallback), steps));
        self
    }

    pub(super) fn summary(self, result: Result<&str, AppError>) -> Self {
        self.completions
            .lock()
            .unwrap()
            .push_back(result.map(str::to_owned));
        self
    }

    pub(super) fn request_count(&self) -> usize {
        self.requests.lock().unwrap().len()
    }
}

impl ChatBackend for FakeBackend {
    fn stream(
        &self,
        meta: &CallMeta,
        _target: ChatTarget,
        mut request: HubRequest,
        _cancel: Option<CancelToken>,
    ) -> BoxStream<'static, Result<HubEvent, ChatError>> {
        let (fallback, mut steps) = self
            .streams
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| (None, text_turn("(unscripted)")));
        if let Some(facts) = fallback {
            let reshape: Reshape = Arc::new(super::reshape_for_target);
            shape_for(&reshape, &facts, &mut request);
            let failed = HubAttempt {
                provider_id: "main".into(),
                model: "m".into(),
                key_id: None,
                kind: Some(
                    spanbay_llm::llm::failure::FailureKind::ProviderError
                        .as_str()
                        .into(),
                ),
                status: Some(503),
                action: "next_target".into(),
                why: "503".into(),
            };
            steps.insert(
                0,
                Step::Event(HubEvent::Served {
                    provider_id: facts.provider_id,
                    model: facts.model,
                    attempts: vec![failed],
                }),
            );
        }
        self.requests.lock().unwrap().push(request);
        self.metas.lock().unwrap().push(meta.clone());
        let clock = Arc::clone(&self.clock);
        stream::unfold(steps.into_iter(), move |mut steps| {
            let clock = Arc::clone(&clock);
            async move {
                loop {
                    match steps.next()? {
                        Step::Event(event) => return Some((Ok(event), steps)),
                        Step::Fail(error) => return Some((Err(error), steps)),
                        Step::Advance(by) => clock.advance(by),
                        Step::Hang => std::future::pending::<()>().await,
                    }
                }
            }
        })
        .boxed()
    }

    fn complete(
        &self,
        meta: &CallMeta,
        _target: &ChatTarget,
        request: HubRequest,
        _cancel: Option<&CancelToken>,
    ) -> impl Future<Output = Result<(HubResponse, Option<ServedModel>), AppError>> + Send {
        self.summaries.lock().unwrap().push(request);
        self.metas.lock().unwrap().push(meta.clone());
        let next = self
            .completions
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or_else(|| Ok("summary".to_owned()));
        async move {
            next.map(|text| {
                let response = HubResponse {
                    id: "c".into(),
                    model: "m".into(),
                    content: vec![HubPart::text(text)],
                    finish: spanbay_llm::llm::hub::HubFinish::Stop,
                    usage: HubUsage::default(),
                };
                (response, None)
            })
        }
    }
}

pub(super) fn start() -> Step {
    Step::Event(HubEvent::Start {
        id: "r".into(),
        model: "m".into(),
    })
}

pub(super) fn text(text: &str) -> Step {
    Step::Event(HubEvent::TextDelta { text: text.into() })
}

pub(super) fn finish(reason: spanbay_llm::llm::hub::HubFinish) -> Step {
    Step::Event(HubEvent::Finish { reason })
}

pub(super) fn usage(input: u64, output: u64) -> Step {
    Step::Event(HubEvent::Usage {
        usage: HubUsage {
            input_tokens: Some(input),
            output_tokens: Some(output),
            ..HubUsage::default()
        },
    })
}

/// A complete turn that answers `answer`.
pub(super) fn text_turn(answer: &str) -> Vec<Step> {
    vec![
        start(),
        text(answer),
        usage(10, 5),
        finish(spanbay_llm::llm::hub::HubFinish::Stop),
    ]
}

/// A complete turn that calls the tools `(id, name, arguments)`.
pub(super) fn call_turn(calls: &[(&str, &str, &str)]) -> Vec<Step> {
    let mut steps = vec![start(), text("let me check"), usage(20, 8)];
    for (index, (id, name, arguments)) in calls.iter().enumerate() {
        let index = u32::try_from(index).unwrap();
        steps.push(Step::Event(HubEvent::ToolCallStart {
            index,
            id: (*id).into(),
            name: (*name).into(),
            signature: None,
        }));
        steps.push(Step::Event(HubEvent::ToolCallDelta {
            index,
            arguments: (*arguments).into(),
        }));
    }
    steps.push(finish(spanbay_llm::llm::hub::HubFinish::ToolCalls));
    steps
}

pub(super) fn context_length_failure() -> Step {
    Step::Fail(ChatError::new(
        ChatErrorKind::Failed,
        AppError::network("This model's maximum context length is 8192 tokens"),
    ))
}

// ---------------------------------------------------------------------------------- the host

/// What a fake tool does when called.
pub(super) type Behavior =
    Arc<dyn Fn(Arc<FakeHostState>, ToolCall) -> BoxFuture<'static, ToolOutput> + Send + Sync>;

pub(super) fn says(text: &'static str) -> Behavior {
    Arc::new(move |_, _| async move { ToolOutput::text(text) }.boxed())
}

struct FakeTool {
    class: ToolClass,
    parallel_safe: bool,
    verdict: Arc<dyn Fn(&ToolCall) -> Verdict + Send + Sync>,
    behavior: Behavior,
}

/// A stored message of the in-memory chat.
#[derive(Clone)]
pub(super) struct Stored {
    pub id: String,
    pub role: AgentRole,
    pub parts: Vec<HubPart>,
    pub partial: bool,
    pub error: Option<String>,
    pub steered: bool,
    pub failure: Option<AgentFailure>,
}

/// The mutable state of a [`FakeHost`], shared with tool behaviours.
#[derive(Default)]
pub(super) struct FakeHostState {
    pub messages: Mutex<Vec<Stored>>,
    /// `(summary index in messages, index of the last replaced message)`.
    pub cut: Mutex<Option<(usize, usize)>>,
    pub events: Mutex<Vec<AgentEvent>>,
    pub audits: Mutex<Vec<CallAudit>>,
    pub steer: Mutex<Vec<String>>,
    pub ran: Mutex<Vec<(String, String)>>,
    /// Failure classes the runner noted for the run summary.
    pub failures: Mutex<Vec<AgentFailure>>,
    pub partial_saves: AtomicUsize,
    pub attachment_resolutions: AtomicUsize,
    pub approvals_asked: Mutex<Vec<(String, AskInfo)>>,
    pub cost_pauses: Mutex<Vec<(i64, i64, String)>>,
    pub totals: Mutex<RunTotals>,
    /// Spend in other currencies (a sub-agent on another model); the run's own calls are USD.
    pub other_spend: Mutex<Vec<(String, i64)>>,
    /// What [`RunHost::model_info`] answers per `provider_id/model` (unknown otherwise).
    pub served_models: Mutex<HashMap<String, ModelInfo>>,
    /// The `currency` argument of every `record_usage` call.
    pub usage_currencies: Mutex<Vec<Option<String>>>,
    /// The `reasoning_effort` of every assistant draft saved.
    pub saved_efforts: Mutex<Vec<Option<crate::agent::model::ReasoningEffort>>>,
    pub active: AtomicUsize,
    pub max_active: AtomicUsize,
    /// Full outputs the runner kept, by call id.
    pub kept: Mutex<Vec<(String, String)>>,
}

/// An in-memory [`RunHost`].
pub(super) struct FakeHost {
    pub clock: Arc<ManualClock>,
    pub state: Arc<FakeHostState>,
    tools: HashMap<String, FakeTool>,
    approvals: Mutex<VecDeque<ApprovalOutcome>>,
    cost_decisions: Mutex<VecDeque<Option<CostDecision>>>,
    needles: Vec<String>,
    next_id: AtomicUsize,
    /// Approvals are announced on the stream, as the app's host does.
    announce: bool,
    /// `result_cap_bytes`; with a cap the host keeps cut outputs.
    result_cap: Option<usize>,
    /// `folder_untrusted`.
    untrusted_folder: bool,
}

impl FakeHost {
    pub(super) fn new(clock: &Arc<ManualClock>) -> Self {
        Self {
            clock: Arc::clone(clock),
            state: Arc::default(),
            tools: HashMap::new(),
            approvals: Mutex::default(),
            cost_decisions: Mutex::default(),
            needles: Vec::new(),
            next_id: AtomicUsize::new(0),
            announce: false,
            result_cap: None,
            untrusted_folder: false,
        }
    }

    /// The run works in a folder the user has not trusted.
    pub(super) fn untrusted_folder(mut self) -> Self {
        self.untrusted_folder = true;
        self
    }

    /// Cuts results at `cap` bytes and keeps the full output of a cut result.
    pub(super) fn capped(mut self, cap: usize) -> Self {
        self.result_cap = Some(cap);
        self
    }

    pub(super) fn tool(
        mut self,
        name: &str,
        class: ToolClass,
        parallel_safe: bool,
        verdict: Verdict,
        behavior: Behavior,
    ) -> Self {
        self.tools.insert(
            name.to_owned(),
            FakeTool {
                class,
                parallel_safe,
                verdict: Arc::new(move |_| verdict.clone()),
                behavior,
            },
        );
        self
    }

    /// A tool whose verdict depends on the call.
    pub(super) fn tool_with(
        mut self,
        name: &str,
        verdict: impl Fn(&ToolCall) -> Verdict + Send + Sync + 'static,
        behavior: Behavior,
    ) -> Self {
        self.tools.insert(
            name.to_owned(),
            FakeTool {
                class: ToolClass::Exec,
                parallel_safe: false,
                verdict: Arc::new(verdict),
                behavior,
            },
        );
        self
    }

    pub(super) fn approve_with(self, outcome: ApprovalOutcome) -> Self {
        self.approvals.lock().unwrap().push_back(outcome);
        self
    }

    pub(super) fn cost_decision(self, decision: Option<CostDecision>) -> Self {
        self.cost_decisions.lock().unwrap().push_back(decision);
        self
    }

    /// Emits `ToolApprovalRequired` before an approval and `ToolApprovalResolved` after an
    /// answer, as `agent_service/host.rs` does.
    pub(super) fn announcing(mut self) -> Self {
        self.announce = true;
        self
    }

    pub(super) fn redacting(mut self, needle: &str) -> Self {
        self.needles.push(needle.to_owned());
        self
    }

    pub(super) fn say(self, role: AgentRole, text: &str) -> Self {
        let id = self.id();
        self.state.messages.lock().unwrap().push(Stored {
            id,
            role,
            parts: vec![HubPart::text(text)],
            partial: false,
            error: None,
            steered: false,
            failure: None,
        });
        self
    }

    pub(super) fn push_messages(self, messages: Vec<Stored>) -> Self {
        self.state.messages.lock().unwrap().extend(messages);
        self
    }

    fn id(&self) -> String {
        format!("m{}", self.next_id.fetch_add(1, Ordering::SeqCst))
    }

    pub(super) fn events(&self) -> Vec<AgentEvent> {
        self.state.events.lock().unwrap().clone()
    }

    pub(super) fn messages(&self) -> Vec<Stored> {
        self.state.messages.lock().unwrap().clone()
    }

    pub(super) fn ran(&self) -> Vec<(String, String)> {
        self.state.ran.lock().unwrap().clone()
    }

    pub(super) fn audit_states(&self, call_id: &str) -> Vec<&'static str> {
        self.state
            .audits
            .lock()
            .unwrap()
            .iter()
            .filter(|audit| audit.call_id == call_id)
            .map(|audit| audit.state)
            .collect()
    }

    /// The `ToolResult` events as `(call id, status)`.
    pub(super) fn results(&self) -> Vec<(String, crate::agent::model::ToolCallStatus)> {
        self.events()
            .into_iter()
            .filter_map(|event| match event {
                AgentEvent::ToolResult {
                    call_id, status, ..
                } => Some((call_id, status)),
                _ => None,
            })
            .collect()
    }

    /// The text of the stored result of `call_id`.
    pub(super) fn result_text(&self, call_id: &str) -> Option<String> {
        self.messages().iter().find_map(|message| {
            message.parts.iter().find_map(|part| match part {
                HubPart::ToolResult {
                    call_id: id,
                    content,
                    ..
                } if id == call_id => Some(spanbay_llm::llm::hub::text_of(content)),
                _ => None,
            })
        })
    }

    fn history_messages(&self) -> Vec<Stored> {
        let messages = self.messages();
        match *self.state.cut.lock().unwrap() {
            Some((summary, through)) => {
                let mut history = vec![messages[summary].clone()];
                history.extend(
                    messages
                        .iter()
                        .enumerate()
                        .filter(|(index, message)| {
                            *index > through && message.role != AgentRole::Summary
                        })
                        .map(|(_, message)| message.clone()),
                );
                history
            }
            None => messages,
        }
    }
}

fn agent_message(stored: &Stored) -> AgentMessage {
    AgentMessage {
        id: stored.id.clone(),
        session_id: "s".into(),
        seq: 0,
        role: stored.role,
        parts: stored.parts.clone(),
        usage: None,
        model: None,
        error: stored.error.clone(),
        run_id: Some("r".into()),
        parent_id: None,
        branch_index: 0,
        partial: stored.partial,
        cost_micro_usd: 0,
        cost_currency: None,
        meta: AgentMessageMeta {
            steered: stored.steered,
            failure: stored.failure.clone(),
            ..AgentMessageMeta::default()
        },
        branch_siblings: Vec::new(),
        attachments: Vec::new(),
        created_at_ms: 0,
    }
}

impl RunHost for FakeHost {
    fn record_failure(&self, failure: AgentFailure) {
        self.state.failures.lock().unwrap().push(failure);
    }

    fn emit(&self, event: AgentEvent) {
        self.state.events.lock().unwrap().push(event);
    }

    fn clock(&self) -> &dyn RunClock {
        &*self.clock
    }

    fn run_key(&self) -> (String, spanbay_llm::llm::client::CallPurpose) {
        ("run-1".into(), spanbay_llm::llm::client::CallPurpose::Step)
    }

    fn tool_source(&self, _name: &str) -> ToolSource {
        ToolSource::Builtin
    }

    fn result_cap_bytes(&self) -> usize {
        self.result_cap.unwrap_or(usize::MAX)
    }

    fn folder_untrusted(&self) -> bool {
        self.untrusted_folder
    }

    async fn spill_output(&self, call_id: &str, text: String) -> Option<u64> {
        self.result_cap?;
        let size = text.len() as u64;
        self.state
            .kept
            .lock()
            .unwrap()
            .push((call_id.to_owned(), text));
        Some(size)
    }

    fn redact(&self, parts: Vec<HubPart>) -> Vec<HubPart> {
        parts
            .into_iter()
            .map(|part| match part {
                HubPart::Text { text, .. } => HubPart::text(redact_secrets(&text, &self.needles)),
                other => other,
            })
            .collect()
    }

    async fn load_history(&self) -> Result<Vec<HistoryEntry>, AppError> {
        Ok(self
            .history_messages()
            .into_iter()
            .map(|message| HistoryEntry {
                summary: message.role == AgentRole::Summary,
                id: message.id,
                message: HubMessage {
                    role: hub_role(message.role),
                    content: message.parts,
                },
            })
            .collect())
    }

    async fn resolve_attachments(&self, messages: Vec<HubMessage>) -> Vec<HubMessage> {
        self.state
            .attachment_resolutions
            .fetch_add(1, Ordering::SeqCst);
        messages
    }

    fn steer_pending(&self) -> bool {
        !self.state.steer.lock().unwrap().is_empty()
    }

    async fn drain_steer(&self) -> Result<bool, AppError> {
        let steers: Vec<String> = std::mem::take(&mut *self.state.steer.lock().unwrap());
        for text in &steers {
            let stored = Stored {
                id: self.id(),
                role: AgentRole::User,
                parts: vec![HubPart::text(text.clone())],
                partial: false,
                error: None,
                steered: true,
                failure: None,
            };
            self.emit(AgentEvent::UserMessage {
                message: Box::new(agent_message(&stored)),
                skills_undelivered: Vec::new(),
            });
            self.state.messages.lock().unwrap().push(stored);
        }
        Ok(!steers.is_empty())
    }

    async fn save_partial(&self, message_id: &str, parts: Vec<HubPart>) {
        self.state.partial_saves.fetch_add(1, Ordering::SeqCst);
        let mut messages = self.state.messages.lock().unwrap();
        if let Some(existing) = messages.iter_mut().find(|m| m.id == message_id) {
            existing.parts = parts;
        } else {
            messages.push(Stored {
                id: message_id.to_owned(),
                role: AgentRole::Assistant,
                parts,
                partial: true,
                error: None,
                steered: false,
                failure: None,
            });
        }
    }

    async fn save_assistant(&self, draft: AssistantDraft) -> Result<AgentMessage, AppError> {
        self.state
            .saved_efforts
            .lock()
            .unwrap()
            .push(draft.reasoning_effort);
        let stored = Stored {
            id: draft.id.clone(),
            role: AgentRole::Assistant,
            parts: draft.parts,
            partial: false,
            error: draft.error,
            steered: false,
            failure: draft.failure,
        };
        let mut messages = self.state.messages.lock().unwrap();
        if let Some(existing) = messages.iter_mut().find(|m| m.id == draft.id) {
            *existing = stored.clone();
        } else {
            messages.push(stored.clone());
        }
        Ok(agent_message(&stored))
    }

    async fn save_tool_turn(
        &self,
        draft: AssistantDraft,
    ) -> Result<(AgentMessage, AgentMessage), AppError> {
        let placeholders: Vec<HubPart> = draft
            .parts
            .iter()
            .filter_map(|part| match part {
                HubPart::ToolCall { id, name, .. } => Some(HubPart::ToolResult {
                    call_id: id.clone(),
                    name: Some(name.clone()),
                    content: vec![HubPart::text("[interrupted before tool finished]")],
                    is_error: true,
                }),
                _ => None,
            })
            .collect();
        let assistant = self.save_assistant(draft).await?;
        let tool = Stored {
            id: self.id(),
            role: AgentRole::Tool,
            parts: placeholders,
            partial: false,
            error: None,
            steered: false,
            failure: None,
        };
        self.state.messages.lock().unwrap().push(tool.clone());
        Ok((assistant, agent_message(&tool)))
    }

    async fn save_tool_result(
        &self,
        tool_message_id: &str,
        result: HubPart,
    ) -> Result<(), AppError> {
        let HubPart::ToolResult { call_id, .. } = &result else {
            panic!("a tool result was expected");
        };
        let mut messages = self.state.messages.lock().unwrap();
        let message = messages
            .iter_mut()
            .find(|m| m.id == tool_message_id)
            .expect("tool message");
        let slot = message
            .parts
            .iter_mut()
            .find(|part| matches!(part, HubPart::ToolResult { call_id: id, .. } if id == call_id))
            .expect("pending result");
        *slot = result;
        Ok(())
    }

    async fn save_summary(
        &self,
        text: String,
        compacted_through: &str,
    ) -> Result<AgentMessage, AppError> {
        let mut messages = self.state.messages.lock().unwrap();
        let through = messages
            .iter()
            .position(|m| m.id == compacted_through)
            .expect("message to compact through");
        let stored = Stored {
            id: format!("summary{}", messages.len()),
            role: AgentRole::Summary,
            parts: vec![HubPart::text(text)],
            partial: false,
            error: None,
            steered: false,
            failure: None,
        };
        messages.push(stored.clone());
        *self.state.cut.lock().unwrap() = Some((messages.len() - 1, through));
        Ok(agent_message(&stored))
    }

    async fn record_call(&self, audit: CallAudit) {
        self.state.audits.lock().unwrap().push(audit);
    }

    async fn record_usage(
        &self,
        usage: &HubUsage,
        cost: &super::CallCost,
        _context_tokens: u64,
        _purpose: spanbay_llm::llm::client::CallPurpose,
    ) -> Result<RunTotals, AppError> {
        let (cost_micro, currency) = (cost.micro_or_zero(), cost.currency.as_deref());
        self.state
            .usage_currencies
            .lock()
            .unwrap()
            .push(currency.map(str::to_owned));
        let mut totals = self.state.totals.lock().unwrap();
        super::add_usage(&mut totals.usage, usage);
        match currency {
            None => {}
            Some("USD") => totals.cost_micro += cost_micro,
            // A fallback priced in another currency counts there, as the app host does.
            Some(other) => self
                .state
                .other_spend
                .lock()
                .unwrap()
                .push((other.to_owned(), cost_micro)),
        }
        Ok(totals.clone())
    }

    async fn model_info(&self, served: &ServedModel) -> ModelInfo {
        self.state
            .served_models
            .lock()
            .unwrap()
            .get(&served.id_form())
            .cloned()
            .unwrap_or_default()
    }

    fn run_tokens(&self) -> u64 {
        let totals = self.state.totals.lock().unwrap();
        totals.usage.input_tokens.unwrap_or(0) + totals.usage.output_tokens.unwrap_or(0)
    }

    fn run_cost_micro(&self, currency: &str) -> i64 {
        let own = if currency == "USD" {
            self.state.totals.lock().unwrap().cost_micro
        } else {
            0
        };
        let other: i64 = self
            .state
            .other_spend
            .lock()
            .unwrap()
            .iter()
            .filter(|(c, _)| c == currency)
            .map(|(_, micro)| *micro)
            .sum();
        own + other
    }

    fn parallel_safe(&self, name: &str) -> bool {
        self.tools.get(name).is_some_and(|tool| tool.parallel_safe)
    }

    async fn plan_call(&self, call: &ToolCall) -> ToolPlan {
        let Some(tool) = self.tools.get(&call.name) else {
            return ToolPlan {
                class: ToolClass::Internal,
                risk: "low",
                parallel_safe: false,
                verdict: Verdict::Invalid(format!("There is no tool named {}.", call.name)),
                decided_by: None,
            };
        };
        ToolPlan {
            class: tool.class,
            risk: "low",
            parallel_safe: tool.parallel_safe,
            verdict: (tool.verdict)(call),
            decided_by: None,
        }
    }

    async fn request_approval(&self, call: &ToolCall, ask: &AskInfo) -> ApprovalOutcome {
        self.state
            .approvals_asked
            .lock()
            .unwrap()
            .push((call.id.clone(), ask.clone()));
        if self.announce {
            self.emit(AgentEvent::ToolApprovalRequired {
                run_id: "r".into(),
                call_id: call.id.clone(),
                name: call.name.clone(),
                arguments: call.arguments.clone(),
                class: ask.class,
                read_only: ask.read_only,
                destructive: ask.destructive,
                sandbox: SandboxKind::None,
                reason: ask.reason.clone(),
                preview: ask.preview.clone(),
                network: ask.network,
                risk: ask.risk.to_owned(),
                summary: ask.reason.clone(),
                full_text: None,
                card: ask.card.clone(),
            });
        }
        let outcome = self
            .approvals
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(ApprovalOutcome::Cancelled);
        if let (true, ApprovalOutcome::Answered(answer)) = (self.announce, &outcome) {
            self.emit(AgentEvent::ToolApprovalResolved {
                call_id: call.id.clone(),
                decision: answer.decision,
                scope: answer.scope,
                timed_out: false,
            });
        }
        if outcome == ApprovalOutcome::Superseded {
            // What superseded it is the user's steer message.
            self.state.steer.lock().unwrap().push("steered".to_owned());
        }
        outcome
    }

    async fn run_tool(&self, call: &ToolCall, _options: RunOptions) -> ToolOutput {
        self.state
            .ran
            .lock()
            .unwrap()
            .push((call.name.clone(), call.arguments.clone()));
        let tool = self.tools.get(&call.name).expect("planned tool exists");
        let active = self.state.active.fetch_add(1, Ordering::SeqCst) + 1;
        self.state.max_active.fetch_max(active, Ordering::SeqCst);
        let output = (tool.behavior)(Arc::clone(&self.state), call.clone()).await;
        self.state.active.fetch_sub(1, Ordering::SeqCst);
        output
    }

    async fn await_cost_decision(&self, pause: &super::CostPause<'_>) -> Option<CostDecision> {
        self.state.cost_pauses.lock().unwrap().push((
            pause.spent,
            pause.cap,
            pause.currency.unwrap_or(super::TOKENS_CAP).to_owned(),
        ));
        self.cost_decisions
            .lock()
            .unwrap()
            .pop_front()
            .unwrap_or(Some(CostDecision::Stop))
    }
}
