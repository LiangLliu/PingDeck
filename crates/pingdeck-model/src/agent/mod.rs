//! The agent harness's wire and storage models: sessions, messages, MCP servers, skills.

pub mod approvals;
pub mod context;
pub mod failure;
pub mod ledger;
pub mod mcp;
pub mod permissions;
pub mod sandbox;
pub mod tools;

pub use approvals::RuleContext;
pub use context::repair_orphan_tool_calls;
pub use failure::{AgentFailure, AgentFailureKind, AgentFailureTarget};
pub use ledger::{LedgerEntry, LedgerKind};
pub use mcp::McpTransportConfig;
pub use permissions::{ApprovalCard, PermissionAction, PermissionRule, RiskLevel, RiskTag};
pub use sandbox::{SandboxKind, SandboxState, SandboxStatus, SandboxTag};
pub use tools::{
    OutputStream, ToolApprovalPreview, ToolClass, ToolErrorCode, ToolSource, coded_error_text,
};

pub use crate::agent_target::AgentTarget;

use serde::{Deserialize, Serialize};

use crate::agent::tools::FileChangeKind as ToolFileChangeKind;
use crate::events::Sequenced;
use crate::hub::{HubCitation, HubEffort, HubPart, HubUsage};
use crate::settings::ToolApproval;

/// Reasoning effort a session asks for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ReasoningEffort {
    /// Least thinking the provider offers.
    Minimal,
    /// Low.
    Low,
    /// Medium.
    Medium,
    /// High.
    High,
    /// Maximum.
    Max,
}

impl From<ReasoningEffort> for HubEffort {
    fn from(effort: ReasoningEffort) -> Self {
        match effort {
            ReasoningEffort::Minimal => Self::Minimal,
            ReasoningEffort::Low => Self::Low,
            ReasoningEffort::Medium => Self::Medium,
            ReasoningEffort::High => Self::High,
            ReasoningEffort::Max => Self::Max,
        }
    }
}

/// A chat session.
///
/// `todos`, the totals and `last_context_tokens` are maintained by tools and runs through their
/// own store calls, never by a settings save; so are `head_message_id` and the lineage fields.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSession {
    /// Session id.
    pub id: String,
    /// Title shown in the session list.
    pub title: String,
    /// The model to call; `None` until one is chosen.
    pub target: Option<AgentTarget>,
    /// Bound local folder (absolute, canonical) for the file and shell tools; `None` disables
    /// the local workspace tools.
    pub workspace_dir: Option<String>,
    /// Which tool calls run without asking. Per chat; branches never change it.
    pub permission_mode: ToolApproval,
    /// Reasoning effort; `None` leaves the model's default.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Tools switched off for this chat (every other tool is available).
    pub disabled_tools: Vec<String>,
    /// Skills switched off for this chat.
    pub disabled_skills: Vec<String>,
    /// Web search and fetch are on for this chat.
    pub web_enabled: bool,
    /// The user named the chat: automatic titling leaves it alone.
    pub title_locked: bool,
    /// Last message of the active path; `None` for an empty chat.
    pub head_message_id: Option<String>,
    /// When the chat was pinned (pinned chats are listed first); `None` if not pinned.
    pub pinned_at_ms: Option<i64>,
    /// When the chat was archived (hidden from the default list); `None` if not archived.
    pub archived_at_ms: Option<i64>,
    /// When the chat was deleted. A tombstone: the rows stay until they are purged, and
    /// `agent_session_restore` clears it.
    pub deleted_at_ms: Option<i64>,
    /// The session this one was branched from.
    pub forked_from: Option<String>,
    /// The message of `forked_from` the copy ends at.
    pub forked_from_message_id: Option<String>,
    /// The session whose `task` call created this one.
    pub parent_session_id: Option<String>,
    /// The run that spawned this session.
    pub parent_run_id: Option<String>,
    /// The model's task list (`todo_write`).
    pub todos: Vec<AgentTodoItem>,
    /// Token usage summed over every run of the session.
    pub total_usage: HubUsage,
    /// Cost summed over every run of the session, in millionths of a US dollar.
    pub total_cost_micro_usd: i64,
    /// Prompt size of the latest model call, the basis of auto-compaction.
    pub last_context_tokens: u64,
    /// The PingDeck workspace the chat started in; the PingDeck tools act there whichever
    /// workspace is open later. `None` until the chat is bound.
    #[serde(default)]
    pub pingdeck_workspace_id: Option<String>,
    /// The environment that was active when the chat started (`send_request`'s default).
    #[serde(default)]
    pub pingdeck_environment_id: Option<String>,
    /// Name of the bound workspace for the Details panel; `None` when unbound or deleted. Filled
    /// by the store for read results, never written.
    #[serde(default)]
    pub pingdeck_workspace_name: Option<String>,
    /// A one-time note the chat shows inline until dismissed (`agent_session_dismiss_notice`):
    /// `full_access_off` after the update that turned the old global Full access off.
    #[serde(default)]
    pub notice: Option<String>,
    /// The chat's own instructions (a per-chat system prompt): inserted into the prompt after the
    /// base instructions and before the folder's project instructions. `None` is the core's
    /// prompt alone. A branch into a new chat copies none of it.
    #[serde(default)]
    pub instructions: Option<String>,
    /// Creation time, Unix milliseconds.
    pub created_at_ms: i64,
    /// Last change (a message or a setting), Unix milliseconds.
    pub updated_at_ms: i64,
}

/// Progress of one task of a session's task list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentTodoStatus {
    /// Not started.
    Pending,
    /// Being worked on.
    InProgress,
    /// Done.
    Completed,
    /// Dropped.
    Cancelled,
}

/// One task of a session's task list.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentTodoItem {
    /// Task id, unique within the list.
    pub id: String,
    /// What to do.
    pub content: String,
    /// Progress.
    pub status: AgentTodoStatus,
}

/// Author of a stored message.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRole {
    /// The user.
    User,
    /// The model.
    Assistant,
    /// Tool results fed back to the model.
    Tool,
    /// The compaction summary that replaces older history for the model.
    Summary,
}

impl AgentRole {
    /// The stable name used on the wire.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::User => "user",
            Self::Assistant => "assistant",
            Self::Tool => "tool",
            Self::Summary => "summary",
        }
    }

    /// Parses [`AgentRole::as_str`].
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "user" => Some(Self::User),
            "assistant" => Some(Self::Assistant),
            "tool" => Some(Self::Tool),
            "summary" => Some(Self::Summary),
            _ => None,
        }
    }
}

/// Whether a cited source was only a search hit or its page was fetched.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentSourceKind {
    /// Listed by a search.
    Search,
    /// Fetched with `fetch_url`.
    Page,
}

/// One numbered source of a turn; the number is what the answer text cites as `[n]`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSourceDto {
    /// Number within the turn, from 1; never renumbered or reused.
    pub source_id: u32,
    /// The page address.
    pub url: String,
    /// The page title, or the address when there is none.
    pub title: String,
    /// How the source was obtained.
    pub kind: AgentSourceKind,
}

/// Why a skill attached to a message did not reach the model in full.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SkillUndeliveredReason {
    /// Not among the chat's enabled skills: deleted, switched off or in an untrusted folder.
    Unavailable,
    /// `SKILL.md` could not be read.
    Unreadable,
    /// Over the inline budget; the model was told to call `read_skill` instead.
    TooLong,
}

