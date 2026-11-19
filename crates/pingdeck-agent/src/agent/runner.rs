//! The tool-calling loop: stream an answer, run the tools it asks for, feed the results back.
//!
//! Single responsibility: one agent run over an abstract model ([`ChatBackend`]) and an abstract
//! environment ([`RunHost`]: persistence, permissions, approvals, tool execution, events), so the
//! loop is testable without a network or a database. The loop, per step: take the steer messages
//! the user sent and the background exits since the last step, pause at the cost cap, compact the
//! history when it is nearly full, call the model (streaming; a stall or a transient failure
//! before any content is retried; a context-length error compacts once and retries), store the
//! answer (partial rows every 2 s, one transaction for a tool turn), run the tool calls
//! (permission, approval, parallel reads), store each result past the progress probe. The last
//! step of a run with a step limit forbids tool calls and asks for a summary; a call cut
//! off by the output cap is never run. It returns how the run stopped; the caller turns
//! that into the run's final status and its one `Done` or `Error`.

mod progress;
mod stream;
mod tools;
mod wake;

#[cfg(test)]
mod fakes;
#[cfg(test)]
mod prefix_tests;
#[cfg(test)]
mod tests;
#[cfg(test)]
mod transcripts;

use std::time::{Duration, Instant};

use futures_util::future::BoxFuture;
use futures_util::stream::BoxStream;

use pingdeck_base::cancel::CancelToken;
use pingdeck_base::error::AppError;
use pingdeck_llm::llm::capabilities::ModelCapabilities;
use pingdeck_llm::llm::client::{
    CallMeta, CallPurpose, ChatError, ChatErrorKind, ChatTarget, LlmClient, TargetFacts,
};
use pingdeck_llm::llm::failure::FailureKind;
use pingdeck_llm::llm::hub::{
    CacheBreakpoint, CacheHint, HubEvent, HubFinish, HubMessage, HubNativeTool, HubReasoning,
    HubRequest, HubResponse, HubRole, HubTool, HubToolChoice, HubUsage, lower_native_tools,
};
use pingdeck_llm::llm::usage::{BookedCost, CostUsage, PriceResolution, book_cost};

use super::approvals::ApprovalOutcome;
use super::compaction;
use super::context::{
    ContextState, ResultScope, advance_watermark, estimate_context_tokens, full_result_text,
    prune_calls, repair_orphan_tool_calls, result_cap_bytes, scoped_result_bytes,
    shorten_scoped_results, shorten_turn_results, turn_cap_bytes, window_from_error,
};
use super::failure::{AgentFailure, error_kind_name};
use super::model::{
    AgentEvent, AgentMessage, AgentRunResult, AgentStopReason, ReasoningEffort, ToolCallStatus,
};
use super::registry::CostDecision;
use super::tools::{ToolClass, ToolOutput, ToolSource, ToolSpec};

pub(crate) use super::limits::{STALL_TIMEOUT, WAKE_STALL_TIMEOUT};
/// How often a streaming answer is saved as a partial row.
pub const PARTIAL_SAVE_INTERVAL: Duration = Duration::from_secs(2);
/// How long a cancelled tool call may take to wind down before it is abandoned.
pub const CANCEL_GRACE: Duration = Duration::from_secs(2);
/// Consecutive parallel-safe calls that run at once.
pub const MAX_PARALLEL_CALLS: usize = 4;
/// Failed proactive compactions after which a run stops trying.
const COMPACTION_ATTEMPTS: u32 = 2;
/// What the model reads after the history on the last step; it is not stored.
const WRAP_UP_PROMPT: &str = "[System notice] This is your last step. Do not call tools. Summarize what you did, what is left and what the user should decide.";
/// The result of a call made on the last step anyway.
const STEP_LIMIT_TEXT: &str = "Not run: the step limit was reached.";
/// The result of a call whose arguments the output token cap cut off.
const TRUNCATED_CALL_TEXT: &str = "Your output hit the token limit while writing this call. Split the content into smaller calls (e.g. write the file in parts).";

/// The model side of a run. Every call carries its run key (`meta`), which its log row keeps
pub trait ChatBackend: Sync {
    /// Streams one answer; an `Err` item ends the stream, dropping it cancels the upstream call.
    /// A backend that fails over sends each target the request adapted with
    /// [`reshape_for_target`].
    fn stream(
        &self,
        meta: &CallMeta,
        target: ChatTarget,
        request: HubRequest,
        cancel: Option<CancelToken>,
    ) -> BoxStream<'static, Result<HubEvent, ChatError>>;

    /// One complete, non-streamed answer (compaction summaries), with the model that answered
    /// when the backend knows it.
    fn complete(
        &self,
        meta: &CallMeta,
        target: &ChatTarget,
        request: HubRequest,
        cancel: Option<&CancelToken>,
    ) -> impl Future<Output = Result<(HubResponse, Option<ServedModel>), AppError>> + Send;
}

/// The provider model that answered a call (a fallback may differ from the run's model).
pub use pingdeck_llm::llm::store::ModelRef as ServedModel;

impl ChatBackend for LlmClient {
    fn stream(
        &self,
        meta: &CallMeta,
        target: ChatTarget,
        request: HubRequest,
        cancel: Option<CancelToken>,
    ) -> BoxStream<'static, Result<HubEvent, ChatError>> {
        LlmClient::stream_reshaped(
            &self.for_call(meta),
            target,
            request,
            std::sync::Arc::new(reshape_for_target),
            cancel,
        )
    }

    fn complete(
        &self,
        meta: &CallMeta,
        target: &ChatTarget,
        request: HubRequest,
        cancel: Option<&CancelToken>,
    ) -> impl Future<Output = Result<(HubResponse, Option<ServedModel>), AppError>> + Send {
        let client = self.for_call(meta);
        let target = target.clone();
        let cancel = cancel.cloned();
        async move {
            let call = LlmClient::complete_served(&client, &target, request, cancel.as_ref());
            call.await
                .map(|(response, served)| (response, Some(served)))
        }
    }
}

/// The run key of one model call: a step writes `message_id`; a summary has none.
pub(crate) fn call_meta<H: RunHost>(host: &H, message_id: Option<&str>, summary: bool) -> CallMeta {
    let (run_id, purpose) = host.run_key();
    CallMeta {
        run_id,
        message_id: message_id.map(str::to_owned),
        purpose: if summary {
            CallPurpose::Compaction
        } else {
            purpose
        },
    }
}

