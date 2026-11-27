//! Execution topology & waterfall timeline assembly for top-level agent runs.
//!
//! Reconstructs deterministic serial waves and parallel fan-out batches (both concurrent `task`
//! sub-agents and parallel-safe tool batches) by merging live [`ActiveRun`] memory snapshots with
//! persisted `agent_runs` and `agent_tool_calls` rows — with zero SQLite schema changes.

use std::collections::{HashMap, HashSet};

use crate::agent::attention::PendingApprovalSummary;
use crate::agent::dto::{
    AgentRunTopology, BoardChanges, BoardSubagentCounts, BoardWait, ExecutionWave,
    SessionRunHistoryItem, TopologySubagent, TopologyToolStep,
};
use crate::agent::model::{
    AgentMessage, AgentRole, AgentRunKind, AgentRunStatus, AgentRunSummary, AgentTodoStatus,
    AgentToolCallRecord, FileChange, LiveRunSnapshot, LiveToolState, ToolCallStatus,
};
use crate::agent_tools::subagent::TASK;
use crate::error::AppError;
use crate::llm::hub::HubPart;

use super::board::COST_LIMIT_STEP;
use crate::app::{AppState, command_error, current_time_ms};

const LAST_ANSWER_CHARS: usize = 180;
const SUMMARY_MAX_CHARS: usize = 96;
const ARGS_PREVIEW_MAX_CHARS: usize = 3_000;
const OUTPUT_PREVIEW_MAX_CHARS: usize = 4_800;
const PROMPT_PREVIEW_MAX_CHARS: usize = 1_600;
const REASONING_PREVIEW_MAX_CHARS: usize = 3_200;
const REPORT_PREVIEW_MAX_CHARS: usize = 12_000;