/// One attached skill whose text the model did not get.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SkillUndelivered {
    /// The skill name as attached.
    pub name: String,
    /// Why.
    pub reason: SkillUndeliveredReason,
}

/// What a message carries besides its parts.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentMessageMeta {
    /// Skills the user attached to this message.
    pub skills: Vec<String>,
    /// The message was a steer injected mid-run, not a new instruction: it is not a turn
    /// boundary.
    pub steered: bool,
    /// The message was written by the app, not the user (the background process exit line, "the
    /// user is back"): like a steer it is not a turn boundary.
    pub system: bool,
    /// Sources cited in this assistant message.
    pub sources: Vec<AgentSourceDto>,
    /// `provider_id/model` of the fallback that wrote this answer instead of the chat's model;
    /// absent when the chat's own model answered.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub served_model: Option<String>,
    /// The reasoning effort sent with the model call behind an assistant message; absent when the
    /// call carried none (Auto, or a model without a reasoning control).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Milliseconds from sending the request to the first answer content of the model call
    /// behind an assistant message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub ttft_ms: Option<u64>,
    /// Milliseconds from the first answer content to the end of the stream.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stream_ms: Option<u64>,
    /// Why the run ended, written on the last assistant message of the run.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub stop_reason: Option<AgentStopReason>,
    /// `AppError` kind of the failure that ended the run at this message.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub error_kind: Option<String>,
    /// The structured failure class of that failure.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub failure: Option<AgentFailure>,
}

/// One stored message of a session. Messages form a tree: `parent_id` links to the previous
/// message and siblings are alternatives made by edit (user messages) and retry (the first
/// assistant message of a turn). The session's `head_message_id` selects the active path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentMessage {
    /// Message id.
    pub id: String,
    /// Owning session.
    pub session_id: String,
    /// Insertion order within the session, from 1 (an ordering key, not a path position).
    pub seq: i64,
    /// Author.
    pub role: AgentRole,
    /// Content: text, reasoning, tool calls and tool results; attachments appear only as
    /// `AttachmentRef`s.
    pub parts: Vec<HubPart>,
    /// Token usage of the model call that produced an assistant message.
    pub usage: Option<HubUsage>,
    /// Model label that produced an assistant message.
    pub model: Option<String>,
    /// Why generation ended early (error text), if it did.
    pub error: Option<String>,
    /// The run that stored the message.
    pub run_id: Option<String>,
    /// The previous message of the path; `None` for a root.
    pub parent_id: Option<String>,
    /// Position among the messages sharing `parent_id`, from 0.
    pub branch_index: u32,
    /// The assistant message is still being written (or the run died before it finished).
    pub partial: bool,
    /// Cost of the model call behind an assistant message, in millionths of a US dollar.
    pub cost_micro_usd: i64,
    /// Currency of `cost_micro_usd` (`USD`, `CNY`); `None` for a free or unpriced call.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_currency: Option<String>,
    /// Skills, steer flag and cited sources.
    #[serde(default)]
    pub meta: AgentMessageMeta,
    /// Ids of the messages at this node (same parent) ordered by `branch_index`, itself
    /// included. Filled by the store for read results and empty when the message has no
    /// sibling.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub branch_siblings: Vec<String>,
    /// Files attached to the message. Filled by the service layer, never stored in the row.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub attachments: Vec<AgentAttachment>,
    /// Creation time, Unix milliseconds.
    pub created_at_ms: i64,
}

impl AgentMessage {
    /// A message that opens a turn: written by the user as a new instruction, not a steer.
    #[must_use]
    pub fn is_turn_boundary(&self) -> bool {
        self.role == AgentRole::User && !self.meta.steered && !self.meta.system
    }
}

/// A session with one page of its active path.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionDetail {
    /// The session.
    pub session: AgentSession,
    /// Messages of the active path in order, oldest first.
    pub messages: Vec<AgentMessage>,
    /// Older turns exist before the first returned message.
    pub has_more: bool,
    /// Later turns exist after the last returned message (a page opened around a search hit).
    #[serde(default)]
    pub has_more_after: bool,
    /// The audit rows of the tool calls of the returned messages (state, decision, exit code):
    /// a stored tool result only knows whether it was an error.
    #[serde(default)]
    pub tool_calls: Vec<AgentToolCallRecord>,
    /// The session's runs that are still active.
    pub runs: Vec<AgentRunSummary>,
    /// The unfinished answer of the chat's running top-level run, read atomically with its event
    /// position: subscribe to the run with `from_seq = live.seq` to continue it without a gap or
    /// a duplicate. Absent while no run is active.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub live: Option<LiveRunSnapshot>,
    /// The chat's queue waits for a resume (after a restart, a failed run or a failed start);
    /// absent when it is not paused.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub queue_paused: Option<bool>,
    /// What the chat's runs and their sub-agents cost, one entry per currency, never summed
    /// across currencies; absent when nothing was priced.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_by_currency: Option<Vec<CurrencyCost>>,
    /// The chat is in Plan mode (`agent_sessions.plan_mode`, kept across a relaunch); absent
    /// when it is not.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub plan_mode: Option<bool>,
}

/// An amount in one currency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencyCost {
    /// ISO 4217 code.
    pub currency: String,
    /// Millionths of the currency.
    pub cost_micro: i64,
}

/// Cost of runs in one currency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencyTotal {
    /// `USD` or `CNY`.
    pub currency: String,
    /// Millionths of the currency.
    pub cost_micro: i64,
}

/// Where one tool call of the running model turn stands.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum LiveToolState {
    /// The model is still writing the arguments.
    StreamingArgs,
    /// The call waits for the user's approval.
    WaitingApproval,
    /// The arguments are complete and the tool runs (or is about to).
    Running,
    /// The call ended; see `result`.
    Done,
}

/// How a finished tool call ended, as far as the live view shows it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveToolResult {
    /// How it ended.
    pub status: ToolCallStatus,
    /// The first 200 characters of the result text (empty for a result without text).
    pub summary: String,
    /// Time from start of execution (or decision) to the end.
    pub duration_ms: u64,
}

/// One tool call of the running model turn. Its name and the arguments so far are the matching
/// [`HubPart::ToolCall`] in [`LiveRunSnapshot::parts`].
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveToolCall {
    /// Call id.
    pub call_id: String,
    /// Tool name.
    pub name: String,
    /// Where the tool comes from.
    pub source: ToolSource,
    /// Where the call stands.
    pub state: LiveToolState,
    /// The sub-agent run a `task` call started.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub child_run_id: Option<String>,
    /// Set once `state` is `Done`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub result: Option<LiveToolResult>,
    /// The approval card while `state` is `WaitingApproval` (when the permission check built
    /// one), so a reattached chat can draw the card without the evicted `ToolApprovalRequired`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub approval: Option<Box<ApprovalCard>>,
}

/// The run's last `Usage` event, for a chat that reattaches after the replay ring evicted it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveUsage {
    /// The run so far.
    pub run_usage: HubUsage,
    /// The run's cost so far (children included), in millionths of `currency`.
    pub cost_micro_usd: i64,
    /// Currency of `cost_micro_usd`; `None` while the model has no price.
    pub currency: Option<String>,
    /// Prompt size of the last call.
    pub context_used_tokens: u64,
    /// The model's context window when known.
    pub context_limit_tokens: Option<u64>,
}

