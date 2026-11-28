//! Turn boundary slicing and [`AssistantTurnView`] construction.

use std::collections::{HashMap, HashSet};
use std::sync::Arc;

use spanbay_core::agent::model::{
    AgentMessage, AgentRole, AgentSourceDto, AgentStopReason, AgentToolCallRecord,
    ApprovalDecision, FileChange, OutputStream, ReasoningEffort,
};
use spanbay_core::model::hub::{HubCitation, HubPart};

use super::super::chat_reducer::{
    ApprovalResolution, ChatThreadState, LiveToolCall, LiveToolMeta, is_pending_placeholder_parts,
};
use super::super::run_graph::{
    GraphNodeStatus, map_audit_tool_state, map_run_status, map_tool_call_status,
};
use super::super::tool_trace::format_tool_trace;
use super::explore::coalesce_spine_items;
use super::metrics::{
    compute_fold_stats_and_failures, compute_turn_file_changes, extract_inline_thinking_text,
    extract_message_reasoning_parts, extract_saved_request_link, extract_untracked_paths,
};
use super::{
    AssistantTurnView, ThreadTurnEntry, TranscriptDensity, TurnActivityItem, TurnMetricsSummary,
    TurnToolCallItem,
};

/// Builds the full list of [`ThreadTurnEntry`] items from a [`ChatThreadState`] and its persisted side tables.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn build_thread_turn_entries(
    thread: &ChatThreadState,
    audit_tool_calls: &[AgentToolCallRecord],
    persisted_changes: &[FileChange],
    thinking_durations: &HashMap<String, u64>,
    session_ws_id: Option<&str>,
    run_active: bool,
    density: TranscriptDensity,
) -> Vec<ThreadTurnEntry> {
    let mut entries = build_thread_turn_entries_range(
        thread,
        0..thread.messages.len(),
        true,
        audit_tool_calls,
        persisted_changes,
        thinking_durations,
        session_ws_id,
        run_active,
        density,
    );
    finalize_latest_turn_collapse(&mut entries, density);
    entries
}

