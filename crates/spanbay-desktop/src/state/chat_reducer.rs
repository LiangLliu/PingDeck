//! Pure-Rust headless reducer for Chat / Coding Agent streaming runs (`AgentEnvelope`).
//!
//! Consumes coalesced [`AgentEnvelope`] events under the per-run sequence rule of
//! `docs/architecture.md` §12 (dedup, gap detection, buffered resync against a fresh snapshot),
//! producing render-ready state for any UI shell (unit-testable without GPU or window).

use std::collections::{BTreeMap, BTreeSet, VecDeque};

use base64::Engine as _;
use spanbay_core::agent::model::{
    AgentEnvelope, AgentEvent, AgentFailure, AgentMessage, AgentRole, AgentRunKind, AgentRunStatus,
    AgentRunSummary, AgentStopReason, AgentTarget, AgentTodoItem, ApprovalDecision, ApprovalScope,
    CostShare, HumanGateDto, LiveApprovalRequest, LiveCostLimit, LiveRunSnapshot, LiveToolState,
    OutputStream, QueuedTurn, SkillUndelivered, ToolCallStatus, ToolSource,
};
use spanbay_core::agent::tools::{FileChangeKind, ToolResultView};
use spanbay_core::model::hub::{HubCitation, HubImage, HubPart, HubUsage};

/// Bytes of live output kept per tool call (the head is trimmed) to keep rendering O(1).
const MAX_LIVE_TOOL_OUTPUT_BYTES: usize = 64 * 1024;
/// Placeholder written into freshly saved `AgentRole::Tool` rows before each call settles.
const PENDING_TOOL_RESULT_MARKER: &str = "[interrupted before tool finished]";
/// Notices kept; older ones drop first.
const MAX_NOTICES: usize = 20;
/// Envelopes held while a resync is open; past it the buffer is dropped and another snapshot read.
const RESYNC_BUFFER_MAX: usize = 1024;
/// Scopes an approval offers when the core sent no card.
const DEFAULT_SCOPES: [ApprovalScope; 2] = [ApprovalScope::Once, ApprovalScope::Session];

/// Completed tool execution metadata captured from live `AgentEvent::ToolResult` events before
/// the post-turn database refresh reloads `agent_tool_calls`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveToolMeta {
    pub status: ToolCallStatus,
    pub duration_ms: u64,
    pub exit_code: Option<i32>,
    /// The result's link-out (`ToolResult.meta.view`); live only, not stored.
    pub link: Option<ToolResultView>,
}

/// How the user (or the timeout) answered a call's approval.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub struct ApprovalResolution {
    pub decision: ApprovalDecision,
    pub scope: ApprovalScope,
    pub timed_out: bool,
}

/// Live tool call state inside an active run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveToolCall {
    pub call_id: String,
    pub name: String,
    pub source: ToolSource,
    pub arguments: String,
    /// Streamed output chunks (`ToolOutputDelta`) in arrival order; adjacent same-stream fragments
    /// are merged, the total is trimmed from the head to `MAX_LIVE_TOOL_OUTPUT_BYTES`.
    pub output: Vec<(OutputStream, String)>,
    /// Final structured `ToolResult` parts kept separately from streamed `output` so formatters
    /// (inline metrics, error copy, `<external_data>` stripping, previews) still apply mid-run.
    pub result_parts: Vec<HubPart>,
    pub status: Option<ToolCallStatus>,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
    pub resolution: Option<ApprovalResolution>,
}

/// One approval card; built only by [`LiveApproval::new`] (event and hydrate paths share it).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveApproval {
    /// The run that answers it (a sub-agent's run for a sub-agent call).
    pub run_id: String,
    pub call_id: String,
    pub name: String,
    pub request: LiveApprovalRequest,
}

impl LiveApproval {
    #[must_use]
    pub fn new(
        run_id: String,
        call_id: String,
        name: String,
        request: LiveApprovalRequest,
    ) -> Self {
        Self {
            run_id,
            call_id,
            name,
            request,
        }
    }

    /// The card's scopes, or Once and Session without a card.
    #[must_use]
    pub fn offered_scopes(&self) -> &[ApprovalScope] {
        self.request
            .card
            .as_ref()
            .map_or(&DEFAULT_SCOPES[..], |c| &c.offered_scopes)
    }

    /// When the call started waiting for approval, in Unix milliseconds.
    #[must_use]
    pub const fn requested_at_ms(&self) -> i64 {
        self.request.requested_at_ms
    }
}

/// Summary of one file changed during a run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveFileChange {
    pub call_id: String,
    pub path: String,
    pub change: FileChangeKind,
    pub added: u32,
    pub removed: u32,
    pub change_id: Option<String>,
}

/// Active cost/token pause waiting for `agent_run_continue_impl` (mirror of `LiveCostLimit`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveCostPause {
    pub run_id: String,
    pub cost_micro: i64,
    pub cap_micro: i64,
    pub currency: String,
    pub is_tokens: bool,
    pub breakdown: Vec<CostShare>,
    pub next_cap_micro: Option<i64>,
    pub unpriced_models: Vec<String>,
}

impl From<LiveCostLimit> for LiveCostPause {
    fn from(c: LiveCostLimit) -> Self {
        Self {
            run_id: c.run_id,
            cost_micro: c.cost_micro,
            cap_micro: c.cap_micro,
            currency: c.currency,
            is_tokens: c.tokens.unwrap_or(false),
            breakdown: c.breakdown.unwrap_or_default(),
            next_cap_micro: c.next_cap_micro,
            unpriced_models: c.unpriced_models.unwrap_or_default(),
        }
    }
}

/// Compact tool step record observed inside a live sub-agent stream.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveSubagentTool {
    pub call_id: String,
    pub name: String,
    pub status: Option<ToolCallStatus>,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
}