/// Which kind of `HumanGate` a `GateOpened` announces.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum HumanGateKind {
    /// The model submitted a plan (`submit_plan`) and waits for approval.
    Plan,
    /// The model asks the user a question (`ask_user`).
    Question,
}

/// What a gate shows, by kind.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "type", rename_all = "snake_case")]
pub enum HumanGateBody {
    /// The plan the model submitted, as Markdown (the `submit_plan` call's argument).
    Plan {
        /// The plan text.
        markdown: String,
        /// The model's title for it.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        title: Option<String>,
        /// Top-level list items of the plan (its steps).
        steps: u32,
    },
    /// A question the model asked (`ask_user`). Shown as plain text; the UI adds "Other".
    Question {
        /// The question.
        question: String,
        /// Two to four options, numbered from 1 in this order.
        options: Vec<QuestionOption>,
        /// More than one option may be chosen.
        multi: bool,
    },
}

/// One option of an `ask_user` question.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct QuestionOption {
    /// The option.
    pub label: String,
    /// One line about it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub detail: Option<String>,
}

/// The user's answer to an `ask_user` question (`agent_question_answer_save`). Information for the
/// model, never an approval of any action.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentQuestionAnswer {
    /// Chosen options, numbered from 1.
    #[serde(default)]
    pub choices: Vec<u8>,
    /// The user's own answer ("Other").
    #[serde(default)]
    pub other: Option<String>,
    /// The user skipped the question.
    #[serde(default)]
    pub skipped: bool,
}

/// A plan or question waiting for the user.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HumanGateDto {
    /// The gate (the tool call's id).
    pub id: String,
    /// The waiting run.
    pub run_id: String,
    /// The tool call that opened it.
    pub call_id: String,
    /// What waits.
    pub kind: HumanGateKind,
    /// When it opened, Unix milliseconds.
    pub created_at_ms: i64,
    /// What the card shows.
    pub body: HumanGateBody,
}

/// How a gate closed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum GateOutcomeDto {
    /// The user answered (approved, or replied with changes).
    Answered,
    /// The user skipped it.
    Skipped,
    /// Nobody watched and the wait ran out.
    TimedOut,
    /// The run ended or was stopped first.
    Moot,
}

/// What a running top-level run has streamed since its last stored assistant message: the part of
/// the answer that no stored message holds yet. Taken before the messages are read, so a message
/// stored since then is announced by a later `MessageSaved` (drop the live parts when it
/// arrives); a partial (`partial: true`) stored row of the same turn is superseded by `parts`.
/// `usage`, a waiting call's `approval` and a waiting cost pause outlive a step, so a chat that
/// reattaches after those events left the replay ring still draws the run's meter, its approval
/// card and its cost card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveRunSnapshot {
    /// The run.
    pub run_id: String,
    /// Position of the run's last event included here (0 before the first).
    pub seq: u64,
    /// Text, reasoning and tool calls in arrival order; text and reasoning are the accumulated
    /// deltas, a tool call carries the arguments so far.
    pub parts: Vec<HubPart>,
    /// The state of every [`HubPart::ToolCall`] in `parts`.
    pub tools: Vec<LiveToolCall>,
    /// The run's last `Usage` event; `None` before the first model call ended.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub usage: Option<LiveUsage>,
    /// The run streamed more than 1 MiB since the last stored message; later fragments are not
    /// in `parts`.
    pub truncated: bool,
    /// The plans and questions the run waits on, so a reloaded chat still shows them.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub gates: Vec<HumanGateDto>,
    /// The cost pause the run waits on; `None` once the run goes on or ends.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_limit: Option<LiveCostLimit>,
}

/// The `CostLimitReached` a run waits on, for a chat that reattaches after the replay ring
/// evicted it: the event's fields without its tag.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct LiveCostLimit {
    /// The run.
    pub run_id: String,
    /// Its cost so far, in millionths of `currency` (tokens when `tokens`).
    pub cost_micro: i64,
    /// The cap in force, in millionths of `currency` (tokens when `tokens`).
    pub cap_micro: i64,
    /// ISO 4217 code of the price; empty when `tokens`.
    pub currency: String,
    /// What the main model, the sub-agents and the summaries spent of `cost_micro`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub breakdown: Option<Vec<CostShare>>,
    /// Where the next pause comes when the user continues.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub next_cap_micro: Option<i64>,
    /// Models the run called that have no price.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub unpriced_models: Option<Vec<String>>,
    /// The pause is `ai.agent.max_run_tokens`: the figures are tokens.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub tokens: Option<bool>,
}

/// Whole turns of the active path, with whether more turns exist on either side.
#[derive(Debug, Clone, PartialEq)]
pub struct AgentPathWindow {
    /// The messages of the turns, oldest first.
    pub messages: Vec<AgentMessage>,
    /// Earlier turns exist.
    pub has_before: bool,
    /// Later turns exist.
    pub has_after: bool,
}

/// One hit of the in-chat find: where it is on the active path, so an unloaded turn can be
/// opened with `agent_session_get(around_message_id)`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentThreadHit {
    /// The message holding the match.
    pub message_id: String,
    /// Insertion order of the message (an ordering key).
    pub seq: i64,
    /// Index of the message's turn on the active path, from 0.
    pub turn_index: u32,
    /// The match is inside a tool result, not in text the user or the model wrote.
    pub in_tool_output: bool,
    /// Text around the first match with the match wrapped in `[` and `]`.
    pub snippet: String,
}

/// One row of the session list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionSummary {
    /// Session id.
    pub id: String,
    /// Title.
    pub title: String,
    /// The session's model.
    pub target: Option<AgentTarget>,
    /// Stored messages (every branch).
    pub message_count: i64,
    /// Text of the last user or assistant message, cut to 160 characters.
    pub preview: String,
    /// Last change, Unix milliseconds.
    pub updated_at_ms: i64,
    /// Pinned sessions are listed first.
    pub pinned: bool,
    /// Archived sessions are hidden unless asked for.
    pub archived: bool,
    /// The session's bound folder.
    pub workspace_dir: Option<String>,
    /// The session that spawned this one as a sub-agent.
    pub parent_session_id: Option<String>,
    /// A run of the session is queued, running or waiting for approval.
    pub running: bool,
    /// The PingDeck workspace the chat is bound to; `None` until the first message binds it.
    #[serde(default)]
    pub pingdeck_workspace_id: Option<String>,
    /// Name of that workspace; `None` when unbound or deleted.
    #[serde(default)]
    pub pingdeck_workspace_name: Option<String>,
    /// The chat this one was branched from ("Branch into new chat"); the list marks a branch by it.
    #[serde(default)]
    pub forked_from: Option<String>,
}

/// One page of the session list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentSessionPage {
    /// The rows, pinned first and then newest first.
    pub sessions: Vec<AgentSessionSummary>,
    /// Pass back as `cursor` for the next page; `None` on the last page.
    pub next_cursor: Option<String>,
}

