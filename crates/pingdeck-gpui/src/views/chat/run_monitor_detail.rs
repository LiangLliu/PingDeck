//! Right-hand detail pane of the full-canvas Run Monitor: selected run header, metadata ribbon,
//! blocker / failure diagnostic banners, and embedded [`render_topology`] waterfall timeline.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use pingdeck_desktop::core::agent::dto::{AgentBoard, BoardWait};
use pingdeck_desktop::core::agent::model::AgentRunStatus;

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, alert_banner, badge, btn_danger, btn_ghost,
    btn_icon, btn_primary, btn_secondary, empty_inline, empty_state, h_row, icon_xs, palette,
    status_indicator, to_hsla, v_col,
};

use super::ChatView;
use super::run_monitor::all_board_rows;
use super::run_status::{
    folder_basename, format_elapsed_short, format_tokens_compact, run_status_meta,
};
use super::run_topology::render_topology;
use super::types::ChatHandoff;

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

pub fn render_selected_detail(
    view: &ChatView,
    board: &AgentBoard,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();

    let Some(selected_id) = view.selected_monitor_run_id.as_deref() else {
        return div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .p(px(24.0))
            .child(empty_state(
                UiIcon::Activity,
                ch.monitor_title(),
                ch.monitor_select_hint(),
                cx,
            ))
            .into_any_element();
    };

    let Some(topo) = view
        .selected_topology
        .as_ref()
        .filter(|t| t.run.run_id == selected_id)
    else {
        return div()
            .flex_1()
            .min_w_0()
            .h_full()
            .flex()
            .items_center()
            .justify_center()
            .child(empty_inline(ch.monitor_loading_topology(), None, None, cx))
            .into_any_element();
    };

    let (shape, tone, status_lbl) = run_status_meta(
        topo.run.status,
        topo.wait,
        topo.run.status_detail.as_deref(),
        cx,
    );
    let title = if topo.title.trim().is_empty() {
        ch.header_new_chat().to_owned()
    } else {
        topo.title.clone()
    };
    let model_lbl = view.format_raw_model_label(&topo.run.model_label);
    let is_active = topo.run.status.is_active();
    let elapsed_ms = if is_active {
        let now = pingdeck_desktop::core::app::current_time_ms();
        (now - topo.run.started_at_ms).max(topo.awake_ms as i64) as u64
    } else {
        topo.awake_ms
    };
    let total_tok =
        topo.run.usage.input_tokens.unwrap_or(0) + topo.run.usage.output_tokens.unwrap_or(0);
    let sid_open = topo.run.session_id.clone();
    let sid_gate = topo.run.session_id.clone();
    let rid_stop = topo.run.run_id.clone();
    let rid_logs = topo.run.run_id.clone();
    let timed_out = all_board_rows(board)
        .find(|r| r.run.run_id == topo.run.run_id)
        .is_some_and(|r| r.approval_timed_out);

    let header = v_col()
        .w_full()
        .gap(px(6.0))
        .px(px(18.0))
        .py(px(12.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(10.0))
                .child(
                    h_row()
                        .flex_1()
                        .min_w_0()
                        .gap(px(8.0))
                        .child(status_indicator(shape, cx))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(SharedString::from(title)),
                        )
                        .child(badge(status_lbl, tone, cx))
                        .children(
                            timed_out
                                .then(|| badge(ch.run_approval_timed_out(), Tone::Warning, cx)),
                        ),
                )
                .child(
                    h_row()
                        .flex_shrink_0()
                        .gap(px(6.0))
                        .child(
                            btn_ghost("mon-det-logs", ch.monitor_view_logs())
                                .icon(icon_xs(UiIcon::Usage))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.pending_handoff
                                        .set(Some(ChatHandoff::OpenUsageRun(rid_logs.clone())));
                                    cx.notify();
                                })),
                        )
                        .children(is_active.then(|| {
                            btn_danger("mon-det-stop", ch.card_stop()).on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.stop_run_by_id(rid_stop.clone(), cx);
                                },
                            ))
                        }))
                        .child(
                            btn_primary("mon-det-open", ch.monitor_open_chat())
                                .icon(icon_xs(UiIcon::ExternalLink))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_session_from_monitor(sid_open.clone(), cx);
                                })),
                        ),
                ),
        )
        .child(
            h_row()
                .w_full()
                .gap(px(10.0))
                .flex_wrap()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(model_lbl))
                .children(topo.folder.as_deref().map(|f| {
                    h_row()
                        .gap(px(4.0))
                        .child(icon_xs(UiIcon::Folder))
                        .child(SharedString::from(folder_basename(f).to_owned()))
                }))
                .child(SharedString::from(format_elapsed_short(elapsed_ms)))
                .child(SharedString::from(
                    ch.monitor_steps(topo.run.iterations, board.limits.max_steps),
                ))
                .children((total_tok > 0).then(|| {
                    SharedString::from(format!("{} tok", format_tokens_compact(total_tok)))
                }))
                .children((topo.run.cost_micro_usd > 0).then(|| {
                    let cur = topo.cost_currency.as_deref().unwrap_or("USD");
                    SharedString::from(format!(
                        "{cur} {:.4}",
                        topo.run.cost_micro_usd as f64 / 1_000_000.0
                    ))
                }))
                .children((topo.pending_changes.files > 0).then(|| {
                    h_row()
                        .gap(px(4.0))
                        .child(SharedString::from(
                            ch.monitor_files_changed(topo.pending_changes.files),
                        ))
                        .child(
                            div()
                                .text_color(to_hsla(p.ok))
                                .child(format!("+{}", topo.pending_changes.added)),
                        )
                        .child(
                            div()
                                .text_color(to_hsla(p.bad))
                                .child(format!("-{}", topo.pending_changes.removed)),
                        )
                })),
        );

    let mut body = v_col().w_full().gap(px(12.0)).p(px(18.0));

    if let Some(appr) = &topo.pending_approval {
        body = body.child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(10.0))
                .px(px(12.0))
                .py(px(10.0))
                .rounded(px(8.0))
                .bg(to_hsla(p.warn_soft))
                .border_1()
                .border_color(to_hsla(p.warn_line.composite_over(p.warn_soft)))
                .child(
                    h_row()
                        .flex_1()
                        .min_w_0()
                        .gap(px(8.0))
                        .child(status_indicator(StatusShape::Warn, cx))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(to_hsla(p.warn))
                                .child(SharedString::from(appr.summary.clone())),
                        ),
                )
                .child(
                    btn_secondary("mon-det-handle", ch.monitor_handle_in_chat()).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.open_session_from_monitor(sid_gate.clone(), cx);
                        }),
                    ),
                ),
        );
    } else if topo.wait == Some(BoardWait::CostLimit)
        || topo.run.status == AgentRunStatus::WaitingApproval
    {
        body = body.child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(10.0))
                .px(px(12.0))
                .py(px(9.0))
                .rounded(px(8.0))
                .bg(to_hsla(p.warn_soft))
                .border_1()
                .border_color(to_hsla(p.warn_line.composite_over(p.warn_soft)))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.warn))
                        .child(status_lbl),
                )
                .child(
                    btn_secondary("mon-det-handle2", ch.monitor_handle_in_chat()).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.open_session_from_monitor(sid_gate.clone(), cx);
                        }),
                    ),
                ),
        );
    } else if let Some(err) = &topo.run.error
        && matches!(
            topo.run.status,
            AgentRunStatus::Failed | AgentRunStatus::Interrupted
        )
    {
        body = body.child(alert_banner(
            Tone::Danger,
            format!("{} · {err}", ch.monitor_error_heading()),
            None,
            cx,
        ));
    }

    body = body.child(render_topology(view, topo, false, cx));

    v_col()
        .flex_1()
        .min_w_0()
        .h_full()
        .child(header)
        .child(
            div()
                .id("chat-monitor-detail-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(body),
        )
        .into_any_element()
}
