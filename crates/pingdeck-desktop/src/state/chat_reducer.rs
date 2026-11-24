//! Pure-Rust headless reducer for Chat / Coding Agent streaming runs (`AgentEnvelope`).
//!
//! Consumes coalesced [`AgentEnvelope`] events with sequence deduplication and gap detection,
//! producing render-ready state for any UI shell (100% unit-testable without GPU or window).

use std::collections::BTreeMap;

use pingdeck_core::agent::model::{
    AgentEnvelope, AgentEvent, AgentMessage, AgentRunStatus, AgentRunSummary, AgentStopReason,
    AgentTodoItem, ApprovalDecision, ApprovalScope, HumanGateDto, QueuedTurn, ToolCallStatus,
    ToolSource,
};
use pingdeck_core::agent::tools::FileChangeKind;

/// Maximum tail bytes retained in live streaming tool output per call to keep UI rendering O(1).
const MAX_LIVE_TOOL_OUTPUT_BYTES: usize = 64 * 1024;

/// Live tool call state inside an active run.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveToolCall {
    pub call_id: String,
    pub name: String,
    pub source: ToolSource,
    pub arguments: String,
    pub output: String,
    pub status: Option<ToolCallStatus>,
    pub duration_ms: Option<u64>,
    pub exit_code: Option<i32>,
}

/// Pending tool approval card surfaced to the UI.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveApproval {
    pub run_id: String,
    pub call_id: String,
    pub name: String,
    pub risk: String,
    pub summary: String,
    pub reason: String,
    pub arguments: String,
    pub full_text: Option<String>,
    pub destructive: bool,
    pub network: bool,
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

/// Active cost/token pause waiting for `agent_run_continue_impl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LiveCostPause {
    pub run_id: String,
    pub cost_micro: i64,
    pub cap_micro: i64,
    pub currency: String,
    pub is_tokens: bool,
}

/// Live sub-agent progress nested under a parent `task` tool call.
#[derive(Debug, Clone, PartialEq)]
pub struct LiveSubagent {
    pub parent_call_id: String,
    pub child_run_id: String,
    pub child_session_id: String,
    pub summary: AgentRunSummary,
    pub streaming_text: String,
    pub active_tools: usize,
    pub finished: bool,
}

/// Complete render-ready thread state maintained by [`ChatStreamReducer`].
#[derive(Debug, Clone, Default)]
pub struct ChatThreadState {
    pub session_id: String,
    pub title: String,
    pub messages: Vec<AgentMessage>,
    pub active_run: Option<AgentRunSummary>,
    pub last_stop_reason: Option<AgentStopReason>,
    pub last_error: Option<String>,
    pub streaming_text: String,
    pub streaming_reasoning: String,
    pub tool_calls: Vec<LiveToolCall>,
    pub pending_approvals: Vec<LiveApproval>,
    pub open_gates: Vec<(String, HumanGateDto)>,
    pub cost_pause: Option<LiveCostPause>,
    pub file_changes: Vec<LiveFileChange>,
    pub todos: Vec<AgentTodoItem>,
    pub queued_turns: Vec<QueuedTurn>,
    pub queue_paused: bool,
    pub subagents: BTreeMap<String, LiveSubagent>,
    pub notices: Vec<String>,
    pub context_used_tokens: u64,
    pub context_limit_tokens: Option<u64>,
    pub cost_micro_usd: i64,
    pub cost_currency: Option<String>,
    /// Last applied sequence per run id for strict deduplication.
    last_seq_by_run: BTreeMap<String, u64>,
    /// Set when the replay ring evicted unseen events (`AgentEvent::Gap`); UI should reload snapshot.
    pub needs_reload: bool,
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

    /// Replaces persisted message history from a fresh database snapshot and clears `needs_reload`.
    pub fn replace_snapshot(&mut self, title: String, messages: Vec<AgentMessage>) {
        self.title = title;
        self.messages = messages;
        self.needs_reload = false;
    }

