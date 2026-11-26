//! Full-canvas Run Monitor (`运行看板`) rendered in the Chat center stage.
//!
//! Layout:
//! - Top telemetry bar: concurrency slot gauge (`used/max`), queued count, active sub-agents,
//!   waiting count, 24h ok/failed ratio, today's tokens & cost, Refresh, and Stop All.
//! - Left master feed (340px): search + lifecycle filters (`All` / `Waiting` / `Active` /
//!   `Failed` / `Done`) + grouped runs with accurate 7-state indicators, sub-agent summaries,
//!   and quick stop buttons.
//! - Right detail canvas (`flex-1`): selected run header, blocker/failure diagnostics, and
//!   full parallel/serial [`render_topology`] waterfall timeline.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::dto::{AgentBoard, AgentBoardRow};
use pingdeck_desktop::core::agent::model::AgentRunStatus;

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_icon, choice_chip, empty_inline,
    h_row, palette, search_input, status_indicator, to_hsla, v_col,
};

use super::ChatView;
use super::run_monitor_detail::{render_selected_detail, render_telemetry_bar};
use super::run_status::{format_elapsed_short, run_status_meta};
use super::sidebar_board::group_label;
use super::types::MonitorFilter;

pub fn render_run_monitor(view: &ChatView, cx: &mut Context<ChatView>) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();

    let Some(board) = &view.board else {
        return div()
            .size_full()
            .bg(to_hsla(p.surface))
            .flex()
            .items_center()
            .justify_center()
            .child(empty_inline(
                ch.sidebar_board_loading_board(),
                None,
                None,
                cx,
            ))
            .into_any_element();
    };

    let telemetry = render_telemetry_bar(board, cx);
    let feed = render_run_feed(view, board, cx);
    let detail = render_selected_detail(view, board, cx);

    v_col()
        .size_full()
        .min_w_0()
        .bg(to_hsla(p.surface))
        .child(telemetry)
        .child(
            h_row()
                .flex_1()
                .min_h_0()
                .w_full()
                .items_stretch()
                .child(feed)
                .child(detail),
        )
        .into_any_element()
}