/// Builds [`ThreadTurnEntry`] items for `msg_range` (`0..last_boundary` for cached older turns,
/// or `last_boundary..messages.len()` for the active tail turn).
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn build_thread_turn_entries_range(
    thread: &ChatThreadState,
    msg_range: std::ops::Range<usize>,
    include_tail_live: bool,
    audit_tool_calls: &[AgentToolCallRecord],
    persisted_changes: &[FileChange],
    thinking_durations: &HashMap<String, u64>,
    session_ws_id: Option<&str>,
    run_active: bool,
    density: TranscriptDensity,
) -> Vec<ThreadTurnEntry> {
    let messages = &thread.messages;
    let range_start = msg_range.start.min(messages.len());
    let range_end = msg_range.end.min(messages.len());
    let slice_msgs = &messages[range_start..range_end];

    let audit_map: HashMap<&str, &AgentToolCallRecord> = audit_tool_calls
        .iter()
        .map(|r| (r.call_id.as_str(), r))
        .collect();

    let saved_call_ids: HashSet<&str> = if include_tail_live {
        slice_msgs
            .iter()
            .filter(|m| m.role == AgentRole::Assistant)
            .flat_map(|m| m.parts.iter())
            .filter_map(|pt| match pt {
                HubPart::ToolCall { id, .. } => Some(id.as_str()),
                _ => None,
            })
            .collect()
    } else {
        HashSet::new()
    };

    let run_truly_active = run_active
        && !thread
            .active_run
            .as_ref()
            .is_some_and(|r| !r.status.is_active());
    let has_unsaved_live = include_tail_live
        && (run_truly_active
            || !thread.streaming_text.is_empty()
            || !thread.streaming_reasoning.is_empty()
            || thread
                .tool_calls
                .iter()
                .any(|c| !saved_call_ids.contains(c.call_id.as_str())));

    let mut entries: Vec<ThreadTurnEntry> = Vec::new();
    let mut i = range_start;

    while i < range_end {
        let msg = &messages[i];
        match msg.role {
            AgentRole::Summary => {
                entries.push(ThreadTurnEntry::SummaryDivider { msg_index: i });
                i += 1;
            }
            AgentRole::Tool => {
                i += 1;
            }
            AgentRole::User if msg.is_turn_boundary() => {
                let user_idx = i;
                let user_msg_id = Some(msg.id.clone());
                let user_seq = Some(msg.seq);
                entries.push(ThreadTurnEntry::UserTurn {
                    msg_index: user_idx,
                });
                i += 1;

                let span_start = i;
                while i < range_end && !messages[i].is_turn_boundary() {
                    i += 1;
                }
                let span_end = i;
                let attach_live =
                    include_tail_live && span_end == messages.len() && has_unsaved_live;
                let has_assistant_or_summary = messages[span_start..span_end]
                    .iter()
                    .any(|m| matches!(m.role, AgentRole::Assistant | AgentRole::Summary));

                if has_assistant_or_summary || attach_live {
                    let mut turn = build_single_assistant_turn(
                        thread,
                        &messages[span_start..span_end],
                        span_start,
                        user_msg_id,
                        user_seq,
                        attach_live,
                        &saved_call_ids,
                        &audit_map,
                        persisted_changes,
                        thinking_durations,
                        session_ws_id,
                        run_active,
                    );
                    turn.auto_collapsed = compute_auto_collapsed(&turn, false, density);
                    entries.push(ThreadTurnEntry::AssistantTurn(Arc::new(turn)));
                }
            }
            AgentRole::User | AgentRole::Assistant => {
                let span_start = i;
                while i < range_end && !messages[i].is_turn_boundary() {
                    i += 1;
                }
                let span_end = i;
                let attach_live =
                    include_tail_live && span_end == messages.len() && has_unsaved_live;
                let mut turn = build_single_assistant_turn(
                    thread,
                    &messages[span_start..span_end],
                    span_start,
                    None,
                    None,
                    attach_live,
                    &saved_call_ids,
                    &audit_map,
                    persisted_changes,
                    thinking_durations,
                    session_ws_id,
                    run_active,
                );
                turn.auto_collapsed = compute_auto_collapsed(&turn, false, density);
                entries.push(ThreadTurnEntry::AssistantTurn(Arc::new(turn)));
            }
        }
    }

    let already_attached_live = entries
        .iter()
        .rev()
        .any(|e| matches!(e, ThreadTurnEntry::AssistantTurn(t) if t.is_live));
    if has_unsaved_live && !already_attached_live {
        let mut turn = build_single_assistant_turn(
            thread,
            &[],
            messages.len(),
            None,
            None,
            true,
            &saved_call_ids,
            &audit_map,
            persisted_changes,
            thinking_durations,
            session_ws_id,
            run_active,
        );
        turn.auto_collapsed = compute_auto_collapsed(&turn, true, density);
        entries.push(ThreadTurnEntry::AssistantTurn(Arc::new(turn)));
    }

    entries
}

const fn compute_auto_collapsed(
    turn: &AssistantTurnView,
    is_latest: bool,
    density: TranscriptDensity,
) -> bool {
    if turn.is_live || turn.spine.is_empty() || turn.has_failure {
        false
    } else {
        match density {
            TranscriptDensity::Verbose => false,
            TranscriptDensity::Compact => true,
            TranscriptDensity::Normal => !is_latest,
        }
    }
}