/// Returns the full execution topology (serial waves, parallel sub-agent fan-outs, tool steps,
/// and waterfall timestamps) of one top-level run.
///
/// # Errors
/// [`AppError::NotFound`] when `run_id` does not exist; [`AppError::Database`] on store error.
pub fn agent_run_topology_impl(
    state: &AppState,
    run_id: &str,
) -> Result<AgentRunTopology, AppError> {
    let snapshot_ms = current_time_ms();
    let live_parent = state.agent.runs.get(run_id);
    let store = state.store();
    let parent_summary = match &live_parent {
        Some(run) => run.summary(),
        None => store
            .get_agent_run(run_id)
            .map_err(command_error)?
            .ok_or_else(|| AppError::not_found(format!("agent run {run_id} not found")))?,
    };
    let parent_live_snap = live_parent.as_ref().map(|r| r.live_snapshot());
    let session = store
        .get_agent_session(&parent_summary.session_id)
        .map_err(command_error)?;
    let extra = store
        .agent_run_extras(&[run_id.to_owned()])
        .map_err(command_error)?
        .remove(run_id)
        .unwrap_or_default();
    let changes = store.agent_pending_changes(run_id).map_err(command_error)?;
    let last_answer = store
        .agent_run_last_answer(run_id, LAST_ANSWER_CHARS)
        .ok()
        .flatten();
    let queued_turns = u32::try_from(
        store
            .list_queue(&parent_summary.session_id)
            .unwrap_or_default()
            .len(),
    )
    .unwrap_or(u32::MAX);
    let queue_paused = state.agent.runs.queue_paused(&parent_summary.session_id);

    // Load persisted child sub-agents and overlay live in-memory runs.
    let stored_children = store.list_child_agent_runs(run_id).map_err(command_error)?;
    let mut children_map: HashMap<String, (AgentRunSummary, String, u64, Option<LiveRunSnapshot>)> =
        HashMap::new();
    let mut child_order: Vec<String> = Vec::new();
    for (summary, title) in stored_children {
        let cid = summary.run_id.clone();
        let live_child = state.agent.runs.get(&cid);
        let merged = live_child.as_ref().map_or(summary, |r| r.summary());
        let awake = live_child.as_ref().map_or_else(
            || {
                merged
                    .finished_at_ms
                    .unwrap_or(snapshot_ms)
                    .saturating_sub(merged.started_at_ms)
                    .max(0) as u64
            },
            |r| r.awake_ms(),
        );
        let snap = live_child.as_ref().map(|r| r.live_snapshot());
        child_order.push(cid.clone());
        children_map.insert(cid, (merged, title, awake, snap));
    }
    for live_child in state.agent.runs.active_runs() {
        if live_child.kind == AgentRunKind::Subagent
            && live_child.parent_run_id.as_deref() == Some(run_id)
            && !children_map.contains_key(&live_child.run_id)
        {
            let summary = live_child.summary();
            let title = store
                .get_agent_session(&summary.session_id)
                .ok()
                .flatten()
                .map(|s| s.title)
                .unwrap_or_default();
            let awake = live_child.awake_ms();
            let snap = Some(live_child.live_snapshot());
            child_order.push(live_child.run_id.clone());
            children_map.insert(live_child.run_id.clone(), (summary, title, awake, snap));
        }
    }

    // Batch-read all tool audit rows for the parent run and all its child runs.
    let mut all_run_ids = Vec::with_capacity(1 + child_order.len());
    all_run_ids.push(run_id.to_owned());
    all_run_ids.extend(child_order.iter().cloned());
    let all_calls = store
        .list_runs_tool_calls(&all_run_ids)
        .map_err(command_error)?;
    let mut calls_by_run: HashMap<String, Vec<AgentToolCallRecord>> = HashMap::new();
    for rec in all_calls {
        calls_by_run
            .entry(rec.run_id.clone())
            .or_default()
            .push(rec);
    }

    // Index active approvals by run_id (both parent and children).
    let mut approvals_by_run: HashMap<String, PendingApprovalSummary> = HashMap::new();
    for appr in state.agent.approvals.list() {
        approvals_by_run
            .entry(appr.run_id.clone())
            .and_modify(|existing| {
                if (appr.requested_ms, &appr.call_id) < (existing.requested_ms, &existing.call_id) {
                    *existing = appr.clone();
                }
            })
            .or_insert(appr);
    }

    let parent_messages = store
        .list_agent_messages(&parent_summary.session_id)
        .unwrap_or_default();
    let parent_outputs = collect_tool_outputs(&parent_messages);
    let (user_prompt, mut reasoning_preview, mut full_answer) =
        extract_run_narrative(&parent_messages, run_id, parent_summary.started_at_ms);
    if let Some(snap) = &parent_live_snap {
        if reasoning_preview.is_none() {
            reasoning_preview = extract_live_reasoning(snap);
        }
        if full_answer.is_none() {
            full_answer = extract_live_text(snap);
        }
    }
    let chat_file_changes = store
        .list_chat_file_changes(&parent_summary.session_id)
        .unwrap_or_default();
    let changes_by_call: HashMap<String, FileChange> = chat_file_changes
        .into_iter()
        .map(|fc| (fc.call_id.clone(), fc))
        .collect();

    // Parent tool call args lookup by call_id so subagents can read `kind` / `resume` / `description`.
    let parent_records = calls_by_run.remove(run_id).unwrap_or_default();
    let mut parent_args_by_call: HashMap<String, String> = HashMap::new();
    for rec in &parent_records {
        parent_args_by_call.insert(rec.call_id.clone(), rec.args_json.clone());
    }
    if let Some(snap) = &parent_live_snap {
        for part in &snap.parts {
            if let HubPart::ToolCall { id, arguments, .. } = part {
                parent_args_by_call
                    .entry(id.clone())
                    .or_insert_with(|| arguments.clone());
            }
        }
    }

    // Assemble TopologySubagent nodes and aggregate counts.
    let mut subagent_counts = BoardSubagentCounts::default();
    let mut subagents_by_parent_call: HashMap<String, TopologySubagent> = HashMap::new();
    let mut orphan_subagents: Vec<TopologySubagent> = Vec::new();
    let mut child_approval_fallback: Option<PendingApprovalSummary> = None;

    for cid in &child_order {
        let Some((summary, title, awake_ms, snap)) = children_map.remove(cid) else {
            continue;
        };
        subagent_counts.total = subagent_counts.total.saturating_add(1);
        match summary.status {
            AgentRunStatus::Queued | AgentRunStatus::Running => {
                subagent_counts.active = subagent_counts.active.saturating_add(1);
            }
            AgentRunStatus::WaitingApproval => {
                subagent_counts.waiting = subagent_counts.waiting.saturating_add(1);
            }
            AgentRunStatus::Completed => {
                subagent_counts.completed = subagent_counts.completed.saturating_add(1);
            }
            AgentRunStatus::Failed | AgentRunStatus::Interrupted => {
                subagent_counts.failed = subagent_counts.failed.saturating_add(1);
            }
            AgentRunStatus::Cancelled => {
                subagent_counts.cancelled = subagent_counts.cancelled.saturating_add(1);
            }
        }
        let child_approval = approvals_by_run.get(&summary.run_id).cloned();
        if let Some(ref ca) = child_approval {
            match &child_approval_fallback {
                Some(prev) if prev.requested_ms <= ca.requested_ms => {}
                _ => child_approval_fallback = Some(ca.clone()),
            }
        }
        let child_records = calls_by_run.remove(&summary.run_id).unwrap_or_default();
        let child_messages = store
            .list_agent_messages(&summary.session_id)
            .unwrap_or_default();
        let child_outputs = collect_tool_outputs(&child_messages);
        let (_, _, mut child_full_report) =
            extract_run_narrative(&child_messages, &summary.run_id, summary.started_at_ms);
        if child_full_report.is_none()
            && let Some(live_s) = snap.as_ref()
        {
            child_full_report = extract_live_text(live_s);
        }
        let child_steps = merge_tool_steps(
            &child_records,
            snap.as_ref(),
            &child_outputs,
            &changes_by_call,
        );
        let parent_call_id = summary.parent_call_id.clone().unwrap_or_default();
        let parsed = parent_args_by_call
            .get(&parent_call_id)
            .map(|a| parse_task_args(a))
            .unwrap_or_default();
        let description = if !title.trim().is_empty() {
            title
        } else if !parsed.description.is_empty() {
            parsed.description
        } else {
            summary.model_label.clone()
        };
        let child_answer = if !summary.status.is_active() {
            store
                .agent_run_last_answer(&summary.run_id, LAST_ANSWER_CHARS)
                .ok()
                .flatten()
        } else {
            None
        };
        let node = TopologySubagent {
            child_session_id: summary.session_id.clone(),
            parent_call_id: parent_call_id.clone(),
            description,
            subagent_kind: parsed.kind,
            resumed: parsed.resumed,
            awake_ms,
            pending_approval: child_approval,
            last_answer: child_answer,
            tool_calls: child_steps,
            brief: parsed.brief,
            full_report: child_full_report,
            run: summary,
        };
        if !parent_call_id.is_empty() {
            subagents_by_parent_call.insert(parent_call_id, node);
        } else {
            orphan_subagents.push(node);
        }
    }

    let waves = build_execution_waves(
        &parent_records,
        parent_live_snap.as_ref(),
        &parent_outputs,
        &changes_by_call,
        &mut subagents_by_parent_call,
        orphan_subagents,
    );

    let pending_approval = approvals_by_run
        .get(run_id)
        .cloned()
        .or(child_approval_fallback);
    let wait = if pending_approval.is_some() {
        Some(BoardWait::Approval)
    } else if parent_summary.status == AgentRunStatus::WaitingApproval
        && parent_summary.last_step.as_deref() == Some(COST_LIMIT_STEP)
    {
        Some(BoardWait::CostLimit)
    } else {
        None
    };

    let task_graph = session
        .as_ref()
        .map(|sess| overlay_task_graph_runtime(&sess.todos, &waves))
        .unwrap_or_default();

    let (plan_done, plan_total) = session.as_ref().map_or((0, 0), |sess| {
        let counted = sess
            .todos
            .iter()
            .filter(|t| t.status != AgentTodoStatus::Cancelled);
        let done = counted
            .clone()
            .filter(|t| t.status == AgentTodoStatus::Completed)
            .count();
        (
            u32::try_from(done).unwrap_or(u32::MAX),
            u32::try_from(counted.count()).unwrap_or(u32::MAX),
        )
    });

    let awake_ms = live_parent
        .as_ref()
        .map_or(extra.awake_ms, |r| r.awake_ms());
    let cost_currency = extra
        .cost_currency
        .or_else(|| parent_summary.cost_currency.clone());

    Ok(AgentRunTopology {
        waiting_for_slot: parent_summary.status == AgentRunStatus::Queued,
        title: session
            .as_ref()
            .map_or_else(String::new, |s| s.title.clone()),
        folder: session.as_ref().and_then(|s| s.workspace_dir.clone()),
        run: parent_summary,
        awake_ms,
        snapshot_ms,
        wait,
        pending_approval,
        pending_changes: BoardChanges {
            files: changes.files,
            added: changes.added,
            removed: changes.removed,
        },
        plan_done,
        plan_total,
        cost_currency,
        last_answer,
        queued_turns,
        queue_paused,
        waves,
        subagent_counts,
        task_graph,
        user_prompt,
        reasoning_preview,
        full_answer,
    })
}

