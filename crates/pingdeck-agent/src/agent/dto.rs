//! Wire shapes of the run lifecycle commands: what starts a run, what a start returns, what a
//! subscription follows and how the run list is filtered.
//!
//! Single responsibility: plain serde data shared by `app::agent_service::run` and the shell. The
//! events a run emits are [`super::model::AgentEvent`]; the global attention snapshot is
//! [`super::attention::AgentAttention`].

use serde::{Deserialize, Serialize};

use super::attention::PendingApprovalSummary;
use super::model::{AgentRunSummary, CurrencyTotal};

/// What the user sends to start a run: a turn in an existing chat.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentRunInput {
    /// The chat.
    pub session_id: String,
    /// The message text; may be empty when attachments are given.
    pub text: String,
    /// Attachments to send with it (ids from `agent_attachment_put_*`).
    pub attachment_ids: Vec<String>,
    /// Skills the user attached to this message by name.
    pub skills: Vec<String>,
    /// The PingDeck workspace that is active in the UI now. The chat binds to it with its first
    /// message and stays there; later values are ignored.
    pub workspace_id: Option<String>,
    /// The environment that is active in that workspace now; it follows the UI from message to
    /// message.
    pub environment_id: Option<String>,
    /// PingDeck objects the message mentions or hands over ("Ask agent"); the core renders each
    /// into one redacted, tagged text part.
    pub contexts: Vec<ContextRef>,
    /// Which byte budget `contexts` use.
    pub context_use: ContextUse,
}

/// What `agent_run_start` returns at once, before the run has done anything.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentRunStarted {
    /// The new run.
    pub run_id: String,
    /// Its chat.
    pub session_id: String,
    /// The stored user message the run answers.
    pub user_message_id: String,
}

/// What a subscription follows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentSubscribeTarget {
    /// One run: active, or finished within the last 10 minutes.
    Run {
        /// The run.
        run_id: String,
    },
    /// A chat: its current (or latest) run now, and every later run as it starts.
    Session {
        /// The chat.
        session_id: String,
    },
}

/// Narrows the run list.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentRunListFilter {
    /// Only this chat's runs.
    pub session_id: Option<String>,
    /// Only runs that have not ended.
    pub active_only: bool,
    /// At most this many (default 500, at most 5,000), newest first.
    pub limit: Option<u32>,
}

/// Why a run in the Needs you lane waits.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BoardWait {
    /// A tool call waits for the user's answer (of this run or of one of its sub-agents).
    Approval,
    /// The run reached its cost cap: "Cost limit reached. Continue or stop?".
    CostLimit,
}

/// File changes a run left pending.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardChanges {
    /// Distinct files.
    pub files: u32,
    /// Lines added.
    pub added: u32,
    /// Lines removed.
    pub removed: u32,
}

/// Lifecycle breakdown of sub-agent runs spawned by one parent run.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardSubagentCounts {
    /// Total sub-agent runs spawned by this parent run.
    pub total: u32,
    /// Sub-agents currently queued or running.
    pub active: u32,
    /// Sub-agents paused waiting for approval.
    pub waiting: u32,
    /// Sub-agents finished cleanly.
    pub completed: u32,
    /// Sub-agents that failed or were interrupted.
    pub failed: u32,
    /// Sub-agents cancelled by the user or parent steer/stop.
    pub cancelled: u32,
}

