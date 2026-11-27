//! Full-width global Run Monitor & cross-chat history list rendered inside `NavModule::Runs`.
//!
//! Deliberately uses 100% horizontal width for clear multi-chat concurrency telemetry, search,
//! filtering, background processes, and spacious run cards. Clicking any run row launches the
//! shared 90% viewport `WorkflowMonitorModal` on top of the entire window so the horizontal
//! Execution Graph never fights a 336px feed column for space.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use pingdeck_desktop::core::agent::dto::{AgentBoard, AgentBoardRow, SessionRunHistoryItem};
use pingdeck_desktop::core::agent::model::AgentRunStatus;
use pingdeck_desktop::core::agent::tools::process::ProcessState;

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_danger, btn_icon, card,
    choice_chip, empty_inline, h_row, icon_xs, palette, search_input, section_heading,
    status_indicator, to_hsla, v_col,
};

use super::feed_cards::{render_board_row_card, render_history_item_card};
use super::status::{count_24h_outcomes, format_cost_today, format_tokens_compact};
use super::types::{RunsFilter, RunsScope};
use super::{RunsView, all_board_rows};

pub fn render_runs_page(view: &RunsView, cx: &mut Context<RunsView>) -> AnyElement {
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

    let telemetry = render_global_telemetry(view, board, cx);
    let proc_drawer = view
        .show_processes
        .then(|| render_processes_drawer(view, cx));
    let toolbar = render_filter_toolbar(view, board, cx);
    let body = match view.scope {
        RunsScope::Recent24h => render_board_groups(view, board, cx),
        RunsScope::AllHistory => render_all_history_list(view, cx),
    };

    v_col()
        .size_full()
        .min_w_0()
        .bg(to_hsla(p.surface))
        .child(telemetry)
        .children(proc_drawer)
        .child(toolbar)
        .child(
            div()
                .id("runs-global-scroll")
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_y_scrollbar()
                .child(
                    v_col()
                        .w_full()
                        .gap(px(12.0))
                        .px(px(18.0))
                        .py(px(14.0))
                        .child(body),
                ),
        )
        .into_any_element()
}

fn render_global_telemetry(
    view: &RunsView,
    board: &AgentBoard,
    cx: &mut Context<RunsView>,
) -> AnyElement {
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
    let proc_count = view
        .active_process_count()
        .max(board.running_processes as usize);

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

    h_row()
        .w_full()
        .flex_shrink_0()
        .justify_between()
        .gap(px(12.0))
        .px(px(18.0))
        .py(px(10.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            h_row()
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
                                .child(SharedString::from(
                                    ch.monitor_slots(running_now, max_slots),
                                )),
                        ),
                )
                .children(
                    (queued_now > 0)
                        .then(|| badge(ch.monitor_queued_count(queued_now), Tone::Neutral, cx)),
                )
                .children((board.active_subagents > 0).then(|| {
                    badge(
                        ch.monitor_subagents_active(board.active_subagents),
                        Tone::Accent,
                        cx,
                    )
                }))
                .children(
                    (waiting_count > 0).then(|| {
                        badge(ch.monitor_attention_count(waiting_count), Tone::Warning, cx)
                    }),
                )
                .children((proc_count > 0 || !view.processes.is_empty()).then(|| {
                    choice_chip(
                        "runs-bg-procs",
                        ch.monitor_bg_processes_btn(proc_count),
                        view.show_processes,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.toggle_processes(cx)))
                }))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(ch.monitor_outcome_24h(ok_24h, fail_24h))),
                ),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(8.0))
                .children(
                    view.status_msg
                        .clone()
                        .map(|(tone, msg)| badge(msg, tone, cx)),
                )
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
                    btn_icon("runs-refresh", UiIcon::Refresh)
                        .tooltip(ch.monitor_refresh())
                        .on_click(cx.listener(|this, _, _, cx| this.reload_all(cx))),
                )
                .children((active_total > 0).then(|| {
                    btn_danger("runs-stop-all", ch.monitor_stop_all())
                        .on_click(cx.listener(|this, _, _, cx| this.request_stop_all_runs(cx)))
                })),
        )
        .into_any_element()
}

