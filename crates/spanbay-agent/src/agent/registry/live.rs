//! The in-progress answer of a run, rebuilt from the run's own events.
//!
//! Single responsibility: fold the events of one top-level run into what no stored message holds
//! yet (text, reasoning, tool calls and their state), so a client that reattaches after the replay
//! ring evicted those events can still show them. `ActiveRun::emit` feeds it under the same lock a
//! snapshot takes, which makes a snapshot and its `seq` consistent with every emit.

use spanbay_llm::llm::hub::{HubCitation, HubPart};

use super::super::model::{
    AgentEvent, AgentRole, HumanGateDto, LiveApprovalRequest, LiveCostLimit, LiveRunSnapshot,
    LiveToolCall, LiveToolResult, LiveToolState, LiveUsage,
};

/// Text, reasoning and tool-call arguments kept per run; later fragments are dropped and
/// `truncated` is set (shortcut: the head is kept, so a client resumes from the stored message).
pub(super) const MAX_LIVE_BYTES: usize = 1024 * 1024;
/// Characters of a tool result kept as its summary.
const SUMMARY_CHARS: usize = 200;

/// What the run has streamed since its last stored assistant message.
#[derive(Default)]
pub(super) struct LiveAcc {
    parts: Vec<HubPart>,
    tools: Vec<LiveToolCall>,
    /// The run's last usage; unlike the rest it outlives a step.
    usage: Option<LiveUsage>,
    bytes: usize,
    truncated: bool,
    /// The assistant message of the current step is stored; the next content starts a new step.
    saved: bool,
    /// Plans and questions the run waits on; like `usage` they outlive a step.
    gates: Vec<HumanGateDto>,
    /// The cost pause the run waits on; the run's next content (or its end) answers it.
    cost_limit: Option<LiveCostLimit>,
}

