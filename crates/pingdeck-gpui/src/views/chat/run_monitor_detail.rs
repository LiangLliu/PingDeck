//! Right-hand detail pane of the full-canvas Run Monitor: selected run header, metadata ribbon,
//! adaptive 3-view switcher (`Steps` / `Graph` / `Timeline`), blocker / failure banners, active
//! execution canvas, and docked `Node Inspector` (`节点详情`).

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::dto::{AgentBoard, AgentRunTopology, BoardWait};
use pingdeck_desktop::core::agent::model::AgentRunStatus;
use pingdeck_desktop::state::{RunViewMode, is_graph_available};

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, alert_banner, badge, btn_danger, btn_ghost,
    btn_primary, btn_secondary, choice_chip, empty_inline, empty_state, h_row, icon_xs, palette,
    status_indicator, text_tooltip, to_hsla, v_col,
};

use super::ChatView;
use super::run_graph::{build_run_graph_layout, render_execution_graph};
use super::run_monitor::all_board_rows;
use super::run_node_inspector::render_node_inspector;
use super::run_status::{
    folder_basename, format_elapsed_short, format_tokens_compact, run_status_meta,
};
use super::run_steps::render_steps_view;
use super::run_topology::render_topology;
use super::types::ChatHandoff;

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

    let (active_mode, is_auto) = view.effective_monitor_view_mode(topo);
    let graph_ok = is_graph_available(topo);
    let view_bar = render_view_mode_bar(topo, active_mode, is_auto, graph_ok, cx);

    let alert_strip = render_run_alert_banner(topo, status_lbl, sid_gate, cx);

    let graph_layout = (active_mode == RunViewMode::Graph || view.selected_graph_node_id.is_some())
        .then(|| build_run_graph_layout(view, topo, cx));
    let inspector_pane = graph_layout
        .as_ref()
        .and_then(|layout| render_node_inspector(view, topo, layout, cx));

    let main_stage: AnyElement = match active_mode {
        RunViewMode::Graph => match &graph_layout {
            Some(layout) => render_execution_graph(view, layout, cx),
            None => render_steps_view(view, topo, cx),
        },
        RunViewMode::Steps => div()
            .id("chat-monitor-steps-scroll")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .overflow_y_scrollbar()
            .child(
                v_col()
                    .w_full()
                    .gap(px(12.0))
                    .p(px(18.0))
                    .child(render_steps_view(view, topo, cx)),
            )
            .into_any_element(),
        RunViewMode::Timeline => div()
            .id("chat-monitor-detail-scroll")
            .flex_1()
            .min_w_0()
            .min_h_0()
            .h_full()
            .overflow_y_scrollbar()
            .child(
                v_col()
                    .w_full()
                    .gap(px(12.0))
                    .p(px(18.0))
                    .child(render_topology(view, topo, false, cx)),
            )
            .into_any_element(),
    };

    v_col()
        .flex_1()
        .min_w_0()
        .h_full()
        .child(header)
        .child(view_bar)
        .children(alert_strip)
        .child(
            h_row()
                .flex_1()
                .min_h_0()
                .w_full()
                .items_stretch()
                .child(main_stage)
                .children(inspector_pane),
        )
        .into_any_element()
}

fn render_view_mode_bar(
    topo: &AgentRunTopology,
    active_mode: RunViewMode,
    is_auto: bool,
    graph_ok: bool,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let rid_steps = topo.run.run_id.clone();
    let rid_graph = topo.run.run_id.clone();
    let rid_time = topo.run.run_id.clone();

    let graph_btn: AnyElement = if graph_ok {
        choice_chip(
            "mon-view-graph",
            ch.monitor_view_graph(),
            active_mode == RunViewMode::Graph,
            cx,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.set_monitor_view_mode(rid_graph.clone(), RunViewMode::Graph, cx);
        }))
        .into_any_element()
    } else {
        div()
            .id("mon-view-graph-disabled")
            .px(px(9.0))
            .py(px(4.0))
            .rounded(px(6.0))
            .bg(to_hsla(p.sunken))
            .border_1()
            .border_color(to_hsla(p.line.composite_over(p.sunken)))
            .text_xs()
            .text_color(to_hsla(p.faint))
            .tooltip(text_tooltip(ch.monitor_view_graph_disabled()))
            .child(ch.monitor_view_graph())
            .into_any_element()
    };

    h_row()
        .w_full()
        .flex_shrink_0()
        .justify_between()
        .gap(px(8.0))
        .px(px(18.0))
        .py(px(7.0))
        .bg(to_hsla(p.surface))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            h_row()
                .gap(px(5.0))
                .child(
                    choice_chip(
                        "mon-view-steps",
                        ch.monitor_view_steps(),
                        active_mode == RunViewMode::Steps,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_monitor_view_mode(rid_steps.clone(), RunViewMode::Steps, cx);
                    })),
                )
                .child(graph_btn)
                .child(
                    choice_chip(
                        "mon-view-timeline",
                        ch.monitor_view_timeline(),
                        active_mode == RunViewMode::Timeline,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_monitor_view_mode(rid_time.clone(), RunViewMode::Timeline, cx);
                    })),
                )
                .children(is_auto.then(|| badge(ch.monitor_view_auto_badge(), Tone::Neutral, cx))),
        )
        .into_any_element()
}

fn render_run_alert_banner(
    topo: &AgentRunTopology,
    status_lbl: &'static str,
    sid_gate: String,
    cx: &mut Context<ChatView>,
) -> Option<AnyElement> {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();

    let inner: AnyElement = if let Some(appr) = &topo.pending_approval {
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
                btn_secondary("mon-det-handle", ch.monitor_handle_in_chat()).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.open_session_from_monitor(sid_gate.clone(), cx);
                    },
                )),
            )
            .into_any_element()
    } else if topo.wait == Some(BoardWait::CostLimit)
        || topo.run.status == AgentRunStatus::WaitingApproval
    {
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
            )
            .into_any_element()
    } else if let Some(err) = &topo.run.error
        && matches!(
            topo.run.status,
            AgentRunStatus::Failed | AgentRunStatus::Interrupted
        )
    {
        alert_banner(
            Tone::Danger,
            format!("{} · {err}", ch.monitor_error_heading()),
            None,
            cx,
        )
        .into_any_element()
    } else {
        return None;
    };

    Some(
        div()
            .w_full()
            .flex_shrink_0()
            .px(px(18.0))
            .py(px(8.0))
            .bg(to_hsla(p.surface))
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)))
            .child(inner)
            .into_any_element(),
    )
}