/// The run's request adapted to the target an attempt calls, which may be a fallback: the
/// current turn's tool results fit that target's window and native tools it cannot run are left
/// out. The client has already replaced images for a model known not to read them. For the run's
/// own model this changes nothing: the request was built from the same facts.
pub(crate) fn reshape_for_target(facts: &TargetFacts, request: &mut HubRequest) {
    shorten_turn_results(&mut request.messages, turn_cap_bytes(facts.context_limit));
    lower_native_tools(request, &facts.capabilities);
    if !caches(&facts.capabilities) {
        request.cache = None;
    }
}

/// Whether the catalogue (or a probe, or the user) says the model caches prompts: the default
/// of `ai.agent.prompt_cache = auto`. Unknown relays stay off.
fn caches(capabilities: &ModelCapabilities) -> bool {
    let state = capabilities.prompt_caching;
    state.supported && state.source != pingdeck_model::capabilities::CapabilitySource::Unknown
}

/// Time as the loop sees it, so stalls, partial saves and the cancel grace period are testable
/// without sleeping.
pub trait RunClock: Send + Sync {
    /// A monotonic reading.
    fn now(&self) -> Instant;
    /// A timer for `duration`.
    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()>;
    /// A timer that ends after `stall`, or `after_wake` after the machine woke from sleep,
    /// whichever comes first: a stream that stayed silent across a sleep is judged sooner.
    /// A clock that cannot see sleep is a plain `stall` timer.
    fn stall_wait(&self, stall: Duration, _after_wake: Duration) -> BoxFuture<'static, ()> {
        self.sleep(stall)
    }
}

/// The real clock.
pub struct SystemClock;

impl RunClock for SystemClock {
    fn now(&self) -> Instant {
        Instant::now()
    }

    fn sleep(&self, duration: Duration) -> BoxFuture<'static, ()> {
        Box::pin(tokio::time::sleep(duration))
    }

    fn stall_wait(&self, stall: Duration, after_wake: Duration) -> BoxFuture<'static, ()> {
        Box::pin(wake::wake_aware_sleep(
            stall,
            after_wake,
            wake::TICK,
            pingdeck_base::time::now_ms,
        ))
    }
}

/// One stored message of the history the model sees: its id (for compaction's cut point), the hub
/// form, and whether it is a compaction summary.
#[derive(Debug, Clone)]
pub struct HistoryEntry {
    pub id: String,
    pub message: HubMessage,
    pub summary: bool,
}

/// An assistant message to store.
#[derive(Debug, Clone)]
pub struct AssistantDraft {
    pub id: String,
    pub parts: Vec<pingdeck_llm::llm::hub::HubPart>,
    pub usage: Option<HubUsage>,
    pub model: String,
    pub error: Option<String>,
    /// What the call cost, in the currency of the model that answered.
    pub cost: CallCost,
    /// `provider_id/model` of a fallback that answered instead of the run's model.
    pub served_model: Option<String>,
    /// The reasoning effort the call sent; `None` when it carried none.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Milliseconds from sending the call to its first answer content.
    pub ttft_ms: Option<u64>,
    /// Milliseconds from the first answer content to the end of the stream.
    pub stream_ms: Option<u64>,
    /// `AppError` kind of the failure that cut the message short.
    pub error_kind: Option<String>,
    /// The structured class of that failure.
    pub failure: Option<AgentFailure>,
}

/// One tool call the model made.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ToolCall {
    pub id: String,
    pub name: String,
    pub arguments: String,
}

/// What the permission gate says about a call.
#[derive(Debug, Clone, PartialEq)]
pub enum Verdict {
    /// Run it now.
    Run,
    /// Ask the user first.
    Ask(AskInfo),
    /// Refuse without asking (`Blocked`).
    Refuse(String),
    /// The call cannot run (unknown tool, bad arguments); the text goes to the model.
    Invalid(String),
}

/// What the approval card shows.
#[derive(Debug, Clone, PartialEq)]
pub struct AskInfo {
    pub reason: String,
    pub class: ToolClass,
    pub read_only: bool,
    pub destructive: bool,
    pub preview: Option<super::tools::ToolApprovalPreview>,
    /// The call is a shell command that will run without network unless allowed once.
    pub network: bool,
    /// Everything the card shows about the call (risk, tags, scopes, why it asks); it also decides
    /// which rule an answer may leave behind.
    pub card: Option<Box<super::permissions::ApprovalCard>>,
    /// `low`, `medium` or `high`.
    pub risk: &'static str,
}

/// A call classified and checked, ready to run or ask about.
#[derive(Debug, Clone, PartialEq)]
pub struct ToolPlan {
    pub class: ToolClass,
    /// `low`, `medium` or `high`.
    pub risk: &'static str,
    pub parallel_safe: bool,
    pub verdict: Verdict,
    /// The audit's `decided_by` of the verdict (the permission layer that decided); `None` reads
    /// as `mode`.
    pub decided_by: Option<String>,
}

/// How to run an approved call.
#[derive(Debug, Clone, Copy, Default)]
pub struct RunOptions {
    /// "Allow once with network" was chosen for this call.
    pub allow_network_once: bool,
}

/// What the run has used so far (sub-agents included in the cost).
#[derive(Debug, Clone, Default)]
pub struct RunTotals {
    pub usage: HubUsage,
    pub cost_micro: i64,
}

/// The audit row of a tool call at one moment; the host stores it (`agent_tool_calls`).
#[derive(Debug, Clone, PartialEq)]
pub struct CallAudit {
    pub call_id: String,
    pub name: String,
    pub class: ToolClass,
    pub state: &'static str,
    pub arguments: String,
    pub risk: &'static str,
    pub message_id: String,
    pub decided_by: Option<String>,
    pub decision: Option<&'static str>,
    pub decided_ms: Option<i64>,
    pub started_ms: Option<i64>,
    pub ended_ms: Option<i64>,
    pub output_bytes: u64,
    /// The result the model saw was cut.
    pub truncated: bool,
    /// The handle of the full output on disk (the call id), when it was kept.
    pub spill_ref: Option<String>,
    /// The exit status of the process a finished call ran (`shell`), when it had one.
    pub exit_code: Option<i32>,
}

/// The environment of a run: everything the loop needs that is not a pure function.
pub trait RunHost: Sync {
    /// Publishes `event` to the run's stream.
    fn emit(&self, event: AgentEvent);
    /// The run's clock.
    fn clock(&self) -> &dyn RunClock;
    /// The key the run's model calls are logged with: the top-level run's id, and `Subagent` for
    /// a sub-agent's loop.
    fn run_key(&self) -> (String, CallPurpose) {
        (String::new(), CallPurpose::Step)
    }
    /// Where the tool named `name` comes from.
    fn tool_source(&self, name: &str) -> ToolSource;
    /// Redacts known secrets from the text parts of a tool result before the model sees them.
    fn redact(
        &self,
        parts: Vec<pingdeck_llm::llm::hub::HubPart>,
    ) -> Vec<pingdeck_llm::llm::hub::HubPart>;