    /// Last sequence number received for a given run (for `from_seq` resumption).
    #[must_use]
    pub fn last_seq(&self, run_id: &str) -> Option<u64> {
        self.last_seq_by_run.get(run_id).copied()
    }

    /// Applies one [`AgentEnvelope`] with sequence deduplication. Returns `true` if state changed.
    pub fn apply(&mut self, envelope: AgentEnvelope) -> bool {
        if let Some(&seen) = self.last_seq_by_run.get(&envelope.run_id)
            && envelope.seq <= seen
        {
            return false;
        }
        self.last_seq_by_run
            .insert(envelope.run_id.clone(), envelope.seq);
        self.apply_event(&envelope.run_id, envelope.event);
        true
    }

    /// Applies a drained batch of envelopes in order. Returns `true` if any changed state.
    pub fn apply_batch(&mut self, batch: impl IntoIterator<Item = AgentEnvelope>) -> bool {
        let mut changed = false;
        for env in batch {
            changed |= self.apply(env);
        }
        changed
    }

    fn apply_event(&mut self, _run_id: &str, event: AgentEvent) {
        match event {
            AgentEvent::RunStarted { run } => {
                self.active_run = Some(*run);
                self.last_error = None;
                self.last_stop_reason = None;
                self.cost_pause = None;
                self.streaming_text.clear();
                self.streaming_reasoning.clear();
                self.tool_calls.clear();
            }
            AgentEvent::QueuedUpdated { queued, paused, .. } => {
                self.queued_turns = queued;
                self.queue_paused = paused;
            }
            AgentEvent::UserMessage { message, .. } => {
                self.push_or_replace_message(*message);
            }
            AgentEvent::MessageSaved { message } => {
                self.push_or_replace_message(*message);
                self.streaming_text.clear();
                self.streaming_reasoning.clear();
            }
            AgentEvent::TextDelta { text } => {
                self.streaming_text.push_str(&text);
            }
            AgentEvent::ReasoningDelta { text } => {
                self.streaming_reasoning.push_str(&text);
            }
            AgentEvent::ToolCallStarted {
                call_id,
                name,
                source,
            } => {
                if !self.tool_calls.iter().any(|c| c.call_id == call_id) {
                    self.tool_calls.push(LiveToolCall {
                        call_id,
                        name,
                        source,
                        arguments: String::new(),
                        output: String::new(),
                        status: None,
                        duration_ms: None,
                        exit_code: None,
                    });
                }
            }
            AgentEvent::ToolCallArgsDelta { call_id, arguments } => {
                if let Some(call) = self.tool_calls.iter_mut().find(|c| c.call_id == call_id) {
                    call.arguments.push_str(&arguments);
                }
            }
            AgentEvent::ToolOutputDelta { call_id, text, .. } => {
                if let Some(call) = self.tool_calls.iter_mut().find(|c| c.call_id == call_id) {
                    call.output.push_str(&text);
                    if call.output.len() > MAX_LIVE_TOOL_OUTPUT_BYTES {
                        let cut = call.output.len() - MAX_LIVE_TOOL_OUTPUT_BYTES;
                        let boundary = call.output.ceil_char_boundary(cut);
                        call.output.drain(..boundary);
                    }
                }
            }
            AgentEvent::ToolApprovalRequired {
                run_id,
                call_id,
                name,
                arguments,
                reason,
                network,
                risk,
                summary,
                full_text,
                destructive,
                ..
            } => {
                self.pending_approvals.retain(|a| a.call_id != call_id);
                self.pending_approvals.push(LiveApproval {
                    run_id,
                    call_id,
                    name,
                    risk,
                    summary,
                    reason,
                    arguments,
                    full_text,
                    destructive,
                    network,
                });
            }
            AgentEvent::ToolApprovalResolved {
                call_id,
                decision: ApprovalDecision::Deny,
                scope: ApprovalScope::Once,
                timed_out: true,
            } => {
                self.pending_approvals.retain(|a| a.call_id != call_id);
                self.notices
                    .push(format!("Tool approval timed out ({call_id})"));
            }
            AgentEvent::ToolApprovalResolved { call_id, .. } => {
                self.pending_approvals.retain(|a| a.call_id != call_id);
            }
            AgentEvent::ToolResult {
                call_id,
                status,
                duration_ms,
                exit_code,
                ..
            } => {
                if let Some(call) = self.tool_calls.iter_mut().find(|c| c.call_id == call_id) {
                    call.status = Some(status);
                    call.duration_ms = Some(duration_ms);
                    call.exit_code = exit_code;
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
                let id = child_run.run_id.clone();
                self.subagents.insert(
                    id.clone(),
                    LiveSubagent {
                        parent_call_id,
                        child_run_id: id,
                        child_session_id,
                        summary: *child_run,
                        streaming_text: String::new(),
                        active_tools: 0,
                        finished: false,
                    },
                );
            }
            AgentEvent::SubagentEvent {
                child_run_id,
                event,
                ..
            } => match *event {
                AgentEvent::TextDelta { text } => {
                    if let Some(sub) = self.subagents.get_mut(&child_run_id) {
                        sub.streaming_text.push_str(&text);
                    }
                }
                AgentEvent::ToolCallStarted { .. } => {
                    if let Some(sub) = self.subagents.get_mut(&child_run_id) {
                        sub.active_tools = sub.active_tools.saturating_add(1);
                    }
                }
                AgentEvent::ToolResult { .. } => {
                    if let Some(sub) = self.subagents.get_mut(&child_run_id) {
                        sub.active_tools = sub.active_tools.saturating_sub(1);
                    }
                }
                AgentEvent::ToolApprovalRequired {
                    run_id,
                    call_id,
                    name,
                    arguments,
                    reason,
                    network,
                    risk,
                    summary,
                    full_text,
                    destructive,
                    ..
                } => {
                    // Surface sub-agent approval cards into the parent chat dock so the user can
                    // approve/deny instead of deadlocking while the parent chat is focused.
                    self.pending_approvals.retain(|a| a.call_id != call_id);
                    self.pending_approvals.push(LiveApproval {
                        run_id,
                        call_id,
                        name,
                        risk,
                        summary,
                        reason,
                        arguments,
                        full_text,
                        destructive,
                        network,
                    });
                }
                AgentEvent::ToolApprovalResolved {
                    call_id,
                    decision: ApprovalDecision::Deny,
                    scope: ApprovalScope::Once,
                    timed_out: true,
                } => {
                    self.pending_approvals.retain(|a| a.call_id != call_id);
                    self.notices
                        .push(format!("Sub-agent tool approval timed out ({call_id})"));
                }
                AgentEvent::ToolApprovalResolved { call_id, .. } => {
                    self.pending_approvals.retain(|a| a.call_id != call_id);
                }
                AgentEvent::CostLimitReached {
                    run_id,
                    cost_micro,
                    cap_micro,
                    currency,
                    tokens,
                    ..
                } => {
                    self.cost_pause = Some(LiveCostPause {
                        run_id,
                        cost_micro,
                        cap_micro,
                        currency,
                        is_tokens: tokens.unwrap_or(false),
                    });
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
                AgentEvent::Notice { text, .. } => {
                    self.notices.push(text);
                    if self.notices.len() > 20 {
                        self.notices.remove(0);
                    }
                }
                _ => {}
            },
            AgentEvent::SubagentFinished { child_run, .. } => {
                let finished_run_id = child_run.run_id.clone();
                if let Some(sub) = self.subagents.get_mut(&finished_run_id) {
                    sub.summary = *child_run;
                    sub.finished = true;
                    sub.active_tools = 0;
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
                cost_micro_usd,
                context_used_tokens,
                context_limit_tokens,
                currency,
                ..
            } => {
                self.cost_micro_usd = cost_micro_usd;
                self.context_used_tokens = context_used_tokens;
                self.context_limit_tokens = context_limit_tokens;
                self.cost_currency = currency;
            }
            AgentEvent::CostLimitReached {
                run_id,
                cost_micro,
                cap_micro,
                currency,
                tokens,
                ..
            } => {
                self.cost_pause = Some(LiveCostPause {
                    run_id,
                    cost_micro,
                    cap_micro,
                    currency,
                    is_tokens: tokens.unwrap_or(false),
                });
            }
            AgentEvent::GateOpened { run_id, gate } => {
                self.open_gates.retain(|(_, g)| g.id != gate.id);
                self.open_gates.push((run_id, gate));
            }
            AgentEvent::GateResolved { gate_id, .. } => {
                self.open_gates.retain(|(_, g)| g.id != gate_id);
            }
            AgentEvent::SessionTitleUpdated { title, .. } => {
                self.title = title;
            }
            AgentEvent::Notice { text, .. } => {
                self.notices.push(text);
                if self.notices.len() > 20 {
                    self.notices.remove(0);
                }
            }
            AgentEvent::Gap { .. } => {
                self.needs_reload = true;
            }
            AgentEvent::Done { run, stop_reason } => {
                self.active_run = Some(*run);
                self.last_stop_reason = Some(stop_reason);
                self.streaming_text.clear();
                self.streaming_reasoning.clear();
                self.tool_calls.clear();
                self.cost_pause = None;
                self.pending_approvals.clear();
            }
            AgentEvent::Error { message, .. } => {
                if let Some(run) = self.active_run.as_mut() {
                    run.status = AgentRunStatus::Failed;
                    run.error = Some(message.clone());
                }
                self.last_error = Some(message);
                self.streaming_text.clear();
                self.streaming_reasoning.clear();
                self.tool_calls.clear();
                self.cost_pause = None;
                self.pending_approvals.clear();
            }
            AgentEvent::McpConnecting { .. }
            | AgentEvent::ContextCompacted { .. }
            | AgentEvent::CitationAdded { .. } => {}
        }
    }

    fn push_or_replace_message(&mut self, msg: AgentMessage) {
        if let Some(existing) = self.messages.iter_mut().find(|m| m.id == msg.id) {
            *existing = msg;
        } else {
            self.messages.push(msg);
        }
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn env(seq: u64, event: AgentEvent) -> AgentEnvelope {
        AgentEnvelope {
            seq,
            first_seq: seq,
            ts_ms: 1000,
            run_id: "r1".into(),
            event,
        }
    }

    #[test]
    fn deduplicates_seq_accumulates_deltas_and_flags_gaps() {
        let mut state = ChatThreadState::new("s1", "Untitled");
        assert!(state.apply(env(
            1,
            AgentEvent::TextDelta {
                text: "Hello ".into()
            }
        )));
        assert!(!state.apply(env(1, AgentEvent::TextDelta { text: "DUP".into() })));
        assert!(state.apply(env(
            2,
            AgentEvent::TextDelta {
                text: "GPUI".into()
            }
        )));
        assert_eq!(state.streaming_text, "Hello GPUI");

        assert!(state.apply(env(
            3,
            AgentEvent::ToolCallStarted {
                call_id: "c1".into(),
                name: "shell".into(),
                source: ToolSource::Builtin,
            }
        )));
        assert!(state.apply(env(
            4,
            AgentEvent::ToolCallArgsDelta {
                call_id: "c1".into(),
                arguments: "{\"cmd\":\"ls\"}".into(),
            }
        )));
        assert_eq!(state.tool_calls[0].arguments, "{\"cmd\":\"ls\"}");

        assert!(state.apply(env(5, AgentEvent::Gap { first_seq: 10 })));
        assert!(state.needs_reload);
    }

    #[test]
    fn subagent_approvals_cost_pause_file_changes_and_cleanup_reach_parent_state() {
        use pingdeck_core::agent::model::{
            AgentRunKind, AgentRunSummary, AgentTarget, SandboxKind, ToolClass,
        };

        let mut state = ChatThreadState::new("s_parent", "Parent");
        let child_run = AgentRunSummary {
            run_id: "r_child".into(),
            session_id: "s_child".into(),
            kind: AgentRunKind::Subagent,
            parent_run_id: Some("r1".into()),
            parent_call_id: Some("tc_task".into()),
            status: AgentRunStatus::Running,
            model_label: "fast".into(),
            target: Some(AgentTarget::Named {
                name: "fast".into(),
            }),
            iterations: 0,
            usage: Default::default(),
            cost_micro_usd: 0,
            started_at_ms: 1000,
            finished_at_ms: None,
            last_event_ms: None,
            last_step: None,
            stop_reason: None,
            error_kind: None,
            error: None,
            prompt_version: 1,
            last_seq: 0,
            failure: None,
            role: None,
            cost_currency: None,
            cap_inapplicable: None,
            status_detail: None,
        };
        state.apply(env(
            1,
            AgentEvent::SubagentStarted {
                parent_call_id: "tc_task".into(),
                child_session_id: "s_child".into(),
                child_run: Box::new(child_run.clone()),
            },
        ));
        state.apply(env(
            2,
            AgentEvent::SubagentEvent {
                parent_call_id: "tc_task".into(),
                child_run_id: "r_child".into(),
                event: Box::new(AgentEvent::ToolApprovalRequired {
                    run_id: "r_child".into(),
                    call_id: "c_write".into(),
                    name: "write_file".into(),
                    arguments: r#"{"path":"out.txt"}"#.into(),
                    class: ToolClass::Edit,
                    read_only: false,
                    destructive: false,
                    sandbox: SandboxKind::None,
                    reason: "writes file".into(),
                    preview: None,
                    network: false,
                    risk: "medium".into(),
                    summary: "Write out.txt".into(),
                    full_text: None,
                    card: None,
                }),
            },
        ));
        assert_eq!(state.pending_approvals.len(), 1);
        assert_eq!(state.pending_approvals[0].run_id, "r_child");
        assert_eq!(state.pending_approvals[0].call_id, "c_write");

        state.apply(env(
            3,
            AgentEvent::SubagentEvent {
                parent_call_id: "tc_task".into(),
                child_run_id: "r_child".into(),
                event: Box::new(AgentEvent::ToolApprovalResolved {
                    call_id: "c_write".into(),
                    decision: ApprovalDecision::Allow,
                    scope: ApprovalScope::Once,
                    timed_out: false,
                }),
            },
        ));
        assert!(state.pending_approvals.is_empty());

        state.apply(env(
            4,
            AgentEvent::SubagentEvent {
                parent_call_id: "tc_task".into(),
                child_run_id: "r_child".into(),
                event: Box::new(AgentEvent::CostLimitReached {
                    run_id: "r_child".into(),
                    cost_micro: 2_000_000,
                    cap_micro: 1_000_000,
                    currency: "USD".into(),
                    breakdown: None,
                    next_cap_micro: None,
                    unpriced_models: None,
                    tokens: None,
                }),
            },
        ));
        assert_eq!(
            state.cost_pause.as_ref().map(|p| p.run_id.as_str()),
            Some("r_child")
        );

        state.apply(env(
            5,
            AgentEvent::SubagentEvent {
                parent_call_id: "tc_task".into(),
                child_run_id: "r_child".into(),
                event: Box::new(AgentEvent::FileChanged {
                    call_id: "c_write".into(),
                    path: "out.txt".into(),
                    change: FileChangeKind::Create,
                    added: 3,
                    removed: 0,
                    change_id: Some("fc1".into()),
                }),
            },
        ));
        assert_eq!(state.file_changes.len(), 1);
        assert_eq!(state.file_changes[0].path, "out.txt");

        // SubagentFinished clears any lingering approval or cost pause belonging to that child.
        let finished_run = AgentRunSummary {
            status: AgentRunStatus::Completed,
            stop_reason: Some("completed".into()),
            ..child_run
        };
        state.apply(env(
            6,
            AgentEvent::SubagentFinished {
                parent_call_id: "tc_task".into(),
                child_run: Box::new(finished_run),
            },
        ));
        assert!(state.cost_pause.is_none());
        assert!(state.pending_approvals.is_empty());
        assert!(state.subagents["r_child"].finished);
    }
}