impl LiveAcc {
    /// Folds one event of the run into the accumulator.
    pub(super) fn apply(&mut self, event: &AgentEvent) {
        // A cost pause holds the run between model calls: the next call's output means the user
        // let it go on (a stop ends the run, which resets everything).
        if matches!(
            event,
            AgentEvent::TextDelta { .. }
                | AgentEvent::ReasoningDelta { .. }
                | AgentEvent::CitationAdded { .. }
                | AgentEvent::ToolCallStarted { .. }
                | AgentEvent::Usage { .. }
        ) {
            self.cost_limit = None;
        } else if let AgentEvent::SubagentEvent {
            child_run_id,
            event: child_ev,
            ..
        } = event
            && self
                .cost_limit
                .as_ref()
                .is_some_and(|limit| limit.run_id == *child_run_id)
            && matches!(
                child_ev.as_ref(),
                AgentEvent::TextDelta { .. }
                    | AgentEvent::ReasoningDelta { .. }
                    | AgentEvent::CitationAdded { .. }
                    | AgentEvent::ToolCallStarted { .. }
                    | AgentEvent::Usage { .. }
                    | AgentEvent::ToolResult { .. }
                    | AgentEvent::TodosUpdated { .. }
            )
        {
            self.cost_limit = None;
        }
        match event {
            AgentEvent::TextDelta { text } => {
                self.begin();
                if self.reserve(text.len()) {
                    match self.parts.last_mut() {
                        Some(HubPart::Text { text: last, .. }) => last.push_str(text),
                        _ => self.parts.push(text_part(text.clone(), Vec::new())),
                    }
                }
            }
            AgentEvent::ReasoningDelta { text } => {
                self.begin();
                if self.reserve(text.len()) {
                    match self.parts.last_mut() {
                        Some(HubPart::Reasoning { text: last, .. }) => last.push_str(text),
                        _ => self.parts.push(HubPart::Reasoning {
                            text: text.clone(),
                            signature: None,
                            encrypted: None,
                            id: None,
                            origin: None,
                        }),
                    }
                }
            }
            AgentEvent::CitationAdded { citation } => {
                self.begin();
                self.cite(citation);
            }
            AgentEvent::ToolCallStarted {
                call_id,
                name,
                source,
            } => {
                self.begin();
                self.parts.push(HubPart::ToolCall {
                    id: call_id.clone(),
                    name: name.clone(),
                    arguments: String::new(),
                    signature: None,
                    origin: None,
                });
                self.tools.push(LiveToolCall {
                    call_id: call_id.clone(),
                    name: name.clone(),
                    source: source.clone(),
                    state: LiveToolState::StreamingArgs,
                    child_run_id: None,
                    result: None,
                    approval: None,
                });
            }
            AgentEvent::ToolCallArgsDelta { call_id, arguments } => {
                if self.reserve(arguments.len())
                    && let Some(HubPart::ToolCall {
                        arguments: so_far, ..
                    }) = self.call_part(call_id)
                {
                    so_far.push_str(arguments);
                }
            }
            AgentEvent::ToolApprovalRequired {
                call_id, arguments, ..
            } => {
                // The complete arguments replace whatever fragments arrived (or were dropped).
                let old = match self.call_part(call_id) {
                    Some(HubPart::ToolCall {
                        arguments: so_far, ..
                    }) => Some(std::mem::replace(so_far, arguments.clone())),
                    _ => None,
                };
                if let Some(old) = old {
                    self.bytes = (self.bytes + arguments.len()).saturating_sub(old.len());
                }
                self.set_state(call_id, LiveToolState::WaitingApproval);
                if let Some(tool) = self.tool(call_id) {
                    tool.approval = approval_request(event);
                }
            }
            AgentEvent::ToolApprovalResolved { call_id, .. } => {
                self.set_state(call_id, LiveToolState::Running);
                if let Some(tool) = self.tool(call_id) {
                    tool.approval = None;
                }
            }
            AgentEvent::Usage {
                run_usage,
                cost_micro_usd,
                currency,
                context_used_tokens,
                context_limit_tokens,
                ..
            } => {
                self.usage = Some(LiveUsage {
                    run_usage: run_usage.clone(),
                    cost_micro_usd: *cost_micro_usd,
                    currency: currency.clone(),
                    context_used_tokens: *context_used_tokens,
                    context_limit_tokens: *context_limit_tokens,
                });
            }
            AgentEvent::ToolResult {
                call_id,
                status,
                parts,
                duration_ms,
                exit_code,
                ..
            } => {
                if let Some(tool) = self.tool(call_id) {
                    tool.state = LiveToolState::Done;
                    tool.result = Some(LiveToolResult {
                        status: *status,
                        summary: summary(parts),
                        duration_ms: *duration_ms,
                        exit_code: *exit_code,
                    });
                }
            }
            AgentEvent::SubagentStarted {
                parent_call_id,
                child_run,
                ..
            } => {
                if let Some(tool) = self.tool(parent_call_id) {
                    tool.child_run_id = Some(child_run.run_id.clone());
                }
            }
            AgentEvent::SubagentEvent {
                child_run_id,
                event,
                ..
            } => match event.as_ref() {
                child @ AgentEvent::ToolApprovalRequired { call_id, name, .. } => {
                    let approval = approval_request(child);
                    if let Some(tool) = self.tool(call_id) {
                        tool.state = LiveToolState::WaitingApproval;
                        tool.approval = approval;
                        tool.child_run_id = Some(child_run_id.clone());
                    } else {
                        self.tools.push(LiveToolCall {
                            call_id: call_id.clone(),
                            name: name.clone(),
                            source: crate::agent::tools::ToolSource::Builtin,
                            state: LiveToolState::WaitingApproval,
                            child_run_id: Some(child_run_id.clone()),
                            result: None,
                            approval,
                        });
                    }
                }
                AgentEvent::ToolApprovalResolved { call_id, .. } => {
                    self.tools.retain(|tool| {
                        !(tool.call_id == *call_id
                            && tool.child_run_id.as_deref() == Some(child_run_id.as_str()))
                    });
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
                    self.cost_limit = Some(LiveCostLimit {
                        run_id: run_id.clone(),
                        cost_micro: *cost_micro,
                        cap_micro: *cap_micro,
                        currency: currency.clone(),
                        breakdown: breakdown.clone(),
                        next_cap_micro: *next_cap_micro,
                        unpriced_models: unpriced_models.clone(),
                        tokens: *tokens,
                    });
                }
                _ => {}
            },
            AgentEvent::SubagentFinished { child_run, .. } => {
                self.tools.retain(|tool| {
                    !(tool.child_run_id.as_deref() == Some(child_run.run_id.as_str())
                        && tool.approval.is_some())
                });
                if self
                    .cost_limit
                    .as_ref()
                    .is_some_and(|limit| limit.run_id == child_run.run_id)
                {
                    self.cost_limit = None;
                }
            }
            AgentEvent::MessageSaved { message } if message.role == AgentRole::Assistant => {
                // The stored message holds the text now; the calls keep their live state.
                let kept: usize = self
                    .parts
                    .iter()
                    .map(|part| match part {
                        HubPart::ToolCall { arguments, .. } => arguments.len(),
                        _ => 0,
                    })
                    .sum();
                self.parts
                    .retain(|part| matches!(part, HubPart::ToolCall { .. }));
                self.bytes = kept;
                for tool in &mut self.tools {
                    if tool.state == LiveToolState::StreamingArgs {
                        tool.state = LiveToolState::Running;
                    }
                }
                self.saved = true;
            }
            AgentEvent::GateOpened { gate, .. } => {
                self.gates.retain(|open| open.id != gate.id);
                self.gates.push(gate.clone());
            }
            AgentEvent::GateResolved { gate_id, .. } => {
                self.gates.retain(|open| open.id != *gate_id);
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
                self.cost_limit = Some(LiveCostLimit {
                    run_id: run_id.clone(),
                    cost_micro: *cost_micro,
                    cap_micro: *cap_micro,
                    currency: currency.clone(),
                    breakdown: breakdown.clone(),
                    next_cap_micro: *next_cap_micro,
                    unpriced_models: unpriced_models.clone(),
                    tokens: *tokens,
                });
            }
            AgentEvent::Done { .. } | AgentEvent::Error { .. } => *self = Self::default(),
            _ => {}
        }
    }