    /// The stored history the model sees: the latest summary and what follows it.
    fn load_history(&self) -> impl Future<Output = Result<Vec<HistoryEntry>, AppError>> + Send;
    /// Replaces attachment references with their content for the target model.
    fn resolve_attachments(
        &self,
        messages: Vec<HubMessage>,
    ) -> impl Future<Output = Vec<HubMessage>> + Send;
    /// Whether the user steered the run since the last step boundary.
    fn steer_pending(&self) -> bool;
    /// Stores the steer messages as user messages (emitting `UserMessage`); `true` when any.
    fn drain_steer(&self) -> impl Future<Output = Result<bool, AppError>> + Send;
    /// Stores one app-written message per background process of the chat that ended since the
    /// last step, so the model hears of it once (AI-loop-11).
    fn drain_process_exits(&self) -> impl Future<Output = Result<(), AppError>> + Send {
        async { Ok(()) }
    }
    /// Saves the streaming answer so far; a failure only loses a recovery point.
    fn save_partial(
        &self,
        message_id: &str,
        parts: Vec<pingdeck_llm::llm::hub::HubPart>,
    ) -> impl Future<Output = ()> + Send;
    /// Stores a finished assistant message (finishing the partial row of its id).
    fn save_assistant(
        &self,
        draft: AssistantDraft,
    ) -> impl Future<Output = Result<AgentMessage, AppError>> + Send;
    /// Stores a tool turn: the assistant message and the tool message with one pending result per
    /// call, in one transaction. Returns `(assistant, tool)`.
    fn save_tool_turn(
        &self,
        draft: AssistantDraft,
    ) -> impl Future<Output = Result<(AgentMessage, AgentMessage), AppError>> + Send;
    /// Replaces the pending result of a call in the tool message.
    fn save_tool_result(
        &self,
        tool_message_id: &str,
        result: pingdeck_llm::llm::hub::HubPart,
    ) -> impl Future<Output = Result<(), AppError>> + Send;
    /// Stores a compaction summary that replaces history through `compacted_through`.
    fn save_summary(
        &self,
        text: String,
        compacted_through: &str,
    ) -> impl Future<Output = Result<AgentMessage, AppError>> + Send;
    /// Stores the audit row of a call.
    fn record_call(&self, audit: CallAudit) -> impl Future<Output = ()> + Send;
    /// Adds one model call to the run and the chat; returns the run's totals. `cost` is in the
    /// currency of the model that answered; an unknown cost counts in no currency.
    /// `purpose` tells a step from a sub-agent's step and a summary: a summary is not a step.
    fn record_usage(
        &self,
        usage: &HubUsage,
        cost: &CallCost,
        context_tokens: u64,
        purpose: CallPurpose,
    ) -> impl Future<Output = Result<RunTotals, AppError>> + Send;
    /// What the loop needs to know about `served`, a model that answered instead of the run's
    /// (a fallback): read like the run's own model.
    fn model_info(&self, served: &ServedModel) -> impl Future<Output = ModelInfo> + Send;
    /// What the run has cost so far in `currency`, sub-agents included.
    fn run_cost_micro(&self, currency: &str) -> i64;
    /// Tokens (input and output) the run tree has used so far (`ai.agent.max_run_tokens`).
    fn run_tokens(&self) -> u64 {
        0
    }
    /// Cap passes of the run tree for the cap named `key`; `None` when the host keeps none.
    fn cap_passes(&self, _key: &str) -> Option<i64> {
        None
    }
    /// Records the cap passes of the run tree.
    fn set_cap_passes(&self, _key: &str, _passes: i64) {}

    /// Whether the tool named `name` may run concurrently with other parallel-safe calls.
    fn parallel_safe(&self, name: &str) -> bool;
    /// Classifies and checks a call: unknown tool, bad arguments, permission.
    fn plan_call(&self, call: &ToolCall) -> impl Future<Output = ToolPlan> + Send;
    /// Asks the user and waits for the answer; the host announces the request and the outcome.
    fn request_approval(
        &self,
        call: &ToolCall,
        ask: &AskInfo,
    ) -> impl Future<Output = ApprovalOutcome> + Send;
    /// Executes a call; the tool streams its own output and events through the host.
    fn run_tool(
        &self,
        call: &ToolCall,
        options: RunOptions,
    ) -> impl Future<Output = ToolOutput> + Send;
    /// Pauses at a cap and waits for the user; `None` when the run was cancelled.
    fn await_cost_decision(
        &self,
        pause: &CostPause<'_>,
    ) -> impl Future<Output = Option<CostDecision>> + Send;
    /// Seconds a run with no attached UI waits for an approval before the call ends timed out.
    fn approval_timeout_s(&self) -> u32 {
        crate::agent::limits::APPROVAL_TIMEOUT_S
    }
    /// Whether the user's steer message ended `call_id` (a `task` call whose sub-agent was
    /// stopped for it): the call is `superseded`, not an error. Consumes the mark.
    fn superseded(&self, _call_id: &str) -> bool {
        false
    }
    /// Re-resolves what a step boundary may change (a Folder chosen or cleared, Plan switched,
    /// a tool toggled); `None` when nothing did.
    fn step_config(&self) -> impl Future<Output = Option<StepConfig>> + Send {
        async { None }
    }
    /// The largest tool result the model may be given whole, in bytes.
    fn result_cap_bytes(&self) -> usize {
        usize::MAX
    }
    /// The run works in a folder the user has not trusted: what the file tools and the shell read
    /// from it reaches the model as external data (enveloped, hidden characters made visible).
    fn folder_untrusted(&self) -> bool {
        false
    }
    /// Keeps the redacted full output `text` of `call_id` on disk for `read_output` and "Show
    /// full output"; returns the bytes kept, `None` when nothing could be kept.
    fn spill_output(
        &self,
        _call_id: &str,
        _text: String,
    ) -> impl Future<Output = Option<u64>> + Send {
        async { None }
    }
    /// Notes the failure class of the model call that is about to end the run, so the run's
    /// `Error` event and row carry it.
    fn record_failure(&self, _failure: AgentFailure) {}
    /// What the chat remembers about its context (the pruning watermark, a learned window);
    /// kept in memory across runs by the host.
    fn context_state(&self) -> ContextState {
        ContextState::default()
    }
    /// Keeps `state` for the chat's next step and run.
    fn save_context_state(&self, _state: &ContextState) {}
    /// Whether a tool call of the last step asked the run to end once its results are stored (an
    /// approved or timed-out plan). Consumes the request.
    fn take_end_request(&self) -> bool {
        false
    }
}

