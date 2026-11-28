//! Consecutive file-edit coalescing, `ExploreGroup` merging, path deduplication, and `└ Read a, b`
//! sub-row coalescing for [`AssistantTurnView`] (`.overhaul/chat-redesign-spec.md` §4).

use std::collections::HashSet;

use super::super::run_graph::GraphNodeStatus;
use super::super::tool_trace::{ToolCategory, ToolInlineMetric, classify_tool};
use super::{ExploreGroupView, TurnActivityItem, TurnToolCallItem};

/// Returns true if a tool call is a read-only exploration step eligible for `ExploreGroup`.
#[must_use]
pub fn is_readonly_explore_tool(name: &str) -> bool {
    matches!(
        classify_tool(name),
        ToolCategory::FileRead | ToolCategory::FileSearch | ToolCategory::OutputRead
    ) || matches!(
        name,
        "get_request"
            | "list_requests"
            | "list_environments"
            | "get_trace"
            | "query_gateway_log"
            | "web_search"
    )
}

/// Returns true if a tool name is counted as a search in `ExploreGroup` (`grep`, `glob`, `web_search`, `query_gateway_log`).
#[must_use]
pub fn is_search_explore_tool(name: &str) -> bool {
    matches!(name, "grep" | "glob" | "web_search" | "query_gateway_log")
}

/// Extracts a concise basename (or trimmed target) for merged `Read a, b, c` sub-rows.
fn short_file_label(target: &str) -> &str {
    let trimmed = target.trim().trim_end_matches('/');
    trimmed
        .rsplit(['/', '\\'])
        .next()
        .filter(|s| !s.is_empty())
        .unwrap_or(trimmed)
}

/// Merges consecutive `FileRead` items inside an `ExploreGroup` into one `TurnToolCallItem`
/// displaying deduplicated basenames (`router.rs, limits.rs`) like the prototype.
fn merge_explore_sub_items(items: Vec<TurnToolCallItem>) -> Vec<TurnToolCallItem> {
    let mut out: Vec<TurnToolCallItem> = Vec::with_capacity(items.len());
    let mut read_buf: Vec<TurnToolCallItem> = Vec::new();

    let flush_reads = |buf: &mut Vec<TurnToolCallItem>, dest: &mut Vec<TurnToolCallItem>| {
        if buf.is_empty() {
            return;
        }
        if buf.len() == 1 {
            if let Some(single) = buf.pop() {
                dest.push(single);
            }
            return;
        }
        let batch = std::mem::take(buf);
        let mut merged = batch[0].clone();
        let mut seen_paths: Vec<String> = Vec::new();
        let mut labels: Vec<String> = Vec::new();
        let mut dur_sum = 0u64;
        let mut any_dur = false;

        for item in &batch {
            let key = item
                .pres
                .file_path
                .as_deref()
                .unwrap_or(item.pres.target.as_str())
                .trim();
            if !key.is_empty() && !seen_paths.iter().any(|p| p == key) {
                seen_paths.push(key.to_owned());
                labels.push(short_file_label(&item.pres.target).to_owned());
            }
            if let Some(d) = item.duration_ms {
                dur_sum = dur_sum.saturating_add(d);
                any_dur = true;
            }
            if item.status == GraphNodeStatus::Running {
                merged.status = GraphNodeStatus::Running;
            }
        }
        merged.edit_count = seen_paths.len().max(1);
        if seen_paths.len() > 1 {
            merged.pres.target = labels.join(", ");
            merged.pres.metric = Some(ToolInlineMetric::Files(seen_paths.len()));
        }
        merged.duration_ms = any_dur.then_some(dur_sum);
        dest.push(merged);
    };

    for item in items {
        if item.pres.category == ToolCategory::FileRead {
            read_buf.push(item);
        } else {
            flush_reads(&mut read_buf, &mut out);
            out.push(item);
        }
    }
    flush_reads(&mut read_buf, &mut out);
    out
}

/// Counts distinct files read/listed and total searches across a slice of read-only tool calls.
pub(super) fn count_explore_files_and_searches(items: &[TurnToolCallItem]) -> (usize, usize) {
    let mut distinct_files: HashSet<String> = HashSet::new();
    let mut anonymous_reads = 0usize;
    let mut searches = 0usize;

    for c in items {
        if is_search_explore_tool(&c.tool_name) {
            searches += 1;
        } else {
            let key = c
                .pres
                .file_path
                .as_deref()
                .unwrap_or(c.pres.target.as_str())
                .trim();
            if key.is_empty() {
                anonymous_reads += 1;
            } else {
                distinct_files.insert(key.to_owned());
            }
        }
    }
    (distinct_files.len() + anonymous_reads, searches)
}