/// Binds matching runtime sub-agents onto structured Task Graph nodes (`run_id` or normalized
/// description match) so nodes without an explicit `run_id` still reflect live runtime progress.
fn overlay_task_graph_runtime(
    todos: &[crate::agent::model::AgentTodoItem],
    waves: &[ExecutionWave],
) -> Vec<crate::agent::model::AgentTodoItem> {
    if todos.is_empty() {
        return Vec::new();
    }
    let mut sub_by_id: HashMap<&str, &TopologySubagent> = HashMap::new();
    let mut sub_by_desc: HashMap<String, &TopologySubagent> = HashMap::new();
    for wave in waves {
        match wave {
            ExecutionWave::ParallelSubagents { subagents, .. } => {
                for sub in subagents {
                    sub_by_id.insert(sub.run.run_id.as_str(), sub);
                    sub_by_desc.insert(sub.description.trim().to_lowercase(), sub);
                }
            }
            ExecutionWave::SingleSubagent { subagent, .. } => {
                sub_by_id.insert(subagent.run.run_id.as_str(), subagent);
                sub_by_desc.insert(subagent.description.trim().to_lowercase(), subagent);
            }
            _ => {}
        }
    }

    todos
        .iter()
        .cloned()
        .map(|mut item| {
            let matched = item
                .run_id
                .as_deref()
                .and_then(|rid| sub_by_id.get(rid).copied())
                .or_else(|| {
                    sub_by_desc
                        .get(&item.content.trim().to_lowercase())
                        .copied()
                });
            if let Some(sub) = matched {
                if item.run_id.is_none() {
                    item.run_id = Some(sub.run.run_id.clone());
                }
                if item.error.is_none() && sub.run.error.is_some() {
                    item.error = sub.run.error.clone();
                }
                if item.summary.is_none() && sub.last_answer.is_some() {
                    item.summary = sub.last_answer.clone();
                }
                if matches!(
                    item.status,
                    AgentTodoStatus::Pending | AgentTodoStatus::InProgress
                ) {
                    item.status = match sub.run.status {
                        AgentRunStatus::Running | AgentRunStatus::WaitingApproval => {
                            AgentTodoStatus::InProgress
                        }
                        AgentRunStatus::Completed => AgentTodoStatus::Completed,
                        AgentRunStatus::Failed | AgentRunStatus::Interrupted => {
                            AgentTodoStatus::Failed
                        }
                        AgentRunStatus::Cancelled => AgentTodoStatus::Cancelled,
                        AgentRunStatus::Queued => item.status,
                    };
                }
            }
            item
        })
        .collect()
}

/// Returns the execution topology of `session_id`'s active top-level run, or its most recent
/// persisted `main` run when idle.
///
/// # Errors
/// [`AppError::Database`] on store error.
pub fn agent_session_latest_topology_impl(
    state: &AppState,
    session_id: &str,
) -> Result<Option<AgentRunTopology>, AppError> {
    if let Some(active) = state.agent.runs.active_for_session(session_id)
        && active.kind == AgentRunKind::Main
    {
        return agent_run_topology_impl(state, &active.run_id).map(Some);
    }
    let latest_id = state
        .store()
        .list_session_main_runs(session_id, 1)
        .map_err(command_error)?
        .into_iter()
        .next()
        .map(|r| r.run_id);
    match latest_id {
        Some(run_id) => agent_run_topology_impl(state, &run_id).map(Some),
        None => Ok(None),
    }
}

/// Lists a chat session's top-level (`main`) runs, newest first, overlaying live summaries.
///
/// # Errors
/// [`AppError::Database`] on store error.
pub fn agent_session_runs_impl(
    state: &AppState,
    session_id: &str,
    limit: Option<u32>,
) -> Result<Vec<AgentRunSummary>, AppError> {
    let lim = limit.unwrap_or(20).clamp(1, 100);
    let mut runs = state
        .store()
        .list_session_main_runs(session_id, lim)
        .map_err(command_error)?;
    if let Some(active) = state.agent.runs.active_for_session(session_id)
        && active.kind == AgentRunKind::Main
    {
        let live_summary = active.summary();
        if let Some(slot) = runs.iter_mut().find(|r| r.run_id == live_summary.run_id) {
            *slot = live_summary;
        } else {
            runs.insert(0, live_summary);
            runs.truncate(lim as usize);
        }
    }
    Ok(runs)
}

/// Lists a chat session's top-level (`main`) runs enriched with prompt snippet, node counts,
/// parallel/TaskGraph flags, and pending file-change totals for the Run History switcher.
///
/// # Errors
/// [`AppError::Database`] on store error.
pub fn agent_session_run_history_impl(
    state: &AppState,
    session_id: &str,
    limit: Option<u32>,
) -> Result<Vec<SessionRunHistoryItem>, AppError> {
    let runs = agent_session_runs_impl(state, session_id, limit)?;
    build_run_history_items(state, runs)
}

/// Lists top-level (`main`) runs across all chat sessions, newest first, for the global Run
/// Monitor's full-history tab beyond the 24-hour board window.
///
/// # Errors
/// [`AppError::Database`] on store error.
pub fn agent_all_runs_history_impl(
    state: &AppState,
    limit: Option<u32>,
) -> Result<Vec<SessionRunHistoryItem>, AppError> {
    let lim = limit.unwrap_or(60).clamp(1, 200);
    let mut runs: Vec<AgentRunSummary> = state
        .store()
        .list_agent_runs(None, false, lim.saturating_mul(2))
        .map_err(command_error)?
        .into_iter()
        .filter(|r| r.kind == AgentRunKind::Main)
        .collect();
    for active in state.agent.runs.active_runs() {
        if active.kind != AgentRunKind::Main {
            continue;
        }
        let live_summary = active.summary();
        if let Some(slot) = runs.iter_mut().find(|r| r.run_id == live_summary.run_id) {
            *slot = live_summary;
        } else {
            runs.push(live_summary);
        }
    }
    runs.sort_by(|a, b| {
        b.started_at_ms
            .cmp(&a.started_at_ms)
            .then_with(|| b.run_id.cmp(&a.run_id))
    });
    runs.truncate(lim as usize);
    build_run_history_items(state, runs)
}