/// What the loop needs to know about the model.
#[derive(Debug, Clone, Default)]
pub struct ModelInfo {
    /// Context window in tokens.
    pub context_limit: Option<u64>,
    /// What a call costs: known, free or unknown (C-$5); only a known price caps the run.
    pub price: PriceResolution,
    /// What the model can do.
    pub capabilities: ModelCapabilities,
}

impl ModelInfo {
    /// What one answered model call costs: known and free calls have an amount and a
    /// currency; a call without a price or without token counts has neither, never 0.
    pub(crate) fn call_cost(&self, usage: &HubUsage) -> CallCost {
        match book_cost(&self.price, CostUsage::from_hub(usage).as_ref(), true) {
            BookedCost::Known(cost) => CallCost {
                micro: Some(cost.micro),
                currency: Some(cost.currency),
            },
            BookedCost::Free => CallCost {
                micro: Some(0),
                currency: Some(FREE_CURRENCY.to_owned()),
            },
            BookedCost::Unknown(_) | BookedCost::NoUsage => CallCost::default(),
        }
    }
}

/// The currency a free call is booked in: zero is zero in any currency, but "0 without a
/// currency" is how the DTO says "unknown".
const FREE_CURRENCY: &str = "USD";

/// What one model call is booked at: no amount and no currency when it is unknown.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct CallCost {
    /// Millionths of `currency`.
    pub micro: Option<i64>,
    /// ISO 4217 code.
    pub currency: Option<String>,
}

impl CallCost {
    /// The amount for an `i64` field: unknown counts as 0 there, told apart by the missing
    /// currency.
    #[must_use]
    pub fn micro_or_zero(&self) -> i64 {
        self.micro.unwrap_or(0)
    }
}

/// When to summarise old history.
#[derive(Debug, Clone, Copy)]
pub struct CompactionPolicy {
    pub auto: bool,
    pub threshold_pct: u8,
    pub keep_turns: usize,
}

/// A run's cost cap in one currency: pause before a model call once the run costs this much in it.
#[derive(Debug, Clone)]
pub struct CostCap {
    pub micro: i64,
    pub currency: String,
}

/// The key of `ai.agent.max_run_tokens` among the caps.
pub const TOKENS_CAP: &str = "tokens";

/// A cap the run tree reached.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct CostPause<'a> {
    /// Spent so far: millionths of `currency`, or tokens.
    pub spent: i64,
    /// The cap in force.
    pub cap: i64,
    /// Where the next pause comes after a Continue.
    pub next_cap: i64,
    /// ISO 4217 code; `None` for `ai.agent.max_run_tokens` (the figures are tokens).
    pub currency: Option<&'a str>,
}

/// What a run needs to know besides its host.
pub struct RunInput {
    pub target: ChatTarget,
    pub model_label: String,
    pub system: Option<String>,
    pub tools: Vec<ToolSpec>,
    /// The provider searches the web itself (`web_search` is not registered).
    pub native_web_search: bool,
    pub reasoning: Option<ReasoningEffort>,
    pub max_iterations: u32,
    pub model: ModelInfo,
    pub compaction: CompactionPolicy,
    /// One cap per currency that has one on; spend in each currency is checked on its own.
    pub cost_caps: Vec<CostCap>,
    /// The last of `max_iterations` (2 or more) steps forbids tool calls and asks for a summary
    /// (`ai.agent.wrap_up_last_step`).
    pub wrap_up_last_step: bool,
    /// The prompt-cache key (the chat id) when `ai.agent.prompt_cache` is not `off`.
    pub prompt_cache: Option<String>,
    /// Pause when the run tree has used this many tokens (`ai.agent.max_run_tokens`).
    pub max_run_tokens: Option<u64>,
}

/// What the next model call is built from and may change between two steps: the system prompt
/// and the tool list ([`RunHost::step_config`]).
#[derive(Debug, Clone)]
pub struct StepConfig {
    pub system: Option<String>,
    pub tools: Vec<ToolSpec>,
    /// The provider searches the web itself.
    pub native_web_search: bool,
}

/// Why a model call failed: the error and what the stream had said by then.
pub struct TurnFailure {
    pub error: AppError,
    /// Text and reasoning received before the failure.
    pub partial: Vec<pingdeck_llm::llm::hub::HubPart>,
    /// Any answer content had arrived (a retry would repeat it).
    pub content_started: bool,
    /// The typed kind of the client's error; a stall the loop detected itself is `IdleStall`.
    pub chat_kind: Option<ChatErrorKind>,
    /// The provider failure category of the last attempt, when the client reported one.
    pub provider_failure: Option<FailureKind>,
    /// Every attempt of the client's walk over targets; empty for failures that ended no walk.
    pub attempts: Vec<pingdeck_llm::llm::client::AttemptSummary>,
    /// HTTP status of the client's last attempt.
    pub status: Option<u16>,
    /// The upstream's `Retry-After`, in milliseconds.
    pub retry_after_ms: Option<u64>,
    /// The client already retried the last attempt in place.
    pub retried: bool,
    /// Every attempt of the walk failed transiently.
    pub transient: bool,
}

/// A finished model call.
pub struct TurnOutcome {
    pub content: Vec<pingdeck_llm::llm::hub::HubPart>,
    pub usage: HubUsage,
    /// Why the model stopped.
    pub finish: HubFinish,
    /// Milliseconds from sending the request to the first answer content.
    pub ttft_ms: Option<u64>,
    /// Milliseconds from the first answer content to the end of the stream.
    pub stream_ms: Option<u64>,
    /// The provider model that answered, when the backend said.
    pub served: Option<ServedModel>,
}

/// Whether `served` is the model `target` names itself (a `provider_id/model` reference or a
/// pinned model); a name-form or legacy reference does not match and is looked up instead.
fn is_primary(target: &ChatTarget, served: &ServedModel) -> bool {
    match target {
        ChatTarget::Pinned {
            provider_id, model, ..
        } => *provider_id == served.provider_id && *model == served.model,
        ChatTarget::Named { name, .. } => {
            name.trim()
                .split_once('/')
                .is_some_and(|(provider, model)| {
                    provider == served.provider_id && model == served.model
                })
        }
    }
}

/// The model a call is priced with when it is not the run's own: the facts of `served`, `None`
/// when the run's model answered (or the backend did not say).
pub(crate) async fn fallback_info<H: RunHost>(
    host: &H,
    input: &RunInput,
    served: Option<&ServedModel>,
) -> Option<ModelInfo> {
    let served = served.filter(|served| !is_primary(&input.target, served))?;
    Some(host.model_info(served).await)
}