/// One card of the Agent board: a top-level run and everything the card shows.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentBoardRow {
    /// The run: status, model label, start time, last step, cost, usage.
    pub run: AgentRunSummary,
    /// The chat's title.
    pub title: String,
    /// The chat's Folder.
    pub folder: Option<String>,
    /// Elapsed run time without machine sleep, in milliseconds.
    pub awake_ms: u64,
    /// The run is queued: "Waiting for a free slot".
    pub waiting_for_slot: bool,
    /// Why the run waits for the user, when it does.
    pub wait: Option<BoardWait>,
    /// The oldest approval waiting in the run or its sub-agents: tool, risk, one-line summary and,
    /// for a Low-risk call, the whole text.
    pub pending_approval: Option<PendingApprovalSummary>,
    /// The card may offer an inline Allow: the approval is Low risk and carries its whole text.
    pub inline_allow: bool,
    /// Plan items done.
    pub plan_done: u32,
    /// Plan items in total (dropped ones not counted).
    pub plan_total: u32,
    /// Currency of `run.cost_micro_usd` (`USD`, `CNY`); `None` while no priced call was made.
    pub cost_currency: Option<String>,
    /// Sub-agents the run started.
    pub subagents: u32,
    /// Per-status breakdown of sub-agents spawned by this run.
    #[serde(default)]
    pub subagent_counts: BoardSubagentCounts,
    /// File changes still awaiting Keep or Undo.
    pub pending_changes: BoardChanges,
    /// First line of the run's last answer.
    pub last_answer: Option<String>,
    /// A tool approval of the run ended "Timed out, not run".
    pub approval_timed_out: bool,
    /// The user has opened the run since it ended (always `true` while it is active).
    pub seen: bool,
    /// The run ended in any status and nobody has opened it: the card says "Review".
    pub review: bool,
}

/// The numbers the board shows next to its lanes.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BoardLimits {
    /// Concurrent top-level runs.
    pub max_concurrent_runs: u8,
    /// Sub-agents per run.
    pub max_subagents: u8,
    /// Steps per run.
    pub max_steps: u32,
    /// Seconds an unattended run waits for an approval.
    pub approval_timeout_s: u32,
    /// Messages a chat's queue holds.
    pub queue_messages: u32,
    /// Context fill, in percent, at which the core compacts on its own; absent while automatic
    /// compaction is off.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub auto_compact_pct: Option<u8>,
}

/// Tokens the agent's model calls used today (the usage rollup, source `agent`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TokensToday {
    /// Prompt tokens.
    pub input: u64,
    /// Generated tokens.
    pub output: u64,
}

/// The Agent board: three lanes, the limits and today's cost per currency.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentBoard {
    /// Queued and running runs, newest first.
    pub running: Vec<AgentBoardRow>,
    /// Runs waiting for the user, oldest first, then ended runs nobody has opened, newest first.
    pub needs_you: Vec<AgentBoardRow>,
    /// The 20 most recent opened runs that ended in the last 24 hours.
    pub done: Vec<AgentBoardRow>,
    /// Run limits in force.
    pub limits: BoardLimits,
    /// Cost of today's runs, per currency (never converted).
    pub cost_today: Vec<CurrencyTotal>,
    /// Tokens used today by every chat (local midnight).
    pub tokens_today: TokensToday,
    /// Background shell processes still running, over all chats.
    pub running_processes: u32,
    /// Active sub-agent runs across all chats right now.
    #[serde(default)]
    pub active_subagents: u32,
}

/// One tool execution step inside a parent run or child sub-agent run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopologyToolStep {
    pub call_id: String,
    pub name: String,
    pub class: String,
    pub summary: String,
    /// Normalized step state (`running`, `waiting_approval`, `ok`, `error`, `denied`, `cancelled`,
    /// `timed_out`, `superseded`).
    pub status: String,
    pub started_ms: Option<i64>,
    pub ended_ms: Option<i64>,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub parallel_safe: bool,
}

/// One sub-agent run spawned by a parent `task` tool call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct TopologySubagent {
    pub run: AgentRunSummary,
    pub parent_call_id: String,
    pub child_session_id: String,
    /// Task brief (`brief.description`, stored as the child session title).
    pub description: String,
    /// `explore` or `general`.
    pub subagent_kind: String,
    /// Whether this call resumed an earlier child session (`args.resume`).
    pub resumed: bool,
    pub awake_ms: u64,
    pub pending_approval: Option<PendingApprovalSummary>,
    /// Concise first line of the child's final report when available.
    pub last_answer: Option<String>,
    /// Tool steps executed inside this child sub-agent.
    pub tool_calls: Vec<TopologyToolStep>,
}

