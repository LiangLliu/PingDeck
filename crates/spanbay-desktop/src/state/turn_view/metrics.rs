//! Per-turn file change aggregation, fold statistics, and reasoning/thinking extraction
//! for [`AssistantTurnView`].

use std::collections::{BTreeMap, HashSet};

use spanbay_core::agent::model::{AgentMessage, FileChange, FileChangeKind, FileChangeStatus};
use spanbay_core::model::hub::HubPart;

use super::super::chat_reducer::{ChatThreadState, LiveFileChange, LiveToolMeta};
use super::super::run_graph::GraphNodeStatus;
use super::super::tool_trace::ToolCategory;
use super::explore::{is_readonly_explore_tool, is_search_explore_tool};
use super::{TurnActivityItem, TurnFileChangeSummary, TurnFoldStats, TurnMetricsSummary};

pub(super) fn compute_turn_file_changes(
    turn_call_ids: &[String],
    run_id: Option<&str>,
    persisted_changes: &[FileChange],
    live_changes: &[LiveFileChange],
) -> Vec<TurnFileChangeSummary> {
    let call_set: HashSet<&str> = turn_call_ids.iter().map(String::as_str).collect();
    let mut by_path: BTreeMap<String, usize> = BTreeMap::new();
    let mut summaries: Vec<TurnFileChangeSummary> = Vec::new();
    let mut seen_call_ids: HashSet<&str> = HashSet::new();

    for fc in persisted_changes {
        if fc.status == FileChangeStatus::Applying {
            continue;
        }
        let matches_turn = call_set.contains(fc.call_id.as_str())
            || (call_set.is_empty() && run_id.is_some_and(|r| r == fc.run_id));
        if !matches_turn {
            continue;
        }
        seen_call_ids.insert(fc.call_id.as_str());
        if let Some(&idx) = by_path.get(&fc.path) {
            let entry: &mut TurnFileChangeSummary = &mut summaries[idx];
            entry.added = entry.added.saturating_add(fc.added);
            entry.removed = entry.removed.saturating_add(fc.removed);
            entry.status = fc.status;
            entry.change_ids.push(fc.id.clone());
            entry.edit_count += 1;
        } else {
            let idx = summaries.len();
            by_path.insert(fc.path.clone(), idx);
            summaries.push(TurnFileChangeSummary {
                path: fc.path.clone(),
                kind: fc.kind,
                status: fc.status,
                added: fc.added,
                removed: fc.removed,
                change_ids: vec![fc.id.clone()],
                edit_count: 1,
            });
        }
    }

    for lc in live_changes {
        if !call_set.contains(lc.call_id.as_str()) || seen_call_ids.contains(lc.call_id.as_str()) {
            continue;
        }
        if let Some(&idx) = by_path.get(&lc.path) {
            let entry: &mut TurnFileChangeSummary = &mut summaries[idx];
            entry.added = entry.added.saturating_add(lc.added);
            entry.removed = entry.removed.saturating_add(lc.removed);
            if let Some(cid) = &lc.change_id {
                entry.change_ids.push(cid.clone());
            }
            entry.edit_count += 1;
        } else {
            let idx = summaries.len();
            by_path.insert(lc.path.clone(), idx);
            summaries.push(TurnFileChangeSummary {
                path: lc.path.clone(),
                kind: match lc.change {
                    spanbay_core::agent::tools::FileChangeKind::Create => FileChangeKind::Create,
                    spanbay_core::agent::tools::FileChangeKind::Edit => FileChangeKind::Edit,
                },
                status: FileChangeStatus::Pending,
                added: lc.added,
                removed: lc.removed,
                change_ids: lc.change_id.iter().cloned().collect(),
                edit_count: 1,
            });
        }
    }

    summaries
}

pub(super) fn extract_untracked_paths(
    thread: &ChatThreadState,
    is_latest_or_live: bool,
) -> Vec<String> {
    if !is_latest_or_live {
        return Vec::new();
    }
    let mut paths = Vec::new();
    for notice in &thread.notices {
        if notice.code.as_deref() == Some("untracked_changes")
            && let Some(raw) = notice.args.get("paths")
        {
            for p in raw
                .split([',', '、'])
                .map(str::trim)
                .filter(|s| !s.is_empty())
            {
                if !paths.iter().any(|existing| existing == p) {
                    paths.push(p.to_owned());
                }
            }
        }
    }
    paths
}