/// Which sessions the list returns; every field narrows the result. Tombstoned sessions and
/// sub-agent sessions never appear unless asked for.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSessionFilter {
    /// Keeps sessions whose title or any message text contains it (case-insensitive for ASCII).
    pub query: Option<String>,
    /// Only pinned sessions.
    pub pinned_only: bool,
    /// Also list archived sessions.
    pub include_archived: bool,
    /// Only sessions bound to exactly this folder.
    pub workspace_dir: Option<String>,
    /// Only sessions with an active run.
    pub only_running: bool,
    /// Also list tombstoned sessions.
    pub include_deleted: bool,
    /// Only tombstoned sessions (the "recently deleted" view); implies `include_deleted`.
    pub deleted_only: bool,
    /// `None` lists top-level chats; a session id lists the sub-agent sessions it spawned.
    pub parent_session_id: Option<String>,
    /// Only sessions bound to this PingDeck workspace.
    pub pingdeck_workspace_id: Option<String>,
    /// Only sessions with no project folder.
    pub no_folder: bool,
}

/// One hit of the cross-session message search.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SessionSearchHit {
    /// The session holding the message.
    pub session_id: String,
    /// The matching message.
    pub message_id: String,
    /// The session's title.
    pub title: String,
    /// Text around the match with the match wrapped in `[` and `]`.
    pub snippet: String,
    /// When the message was written, Unix milliseconds.
    pub ts_ms: i64,
    /// When the session last changed, Unix milliseconds.
    pub updated_at_ms: i64,
    /// The match is inside a tool result (the message is a tool message), not in prose.
    pub in_tool_output: bool,
}

/// Fields a caller may set when creating a session; the rest come from the agent settings.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSessionInput {
    /// Title; blank becomes `New chat`.
    pub title: Option<String>,
    /// Model; `None` uses `ai.agent.default_target`, else the newest chat's model.
    pub target: Option<AgentTarget>,
    /// Refuse with `no_model` instead of creating a chat no model can answer (the first message
    /// of a new chat).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub require_model: Option<bool>,
    /// Folder to bind (absolute, existing); `None` uses `ai.agent.default_workspace`, blank
    /// binds none.
    pub workspace_dir: Option<String>,
    /// Permission mode; `None` starts in Approve reads.
    pub permission_mode: Option<ToolApproval>,
    /// Reasoning effort; `None` uses `ai.agent.default_effort`.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// Tools to switch off.
    pub disabled_tools: Option<Vec<String>>,
    /// Skills to switch off.
    pub disabled_skills: Option<Vec<String>>,
    /// Web access; `None` is off.
    pub web_enabled: Option<bool>,
    /// The PingDeck workspace active when the chat starts; the PingDeck tools act there.
    pub pingdeck_workspace_id: Option<String>,
    /// The environment active when the chat starts.
    pub pingdeck_environment_id: Option<String>,
    /// The chat's own instructions (see [`AgentSession::instructions`]); `None` uses the core's
    /// prompt alone.
    pub instructions: Option<String>,
}

/// Reads a field where an absent key and an explicit `null` differ: absent is `None` (by
/// `#[serde(default)]`), `null` is `Some(None)` and a value is `Some(Some(value))`.
fn double_option<'de, D, T>(deserializer: D) -> Result<Option<Option<T>>, D::Error>
where
    D: serde::Deserializer<'de>,
    T: Deserialize<'de>,
{
    Option::<T>::deserialize(deserializer).map(Some)
}

/// A partial update of a session; absent fields stay as they are.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
#[serde(default)]
pub struct AgentSessionPatch {
    /// New title; setting one locks automatic titling.
    pub title: Option<String>,
    /// New model.
    pub target: Option<AgentTarget>,
    /// New bound folder (absolute, existing); an empty string unbinds.
    pub workspace_dir: Option<String>,
    /// New permission mode.
    pub permission_mode: Option<ToolApproval>,
    /// New reasoning effort: absent leaves it, `null` clears it to the model's default (Auto).
    #[serde(
        default,
        deserialize_with = "double_option",
        skip_serializing_if = "Option::is_none"
    )]
    pub reasoning_effort: Option<Option<ReasoningEffort>>,
    /// Replace the switched-off tools.
    pub disabled_tools: Option<Vec<String>>,
    /// Replace the switched-off skills.
    pub disabled_skills: Option<Vec<String>>,
    /// Web access on or off.
    pub web_enabled: Option<bool>,
    /// Lock or unlock automatic titling.
    pub title_locked: Option<bool>,
    /// Pin or unpin.
    pub pinned: Option<bool>,
    /// Archive or unarchive.
    pub archived: Option<bool>,
    /// Replace the task list.
    pub todos: Option<Vec<AgentTodoItem>>,
    /// The chat's own instructions; an empty string clears them, `None` leaves them as they are.
    pub instructions: Option<String>,
}

/// Export format of a session.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentExportFormat {
    /// Readable Markdown transcript of the active path.
    Markdown,
    /// The session, its active path and its runs as JSON.
    Json,
}

/// What a redacted export of a session would replace.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentExportPreview {
    /// Secret values the redaction replaces in the export.
    pub redactions: u32,
}

/// Lifecycle state of a run. Wire values are the `snake_case` names, `waiting_approval`
/// included.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunStatus {
    /// Waiting for one of the top-level slots or for the chat's lock.
    Queued,
    /// Calling the model or a tool.
    Running,
    /// Paused on a tool approval; holds no concurrency slot.
    WaitingApproval,
    /// Ended with an answer.
    Completed,
    /// Ended with an error.
    Failed,
    /// Stopped by the user.
    Cancelled,
    /// Cut off by an application restart.
    Interrupted,
}

impl AgentRunStatus {
    /// The stable storage and wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Queued => "queued",
            Self::Running => "running",
            Self::WaitingApproval => "waiting_approval",
            Self::Completed => "completed",
            Self::Failed => "failed",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
        }
    }

    /// Parses [`AgentRunStatus::as_str`].
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "queued" => Some(Self::Queued),
            "running" => Some(Self::Running),
            "waiting_approval" => Some(Self::WaitingApproval),
            "completed" => Some(Self::Completed),
            "failed" => Some(Self::Failed),
            "cancelled" => Some(Self::Cancelled),
            "interrupted" => Some(Self::Interrupted),
            _ => None,
        }
    }

    /// The run has not ended: queued, running or waiting for approval.
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::WaitingApproval)
    }
}

/// The model role a `task` sub-agent runs with: which configured model does the job
/// (`ai.agent.role_models`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum SubagentRole {
    /// Quick, cheap work: searching, reading and summarising, many small edits.
    Fast,
    /// Hard reasoning: design, tricky debugging, checking conclusions.
    Deep,
    /// Reading images: screenshots, UI, charts, scanned pages.
    Vision,
}

impl SubagentRole {
    /// Every role, in display order.
    pub const ALL: [Self; 3] = [Self::Fast, Self::Deep, Self::Vision];

    /// The stable storage and wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Fast => "fast",
            Self::Deep => "deep",
            Self::Vision => "vision",
        }
    }

    /// The role a stored string names.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|role| role.as_str() == text)
    }
}

/// What a run is.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentRunKind {
    /// A user's turn in a chat.
    Main,
    /// A `task` child run.
    Subagent,
    /// A run nobody started in the foreground (compaction, queued dispatch).
    Background,
}

impl AgentRunKind {
    /// The stable storage string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Main => "main",
            Self::Subagent => "subagent",
            Self::Background => "background",
        }
    }

    /// Parses [`AgentRunKind::as_str`].
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "main" => Some(Self::Main),
            "subagent" => Some(Self::Subagent),
            "background" => Some(Self::Background),
            _ => None,
        }
    }
}

