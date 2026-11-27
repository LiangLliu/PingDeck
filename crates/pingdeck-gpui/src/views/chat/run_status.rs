//! Shared lifecycle status encoding, elapsed-time formatting, and horizontal waterfall timing
//! bars used across the Run Monitor, Execution Topology tree, Chat Sidebar, and Inspector.

use gpui_kit::{
    AnyElement, App, Context, Div, IntoElement, ParentElement as _, SharedString, Styled as _, div,
    px,
};
use pingdeck_desktop::core::agent::dto::{AgentBoard, BoardWait, TopologyToolStep};
use pingdeck_desktop::core::agent::model::{AgentRunStatus, SubagentRole};
use pingdeck_desktop::state::GraphNodeStatus;

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, btn_danger, btn_icon, h_row, icon_xs, palette,
    status_indicator, to_hsla, v_col,
};

use super::ChatView;

/// Maps an agent run's lifecycle status + wait reason to an accessible geometric indicator,
/// semantic tone, and localized status label.
#[must_use]
pub fn run_status_meta(
    status: AgentRunStatus,
    wait: Option<BoardWait>,
    status_detail: Option<&str>,
    cx: &App,
) -> (StatusShape, Tone, &'static str) {
    let ch = crate::ui::tr(cx).chat();
    match status {
        AgentRunStatus::Queued => (StatusShape::Idle, Tone::Neutral, ch.run_status_queued()),
        AgentRunStatus::Running => (StatusShape::Run, Tone::Accent, ch.run_status_running()),
        AgentRunStatus::WaitingApproval => {
            let label = if wait == Some(BoardWait::CostLimit) || status_detail == Some("cost limit")
            {
                ch.run_status_waiting_cost()
            } else if status_detail == Some("plan") {
                ch.run_status_waiting_plan()
            } else if status_detail == Some("question") {
                ch.run_status_waiting_question()
            } else {
                ch.run_status_waiting_approval()
            };
            (StatusShape::Warn, Tone::Warning, label)
        }
        AgentRunStatus::Completed => (StatusShape::Ok, Tone::Success, ch.run_status_completed()),
        AgentRunStatus::Failed => (StatusShape::Bad, Tone::Danger, ch.run_status_failed()),
        AgentRunStatus::Cancelled => (StatusShape::Idle, Tone::Neutral, ch.run_status_cancelled()),
        AgentRunStatus::Interrupted => (
            StatusShape::Warn,
            Tone::Warning,
            ch.run_status_interrupted(),
        ),
    }
}