/// Live sub-agent progress nested under a parent `task` tool call. The core forwards only its
/// steps, approvals, todos and notices; the full stream belongs to the child chat.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveSubagent {
    pub parent_call_id: String,
    pub child_run_id: String,
    pub child_session_id: String,
    pub description: String,
    pub subagent_kind: String,
    pub resumed: bool,
    pub summary: AgentRunSummary,
    pub tool_steps: Vec<LiveSubagentTool>,
    pub todos: Vec<AgentTodoItem>,
    pub finished: bool,
}

impl LiveSubagent {
    /// The newest step still running.
    #[must_use]
    pub fn current_tool(&self) -> Option<&str> {
        self.tool_steps
            .iter()
            .rev()
            .find(|t| t.status.is_none())
            .map(|t| t.name.as_str())
    }
}

/// A localisable note: the view renders `code` + `args`, falling back to `text`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ChatNotice {
    pub code: Option<String>,
    pub args: BTreeMap<String, String>,
    pub text: String,
}

/// The run's failure as the banner shows it; a cancellation leaves none.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunError {
    pub message: String,
    pub failure: Option<AgentFailure>,
}

/// Live run meter; session totals are read from `AgentSession`, never mixed in here.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct RunMeter {
    pub usage: HubUsage,
    pub cost_micro: i64,
    pub currency: Option<String>,
}

/// What a batch changed, for the view's timers and scroll.
#[derive(Debug, Clone, Default, PartialEq, Eq)]
pub struct BatchEffects {
    pub changed: bool,
    /// Run id of a `RunStarted`.
    pub run_started: Option<String>,
    pub user_message: bool,
    /// `(session_id, title)` of a `SessionTitleUpdated`.
    pub title: Option<(String, String)>,
    /// A top-level `Done` or `Error`.
    pub finished: bool,
    pub scroll_worthy: bool,
    /// The first reasoning, text or tool call of a model step (thinking timer).
    pub first_content: bool,
    /// Ids of assistant messages stored by `MessageSaved`.
    pub saved_assistant_ids: Vec<String>,
    /// A sequence gap opened a resync: read a fresh `AgentSessionDetail` and call
    /// [`ChatThreadState::finish_resync`].
    pub needs_resync: bool,
}

/// Complete render-ready thread state.
#[derive(Debug, Clone, Default)]
pub struct ChatThreadState {
    pub session_id: String,
    pub title: String,
    pub messages: Vec<AgentMessage>,
    pub active_run: Option<AgentRunSummary>,
    pub last_stop_reason: Option<AgentStopReason>,
    /// The failure of the last run; `None` for a cancellation.
    pub last_error: Option<RunError>,
    pub streaming_text: String,
    pub streaming_reasoning: String,
    pub tool_calls: Vec<LiveToolCall>,
    /// Tool call ids whose saved `AgentRole::Tool` placeholder has not yet received a live
    /// `AgentEvent::ToolResult` (still running during an active multi-step run).
    pub pending_tool_calls: BTreeSet<String>,
    /// Live duration/status/exit_code recorded as `AgentEvent::ToolResult` arrives mid-run.
    pub live_tool_meta: BTreeMap<String, LiveToolMeta>,
    pub pending_approvals: Vec<LiveApproval>,
    pub open_gates: Vec<(String, HumanGateDto)>,
    pub cost_pause: Option<LiveCostPause>,
    pub file_changes: Vec<LiveFileChange>,
    pub todos: Vec<AgentTodoItem>,
    pub queued_turns: Vec<QueuedTurn>,
    pub queue_paused: bool,
    pub subagents: BTreeMap<String, LiveSubagent>,
    pub notices: Vec<ChatNotice>,
    pub context_used_tokens: u64,
    pub context_limit_tokens: Option<u64>,
    pub run_meter: Option<RunMeter>,
    /// MCP servers the run is connecting to; cleared by its first content or its end.
    pub mcp_connecting: Vec<String>,
    /// `(before_tokens, after_tokens)` of live compactions until the stored summary takes over.
    pub live_compactions: Vec<(u64, u64)>,
    /// Sources cited in the current step, deduplicated by URL.
    pub live_citations: Vec<HubCitation>,
    /// Skills that did not reach the model, by user message id (this process only).
    pub skills_undelivered: BTreeMap<String, Vec<SkillUndelivered>>,
    /// Set by the view once subscription retries are spent; cleared when one succeeds.
    pub stream_degraded: bool,
    /// Monotonic version counter bumped on every applied event, snapshot, or hydration.
    pub version: u64,
    /// Monotonic counter bumped on every mutation of `messages` (never on pure streaming deltas).
    pub messages_version: u64,
    /// Last applied sequence per run id.
    last_seq_by_run: BTreeMap<String, u64>,
    /// The current model step showed content already.
    step_has_content: bool,
    /// A gap is waiting for a fresh snapshot; envelopes go to `resync_buffer`.
    resync_open: bool,
    resync_buffer: VecDeque<AgentEnvelope>,
    resync_overflowed: bool,
}

/// Whether an envelope applies, repeats, or leaves a hole.
enum SeqCheck {
    Apply,
    Duplicate,
    Gap,
}

impl ChatThreadState {
    #[must_use]
    pub fn new(session_id: impl Into<String>, title: impl Into<String>) -> Self {
        Self {
            session_id: session_id.into(),
            title: title.into(),
            ..Self::default()
        }
    }

    /// Replaces persisted message history from a fresh database snapshot.
    pub fn replace_snapshot(&mut self, title: String, messages: Vec<AgentMessage>) {
        self.title = title;
        self.messages = messages;
        self.pending_tool_calls.clear();
        self.live_tool_meta.clear();
        self.file_changes.clear();
        self.live_compactions.clear();
        self.skills_undelivered.clear();
        self.version = self.version.wrapping_add(1);
        self.messages_version = self.messages_version.wrapping_add(1);
    }