pub(super) fn finalize_latest_turn_collapse(
    entries: &mut [ThreadTurnEntry],
    density: TranscriptDensity,
) {
    if let Some(ThreadTurnEntry::AssistantTurn(last_arc)) = entries
        .iter_mut()
        .rev()
        .find(|e| matches!(e, ThreadTurnEntry::AssistantTurn(_)))
    {
        let last_turn = Arc::make_mut(last_arc);
        last_turn.is_latest_turn = true;
        last_turn.auto_collapsed = compute_auto_collapsed(last_turn, true, density);
    }
}
#[allow(clippy::too_many_arguments)]
fn build_single_assistant_turn(
    thread: &ChatThreadState,
    slice: &[AgentMessage],
    slice_offset: usize,
    user_message_id: Option<String>,
    user_seq: Option<i64>,
    attach_live: bool,
    saved_call_ids: &HashSet<&str>,
    audit_map: &HashMap<&str, &AgentToolCallRecord>,
    persisted_changes: &[FileChange],
    thinking_durations: &HashMap<String, u64>,
    session_ws_id: Option<&str>,
    run_active: bool,
) -> AssistantTurnView {
    let mut tool_results: HashMap<&str, (&[HubPart], bool)> = HashMap::new();
    for msg in slice {
        for part in &msg.parts {
            if let HubPart::ToolResult {
                call_id,
                content,
                is_error,
                ..
            } = part
            {
                tool_results.insert(call_id.as_str(), (content.as_slice(), *is_error));
            }
        }
    }

    let mut message_indices = Vec::with_capacity(slice.len());
    let mut first_assistant_id: Option<String> = None;
    let mut first_assistant_seq: Option<i64> = None;
    let mut last_assistant_id: Option<String> = None;
    let mut run_id: Option<String> = None;
    let mut models_used: Vec<String> = Vec::new();
    let mut reasoning_effort: Option<ReasoningEffort> = None;
    let mut raw_spine: Vec<TurnActivityItem> = Vec::new();
    let mut prose_segments: Vec<(String, String, bool)> = Vec::new();
    let mut sources: Vec<AgentSourceDto> = Vec::new();
    let mut citations: Vec<HubCitation> = Vec::new();
    let mut seen_citation_urls: HashSet<String> = HashSet::new();
    let mut inline_error: Option<String> = None;
    let mut stop_reason: Option<AgentStopReason> = None;
    let mut totals = TurnMetricsSummary {
        context_limit_tokens: thread.context_limit_tokens,
        ..TurnMetricsSummary::default()
    };
    let mut all_turn_call_ids: Vec<String> = Vec::new();
    let mut min_call_start: Option<i64> = None;
    let mut max_call_end: Option<i64> = None;

    for (rel_ix, msg) in slice.iter().enumerate() {
        let abs_ix = slice_offset + rel_ix;
        message_indices.push(abs_ix);
        if run_id.is_none() && msg.run_id.is_some() {
            run_id = msg.run_id.clone();
        }
        match msg.role {
            AgentRole::Summary => {
                raw_spine.push(TurnActivityItem::CompactionDivider {
                    key: format!("comp-{}", msg.id),
                });
            }
            AgentRole::User => {
                let text = extract_plain_text(&msg.parts);
                if !text.trim().is_empty() {
                    raw_spine.push(TurnActivityItem::SteerNote {
                        message_id: msg.id.clone(),
                        text,
                    });
                }
            }
            AgentRole::Assistant => {
                if first_assistant_id.is_none() {
                    first_assistant_id = Some(msg.id.clone());
                    first_assistant_seq = Some(msg.seq);
                }
                last_assistant_id = Some(msg.id.clone());
                totals.step_count += 1;

                if let Some(m) = msg
                    .meta
                    .served_model
                    .as_deref()
                    .or(msg.model.as_deref())
                    .filter(|s| !s.is_empty())
                    && !models_used.iter().any(|existing| existing == m)
                {
                    models_used.push(m.to_owned());
                }
                if reasoning_effort.is_none() {
                    reasoning_effort = msg.meta.reasoning_effort;
                }

                if let Some(u) = &msg.usage {
                    let in_tok = u.input_tokens.unwrap_or(0);
                    let out_tok = u.output_tokens.unwrap_or(0);
                    let r_tok = u.reasoning_tokens.unwrap_or(0);
                    let c_tok = u.cached_input_tokens.unwrap_or(0);
                    totals.input_tokens = totals.input_tokens.saturating_add(in_tok);
                    totals.output_tokens = totals.output_tokens.saturating_add(out_tok);
                    totals.reasoning_tokens = totals.reasoning_tokens.saturating_add(r_tok);
                    totals.cached_input_tokens = totals.cached_input_tokens.saturating_add(c_tok);
                    totals.peak_context_tokens = totals.peak_context_tokens.max(in_tok);
                }
                if totals.ttft_ms.is_none() {
                    totals.ttft_ms = msg.meta.ttft_ms;
                }
                if let Some(s_ms) = msg.meta.stream_ms {
                    totals.stream_ms = totals.stream_ms.saturating_add(s_ms);
                }
                let step_ms = msg
                    .meta
                    .ttft_ms
                    .unwrap_or(0)
                    .saturating_add(msg.meta.stream_ms.unwrap_or(0));
                totals.duration_ms = totals.duration_ms.saturating_add(step_ms);
                if msg.cost_micro_usd > 0 {
                    totals.cost_micro_usd =
                        totals.cost_micro_usd.saturating_add(msg.cost_micro_usd);
                    if totals.cost_currency.is_none() {
                        totals.cost_currency = msg.cost_currency.clone();
                    }
                }
                if let Some(sr) = msg
                    .meta
                    .stop_reason
                    .filter(|r| *r != AgentStopReason::Completed)
                {
                    stop_reason = Some(sr);
                }
                if let Some(err) = &msg.error {
                    inline_error = Some(err.clone());
                }
                for src in &msg.meta.sources {
                    if !sources.iter().any(|s| s.url == src.url) {
                        sources.push(src.clone());
                    }
                }

                let recorded_dur = thinking_durations.get(&msg.id).copied();
                let (r_text, r_sealed, _, text_chunks) = extract_message_reasoning_parts(msg);
                if r_sealed {
                    totals.sealed_reasoning = true;
                }
                if !r_text.is_empty() {
                    raw_spine.push(TurnActivityItem::Reasoning {
                        key: format!("reason-{}", msg.id),
                        text: r_text,
                        duration_ms: recorded_dur,
                        is_live: false,
                        sealed: false,
                    });
                } else if r_sealed {
                    raw_spine.push(TurnActivityItem::Reasoning {
                        key: format!("reason-{}", msg.id),
                        text: String::new(),
                        duration_ms: recorded_dur,
                        is_live: false,
                        sealed: true,
                    });
                }

                let joined_text = text_chunks.join("\n").trim().to_owned();
                let step_tool_calls: Vec<(&str, &str, &str)> = msg
                    .parts
                    .iter()
                    .filter_map(|part| match part {
                        HubPart::ToolCall {
                            id,
                            name,
                            arguments,
                            ..
                        } => Some((id.as_str(), name.as_str(), arguments.as_str())),
                        _ => None,
                    })
                    .collect();

                for part in &msg.parts {
                    if let HubPart::Text {
                        citations: msg_cites,
                        ..
                    } = part
                    {
                        for c in msg_cites {
                            if seen_citation_urls.insert(c.url.clone()) {
                                citations.push(c.clone());
                            }
                        }
                    }
                }

                let has_tools_in_or_after = !step_tool_calls.is_empty()
                    || slice[rel_ix + 1..].iter().any(|later| {
                        later.role == AgentRole::Assistant
                            && later
                                .parts
                                .iter()
                                .any(|p| matches!(p, HubPart::ToolCall { .. }))
                    })
                    || (attach_live
                        && thread
                            .tool_calls
                            .iter()
                            .any(|c| !saved_call_ids.contains(c.call_id.as_str())));

                if !joined_text.is_empty() {
                    if has_tools_in_or_after {
                        raw_spine.push(TurnActivityItem::IntermediateText {
                            key: format!("inter-{}", msg.id),
                            markdown: joined_text.clone(),
                        });
                    }
                    prose_segments.push((msg.id.clone(), joined_text, has_tools_in_or_after));
                }

                let mut step_task_calls: Vec<LiveToolCall> = Vec::new();
                for &(call_id, name, args) in &step_tool_calls {
                    all_turn_call_ids.push(call_id.to_owned());
                    if let Some(a) = audit_map.get(call_id) {
                        if let Some(s) = a.started_ms.or(a.requested_ms).or(a.decided_ms) {
                            min_call_start = Some(min_call_start.map_or(s, |c| c.min(s)));
                        }
                        if let Some(e) = a.ended_ms.or(a.decided_ms) {
                            max_call_end = Some(max_call_end.map_or(e, |c| c.max(e)));
                        }
                    }
                    if name == "task" {
                        step_task_calls.push(build_saved_task_call(
                            thread,
                            call_id,
                            name,
                            args,
                            &tool_results,
                            audit_map,
                            run_active,
                        ));
                        totals.visible_tool_count += 1;
                        continue;
                    }
                    if !step_task_calls.is_empty() {
                        let first_id = step_task_calls[0].call_id.clone();
                        raw_spine.push(TurnActivityItem::SubagentGroup {
                            group_key: format!("subgrp-{first_id}"),
                            calls: std::mem::take(&mut step_task_calls),
                        });
                    }
                    if name == "submit_plan" {
                        let result = tool_results.get(call_id).copied();
                        let status = resolve_plan_status(
                            thread, msg, call_id, result, audit_map, run_active,
                        );
                        let (actively_revising, next_user_text) =
                            compute_plan_revision_ctx(slice, rel_ix, msg, thread, run_active);
                        raw_spine.push(TurnActivityItem::PlanBlock {
                            call_id: call_id.to_owned(),
                            args_json: args.to_owned(),
                            result_parts: result.map_or_else(Vec::new, |(pts, _)| pts.to_vec()),
                            status,
                            actively_revising,
                            next_user_text,
                        });
                        totals.visible_tool_count += 1;
                        continue;
                    }

                    let item = build_saved_tool_item(
                        thread,
                        call_id,
                        name,
                        args,
                        &tool_results,
                        audit_map,
                        persisted_changes,
                        session_ws_id,
                        run_active,
                    );
                    if name == "todo_write" && !item.status.is_terminal_error() {
                        totals.hidden_todo_count += 1;
                        continue;
                    }
                    totals.visible_tool_count += 1;
                    raw_spine.push(TurnActivityItem::ToolCall(Box::new(item)));
                }
                if !step_task_calls.is_empty() {
                    let first_id = step_task_calls[0].call_id.clone();
                    raw_spine.push(TurnActivityItem::SubagentGroup {
                        group_key: format!("subgrp-{first_id}"),
                        calls: step_task_calls,
                    });
                }
            }
            AgentRole::Tool => {}
        }
    }

    let mut final_is_streaming = false;
    let mut live_visible_text = String::new();
    if attach_live {
        if run_id.is_none() {
            run_id = thread.active_run.as_ref().map(|r| r.run_id.clone());
        }
        if let Some(active) = &thread.active_run
            && !active.model_label.is_empty()
            && !models_used.iter().any(|m| m == &active.model_label)
        {
            models_used.push(active.model_label.clone());
        }
        if let Some(meter) = &thread.run_meter
            && totals.step_count == 0
        {
            totals.input_tokens = meter.usage.input_tokens.unwrap_or(0);
            totals.output_tokens = meter.usage.output_tokens.unwrap_or(0);
            totals.reasoning_tokens = meter.usage.reasoning_tokens.unwrap_or(0);
            totals.cached_input_tokens = meter.usage.cached_input_tokens.unwrap_or(0);
            totals.cost_micro_usd = meter.cost_micro;
            totals.cost_currency = meter.currency.clone();
        }

        let (inline_thinking, visible) = extract_inline_thinking_text(&thread.streaming_text, true);
        live_visible_text = visible;
        let mut live_reasoning = thread.streaming_reasoning.clone();
        if !inline_thinking.is_empty() {
            if !live_reasoning.is_empty() {
                live_reasoning.push_str("\n\n");
            }
            live_reasoning.push_str(&inline_thinking.join("\n\n"));
        }
        let unsaved_calls: Vec<&LiveToolCall> = thread
            .tool_calls
            .iter()
            .filter(|c| !saved_call_ids.contains(c.call_id.as_str()))
            .collect();

        if !live_reasoning.is_empty() {
            let still_thinking = live_visible_text.is_empty() && unsaved_calls.is_empty();
            raw_spine.push(TurnActivityItem::Reasoning {
                key: "live-reasoning".to_owned(),
                text: live_reasoning,
                duration_ms: None,
                is_live: still_thinking,
                sealed: false,
            });
        }

        let mut live_tasks: Vec<LiveToolCall> = Vec::new();
        for call in unsaved_calls {
            all_turn_call_ids.push(call.call_id.clone());
            if call.name == "task" {
                live_tasks.push(call.clone());
                totals.visible_tool_count += 1;
                continue;
            }
            if !live_tasks.is_empty() {
                let first_id = live_tasks[0].call_id.clone();
                raw_spine.push(TurnActivityItem::SubagentGroup {
                    group_key: format!("subgrp-{first_id}"),
                    calls: std::mem::take(&mut live_tasks),
                });
            }
            let item = build_live_tool_item(thread, call, session_ws_id);
            if call.name == "todo_write" && !item.status.is_terminal_error() {
                totals.hidden_todo_count += 1;
                continue;
            }
            totals.visible_tool_count += 1;
            raw_spine.push(TurnActivityItem::ToolCall(Box::new(item)));
        }
        if !live_tasks.is_empty() {
            let first_id = live_tasks[0].call_id.clone();
            raw_spine.push(TurnActivityItem::SubagentGroup {
                group_key: format!("subgrp-{first_id}"),
                calls: live_tasks,
            });
        }

        for c in &thread.live_citations {
            if seen_citation_urls.insert(c.url.clone()) {
                citations.push(c.clone());
            }
        }
    }

    let final_markdown = if !live_visible_text.is_empty() {
        final_is_streaming = true;
        live_visible_text
    } else if let Some((_, last_text, had_later_tools)) = prose_segments.last() {
        if !*had_later_tools {
            last_text.clone()
        } else if !attach_live {
            if let Some(pos) = raw_spine
                .iter()
                .rposition(|item| matches!(item, TurnActivityItem::IntermediateText { .. }))
            {
                raw_spine.remove(pos);
            }
            last_text.clone()
        } else {
            String::new()
        }
    } else {
        String::new()
    };

    let coalesced_spine = coalesce_spine_items(raw_spine);

    if totals.duration_ms == 0 {
        if let (Some(s), Some(e)) = (min_call_start, max_call_end)
            && e >= s
        {
            totals.duration_ms = (e - s) as u64;
        } else if let Some(run) = thread
            .active_run
            .as_ref()
            .filter(|r| Some(r.run_id.as_str()) == run_id.as_deref())
            && let Some(end) = run.finished_at_ms
            && end >= run.started_at_ms
        {
            totals.duration_ms = (end - run.started_at_ms) as u64;
        }
    }

    let turn_changes = compute_turn_file_changes(
        &all_turn_call_ids,
        run_id.as_deref(),
        persisted_changes,
        &thread.file_changes,
    );
    let untracked_paths = extract_untracked_paths(thread, attach_live);
    let (fold_stats, has_failure) =
        compute_fold_stats_and_failures(&coalesced_spine, &turn_changes, &totals);

    // Key `turn_id` and `turn_key_seq` on the initiating user message so live→saved transitions
    // never change identity or lose user fold state.
    let turn_id = user_message_id
        .as_ref()
        .map(|u| format!("turn-{u}"))
        .or_else(|| first_assistant_id.as_ref().map(|a| format!("turn-{a}")))
        .unwrap_or_else(|| "turn-root".to_owned());
    let turn_key_seq = user_seq.or(first_assistant_seq).unwrap_or(0);

    AssistantTurnView {
        turn_id,
        turn_key_seq,
        user_message_id,
        first_assistant_id,
        last_assistant_id,
        run_id,
        message_indices,
        models_used,
        reasoning_effort,
        is_latest_turn: false,
        is_live: attach_live && run_active,
        spine: coalesced_spine,
        final_markdown,
        final_is_streaming,
        sources,
        citations,
        turn_changes,
        untracked_paths,
        totals,
        inline_error,
        stop_reason,
        has_failure,
        fold_stats,
        auto_collapsed: false,
    }
}