fn build_run_history_items(
    state: &AppState,
    runs: Vec<AgentRunSummary>,
) -> Result<Vec<SessionRunHistoryItem>, AppError> {
    if runs.is_empty() {
        return Ok(Vec::new());
    }
    let snapshot_ms = current_time_ms();
    let store = state.store();
    let run_ids: Vec<String> = runs.iter().map(|r| r.run_id.clone()).collect();
    let mut extras = store.agent_run_extras(&run_ids).map_err(command_error)?;
    let all_calls = store
        .list_runs_tool_calls(&run_ids)
        .map_err(command_error)?;
    let mut calls_by_run: HashMap<String, Vec<AgentToolCallRecord>> = HashMap::new();
    for rec in all_calls {
        calls_by_run
            .entry(rec.run_id.clone())
            .or_default()
            .push(rec);
    }

    let mut session_meta: HashMap<String, (String, bool, Vec<AgentMessage>)> = HashMap::new();
    let mut out = Vec::with_capacity(runs.len());
    for run in runs {
        let (title, has_task_graph, msgs) = session_meta
            .entry(run.session_id.clone())
            .or_insert_with(|| {
                let sess = store.get_agent_session(&run.session_id).ok().flatten();
                let title = sess.as_ref().map_or_else(String::new, |s| s.title.clone());
                let has_tg = sess
                    .as_ref()
                    .is_some_and(|s| s.todos.iter().any(|t| t.has_graph_metadata()));
                let msgs = store
                    .list_agent_messages(&run.session_id)
                    .unwrap_or_default();
                (title, has_tg, msgs)
            });

        let extra = extras.remove(&run.run_id).unwrap_or_default();
        let live = state.agent.runs.get(&run.run_id);
        let awake_ms = live.as_ref().map_or_else(
            || {
                if extra.awake_ms > 0 {
                    extra.awake_ms
                } else {
                    run.finished_at_ms
                        .unwrap_or(snapshot_ms)
                        .saturating_sub(run.started_at_ms)
                        .max(0) as u64
                }
            },
            |r| r.awake_ms(),
        );
        let records = calls_by_run.remove(&run.run_id).unwrap_or_default();
        let tool_count = u32::try_from(records.len()).unwrap_or(u32::MAX);
        let subagent_count =
            u32::try_from(records.iter().filter(|r| r.name == TASK).count()).unwrap_or(u32::MAX);
        let has_parallel = detect_parallel_records(&records);
        let changes = store.agent_pending_changes(&run.run_id).unwrap_or_default();
        let prompt_preview = extract_user_prompt_first_line(msgs, run.started_at_ms);
        let cost_currency = extra.cost_currency.or_else(|| run.cost_currency.clone());

        out.push(SessionRunHistoryItem {
            title: title.clone(),
            prompt_preview,
            awake_ms,
            tool_count,
            subagent_count,
            has_parallel,
            has_task_graph: *has_task_graph,
            pending_changes: BoardChanges {
                files: changes.files,
                added: changes.added,
                removed: changes.removed,
            },
            cost_currency,
            run,
        });
    }
    Ok(out)
}

fn detect_parallel_records(records: &[AgentToolCallRecord]) -> bool {
    let mut i = 0;
    while i < records.len() {
        let msg = &records[i].message_id;
        let is_task = records[i].name == TASK;
        let is_par = is_parallel_safe_tool(&records[i].name, &records[i].class);
        if is_task || is_par {
            let mut j = i + 1;
            while j < records.len()
                && &records[j].message_id == msg
                && (if is_task {
                    records[j].name == TASK
                } else {
                    records[j].name != TASK
                        && is_parallel_safe_tool(&records[j].name, &records[j].class)
                })
            {
                j += 1;
            }
            if j - i >= 2 {
                return true;
            }
            i = j;
        } else {
            i += 1;
        }
    }
    false
}

#[derive(Default)]
struct ParsedTaskArgs {
    description: String,
    kind: String,
    resumed: bool,
    brief: Option<String>,
}

fn parse_task_args(raw: &str) -> ParsedTaskArgs {
    let Ok(val) = serde_json::from_str::<serde_json::Value>(raw) else {
        return ParsedTaskArgs {
            description: String::new(),
            kind: "general".to_owned(),
            resumed: false,
            brief: None,
        };
    };
    let description = val
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
    let brief = ["prompt", "brief", "instructions", "task"]
        .iter()
        .find_map(|k| val.get(*k).and_then(|v| v.as_str()))
        .map(str::trim)
        .filter(|s| !s.is_empty())
        .map(|s| truncate_chars(s, PROMPT_PREVIEW_MAX_CHARS));
    ParsedTaskArgs {
        description,
        kind,
        resumed,
        brief,
    }
}

fn collect_tool_outputs(messages: &[AgentMessage]) -> HashMap<String, String> {
    let mut map = HashMap::new();
    for msg in messages {
        for part in &msg.parts {
            if let HubPart::ToolResult {
                call_id, content, ..
            } = part
            {
                let mut buf = String::new();
                for sub in content {
                    if let HubPart::Text { text, .. } = sub {
                        if !buf.is_empty() {
                            buf.push('\n');
                        }
                        buf.push_str(text);
                    }
                }
                let trimmed = buf.trim();
                if !trimmed.is_empty() {
                    map.insert(call_id.clone(), trimmed.to_owned());
                }
            }
        }
    }
    map
}

fn extract_run_narrative(
    messages: &[AgentMessage],
    run_id: &str,
    run_started_ms: i64,
) -> (Option<String>, Option<String>, Option<String>) {
    let mut reasoning_chunks: Vec<&str> = Vec::new();
    let mut answer_chunks: Vec<&str> = Vec::new();
    let mut first_run_msg_idx: Option<usize> = None;

    for (idx, msg) in messages.iter().enumerate() {
        if msg.run_id.as_deref() == Some(run_id) && msg.role == AgentRole::Assistant {
            if first_run_msg_idx.is_none() {
                first_run_msg_idx = Some(idx);
            }
            for part in &msg.parts {
                match part {
                    HubPart::Reasoning { text, .. } if !text.trim().is_empty() => {
                        reasoning_chunks.push(text.trim());
                    }
                    HubPart::Text { text, .. } if !text.trim().is_empty() => {
                        answer_chunks.push(text.trim());
                    }
                    _ => {}
                }
            }
        }
    }

    let user_prompt = first_run_msg_idx
        .and_then(|idx| {
            messages[..idx]
                .iter()
                .rev()
                .find(|m| m.role == AgentRole::User)
        })
        .or_else(|| {
            messages
                .iter()
                .rev()
                .find(|m| m.role == AgentRole::User && m.created_at_ms <= run_started_ms + 5_000)
        })
        .and_then(extract_message_text)
        .map(|s| truncate_chars(&s, PROMPT_PREVIEW_MAX_CHARS));

    let reasoning_preview = (!reasoning_chunks.is_empty())
        .then(|| truncate_chars(&reasoning_chunks.join("\n\n"), REASONING_PREVIEW_MAX_CHARS));
    let full_answer = answer_chunks
        .last()
        .map(|s| truncate_chars(s, REPORT_PREVIEW_MAX_CHARS));

    (user_prompt, reasoning_preview, full_answer)
}