    /// Prepends an older page of messages before the currently loaded slice, skipping duplicates
    /// by message id so earlier history loads idempotently.
    pub fn prepend_earlier_messages(&mut self, earlier: Vec<AgentMessage>) {
        if earlier.is_empty() {
            return;
        }
        let existing_ids: std::collections::HashSet<&str> =
            self.messages.iter().map(|m| m.id.as_str()).collect();
        let mut prefix: Vec<AgentMessage> = earlier
            .into_iter()
            .filter(|m| !existing_ids.contains(m.id.as_str()))
            .collect();
        if prefix.is_empty() {
            return;
        }
        prefix.append(&mut self.messages);
        self.messages = prefix;
        self.version = self.version.wrapping_add(1);
        self.messages_version = self.messages_version.wrapping_add(1);
    }

    /// Merges the newest page snapshot without discarding earlier pages that the user
    /// previously loaded into `self.messages`.
    pub fn merge_latest_snapshot(&mut self, title: String, latest: Vec<AgentMessage>) {
        self.title = title;
        self.live_compactions.clear();
        let Some(first_latest) = latest.first() else {
            self.messages.clear();
            self.version = self.version.wrapping_add(1);
            self.messages_version = self.messages_version.wrapping_add(1);
            return;
        };
        // Keep earlier loaded pages only when they still belong to the same branch as `latest`.
        // When the first message was edited/branched (new root or orphaned parent), drop the
        // abandoned branch instead of retaining rows by session-global `seq`.
        let keep_len = if let Some(pos) = self.messages.iter().position(|m| m.id == first_latest.id)
        {
            pos
        } else if let Some(parent_id) = first_latest.parent_id.as_deref() {
            self.messages
                .iter()
                .position(|m| m.id == parent_id)
                .map_or(0, |pos| pos + 1)
        } else {
            // Fallback for snapshots whose messages omit `parent_id` (e.g. unit-test fixtures)
            // only when all existing messages lack `parent_id` and precede `first_latest.seq`
            // without an abandoned root replacement.
            let has_tree_links = self.messages.iter().any(|m| m.parent_id.is_some());
            if has_tree_links {
                0
            } else {
                self.messages
                    .iter()
                    .take_while(|m| m.seq < first_latest.seq)
                    .count()
            }
        };
        let mut merged: Vec<AgentMessage> = self.messages.iter().take(keep_len).cloned().collect();
        merged.extend(latest);
        self.messages = merged;
        self.version = self.version.wrapping_add(1);
        self.messages_version = self.messages_version.wrapping_add(1);
    }

    /// Hydrates live unfinished stream parts, tool calls, pending approvals, open gates, cost
    /// pause and meter from an atomic `LiveRunSnapshot`; the run's baseline becomes `live.seq`.
    pub fn hydrate_live(&mut self, live: &LiveRunSnapshot) {
        self.last_seq_by_run.insert(live.run_id.clone(), live.seq);
        self.streaming_text.clear();
        self.streaming_reasoning.clear();
        self.tool_calls.clear();
        self.pending_tool_calls.clear();
        self.pending_approvals.clear();
        self.live_citations.clear();
        self.mcp_connecting.clear();

        // A `partial: true` assistant row of the running turn is superseded by `live.parts`
        // (model contract: `LiveRunSnapshot`); drop it so the reply is never shown twice.
        if !live.parts.is_empty() {
            let prev_len = self.messages.len();
            self.messages.retain(|m| {
                !(m.role == AgentRole::Assistant
                    && m.partial
                    && m.run_id.as_deref() == Some(live.run_id.as_str()))
            });
            if self.messages.len() != prev_len {
                self.messages_version = self.messages_version.wrapping_add(1);
            }
        }

        let mut args_by_id: BTreeMap<&str, &str> = BTreeMap::new();
        let mut has_unsaved_text_or_reasoning = false;
        for part in &live.parts {
            match part {
                HubPart::Text {
                    text, citations, ..
                } => {
                    if !text.is_empty() {
                        has_unsaved_text_or_reasoning = true;
                    }
                    self.streaming_text.push_str(text);
                    for citation in citations {
                        if !self.live_citations.iter().any(|c| c.url == citation.url) {
                            self.live_citations.push(citation.clone());
                        }
                    }
                }
                HubPart::Reasoning { text, .. } => {
                    if !text.is_empty() {
                        has_unsaved_text_or_reasoning = true;
                    }
                    self.streaming_reasoning.push_str(text);
                }
                HubPart::ToolCall { id, arguments, .. } => {
                    args_by_id.insert(id.as_str(), arguments.as_str());
                }
                _ => {}
            }
        }
        let has_streaming_tool_args = live
            .tools
            .iter()
            .any(|t| t.state == LiveToolState::StreamingArgs);
        self.step_has_content = has_unsaved_text_or_reasoning
            || !self.live_citations.is_empty()
            || has_streaming_tool_args;

        let mut active_child_runs: BTreeSet<&str> = BTreeSet::new();
        for tc in &live.tools {
            if tc.state == LiveToolState::WaitingApproval
                && let Some(request) = &tc.approval
            {
                // A child's approval is answered under the child run's id.
                let run_id = tc
                    .child_run_id
                    .clone()
                    .unwrap_or_else(|| live.run_id.clone());
                self.pending_approvals.push(LiveApproval::new(
                    run_id,
                    tc.call_id.clone(),
                    tc.name.clone(),
                    (**request).clone(),
                ));
            }
            if let Some(child_id) = tc.child_run_id.as_deref() {
                active_child_runs.insert(child_id);
            }
            // A sub-agent's waiting call has no part of the parent's turn: card only, no row.
            let Some(args) = args_by_id.get(tc.call_id.as_str()) else {
                continue;
            };
            let result = tc.result.as_ref();
            self.tool_calls.push(LiveToolCall {
                call_id: tc.call_id.clone(),
                name: tc.name.clone(),
                source: tc.source.clone(),
                arguments: (*args).to_owned(),
                output: Vec::new(),
                result_parts: result
                    .filter(|r| !r.summary.is_empty())
                    .map(|r| vec![HubPart::text(r.summary.clone())])
                    .unwrap_or_default(),
                status: result.map(|r| r.status),
                duration_ms: result.map(|r| r.duration_ms),
                exit_code: result.and_then(|r| r.exit_code),
                resolution: None,
            });
            match result {
                Some(res) => {
                    self.live_tool_meta.insert(
                        tc.call_id.clone(),
                        LiveToolMeta {
                            status: res.status,
                            duration_ms: res.duration_ms,
                            exit_code: res.exit_code,
                            link: None,
                        },
                    );
                }
                None => {
                    self.pending_tool_calls.insert(tc.call_id.clone());
                }
            }
        }
        // Drop sub-agents that no longer belong to the hydrated live step.
        self.subagents
            .retain(|child_id, _| active_child_runs.contains(child_id.as_str()));
        self.open_gates = live
            .gates
            .iter()
            .cloned()
            .map(|g| (live.run_id.clone(), g))
            .collect();
        self.cost_pause = live.cost_limit.clone().map(LiveCostPause::from);
        if let Some(u) = &live.usage {
            self.run_meter = Some(RunMeter {
                usage: u.run_usage.clone(),
                cost_micro: u.cost_micro_usd,
                currency: u.currency.clone(),
            });
            self.context_used_tokens = u.context_used_tokens;
            self.context_limit_tokens = u.context_limit_tokens;
        }
        // Keep `active_run` alive for Stop/Steer/queueing when hydrating a running chat without
        // having seen `RunStarted` (e.g. switching back to a running session or resyncing).
        let live_status = if !self.pending_approvals.is_empty()
            || !self.open_gates.is_empty()
            || self.cost_pause.is_some()
        {
            AgentRunStatus::WaitingApproval
        } else {
            AgentRunStatus::Running
        };
        match self.active_run.as_mut() {
            Some(run) if run.run_id == live.run_id => {
                run.status = live_status;
                run.last_seq = live.seq;
            }
            _ => {
                self.active_run = Some(provisional_run_summary(
                    live.run_id.clone(),
                    self.session_id.clone(),
                    live_status,
                    None,
                    self.run_meter.as_ref(),
                    live.seq,
                ));
            }
        }
        self.version = self.version.wrapping_add(1);
    }