#[allow(clippy::too_many_arguments)]
fn build_saved_tool_item(
    thread: &ChatThreadState,
    call_id: &str,
    name: &str,
    args: &str,
    tool_results: &HashMap<&str, (&[HubPart], bool)>,
    audit_map: &HashMap<&str, &AgentToolCallRecord>,
    persisted_changes: &[FileChange],
    session_ws_id: Option<&str>,
    run_active: bool,
) -> TurnToolCallItem {
    let audit = audit_map.get(call_id).copied();
    let live_meta = thread.live_tool_meta.get(call_id);
    let result = tool_results.get(call_id).copied();
    let live_call = thread.tool_calls.iter().find(|c| c.call_id == call_id);
    let is_waiting_approval = run_active
        && thread
            .pending_approvals
            .iter()
            .any(|a| a.call_id == call_id);
    let status = if is_waiting_approval {
        GraphNodeStatus::Waiting
    } else {
        compute_saved_call_status(
            run_active,
            thread.pending_tool_calls.contains(call_id),
            live_meta,
            audit,
            result,
        )
    };
    let streams: Vec<(OutputStream, String)> =
        live_call.map_or_else(Vec::new, |c| c.output.clone());
    let out_text = (status != GraphNodeStatus::Running)
        .then(|| result.map(|(parts, _)| extract_plain_text(parts)))
        .flatten();
    let raw_result_first_line = out_text
        .as_deref()
        .and_then(|t| t.lines().next())
        .filter(|l| l.trim_start().starts_with("{\"error\""))
        .map(str::to_owned);
    let duration_ms = audit
        .and_then(|a| match (a.started_ms, a.ended_ms) {
            (Some(s), Some(e)) if e >= s => Some((e - s) as u64),
            _ => None,
        })
        .or_else(|| live_meta.map(|m| m.duration_ms));
    let exit_code = audit
        .and_then(|a| a.exit_code)
        .or_else(|| live_meta.and_then(|m| m.exit_code));
    let spilled = audit.is_some_and(|a| a.truncated || a.spill_ref.is_some());
    let matched_change = persisted_changes
        .iter()
        .find(|c| c.call_id == call_id)
        .map(|c| (c.id.clone(), c.path.clone(), c.added, c.removed));
    let diff_stats = matched_change
        .as_ref()
        .map(|(_, _, a, r)| (*a, *r))
        .or_else(|| {
            thread
                .file_changes
                .iter()
                .find(|c| c.call_id == call_id)
                .map(|c| (c.added, c.removed))
        });

    let pres = format_tool_trace(
        name,
        args,
        if !streams.is_empty() || status == GraphNodeStatus::Denied {
            None
        } else {
            out_text.as_deref()
        },
        status.is_terminal_error(),
        spilled,
        diff_stats,
    );

    let resolution = live_call.and_then(|c| c.resolution).or_else(|| {
        audit.and_then(|a| {
            let dec = match a.decision.as_deref()? {
                "allow" => ApprovalDecision::Allow,
                "deny" => ApprovalDecision::Deny,
                _ => return None,
            };
            if a.decided_by.as_deref() != Some("user") {
                return None;
            }
            Some(ApprovalResolution {
                decision: dec,
                scope: spanbay_core::agent::model::ApprovalScope::Once,
                timed_out: a.state == "approval-timed-out",
            })
        })
    });

    TurnToolCallItem {
        call_id: call_id.to_owned(),
        tool_name: name.to_owned(),
        edit_count: 1,
        pres,
        status,
        is_waiting_approval,
        duration_ms,
        exit_code,
        spilled,
        change_ref: matched_change.map(|(id, path, _, _)| (id, path)),
        resolution,
        audit: audit.cloned(),
        raw_result_first_line,
        request_link: extract_saved_request_link(
            name,
            args,
            out_text.as_deref(),
            status,
            live_meta,
            session_ws_id,
        ),
        streams,
        result_parts: result.map_or_else(Vec::new, |(parts, _)| parts.to_vec()),
    }
}