/// A run as the UI and the run list see it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRunSummary {
    /// Run id.
    pub run_id: String,
    /// The session the run answers in.
    pub session_id: String,
    /// What the run is.
    pub kind: AgentRunKind,
    /// The run that spawned this one as a sub-agent.
    pub parent_run_id: Option<String>,
    /// The `task` call that spawned this one.
    pub parent_call_id: Option<String>,
    /// Lifecycle state.
    pub status: AgentRunStatus,
    /// Label of the model that was called.
    pub model_label: String,
    /// The model the run was started with.
    pub target: Option<AgentTarget>,
    /// Model calls made so far.
    pub iterations: u32,
    /// Usage summed over the run.
    pub usage: HubUsage,
    /// Cost of the run, in millionths of a US dollar.
    pub cost_micro_usd: i64,
    /// Start time, Unix milliseconds.
    pub started_at_ms: i64,
    /// End time; `None` while active.
    pub finished_at_ms: Option<i64>,
    /// Time of the run's latest event, Unix milliseconds.
    pub last_event_ms: Option<i64>,
    /// What the run was doing last (`model`, a tool name), for the agent board.
    pub last_step: Option<String>,
    /// Why the run ended (`completed`, `step_limit`, ...); `None` while active.
    pub stop_reason: Option<String>,
    /// `AppError` kind name of a failed run.
    pub error_kind: Option<String>,
    /// Failure text of a failed or interrupted run.
    pub error: Option<String>,
    /// Version of the prompt builder that composed the run's system prompt.
    pub prompt_version: u32,
    /// Sequence number of the latest event of the run's event stream. Not persisted: rows read
    /// from the store report 0 and the live run registry overlays the real value.
    pub last_seq: u64,
    /// The structured failure class of a failed run; `None` otherwise.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub failure: Option<AgentFailure>,
    /// The model role a sub-agent run was started with; `None` for the chat's own model.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub role: Option<SubagentRole>,
    /// ISO 4217 code of `cost_micro_usd` (the currency of the run's latest priced call); `None`
    /// while unknown.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cost_currency: Option<String>,
    /// A cost cap is set but the run's model has no price, so the cap cannot stop it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub cap_inapplicable: Option<bool>,
    /// What a `WaitingApproval` run waits for: `approval`, `question`, `plan` or `cost limit`.
    /// Not persisted.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub status_detail: Option<String>,
}

/// What deleting a message takes with it (also the preview shown before the user confirms).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct MessageDeletion {
    /// The message and its descendants.
    pub messages: usize,
    /// Runs all of whose messages are among them.
    pub runs: usize,
    /// Tool-call audit rows of those messages and runs.
    pub tool_calls: usize,
    /// File-change journal rows of those runs.
    pub file_changes: usize,
    /// Of those journal rows, the ones still awaiting review (`pending` or `conflict`): their
    /// Undo goes with them.
    #[serde(default)]
    pub pending_file_changes: usize,
    /// Leaf messages among them: the branches that end below the message (the active path and
    /// every retry or edit kept under it).
    #[serde(default)]
    pub branches: usize,
}

/// Most messages a session's queue holds.
pub const MAX_QUEUED_TURNS: usize = 5;

/// A message waiting for the session's current run to end.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct QueuedTurn {
    /// Queue item id.
    pub id: String,
    /// The session whose queue holds it.
    pub session_id: String,
    /// Position in the queue, from 0; dense.
    pub ord: u32,
    /// What will be sent: text and `AttachmentRef` parts.
    pub parts: Vec<HubPart>,
    /// Skills attached to it.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub skills: Vec<String>,
    /// Objects it mentions (`ContextRef`s as sent), read when it is sent.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub contexts: Vec<serde_json::Value>,
    /// When it was queued, Unix milliseconds.
    pub created_at_ms: i64,
}

/// What a queued message carries; stored as JSON in `agent_queue.content_json`.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct QueuedContent {
    /// Text and `AttachmentRef` parts.
    pub parts: Vec<HubPart>,
    /// Attached skills.
    #[serde(default)]
    pub skills: Vec<String>,
    /// Mentioned objects (`ContextRef`s as sent).
    #[serde(default)]
    pub contexts: Vec<serde_json::Value>,
}

impl From<Vec<HubPart>> for QueuedContent {
    fn from(parts: Vec<HubPart>) -> Self {
        Self {
            parts,
            ..Self::default()
        }
    }
}

/// How one tool call ended. Wire values are `snake_case`: `approval_timed_out` is the call whose
/// approval nobody answered in time (the tool state `approval-timed-out`), which is not a denial.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ToolCallStatus {
    /// The tool succeeded.
    Ok,
    /// The tool failed.
    Error,
    /// The user refused the call.
    Denied,
    /// A hard block or a per-tool Block refused the call without a card.
    Blocked,
    /// The run was cancelled meanwhile.
    Cancelled,
    /// The application stopped before the tool finished.
    Interrupted,
    /// Nobody answered the approval within `ai.agent.approval_timeout_s`.
    ApprovalTimedOut,
    /// The approval was pending when the run stopped or the application quit.
    Expired,
    /// The call never ran because a steer message or an earlier cancelled call came first.
    Superseded,
}

impl ToolCallStatus {
    /// The stable storage and wire string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Ok => "ok",
            Self::Error => "error",
            Self::Denied => "denied",
            Self::Blocked => "blocked",
            Self::Cancelled => "cancelled",
            Self::Interrupted => "interrupted",
            Self::ApprovalTimedOut => "approval_timed_out",
            Self::Expired => "expired",
            Self::Superseded => "superseded",
        }
    }

    /// Parses [`ToolCallStatus::as_str`].
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "ok" => Some(Self::Ok),
            "error" => Some(Self::Error),
            "denied" => Some(Self::Denied),
            "blocked" => Some(Self::Blocked),
            "cancelled" => Some(Self::Cancelled),
            "interrupted" => Some(Self::Interrupted),
            "approval_timed_out" => Some(Self::ApprovalTimedOut),
            "expired" => Some(Self::Expired),
            "superseded" => Some(Self::Superseded),
            _ => None,
        }
    }
}

/// The audit row of one tool call (`agent_tool_calls`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentToolCallRecord {
    /// Owning session.
    pub session_id: String,
    /// Call id, unique within the session.
    pub call_id: String,
    /// The run that made the call.
    pub run_id: String,
    /// The assistant message that carries the call.
    pub message_id: Option<String>,
    /// Tool name.
    pub name: String,
    /// Permission class of the tool (`read`, `edit`, `exec`, ...).
    pub class: String,
    /// The call's state in the tool state machine (`input-available`, `output-available`,
    /// `approval-timed-out`, ...).
    pub state: String,
    /// Complete arguments as JSON text.
    pub args_json: String,
    /// Risk the permission evaluator assigned (`low`, `medium`, `high`).
    pub risk: String,
    /// Who decided: `user`, `rule`, `mode`, `timeout`.
    pub decided_by: Option<String>,
    /// The decision text (`allow`, `deny`, ...).
    pub decision: Option<String>,
    /// When it was decided, Unix milliseconds.
    pub decided_ms: Option<i64>,
    /// When execution started, Unix milliseconds.
    pub started_ms: Option<i64>,
    /// When execution ended, Unix milliseconds.
    pub ended_ms: Option<i64>,
    /// Exit code of a shell command.
    pub exit_code: Option<i32>,
    /// Sandbox the call ran under (`seatbelt`, `none`).
    pub sandbox: Option<String>,
    /// The output was cut.
    pub truncated: bool,
    /// Size of the full output in bytes.
    pub output_bytes: u64,
    /// Where the full output was spilled to.
    pub spill_ref: Option<String>,
    /// When the approval card appeared, Unix milliseconds; `None` for a call that never asked.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub requested_ms: Option<i64>,
}