fn render_processes_drawer(view: &RunsView, cx: &mut Context<RunsView>) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let mut rows = h_row().w_full().gap(px(8.0)).flex_wrap();
    for proc_item in &view.processes {
        let pid_kill = proc_item.id.clone();
        let running = proc_item.state == ProcessState::Running;
        rows = rows.child(
            card(cx).px(px(10.0)).py(px(6.0)).child(
                h_row()
                    .gap(px(8.0))
                    .child(status_indicator(
                        if running {
                            StatusShape::Run
                        } else {
                            StatusShape::Idle
                        },
                        cx,
                    ))
                    .child(
                        v_col()
                            .max_w(px(320.0))
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(SharedString::from(proc_item.command.clone())),
                            )
                            .child(
                                div()
                                    .text_size(px(10.5))
                                    .text_color(to_hsla(p.muted))
                                    .child(format!(
                                        "{:?}{}",
                                        proc_item.state,
                                        proc_item
                                            .pid
                                            .map(|pid| format!(" · PID {pid}"))
                                            .unwrap_or_default()
                                    )),
                            ),
                    )
                    .children(running.then(|| {
                        btn_danger(
                            SharedString::from(format!("runs-pk-{}", proc_item.id)),
                            ch.inspector_kill(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.kill_process(pid_kill.clone(), cx);
                        }))
                    })),
            ),
        );
    }
    v_col()
        .w_full()
        .flex_shrink_0()
        .gap(px(6.0))
        .px(px(18.0))
        .py(px(10.0))
        .bg(to_hsla(p.sunken))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.sunken)))
        .child(section_heading(ch.inspector_background_processes(), cx))
        .child(rows)
        .into_any_element()
}

fn render_filter_toolbar(
    view: &RunsView,
    board: &AgentBoard,
    cx: &mut Context<RunsView>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
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

    h_row()
        .w_full()
        .flex_shrink_0()
        .justify_between()
        .gap(px(12.0))
        .flex_wrap()
        .px(px(18.0))
        .py(px(9.0))
        .bg(to_hsla(p.surface))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(10.0))
                .flex_wrap()
                .child(
                    div()
                        .w(px(280.0))
                        .child(search_input(&view.search_input, cx)),
                )
                .child(
                    h_row()
                        .gap(px(4.0))
                        .flex_wrap()
                        .child(filter_pill(
                            "rf-all",
                            ch.monitor_filter_all().to_owned(),
                            RunsFilter::All,
                            view.filter,
                            cx,
                        ))
                        .child(filter_pill(
                            "rf-wait",
                            ch.monitor_filter_waiting(waiting_n),
                            RunsFilter::Waiting,
                            view.filter,
                            cx,
                        ))
                        .child(filter_pill(
                            "rf-run",
                            ch.monitor_filter_running(running_n),
                            RunsFilter::Running,
                            view.filter,
                            cx,
                        ))
                        .child(filter_pill(
                            "rf-fail",
                            ch.monitor_filter_failed(failed_n),
                            RunsFilter::Failed,
                            view.filter,
                            cx,
                        ))
                        .child(filter_pill(
                            "rf-done",
                            ch.monitor_filter_done().to_owned(),
                            RunsFilter::Done,
                            view.filter,
                            cx,
                        )),
                ),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(4.0))
                .child(
                    choice_chip(
                        "runs-scope-24h",
                        ch.monitor_scope_24h(),
                        view.scope == RunsScope::Recent24h,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_scope(RunsScope::Recent24h, cx);
                    })),
                )
                .child(
                    choice_chip(
                        "runs-scope-all",
                        ch.monitor_scope_all(),
                        view.scope == RunsScope::AllHistory,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.set_scope(RunsScope::AllHistory, cx);
                    })),
                ),
        )
        .into_any_element()
}

fn filter_pill(
    id: &'static str,
    label: String,
    target: RunsFilter,
    active: RunsFilter,
    cx: &mut Context<RunsView>,
) -> impl IntoElement {
    choice_chip(id, label, active == target, cx)
        .on_click(cx.listener(move |this, _, _, cx| this.set_filter(target, cx)))
}