fn extract_user_prompt_first_line(
    messages: &[AgentMessage],
    run_started_ms: i64,
) -> Option<String> {
    messages
        .iter()
        .rev()
        .find(|m| m.role == AgentRole::User && m.created_at_ms <= run_started_ms + 5_000)
        .and_then(extract_message_text)
        .and_then(|text| {
            text.lines()
                .map(str::trim)
                .find(|l| !l.is_empty())
                .map(|l| truncate_chars(l, SUMMARY_MAX_CHARS))
        })
}

fn extract_message_text(msg: &AgentMessage) -> Option<String> {
    let mut out = String::new();
    for part in &msg.parts {
        if let HubPart::Text { text, .. } = part
            && !text.trim().is_empty()
        {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text.trim());
        }
    }
    (!out.is_empty()).then_some(out)
}

fn extract_live_reasoning(snap: &LiveRunSnapshot) -> Option<String> {
    let mut out = String::new();
    for part in &snap.parts {
        if let HubPart::Reasoning { text, .. } = part
            && !text.trim().is_empty()
        {
            if !out.is_empty() {
                out.push_str("\n\n");
            }
            out.push_str(text.trim());
        }
    }
    (!out.is_empty()).then(|| truncate_chars(&out, REASONING_PREVIEW_MAX_CHARS))
}

fn extract_live_text(snap: &LiveRunSnapshot) -> Option<String> {
    let mut out = String::new();
    for part in &snap.parts {
        if let HubPart::Text { text, .. } = part
            && !text.trim().is_empty()
        {
            if !out.is_empty() {
                out.push('\n');
            }
            out.push_str(text.trim());
        }
    }
    (!out.is_empty()).then(|| truncate_chars(&out, REPORT_PREVIEW_MAX_CHARS))
}

fn format_args_preview(args: &str) -> Option<String> {
    let trimmed = args.trim();
    if trimmed.is_empty() || trimmed == "{}" {
        return None;
    }
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(trimmed)
        && let Ok(pretty) = serde_json::to_string_pretty(&val)
    {
        return Some(truncate_chars(&pretty, ARGS_PREVIEW_MAX_CHARS));
    }
    Some(truncate_chars(trimmed, ARGS_PREVIEW_MAX_CHARS))
}

fn format_output_preview(raw: &str, class: &str) -> Option<String> {
    let trimmed = raw.trim();
    if trimmed.is_empty() {
        return None;
    }
    if class == "exec" && trimmed.chars().count() > OUTPUT_PREVIEW_MAX_CHARS {
        let total = trimmed.chars().count();
        let skip = total.saturating_sub(OUTPUT_PREVIEW_MAX_CHARS);
        let tail: String = trimmed.chars().skip(skip).collect();
        return Some(format!("…\n{tail}"));
    }
    Some(truncate_chars(trimmed, OUTPUT_PREVIEW_MAX_CHARS))
}

fn is_parallel_safe_tool(name: &str, class: &str) -> bool {
    if name == TASK {
        return true;
    }
    match class {
        "read" | "spanbay_read" | "network" => true,
        "internal" => matches!(name, "read_output" | "read_attachment" | "read_skill"),
        _ => false,
    }
}

fn normalize_audit_status(state: &str) -> &'static str {
    match state {
        "output-available" => "ok",
        "output-error" => "error",
        "output-denied" => "denied",
        "output-cancelled" => "cancelled",
        "approval-timed-out" => "timed_out",
        "approval-expired" | "output-superseded" => "superseded",
        "approval-requested" => "waiting_approval",
        _ => "running",
    }
}

fn normalize_live_status(state: LiveToolState, result: Option<ToolCallStatus>) -> &'static str {
    if let Some(res) = result {
        return match res {
            ToolCallStatus::Ok => "ok",
            ToolCallStatus::Error | ToolCallStatus::Interrupted => "error",
            ToolCallStatus::Denied | ToolCallStatus::Blocked => "denied",
            ToolCallStatus::Cancelled => "cancelled",
            ToolCallStatus::ApprovalTimedOut => "timed_out",
            ToolCallStatus::Expired | ToolCallStatus::Superseded => "superseded",
        };
    }
    match state {
        LiveToolState::WaitingApproval => "waiting_approval",
        LiveToolState::Done => "ok",
        LiveToolState::StreamingArgs | LiveToolState::Running => "running",
    }
}

fn summarize_args(args: &str) -> String {
    if let Ok(val) = serde_json::from_str::<serde_json::Value>(args)
        && let Some(obj) = val.as_object()
    {
        for key in [
            "description",
            "cmd",
            "command",
            "path",
            "file_path",
            "url",
            "query",
            "pattern",
            "name",
        ] {
            if let Some(s) = obj.get(key).and_then(|v| v.as_str()) {
                return truncate_chars(s.trim(), SUMMARY_MAX_CHARS);
            }
        }
    }
    truncate_chars(args.trim(), SUMMARY_MAX_CHARS)
}

fn truncate_chars(s: &str, max: usize) -> String {
    let mut chars = s.chars();
    let head: String = chars.by_ref().take(max).collect();
    if chars.next().is_some() {
        format!("{head}…")
    } else {
        head
    }
}

fn record_to_step(
    rec: &AgentToolCallRecord,
    outputs: &HashMap<String, String>,
    changes: &HashMap<String, FileChange>,
) -> TopologyToolStep {
    let duration_ms = match (rec.started_ms, rec.ended_ms) {
        (Some(s), Some(e)) if e >= s => Some((e - s) as u64),
        _ => None,
    };
    let fc = changes.get(&rec.call_id);
    TopologyToolStep {
        call_id: rec.call_id.clone(),
        name: rec.name.clone(),
        class: rec.class.clone(),
        summary: summarize_args(&rec.args_json),
        status: normalize_audit_status(&rec.state).to_owned(),
        started_ms: rec.started_ms.or(rec.requested_ms),
        ended_ms: rec.ended_ms,
        duration_ms,
        exit_code: rec.exit_code,
        parallel_safe: is_parallel_safe_tool(&rec.name, &rec.class),
        args_preview: format_args_preview(&rec.args_json),
        output_preview: outputs
            .get(&rec.call_id)
            .and_then(|s| format_output_preview(s, &rec.class)),
        risk: (!rec.risk.trim().is_empty()).then(|| rec.risk.clone()),
        decision: rec.decision.clone(),
        decided_by: rec.decided_by.clone(),
        sandbox: rec.sandbox.clone(),
        output_bytes: rec.output_bytes,
        truncated: rec.truncated || rec.spill_ref.is_some(),
        file_change_id: fc.map(|c| c.id.clone()),
        file_path: fc.map(|c| c.path.clone()),
        file_added: fc.map_or(0, |c| c.added),
        file_removed: fc.map_or(0, |c| c.removed),
    }
}

