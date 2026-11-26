//! Sub-agent lane & inline live sub-agent group card renderers shared by [`super::run_topology`]
//! and [`super::trace`].

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::dto::{TopologySubagent, TopologyToolStep};
use pingdeck_desktop::core::agent::model::ToolCallStatus;
use pingdeck_desktop::state::LiveToolCall;

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, btn_danger, btn_ghost, h_row, icon_xs, palette,
    status_indicator, to_hsla, v_col,
};

use super::ChatView;
use super::run_status::{
    compute_bar_span, format_elapsed_short, run_status_meta, subagent_kind_label,
    subagent_role_label, tool_status_meta, waterfall_bar,
};
use super::run_topology::TimelineAxis;
use super::trace::summarize_tool_args;

pub(super) fn render_subagent_lane(
    view: &ChatView,
    sub: &TopologySubagent,
    axis: &TimelineAxis,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let key = format!("sub-{}", sub.run.run_id);
    let expanded = view.expanded_topology_nodes.contains(&key);
    let (shape, tone, status_lbl) =
        run_status_meta(sub.run.status, None, sub.run.status_detail.as_deref(), cx);
    let (left, width) = compute_bar_span(
        axis.run_start_ms,
        axis.run_total_ms,
        Some(sub.run.started_at_ms),
        sub.run.finished_at_ms,
        axis.snapshot_ms,
    );
    let elapsed_ms = if sub.run.status.is_active() {
        (axis.snapshot_ms - sub.run.started_at_ms).max(sub.awake_ms as i64) as u64
    } else {
        sub.awake_ms
    };
    let model_lbl = view.format_raw_model_label(&sub.run.model_label);
    let is_active = sub.run.status.is_active();
    let k_click = key.clone();
    let stop_id = sub.run.run_id.clone();

    let border_col = match tone {
        Tone::Danger => to_hsla(p.bad_line.composite_over(p.surface)),
        Tone::Warning => to_hsla(p.warn_line.composite_over(p.surface)),
        Tone::Accent => to_hsla(p.accent_line.composite_over(p.surface)),
        _ => to_hsla(p.line.composite_over(p.surface)),
    };

    let mut lane =
        v_col()
            .w_full()
            .rounded(px(7.0))
            .bg(to_hsla(p.surface))
            .border_1()
            .border_color(border_col)
            .overflow_hidden()
            .child(
                h_row()
                    .id(SharedString::from(format!("topo-sub-{}", sub.run.run_id)))
                    .w_full()
                    .justify_between()
                    .gap(px(8.0))
                    .px(px(9.0))
                    .py(px(7.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.surface))))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.toggle_topology_node(k_click.clone(), cx);
                    }))
                    .child(
                        v_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(3.0))
                            .child(
                                h_row()
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
                                    .child(badge(status_lbl, tone, cx)),
                            )
                            .child(
                                h_row()
                                    .gap(px(5.0))
                                    .flex_wrap()
                                    .child(badge(
                                        subagent_kind_label(&sub.subagent_kind, cx),
                                        Tone::Neutral,
                                        cx,
                                    ))
                                    .children(sub.run.role.map(|r| {
                                        badge(subagent_role_label(r, cx), Tone::Accent, cx)
                                    }))
                                    .children(
                                        sub.resumed.then(|| {
                                            badge(ch.monitor_resumed(), Tone::Neutral, cx)
                                        }),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(to_hsla(p.muted))
                                            .truncate()
                                            .child(SharedString::from(format!(
                                                "{model_lbl} · {}",
                                                ch.monitor_steps_short(sub.tool_calls.len())
                                            ))),
                                    ),
                            ),
                    )
                    .child(
                        h_row()
                            .flex_shrink_0()
                            .gap(px(6.0))
                            .child(waterfall_bar(left, width, axis.track_w, tone, cx))
                            .child(
                                div()
                                    .w(px(if axis.compact { 40.0 } else { 48.0 }))
                                    .text_right()
                                    .text_size(px(11.0))
                                    .text_color(to_hsla(p.faint))
                                    .child(SharedString::from(format_elapsed_short(elapsed_ms))),
                            )
                            .children(is_active.then(|| {
                                btn_danger(
                                    SharedString::from(format!("stop-sub-{}", sub.run.run_id)),
                                    ch.monitor_stop_subagent(),
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.stop_run_by_id(stop_id.clone(), cx);
                                    },
                                ))
                            }))
                            .child(div().text_color(to_hsla(p.faint)).child(icon_xs(
                                if expanded {
                                    UiIcon::ChevronDown
                                } else {
                                    UiIcon::ChevronRight
                                },
                            ))),
                    ),
            );

    if let Some(appr) = &sub.pending_approval {
        lane = lane.child(
            h_row()
                .w_full()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(5.0))
                .bg(to_hsla(p.warn_soft))
                .text_size(px(11.0))
                .text_color(to_hsla(p.warn))
                .child(icon_xs(UiIcon::Warning))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(appr.summary.clone())),
                ),
        );
    } else if let Some(err) = &sub.run.error {
        lane = lane.child(
            h_row()
                .w_full()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(5.0))
                .bg(to_hsla(p.bad_soft))
                .text_size(px(11.0))
                .text_color(to_hsla(p.bad))
                .child(icon_xs(UiIcon::CircleAlert))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .child(SharedString::from(err.clone())),
                ),
        );
    }

    if expanded {
        let mut details = v_col()
            .w_full()
            .px(px(10.0))
            .py(px(8.0))
            .gap(px(6.0))
            .bg(to_hsla(p.pane))
            .border_t_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)));

        if !sub.tool_calls.is_empty() {
            details = details.child(
                div()
                    .text_size(px(11.0))
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(to_hsla(p.muted))
                    .child(SharedString::from(
                        ch.monitor_child_tools_heading(sub.tool_calls.len()),
                    )),
            );
            for step in &sub.tool_calls {
                details = details.child(render_tool_step_row(step, axis, cx));
            }
        }
        if let Some(ans) = &sub.last_answer {
            details = details
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.muted))
                        .child(ch.monitor_child_report_heading()),
                )
                .child(
                    div()
                        .p(px(7.0))
                        .rounded(px(6.0))
                        .bg(to_hsla(p.sunken))
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .child(SharedString::from(ans.clone())),
                );
        }
        lane = lane.child(details);
    }

    lane.into_any_element()
}