    /// Last sequence number received for a given run.
    #[must_use]
    pub fn last_seq(&self, run_id: &str) -> Option<u64> {
        self.last_seq_by_run.get(run_id).copied()
    }

    /// `from_seq` for `agent_run_subscribe_impl` right after a snapshot: the live run's seq,
    /// else 0 (so an evicted replay announces a `Gap` instead of failing).
    #[must_use]
    pub fn subscribe_from(live: Option<&LiveRunSnapshot>) -> Option<u64> {
        Some(live.map_or(0, |l| l.seq))
    }

    /// Applies a batch; enforces the per-run sequence rule and buffers while a resync is open.
    pub fn apply_batch(&mut self, batch: impl IntoIterator<Item = AgentEnvelope>) -> BatchEffects {
        let mut fx = BatchEffects::default();
        for envelope in batch {
            self.accept(envelope, &mut fx);
        }
        fx
    }

    /// Marks a view-initiated resync as open so envelopes arriving while the snapshot is in flight
    /// are buffered and replayed on top of the snapshot instead of applied beforehand and
    /// overwritten by an older snapshot.
    pub fn begin_resync(&mut self) {
        self.resync_open = true;
    }

    /// The view read a fresh `AgentSessionDetail`: replaces history (keeping earlier pages),
    /// hydrates `live` (or ends the stale live state when nothing runs), then replays the
    /// buffered envelopes. Returns the replay's effects; `needs_resync` asks for another snapshot
    /// round (the buffer overflowed, or the replay hit another gap).
    pub fn finish_resync(
        &mut self,
        title: String,
        messages: Vec<AgentMessage>,
        live: Option<&LiveRunSnapshot>,
    ) -> BatchEffects {
        let mut fx = BatchEffects {
            changed: true,
            ..BatchEffects::default()
        };
        self.merge_latest_snapshot(title, messages);
        match live {
            Some(live) => self.hydrate_live(live),
            None => {
                self.end_run();
                if self
                    .active_run
                    .as_ref()
                    .is_some_and(|r| r.status.is_active())
                {
                    self.active_run = None;
                }
                self.version = self.version.wrapping_add(1);
            }
        }
        // Only the hydrated run has a trustworthy live baseline; any previously-seen run that is
        // no longer live already finished into persisted `messages`, so seal its baseline to
        // `u64::MAX` to drop stale pre-snapshot buffered deltas instead of replaying them as a
        // fresh active turn.
        let live_run_id = live.map(|l| l.run_id.as_str());
        for (run_id, seq) in &mut self.last_seq_by_run {
            if live_run_id != Some(run_id.as_str()) {
                *seq = u64::MAX;
            }
        }
        if std::mem::take(&mut self.resync_overflowed) {
            // Stay open and discard post-overflow buffered envelopes: what was dropped before the
            // overflow may be newer than this snapshot.
            self.resync_buffer.clear();
            fx.needs_resync = true;
            return fx;
        }
        self.resync_open = false;
        let buffered = std::mem::take(&mut self.resync_buffer);
        for envelope in buffered {
            self.accept(envelope, &mut fx);
        }
        fx
    }