fn merge_tool_steps(
    stored: &[AgentToolCallRecord],
    live: Option<&LiveRunSnapshot>,
    outputs: &HashMap<String, String>,
    changes: &HashMap<String, FileChange>,
) -> Vec<TopologyToolStep> {
    let mut steps: Vec<TopologyToolStep> = stored
        .iter()
        .map(|rec| record_to_step(rec, outputs, changes))
        .collect();
    let mut seen: HashSet<String> = steps.iter().map(|s| s.call_id.clone()).collect();
    if let Some(snap) = live {
        let mut live_args: HashMap<&str, &str> = HashMap::new();
        for part in &snap.parts {
            if let HubPart::ToolCall { id, arguments, .. } = part {
                live_args.insert(id.as_str(), arguments.as_str());
            }
        }
        for tc in &snap.tools {
            // Skip synthetic child approval mirror rows attached to parent snapshot.
            if tc.child_run_id.is_some() && tc.approval.is_some() && tc.name != TASK {
                continue;
            }
            if !seen.insert(tc.call_id.clone()) {
                continue;
            }
            let args = live_args.get(tc.call_id.as_str()).copied().unwrap_or("");
            let class = if tc.name == TASK { "internal" } else { "tool" };
            let out_prev = outputs
                .get(&tc.call_id)
                .and_then(|s| format_output_preview(s, class))
                .or_else(|| {
                    tc.result
                        .as_ref()
                        .and_then(|r| format_output_preview(&r.summary, class))
                });
            let fc = changes.get(&tc.call_id);
            steps.push(TopologyToolStep {
                call_id: tc.call_id.clone(),
                name: tc.name.clone(),
                class: class.to_owned(),
                summary: summarize_args(args),
                status: normalize_live_status(tc.state, tc.result.as_ref().map(|r| r.status))
                    .to_owned(),
                started_ms: None,
                ended_ms: None,
                duration_ms: tc.result.as_ref().map(|r| r.duration_ms),
                exit_code: None,
                parallel_safe: is_parallel_safe_tool(&tc.name, class),
                args_preview: format_args_preview(args),
                output_preview: out_prev,
                file_change_id: fc.map(|c| c.id.clone()),
                file_path: fc.map(|c| c.path.clone()),
                file_added: fc.map_or(0, |c| c.added),
                file_removed: fc.map_or(0, |c| c.removed),
                ..TopologyToolStep::default()
            });
        }
    }
    steps
}

struct ParentCallItem {
    message_id: Option<String>,
    step: TopologyToolStep,
}

/// Groups parent tool calls into deterministic serial waves and parallel fan-out batches.
fn build_execution_waves(
    stored: &[AgentToolCallRecord],
    live: Option<&LiveRunSnapshot>,
    outputs: &HashMap<String, String>,
    changes: &HashMap<String, FileChange>,
    subagents_by_call: &mut HashMap<String, TopologySubagent>,
    mut orphan_subagents: Vec<TopologySubagent>,
) -> Vec<ExecutionWave> {
    let mut items: Vec<ParentCallItem> = stored
        .iter()
        .map(|rec| ParentCallItem {
            message_id: rec.message_id.clone(),
            step: record_to_step(rec, outputs, changes),
        })
        .collect();
    let mut seen: HashSet<String> = items.iter().map(|i| i.step.call_id.clone()).collect();
    if let Some(snap) = live {
        let mut live_args: HashMap<&str, &str> = HashMap::new();
        for part in &snap.parts {
            if let HubPart::ToolCall { id, arguments, .. } = part {
                live_args.insert(id.as_str(), arguments.as_str());
            }
        }
        for tc in &snap.tools {
            if tc.child_run_id.is_some() && tc.approval.is_some() && tc.name != TASK {
                continue;
            }
            if !seen.insert(tc.call_id.clone()) {
                continue;
            }
            let args = live_args.get(tc.call_id.as_str()).copied().unwrap_or("");
            let class = if tc.name == TASK { "internal" } else { "tool" };
            let out_prev = outputs
                .get(&tc.call_id)
                .and_then(|s| format_output_preview(s, class))
                .or_else(|| {
                    tc.result
                        .as_ref()
                        .and_then(|r| format_output_preview(&r.summary, class))
                });
            let fc = changes.get(&tc.call_id);
            items.push(ParentCallItem {
                message_id: None,
                step: TopologyToolStep {
                    call_id: tc.call_id.clone(),
                    name: tc.name.clone(),
                    class: class.to_owned(),
                    summary: summarize_args(args),
                    status: normalize_live_status(tc.state, tc.result.as_ref().map(|r| r.status))
                        .to_owned(),
                    started_ms: None,
                    ended_ms: None,
                    duration_ms: tc.result.as_ref().map(|r| r.duration_ms),
                    exit_code: None,
                    parallel_safe: is_parallel_safe_tool(&tc.name, class),
                    args_preview: format_args_preview(args),
                    output_preview: out_prev,
                    file_change_id: fc.map(|c| c.id.clone()),
                    file_path: fc.map(|c| c.path.clone()),
                    file_added: fc.map_or(0, |c| c.added),
                    file_removed: fc.map_or(0, |c| c.removed),
                    ..TopologyToolStep::default()
                },
            });
        }
    }

    let mut waves = Vec::new();
    let mut wave_index = 0u32;
    let mut idx = 0usize;

    while idx < items.len() {
        let msg_id = items[idx].message_id.clone();

        // 1. Consecutive `task` calls in the same assistant turn -> Parallel or Single subagent wave.
        if items[idx].step.name == TASK && subagents_by_call.contains_key(&items[idx].step.call_id)
        {
            let mut batch: Vec<TopologySubagent> = Vec::new();
            while idx < items.len()
                && items[idx].message_id == msg_id
                && items[idx].step.name == TASK
                && let Some(sub) = subagents_by_call.remove(&items[idx].step.call_id)
            {
                batch.push(sub);
                idx += 1;
            }
            if !batch.is_empty() {
                wave_index += 1;
                let (started_ms, ended_ms) = subagent_wave_span(&batch);
                if batch.len() >= 2 {
                    waves.push(ExecutionWave::ParallelSubagents {
                        wave_index,
                        started_ms,
                        ended_ms,
                        subagents: batch,
                    });
                } else if let Some(sub) = batch.into_iter().next() {
                    waves.push(ExecutionWave::SingleSubagent {
                        wave_index,
                        started_ms,
                        ended_ms,
                        subagent: Box::new(sub),
                    });
                }
                continue;
            }
        }

        // 2. Consecutive parallel-safe non-task calls in the same turn -> Parallel or Serial tool wave.
        if items[idx].step.parallel_safe && items[idx].step.name != TASK {
            let mut tools = Vec::new();
            while idx < items.len()
                && items[idx].message_id == msg_id
                && items[idx].step.parallel_safe
                && items[idx].step.name != TASK
            {
                tools.push(items[idx].step.clone());
                idx += 1;
            }
            wave_index += 1;
            let (started_ms, ended_ms) = tool_wave_span(&tools);
            if tools.len() >= 2 {
                waves.push(ExecutionWave::ParallelTools {
                    wave_index,
                    started_ms,
                    ended_ms,
                    tools,
                });
            } else if let Some(tool) = tools.into_iter().next() {
                waves.push(ExecutionWave::SerialTool {
                    wave_index,
                    started_ms,
                    ended_ms,
                    tool: Box::new(tool),
                });
            }
            continue;
        }

        // 3. Single serial barrier step (or a `task` call that failed validation before spawning a child).
        let tool = items[idx].step.clone();
        idx += 1;
        wave_index += 1;
        let started_ms = tool.started_ms;
        let ended_ms = tool.ended_ms;
        waves.push(ExecutionWave::SerialTool {
            wave_index,
            started_ms,
            ended_ms,
            tool: Box::new(tool),
        });
    }

    // Any remaining unmatched sub-agents (e.g. brand-new live child before parent call audit flush).
    orphan_subagents.extend(subagents_by_call.drain().map(|(_, s)| s));
    orphan_subagents.sort_by_key(|s| (s.run.started_at_ms, s.run.run_id.clone()));
    if !orphan_subagents.is_empty() {
        wave_index += 1;
        let (started_ms, ended_ms) = subagent_wave_span(&orphan_subagents);
        if orphan_subagents.len() >= 2 {
            waves.push(ExecutionWave::ParallelSubagents {
                wave_index,
                started_ms,
                ended_ms,
                subagents: orphan_subagents,
            });
        } else if let Some(sub) = orphan_subagents.into_iter().next() {
            waves.push(ExecutionWave::SingleSubagent {
                wave_index,
                started_ms,
                ended_ms,
                subagent: Box::new(sub),
            });
        }
    }

    waves
}