pub(super) fn render_tool_step_row(
    step: &TopologyToolStep,
    axis: &TimelineAxis,
    cx: &gpui_kit::App,
) -> AnyElement {
    let p = palette(cx);
    let (shape, tone) = tool_status_meta(&step.status);
    let (left, width) = compute_bar_span(
        axis.run_start_ms,
        axis.run_total_ms,
        step.started_ms,
        step.ended_ms,
        axis.snapshot_ms,
    );
    let dur_ms = step.duration_ms.or_else(|| {
        step.started_ms.map(|s| {
            step.ended_ms
                .unwrap_or(axis.snapshot_ms)
                .saturating_sub(s)
                .max(0) as u64
        })
    });

    h_row()
        .w_full()
        .justify_between()
        .gap(px(6.0))
        .py(px(2.0))
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
                        .text_color(to_hsla(p.ink_2))
                        .child(SharedString::from(step.name.clone())),
                )
                .children((!step.summary.is_empty()).then(|| {
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px(11.0))
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
                .gap(px(6.0))
                .child(waterfall_bar(left, width, axis.track_w * 0.72, tone, cx))
                .children(dur_ms.map(|ms| {
                    div()
                        .w(px(42.0))
                        .text_right()
                        .text_size(px(10.5))
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(format_elapsed_short(ms)))
                })),
        )
        .into_any_element()
}