    /// The accumulated state of run `run_id` at event `seq`.
    pub(super) fn snapshot(&self, run_id: &str, seq: u64) -> LiveRunSnapshot {
        LiveRunSnapshot {
            run_id: run_id.to_owned(),
            seq,
            parts: self.parts.clone(),
            tools: self.tools.clone(),
            usage: self.usage.clone(),
            truncated: self.truncated,
            gates: self.gates.clone(),
            cost_limit: self.cost_limit.clone(),
        }
    }

    /// Content of a new model step: forgets the previous step's calls once its message is stored.
    fn begin(&mut self) {
        if self.saved {
            *self = Self {
                usage: self.usage.take(),
                gates: std::mem::take(&mut self.gates),
                ..Self::default()
            };
        }
    }

    /// Accounts `len` more bytes; `false` (and `truncated`) when they would pass the cap.
    fn reserve(&mut self, len: usize) -> bool {
        if self.bytes + len > MAX_LIVE_BYTES {
            self.truncated = true;
            return false;
        }
        self.bytes += len;
        true
    }

    fn cite(&mut self, citation: &HubCitation) {
        match self.parts.last_mut() {
            Some(HubPart::Text { citations, .. }) => citations.push(citation.clone()),
            _ => self
                .parts
                .push(text_part(String::new(), vec![citation.clone()])),
        }
    }

    fn call_part(&mut self, call_id: &str) -> Option<&mut HubPart> {
        self.parts
            .iter_mut()
            .rev()
            .find(|part| matches!(part, HubPart::ToolCall { id, .. } if id == call_id))
    }

    fn tool(&mut self, call_id: &str) -> Option<&mut LiveToolCall> {
        self.tools
            .iter_mut()
            .rev()
            .find(|tool| tool.call_id == call_id)
    }

    fn set_state(&mut self, call_id: &str, state: LiveToolState) {
        if let Some(tool) = self.tool(call_id) {
            tool.state = state;
        }
    }
}

fn text_part(text: String, citations: Vec<HubCitation>) -> HubPart {
    HubPart::Text {
        text,
        signature: None,
        origin: None,
        citations,
    }
}

fn approval_request(event: &AgentEvent) -> Option<Box<LiveApprovalRequest>> {
    LiveApprovalRequest::from_event(event, spanbay_base::time::now_ms()).map(Box::new)
}

/// The first [`SUMMARY_CHARS`] characters of the text parts of a tool result.
fn summary(parts: &[HubPart]) -> String {
    let mut out = String::new();
    for part in parts {
        if let HubPart::Text { text, .. } = part {
            out.extend(text.chars().take(SUMMARY_CHARS - out.chars().count()));
            if out.chars().count() >= SUMMARY_CHARS {
                break;
            }
        }
    }
    out
}
