//! Subagent lane and tool-step waterfall rows rendered inside the Timeline (`时间轴`) tab of
//! [`WorkflowMonitorModal`].

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::agent::dto::{TopologySubagent, TopologyToolStep};

use crate::ui::{Tone, badge, btn_danger, h_row, palette, status_indicator, to_hsla, v_col};

use super::modal::WorkflowMonitorModal;
use super::status::{
    compute_bar_span, format_elapsed_short, run_status_meta, subagent_kind_label,
    subagent_role_label, tool_status_meta, waterfall_bar,
};

pub(super) struct TimelineAxis {
    pub run_start_ms: i64,
    pub run_total_ms: i64,
    pub snapshot_ms: i64,
    pub track_w: f32,
}

pub(super) fn render_subagent_lane(
    view: &WorkflowMonitorModal,
    sub: &TopologySubagent,
    axis: &TimelineAxis,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let node_id = format!("sub:{}", sub.run.run_id);
    let selected = view.selected_node_id.as_deref() == Some(node_id.as_str());
    let (shape, tone, status_lbl) =
        run_status_meta(sub.run.status, None, sub.run.status_detail.as_deref(), cx);
    let (left, width) = compute_bar_span(
        axis.run_start_ms,
        axis.run_total_ms,
        Some(sub.run.started_at_ms),
        sub.run.finished_at_ms,
        axis.snapshot_ms,
    );
    let nid_click = node_id.clone();
    let stop_id = sub.run.run_id.clone();

    let mut box_col = v_col()
        .id(SharedString::from(format!("wf-tl-sub-{}", sub.run.run_id)))
        .w_full()
        .rounded(px(8.0))
        .bg(to_hsla(if selected { p.accent_soft } else { p.surface }))
        .border_1()
        .border_color(to_hsla(if selected {
            p.accent
        } else {
            p.line.composite_over(p.surface)
        }))
        .p(px(8.0))
        .gap(px(5.0))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            this.select_node(Some(nid_click.clone()), cx);
        }))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(8.0))
                .child(
                    h_row()
                        .flex_1()
                        .min_w_0()
                        .gap(px(6.0))
                        .child(status_indicator(shape, cx))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(SharedString::from(sub.description.clone())),
                        )
                        .child(badge(
                            subagent_kind_label(&sub.subagent_kind, cx),
                            Tone::Neutral,
                            cx,
                        ))
                        .children(
                            sub.run
                                .role
                                .map(|r| badge(subagent_role_label(r, cx), Tone::Accent, cx)),
                        )
                        .child(badge(status_lbl, tone, cx)),
                )
                .child(
                    h_row()
                        .flex_shrink_0()
                        .gap(px(8.0))
                        .child(waterfall_bar(left, width, axis.track_w, tone, cx))
                        .child(
                            div()
                                .w(px(52.0))
                                .text_right()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.muted))
                                .child(SharedString::from(format_elapsed_short(sub.awake_ms))),
                        )
                        .children(sub.run.status.is_active().then(|| {
                            btn_danger(
                                SharedString::from(format!("wf-tl-stop-{}", sub.run.run_id)),
                                ch.monitor_stop_subagent(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.stop_run_by_id(stop_id.clone(), cx);
                                },
                            ))
                        })),
                ),
        );

    if !sub.tool_calls.is_empty() {
        let mut child_rows = v_col().w_full().pl(px(14.0)).gap(px(3.0));
        for step in &sub.tool_calls {
            child_rows = child_rows.child(render_tool_row(view, step, axis, cx));
        }
        box_col = box_col.child(child_rows);
    }
    box_col.into_any_element()
}

pub(super) fn render_tool_row(
    view: &WorkflowMonitorModal,
    step: &TopologyToolStep,
    axis: &TimelineAxis,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let node_id = format!("tool:{}", step.call_id);
    let selected = view.selected_node_id.as_deref() == Some(node_id.as_str());
    let (shape, tone) = tool_status_meta(&step.status);
    let (left, width) = compute_bar_span(
        axis.run_start_ms,
        axis.run_total_ms,
        step.started_ms,
        step.ended_ms,
        axis.snapshot_ms,
    );
    let nid_click = node_id.clone();

    h_row()
        .id(SharedString::from(format!("wf-tl-t-{}", step.call_id)))
        .w_full()
        .justify_between()
        .gap(px(8.0))
        .px(px(8.0))
        .py(px(5.0))
        .rounded(px(6.0))
        .bg(to_hsla(if selected { p.accent_soft } else { p.surface }))
        .border_1()
        .border_color(to_hsla(if selected {
            p.accent
        } else {
            p.line.composite_over(p.surface)
        }))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            cx.stop_propagation();
            this.select_node(Some(nid_click.clone()), cx);
        }))
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
                .children((!step.summary.is_empty()).then(|| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .truncate()
                        .child(SharedString::from(step.summary.clone()))
                }))
                .children(
                    step.exit_code
                        .filter(|c| *c != 0)
                        .map(|c| badge(format!("exit {c}"), Tone::Danger, cx)),
                ),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(8.0))
                .child(waterfall_bar(left, width, axis.track_w, tone, cx))
                .child(
                    div()
                        .w(px(52.0))
                        .text_right()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(
                            step.duration_ms
                                .map_or_else(|| "—".to_owned(), format_elapsed_short),
                        )),
                ),
        )
        .into_any_element()
}