/// Renders live `task` sub-agent calls inside the active Chat thread as a parallel/single group card.
pub fn render_live_subagent_group(
    view: &ChatView,
    task_calls: &[&LiveToolCall],
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let parallel = task_calls.len() >= 2;
    let header_title = if parallel {
        ch.trace_parallel_subagents(task_calls.len())
    } else {
        ch.trace_subagent_call().to_owned()
    };
    let active_run_id = view.thread.active_run.as_ref().map(|r| r.run_id.clone());

    let mut group =
        v_col()
            .w_full()
            .rounded(px(10.0))
            .overflow_hidden()
            .bg(to_hsla(p.pane))
            .border_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .child(
                h_row()
                    .w_full()
                    .justify_between()
                    .px(px(10.0))
                    .py(px(7.0))
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.pane)))
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(div().text_color(to_hsla(p.accent)).child(icon_xs(
                                if parallel {
                                    UiIcon::Branch
                                } else {
                                    UiIcon::Bot
                                },
                            )))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(to_hsla(p.ink))
                                    .child(SharedString::from(header_title)),
                            ),
                    )
                    .child(
                        btn_ghost("trace-open-mon", ch.monitor_title())
                            .icon(icon_xs(UiIcon::Activity))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_run_monitor(active_run_id.clone(), cx);
                            })),
                    ),
            );

    for call in task_calls {
        let live_sub = view
            .thread
            .subagents
            .values()
            .find(|s| s.parent_call_id == call.call_id);
        let desc = live_sub
            .map(|s| s.description.clone())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| summarize_tool_args(&call.arguments));
        let (shape, tone, status_lbl) = if let Some(sub) = live_sub {
            run_status_meta(
                sub.summary.status,
                None,
                sub.summary.status_detail.as_deref(),
                cx,
            )
        } else {
            match call.status {
                None => (StatusShape::Run, Tone::Accent, ch.run_status_running()),
                Some(ToolCallStatus::Ok) => {
                    (StatusShape::Ok, Tone::Success, ch.run_status_completed())
                }
                Some(ToolCallStatus::Cancelled) => {
                    (StatusShape::Idle, Tone::Neutral, ch.run_status_cancelled())
                }
                Some(_) => (StatusShape::Bad, Tone::Danger, ch.run_status_failed()),
            }
        };
        let mut row = h_row()
            .w_full()
            .justify_between()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(7.0))
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .child(
                h_row()
                    .flex_1()
                    .min_w_0()
                    .gap(px(7.0))
                    .child(status_indicator(shape, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(SharedString::from(desc)),
                    )
                    .children(live_sub.map(|sub| {
                        badge(
                            subagent_kind_label(&sub.subagent_kind, cx),
                            Tone::Neutral,
                            cx,
                        )
                    }))
                    .children(
                        live_sub
                            .and_then(|sub| sub.summary.role)
                            .map(|r| badge(subagent_role_label(r, cx), Tone::Accent, cx)),
                    )
                    .children(live_sub.and_then(|sub| sub.current_tool.clone()).map(|t| {
                        div()
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.muted))
                            .child(SharedString::from(t))
                    })),
            )
            .child(
                h_row()
                    .flex_shrink_0()
                    .gap(px(6.0))
                    .child(badge(status_lbl, tone, cx))
                    .children(call.duration_ms.map(|ms| {
                        div()
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.faint))
                            .child(SharedString::from(format_elapsed_short(ms)))
                    }))
                    .children(live_sub.filter(|s| !s.finished).map(|sub| {
                        let stop_id = sub.child_run_id.clone();
                        btn_danger(
                            SharedString::from(format!("tr-stop-sub-{}", sub.child_run_id)),
                            ch.monitor_stop_subagent(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.stop_run_by_id(stop_id.clone(), cx);
                        }))
                    })),
            );
        if let Some(sub) = live_sub
            && let Some(err) = &sub.summary.error
        {
            row = v_col().w_full().child(row).child(
                div()
                    .px(px(12.0))
                    .pb(px(6.0))
                    .text_size(px(11.0))
                    .text_color(to_hsla(p.bad))
                    .child(SharedString::from(err.clone())),
            );
        }
        group = group.child(row);
    }
    group
}