/// Adds `more` to `total`, figure by figure.
pub fn add_usage(total: &mut HubUsage, more: &HubUsage) {
    let add = |total: &mut Option<u64>, extra: Option<u64>| {
        if let Some(extra) = extra {
            *total = Some(total.unwrap_or(0).saturating_add(extra));
        }
    };
    add(&mut total.input_tokens, more.input_tokens);
    add(&mut total.output_tokens, more.output_tokens);
    add(&mut total.cached_input_tokens, more.cached_input_tokens);
    add(&mut total.cache_write_tokens, more.cache_write_tokens);
    add(
        &mut total.cache_write_long_tokens,
        more.cache_write_long_tokens,
    );
    add(&mut total.reasoning_tokens, more.reasoning_tokens);
}

/// The result text of a call that ended without running.
pub fn unrun_text(status: crate::agent::model::ToolCallStatus) -> &'static str {
    use crate::agent::model::ToolCallStatus;
    match status {
        ToolCallStatus::Superseded => {
            "Not run: the user sent a new message first. Read it and decide again."
        }
        ToolCallStatus::Cancelled => "Cancelled: the run was stopped before this tool ran.",
        ToolCallStatus::Expired => "Not run: the run stopped while this call waited for approval.",
        _ => "Not run.",
    }
}

/// What the model reads when nobody answered an approval within `timeout_s` seconds.
/// The call ends `approval-timed-out`, never as a denial, and the run goes on.
#[must_use]
pub fn approval_timed_out_text(timeout_s: u32) -> String {
    let minutes = timeout_s.div_ceil(60).max(1);
    format!(
        "Not run: no one answered within {minutes} min. Do not retry the same call; ask the user."
    )
}

/// Whether `id` is the `call_<n>` placeholder the Gemini and OpenAI-compatible adapters invent
/// for a call the provider left unnamed; it repeats every turn, also on sibling branches.
fn is_synthesized_call_id(id: &str) -> bool {
    id.strip_prefix("call_")
        .is_some_and(|rest| !rest.is_empty() && rest.bytes().all(|byte| byte.is_ascii_digit()))
}

/// Hands out the tool-call ids of one assistant message, unique across the whole chat (they key
/// the audit rows and the spilled outputs). A provider id is kept when it is new; an empty,
/// placeholder, repeated or already-used id becomes `<message_id>-<n>`. The stream applies it as
/// each call starts, so the events, the stored parts and the results all carry the same id.
#[derive(Clone)]
pub struct CallIds {
    message_id: String,
    /// Ids the chat's history already holds.
    taken: std::collections::HashSet<String>,
    /// Ids handed out for this message.
    seen: std::collections::HashSet<String>,
    next: usize,
}

impl CallIds {
    pub(crate) fn new(message_id: &str, history: &[HistoryEntry]) -> Self {
        let taken = history
            .iter()
            .flat_map(|entry| &entry.message.content)
            .filter_map(|part| match part {
                pingdeck_llm::llm::hub::HubPart::ToolCall { id, .. } => Some(id.clone()),
                _ => None,
            })
            .collect();
        Self {
            message_id: message_id.to_owned(),
            taken,
            seen: std::collections::HashSet::new(),
            next: 0,
        }
    }

    /// The id the call that the provider named `provider_id` goes by.
    pub(crate) fn assign(&mut self, provider_id: &str) -> String {
        let keep = !provider_id.is_empty()
            && !is_synthesized_call_id(provider_id)
            && !self.taken.contains(provider_id);
        if keep && self.seen.insert(provider_id.to_owned()) {
            return provider_id.to_owned();
        }
        loop {
            self.next += 1;
            let candidate = format!("{}-{}", self.message_id, self.next);
            if self.seen.insert(candidate.clone()) {
                return candidate;
            }
        }
    }
}

/// The calls of an assistant message, in order.
fn tool_calls(parts: &[pingdeck_llm::llm::hub::HubPart]) -> Vec<ToolCall> {
    parts
        .iter()
        .filter_map(|part| match part {
            pingdeck_llm::llm::hub::HubPart::ToolCall {
                id,
                name,
                arguments,
                ..
            } => Some(ToolCall {
                id: id.clone(),
                name: name.clone(),
                arguments: arguments.clone(),
            }),
            _ => None,
        })
        .collect()
}

struct Loop<'a, B, H> {
    backend: &'a B,
    host: &'a H,
    input: &'a RunInput,
    cancel: &'a CancelToken,
    iterations: u32,
    usage: HubUsage,
    last_message_id: String,
    /// The provider-reported prompt size of the latest call (0 right after a compaction).
    last_context_tokens: u64,
    /// Cap pauses the user continued through, by cap (a currency, or [`TOKENS_CAP`]), for a host
    /// that does not keep them for the run tree.
    cap_passes: Vec<(String, i64)>,
    /// The system prompt and tools of the next call (re-resolved at each step boundary).
    config: StepConfig,
    /// A context-length error asked for the current turn's tool results to be shortened.
    shrink: bool,
    /// The latest answer was cut off by the model's output token cap.
    output_limited: bool,
    /// The provider refused `tool_choice: none` on the last step: it goes without tools instead.
    tool_choice_refused: bool,
    /// Repeats, failures and denials so far.
    probe: std::sync::Mutex<progress::ProgressProbe>,
    /// The chat's pruning watermark and learned window.
    context: ContextState,
    /// Proactive compactions that failed in this run.
    compaction_failures: u32,
    /// The request stayed too long after shortening and compaction.
    context_full: bool,
}

/// Runs the loop until the model stops asking for tools, `max_iterations` model calls were made,
/// the user stops it at the cost cap, the run is cancelled or a call fails.
///
/// # Errors
/// [`AppError::Cancelled`] when `cancel` fires; the model's or the store's error otherwise.
pub async fn run_agent<B: ChatBackend, H: RunHost>(
    backend: &B,
    host: &H,
    input: &RunInput,
    cancel: &CancelToken,
) -> Result<AgentRunResult, AppError> {
    let mut run = Loop {
        backend,
        host,
        input,
        cancel,
        iterations: 0,
        usage: HubUsage::default(),
        last_message_id: String::new(),
        last_context_tokens: 0,
        cap_passes: Vec::new(),
        config: StepConfig {
            system: input.system.clone(),
            tools: input.tools.clone(),
            native_web_search: input.native_web_search,
        },
        shrink: false,
        output_limited: false,
        tool_choice_refused: false,
        probe: std::sync::Mutex::default(),
        context: host.context_state(),
        compaction_failures: 0,
        context_full: false,
    };
    run.run().await
}

