//! Shared lifecycle status encoding, elapsed/token formatting, relative age formatting,
//! and horizontal waterfall timing bar helpers reused by `RunsView`, `WorkflowMonitorModal`,
//! and `ChatView`.

use gpui_kit::{
    AnyElement, App, Div, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};
use spanbay_desktop::core::agent::dto::{AgentBoard, TopologyToolStep};
use spanbay_desktop::core::agent::model::{AgentRunStatus, FileChangeStatus, SubagentRole};
use spanbay_desktop::core::agent::tools::process::ProcessState;
use spanbay_desktop::state::{GraphNodeKind, map_tool_status};

use crate::ui::{StatusShape, exit_code_badge, palette, status_row, to_hsla, v_col};

/// Localized label for a [`GraphNodeKind`].
#[must_use]
pub fn graph_kind_label(kind: GraphNodeKind, cx: &App) -> &'static str {
    let ch = crate::ui::tr(cx).chat();
    match kind {
        GraphNodeKind::Main => ch.monitor_main_agent(),
        GraphNodeKind::Explore => ch.graph_kind_explore(),
        GraphNodeKind::Implement => ch.graph_kind_implement(),
        GraphNodeKind::Verify => ch.graph_kind_verify(),
        GraphNodeKind::Gate => ch.graph_kind_gate(),
        GraphNodeKind::Tool => ch.graph_kind_tool(),
        GraphNodeKind::Group => ch.graph_parallel_group(),
    }
}

/// Localized label for a sub-agent role (`Fast` / `Deep` / `Vision`).
#[must_use]
pub fn subagent_role_label(role: SubagentRole, cx: &App) -> &'static str {
    let ch = crate::ui::tr(cx).chat();
    match role {
        SubagentRole::Fast => ch.monitor_role_fast(),
        SubagentRole::Deep => ch.monitor_role_deep(),
        SubagentRole::Vision => ch.monitor_role_vision(),
    }
}

/// Localized label for a sub-agent kind (`explore` / `general`).
#[must_use]
pub fn subagent_kind_label(kind: &str, cx: &App) -> &'static str {
    let ch = crate::ui::tr(cx).chat();
    if kind.eq_ignore_ascii_case("explore") {
        ch.monitor_kind_explore()
    } else {
        ch.monitor_kind_general()
    }
}

/// Localized label for a [`FileChangeStatus`].
#[must_use]
pub fn file_change_status_label(status: FileChangeStatus, cx: &App) -> &'static str {
    let cm = crate::ui::tr(cx).common();
    match status {
        FileChangeStatus::Pending => cm.change_pending(),
        FileChangeStatus::Kept => cm.change_kept(),
        FileChangeStatus::Undone => cm.change_undone(),
        FileChangeStatus::Conflict => cm.change_conflict(),
        FileChangeStatus::Applying => cm.change_applying(),
    }
}

/// Localized label for a [`ProcessState`].
#[must_use]
pub fn process_state_label(state: ProcessState, cx: &App) -> &'static str {
    let tr = crate::ui::tr(cx);
    match state {
        ProcessState::Running => tr.chat().run_status_running(),
        ProcessState::Exited => tr.common().process_exited(),
        ProcessState::Killed => tr.common().process_killed(),
        ProcessState::Expired => tr.chat().run_status_expired(),
    }
}

/// Formats an elapsed duration in milliseconds into compact human-readable form.
#[must_use]
pub fn format_elapsed_short(ms: u64) -> String {
    if ms < 1_000 {
        format!("{ms}ms")
    } else if ms < 60_000 {
        format!("{:.1}s", ms as f64 / 1_000.0)
    } else if ms < 3_600_000 {
        let secs = ms / 1_000;
        format!("{}m {:02}s", secs / 60, secs % 60)
    } else {
        let mins = ms / 60_000;
        format!("{}h {:02}m", mins / 60, mins % 60)
    }
}

/// Formats a compact token count (e.g. `142.8k`, `1.2M`, `850`).
#[must_use]
pub fn format_tokens_compact(tokens: u64) -> String {
    if tokens >= 1_000_000 {
        format!("{:.1}M", tokens as f64 / 1_000_000.0)
    } else if tokens >= 1_000 {
        format!("{:.1}k", tokens as f64 / 1_000.0)
    } else {
        tokens.to_string()
    }
}

/// Formats a compact relative age label (`now`, `5m`, `3h`, `2d`).
#[must_use]
pub fn format_relative_age(ts_ms: i64, now_ms: i64, cx: &App) -> String {
    let ch = crate::ui::tr(cx).chat();
    let diff_s = now_ms.saturating_sub(ts_ms).max(0) / 1_000;
    if diff_s < 60 {
        ch.sidebar_now().to_owned()
    } else if diff_s < 3_600 {
        ch.sidebar_m(diff_s / 60)
    } else if diff_s < 86_400 {
        ch.sidebar_h(diff_s / 3_600)
    } else {
        ch.sidebar_d(diff_s / 86_400)
    }
}