    fn accept(&mut self, envelope: AgentEnvelope, fx: &mut BatchEffects) {
        if self.resync_open {
            self.buffer(envelope);
            return;
        }
        if let AgentEvent::Gap { first_seq } = envelope.event {
            let baseline = first_seq.saturating_sub(1);
            if self
                .last_seq_by_run
                .get(&envelope.run_id)
                .is_some_and(|&seen| seen >= baseline)
            {
                // Snapshot or prior envelopes already advanced past this gap announcement.
                return;
            }
            self.last_seq_by_run.insert(envelope.run_id, baseline);
            self.open_resync(fx);
            return;
        }
        match self.check_seq(&envelope) {
            SeqCheck::Duplicate => {}
            SeqCheck::Gap => {
                self.open_resync(fx);
                self.buffer(envelope);
            }
            SeqCheck::Apply => {
                let run_id = envelope.run_id.clone();
                self.last_seq_by_run.insert(run_id.clone(), envelope.seq);
                fx.changed = true;
                self.version = self.version.wrapping_add(1);
                self.apply_event(&run_id, envelope.event, envelope.ts_ms, fx);
            }
        }
    }

    fn check_seq(&self, envelope: &AgentEnvelope) -> SeqCheck {
        // An unknown run starts its baseline here: a finished run is in the stored messages and
        // an active one was hydrated.
        let Some(&seen) = self.last_seq_by_run.get(&envelope.run_id) else {
            return SeqCheck::Apply;
        };
        if envelope.seq <= seen {
            SeqCheck::Duplicate
        } else if envelope.first_seq <= seen || envelope.first_seq > seen + 1 {
            // A merged envelope straddling the baseline would repeat seen deltas; a skip lost some.
            SeqCheck::Gap
        } else {
            SeqCheck::Apply
        }
    }

    fn open_resync(&mut self, fx: &mut BatchEffects) {
        self.resync_open = true;
        fx.needs_resync = true;
    }

    fn buffer(&mut self, envelope: AgentEnvelope) {
        self.resync_buffer.push_back(envelope);
        if self.resync_buffer.len() > RESYNC_BUFFER_MAX {
            self.resync_buffer.clear();
            self.resync_overflowed = true;
        }
    }

    fn content(&mut self, fx: &mut BatchEffects) {
        fx.scroll_worthy = true;
        self.mcp_connecting.clear();
        if self.cost_pause.as_ref().is_some_and(|p| {
            self.active_run
                .as_ref()
                .is_none_or(|r| r.run_id == p.run_id)
        }) {
            self.cost_pause = None;
        }
        if !self.step_has_content {
            self.step_has_content = true;
            fx.first_content = true;
        }
    }

    fn push_approval(&mut self, event: &AgentEvent, ts_ms: i64) {
        let AgentEvent::ToolApprovalRequired {
            run_id,
            call_id,
            name,
            ..
        } = event
        else {
            return;
        };
        let Some(request) = LiveApprovalRequest::from_event(event, ts_ms) else {
            return;
        };
        self.pending_approvals
            .retain(|a| !(a.run_id == *run_id && a.call_id == *call_id));
        self.pending_approvals.push(LiveApproval::new(
            run_id.clone(),
            call_id.clone(),
            name.clone(),
            request,
        ));
    }

    fn push_notice(
        &mut self,
        text: String,
        code: Option<String>,
        args: Option<BTreeMap<String, String>>,
    ) {
        self.notices.push(ChatNotice {
            code,
            args: args.unwrap_or_default(),
            text,
        });
        if self.notices.len() > MAX_NOTICES {
            self.notices.remove(0);
        }
    }

    /// What a top-level `Done` or `Error` clears.
    fn end_run(&mut self) {
        self.streaming_text.clear();
        self.streaming_reasoning.clear();
        self.tool_calls.clear();
        self.pending_tool_calls.clear();
        self.cost_pause = None;
        self.pending_approvals.clear();
        self.open_gates.clear();
        self.mcp_connecting.clear();
        self.live_citations.clear();
        self.subagents.clear();
    }