impl<B: ChatBackend, H: RunHost> Loop<'_, B, H> {
    fn result(&self, stop_reason: AgentStopReason) -> AgentRunResult {
        AgentRunResult {
            message_id: self.last_message_id.clone(),
            stop_reason,
            iterations: self.iterations,
            usage: self.usage.clone(),
        }
    }

    fn check_cancelled(&self) -> Result<(), AppError> {
        if self.cancel.is_cancelled() {
            Err(AppError::cancelled())
        } else {
            Ok(())
        }
    }

    async fn run(&mut self) -> Result<AgentRunResult, AppError> {
        let max = self.input.max_iterations;
        for index in 0..max {
            self.check_cancelled()?;
            self.host.drain_steer().await?;
            self.host.drain_process_exits().await?;
            if self.cost_cap_stops().await? {
                return Ok(self.result(AgentStopReason::CostLimit));
            }
            // A steer sent while the run waited at the cost cap goes with the next call.
            self.host.drain_steer().await?;
            let wrap_up = self.input.wrap_up_last_step && max >= 2 && index + 1 == max;
            if self.step(wrap_up).await? {
                let reason = if self.context_full {
                    AgentStopReason::ContextFull
                } else if self.stuck() {
                    AgentStopReason::NoProgress
                } else if self.output_limited {
                    AgentStopReason::OutputLimit
                } else if wrap_up {
                    AgentStopReason::MaxIterations
                } else {
                    AgentStopReason::Completed
                };
                return Ok(self.result(reason));
            }
            if self.host.take_end_request() {
                return Ok(self.result(AgentStopReason::Completed));
            }
        }
        // A steer that arrived during the last step must not be lost.
        self.host.drain_steer().await?;
        Ok(self.result(AgentStopReason::MaxIterations))
    }

    /// Pauses at the first cap the run tree has reached (a cost cap per currency, then
    /// `max_run_tokens`); `true` when the user chose to stop. A sub-agent shares its root's
    /// passes, so a cap the user continued through does not stop the child at once.
    async fn cost_cap_stops(&mut self) -> Result<bool, AppError> {
        let tokens = self.input.max_run_tokens.map(|cap| {
            let used = i64::try_from(self.host.run_tokens()).unwrap_or(i64::MAX);
            (None, i64::try_from(cap).unwrap_or(i64::MAX), used)
        });
        let caps: Vec<(Option<String>, i64, i64)> = self
            .input
            .cost_caps
            .iter()
            .map(|cap| {
                let spent = self.host.run_cost_micro(&cap.currency);
                (Some(cap.currency.clone()), cap.micro, spent)
            })
            .chain(tokens)
            .collect();
        for (currency, cap, spent) in caps {
            let key = currency.as_deref().unwrap_or(TOKENS_CAP);
            let local = self
                .cap_passes
                .iter()
                .find(|(k, _)| k == key)
                .map_or(0, |(_, passes)| *passes);
            let passes = self.host.cap_passes(key).unwrap_or(0).max(local);
            let limit = cap.saturating_mul(passes + 1);
            if spent < limit {
                continue;
            }
            let reached = spent / cap.max(1);
            let pause = CostPause {
                spent,
                cap: limit,
                next_cap: cap.saturating_mul(reached + 1),
                currency: currency.as_deref(),
            };
            match self.host.await_cost_decision(&pause).await {
                Some(CostDecision::Continue) => {
                    // The next pause comes when the run has cost another cap, from here on.
                    self.cap_passes.retain(|(k, _)| k != key);
                    self.cap_passes.push((key.to_owned(), reached));
                    self.host.set_cap_passes(key, reached);
                }
                Some(CostDecision::Stop) => return Ok(true),
                None => return Err(AppError::cancelled()),
            }
        }
        Ok(false)
    }

    /// One model call and its tool calls; `true` when the run is complete. On the `wrap_up` step
    /// the model may not call tools.
    async fn step(&mut self, wrap_up: bool) -> Result<bool, AppError> {
        if let Some(config) = self.host.step_config().await {
            self.config = config;
        }
        let mut entries = self.host.load_history().await?;
        // The estimate is of the request as it will be sent (attachments resolved, results
        // pruned), corrected upward by the provider's last reading.
        let mut request = self.build_request(&entries, wrap_up).await;
        let mut used = self.used_tokens(&request);
        if self.advance_watermark(&entries, used).await {
            self.last_context_tokens = 0;
            request = self.build_request(&entries, wrap_up).await;
            used = self.used_tokens(&request);
        }
        let mut pending = Some(request);
        if self.input.compaction.auto
            && self.compaction_failures < COMPACTION_ATTEMPTS
            && super::context::should_compact(
                used,
                Some(self.window()),
                self.input.compaction.threshold_pct,
            )
        {
            match compaction::compact(self.backend, self.host, self.input, &entries, self.cancel)
                .await
            {
                Ok(Some(_)) => {
                    self.last_context_tokens = 0;
                    entries = self.host.load_history().await?;
                    pending = None;
                }
                Ok(None) => {}
                Err(error) if error.is_cancelled() => return Err(error),
                Err(_) => {
                    // The request goes out in full; two failures end the attempts.
                    self.compaction_failures += 1;
                    self.compaction_failed();
                }
            }
        }

        let message_id = crate::agent::new_entity_id("am");
        let mut compacted_reactively = false;
        if wrap_up {
            self.host.emit(AgentEvent::coded_notice(
                "step_limit_summary",
                "Last step: the agent summarises instead of calling tools.",
                &[("steps", self.input.max_iterations.to_string())],
            ));
        }
        let turn = loop {
            let request = match pending.take() {
                Some(request) => request,
                None => self.build_request(&entries, wrap_up).await,
            };
            match stream::stream_turn(
                self.backend,
                self.host,
                self.input,
                request,
                &message_id,
                &CallIds::new(&message_id, &entries),
                self.cancel,
            )
            .await
            {
                Ok(turn) => break turn,
                Err(failure) => {
                    if wrap_up
                        && !self.tool_choice_refused
                        && !failure.content_started
                        && failure.provider_failure == Some(FailureKind::BadRequest)
                        && !self.cancel.is_cancelled()
                    {
                        // The provider refused `tool_choice: none`: the last step goes without
                        // the tool list instead, once (the failed call is in the call log).
                        self.tool_choice_refused = true;
                        continue;
                    }
                    let too_long = !failure.content_started
                        && super::context::is_context_length_error(&failure.error)
                        && !self.cancel.is_cancelled();
                    if too_long && !compacted_reactively {
                        // First shorten the current turn's largest tool results, then
                        // summarise, then retry once; a failed summary still leaves
                        // the shortened retry.
                        compacted_reactively = true;
                        self.learn_window(failure.error.message());
                        let raw: Vec<HubMessage> =
                            entries.iter().map(|e| e.message.clone()).collect();
                        let shortenable = shorten_scoped_results(
                            &mut raw.clone(),
                            self.shrink_budget(&raw),
                            ResultScope::UserTurn,
                        ) > 0;
                        self.shrink = true;
                        let compacted = match compaction::compact(
                            self.backend,
                            self.host,
                            self.input,
                            &entries,
                            self.cancel,
                        )
                        .await
                        {
                            Ok(compacted) => compacted.is_some(),
                            Err(error) if error.is_cancelled() => return Err(error),
                            Err(_) => {
                                self.compaction_failed();
                                false
                            }
                        };
                        if compacted {
                            self.last_context_tokens = 0;
                            entries = self.host.load_history().await?;
                        }
                        if compacted || shortenable {
                            continue;
                        }
                    }
                    if too_long {
                        // Still too long after everything: the run stops with a status line,
                        // not as a failure.
                        self.host.emit(AgentEvent::coded_notice(
                            "context_full",
                            "The conversation no longer fits the model's context window.",
                            &[],
                        ));
                        self.context_full = true;
                        return Ok(true);
                    }
                    let class = super::failure::classify_failure(
                        failure.chat_kind,
                        failure.provider_failure,
                        &failure.error,
                        &self.input.model_label,
                        &failure.attempts,
                    );
                    if let Some(class) = &class {
                        self.host.record_failure(class.clone());
                    }
                    self.save_failed_turn(&message_id, &failure, class).await;
                    return Err(failure.error);
                }
            }
        };

        self.iterations += 1;
        let context_tokens = turn.usage.input_tokens.unwrap_or_else(|| {
            let raw: Vec<HubMessage> = entries.iter().map(|e| e.message.clone()).collect();
            estimate_context_tokens(self.config.system.as_deref(), &raw, &self.config.tools)
        });
        self.last_context_tokens = context_tokens;
        // A fallback that answered is priced with its own price, in its own currency.
        let fallback = fallback_info(self.host, self.input, turn.served.as_ref()).await;
        let priced = fallback.as_ref().unwrap_or(&self.input.model);
        let call_cost = priced.call_cost(&turn.usage);
        let totals = self
            .host
            .record_usage(
                &turn.usage,
                &call_cost,
                context_tokens,
                self.host.run_key().1,
            )
            .await?;
        self.usage = totals.usage.clone();
        self.host.emit(AgentEvent::Usage {
            iteration_usage: turn.usage.clone(),
            run_usage: totals.usage,
            cost_micro_usd: totals.cost_micro,
            context_used_tokens: context_tokens,
            context_limit_tokens: self.input.model.context_limit,
            currency: call_cost.currency.clone(),
            ttft_ms: turn.ttft_ms,
            stream_ms: turn.stream_ms,
        });

        let parts = turn.content;
        let calls = tool_calls(&parts);
        // A call whose arguments do not parse at the output cap was cut off mid-write.
        let truncated = turn.finish == HubFinish::Length
            && calls.last().is_some_and(|call| {
                serde_json::from_str::<serde_json::Value>(&call.arguments).is_err()
            });
        let output_limited = (calls.is_empty() && turn.finish == HubFinish::Length) || truncated;
        let draft = AssistantDraft {
            id: message_id.clone(),
            parts,
            usage: Some(turn.usage),
            model: self.input.model_label.clone(),
            error: None,
            cost: call_cost,
            served_model: fallback
                .is_some()
                .then(|| turn.served.as_ref().map(ServedModel::id_form))
                .flatten(),
            reasoning_effort: self.sent_effort(),
            ttft_ms: turn.ttft_ms,
            stream_ms: turn.stream_ms,
            error_kind: None,
            failure: None,
        };
        self.output_limited = output_limited;
        self.last_message_id.clone_from(&message_id);
        if calls.is_empty() {
            let saved = self.host.save_assistant(draft).await?;
            self.host.emit(AgentEvent::MessageSaved {
                message: Box::new(saved),
            });
            // A steer that came in while the model answered gets its own step.
            return Ok(!self.host.steer_pending());
        }
        let (assistant, tool_message) = self.host.save_tool_turn(draft).await?;
        let tool_message_id = tool_message.id.clone();
        self.host.emit(AgentEvent::MessageSaved {
            message: Box::new(assistant),
        });
        self.host.emit(AgentEvent::MessageSaved {
            message: Box::new(tool_message),
        });
        let run = tools::CallRun {
            host: self.host,
            cancel: self.cancel,
            assistant_id: &message_id,
            tool_message_id: &tool_message_id,
            probe: &self.probe,
            learned_cap: self.learned_result_cap(),
        };
        if wrap_up {
            tools::refuse_calls(&run, &calls, ToolCallStatus::Cancelled, STEP_LIMIT_TEXT).await?;
            return Ok(false);
        }
        let mut calls = calls;
        if truncated && let Some(cut) = calls.pop() {
            tools::refuse_calls(
                &run,
                std::slice::from_ref(&cut),
                ToolCallStatus::Error,
                TRUNCATED_CALL_TEXT,
            )
            .await?;
        }
        if tools::execute_calls(&run, calls).await? {
            return Err(AppError::cancelled());
        }
        Ok(truncated || self.stuck())
    }

    /// The model went on repeating itself past the nudge (second stage).
    fn stuck(&self) -> bool {
        self.probe
            .lock()
            .unwrap_or_else(std::sync::PoisonError::into_inner)
            .stuck
    }

    /// The effort a call carries, as recorded on its message: none for a model without a reasoning control.
    fn sent_effort(&self) -> Option<ReasoningEffort> {
        self.input
            .reasoning
            .filter(|_| self.input.model.capabilities.reasoning.supported)
    }

    /// Keeps what the model said before a failure (text and reasoning only: an unfinished tool
    /// call would make the stored history unsendable) as a finished message with the error.
    async fn save_failed_turn(
        &self,
        message_id: &str,
        failure: &TurnFailure,
        class: Option<AgentFailure>,
    ) {
        if failure.partial.is_empty() {
            return;
        }
        let draft = AssistantDraft {
            id: message_id.to_owned(),
            parts: failure.partial.clone(),
            usage: None,
            model: self.input.model_label.clone(),
            error: Some(failure.error.message().to_owned()),
            cost: CallCost::default(),
            served_model: None,
            reasoning_effort: self.sent_effort(),
            ttft_ms: None,
            stream_ms: None,
            error_kind: Some(error_kind_name(&failure.error)),
            failure: class,
        };
        if let Ok(saved) = self.host.save_assistant(draft).await {
            self.host.emit(AgentEvent::MessageSaved {
                message: Box::new(saved),
            });
        }
    }

    /// The request for the next model call: the history made provider-valid, with attachments
    /// resolved and old tool results shrunk, and the run's tools. On the `wrap_up` step the tools
    /// stay defined (the prefix stays the same) but may not be called, and a closing prompt that is
    /// never stored follows the history.
    async fn build_request(&self, entries: &[HistoryEntry], wrap_up: bool) -> HubRequest {
        let mut messages: Vec<HubMessage> = entries.iter().map(|e| e.message.clone()).collect();
        // Below the watermark the same results are pruned every step, so the prefix the
        // provider caches stays byte-identical (R3).
        prune_calls(&mut messages, &self.context);
        repair_orphan_tool_calls(&mut messages);
        let mut messages = self.host.resolve_attachments(messages).await;
        // After a context-length error every result of the current user turn shares one smaller
        // budget; otherwise the newest step's results share a quarter of the window.
        if self.shrink {
            let budget = self.shrink_budget(&messages);
            shorten_scoped_results(&mut messages, budget, ResultScope::UserTurn);
        } else {
            shorten_turn_results(&mut messages, self.turn_budget());
        }
        let mut request = HubRequest::user_prompt(self.input.model_label.clone(), "");
        request.messages = messages;
        request.system = self.config.system.clone();
        request.tools = self
            .config
            .tools
            .iter()
            .map(|tool| HubTool {
                name: tool.name.clone(),
                description: Some(tool.description.clone()),
                parameters: tool.input_schema.clone(),
                strict: None,
            })
            .collect();
        if self.config.native_web_search {
            request.native_tools = vec![HubNativeTool::WebSearch {
                max_uses: None,
                allowed_domains: Vec::new(),
                blocked_domains: Vec::new(),
            }];
            if lower_native_tools(&mut request, &self.input.model.capabilities) {
                self.host.emit(AgentEvent::coded_notice(
                    "native_search_lowered",
                    "This model cannot search the web itself; using the built-in search.",
                    &[],
                ));
            }
        }
        if wrap_up {
            if self.tool_choice_refused {
                request.tools.clear();
                request.native_tools.clear();
            } else {
                request.tool_choice = Some(HubToolChoice::None);
            }
            request.messages.push(HubMessage::user(WRAP_UP_PROMPT));
        }
        request.reasoning = self.input.reasoning.map(|effort| HubReasoning {
            effort: Some(effort.into()),
            ..HubReasoning::default()
        });
        request.cache = self
            .input
            .prompt_cache
            .as_ref()
            .filter(|_| caches(&self.input.model.capabilities))
            .map(|key| CacheHint {
                breakpoints: vec![
                    CacheBreakpoint::SystemAndTools,
                    CacheBreakpoint::LastMessage,
                ],
                key: Some(key.clone()),
            });
        request.stream = true;
        request
    }

    /// The window the run judges by: one a context-length error named, else the model's, else
    /// [`UNKNOWN_WINDOW_TOKENS`](super::limits::UNKNOWN_WINDOW_TOKENS).
    fn window(&self) -> u64 {
        self.context
            .learned_window
            .or(self.input.model.context_limit)
            .unwrap_or(super::limits::UNKNOWN_WINDOW_TOKENS)
    }

    /// Estimated tokens of `request`, never below the provider's last reading.
    fn used_tokens(&self, request: &HubRequest) -> u64 {
        pingdeck_model::tokens::request_tokens(request).max(self.last_context_tokens)
    }

    /// Moves the pruning watermark when the context is half full and that frees enough; the
    /// newly pruned full outputs are kept on disk first so their markers name a handle.
    async fn advance_watermark(&mut self, entries: &[HistoryEntry], used: u64) -> bool {
        let history: Vec<HubMessage> = entries.iter().map(|e| e.message.clone()).collect();
        let current = self
            .context
            .prune_through
            .as_ref()
            .and_then(|id| entries.iter().position(|entry| entry.id == *id));
        let Some((through, calls)) =
            advance_watermark(&history, current, &self.context.pruned, used, self.window())
        else {
            return false;
        };
        for call in calls {
            if let Some(text) = full_result_text(&history, &call)
                && self.host.spill_output(&call, text).await.is_some()
            {
                self.context.kept.insert(call.clone());
            }
            self.context.pruned.insert(call);
        }
        self.context.prune_through = entries.get(through).map(|entry| entry.id.clone());
        self.host.save_context_state(&self.context);
        true
    }

    /// Remembers the window a context-length error names, for the rest of the chat's session.
    fn learn_window(&mut self, message: &str) {
        let Some(window) = window_from_error(message) else {
            return;
        };
        if self.window() == window {
            return;
        }
        self.context.learned_window = Some(window);
        self.host.save_context_state(&self.context);
        self.host.emit(AgentEvent::coded_notice(
            "context_window_learned",
            format!("The provider says this model reads at most {window} tokens."),
            &[("tokens", window.to_string())],
        ));
    }

    fn compaction_failed(&self) {
        self.host.emit(AgentEvent::coded_notice(
            "compaction_failed",
            "Couldn't summarise the earlier conversation; the request goes out in full.",
            &[],
        ));
    }

    /// The most bytes of one assistant turn's tool results the request may carry: a quarter of
    /// the run's [`window`](Self::window) at 4 bytes a token.
    fn turn_budget(&self) -> usize {
        turn_cap_bytes(Some(self.window()))
    }

    /// The budget after a context-length error: half of what the current user turn's results take
    /// now, at most the step budget.
    fn shrink_budget(&self, messages: &[HubMessage]) -> usize {
        (scoped_result_bytes(messages, ResultScope::UserTurn) / 2).min(self.turn_budget())
    }

    /// The cap a window learned from a context-length error puts on one tool result; `None`
    /// while the run judges by the model's own window (the host's cap already follows it).
    fn learned_result_cap(&self) -> Option<usize> {
        self.context
            .learned_window
            .map(|window| result_cap_bytes(Some(window)))
    }
}

/// The hub message of a stored message: a summary reads as a user message, so the model treats it
/// as context rather than something it said.
#[must_use]
pub fn hub_role(role: super::model::AgentRole) -> HubRole {
    use super::model::AgentRole;
    match role {
        AgentRole::User | AgentRole::Summary => HubRole::User,
        AgentRole::Assistant => HubRole::Assistant,
        AgentRole::Tool => HubRole::Tool,
    }
}