fn build_live_tool_item(
    thread: &ChatThreadState,
    call: &LiveToolCall,
    session_ws_id: Option<&str>,
) -> TurnToolCallItem {
    let is_waiting_approval = thread
        .pending_approvals
        .iter()
        .any(|a| a.call_id == call.call_id);
    let status = if is_waiting_approval {
        GraphNodeStatus::Waiting
    } else {
        call.status
            .map_or(GraphNodeStatus::Running, map_tool_call_status)
    };
    let live_diff = thread
        .file_changes
        .iter()
        .find(|c| c.call_id == call.call_id)
        .map(|c| (c.added, c.removed));
    let out_text = (status != GraphNodeStatus::Running && !call.result_parts.is_empty())
        .then(|| extract_plain_text(&call.result_parts));
    let raw_result_first_line = out_text
        .as_deref()
        .and_then(|t| t.lines().next())
        .filter(|l| l.trim_start().starts_with("{\"error\""))
        .map(str::to_owned);
    let pres = format_tool_trace(
        &call.name,
        &call.arguments,
        if !call.output.is_empty() || status == GraphNodeStatus::Denied {
            None
        } else {
            out_text.as_deref()
        },
        status.is_terminal_error(),
        false,
        live_diff,
    );
    let change_ref = thread
        .file_changes
        .iter()
        .find(|c| c.call_id == call.call_id)
        .and_then(|c| c.change_id.as_ref().map(|id| (id.clone(), c.path.clone())));

    TurnToolCallItem {
        call_id: call.call_id.clone(),
        tool_name: call.name.clone(),
        edit_count: 1,
        pres,
        status,
        is_waiting_approval,
        duration_ms: call.duration_ms,
        exit_code: call.exit_code,
        spilled: false,
        change_ref,
        resolution: call.resolution,
        audit: None,
        raw_result_first_line,
        request_link: extract_saved_request_link(
            &call.name,
            &call.arguments,
            out_text.as_deref(),
            status,
            thread.live_tool_meta.get(&call.call_id),
            session_ws_id,
        ),
        streams: call.output.clone(),
        result_parts: call.result_parts.clone(),
    }
}

