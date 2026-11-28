//! Turn-level view-model aggregation for AI Chat (`docs/design.md`, `.overhaul/chat-redesign-spec.md` §3–§6, §10, §13.1).
//!
//! Split into focused submodules:
//! - `grouping`: Turn boundary slicing and `AssistantTurnView` construction,
//! - `explore`: Consecutive file-edit coalescing, `ExploreGroup` merging, path deduplication,
//! - `metrics`: Per-turn file change aggregation, fold statistics, and reasoning/thinking extraction,
//! - `tests`: Unit tests covering turn grouping, explore deduplication/merging, todo filtering,
//!   mid-turn compaction, stable live→saved turn keys, and cache invalidation.

mod explore;
mod grouping;
mod metrics;
#[cfg(test)]
mod tests;

use std::collections::HashMap;
use std::sync::Arc;

use spanbay_core::agent::model::{
    AgentSourceDto, AgentStopReason, AgentToolCallRecord, FileChange, FileChangeKind,
    FileChangeStatus, OutputStream, ReasoningEffort,
};
use spanbay_core::model::hub::{HubCitation, HubPart};

use super::chat_reducer::{ApprovalResolution, ChatThreadState, LiveToolCall};
use super::run_graph::GraphNodeStatus;
use super::tool_trace::ToolTracePresentation;

pub use explore::{is_readonly_explore_tool, is_search_explore_tool};
pub use grouping::{
    build_thread_turn_entries, build_thread_turn_entries_range, compute_saved_call_status,
};
pub use metrics::{extract_inline_thinking_text, extract_message_reasoning_parts};

/// Transcript density mode (`⌃O` / header menu).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum TranscriptDensity {
    Compact,
    #[default]
    Normal,
    Verbose,
}

impl TranscriptDensity {
    /// Cycles `Normal -> Verbose -> Compact -> Normal`.
    #[must_use]
    pub const fn next(self) -> Self {
        match self {
            Self::Compact => Self::Normal,
            Self::Normal => Self::Verbose,
            Self::Verbose => Self::Compact,
        }
    }
}

/// Top-level item in the virtualized thread list after turn grouping.
#[derive(Debug, Clone, PartialEq)]
pub enum ThreadTurnEntry {
    /// User message that starts a turn (`msg_index` in `ChatThreadState::messages`).
    UserTurn { msg_index: usize },
    /// Context compaction summary divider before any user turn (`msg_index` in `ChatThreadState::messages`).
    SummaryDivider { msg_index: usize },
    /// Cohesive assistant turn combining all assistant steps, tool results, mid-turn steers,
    /// mid-turn compactions, and optional live streaming tail for one user turn.
    AssistantTurn(Arc<AssistantTurnView>),
}

/// Cohesive view-model for one assistant turn (1..=N model steps + optional live streaming step).
#[derive(Debug, Clone, PartialEq)]
pub struct AssistantTurnView {
    /// Stable identifier keyed on the initiating user message (`turn-{user_id}`), or `"turn-root"`
    /// when no user message precedes the turn, so live→saved transitions keep fold state intact.
    pub turn_id: String,
    /// Stable sequence key for the virtualized list row (`user.seq`, or `0` for a userless turn)
    /// so live→saved transitions never re-splice the row.
    pub turn_key_seq: i64,
    /// User message id that initiated this turn, if any.
    pub user_message_id: Option<String>,
    /// First assistant message id in this turn (used for Retry).
    pub first_assistant_id: Option<String>,
    /// Last assistant message id in this turn (used for Fork / Delete).
    pub last_assistant_id: Option<String>,
    /// Run id associated with this turn.
    pub run_id: Option<String>,
    /// Indices into `ChatThreadState::messages` covered by this turn.
    pub message_indices: Vec<usize>,
    /// Distinct models used in order of appearance (e.g. `["deepseek-v4-pro", "kimi-k2.6"]`).
    pub models_used: Vec<String>,
    /// Reasoning effort configured for the turn, if any.
    pub reasoning_effort: Option<ReasoningEffort>,
    /// True when this turn is the latest turn in the thread.
    pub is_latest_turn: bool,
    /// True when the turn includes active live streaming or pending tool execution.
    pub is_live: bool,
    /// Ordered activity spine items.
    pub spine: Vec<TurnActivityItem>,
    /// Final answer Markdown rendered outside the collapsible spine.
    pub final_markdown: String,
    /// True when `final_markdown` is actively streaming from `streaming_text`.
    pub final_is_streaming: bool,
    /// Stored sources on the turn's messages.
    pub sources: Vec<AgentSourceDto>,
    /// Deduplicated citations across the turn.
    pub citations: Vec<HubCitation>,
    /// Aggregated file changes across the turn, deduplicated by path in first-seen order.
    pub turn_changes: Vec<TurnFileChangeSummary>,
    /// Untracked file paths modified outside file tools during this turn, if reported by notice.
    pub untracked_paths: Vec<String>,
    /// Aggregated token, timing, and cost metrics across all steps in this turn.
    pub totals: TurnMetricsSummary,
    /// Inline error message attached to a message in this turn, if any.
    pub inline_error: Option<String>,
    /// Non-normal stop reason (e.g. `MaxIterations`, `CostLimit`), if any.
    pub stop_reason: Option<AgentStopReason>,
    /// Whether any tool call in this turn failed or exited non-zero (forces spine & row expansion).
    pub has_failure: bool,
    /// Summary counts for the top-level collapsed turn fold bar.
    pub fold_stats: TurnFoldStats,
    /// Default collapsed state of the activity spine before user manual overrides.
    pub auto_collapsed: bool,
}