/// What an edit did to a file.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeKind {
    /// The file did not exist.
    Create,
    /// The file's content was changed.
    Edit,
}

impl FileChangeKind {
    /// The stable storage string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Create => "create",
            Self::Edit => "edit",
        }
    }

    /// Parses [`FileChangeKind::as_str`].
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "create" => Some(Self::Create),
            "edit" => Some(Self::Edit),
            _ => None,
        }
    }
}

/// Review state of a journaled file change.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FileChangeStatus {
    /// The row was written before the file was (crash-safe order): the write may or
    /// may not have happened. Hidden from every list; startup recovery completes or drops it.
    Applying,
    /// Waiting for the user to keep or undo it.
    Pending,
    /// Accepted; no disk operation.
    Kept,
    /// Reverted from the journal.
    Undone,
    /// The file changed since; undo needs `force`.
    Conflict,
}

impl FileChangeStatus {
    /// The stable storage string.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Applying => "applying",
            Self::Pending => "pending",
            Self::Kept => "kept",
            Self::Undone => "undone",
            Self::Conflict => "conflict",
        }
    }

    /// Parses [`FileChangeStatus::as_str`].
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "applying" => Some(Self::Applying),
            "pending" => Some(Self::Pending),
            "kept" => Some(Self::Kept),
            "undone" => Some(Self::Undone),
            "conflict" => Some(Self::Conflict),
            _ => None,
        }
    }
}

/// One row of the file-change journal (`agent_file_changes`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FileChange {
    /// Change id.
    pub id: String,
    /// Owning session.
    pub session_id: String,
    /// The run that made the change.
    pub run_id: String,
    /// The tool call that made it.
    pub call_id: String,
    /// Path as the tool saw it (inside the workspace folder).
    pub path: String,
    /// The canonical folder `path` is relative to: the chat's folder when the change was made.
    /// `None` for an old row whose folder is unknown; such a change cannot be undone.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub folder: Option<String>,
    /// Created or edited.
    pub kind: FileChangeKind,
    /// SHA-256 of the file before (`None` for a created file); the before-bytes live in the
    /// attachment store under this hash.
    pub before_sha256: Option<String>,
    /// SHA-256 of the file after.
    pub after_sha256: Option<String>,
    /// Lines added.
    pub added: u32,
    /// Lines removed.
    pub removed: u32,
    /// Review state (`Applying` is hidden from every list).
    pub status: FileChangeStatus,
    /// When the change was made, Unix milliseconds.
    pub created_at_ms: i64,
}

/// The answer to "Trust this folder?" for one canonical folder path.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentFolderTrust {
    /// The canonical folder path; the answer covers this path only, never its sub-folders.
    pub path: String,
    /// The user trusted the folder (`false`: asked and declined).
    pub trusted: bool,
    /// SHA-256 of the instructions file (`AGENTS.md`, else `CLAUDE.md`) when the folder was
    /// trusted; `None` when it had none. A different hash later means "Instructions changed".
    pub instructions_sha256: Option<String>,
    /// When the user answered, Unix milliseconds.
    pub decided_at_ms: i64,
}

/// What "Clear tool output" removed (`agent_tool_output_delete`).
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PurgeResult {
    /// Files removed.
    pub files: u64,
    /// Their size in bytes.
    pub bytes: u64,
}

/// A global per-tool policy (`agent_tool_policy`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentToolPolicy {
    /// Tool name, or an MCP tool name.
    pub tool_key: String,
    /// What to do with every call of the tool.
    pub policy: PermissionAction,
}

/// A configured MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMcpServer {
    /// Server id.
    pub id: String,
    /// Unique display name; also part of its tool names.
    pub name: String,
    /// How to reach it.
    pub transport: McpTransportConfig,
    /// Disabled servers are never started and offer no tools.
    pub enabled: bool,
    /// The server's `readOnlyHint`/`destructiveHint` are believed. Untrusted servers (the default)
    /// have every tool treated as unknown, so approval is always asked. Local-only: never synced.
    #[serde(default)]
    pub trusted: bool,
    /// Last change, Unix milliseconds.
    pub updated_at_ms: i64,
}

/// Result of testing an MCP server.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentMcpTestResult {
    /// The connection and `tools/list` worked.
    pub ok: bool,
    /// Server name and version when connected.
    pub server: Option<String>,
    /// Tools the server offers.
    pub tool_names: Vec<String>,
    /// Failure text when `ok` is false.
    pub error: Option<String>,
    /// How long connect plus listing took.
    pub elapsed_ms: u64,
}

/// A discovered or imported skill and whether the agent may use it.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentSkill {
    /// Skill name (frontmatter `name`, else the folder name).
    pub name: String,
    /// One-line description.
    pub description: String,
    /// Absolute folder holding `SKILL.md`.
    pub path: String,
    /// The scanned directory, or `managed` for imported skills.
    pub origin: String,
    /// Skills are enabled unless switched off.
    pub enabled: bool,
}

/// One tool the UI can offer in a session's tool picker.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentToolInfo {
    /// Function name.
    pub name: String,
    /// What it does.
    pub description: String,
    /// Read-only hint.
    pub read_only: bool,
    /// Destructive hint.
    pub destructive: bool,
    /// `builtin`, or the MCP server name.
    pub source: String,
    /// Whether it can run now (a builtin without its configuration is listed as unavailable).
    pub available: bool,
    /// The key of its per-tool policy when that is not `name`: `mcp:<server id>:<tool>` for an
    /// MCP tool.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub policy_key: Option<String>,
    /// The tool acts on the chat's bound PingDeck workspace, so a chat without one is not offered
    /// it. Written only when true; always false for an MCP tool.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub needs_workspace: bool,
}

/// One part of a run tree's spend at a cost pause.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostShare {
    /// `main`, `subagent` or `compaction`.
    pub part: String,
    /// Millionths of the pause's currency, or tokens.
    pub micro: i64,
}

/// How a run ended without failing.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AgentStopReason {
    /// The model answered without further tool calls.
    Completed,
    /// `ai.agent.max_tool_iterations` model calls were used.
    MaxIterations,
    /// The user chose to stop at the cost cap (the run ends `Cancelled`).
    CostLimit,
    /// The model stopped because its output token cap was hit: the answer is cut short.
    OutputLimit,
    /// The request no longer fits the context window, even after shortening and compaction.
    ContextFull,
    /// The model kept repeating the same call with the same result after it was told.
    NoProgress,
}

impl AgentStopReason {
    /// The stable storage string (`agent_runs.stop_reason`).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Completed => "completed",
            Self::MaxIterations => "max_iterations",
            Self::CostLimit => "cost_limit",
            Self::OutputLimit => "output_limit",
            Self::ContextFull => "context_full",
            Self::NoProgress => "no_progress",
        }
    }

    /// Whether a run that stopped this way leaves the chat's queue waiting for the user: the
    /// turn did not end with a finished answer.
    #[must_use]
    pub const fn pauses_queue(self) -> bool {
        matches!(
            self,
            Self::MaxIterations
                | Self::OutputLimit
                | Self::CostLimit
                | Self::ContextFull
                | Self::NoProgress
        )
    }
}