fn render_run_feed(view: &ChatView, board: &AgentBoard, cx: &mut Context<ChatView>) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let query = view
        .monitor_search_input
        .read(cx)
        .value()
        .trim()
        .to_lowercase();

    let waiting_n = board.needs_you.len();
    let running_n = board.running.len();
    let failed_n = all_board_rows(board)
        .filter(|r| {
            matches!(
                r.run.status,
                AgentRunStatus::Failed | AgentRunStatus::Interrupted
            )
        })
        .count();

    let filter_bar = h_row()
        .w_full()
        .gap(px(4.0))
        .flex_wrap()
        .child(filter_pill(
            "mf-all",
            ch.monitor_filter_all().to_owned(),
            MonitorFilter::All,
            view.monitor_filter,
            cx,
        ))
        .child(filter_pill(
            "mf-wait",
            ch.monitor_filter_waiting(waiting_n),
            MonitorFilter::Waiting,
            view.monitor_filter,
            cx,
        ))
        .child(filter_pill(
            "mf-run",
            ch.monitor_filter_running(running_n),
            MonitorFilter::Running,
            view.monitor_filter,
            cx,
        ))
        .child(filter_pill(
            "mf-fail",
            ch.monitor_filter_failed(failed_n),
            MonitorFilter::Failed,
            view.monitor_filter,
            cx,
        ))
        .child(filter_pill(
            "mf-done",
            ch.monitor_filter_done().to_owned(),
            MonitorFilter::Done,
            view.monitor_filter,
            cx,
        ));

    let matches_row = |row: &AgentBoardRow| -> bool {
        let filter_ok = match view.monitor_filter {
            MonitorFilter::All => true,
            MonitorFilter::Waiting => {
                row.wait.is_some()
                    || row.approval_timed_out
                    || row.review
                    || row.run.status == AgentRunStatus::WaitingApproval
            }
            MonitorFilter::Running => {
                matches!(
                    row.run.status,
                    AgentRunStatus::Queued | AgentRunStatus::Running
                )
            }
            MonitorFilter::Failed => {
                matches!(
                    row.run.status,
                    AgentRunStatus::Failed | AgentRunStatus::Interrupted
                )
            }
            MonitorFilter::Done => !row.run.status.is_active(),
        };
        if !filter_ok {
            return false;
        }
        if query.is_empty() {
            return true;
        }
        row.title.to_lowercase().contains(&query)
            || row.run.model_label.to_lowercase().contains(&query)
            || row
                .run
                .error
                .as_deref()
                .is_some_and(|e| e.to_lowercase().contains(&query))
            || row
                .pending_approval
                .as_ref()
                .is_some_and(|a| a.summary.to_lowercase().contains(&query))
            || row
                .last_answer
                .as_deref()
                .is_some_and(|a| a.to_lowercase().contains(&query))
    };

    let needs_rows: Vec<&AgentBoardRow> =
        board.needs_you.iter().filter(|r| matches_row(r)).collect();
    let run_rows: Vec<&AgentBoardRow> = board.running.iter().filter(|r| matches_row(r)).collect();
    let done_rows: Vec<&AgentBoardRow> = board.done.iter().filter(|r| matches_row(r)).collect();

    let mut list = v_col().w_full().gap(px(5.0)).p(px(10.0));
    if !needs_rows.is_empty() {
        list = list.child(group_label(
            ch.sidebar_board_needs_you(),
            needs_rows.len(),
            Some(StatusShape::Warn),
            cx,
        ));
        for row in &needs_rows {
            list = list.child(render_feed_card(view, row, board.limits.max_steps, cx));
        }
    }
    if !run_rows.is_empty() {
        list = list.child(group_label(
            ch.monitor_group_active(),
            run_rows.len(),
            Some(StatusShape::Run),
            cx,
        ));
        for row in &run_rows {
            list = list.child(render_feed_card(view, row, board.limits.max_steps, cx));
        }
    }
    if !done_rows.is_empty() {
        list = list.child(group_label(
            ch.monitor_group_recent(),
            done_rows.len(),
            None,
            cx,
        ));
        for row in &done_rows {
            list = list.child(render_feed_card(view, row, board.limits.max_steps, cx));
        }
    }

    if needs_rows.is_empty() && run_rows.is_empty() && done_rows.is_empty() {
        list = list.child(empty_inline(
            if board.needs_you.is_empty() && board.running.is_empty() && board.done.is_empty() {
                ch.sidebar_board_no_active_or_recent_runs()
            } else {
                ch.monitor_empty_title()
            },
            Some(SharedString::from(ch.monitor_empty_hint())),
            None,
            cx,
        ));
    }

    v_col()
        .w(px(336.0))
        .flex_shrink_0()
        .h_full()
        .bg(to_hsla(p.pane))
        .border_r_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            v_col()
                .w_full()
                .gap(px(6.0))
                .p(px(10.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(search_input(&view.monitor_search_input, cx))
                .child(filter_bar),
        )
        .child(
            div()
                .id("chat-monitor-feed-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(list),
        )
        .into_any_element()
}

fn filter_pill(
    id: &'static str,
    label: String,
    target: MonitorFilter,
    active: MonitorFilter,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    choice_chip(id, label, active == target, cx).on_click(cx.listener(move |this, _, _, cx| {
        this.monitor_filter = target;
        cx.notify();
    }))
}

fn render_feed_card(
    view: &ChatView,
    row: &AgentBoardRow,
    max_steps: u32,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let selected = view.selected_monitor_run_id.as_deref() == Some(row.run.run_id.as_str());
    let (shape, tone, status_lbl) = run_status_meta(
        row.run.status,
        row.wait,
        row.run.status_detail.as_deref(),
        cx,
    );
    let title = if row.title.trim().is_empty() {
        ch.header_new_chat().to_owned()
    } else {
        row.title.clone()
    };
    let subtitle = if row.waiting_for_slot {
        ch.monitor_waiting_slot().to_owned()
    } else if let Some(appr) = &row.pending_approval {
        appr.summary.clone()
    } else if let Some(err) = &row.run.error {
        err.clone()
    } else if let Some(step) = &row.run.last_step
        && row.run.status.is_active()
    {
        step.clone()
    } else if let Some(ans) = &row.last_answer {
        ans.clone()
    } else {
        view.format_raw_model_label(&row.run.model_label)
    };
    let elapsed_ms = if row.run.status.is_active() {
        let now = pingdeck_desktop::core::app::current_time_ms();
        (now - row.run.started_at_ms).max(row.awake_ms as i64) as u64
    } else {
        row.awake_ms
    };

    let bg = if selected {
        to_hsla(p.accent_soft)
    } else {
        to_hsla(p.surface)
    };
    let border = if selected {
        to_hsla(p.accent_line.composite_over(p.accent_soft))
    } else if tone == Tone::Danger {
        to_hsla(p.bad_line.composite_over(p.surface))
    } else if tone == Tone::Warning {
        to_hsla(p.warn_line.composite_over(p.surface))
    } else {
        to_hsla(p.line.composite_over(p.surface))
    };

    let rid_sel = row.run.run_id.clone();
    let rid_stop = row.run.run_id.clone();
    let is_active = row.run.status.is_active();
    let subs = row.subagent_counts;
    let sub_text = if subs.total > 0 {
        if subs.active + subs.waiting > 0 {
            Some((
                ch.monitor_sub_active_badge(subs.active + subs.waiting, subs.total),
                if subs.waiting > 0 {
                    Tone::Warning
                } else {
                    Tone::Accent
                },
            ))
        } else if subs.failed > 0 {
            Some((ch.monitor_sub_badge(subs.total), Tone::Danger))
        } else {
            Some((ch.monitor_sub_badge(subs.total), Tone::Neutral))
        }
    } else {
        None
    };

    div()
        .id(SharedString::from(format!("mon-row-{}", row.run.run_id)))
        .w_full()
        .p(px(9.0))
        .rounded(px(8.0))
        .bg(bg)
        .border_1()
        .border_color(border)
        .cursor_pointer()
        .hover(|s| {
            if selected {
                s
            } else {
                s.bg(to_hsla(p.hover.composite_over(p.surface)))
            }
        })
        .on_click(cx.listener(move |this, _, _, cx| {
            this.select_monitor_run(rid_sel.clone(), cx);
        }))
        .child(
            v_col()
                .w_full()
                .gap(px(4.0))
                .child(
                    h_row()
                        .w_full()
                        .gap(px(6.0))
                        .child(status_indicator(shape, cx))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(SharedString::from(title)),
                        )
                        .children(
                            row.review
                                .then(|| badge(ch.monitor_unread(), Tone::Accent, cx)),
                        )
                        .child(
                            div()
                                .flex_shrink_0()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.faint))
                                .child(SharedString::from(format_elapsed_short(elapsed_ms))),
                        ),
                )
                .child(
                    div()
                        .text_size(px(11.5))
                        .text_color(to_hsla(if tone == Tone::Danger {
                            p.bad
                        } else if tone == Tone::Warning {
                            p.warn
                        } else {
                            p.muted
                        }))
                        .truncate()
                        .child(SharedString::from(subtitle)),
                )
                .child(
                    h_row()
                        .w_full()
                        .justify_between()
                        .gap(px(6.0))
                        .child(
                            h_row()
                                .flex_1()
                                .min_w_0()
                                .gap(px(5.0))
                                .flex_wrap()
                                .child(badge(status_lbl, tone, cx))
                                .children(sub_text.map(|(txt, t)| badge(txt, t, cx)))
                                .children((row.plan_total > 0).then(|| {
                                    badge(
                                        ch.sidebar_board_plan(row.plan_done, row.plan_total),
                                        Tone::Neutral,
                                        cx,
                                    )
                                }))
                                .children((row.run.iterations > 0).then(|| {
                                    div()
                                        .text_size(px(10.5))
                                        .text_color(to_hsla(p.faint))
                                        .child(SharedString::from(
                                            ch.monitor_steps(row.run.iterations, max_steps),
                                        ))
                                })),
                        )
                        .children(is_active.then(|| {
                            btn_icon(
                                SharedString::from(format!("mon-stop-{}", row.run.run_id)),
                                UiIcon::Stop,
                            )
                            .tooltip(ch.card_stop())
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.stop_run_by_id(rid_stop.clone(), cx);
                                },
                            ))
                        })),
                ),
        )
        .into_any_element()
}

pub fn all_board_rows(board: &AgentBoard) -> impl Iterator<Item = &AgentBoardRow> {
    board
        .needs_you
        .iter()
        .chain(board.running.iter())
        .chain(board.done.iter())
}