fn render_board_groups(
    view: &RunsView,
    board: &AgentBoard,
    cx: &mut Context<RunsView>,
) -> AnyElement {
    let ch = crate::ui::tr(cx).chat();
    let query = view.search_input.read(cx).value().trim().to_lowercase();
    let now_ms = pingdeck_desktop::core::app::current_time_ms();

    let matches_row = |row: &AgentBoardRow| -> bool {
        let status_ok = match view.filter {
            RunsFilter::All => true,
            RunsFilter::Waiting => {
                row.wait.is_some()
                    || row.approval_timed_out
                    || row.review
                    || row.run.status == AgentRunStatus::WaitingApproval
            }
            RunsFilter::Running => {
                matches!(
                    row.run.status,
                    AgentRunStatus::Queued | AgentRunStatus::Running
                )
            }
            RunsFilter::Failed => {
                matches!(
                    row.run.status,
                    AgentRunStatus::Failed | AgentRunStatus::Interrupted
                )
            }
            RunsFilter::Done => !row.run.status.is_active(),
        };
        if !status_ok {
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
    };

    let needs_rows: Vec<&AgentBoardRow> =
        board.needs_you.iter().filter(|r| matches_row(r)).collect();
    let run_rows: Vec<&AgentBoardRow> = board.running.iter().filter(|r| matches_row(r)).collect();
    let done_rows: Vec<&AgentBoardRow> = board.done.iter().filter(|r| matches_row(r)).collect();

    let has_visible = !needs_rows.is_empty() || !run_rows.is_empty() || !done_rows.is_empty();
    let mut list = v_col().w_full().gap(px(8.0));
    if !needs_rows.is_empty() {
        list = list.child(section_heading(
            format!("{} ({})", ch.sidebar_board_needs_you(), needs_rows.len()),
            cx,
        ));
        for row in needs_rows {
            list = list.child(render_board_row_card(
                row,
                board.limits.max_steps,
                now_ms,
                cx,
            ));
        }
    }
    if !run_rows.is_empty() {
        list = list.child(section_heading(
            format!("{} ({})", ch.monitor_group_active(), run_rows.len()),
            cx,
        ));
        for row in run_rows {
            list = list.child(render_board_row_card(
                row,
                board.limits.max_steps,
                now_ms,
                cx,
            ));
        }
    }
    if !done_rows.is_empty() {
        list = list.child(section_heading(
            format!("{} ({})", ch.monitor_group_recent(), done_rows.len()),
            cx,
        ));
        for row in done_rows {
            list = list.child(render_board_row_card(
                row,
                board.limits.max_steps,
                now_ms,
                cx,
            ));
        }
    }
    if board.needs_you.is_empty() && board.running.is_empty() && board.done.is_empty() {
        list = list.child(empty_inline(
            ch.sidebar_board_no_active_or_recent_runs(),
            Some(SharedString::from(ch.monitor_empty_hint())),
            None,
            cx,
        ));
    } else if !has_visible {
        list = list.child(empty_inline(
            ch.monitor_empty_title(),
            Some(SharedString::from(ch.monitor_empty_hint())),
            None,
            cx,
        ));
    }
    list.into_any_element()
}

fn render_all_history_list(view: &RunsView, cx: &mut Context<RunsView>) -> AnyElement {
    let ch = crate::ui::tr(cx).chat();
    let query = view.search_input.read(cx).value().trim().to_lowercase();
    let now_ms = pingdeck_desktop::core::app::current_time_ms();

    let filtered: Vec<&SessionRunHistoryItem> = view
        .all_history
        .iter()
        .filter(|item| {
            let status_ok = match view.filter {
                RunsFilter::All => true,
                RunsFilter::Waiting => item.run.status == AgentRunStatus::WaitingApproval,
                RunsFilter::Running => {
                    matches!(
                        item.run.status,
                        AgentRunStatus::Queued | AgentRunStatus::Running
                    )
                }
                RunsFilter::Failed => {
                    matches!(
                        item.run.status,
                        AgentRunStatus::Failed | AgentRunStatus::Interrupted
                    )
                }
                RunsFilter::Done => !item.run.status.is_active(),
            };
            if !status_ok {
                return false;
            }
            if query.is_empty() {
                return true;
            }
            item.title.to_lowercase().contains(&query)
                || item.run.model_label.to_lowercase().contains(&query)
                || item
                    .prompt_preview
                    .as_deref()
                    .is_some_and(|p| p.to_lowercase().contains(&query))
                || item
                    .run
                    .error
                    .as_deref()
                    .is_some_and(|e| e.to_lowercase().contains(&query))
        })
        .collect();

    if filtered.is_empty() {
        return empty_inline(
            ch.monitor_empty_title(),
            Some(SharedString::from(ch.monitor_empty_hint())),
            None,
            cx,
        )
        .into_any_element();
    }

    let mut list = v_col().w_full().gap(px(6.0));
    for item in filtered {
        list = list.child(render_history_item_card(item, now_ms, cx));
    }
    list.into_any_element()
}
