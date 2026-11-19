//! Running the tool calls of one assistant turn.
//!
//! Single responsibility: for each call of a turn, in order: check the permission gate, ask the
//! user when needed, run the tool (consecutive parallel-safe allowed calls run concurrently, up to
//! 4, and their results are stored in call order), and store the result, so the history is valid
//! after every call. Every call ends with a result part: a run that is cancelled or steered
//! answers the calls it did not run with a status text instead of leaving them pending. Each stored
//! result goes past the progress probe, which may append a nudge to it.

use std::sync::{Mutex, PoisonError};

use pingdeck_base::cancel::CancelToken;
use pingdeck_base::error::AppError;
use pingdeck_llm::llm::hub::HubPart;
use pingdeck_llm::llm::hub::text_of;

use super::super::approvals::{ApprovalAnswer, ApprovalOutcome};
use super::super::context::cap_text_parts;
use super::super::model::{AgentEvent, ApprovalDecision, ToolCallStatus};
use super::super::tool_output_files::{READ_OUTPUT, head_with_marker};
use super::super::tools::{ToolOutput, envelope_parts};
use super::progress::{Observed, ProgressProbe};
use super::{
    AskInfo, CANCEL_GRACE, CallAudit, MAX_PARALLEL_CALLS, RunHost, RunOptions, ToolCall, ToolPlan,
    Verdict, approval_timed_out_text, unrun_text,
};

fn now_ms() -> i64 {
    pingdeck_base::time::now_ms()
}

/// How one call ended, before it is stored.
struct Settled {
    status: ToolCallStatus,
    parts: Vec<HubPart>,
    is_error: bool,
    decided_by: Option<String>,
    decision: Option<&'static str>,
    decided_ms: Option<i64>,
    started_ms: Option<i64>,
    /// The call as it ran (the user may have edited its arguments).
    arguments: String,
    /// The tool's UI link-out, when it has one.
    meta: Option<crate::agent::tools::ToolResultMeta>,
    /// The result is external data: stored inside an `<external_data>` envelope.
    external: bool,
    /// The user's note on the approval, put before the result once it is cut and wrapped.
    note: Option<String>,
    /// The tool's own, smaller cut for the model ([`ToolOutput::cap`]).
    cap: Option<usize>,
    /// The exit status of the process behind the result ([`ToolOutput::exit_code`]).
    exit_code: Option<i32>,
}

impl Settled {
    fn text(status: ToolCallStatus, text: impl Into<String>, call: &ToolCall) -> Self {
        Self {
            is_error: !matches!(status, ToolCallStatus::Ok),
            status,
            parts: vec![HubPart::text(text)],
            decided_by: Some("mode".to_owned()),
            decision: None,
            decided_ms: None,
            started_ms: None,
            arguments: call.arguments.clone(),
            meta: None,
            external: false,
            cap: None,
            exit_code: None,
            note: None,
        }
    }
}

/// The audit state string of a final status (the tool state machine of the UI).
const fn state_of(status: ToolCallStatus) -> &'static str {
    match status {
        ToolCallStatus::Ok => "output-available",
        ToolCallStatus::Error | ToolCallStatus::Interrupted => "output-error",
        ToolCallStatus::Denied | ToolCallStatus::Blocked => "output-denied",
        ToolCallStatus::Cancelled => "output-cancelled",
        ToolCallStatus::ApprovalTimedOut => "approval-timed-out",
        ToolCallStatus::Expired => "approval-expired",
        ToolCallStatus::Superseded => "output-superseded",
    }
}

/// Results that are never kept on disk: a page of kept output, and attachment reads (the
/// original is in the attachment store).
const NO_SPILL: [&str; 2] = [READ_OUTPUT, "read_attachment"];
/// Results the model reads at most this many bytes of before the `read_output` handle: a
/// sub-agent's report.
const SHORT_RESULTS: [(&str, usize); 1] = [("task", 16 * 1024)];
/// Tools whose results carry the content of the chat's folder (file text, names, command output).
const FOLDER_DATA_TOOLS: [&str; 6] = [
    "read_file",
    "grep",
    "glob",
    "list_dir",
    super::super::tools::shell_meta::SHELL,
    "shell_output",
];