/// Merges consecutive read-only exploration tool calls into [`TurnActivityItem::ExploreGroup`]
/// (when ≥2 consecutive read-only calls occur without failure/waiting) and coalesces adjacent
/// edits to the same file path into a single [`TurnToolCallItem`] with `edit_count > 1`.
pub(super) fn coalesce_spine_items(raw: Vec<TurnActivityItem>) -> Vec<TurnActivityItem> {
    // Pass 1: coalesce consecutive completed `FileEdit` calls targeting the exact same non-empty path.
    let mut pass1: Vec<TurnActivityItem> = Vec::with_capacity(raw.len());
    for item in raw {
        if let TurnActivityItem::ToolCall(curr) = item {
            if curr.pres.category == ToolCategory::FileEdit
                && curr.status == GraphNodeStatus::Completed
                && !curr.pres.target.is_empty()
                && let Some(TurnActivityItem::ToolCall(prev)) = pass1.last_mut()
                && prev.pres.category == ToolCategory::FileEdit
                && prev.status == GraphNodeStatus::Completed
                && prev.pres.target == curr.pres.target
            {
                prev.edit_count += curr.edit_count;
                if let (
                    Some(ToolInlineMetric::DiffStats {
                        added: pa,
                        removed: pr,
                    }),
                    Some(ToolInlineMetric::DiffStats {
                        added: ca,
                        removed: cr,
                    }),
                ) = (&mut prev.pres.metric, &curr.pres.metric)
                {
                    *pa = pa.saturating_add(*ca);
                    *pr = pr.saturating_add(*cr);
                } else if prev.pres.metric.is_none() {
                    prev.pres.metric = curr.pres.metric.clone();
                }
                if let Some(d2) = curr.duration_ms {
                    prev.duration_ms = Some(prev.duration_ms.unwrap_or(0).saturating_add(d2));
                }
                if curr.change_ref.is_some() {
                    prev.change_ref = curr.change_ref;
                }
                continue;
            }
            pass1.push(TurnActivityItem::ToolCall(curr));
        } else {
            pass1.push(item);
        }
    }

    // Pass 2: group ≥2 consecutive read-only exploration calls that are Running or Completed
    // (never hide a failed, blocked, denied, or waiting-approval call inside an ExploreGroup).
    let mut out: Vec<TurnActivityItem> = Vec::with_capacity(pass1.len());
    let mut ro_buf: Vec<TurnToolCallItem> = Vec::new();

    let flush_ro = |buf: &mut Vec<TurnToolCallItem>, dest: &mut Vec<TurnActivityItem>| {
        if buf.is_empty() {
            return;
        }
        if buf.len() == 1 {
            if let Some(single) = buf.pop() {
                dest.push(TurnActivityItem::ToolCall(Box::new(single)));
            }
            return;
        }
        let raw_items = std::mem::take(buf);
        let key = format!("exp-{}", raw_items[0].call_id);
        let running = raw_items
            .iter()
            .any(|c| c.status == GraphNodeStatus::Running);
        let (files_read, searches_run) = count_explore_files_and_searches(&raw_items);
        let mut dur_sum = 0u64;
        let mut any_dur = false;
        for c in &raw_items {
            if let Some(d) = c.duration_ms {
                dur_sum = dur_sum.saturating_add(d);
                any_dur = true;
            }
        }
        let items = merge_explore_sub_items(raw_items);
        dest.push(TurnActivityItem::ExploreGroup(ExploreGroupView {
            key,
            running,
            files_read,
            searches_run,
            duration_ms: any_dur.then_some(dur_sum),
            items,
        }));
    };

    for item in pass1 {
        match item {
            TurnActivityItem::ToolCall(tc)
                if is_readonly_explore_tool(&tc.tool_name)
                    && !tc.is_waiting_approval
                    && !tc.should_auto_expand()
                    && matches!(
                        tc.status,
                        GraphNodeStatus::Running | GraphNodeStatus::Completed
                    ) =>
            {
                ro_buf.push(*tc);
            }
            other => {
                flush_ro(&mut ro_buf, &mut out);
                out.push(other);
            }
        }
    }
    flush_ro(&mut ro_buf, &mut out);
    out
}