fn build_saved_task_call(
    thread: &ChatThreadState,
    call_id: &str,
    name: &str,
    args: &str,
    tool_results: &HashMap<&str, (&[HubPart], bool)>,
    audit_map: &HashMap<&str, &AgentToolCallRecord>,
    run_active: bool,
) -> LiveToolCall {
    if let Some(live) = thread.tool_calls.iter().find(|c| c.call_id == call_id) {
        return live.clone();
    }
    let audit = audit_map.get(call_id).copied();
    let live_meta = thread.live_tool_meta.get(call_id);
    let result = tool_results.get(call_id).copied();
    let dur = audit
        .and_then(|a| match (a.started_ms, a.ended_ms) {
            (Some(s), Some(e)) if e >= s => Some((e - s) as u64),
            _ => None,
        })
        .or_else(|| live_meta.map(|m| m.duration_ms));
    let status = if run_active && thread.pending_tool_calls.contains(call_id) {
        None
    } else if let Some(m) = live_meta {
        Some(m.status)
    } else if result.is_some_and(|(_, is_err)| is_err) {
        Some(spanbay_core::agent::model::ToolCallStatus::Error)
    } else {
        Some(spanbay_core::agent::model::ToolCallStatus::Ok)
    };
    LiveToolCall {
        call_id: call_id.to_owned(),
        name: name.to_owned(),
        source: spanbay_core::agent::model::ToolSource::Builtin,
        arguments: args.to_owned(),
        output: Vec::new(),
        result_parts: result.map_or_else(Vec::new, |(pts, _)| pts.to_vec()),
        status,
        duration_ms: dur,
        exit_code: audit.and_then(|a| a.exit_code),
        resolution: None,
    }
}