/// Maps a normalized [`TopologyToolStep::status`] string to indicator shape + tone.
#[must_use]
pub fn tool_status_meta(status: &str) -> (StatusShape, Tone) {
    match status {
        "ok" => (StatusShape::Ok, Tone::Success),
        "running" => (StatusShape::Run, Tone::Accent),
        "waiting_approval" | "denied" | "timed_out" => (StatusShape::Warn, Tone::Warning),
        "error" => (StatusShape::Bad, Tone::Danger),
        _ => (StatusShape::Idle, Tone::Neutral),
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
pub fn waterfall_bar(left_frac: f32, width_frac: f32, track_w: f32, tone: Tone, cx: &App) -> Div {
    let p = palette(cx);
    let fill = match tone {
        Tone::Accent => to_hsla(p.accent),
        Tone::Success => to_hsla(p.ok),
        Tone::Warning => to_hsla(p.warn),
        Tone::Danger => to_hsla(p.bad),
        Tone::Neutral => to_hsla(p.faint),
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

/// Top global telemetry bar rendered above the Run Monitor master/detail split.
pub fn render_telemetry_bar(board: &AgentBoard, cx: &mut Context<ChatView>) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let max_slots = usize::from(board.limits.max_concurrent_runs.max(1));
    let running_now = board.running.iter().filter(|r| !r.waiting_for_slot).count();
    let queued_now = board.running.iter().filter(|r| r.waiting_for_slot).count();
    let active_total = board.running.len()
        + board
            .needs_you
            .iter()
            .filter(|r| r.run.status.is_active())
            .count();
    let waiting_count = board.needs_you.len();
    let (ok_24h, fail_24h) = count_24h_outcomes(board);
    let tok_total = board.tokens_today.input + board.tokens_today.output;
    let cost_txt = format_cost_today(board);

    let mut slot_pips = h_row().gap(px(3.0));
    for idx in 0..max_slots.min(8) {
        let filled = idx < running_now;
        slot_pips = slot_pips.child(
            div()
                .size(px(7.0))
                .rounded(px(999.0))
                .bg(to_hsla(if filled { p.accent } else { p.line_strong })),
        );
    }

    let left_metrics = h_row()
        .flex_1()
        .min_w_0()
        .gap(px(10.0))
        .flex_wrap()
        .child(
            h_row()
                .gap(px(6.0))
                .child(
                    div()
                        .text_color(to_hsla(p.accent))
                        .child(icon_xs(UiIcon::Activity)),
                )
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(ch.monitor_title()),
                ),
        )
        .child(
            h_row()
                .gap(px(6.0))
                .px(px(8.0))
                .py(px(3.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.sunken))
                .child(slot_pips)
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .child(SharedString::from(ch.monitor_slots(running_now, max_slots))),
                ),
        )
        .children(
            (queued_now > 0).then(|| badge(ch.monitor_queued_count(queued_now), Tone::Neutral, cx)),
        )
        .children((board.active_subagents > 0).then(|| {
            badge(
                ch.monitor_subagents_active(board.active_subagents),
                Tone::Accent,
                cx,
            )
        }))
        .children(
            (waiting_count > 0)
                .then(|| badge(ch.monitor_attention_count(waiting_count), Tone::Warning, cx)),
        )
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(ch.monitor_outcome_24h(ok_24h, fail_24h))),
        );

    let right_controls = h_row()
        .flex_shrink_0()
        .gap(px(8.0))
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(format!(
                    "{} · {cost_txt}",
                    ch.sidebar_today_tok(format_tokens_compact(tok_total))
                ))),
        )
        .child(
            btn_icon("monitor-refresh", UiIcon::Refresh)
                .tooltip(ch.monitor_refresh())
                .on_click(cx.listener(|this, _, _, cx| this.refresh_monitor(cx))),
        )
        .children((active_total > 0).then(|| {
            btn_danger("monitor-stop-all", ch.monitor_stop_all())
                .on_click(cx.listener(|this, _, _, cx| this.request_stop_all_runs(cx)))
        }));

    h_row()
        .w_full()
        .flex_shrink_0()
        .justify_between()
        .gap(px(12.0))
        .px(px(16.0))
        .py(px(10.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(left_metrics)
        .child(right_controls)
        .into_any_element()
}

fn count_24h_outcomes(board: &AgentBoard) -> (usize, usize) {
    let mut ok = 0usize;
    let mut failed = 0usize;
    for r in board
        .done
        .iter()
        .chain(board.needs_you.iter().filter(|r| !r.run.status.is_active()))
    {
        match r.run.status {
            AgentRunStatus::Completed => ok += 1,
            AgentRunStatus::Failed | AgentRunStatus::Interrupted => failed += 1,
            _ => {}
        }
    }
    (ok, failed)
}

fn format_cost_today(board: &AgentBoard) -> String {
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
pub(super) fn compact_tool_row(step: &TopologyToolStep, cx: &App) -> AnyElement {
    let p = palette(cx);
    let (shape, _) = tool_status_meta(&step.status);
    v_col()
        .w_full()
        .gap(px(2.0))
        .px(px(8.0))
        .py(px(6.0))
        .rounded(px(6.0))
        .bg(to_hsla(p.surface))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(6.0))
                .child(
                    h_row()
                        .flex_1()
                        .min_w_0()
                        .gap(px(6.0))
                        .child(status_indicator(shape, cx))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .child(SharedString::from(step.name.clone())),
                        )
                        .children(
                            step.exit_code
                                .filter(|c| *c != 0)
                                .map(|c| badge(format!("exit {c}"), Tone::Danger, cx)),
                        ),
                )
                .children(step.duration_ms.map(|ms| {
                    div()
                        .text_size(px(10.5))
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(format_elapsed_short(ms)))
                })),
        )
        .children((!step.summary.is_empty()).then(|| {
            div()
                .text_size(px(11.0))
                .text_color(to_hsla(p.muted))
                .truncate()
                .child(SharedString::from(step.summary.clone()))
        }))
        .into_any_element()
}

/// Localized status label for a [`GraphNodeStatus`].
#[must_use]
pub fn graph_status_label(status: GraphNodeStatus, cx: &App) -> &'static str {
    let ch = crate::ui::tr(cx).chat();
    match status {
        GraphNodeStatus::Pending | GraphNodeStatus::Queued => ch.run_status_queued(),
        GraphNodeStatus::Running => ch.run_status_running(),
        GraphNodeStatus::Waiting => ch.run_status_waiting_approval(),
        GraphNodeStatus::Completed => ch.run_status_completed(),
        GraphNodeStatus::Failed => ch.run_status_failed(),
        GraphNodeStatus::Cancelled | GraphNodeStatus::Skipped => ch.run_status_cancelled(),
        GraphNodeStatus::Interrupted => ch.run_status_interrupted(),
    }
}