/// One wave in a run's execution topology.
///
/// Waves are ordered serially top-to-bottom along the parent run; branches inside a parallel wave
/// ran concurrently (fan-out -> join).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ExecutionWave {
    /// Two or more `task` sub-agents dispatched concurrently in the same turn.
    ParallelSubagents {
        wave_index: u32,
        started_ms: Option<i64>,
        ended_ms: Option<i64>,
        subagents: Vec<TopologySubagent>,
    },
    /// One `task` sub-agent dispatched serially in its turn.
    SingleSubagent {
        wave_index: u32,
        started_ms: Option<i64>,
        ended_ms: Option<i64>,
        subagent: Box<TopologySubagent>,
    },
    /// Two or more parallel-safe non-`task` tools dispatched concurrently in the same turn.
    ParallelTools {
        wave_index: u32,
        started_ms: Option<i64>,
        ended_ms: Option<i64>,
        tools: Vec<TopologyToolStep>,
    },
    /// One serial tool barrier (`shell`, `edit_file`, `ask_user`, `submit_plan`, etc.).
    SerialTool {
        wave_index: u32,
        started_ms: Option<i64>,
        ended_ms: Option<i64>,
        tool: TopologyToolStep,
    },
}

/// Full execution topology and waterfall timeline of one top-level run.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRunTopology {
    pub run: AgentRunSummary,
    pub title: String,
    pub folder: Option<String>,
    pub awake_ms: u64,
    /// Reference Unix ms timestamp captured when assembling this snapshot for live waterfall bars.
    pub snapshot_ms: i64,
    pub waiting_for_slot: bool,
    pub wait: Option<BoardWait>,
    pub pending_approval: Option<PendingApprovalSummary>,
    pub pending_changes: BoardChanges,
    pub plan_done: u32,
    pub plan_total: u32,
    pub cost_currency: Option<String>,
    pub last_answer: Option<String>,
    pub queued_turns: u32,
    pub queue_paused: bool,
    pub waves: Vec<ExecutionWave>,
    pub subagent_counts: BoardSubagentCounts,
    /// Structured Task Graph (`任务图`) persisted on the session (`todos_json`), overlaid with
    /// live sub-agent runtime bindings when present.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub task_graph: Vec<super::model::AgentTodoItem>,
}

/// How the payload is used; it fixes the byte budget.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ContextUse {
    /// An `@` mention in the composer.
    #[default]
    Mention,
    /// The "Ask agent" hand-off of a response, a log row, a probe or a failed assertion.
    AskAgent,
}

/// One named fact of a context object.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextField {
    /// What it is (`status`, `latency`).
    pub name: String,
    /// Its value.
    pub value: String,
}

/// The object a message mentions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum ContextRef {
    /// A saved request, as stored: variables unresolved.
    Request {
        /// Its workspace.
        workspace_id: String,
        /// The request.
        request_id: String,
    },
    /// A history entry: the request as sent, status, timing, headers, a response preview.
    History {
        /// Its workspace.
        workspace_id: String,
        /// The entry.
        history_id: String,
    },
    /// A gateway log row (or the rows of a filter) as the Activity view shows them; `exchange` is
    /// the stored request and response preview, used by "Ask agent" only.
    Gateway {
        /// What the chip says (`Log row: 429 deepseek-v4-pro`).
        label: String,
        /// The row's columns.
        fields: Vec<ContextField>,
        /// The stored exchange (headers and bodies, already redacted at capture).
        exchange: Option<String>,
    },
    /// A probe result.
    Probe {
        /// The provider.
        provider: String,
        /// The check (`Vision`).
        check: String,
        /// Its status word.
        status: String,
        /// Latency in milliseconds.
        latency_ms: Option<u64>,
    },
    /// A failed assertion.
    Assertion {
        /// The assertion as written.
        assertion: String,
        /// What it expected.
        expected: String,
        /// What it got.
        actual: String,
        /// A summary of the response.
        response_summary: String,
    },
}

/// A built payload: the chip's label and the tagged text the model receives.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ContextPayload {
    /// The chip text.
    pub label: String,
    /// The tagged, redacted text part.
    pub text: String,
}