/// Computes the status of a saved tool call from pending set, live meta, audit row, or tool result.
#[must_use]
pub fn compute_saved_call_status(
    run_active: bool,
    pending: bool,
    meta: Option<&LiveToolMeta>,
    audit: Option<&AgentToolCallRecord>,
    result: Option<(&[HubPart], bool)>,
) -> GraphNodeStatus {
    if run_active && pending {
        GraphNodeStatus::Running
    } else if let Some(meta) = meta {
        map_tool_call_status(meta.status)
    } else if let Some(audit) = audit {
        let settled = matches!(
            audit.state.as_str(),
            "output-available"
                | "output-error"
                | "output-denied"
                | "output-cancelled"
                | "approval-timed-out"
                | "approval-expired"
                | "output-superseded"
        );
        let placeholder = (!run_active && !settled)
            || result.is_some_and(|(parts, _)| is_pending_placeholder_parts(parts));
        map_audit_tool_state(audit, placeholder)
    } else if result.is_some_and(|(parts, _)| is_pending_placeholder_parts(parts)) {
        GraphNodeStatus::Interrupted
    } else if result.is_some_and(|(_, is_err)| is_err) {
        GraphNodeStatus::Failed
    } else {
        GraphNodeStatus::Completed
    }
}

fn resolve_plan_status(
    thread: &ChatThreadState,
    msg: &AgentMessage,
    call_id: &str,
    result: Option<(&[HubPart], bool)>,
    audit_map: &HashMap<&str, &AgentToolCallRecord>,
    run_active: bool,
) -> GraphNodeStatus {
    if run_active
        && (thread.open_gates.iter().any(|(_, g)| g.call_id == call_id)
            || thread.pending_tool_calls.contains(call_id))
    {
        GraphNodeStatus::Waiting
    } else if result.is_some()
        || audit_map.contains_key(call_id)
        || thread.live_tool_meta.contains_key(call_id)
    {
        let st = compute_saved_call_status(
            run_active,
            false,
            thread.live_tool_meta.get(call_id),
            audit_map.get(call_id).copied(),
            result,
        );
        if st == GraphNodeStatus::Failed
            && result.is_some_and(|(pts, _)| {
                pts.iter().any(
                    |p| matches!(p, HubPart::Text { text, .. } if text.starts_with("Cancelled")),
                )
            })
        {
            GraphNodeStatus::Cancelled
        } else if st == GraphNodeStatus::Running {
            GraphNodeStatus::Interrupted
        } else {
            st
        }
    } else if let Some(run) = thread
        .active_run
        .as_ref()
        .filter(|r| Some(r.run_id.as_str()) == msg.run_id.as_deref())
    {
        let st = map_run_status(run.status);
        if st.is_active() {
            GraphNodeStatus::Interrupted
        } else {
            st
        }
    } else {
        GraphNodeStatus::Interrupted
    }
}

fn compute_plan_revision_ctx(
    slice: &[AgentMessage],
    rel_ix: usize,
    msg: &AgentMessage,
    thread: &ChatThreadState,
    run_active: bool,
) -> (bool, Option<String>) {
    let after = &slice[rel_ix + 1..];
    let has_later_assistant = after.iter().any(|m| m.role == AgentRole::Assistant);
    let actively_revising = run_active
        && !has_later_assistant
        && thread
            .active_run
            .as_ref()
            .is_some_and(|r| Some(r.run_id.as_str()) == msg.run_id.as_deref());
    let next_user_text = after
        .iter()
        .find(|m| m.role == AgentRole::User)
        .and_then(|u| {
            let t = extract_plain_text(&u.parts);
            (!t.trim().is_empty()).then_some(t)
        });
    (actively_revising, next_user_text)
}

fn extract_plain_text(parts: &[HubPart]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            HubPart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