pub(super) fn compute_fold_stats_and_failures(
    spine: &[TurnActivityItem],
    turn_changes: &[TurnFileChangeSummary],
    totals: &TurnMetricsSummary,
) -> (TurnFoldStats, bool) {
    let mut stats = TurnFoldStats {
        duration_ms: totals.duration_ms,
        total_tools: totals.visible_tool_count,
        edited_files: turn_changes.len(),
        added_lines: turn_changes.iter().map(|c| c.added).sum(),
        removed_lines: turn_changes.iter().map(|c| c.removed).sum(),
        ..TurnFoldStats::default()
    };
    let mut has_failure = false;
    let mut edited_paths_fallback: HashSet<&str> = HashSet::new();

    for item in spine {
        match item {
            TurnActivityItem::ExploreGroup(g) => {
                stats.explored_files += g.files_read;
                stats.searches += g.searches_run;
            }
            TurnActivityItem::ToolCall(tc) => {
                if tc.should_auto_expand() {
                    has_failure = true;
                }
                if is_readonly_explore_tool(&tc.tool_name) {
                    if is_search_explore_tool(&tc.tool_name) {
                        stats.searches += 1;
                    } else {
                        stats.explored_files += 1;
                    }
                } else if tc.pres.category == ToolCategory::FileEdit {
                    if !tc.pres.target.is_empty() {
                        edited_paths_fallback.insert(tc.pres.target.as_str());
                    }
                } else if tc.pres.category == ToolCategory::Shell {
                    stats.shell_commands += 1;
                }
            }
            TurnActivityItem::SubagentGroup { calls, .. } => {
                if calls
                    .iter()
                    .any(|c| c.status == Some(spanbay_core::agent::model::ToolCallStatus::Error))
                {
                    has_failure = true;
                }
            }
            TurnActivityItem::PlanBlock { status, .. } => {
                if status.is_terminal_error() {
                    has_failure = true;
                }
            }
            TurnActivityItem::Reasoning { .. }
            | TurnActivityItem::IntermediateText { .. }
            | TurnActivityItem::SteerNote { .. }
            | TurnActivityItem::CompactionDivider { .. } => {}
        }
    }

    if stats.edited_files == 0 && !edited_paths_fallback.is_empty() {
        stats.edited_files = edited_paths_fallback.len();
    }

    (stats, has_failure)
}

/// Extracts `(reasoning_text, is_sealed, reasoning_tokens, cleaned_text_chunks)` from an
/// [`AgentMessage`], shared across `turn_view` and GPUI reasoning helpers.
#[must_use]
pub fn extract_message_reasoning_parts(
    msg: &AgentMessage,
) -> (String, bool, Option<u64>, Vec<String>) {
    let mut reasoning_chunks: Vec<String> = Vec::new();
    let mut text_chunks: Vec<String> = Vec::new();
    let mut has_sealed = false;

    for part in &msg.parts {
        match part {
            HubPart::Reasoning {
                text,
                signature,
                encrypted,
                id,
                ..
            } => {
                if !text.trim().is_empty() {
                    reasoning_chunks.push(text.clone());
                }
                if signature.as_deref().is_some_and(|s| !s.is_empty())
                    || encrypted.as_deref().is_some_and(|s| !s.is_empty())
                    || id.as_deref().is_some_and(|s| !s.is_empty())
                {
                    has_sealed = true;
                }
            }
            HubPart::Text { text, .. } if !text.is_empty() => {
                let (inline_r, cleaned) = extract_inline_thinking_text(text, false);
                reasoning_chunks.extend(inline_r);
                if !cleaned.is_empty() {
                    text_chunks.push(cleaned);
                }
            }
            _ => {}
        }
    }
    let reasoning_tokens = msg
        .usage
        .as_ref()
        .and_then(|u| u.reasoning_tokens)
        .filter(|&n| n > 0);
    let r_text = reasoning_chunks.join("\n\n");
    let sealed = r_text.is_empty() && (has_sealed || reasoning_tokens.is_some());
    (r_text, sealed, reasoning_tokens, text_chunks)
}