/// Counts used to render the 1-line collapsed turn summary bar.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnFoldStats {
    pub duration_ms: u64,
    pub total_tools: usize,
    pub edited_files: usize,
    pub added_lines: u32,
    pub removed_lines: u32,
    pub explored_files: usize,
    pub searches: usize,
    pub shell_commands: usize,
}

/// One node along the vertical activity spine of an [`AssistantTurnView`].
#[derive(Debug, Clone, PartialEq)]
pub enum TurnActivityItem {
    /// Collapsible thinking row (`▸ Thought for 1.4s` / `◜ Thinking…`).
    Reasoning {
        key: String,
        text: String,
        duration_ms: Option<u64>,
        is_live: bool,
        sealed: bool,
    },
    /// Consecutive read-only exploration calls merged into a single expandable group.
    ExploreGroup(ExploreGroupView),
    /// Single tool call row (file edit, shell command, API request, MCP call, failed `todo_write`, etc.).
    ToolCall(Box<TurnToolCallItem>),
    /// Single or parallel `task` sub-agent group.
    SubagentGroup {
        group_key: String,
        calls: Vec<LiveToolCall>,
    },
    /// `submit_plan` marker card in the stream.
    PlanBlock {
        call_id: String,
        args_json: String,
        result_parts: Vec<HubPart>,
        status: GraphNodeStatus,
        actively_revising: bool,
        next_user_text: Option<String>,
    },
    /// Intermediate assistant prose emitted before later tool calls or final answer.
    IntermediateText { key: String, markdown: String },
    /// Mid-turn user steering note (`↳ Steered current turn: "…"`).
    SteerNote { message_id: String, text: String },
    /// Mid-turn context compaction divider (`已压缩上下文摘要`).
    CompactionDivider { key: String },
}

/// Merged group of ≥2 consecutive read-only exploration tool calls.
#[derive(Debug, Clone, PartialEq)]
pub struct ExploreGroupView {
    /// Stable toggle key (e.g. `"exp-{first_call_id}"`).
    pub key: String,
    pub running: bool,
    /// Distinct files/resources read or listed in this group (deduplicated by path).
    pub files_read: usize,
    pub searches_run: usize,
    pub duration_ms: Option<u64>,
    pub items: Vec<TurnToolCallItem>,
}

/// Complete presentation and state of a single tool call inside the turn spine.
#[derive(Debug, Clone, PartialEq)]
pub struct TurnToolCallItem {
    pub call_id: String,
    pub tool_name: String,
    /// Number of consecutive edits to the same file (or coalesced reads) merged into this row (`>= 1`).
    pub edit_count: usize,
    pub pres: ToolTracePresentation,
    pub status: GraphNodeStatus,
    pub is_waiting_approval: bool,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub spilled: bool,
    /// `(change_id, path)` when matched to a persisted or live file change.
    pub change_ref: Option<(String, String)>,
    pub resolution: Option<ApprovalResolution>,
    pub audit: Option<AgentToolCallRecord>,
    pub raw_result_first_line: Option<String>,
    pub request_link: Option<(Option<String>, String)>,
    pub streams: Vec<(OutputStream, String)>,
    pub result_parts: Vec<HubPart>,
}

impl TurnToolCallItem {
    /// Returns true if this tool call failed, was blocked, or exited with a non-zero status code.
    #[must_use]
    pub fn should_auto_expand(&self) -> bool {
        self.status == GraphNodeStatus::Failed
            || self.status == GraphNodeStatus::Blocked
            || self.exit_code.is_some_and(|c| c != 0)
    }
}

/// Per-file net change summary for the post-turn Changes Summary Card.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct TurnFileChangeSummary {
    pub path: String,
    pub kind: FileChangeKind,
    pub status: FileChangeStatus,
    pub added: u32,
    pub removed: u32,
    pub change_ids: Vec<String>,
    pub edit_count: usize,
}