    fn apply_event(
        &mut self,
        envelope_run_id: &str,
        event: AgentEvent,
        ts_ms: i64,
        fx: &mut BatchEffects,
    ) {
        match event {
            AgentEvent::RunStarted { run } => {
                fx.run_started = Some(run.run_id.clone());
                self.active_run = Some(*run);
                self.last_error = None;
                self.last_stop_reason = None;
                self.cost_pause = None;
                self.run_meter = None;
                self.streaming_text.clear();
                self.streaming_reasoning.clear();
                self.tool_calls.clear();
                self.pending_tool_calls.clear();
                self.live_tool_meta.clear();
                self.file_changes.clear();
                self.notices.clear();
                self.subagents.clear();
                self.live_citations.clear();
                self.step_has_content = false;
            }
            AgentEvent::QueuedUpdated { queued, paused, .. } => {
                self.queued_turns = queued;
                self.queue_paused = paused;
            }
            AgentEvent::UserMessage {
                message,
                skills_undelivered,
            } => {
                fx.user_message = true;
                fx.scroll_worthy = true;
                if !skills_undelivered.is_empty() {
                    self.skills_undelivered
                        .insert(message.id.clone(), skills_undelivered);
                }
                self.apply_user_message(*message);
            }
            AgentEvent::MessageSaved { message } => {
                let msg = *message;
                fx.scroll_worthy = true;
                match msg.role {
                    AgentRole::Tool => {
                        for part in &msg.parts {
                            if let HubPart::ToolResult {
                                call_id,
                                content,
                                is_error: true,
                                ..
                            } = part
                                && !self.live_tool_meta.contains_key(call_id)
                                && is_pending_placeholder_parts(content)
                            {
                                self.pending_tool_calls.insert(call_id.clone());
                            }
                        }
                    }
                    AgentRole::Assistant => {
                        fx.saved_assistant_ids.push(msg.id.clone());
                        self.live_citations.clear();
                        self.step_has_content = false;
                    }
                    _ => {}
                }
                self.push_or_replace_message(msg);
                self.streaming_text.clear();
                self.streaming_reasoning.clear();
            }
            AgentEvent::McpConnecting { server_names } => {
                self.mcp_connecting = server_names;
            }
            AgentEvent::ContextCompacted {
                before_tokens,
                after_tokens,
                ..
            } => {
                self.live_compactions.push((before_tokens, after_tokens));
                fx.scroll_worthy = true;
            }
            AgentEvent::CitationAdded { citation } => {
                if !self.live_citations.iter().any(|c| c.url == citation.url) {
                    self.live_citations.push(citation);
                }
            }
            AgentEvent::TextDelta { text } => {
                self.content(fx);
                self.streaming_text.push_str(&text);
            }
            AgentEvent::ReasoningDelta { text } => {
                self.content(fx);
                self.streaming_reasoning.push_str(&text);
            }
            AgentEvent::ToolCallStarted {
                call_id,
                name,
                source,
            } => {
                self.content(fx);
                if !self.tool_calls.iter().any(|c| c.call_id == call_id) {
                    self.tool_calls.push(LiveToolCall {
                        call_id,
                        name,
                        source,
                        arguments: String::new(),
                        output: Vec::new(),
                        result_parts: Vec::new(),
                        status: None,
                        duration_ms: None,
                        exit_code: None,
                        resolution: None,
                    });
                }
            }
            AgentEvent::ToolCallArgsDelta { call_id, arguments } => {
                if let Some(call) = self.tool_call(&call_id) {
                    call.arguments.push_str(&arguments);
                }
            }
            AgentEvent::ToolOutputDelta {
                call_id,
                stream,
                text,
            } => {
                fx.scroll_worthy = true;
                if let Some(call) = self.tool_call(&call_id) {
                    push_output(&mut call.output, stream, &text);
                }
            }
            event @ AgentEvent::ToolApprovalRequired { .. } => {
                fx.scroll_worthy = true;
                self.push_approval(&event, ts_ms);
            }
            AgentEvent::ToolApprovalResolved {
                call_id,
                decision,
                scope,
                timed_out,
            } => {
                if let Some(call) = self.tool_call(&call_id) {
                    call.resolution = Some(ApprovalResolution {
                        decision,
                        scope,
                        timed_out,
                    });
                }
                self.pending_approvals
                    .retain(|a| !(a.run_id == envelope_run_id && a.call_id == call_id));
            }
            AgentEvent::ToolResult {
                call_id,
                status,
                parts,
                duration_ms,
                meta,
                exit_code,
                ..
            } => {
                fx.scroll_worthy = true;
                self.pending_tool_calls.remove(&call_id);
                self.live_tool_meta.insert(
                    call_id.clone(),
                    LiveToolMeta {
                        status,
                        duration_ms,
                        exit_code,
                        link: meta.and_then(|m| m.view),
                    },
                );
                if let Some(call) = self.tool_call(&call_id) {
                    call.status = Some(status);
                    call.duration_ms = Some(duration_ms);
                    call.exit_code = exit_code;
                    call.result_parts.clone_from(&parts);
                }
                // Replace the crash-recovery placeholder written by `save_tool_turn` in the saved
                // `AgentRole::Tool` message so completed tool steps display their real status,
                // metrics, and output immediately during multi-step runs.
                let is_err = !matches!(status, ToolCallStatus::Ok);
                'patch_msg: for msg in self.messages.iter_mut().rev() {
                    if msg.role != AgentRole::Tool {
                        continue;
                    }
                    for part in &mut msg.parts {
                        if let HubPart::ToolResult {
                            call_id: cid,
                            content,
                            is_error,
                            ..
                        } = part
                            && *cid == call_id
                        {
                            *content = parts;
                            *is_error = is_err;
                            self.messages_version = self.messages_version.wrapping_add(1);
                            break 'patch_msg;
                        }
                    }
                }
            }
            AgentEvent::FileChanged {
                call_id,
                path,
                change,
                added,
                removed,
                change_id,
            } => {
                self.file_changes.push(LiveFileChange {
                    call_id,
                    path,
                    change,
                    added,
                    removed,
                    change_id,
                });
            }
            AgentEvent::TodosUpdated { todos } => {
                self.todos = todos;
            }
            AgentEvent::SubagentStarted {
                parent_call_id,
                child_run,
                child_session_id,
            } => {
                fx.scroll_worthy = true;
                let (description, subagent_kind, resumed) = self
                    .tool_calls
                    .iter()
                    .find(|c| c.call_id == parent_call_id)
                    .map(|c| parse_task_brief_meta(&c.arguments))
                    .unwrap_or_else(|| (String::new(), "general".to_owned(), false));
                let id = child_run.run_id.clone();
                self.subagents.insert(
                    id.clone(),
                    LiveSubagent {
                        parent_call_id,
                        child_run_id: id,
                        child_session_id,
                        description,
                        subagent_kind,
                        resumed,
                        summary: *child_run,
                        tool_steps: Vec::new(),
                        todos: Vec::new(),
                        finished: false,
                    },
                );
            }
            AgentEvent::SubagentEvent {
                child_run_id,
                event,
                ..
            } => self.apply_subagent_event(&child_run_id, *event, ts_ms, fx),
            AgentEvent::SubagentFinished { child_run, .. } => {
                let finished_run_id = child_run.run_id.clone();
                if let Some(sub) = self.subagents.get_mut(&finished_run_id) {
                    sub.summary = *child_run;
                    sub.finished = true;
                }
                self.pending_approvals
                    .retain(|a| a.run_id != finished_run_id);
                if self
                    .cost_pause
                    .as_ref()
                    .is_some_and(|p| p.run_id == finished_run_id)
                {
                    self.cost_pause = None;
                }
            }
            AgentEvent::Usage {
                run_usage,
                cost_micro_usd,
                context_used_tokens,
                context_limit_tokens,
                currency,
                ..
            } => {
                if self
                    .cost_pause
                    .as_ref()
                    .is_some_and(|p| p.run_id == envelope_run_id)
                {
                    self.cost_pause = None;
                }
                self.run_meter = Some(RunMeter {
                    usage: run_usage,
                    cost_micro: cost_micro_usd,
                    currency,
                });
                self.context_used_tokens = context_used_tokens;
                self.context_limit_tokens = context_limit_tokens;
            }
            AgentEvent::CostLimitReached {
                run_id,
                cost_micro,
                cap_micro,
                currency,
                breakdown,
                next_cap_micro,
                unpriced_models,
                tokens,
            } => {
                fx.scroll_worthy = true;
                self.cost_pause = Some(LiveCostPause::from(LiveCostLimit {
                    run_id,
                    cost_micro,
                    cap_micro,
                    currency,
                    breakdown,
                    next_cap_micro,
                    unpriced_models,
                    tokens,
                }));
            }
            AgentEvent::GateOpened { run_id, gate } => {
                fx.scroll_worthy = true;
                self.open_gates.retain(|(_, g)| g.id != gate.id);
                self.open_gates.push((run_id, gate));
            }
            AgentEvent::GateResolved { gate_id, .. } => {
                self.open_gates.retain(|(_, g)| g.id != gate_id);
            }
            AgentEvent::SessionTitleUpdated { session_id, title } => {
                self.title.clone_from(&title);
                fx.title = Some((session_id, title));
            }
            AgentEvent::Notice { text, code, args } => self.push_notice(text, code, args),
            // Handled by `accept` before an event is applied.
            AgentEvent::Gap { .. } => {}
            AgentEvent::Done { run, stop_reason } => {
                fx.finished = true;
                self.active_run = Some(*run);
                self.last_stop_reason = Some(stop_reason);
                self.last_error = None;
                self.end_run();
            }
            AgentEvent::Error {
                run_id,
                error_kind,
                message,
                failure,
            } => {
                fx.finished = true;
                let cancelled = error_kind == "Cancelled";
                if let Some(run) = self.active_run.as_mut()
                    && run.run_id == run_id
                {
                    if cancelled {
                        run.status = AgentRunStatus::Cancelled;
                    } else {
                        run.status = AgentRunStatus::Failed;
                        run.error = Some(message.clone());
                    }
                }
                if !cancelled {
                    fx.scroll_worthy = true;
                    self.last_error = Some(RunError { message, failure });
                }
                self.end_run();
            }
        }
    }

    fn apply_subagent_event(
        &mut self,
        child_run_id: &str,
        event: AgentEvent,
        ts_ms: i64,
        fx: &mut BatchEffects,
    ) {
        // Activity on the paused sub-agent means its cost pause was continued elsewhere.
        if self
            .cost_pause
            .as_ref()
            .is_some_and(|p| p.run_id == child_run_id)
            && matches!(
                event,
                AgentEvent::TextDelta { .. }
                    | AgentEvent::ReasoningDelta { .. }
                    | AgentEvent::CitationAdded { .. }
                    | AgentEvent::ToolCallStarted { .. }
                    | AgentEvent::Usage { .. }
                    | AgentEvent::ToolResult { .. }
                    | AgentEvent::TodosUpdated { .. }
            )
        {
            self.cost_pause = None;
        }
        match event {
            AgentEvent::ToolCallStarted { call_id, name, .. } => {
                if let Some(sub) = self.subagents.get_mut(child_run_id)
                    && !sub.tool_steps.iter().any(|t| t.call_id == call_id)
                {
                    sub.tool_steps.push(LiveSubagentTool {
                        call_id,
                        name,
                        status: None,
                        duration_ms: None,
                        exit_code: None,
                    });
                }
            }
            AgentEvent::ToolResult {
                call_id,
                status,
                duration_ms,
                exit_code,
                ..
            } => {
                if let Some(step) = self
                    .subagents
                    .get_mut(child_run_id)
                    .and_then(|sub| sub.tool_steps.iter_mut().find(|t| t.call_id == call_id))
                {
                    step.status = Some(status);
                    step.duration_ms = Some(duration_ms);
                    step.exit_code = exit_code;
                }
            }
            event @ AgentEvent::ToolApprovalRequired { .. } => {
                fx.scroll_worthy = true;
                if let Some(sub) = self.subagents.get_mut(child_run_id) {
                    sub.summary.status = AgentRunStatus::WaitingApproval;
                }
                // Surface sub-agent approval cards into the parent chat so the user can answer
                // them while the parent chat is focused.
                self.push_approval(&event, ts_ms);
            }
            AgentEvent::ToolApprovalResolved { call_id, .. } => {
                if let Some(sub) = self.subagents.get_mut(child_run_id)
                    && sub.summary.status == AgentRunStatus::WaitingApproval
                {
                    sub.summary.status = AgentRunStatus::Running;
                }
                self.pending_approvals
                    .retain(|a| !(a.run_id == child_run_id && a.call_id == call_id));
            }
            AgentEvent::CostLimitReached {
                run_id,
                cost_micro,
                cap_micro,
                currency,
                breakdown,
                next_cap_micro,
                unpriced_models,
                tokens,
            } => {
                self.cost_pause = Some(LiveCostPause::from(LiveCostLimit {
                    run_id,
                    cost_micro,
                    cap_micro,
                    currency,
                    breakdown,
                    next_cap_micro,
                    unpriced_models,
                    tokens,
                }));
            }
            AgentEvent::FileChanged {
                call_id,
                path,
                change,
                added,
                removed,
                change_id,
            } => {
                self.file_changes.push(LiveFileChange {
                    call_id,
                    path,
                    change,
                    added,
                    removed,
                    change_id,
                });
            }
            AgentEvent::TodosUpdated { todos } => {
                if let Some(sub) = self.subagents.get_mut(child_run_id) {
                    sub.todos = todos;
                }
            }
            AgentEvent::Notice { text, code, args } => self.push_notice(text, code, args),
            // The core forwards nothing else of a child (its full stream is the child chat's).
            _ => {}
        }
    }

    fn tool_call(&mut self, call_id: &str) -> Option<&mut LiveToolCall> {
        self.tool_calls.iter_mut().find(|c| c.call_id == call_id)
    }

    fn apply_user_message(&mut self, msg: AgentMessage) {
        self.messages_version = self.messages_version.wrapping_add(1);
        if let Some(pos) = self.messages.iter().position(|m| m.id == msg.id) {
            // Retrying an existing turn discards everything that came after it on the old branch.
            self.messages[pos] = msg;
            self.messages.truncate(pos + 1);
            return;
        }
        if let Some(parent_id) = msg.parent_id.as_deref() {
            if let Some(pos) = self.messages.iter().position(|m| m.id == parent_id) {
                // Editing a mid-thread turn branches after `parent_id`: drop the old suffix.
                self.messages.truncate(pos + 1);
            }
            self.messages.push(msg);
        } else if msg.branch_index > 0 || !msg.branch_siblings.is_empty() {
            // Edited root message (sibling of the first message): drop the abandoned branch.
            self.messages.clear();
            self.messages.push(msg);
        } else {
            self.messages.push(msg);
        }
    }

    fn push_or_replace_message(&mut self, msg: AgentMessage) {
        self.messages_version = self.messages_version.wrapping_add(1);
        if let Some(existing) = self.messages.iter_mut().find(|m| m.id == msg.id) {
            *existing = msg;
        } else {
            self.messages.push(msg);
        }
    }
}