/// Extracts inline `<think>...</think>` or `<thinking>...</thinking>` blocks while preserving
/// fenced and inline code blocks.
#[must_use]
pub fn extract_inline_thinking_text(raw: &str, streaming: bool) -> (Vec<String>, String) {
    let mut reasoning = Vec::new();
    let mut answer = String::with_capacity(raw.len());
    let bytes = raw.as_bytes();
    let mut i = 0;
    let mut line_start = true;
    let mut fence: Option<(u8, usize)> = None;
    let mut inline_ticks: usize = 0;

    while i < bytes.len() {
        let b = bytes[i];
        if let Some((fc, flen)) = fence {
            if line_start && b == fc {
                let mut run = 0;
                while i + run < bytes.len() && bytes[i + run] == fc {
                    run += 1;
                }
                let after = i + run;
                let rest_line_blank = bytes[after..]
                    .iter()
                    .take_while(|&&c| c != b'\n')
                    .all(|c| c.is_ascii_whitespace());
                if run >= flen && rest_line_blank {
                    fence = None;
                }
                answer.push_str(&raw[i..after]);
                i = after;
                line_start = false;
                continue;
            }
        } else if inline_ticks > 0 {
            if b == b'`' {
                let mut run = 0;
                while i + run < bytes.len() && bytes[i + run] == b'`' {
                    run += 1;
                }
                if run == inline_ticks {
                    inline_ticks = 0;
                }
                answer.push_str(&raw[i..i + run]);
                i += run;
                line_start = false;
                continue;
            } else if b == b'\n' {
                inline_ticks = 0;
            }
        } else {
            if (b == b'`' || b == b'~') && line_start {
                let mut run = 0;
                while i + run < bytes.len() && bytes[i + run] == b {
                    run += 1;
                }
                if run >= 3 {
                    fence = Some((b, run));
                    answer.push_str(&raw[i..i + run]);
                    i += run;
                    line_start = false;
                    continue;
                }
            }
            if b == b'`' {
                let mut run = 0;
                while i + run < bytes.len() && bytes[i + run] == b'`' {
                    run += 1;
                }
                if raw[i + run..]
                    .split('\n')
                    .next()
                    .is_some_and(|l| l.contains('`'))
                {
                    inline_ticks = run;
                }
                answer.push_str(&raw[i..i + run]);
                i += run;
                line_start = false;
                continue;
            }
            let tail = &raw[i..];
            let matched_tag = if tail.starts_with("<think>") {
                Some(("<think>".len(), "</think>"))
            } else if tail.starts_with("<thinking>") {
                Some(("<thinking>".len(), "</thinking>"))
            } else {
                None
            };
            if let Some((open_len, close_tag)) = matched_tag {
                let after_open = &raw[i + open_len..];
                if let Some(close_idx) = after_open.find(close_tag) {
                    let inner = after_open[..close_idx].trim();
                    if !inner.is_empty() {
                        reasoning.push(inner.to_owned());
                    }
                    i += open_len + close_idx + close_tag.len();
                    line_start = i > 0 && bytes[i - 1] == b'\n';
                    continue;
                } else if streaming {
                    let inner = after_open.trim_start();
                    if !inner.is_empty() {
                        reasoning.push(inner.to_owned());
                    }
                    break;
                } else {
                    answer.push_str(tail);
                    break;
                }
            }
        }
        let ch = raw[i..].chars().next().unwrap_or('\0');
        let clen = ch.len_utf8();
        answer.push(ch);
        if ch == '\n' {
            line_start = true;
        } else if ch != ' ' && ch != '\t' {
            line_start = false;
        }
        i += clen;
    }

    (reasoning, answer.trim().to_owned())
}

pub(super) fn extract_saved_request_link(
    tool_name: &str,
    args: &str,
    out_text: Option<&str>,
    status: GraphNodeStatus,
    meta: Option<&LiveToolMeta>,
    session_ws_id: Option<&str>,
) -> Option<(Option<String>, String)> {
    use spanbay_core::agent::model::tools::ToolResultView;
    if let Some(raw) = out_text {
        let json_head = raw.split("\n\nBody (").next().unwrap_or(raw).trim();
        if let Ok(val) = serde_json::from_str::<serde_json::Value>(json_head) {
            let ws_from_val = val
                .get("view")
                .and_then(|v| v.get("workspace_id"))
                .and_then(|v| v.as_str())
                .or_else(|| {
                    val.get("workspace")
                        .and_then(|w| w.get("id"))
                        .and_then(|v| v.as_str())
                })
                .or(session_ws_id)
                .filter(|s| !s.is_empty())
                .map(str::to_owned);
            if let Some(view) = val.get("view")
                && view.get("kind").and_then(|v| v.as_str()) == Some("saved_request")
                && let Some(rid) = view
                    .get("request_id")
                    .and_then(|v| v.as_str())
                    .or_else(|| val.get("request_id").and_then(|v| v.as_str()))
                    .filter(|s| !s.is_empty())
            {
                return Some((ws_from_val, rid.to_owned()));
            }
            if status == GraphNodeStatus::Completed
                && matches!(tool_name, "create_request" | "get_request")
                && let Some(rid) = val
                    .get("request_id")
                    .and_then(|v| v.as_str())
                    .filter(|s| !s.is_empty())
            {
                return Some((ws_from_val, rid.to_owned()));
            }
        }
    }
    if let Some(ToolResultView::SavedRequest { request_id }) = meta.and_then(|m| m.link.as_ref()) {
        return Some((
            session_ws_id.filter(|s| !s.is_empty()).map(str::to_owned),
            request_id.clone(),
        ));
    }
    if status == GraphNodeStatus::Completed
        && matches!(tool_name, "create_request" | "get_request")
        && let Ok(val) = serde_json::from_str::<serde_json::Value>(args)
        && let Some(rid) = val
            .get("request_id")
            .and_then(|v| v.as_str())
            .map(str::trim)
            .filter(|s| !s.is_empty())
    {
        return Some((
            session_ws_id.filter(|s| !s.is_empty()).map(str::to_owned),
            rid.to_owned(),
        ));
    }
    None
}