impl AgentEvent {
    /// A note without a localised line.
    #[must_use]
    pub fn notice(text: impl Into<String>) -> Self {
        Self::Notice {
            text: text.into(),
            code: None,
            args: None,
        }
    }

    /// A note the UI shows in the user's language by `code`, filled with `args`.
    #[must_use]
    pub fn coded_notice(code: &str, text: impl Into<String>, args: &[(&str, String)]) -> Self {
        Self::Notice {
            text: text.into(),
            code: Some(code.to_owned()),
            args: (!args.is_empty()).then(|| {
                args.iter()
                    .map(|(key, value)| ((*key).to_owned(), value.clone()))
                    .collect()
            }),
        }
    }
}

/// What the run loop returns when it did not fail.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentRunResult {
    /// Id of the last assistant message (empty when the run stopped before the model answered).
    pub message_id: String,
    /// Why the run stopped.
    pub stop_reason: AgentStopReason,
    /// Model calls made.
    pub iterations: u32,
    /// Usage summed over the run.
    pub usage: HubUsage,
}

/// The user's answer to a tool approval request.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalDecision {
    /// Run the tool.
    Allow,
    /// Refuse; the model is told the user denied it.
    Deny,
}

/// How far an approval answer reaches.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ApprovalScope {
    /// This call only.
    Once,
    /// This tool for the rest of the chat.
    Session,
    /// Calls matching a command pattern (shell) or path/URL (other tools) in this folder, for good.
    WorkspacePattern,
    /// This tool everywhere, for good.
    Tool,
}

/// Progress of a run as the UI reads it from the run's event stream. Every event of a run's hub is
/// numbered by `Sequenced`; a run ends with exactly one `Done` or `Error`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "kind")]
pub enum AgentEvent {
    /// The run started executing.
    RunStarted {
        /// The run's summary.
        run: Box<AgentRunSummary>,
    },
    /// The chat's queue changed, or the run ended with messages still queued.
    QueuedUpdated {
        /// The chat.
        session_id: String,
        /// The queue, first to run first.
        queued: Vec<QueuedTurn>,
        /// The queue waits for a resume (a run ended `Cancelled`, `Failed` or `Interrupted`).
        paused: bool,
    },
    /// A user message was stored (the turn's own, or a steer).
    UserMessage {
        /// The stored message.
        message: Box<AgentMessage>,
        /// Attached skills whose text the model did not get; empty when all arrived.
        #[serde(default, skip_serializing_if = "Vec::is_empty")]
        skills_undelivered: Vec<SkillUndelivered>,
    },
    /// MCP servers of this chat that have no cached tool list are connecting in the background.
    McpConnecting {
        /// Their names.
        server_names: Vec<String>,
    },
    /// Older history was replaced by a summary.
    ContextCompacted {
        /// Estimated tokens before.
        before_tokens: u64,
        /// Estimated tokens after.
        after_tokens: u64,
        /// The stored summary message.
        summary_message_id: String,
    },
    /// A fragment of answer text.
    TextDelta {
        /// The fragment.
        text: String,
    },
    /// A fragment of reasoning text.
    ReasoningDelta {
        /// The fragment.
        text: String,
    },
    /// The model cited a source.
    CitationAdded {
        /// The source.
        citation: HubCitation,
    },
    /// The model started a tool call.
    ToolCallStarted {
        /// Call id.
        call_id: String,
        /// Tool name.
        name: String,
        /// Where the tool comes from.
        source: ToolSource,
    },
    /// A fragment of the call's JSON arguments.
    ToolCallArgsDelta {
        /// Call id.
        call_id: String,
        /// The fragment.
        arguments: String,
    },
    /// The call needs the user's approval; answer with `agent_approval_answer`.
    ToolApprovalRequired {
        /// The run waiting.
        run_id: String,
        /// Call id.
        call_id: String,
        /// Tool name.
        name: String,
        /// Complete arguments as JSON text.
        arguments: String,
        /// The tool's permission class.
        class: ToolClass,
        /// This call only reads.
        read_only: bool,
        /// This call may destroy data.
        destructive: bool,
        /// The shell sandbox a `shell` call runs under here (none when the chat skipped it).
        sandbox: SandboxKind,
        /// Why approval is needed.
        reason: String,
        /// What the card shows.
        preview: Option<ToolApprovalPreview>,
        /// The shell would run without network; the card offers "Allow once with network".
        network: bool,
        /// `low`, `medium` or `high`.
        risk: String,
        /// One plain line of what the call does.
        summary: String,
        /// The whole command, URL or argument text, for a Low-risk call only (the board offers an
        /// inline Allow exactly when this is present).
        full_text: Option<String>,
        /// Everything the card shows (risk, tags, offered scopes, why it asks).
        card: Option<Box<ApprovalCard>>,
    },
    /// An approval was answered, timed out, or became moot.
    ToolApprovalResolved {
        /// Call id.
        call_id: String,
        /// The decision (`Deny` for a timeout, with `timed_out` set).
        decision: ApprovalDecision,
        /// How far the answer reaches.
        scope: ApprovalScope,
        /// Nobody answered within `ai.agent.approval_timeout_s`.
        timed_out: bool,
    },
    /// A fragment of a running tool's output.
    ToolOutputDelta {
        /// Call id.
        call_id: String,
        /// Which stream.
        stream: OutputStream,
        /// The fragment.
        text: String,
    },
    /// A tool call ended.
    ToolResult {
        /// Call id.
        call_id: String,
        /// Tool name.
        name: String,
        /// How it ended.
        status: ToolCallStatus,
        /// What the model reads (secrets redacted).
        parts: Vec<HubPart>,
        /// Time from start of execution (or decision) to the end.
        duration_ms: u64,
        /// The UI's link-out for this result; absent when it has none.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        meta: Option<crate::agent::tools::ToolResultMeta>,
        /// The exit status of the process behind the result (a shell call); absent for other tools and
        /// for a call that was cancelled or superseded. The stored audit row carries the same number.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        exit_code: Option<i32>,
    },
    /// A file tool changed a file.
    FileChanged {
        /// Call id.
        call_id: String,
        /// Workspace-relative path.
        path: String,
        /// Created or edited.
        change: ToolFileChangeKind,
        /// Lines added.
        added: u32,
        /// Lines removed.
        removed: u32,
        /// The journal row (`agent_file_changes`) the change was recorded in, for Keep and Undo;
        /// `None` when the journal could not be written (a `Notice` says why).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        change_id: Option<String>,
    },
    /// A short note for the user.
    Notice {
        /// The English text, shown when the UI has no line for `code`.
        text: String,
        /// A stable id the UI localises the note by (`retrying`, `no_progress`, …).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        code: Option<String>,
        /// The values the localised line fills in.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        args: Option<std::collections::BTreeMap<String, String>>,
    },
    /// The chat's task list changed.
    TodosUpdated {
        /// The new list.
        todos: Vec<AgentTodoItem>,
    },
    /// A `task` call started a sub-agent.
    SubagentStarted {
        /// The `task` call.
        parent_call_id: String,
        /// The child run.
        child_run: Box<AgentRunSummary>,
        /// The child's chat.
        child_session_id: String,
    },
    /// An event of a sub-agent run.
    SubagentEvent {
        /// The `task` call.
        parent_call_id: String,
        /// The child run.
        child_run_id: String,
        /// The child's event.
        event: Box<AgentEvent>,
    },
    /// A sub-agent ended.
    SubagentFinished {
        /// The `task` call.
        parent_call_id: String,
        /// The child run's final summary.
        child_run: Box<AgentRunSummary>,
    },
    /// Token usage and cost after one model call.
    Usage {
        /// This call's usage.
        iteration_usage: HubUsage,
        /// The run so far.
        run_usage: HubUsage,
        /// The run's cost so far (children included), in millionths of the price's currency.
        cost_micro_usd: i64,
        /// Prompt size of this call.
        context_used_tokens: u64,
        /// The model's context window when known.
        context_limit_tokens: Option<u64>,
        /// Currency of `cost_micro_usd` (`USD`, `CNY`); `None` while the model has no price.
        currency: Option<String>,
        /// Milliseconds from sending this call to its first answer content.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        ttft_ms: Option<u64>,
        /// Milliseconds from the first answer content to the end of this call's stream.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        stream_ms: Option<u64>,
    },
    /// The run reached its cost cap and waits for `agent_run_continue`.
    CostLimitReached {
        /// The run.
        run_id: String,
        /// Its cost so far, in millionths of `currency` (tokens when `tokens`).
        cost_micro: i64,
        /// The cap in force, in millionths of `currency` (tokens when `tokens`).
        cap_micro: i64,
        /// ISO 4217 code of the price; empty when `tokens`.
        currency: String,
        /// What the main model, the sub-agents and the summaries spent of `cost_micro`.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        breakdown: Option<Vec<CostShare>>,
        /// Where the next pause comes when the user continues.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        next_cap_micro: Option<i64>,
        /// Models the run called that have no price: their spend counts against no cap.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        unpriced_models: Option<Vec<String>>,
        /// The pause is `ai.agent.max_run_tokens`: the figures are tokens.
        #[serde(default, skip_serializing_if = "Option::is_none")]
        tokens: Option<bool>,
    },
    /// A plan (or, later, a question) waits for the user (`HumanGate`). Approvals keep
    /// `ToolApprovalRequired` and cost pauses `CostLimitReached`.
    GateOpened {
        /// The waiting run.
        run_id: String,
        /// What waits.
        gate: HumanGateDto,
    },
    /// A gate announced by `GateOpened` closed.
    GateResolved {
        /// The run.
        run_id: String,
        /// `HumanGateDto::id`.
        gate_id: String,
        /// How it closed.
        outcome: GateOutcomeDto,
    },
    /// An assistant or tool message was stored.
    MessageSaved {
        /// The stored message.
        message: Box<AgentMessage>,
    },
    /// The chat was renamed from its first message.
    SessionTitleUpdated {
        /// The chat.
        session_id: String,
        /// The new title.
        title: String,
    },
    /// Events before `first_seq` were evicted from the replay buffer; reload persisted state.
    Gap {
        /// The oldest sequence number still replayed.
        first_seq: u64,
    },
    /// The run ended normally.
    Done {
        /// The final summary.
        run: Box<AgentRunSummary>,
        /// Why it stopped.
        stop_reason: AgentStopReason,
    },
    /// The run failed or was cancelled.
    Error {
        /// The run.
        run_id: String,
        /// `AppError` kind name, e.g. `Cancelled`, `Network`.
        error_kind: String,
        /// Message.
        message: String,
        /// The failure class of a failed model call; `None` for a cancellation and for failures
        /// that are not a model call's (a store error).
        #[serde(default, skip_serializing_if = "Option::is_none")]
        failure: Option<AgentFailure>,
    },
}