/// Appends a fragment, merging into the last chunk of the same stream, then trims the oldest
/// bytes so the total stays within `MAX_LIVE_TOOL_OUTPUT_BYTES`.
fn push_output(output: &mut Vec<(OutputStream, String)>, stream: OutputStream, text: &str) {
    match output.last_mut() {
        Some((last, buf)) if *last == stream => buf.push_str(text),
        _ => output.push((stream, text.to_owned())),
    }
    let mut excess = output
        .iter()
        .map(|(_, t)| t.len())
        .sum::<usize>()
        .saturating_sub(MAX_LIVE_TOOL_OUTPUT_BYTES);
    while excess > 0 {
        let Some((_, first)) = output.first_mut() else {
            break;
        };
        if first.len() <= excess {
            excess -= first.len();
            output.remove(0);
        } else {
            let cut = first.ceil_char_boundary(excess);
            first.drain(..cut);
            excess = 0;
        }
    }
}

/// The result is the crash-recovery placeholder of a call that has not settled (or never did).
#[must_use]
pub fn is_pending_placeholder_parts(parts: &[HubPart]) -> bool {
    matches!(
        parts,
        [HubPart::Text { text, .. }] if text.trim() == PENDING_TOOL_RESULT_MARKER
    )
}

/// Bytes and media type of an inline (base64) image; `None` for a remote URL, an attachment
/// reference, or invalid base64.
#[must_use]
pub fn decode_inline_image(image: &HubImage) -> Option<(Vec<u8>, String)> {
    match image {
        HubImage::Base64 { media_type, data } => base64::engine::general_purpose::STANDARD
            .decode(data)
            .ok()
            .map(|bytes| (bytes, media_type.clone())),
        _ => None,
    }
}

