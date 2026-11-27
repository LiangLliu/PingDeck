//! Hierarchical Wave-Tree + Millisecond Waterfall Timeline (`时间轴`) view inside
//! [`WorkflowMonitorModal`].
//!
//! Every wave row, sub-agent lane, and tool bar stays click-synchronized with the docked
//! `Node Inspector` so users inspect timings and drill into any node seamlessly.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::dto::{AgentRunTopology, ExecutionWave};
use pingdeck_desktop::core::agent::model::AgentRunStatus;

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, h_row, icon_xs, palette, section_heading, status_indicator,
    to_hsla, v_col,
};

use super::modal::WorkflowMonitorModal;
use super::status::{
    compute_bar_span, format_elapsed_short, format_model_short, run_status_meta, waterfall_bar,
};
use super::timeline_rows::{TimelineAxis, render_subagent_lane, render_tool_row};

pub fn render_timeline_view(
    view: &WorkflowMonitorModal,
    topo: &AgentRunTopology,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let now_ref = if topo.run.status.is_active() {
        pingdeck_desktop::core::app::current_time_ms().max(topo.snapshot_ms)
    } else {
        topo.run.finished_at_ms.unwrap_or(topo.snapshot_ms)
    };
    let run_start_ms = topo.run.started_at_ms;
    let run_total_ms = (now_ref - run_start_ms).max(topo.awake_ms as i64).max(1);
    let track_w = 180.0;
    let axis = TimelineAxis {
        run_start_ms,
        run_total_ms,
        snapshot_ms: now_ref,
        track_w,
    };

    let (root_shape, root_tone, root_label) = run_status_meta(
        topo.run.status,
        topo.wait,
        topo.run.status_detail.as_deref(),
        cx,
    );
    let root_elapsed = if topo.run.status.is_active() {
        (now_ref - run_start_ms).max(topo.awake_ms as i64) as u64
    } else {
        topo.awake_ms
    };
    let root_id = format!("main-start-{}", topo.run.run_id);
    let root_sel = view.selected_node_id.as_deref() == Some(root_id.as_str());

    let top_bar = h_row()
        .w_full()
        .justify_between()
        .gap(px(8.0))
        .child(section_heading(ch.monitor_topology_title(), cx))
        .child(
            h_row()
                .w(px(track_w + 56.0))
                .justify_between()
                .text_size(px(10.0))
                .text_color(to_hsla(p.faint))
                .child("0s")
                .child(SharedString::from(format_elapsed_short(
                    (run_total_ms as u64) / 2,
                )))
                .child(SharedString::from(format_elapsed_short(
                    run_total_ms as u64,
                ))),
        );

    let rid_click = root_id.clone();
    let root_row = h_row()
        .id(SharedString::from(format!(
            "wf-tl-root-{}",
            topo.run.run_id
        )))
        .w_full()
        .justify_between()
        .gap(px(8.0))
        .px(px(10.0))
        .py(px(8.0))
        .rounded(px(8.0))
        .bg(to_hsla(if root_sel { p.accent_soft } else { p.surface }))
        .border_1()
        .border_color(to_hsla(if root_sel {
            p.accent
        } else {
            p.line.composite_over(p.surface)
        }))
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| {
            this.select_node(Some(rid_click.clone()), cx);
        }))
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(7.0))
                .child(status_indicator(root_shape, cx))
                .child(
                    v_col()
                        .flex_1()
                        .min_w_0()
                        .gap(px(1.0))
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(to_hsla(p.ink))
                                        .child(ch.monitor_main_agent()),
                                )
                                .child(badge(root_label, root_tone, cx)),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(SharedString::from(format!(
                                    "{} · {}",
                                    format_model_short(&topo.run.model_label),
                                    ch.monitor_steps_short(topo.run.iterations)
                                ))),
                        ),
                ),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(8.0))
                .child(waterfall_bar(0.0, 1.0, axis.track_w, root_tone, cx))
                .child(
                    div()
                        .w(px(52.0))
                        .text_right()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(format_elapsed_short(root_elapsed))),
                ),
        );

    let mut container = v_col()
        .w_full()
        .gap(px(10.0))
        .child(top_bar)
        .child(root_row);

    if topo.waves.is_empty() {
        container = container.child(
            div()
                .px(px(12.0))
                .py(px(10.0))
                .rounded(px(8.0))
                .bg(to_hsla(p.sunken))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(ch.monitor_no_waves_yet()),
        );
    } else {
        let mut spine = v_col()
            .w_full()
            .pl(px(10.0))
            .ml(px(8.0))
            .border_l_2()
            .border_color(to_hsla(p.line_strong.composite_over(p.pane)))
            .gap(px(8.0));
        for wave in &topo.waves {
            spine = spine.child(render_wave(view, &topo.run.run_id, wave, &axis, cx));
        }
        container = container.child(spine);
    }

    if topo.queued_turns > 0 {
        container = container.child(
            h_row()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(7.0))
                .bg(to_hsla(p.sunken))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(icon_xs(UiIcon::ListTodo))
                .child(SharedString::from(
                    ch.monitor_queued_followups(topo.queued_turns),
                )),
        );
    }
    container.into_any_element()
}