fn subagent_wave_span(subs: &[TopologySubagent]) -> (Option<i64>, Option<i64>) {
    let start = subs.iter().map(|s| s.run.started_at_ms).min();
    let any_active = subs.iter().any(|s| s.run.status.is_active());
    let end = if any_active {
        None
    } else {
        subs.iter().filter_map(|s| s.run.finished_at_ms).max()
    };
    (start, end)
}

fn tool_wave_span(tools: &[TopologyToolStep]) -> (Option<i64>, Option<i64>) {
    let start = tools.iter().filter_map(|t| t.started_ms).min();
    let any_active = tools
        .iter()
        .any(|t| matches!(t.status.as_str(), "running" | "waiting_approval"));
    let end = if any_active {
        None
    } else {
        tools.iter().filter_map(|t| t.ended_ms).max()
    };
    (start, end)
}

#[cfg(test)]
#[allow(clippy::unwrap_used, clippy::too_many_arguments)]
mod tests {
    use super::*;
    use crate::agent::model::{AgentRunKind, AgentSessionInput, AgentTarget};
    use crate::agent_service::sessions::agent_session_create_impl;
    use crate::llm::hub::HubUsage;

    fn make_run(
        run_id: &str,
        session_id: &str,
        kind: AgentRunKind,
        parent_run_id: Option<&str>,
        parent_call_id: Option<&str>,
        status: AgentRunStatus,
        started_at_ms: i64,
        finished_at_ms: Option<i64>,
    ) -> AgentRunSummary {
        AgentRunSummary {
            run_id: run_id.to_owned(),
            session_id: session_id.to_owned(),
            kind,
            parent_run_id: parent_run_id.map(str::to_owned),
            parent_call_id: parent_call_id.map(str::to_owned),
            status,
            model_label: "openai/gpt-5".to_owned(),
            target: Some(AgentTarget::Named {
                name: "openai/gpt-5".to_owned(),
            }),
            iterations: 2,
            usage: HubUsage::default(),
            cost_micro_usd: 0,
            started_at_ms,
            finished_at_ms,
            last_event_ms: finished_at_ms.or(Some(started_at_ms)),
            last_step: None,
            stop_reason: None,
            error_kind: None,
            error: (status == AgentRunStatus::Failed).then(|| "upstream 429".to_owned()),
            prompt_version: 1,
            last_seq: 0,
            failure: None,
            role: None,
            cost_currency: Some("USD".to_owned()),
            cap_inapplicable: None,
            status_detail: None,
        }
    }

    fn call_rec(
        session_id: &str,
        run_id: &str,
        message_id: &str,
        call_id: &str,
        name: &str,
        class: &str,
        args_json: &str,
        state: &str,
        started_ms: i64,
        ended_ms: Option<i64>,
    ) -> AgentToolCallRecord {
        AgentToolCallRecord {
            session_id: session_id.to_owned(),
            call_id: call_id.to_owned(),
            run_id: run_id.to_owned(),
            message_id: Some(message_id.to_owned()),
            name: name.to_owned(),
            class: class.to_owned(),
            state: state.to_owned(),
            args_json: args_json.to_owned(),
            risk: "low".to_owned(),
            decided_by: Some("mode".to_owned()),
            decision: Some("allow".to_owned()),
            decided_ms: Some(started_ms),
            started_ms: Some(started_ms),
            ended_ms,
            exit_code: None,
            sandbox: None,
            truncated: false,
            output_bytes: 0,
            spill_ref: None,
            requested_ms: Some(started_ms),
        }
    }