/// Aggregated metrics across all model steps and tool calls in one [`AssistantTurnView`].
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct TurnMetricsSummary {
    pub step_count: usize,
    pub visible_tool_count: usize,
    pub hidden_todo_count: usize,
    pub input_tokens: u64,
    pub output_tokens: u64,
    pub reasoning_tokens: u64,
    pub cached_input_tokens: u64,
    pub peak_context_tokens: u64,
    pub context_limit_tokens: Option<u64>,
    pub ttft_ms: Option<u64>,
    pub stream_ms: u64,
    pub duration_ms: u64,
    pub cost_micro_usd: i64,
    pub cost_currency: Option<String>,
    pub sealed_reasoning: bool,
}

impl TurnMetricsSummary {
    /// Total tokens (`input + output`) for the compact hover pill.
    #[must_use]
    pub const fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }

    /// Output generation speed in tokens/sec when `output_tokens > 0` and `stream_ms > 0`.
    #[must_use]
    pub fn tokens_per_sec(&self) -> Option<f64> {
        if self.output_tokens > 0 && self.stream_ms > 0 {
            Some(self.output_tokens as f64 / (self.stream_ms as f64 / 1_000.0))
        } else {
            None
        }
    }
}

/// O(1) version key for [`TurnViewCache`].
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct TurnViewCacheKey {
    thread_version: u64,
    turn_inputs_version: u64,
    run_active: bool,
    density: TranscriptDensity,
}

/// Key for cached older turns preceding the last turn-boundary user message.
#[derive(Debug, Clone, PartialEq, Eq)]
struct PrefixCacheKey {
    session_id: String,
    messages_version: u64,
    last_boundary_idx: usize,
    turn_inputs_version: u64,
    density: TranscriptDensity,
}
/// Incremental version-keyed cache over [`build_thread_turn_entries`].
///
/// Caches completed turns before the latest turn boundary (`prefix_entries`) as `Arc` entries so
/// 30 Hz streaming batches only rebuild the active tail turn in O(1) relative to history length.
#[derive(Debug, Clone, Default)]
pub struct TurnViewCache {
    key: Option<TurnViewCacheKey>,
    prefix_key: Option<PrefixCacheKey>,
    prefix_entries: Vec<ThreadTurnEntry>,
    entries: Arc<[ThreadTurnEntry]>,
}

impl TurnViewCache {
    /// Clears the cached entries (e.g. on session reset).
    pub fn clear(&mut self) {
        self.key = None;
        self.prefix_key = None;
        self.prefix_entries.clear();
        self.entries = Arc::from([]);
    }

    /// Returns the cached [`ThreadTurnEntry`] slice if `(thread.version, turn_inputs_version, run_active, density)`
    /// matches, or incrementally rebuilds only the tail turn when older turns are unchanged.
    #[allow(clippy::too_many_arguments)]
    pub fn get_or_build(
        &mut self,
        thread: &ChatThreadState,
        turn_inputs_version: u64,
        audit_tool_calls: &[AgentToolCallRecord],
        persisted_changes: &[FileChange],
        thinking_durations: &HashMap<String, u64>,
        session_ws_id: Option<&str>,
        run_active: bool,
        density: TranscriptDensity,
    ) -> Arc<[ThreadTurnEntry]> {
        let key = TurnViewCacheKey {
            thread_version: thread.version,
            turn_inputs_version,
            run_active,
            density,
        };
        if self.key == Some(key) {
            return Arc::clone(&self.entries);
        }

        let last_boundary_idx = thread
            .messages
            .iter()
            .rposition(spanbay_core::agent::model::AgentMessage::is_turn_boundary)
            .unwrap_or(0);

        let built = if last_boundary_idx > 0 {
            let pkey = PrefixCacheKey {
                session_id: thread.session_id.clone(),
                messages_version: thread.messages_version,
                last_boundary_idx,
                turn_inputs_version,
                density,
            };
            if self.prefix_key.as_ref() != Some(&pkey) {
                self.prefix_entries = build_thread_turn_entries_range(
                    thread,
                    0..last_boundary_idx,
                    false,
                    audit_tool_calls,
                    persisted_changes,
                    thinking_durations,
                    session_ws_id,
                    run_active,
                    density,
                );
                self.prefix_key = Some(pkey);
            }
            let mut tail = build_thread_turn_entries_range(
                thread,
                last_boundary_idx..thread.messages.len(),
                true,
                audit_tool_calls,
                persisted_changes,
                thinking_durations,
                session_ws_id,
                run_active,
                density,
            );
            let mut combined = Vec::with_capacity(self.prefix_entries.len() + tail.len());
            combined.extend(self.prefix_entries.iter().cloned());
            combined.append(&mut tail);
            grouping::finalize_latest_turn_collapse(&mut combined, density);
            combined
        } else {
            self.prefix_key = None;
            self.prefix_entries.clear();
            build_thread_turn_entries(
                thread,
                audit_tool_calls,
                persisted_changes,
                thinking_durations,
                session_ws_id,
                run_active,
                density,
            )
        };

        self.entries = Arc::from(built);
        self.key = Some(key);
        Arc::clone(&self.entries)
    }
}