/// Computes normalized `(left_fraction, width_fraction)` inside `[0.0, 1.0]` for a waterfall bar
/// aligned to the parent run's time span `[run_start .. run_start + run_total_ms]`.
#[must_use]
pub fn compute_bar_span(
    run_start_ms: i64,
    run_total_ms: i64,
    item_start_ms: Option<i64>,
    item_end_ms: Option<i64>,
    snapshot_ms: i64,
) -> (f32, f32) {
    let total = run_total_ms.max(1) as f64;
    let start = item_start_ms.unwrap_or(run_start_ms);
    let end = item_end_ms.unwrap_or(snapshot_ms).max(start);
    let left = ((start - run_start_ms).max(0) as f64 / total).clamp(0.0, 0.96);
    let raw_width = ((end - start).max(0) as f64 / total).clamp(0.04, 1.0 - left);
    (left as f32, raw_width as f32)
}

/// Renders a precision horizontal waterfall timing bar on a subtle grid track.
#[must_use]
pub fn waterfall_bar(
    left_frac: f32,
    width_frac: f32,
    track_w: f32,
    shape: StatusShape,
    cx: &App,
) -> Div {
    let p = palette(cx);
    let fill = match shape {
        StatusShape::Run => to_hsla(p.accent),
        StatusShape::Ok | StatusShape::Live => to_hsla(p.ok),
        StatusShape::Warn => to_hsla(p.warn),
        StatusShape::Bad => to_hsla(p.bad),
        StatusShape::Idle | StatusShape::Stopped => to_hsla(p.faint),
    };
    let track_bg = to_hsla(p.sunken);
    let tick_col = to_hsla(p.line.composite_over(p.sunken));
    let left_px = (left_frac * track_w).clamp(0.0, (track_w - 4.0).max(0.0));
    let width_px = (width_frac * track_w).clamp(4.0, (track_w - left_px).max(4.0));

    div()
        .relative()
        .flex_shrink_0()
        .w(px(track_w))
        .h(px(12.0))
        .rounded(px(3.0))
        .bg(track_bg)
        .overflow_hidden()
        .child(
            div()
                .absolute()
                .left(px(track_w * 0.5))
                .top(px(0.0))
                .w(px(1.0))
                .h(px(12.0))
                .bg(tick_col),
        )
        .child(
            div()
                .absolute()
                .left(px(left_px))
                .top(px(2.0))
                .w(px(width_px))
                .h(px(8.0))
                .rounded(px(2.5))
                .bg(fill),
        )
}

/// Extracts short folder basename from a full workspace directory path.
#[must_use]
pub fn folder_basename(path: &str) -> &str {
    std::path::Path::new(path)
        .file_name()
        .and_then(|n| n.to_str())
        .unwrap_or(path)
}

/// Formats a raw `provider_id/model_id` string concisely when no display map is needed.
#[must_use]
pub fn format_model_short(raw: &str) -> String {
    raw.split_once('/')
        .map_or_else(|| raw.to_owned(), |(_, m)| m.to_owned())
}

/// Counts 24h ok / failed outcomes across the global board snapshot.
#[must_use]
pub fn count_24h_outcomes(board: &AgentBoard) -> (usize, usize) {
    let mut ok = 0usize;
    let mut failed = 0usize;
    for r in board
        .done
        .iter()
        .chain(board.needs_you.iter().filter(|r| !r.run.status.is_active()))
    {
        match r.run.status {
            AgentRunStatus::Completed => ok += 1,
            AgentRunStatus::Failed => failed += 1,
            _ => {}
        }
    }
    (ok, failed)
}

/// Formats today's per-currency cost summary for the telemetry bar.
#[must_use]
pub fn format_cost_today(board: &AgentBoard) -> String {
    if board.cost_today.is_empty() {
        "—".to_owned()
    } else {
        board
            .cost_today
            .iter()
            .map(|c| format!("{} {:.3}", c.currency, c.cost_micro as f64 / 1_000_000.0))
            .collect::<Vec<_>>()
            .join(" + ")
    }
}

/// Compact read-only tool step card reused by `Node Inspector`.
pub fn compact_tool_row(step: &TopologyToolStep, cx: &App) -> AnyElement {
    let p = palette(cx);
    let meta = step
        .duration_ms
        .map(|ms| SharedString::from(format_elapsed_short(ms)));
    let exit = step
        .exit_code
        .filter(|c| *c != 0)
        .map(|c| exit_code_badge(c, cx).into_any_element());
    v_col()
        .w_full()
        .gap(px(2.0))
        .py(px(2.0))
        .rounded(px(6.0))
        .bg(to_hsla(p.surface))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(status_row(
            SharedString::from(format!("tool-row-{}", step.call_id)),
            map_tool_status(&step.status),
            step.name.clone(),
            meta,
            exit,
            cx,
        ))
        .children((!step.summary.is_empty()).then(|| {
            div()
                .px(px(8.0))
                .pb(px(4.0))
                .text_size(px(11.0))
                .text_color(to_hsla(p.muted))
                .truncate()
                .child(SharedString::from(step.summary.clone()))
        }))
        .into_any_element()
}