fn render_wave(
    view: &WorkflowMonitorModal,
    parent_run_id: &str,
    wave: &ExecutionWave,
    axis: &TimelineAxis,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();

    match wave {
        ExecutionWave::ParallelSubagents {
            wave_index,
            subagents,
            ..
        } => {
            let any_active = subagents.iter().any(|s| s.run.status.is_active());
            let any_failed = subagents.iter().any(|s| {
                matches!(
                    s.run.status,
                    AgentRunStatus::Failed | AgentRunStatus::Interrupted
                )
            });
            let any_wait = subagents
                .iter()
                .any(|s| s.run.status == AgentRunStatus::WaitingApproval);
            let rail_color = if any_wait {
                to_hsla(p.warn)
            } else if any_failed {
                to_hsla(p.bad)
            } else if any_active {
                to_hsla(p.accent)
            } else {
                to_hsla(p.ok)
            };

            let mut lanes = v_col()
                .w_full()
                .pl(px(8.0))
                .border_l_2()
                .border_color(rail_color)
                .gap(px(6.0));
            for sub in subagents {
                lanes = lanes.child(render_subagent_lane(view, sub, axis, cx));
            }
            v_col()
                .w_full()
                .rounded(px(8.0))
                .bg(to_hsla(p.sunken))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.sunken)))
                .p(px(8.0))
                .gap(px(6.0))
                .child(
                    h_row()
                        .w_full()
                        .justify_between()
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(div().text_color(rail_color).child(icon_xs(UiIcon::Branch)))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(to_hsla(p.ink))
                                        .child(SharedString::from(ch.monitor_wave_parallel_subs(
                                            *wave_index,
                                            subagents.len(),
                                        ))),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.faint))
                                .child(if any_active {
                                    ch.monitor_join_waiting()
                                } else {
                                    ch.monitor_join_done()
                                }),
                        ),
                )
                .child(lanes)
                .into_any_element()
        }
        ExecutionWave::SingleSubagent {
            wave_index,
            subagent,
            ..
        } => v_col()
            .w_full()
            .gap(px(4.0))
            .child(
                div()
                    .text_size(px(11.0))
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(to_hsla(p.muted))
                    .child(SharedString::from(ch.monitor_wave_single_sub(*wave_index))),
            )
            .child(render_subagent_lane(view, subagent, axis, cx))
            .into_any_element(),
        ExecutionWave::ParallelTools {
            wave_index,
            started_ms,
            ended_ms,
            tools,
        } => {
            let key = format!("tl-pt-{parent_run_id}-{wave_index}");
            let open = view.expanded_rounds.contains(&key);
            let any_err = tools
                .iter()
                .any(|t| matches!(t.status.as_str(), "error" | "denied" | "timed_out"));
            let any_run = tools
                .iter()
                .any(|t| matches!(t.status.as_str(), "running" | "waiting_approval"));
            let (shape, tone) = if any_run {
                (StatusShape::Run, Tone::Accent)
            } else if any_err {
                (StatusShape::Bad, Tone::Danger)
            } else {
                (StatusShape::Ok, Tone::Success)
            };
            let (left, width) = compute_bar_span(
                axis.run_start_ms,
                axis.run_total_ms,
                *started_ms,
                *ended_ms,
                axis.snapshot_ms,
            );
            let dur = started_ms.map(|s| {
                ended_ms
                    .unwrap_or(axis.snapshot_ms)
                    .saturating_sub(s)
                    .max(0) as u64
            });
            let k_click = key.clone();
            let mut card_col = v_col()
                .w_full()
                .rounded(px(8.0))
                .bg(to_hsla(p.surface))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .overflow_hidden()
                .child(
                    h_row()
                        .id(SharedString::from(format!("wf-{key}")))
                        .w_full()
                        .justify_between()
                        .gap(px(8.0))
                        .px(px(9.0))
                        .py(px(6.0))
                        .cursor_pointer()
                        .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.surface))))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.toggle_round(k_click.clone(), cx);
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
                                        .truncate()
                                        .child(SharedString::from(ch.monitor_wave_parallel_tools(
                                            *wave_index,
                                            tools.len(),
                                        ))),
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
                                        .text_color(to_hsla(p.muted))
                                        .child(SharedString::from(
                                            dur.map_or_else(
                                                || "—".to_owned(),
                                                format_elapsed_short,
                                            ),
                                        )),
                                )
                                .child(icon_xs(if open {
                                    UiIcon::ChevronDown
                                } else {
                                    UiIcon::ChevronRight
                                })),
                        ),
                );
            if open {
                let mut inner = v_col()
                    .w_full()
                    .gap(px(4.0))
                    .p(px(8.0))
                    .border_t_1()
                    .border_color(to_hsla(p.line.composite_over(p.surface)));
                for step in tools {
                    inner = inner.child(render_tool_row(view, step, axis, cx));
                }
                card_col = card_col.child(inner);
            }
            card_col.into_any_element()
        }
        ExecutionWave::SerialTool { tool, .. } => render_tool_row(view, tool, axis, cx),
    }
}