/// `parts` with the hidden characters of their text made visible.
fn sanitized_texts(parts: Vec<HubPart>) -> Vec<HubPart> {
    parts
        .into_iter()
        .map(|part| match part {
            HubPart::Text {
                text,
                signature,
                origin,
                citations,
            } => HubPart::Text {
                text: crate::agent::untrusted::sanitize_untrusted(&text).into_owned(),
                signature,
                origin,
                citations,
            },
            other => other,
        })
        .collect()
}

/// How a result was cut for the model.
struct Kept {
    /// Bytes of the full (redacted) text.
    output_bytes: u64,
    truncated: bool,
    spill_ref: Option<String>,
}

/// Prefixes a user's note to a tool result so the model reads it first.
fn with_note(note: Option<&str>, mut parts: Vec<HubPart>) -> Vec<HubPart> {
    if let Some(note) = note.map(str::trim).filter(|note| !note.is_empty()) {
        parts.insert(0, HubPart::text(format!("User note: {note}")));
    }
    parts
}

pub(super) struct CallRun<'a, H> {
    pub host: &'a H,
    pub cancel: &'a CancelToken,
    pub assistant_id: &'a str,
    pub tool_message_id: &'a str,
    pub probe: &'a Mutex<ProgressProbe>,
    /// The cap a window learned from a context-length error puts on one result, below the
    /// host's cap.
    pub learned_cap: Option<usize>,
}

/// Answers `calls` with `text` without running them (the step limit, a call cut off by the output
/// cap).
///
/// # Errors
/// The store's error when a result cannot be saved.
pub(super) async fn refuse_calls<H: RunHost>(
    run: &CallRun<'_, H>,
    calls: &[ToolCall],
    status: ToolCallStatus,
    text: &str,
) -> Result<(), AppError> {
    for call in calls {
        run.finish(call, &unplanned(), Settled::text(status, text, call))
            .await?;
    }
    Ok(())
}

/// The plan of a call that is answered without being classified.
fn unplanned() -> ToolPlan {
    ToolPlan {
        class: crate::agent::tools::ToolClass::Internal,
        risk: "low",
        parallel_safe: false,
        verdict: Verdict::Run,
        decided_by: None,
    }
}

/// Runs the calls of one turn; `true` when the run was cancelled meanwhile.
///
/// # Errors
/// The store's error when a result cannot be saved.
pub(super) async fn execute_calls<H: RunHost>(
    run: &CallRun<'_, H>,
    calls: Vec<ToolCall>,
) -> Result<bool, AppError> {
    let (host, cancel) = (run.host, run.cancel);
    let mut index = 0;
    while index < calls.len() {
        if cancel.is_cancelled() {
            run.finish_unrun(&calls[index..], ToolCallStatus::Cancelled)
                .await?;
            return Ok(true);
        }
        if host.steer_pending() {
            run.finish_unrun(&calls[index..], ToolCallStatus::Superseded)
                .await?;
            return Ok(false);
        }
        let call = &calls[index];
        let plan = host.plan_call(call).await;
        run.record(call, &plan, "input-available").await;
        if plan.verdict == Verdict::Run && plan.parallel_safe {
            let mut batch = vec![(call, plan)];
            let mut next = index + 1;
            while batch.len() < MAX_PARALLEL_CALLS
                && let Some(candidate) = calls.get(next)
                && host.parallel_safe(&candidate.name)
            {
                let planned = host.plan_call(candidate).await;
                if planned.verdict != Verdict::Run || !planned.parallel_safe {
                    break;
                }
                run.record(candidate, &planned, "input-available").await;
                batch.push((candidate, planned));
                next += 1;
            }
            run.run_batch(batch).await?;
            index = next;
        } else {
            let settled = run.settle(call, &plan).await;
            run.finish(call, &plan, settled).await?;
            index += 1;
        }
    }
    Ok(cancel.is_cancelled())
}