fn parse_task_brief_meta(raw: &str) -> (String, String, bool) {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(raw) else {
        return (String::new(), "general".to_owned(), false);
    };
    let desc = val
        .get("description")
        .and_then(|v| v.as_str())
        .unwrap_or("")
        .trim()
        .to_owned();
    let kind = val
        .get("kind")
        .and_then(|v| v.as_str())
        .unwrap_or("general")
        .to_owned();
    let resumed = val
        .get("resume")
        .and_then(|v| v.as_str())
        .is_some_and(|s| !s.trim().is_empty());
    (desc, kind, resumed)
}

/// Minimal active [`AgentRunSummary`] synthesized before `RunStarted` or a DB run row arrives
/// (used when hydrating a live snapshot or seeding a newly-dispatched run).
#[must_use]
pub fn provisional_run_summary(
    run_id: String,
    session_id: String,
    status: AgentRunStatus,
    target: Option<AgentTarget>,
    meter: Option<&RunMeter>,
    last_seq: u64,
) -> AgentRunSummary {
    AgentRunSummary {
        run_id,
        session_id,
        kind: AgentRunKind::Main,
        parent_run_id: None,
        parent_call_id: None,
        status,
        model_label: String::new(),
        target,
        iterations: 0,
        usage: meter.map(|m| m.usage.clone()).unwrap_or_default(),
        cost_micro_usd: meter.map_or(0, |m| m.cost_micro),
        started_at_ms: 0,
        finished_at_ms: None,
        last_event_ms: None,
        last_step: None,
        stop_reason: None,
        error_kind: None,
        error: None,
        last_seq,
        failure: None,
        role: None,
        cost_currency: meter.and_then(|m| m.currency.clone()),
        cap_inapplicable: None,
        status_detail: None,
    }
}

#[cfg(test)]
mod tests;