    #[test]
    fn topology_groups_parallel_subagents_parallel_tools_and_serial_barriers() {
        let state = AppState::open_in_memory().unwrap();
        let parent_sess = agent_session_create_impl(
            &state,
            AgentSessionInput {
                title: Some("Refactor auth".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
        let c1_sess = agent_session_create_impl(
            &state,
            AgentSessionInput {
                title: Some("Audit route guards".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
        let c2_sess = agent_session_create_impl(
            &state,
            AgentSessionInput {
                title: Some("Check OAuth callback".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();
        let c3_sess = agent_session_create_impl(
            &state,
            AgentSessionInput {
                title: Some("Follow-up synthesis".to_owned()),
                ..Default::default()
            },
        )
        .unwrap();

        {
            let store = state.store();
            store
                .insert_agent_run(&make_run(
                    "run_main",
                    &parent_sess.id,
                    AgentRunKind::Main,
                    None,
                    None,
                    AgentRunStatus::Completed,
                    1_000,
                    Some(20_000),
                ))
                .unwrap();
            store
                .insert_agent_run(&make_run(
                    "run_sub_1",
                    &c1_sess.id,
                    AgentRunKind::Subagent,
                    Some("run_main"),
                    Some("call_t1"),
                    AgentRunStatus::Completed,
                    3_000,
                    Some(9_000),
                ))
                .unwrap();
            store
                .insert_agent_run(&make_run(
                    "run_sub_2",
                    &c2_sess.id,
                    AgentRunKind::Subagent,
                    Some("run_main"),
                    Some("call_t2"),
                    AgentRunStatus::Failed,
                    3_050,
                    Some(7_000),
                ))
                .unwrap();
            store
                .insert_agent_run(&make_run(
                    "run_sub_3",
                    &c3_sess.id,
                    AgentRunKind::Subagent,
                    Some("run_main"),
                    Some("call_t3"),
                    AgentRunStatus::Completed,
                    14_000,
                    Some(18_000),
                ))
                .unwrap();

            // Wave 1 (msg_1): 2 parallel read tools
            store
                .upsert_tool_call(&call_rec(
                    &parent_sess.id,
                    "run_main",
                    "msg_1",
                    "call_r1",
                    "read_file",
                    "read",
                    r#"{"path":"src/auth.rs"}"#,
                    "output-available",
                    1_200,
                    Some(1_500),
                ))
                .unwrap();
            store
                .upsert_tool_call(&call_rec(
                    &parent_sess.id,
                    "run_main",
                    "msg_1",
                    "call_r2",
                    "grep",
                    "read",
                    r#"{"pattern":"verify_token"}"#,
                    "output-available",
                    1_210,
                    Some(1_600),
                ))
                .unwrap();
            // Wave 2 (msg_2): 2 parallel sub-agents
            store
                .upsert_tool_call(&call_rec(
                    &parent_sess.id,
                    "run_main",
                    "msg_2",
                    "call_t1",
                    "task",
                    "internal",
                    r#"{"description":"Audit route guards","kind":"explore"}"#,
                    "output-available",
                    3_000,
                    Some(9_000),
                ))
                .unwrap();
            store
                .upsert_tool_call(&call_rec(
                    &parent_sess.id,
                    "run_main",
                    "msg_2",
                    "call_t2",
                    "task",
                    "internal",
                    r#"{"description":"Check OAuth callback","kind":"general"}"#,
                    "output-error",
                    3_050,
                    Some(7_000),
                ))
                .unwrap();
            // Wave 3 (msg_3): serial shell command
            store
                .upsert_tool_call(&call_rec(
                    &parent_sess.id,
                    "run_main",
                    "msg_3",
                    "call_sh",
                    "shell",
                    "exec",
                    r#"{"cmd":"cargo test auth"}"#,
                    "output-available",
                    10_000,
                    Some(13_000),
                ))
                .unwrap();
            // Wave 4 (msg_4): single serial subagent with resume
            store
                .upsert_tool_call(&call_rec(
                    &parent_sess.id,
                    "run_main",
                    "msg_4",
                    "call_t3",
                    "task",
                    "internal",
                    r#"{"description":"Follow-up synthesis","kind":"general","resume":"as_prev"}"#,
                    "output-available",
                    14_000,
                    Some(18_000),
                ))
                .unwrap();
            // Child 1 internal tool call
            store
                .upsert_tool_call(&call_rec(
                    &c1_sess.id,
                    "run_sub_1",
                    "c1_m1",
                    "c1_read",
                    "read_file",
                    "read",
                    r#"{"path":"src/routes.rs"}"#,
                    "output-available",
                    3_200,
                    Some(3_400),
                ))
                .unwrap();
        }

        let topo = agent_run_topology_impl(&state, "run_main").unwrap();
        assert_eq!(topo.subagent_counts.total, 3);
        assert_eq!(topo.subagent_counts.completed, 2);
        assert_eq!(topo.subagent_counts.failed, 1);
        assert_eq!(topo.waves.len(), 4);

        match &topo.waves[0] {
            ExecutionWave::ParallelTools { tools, .. } => assert_eq!(tools.len(), 2),
            other => panic!("expected ParallelTools, got {other:?}"),
        }
        match &topo.waves[1] {
            ExecutionWave::ParallelSubagents { subagents, .. } => {
                assert_eq!(subagents.len(), 2);
                assert_eq!(subagents[0].description, "Audit route guards");
                assert_eq!(subagents[0].subagent_kind, "explore");
                assert_eq!(subagents[0].tool_calls.len(), 1);
                assert_eq!(subagents[1].run.status, AgentRunStatus::Failed);
                assert_eq!(subagents[1].run.error.as_deref(), Some("upstream 429"));
            }
            other => panic!("expected ParallelSubagents, got {other:?}"),
        }
        match &topo.waves[2] {
            ExecutionWave::SerialTool { tool, .. } => {
                assert_eq!(tool.name, "shell");
                assert!(
                    tool.args_preview
                        .as_deref()
                        .is_some_and(|a| a.contains("cargo test auth"))
                );
            }
            other => panic!("expected SerialTool, got {other:?}"),
        }
        match &topo.waves[3] {
            ExecutionWave::SingleSubagent { subagent, .. } => {
                assert!(subagent.resumed);
                assert_eq!(subagent.description, "Follow-up synthesis");
            }
            other => panic!("expected SingleSubagent, got {other:?}"),
        }

        let hist = agent_session_run_history_impl(&state, &parent_sess.id, Some(10)).unwrap();
        assert_eq!(hist.len(), 1);
        assert_eq!(hist[0].run.run_id, "run_main");
        assert!(hist[0].has_parallel);
        assert_eq!(hist[0].subagent_count, 3);
        assert!(hist[0].tool_count >= 5);

        let all_hist = agent_all_runs_history_impl(&state, Some(10)).unwrap();
        assert_eq!(all_hist.len(), 1);
        assert_eq!(all_hist[0].title, "Refactor auth");
    }
}