impl<H: RunHost> CallRun<'_, H> {
    async fn record(&self, call: &ToolCall, plan: &ToolPlan, state: &'static str) {
        self.host
            .record_call(CallAudit {
                call_id: call.id.clone(),
                name: call.name.clone(),
                class: plan.class,
                state,
                arguments: call.arguments.clone(),
                risk: plan.risk,
                message_id: self.assistant_id.to_owned(),
                decided_by: plan.decided_by.clone(),
                decision: None,
                decided_ms: None,
                started_ms: None,
                ended_ms: None,
                output_bytes: 0,
                truncated: false,
                spill_ref: None,
                exit_code: None,
            })
            .await;
    }

    /// Answers calls that never ran and stores the results.
    async fn finish_unrun(
        &self,
        calls: &[ToolCall],
        status: ToolCallStatus,
    ) -> Result<(), AppError> {
        refuse_calls(self, calls, status, unrun_text(status)).await
    }

    /// Runs a batch of allowed parallel-safe calls at once and stores the results in call order.
    async fn run_batch(&self, batch: Vec<(&ToolCall, ToolPlan)>) -> Result<(), AppError> {
        let settled = futures_util::future::join_all(
            batch.iter().map(|(call, plan)| self.settle(call, plan)),
        )
        .await;
        for ((call, plan), settled) in batch.iter().zip(settled) {
            self.finish(call, plan, settled).await?;
        }
        Ok(())
    }

    /// Decides one call: refuse, ask, run.
    async fn settle(&self, call: &ToolCall, plan: &ToolPlan) -> Settled {
        match &plan.verdict {
            // No policy decided a call that could not run, so the audit row names no decider.
            Verdict::Invalid(text) => Settled {
                decided_by: None,
                ..Settled::text(ToolCallStatus::Error, text.clone(), call)
            },
            Verdict::Refuse(reason) => Settled {
                decision: Some("deny"),
                decided_ms: Some(now_ms()),
                decided_by: Some(plan.decided_by.clone().unwrap_or_else(|| "mode".to_owned())),
                ..Settled::text(ToolCallStatus::Blocked, format!("Not run: {reason}."), call)
            },
            Verdict::Run => {
                let mut settled = self.run_now(call, RunOptions::default(), None).await;
                settled.decided_by =
                    Some(plan.decided_by.clone().unwrap_or_else(|| "mode".to_owned()));
                settled.decision = Some("allow");
                settled.decided_ms = settled.started_ms;
                settled
            }
            Verdict::Ask(ask) => self.ask_then_run(call, plan, ask).await,
        }
    }

    async fn ask_then_run(&self, call: &ToolCall, plan: &ToolPlan, ask: &AskInfo) -> Settled {
        self.record(call, plan, "approval-requested").await;
        let outcome = self.host.request_approval(call, ask).await;
        let decided_ms = Some(now_ms());
        let decided = |mut settled: Settled, by: &'static str, decision: &'static str| {
            settled.decided_by = Some(by.to_owned());
            settled.decision = Some(decision);
            settled.decided_ms = decided_ms;
            settled
        };
        match outcome {
            ApprovalOutcome::Answered(ApprovalAnswer {
                decision: ApprovalDecision::Deny,
                message,
                ..
            }) => {
                let text = match message.as_deref().map(str::trim).filter(|m| !m.is_empty()) {
                    Some(note) => format!("The user denied this call: {note}"),
                    None => "The user denied this call.".to_owned(),
                };
                decided(
                    Settled::text(ToolCallStatus::Denied, text, call),
                    "user",
                    "deny",
                )
            }
            ApprovalOutcome::Answered(answer) => {
                let mut effective = call.clone();
                if let Some(edited) = answer.edited_arguments {
                    effective.arguments = edited;
                    // The edit is the user's own, already reviewed; only a hard refusal or a
                    // call that cannot run at all stops it.
                    match self.host.plan_call(&effective).await.verdict {
                        Verdict::Invalid(text) => {
                            return decided(
                                Settled::text(ToolCallStatus::Error, text, &effective),
                                "user",
                                "allow",
                            );
                        }
                        Verdict::Refuse(reason) => {
                            return decided(
                                Settled::text(
                                    ToolCallStatus::Blocked,
                                    format!("Not run: {reason}."),
                                    &effective,
                                ),
                                "mode",
                                "deny",
                            );
                        }
                        Verdict::Run | Verdict::Ask(_) => {}
                    }
                }
                let options = RunOptions {
                    allow_network_once: answer.allow_network_once,
                };
                let settled = self
                    .run_now(&effective, options, answer.message.as_deref())
                    .await;
                decided(settled, "user", "allow")
            }
            ApprovalOutcome::TimedOut => decided(
                Settled::text(
                    ToolCallStatus::ApprovalTimedOut,
                    approval_timed_out_text(self.host.approval_timeout_s()),
                    call,
                ),
                "timeout",
                "timeout",
            ),
            ApprovalOutcome::Superseded => Settled::text(
                ToolCallStatus::Superseded,
                unrun_text(ToolCallStatus::Superseded),
                call,
            ),
            ApprovalOutcome::Cancelled => Settled::text(
                ToolCallStatus::Expired,
                unrun_text(ToolCallStatus::Expired),
                call,
            ),
        }
    }

    /// Executes the tool, abandoning it a short while after a cancel.
    async fn run_now(&self, call: &ToolCall, options: RunOptions, note: Option<&str>) -> Settled {
        let started_ms = Some(now_ms());
        let run = self.host.run_tool(call, options);
        tokio::pin!(run);
        let (output, cancelled) = tokio::select! {
            output = &mut run => (output, self.cancel.is_cancelled()),
            () = self.cancel.cancelled() => {
                tokio::select! {
                    output = &mut run => (output, true),
                    () = self.host.clock().sleep(CANCEL_GRACE) => (
                        ToolOutput::error("Cancelled before the tool finished."),
                        true,
                    ),
                }
            }
        };
        let superseded = !cancelled && self.host.superseded(&call.id);
        let status = if cancelled {
            ToolCallStatus::Cancelled
        } else if superseded {
            ToolCallStatus::Superseded
        } else if output.is_error {
            ToolCallStatus::Error
        } else {
            ToolCallStatus::Ok
        };
        let parts = if superseded {
            vec![HubPart::text(unrun_text(ToolCallStatus::Superseded))]
        } else {
            output.parts
        };
        Settled {
            status,
            is_error: output.is_error || cancelled || superseded,
            parts,
            meta: output.meta,
            external: output.external && !superseded,
            cap: output.cap.filter(|_| !superseded),
            // A cancelled call was killed: whatever status it reports is not an outcome.
            exit_code: output.exit_code.filter(|_| !cancelled && !superseded),
            note: note.filter(|_| !superseded).map(str::to_owned),
            decided_by: None,
            decision: None,
            decided_ms: None,
            started_ms,
            arguments: call.arguments.clone(),
        }
    }

    /// Redacts a result and cuts it to the model's cap: a cut result's full text is kept
    /// on disk first and the model sees its start with a `read_output` handle; when nothing can
    /// be kept, head and tail stay as before.
    async fn cap_result(
        &self,
        call: &ToolCall,
        parts: Vec<HubPart>,
        own_cap: Option<usize>,
    ) -> (Vec<HubPart>, Kept) {
        let host_cap = self
            .learned_cap
            .map_or(self.host.result_cap_bytes(), |learned| {
                self.host.result_cap_bytes().min(learned)
            });
        let cap = SHORT_RESULTS
            .iter()
            .find(|(name, _)| *name == call.name)
            .map_or(host_cap, |(_, short)| host_cap.min(*short));
        let cap = own_cap.map_or(cap, |own| cap.min(own));
        let parts = self.host.redact(parts);
        let text = text_of(&parts);
        let output_bytes = u64::try_from(text.len()).unwrap_or(u64::MAX);
        let over = parts
            .iter()
            .any(|part| matches!(part, HubPart::Text { text, .. } if text.len() > cap));
        if !over {
            let kept = Kept {
                output_bytes,
                truncated: false,
                spill_ref: None,
            };
            return (parts, kept);
        }
        if !NO_SPILL.contains(&call.name.as_str())
            && let Some(total) = self.host.spill_output(&call.id, text.clone()).await
        {
            let mut cut = vec![HubPart::text(head_with_marker(&text, cap, &call.id, total))];
            cut.extend(
                parts
                    .into_iter()
                    .filter(|part| !matches!(part, HubPart::Text { .. })),
            );
            let kept = Kept {
                output_bytes,
                truncated: true,
                spill_ref: Some(call.id.clone()),
            };
            return (cut, kept);
        }
        let kept = Kept {
            output_bytes,
            truncated: true,
            spill_ref: None,
        };
        (cap_text_parts(parts, cap), kept)
    }

    /// Stores the result of a call and publishes it.
    async fn finish(
        &self,
        call: &ToolCall,
        plan: &ToolPlan,
        settled: Settled,
    ) -> Result<(), AppError> {
        let ended_ms = now_ms();
        let (parts, kept) = self.cap_result(call, settled.parts, settled.cap).await;
        let observed = text_of(&parts);
        // What the file tools and the shell read from a folder the user has not trusted is
        // external data like a web page: it is enveloped with its hidden characters shown.
        let folder_data = !settled.external
            && FOLDER_DATA_TOOLS.contains(&call.name.as_str())
            && self.host.folder_untrusted();
        // Wrapped once, here, and stored: later steps resend the same bytes (R3).
        let parts = if folder_data {
            envelope_parts(&call.name, sanitized_texts(parts))
        } else if settled.external {
            envelope_parts(&call.name, parts)
        } else {
            parts
        };
        let mut parts = with_note(settled.note.as_deref(), parts);
        let nudge = self
            .probe
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .observe(&Observed {
                name: &call.name,
                arguments: &settled.arguments,
                status: settled.status,
                denied_by_user: settled.decided_by.as_deref() == Some("user")
                    && settled.decision == Some("deny"),
                result: &observed,
            });
        if let Some(nudge) = nudge {
            parts.push(HubPart::text(nudge.text));
            self.host.emit(AgentEvent::coded_notice(
                "no_progress",
                format!(
                    "The agent seems stuck ({}); it was asked to change approach.",
                    nudge.signal.as_str()
                ),
                &[
                    ("signal", nudge.signal.as_str().to_owned()),
                    ("tool", call.name.clone()),
                ],
            ));
        }
        let duration_ms = settled
            .started_ms
            .map_or(0, |started| u64::try_from(ended_ms - started).unwrap_or(0));
        self.host
            .save_tool_result(
                self.tool_message_id,
                HubPart::ToolResult {
                    call_id: call.id.clone(),
                    name: Some(call.name.clone()),
                    content: parts.clone(),
                    is_error: settled.is_error,
                },
            )
            .await?;
        self.host
            .record_call(CallAudit {
                call_id: call.id.clone(),
                name: call.name.clone(),
                class: plan.class,
                state: state_of(settled.status),
                arguments: settled.arguments,
                risk: plan.risk,
                message_id: self.assistant_id.to_owned(),
                decided_by: settled.decided_by,
                decision: settled.decision,
                decided_ms: settled.decided_ms,
                started_ms: settled.started_ms,
                ended_ms: Some(ended_ms),
                output_bytes: kept.output_bytes,
                truncated: kept.truncated,
                spill_ref: kept.spill_ref,
                exit_code: settled.exit_code,
            })
            .await;
        self.host.emit(AgentEvent::ToolResult {
            call_id: call.id.clone(),
            name: call.name.clone(),
            status: settled.status,
            parts,
            duration_ms,
            meta: settled.meta,
            exit_code: settled.exit_code,
        });
        Ok(())
    }
}