/// An [`AgentEvent`] with its position in a run's stream, as the webview receives it.
///
/// The core's hub numbers every run's events from 1 ([`Sequenced`]); a chat subscription delivers
/// several runs one after another, so every envelope names its run. A merged delta (the 33 ms
/// coalescer folds adjacent fragments) or a folded replay slot (the ring's compaction) covers
/// `first_seq..=seq` and carries the last number, so a consumer can tell a merge from a loss.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct AgentEnvelope {
    /// Position of the (last) event in the run's stream, from 1.
    pub seq: u64,
    /// Position of the first event folded into this envelope; equals `seq` when none was.
    pub first_seq: u64,
    /// When the (last) event was emitted, in Unix milliseconds.
    pub ts_ms: i64,
    /// The run the event belongs to.
    pub run_id: String,
    /// The event itself.
    pub event: AgentEvent,
}

impl AgentEnvelope {
    /// The envelope of `sequenced`, an event of run `run_id`.
    #[must_use]
    pub fn of_run(run_id: &str, sequenced: Sequenced<AgentEvent>) -> Self {
        Self {
            seq: sequenced.seq,
            first_seq: sequenced.first_seq.unwrap_or(sequenced.seq),
            ts_ms: sequenced.ts_ms,
            run_id: run_id.to_owned(),
            event: sequenced.event,
        }
    }
}

/// What an attachment is, decided by sniffing its bytes (never by trusting the file name).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum AttachmentKind {
    /// PNG, JPEG, GIF or WebP.
    Image,
    /// A PDF document.
    Pdf,
    /// UTF-8 text or source code.
    Text,
}

/// A file attached to a chat message, stored content-addressed (`DataDirs::attachments`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AgentAttachment {
    /// Attachment id.
    pub id: String,
    /// SHA-256 of the content (the blob's file name).
    pub sha256: String,
    /// Display name (the first upload's name when the same content is added twice).
    pub name: String,
    /// Sniffed MIME type.
    pub media_type: String,
    /// Size in bytes.
    pub size_bytes: u64,
    /// What it is.
    pub kind: AttachmentKind,
    /// Pixel width of an image.
    pub width: Option<u32>,
    /// Pixel height of an image.
    pub height: Option<u32>,
    /// 96 px preview as a `data:` URL, made by the webview; `None` until one is stored.
    #[serde(default)]
    pub thumb_data_url: Option<String>,
    /// Lines of a text attachment (`None` for an image or a PDF).
    #[serde(default)]
    pub lines: Option<u32>,
    /// Creation time, Unix milliseconds.
    pub created_at_ms: i64,
    /// The blob is gone (Settings "Clear attachments" removed it); the row stays so the message
    /// keeps a "removed" marker and the chat still opens. Filled when a message is read, never
    /// stored.
    #[serde(default)]
    pub removed: bool,
}

/// Where a new attachment's bytes come from.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(tag = "kind", rename_all = "snake_case")]
pub enum AgentAttachmentInput {
    /// A file on disk (the picker or a drop).
    Path {
        /// Absolute path.
        path: String,
        /// Display name; the file name when absent.
        name: Option<String>,
    },
    /// Bytes the UI holds (a pasted image).
    Bytes {
        /// Display name.
        name: String,
        /// The UI's claimed MIME type (hint only; the bytes decide).
        media_type: String,
        /// Standard base64 content.
        data_base64: String,
    },
    /// An attachment that is already stored (re-attaching on edit/retry).
    Existing {
        /// Its id.
        attachment_id: String,
    },
}
