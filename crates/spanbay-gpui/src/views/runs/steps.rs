//! High-density `Steps` (`步骤`) view inside [`WorkflowMonitorModal`].
//!
//! Renders either a clean direct-answer Markdown summary card (0 tools) or a scannable linear /
//! round-grouped step list whose rows stay synchronized with the docked `Node Inspector`.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::agent::dto::{
    AgentRunTopology, ExecutionWave, TopologySubagent, TopologyToolStep,
};

use crate::ui::{
    Tone, UiIcon, badge, btn_danger, btn_ghost, btn_primary, card, h_row, icon_xs, markdown_view,
    palette, section_heading, status_indicator, to_hsla, v_col,
};

use super::modal::WorkflowMonitorModal;
use super::status::{
    format_elapsed_short, format_model_short, format_tokens_compact, run_status_meta,
    subagent_kind_label, subagent_role_label, tool_status_meta,
};

const ROUND_GROUP_THRESHOLD: usize = 8;
const STEPS_PER_ROUND: usize = 4;

enum FlatStep<'a> {
    Subagent(&'a TopologySubagent),
    Tool(&'a TopologyToolStep),
}

pub fn render_steps_view(
    view: &WorkflowMonitorModal,
    topo: &AgentRunTopology,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    if topo.waves.is_empty() {
        return render_direct_answer_summary(topo, cx);
    }

    let ch = crate::ui::tr(cx).chat();
    let mut flat: Vec<FlatStep<'_>> = Vec::new();
    for wave in &topo.waves {
        match wave {
            ExecutionWave::ParallelSubagents { subagents, .. } => {
                for sub in subagents {
                    flat.push(FlatStep::Subagent(sub));
                }
            }
            ExecutionWave::SingleSubagent { subagent, .. } => {
                flat.push(FlatStep::Subagent(subagent));
            }
            ExecutionWave::ParallelTools { tools, .. } => {
                for tool in tools {
                    flat.push(FlatStep::Tool(tool));
                }
            }
            ExecutionWave::SerialTool { tool, .. } => {
                flat.push(FlatStep::Tool(tool));
            }
        }
    }

    let mut container = v_col().w_full().gap(px(10.0)).child(section_heading(
        ch.monitor_steps_section_title(flat.len()),
        cx,
    ));

    if flat.len() >= ROUND_GROUP_THRESHOLD {
        for (round_idx, chunk) in flat.chunks(STEPS_PER_ROUND).enumerate() {
            let key = format!("round-{}-{round_idx}", topo.run.run_id);
            let is_last = (round_idx + 1) * STEPS_PER_ROUND >= flat.len();
            let collapsed = !is_last && !view.expanded_rounds.contains(&key);
            container = container.child(render_round_group(
                view,
                round_idx + 1,
                key,
                collapsed,
                chunk,
                cx,
            ));
        }
    } else {
        for (idx, item) in flat.iter().enumerate() {
            container = container.child(render_flat_step(view, idx + 1, item, cx));
        }
    }

    let answer_md = topo.full_answer.as_deref().or(topo.last_answer.as_deref());
    if let Some(ans) = answer_md {
        let p = palette(cx);
        container = container
            .child(section_heading(ch.monitor_answer_summary(), cx))
            .child(
                div()
                    .w_full()
                    .p(px(12.0))
                    .rounded(px(8.0))
                    .bg(to_hsla(p.sunken))
                    .border_1()
                    .border_color(to_hsla(p.line.composite_over(p.sunken)))
                    .child(markdown_view(
                        SharedString::from(format!("wf-steps-ans-{}", topo.run.run_id)),
                        ans.to_owned(),
                    )),
            );
    }

    container.into_any_element()
}

fn render_direct_answer_summary(
    topo: &AgentRunTopology,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let (shape, tone, status_lbl) = run_status_meta(
        topo.run.status,
        topo.wait,
        topo.run.status_detail.as_deref(),
        cx,
    );
    let elapsed_ms = if topo.run.status.is_active() {
        let now = spanbay_desktop::core::app::current_time_ms();
        (now - topo.run.started_at_ms).max(topo.awake_ms as i64) as u64
    } else {
        topo.awake_ms
    };
    let total_tok =
        topo.run.usage.input_tokens.unwrap_or(0) + topo.run.usage.output_tokens.unwrap_or(0);
    let sid_open = topo.run.session_id.clone();
    let rid_logs = topo.run.run_id.clone();
    let answer_text = topo
        .full_answer
        .clone()
        .or_else(|| topo.last_answer.clone())
        .unwrap_or_else(|| ch.monitor_no_waves_yet().to_owned());

    card(cx)
        .w_full()
        .p(px(16.0))
        .child(
            v_col()
                .w_full()
                .gap(px(10.0))
                .child(
                    h_row()
                        .w_full()
                        .justify_between()
                        .gap(px(8.0))
                        .child(
                            h_row()
                                .gap(px(7.0))
                                .child(status_indicator(shape, cx))
                                .child(
                                    div()
                                        .text_sm()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(to_hsla(p.ink))
                                        .child(ch.monitor_direct_answer_title()),
                                )
                                .child(badge(status_lbl, tone, cx)),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .child(SharedString::from(format_elapsed_short(elapsed_ms))),
                        ),
                )
                .child(
                    h_row()
                        .w_full()
                        .gap(px(8.0))
                        .flex_wrap()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(format_model_short(
                            &topo.run.model_label,
                        )))
                        .children((total_tok > 0).then(|| {
                            SharedString::from(format!("{} tok", format_tokens_compact(total_tok)))
                        }))
                        .children((topo.run.cost_micro_usd > 0).then(|| {
                            let cur = topo.cost_currency.as_deref().unwrap_or("USD");
                            SharedString::from(format!(
                                "{cur} {:.4}",
                                topo.run.cost_micro_usd as f64 / 1_000_000.0
                            ))
                        })),
                )
                .children(topo.user_prompt.as_ref().map(|pr| {
                    v_col()
                        .w_full()
                        .gap(px(4.0))
                        .child(section_heading(ch.graph_inspector_prompt(), cx))
                        .child(
                            div()
                                .w_full()
                                .p(px(10.0))
                                .rounded(px(7.0))
                                .bg(to_hsla(p.pane))
                                .text_xs()
                                .text_color(to_hsla(p.ink_2))
                                .child(SharedString::from(pr.clone())),
                        )
                }))
                .child(
                    v_col()
                        .w_full()
                        .gap(px(5.0))
                        .child(section_heading(ch.monitor_answer_summary(), cx))
                        .child(
                            div()
                                .w_full()
                                .p(px(12.0))
                                .rounded(px(7.0))
                                .bg(to_hsla(p.sunken))
                                .child(markdown_view(
                                    SharedString::from(format!("wf-direct-{}", topo.run.run_id)),
                                    answer_text,
                                )),
                        ),
                )
                .child(
                    h_row()
                        .w_full()
                        .justify_end()
                        .gap(px(6.0))
                        .child(
                            btn_ghost("wf-direct-logs", ch.monitor_view_logs())
                                .icon(icon_xs(UiIcon::Usage))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_usage_and_close(rid_logs.clone(), cx);
                                })),
                        )
                        .child(
                            btn_primary("wf-direct-open", ch.monitor_open_chat())
                                .icon(icon_xs(UiIcon::ExternalLink))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_chat_and_close(sid_open.clone(), cx);
                                })),
                        ),
                ),
        )
        .into_any_element()
}

fn render_round_group(
    view: &WorkflowMonitorModal,
    round_num: usize,
    key: String,
    collapsed: bool,
    steps: &[FlatStep<'_>],
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let k_click = key.clone();

    let mut box_col = v_col()
        .w_full()
        .rounded(px(8.0))
        .bg(to_hsla(p.pane))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .overflow_hidden()
        .child(
            h_row()
                .id(SharedString::from(format!("wf-rnd-{key}")))
                .w_full()
                .justify_between()
                .px(px(10.0))
                .py(px(7.0))
                .cursor_pointer()
                .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.pane))))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_round(k_click.clone(), cx);
                }))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(SharedString::from(
                            ch.monitor_round_label(round_num, steps.len()),
                        )),
                )
                .child(
                    div()
                        .text_color(to_hsla(p.faint))
                        .child(icon_xs(if collapsed {
                            UiIcon::ChevronRight
                        } else {
                            UiIcon::ChevronDown
                        })),
                ),
        );

    if !collapsed {
        let mut inner = v_col().w_full().gap(px(5.0)).p(px(8.0));
        let base_num = (round_num - 1) * STEPS_PER_ROUND;
        for (i, item) in steps.iter().enumerate() {
            inner = inner.child(render_flat_step(view, base_num + i + 1, item, cx));
        }
        box_col = box_col.child(inner);
    }
    box_col.into_any_element()
}

fn render_flat_step(
    view: &WorkflowMonitorModal,
    seq: usize,
    item: &FlatStep<'_>,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();

    match item {
        FlatStep::Subagent(sub) => {
            let node_id = format!("sub:{}", sub.run.run_id);
            let selected = view.selected_node_id.as_deref() == Some(node_id.as_str());
            let (shape, tone, status_lbl) =
                run_status_meta(sub.run.status, None, sub.run.status_detail.as_deref(), cx);
            let stop_id = sub.run.run_id.clone();
            let nid_click = node_id.clone();
            let bg = if selected {
                to_hsla(p.accent_soft)
            } else {
                to_hsla(p.surface)
            };
            let border = if selected {
                to_hsla(p.accent)
            } else {
                to_hsla(p.line.composite_over(p.surface))
            };
            let hover_border = to_hsla(p.accent_line.composite_over(p.surface));

            h_row()
                .id(SharedString::from(format!(
                    "wf-step-sub-{}",
                    sub.run.run_id
                )))
                .w_full()
                .justify_between()
                .gap(px(10.0))
                .px(px(11.0))
                .py(px(8.0))
                .rounded(px(8.0))
                .bg(bg)
                .border_1()
                .border_color(border)
                .cursor_pointer()
                .hover(move |s| {
                    if selected {
                        s
                    } else {
                        s.border_color(hover_border)
                    }
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_node(Some(nid_click.clone()), cx);
                }))
                .child(
                    h_row()
                        .flex_1()
                        .min_w_0()
                        .gap(px(8.0))
                        .child(
                            div()
                                .w(px(20.0))
                                .text_xs()
                                .text_color(to_hsla(p.faint))
                                .child(format!("{seq}.")),
                        )
                        .child(status_indicator(shape, cx))
                        .child(
                            v_col()
                                .flex_1()
                                .min_w_0()
                                .gap(px(2.0))
                                .child(
                                    h_row()
                                        .gap(px(5.0))
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
                                        .children(sub.run.role.map(|r| {
                                            badge(subagent_role_label(r, cx), Tone::Accent, cx)
                                        }))
                                        .child(badge(status_lbl, tone, cx)),
                                )
                                .children(sub.last_answer.as_ref().map(|a| {
                                    div()
                                        .text_xs()
                                        .text_color(to_hsla(p.muted))
                                        .truncate()
                                        .child(SharedString::from(a.clone()))
                                })),
                        ),
                )
                .child(
                    h_row()
                        .flex_shrink_0()
                        .gap(px(6.0))
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.faint))
                                .child(SharedString::from(format_elapsed_short(sub.awake_ms))),
                        )
                        .children(sub.run.status.is_active().then(|| {
                            btn_danger(
                                SharedString::from(format!("wf-st-stop-{}", sub.run.run_id)),
                                ch.monitor_stop_subagent(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    cx.stop_propagation();
                                    this.stop_run_by_id(stop_id.clone(), cx);
                                },
                            ))
                        })),
                )
                .into_any_element()
        }
        FlatStep::Tool(step) => {
            let node_id = format!("tool:{}", step.call_id);
            let selected = view.selected_node_id.as_deref() == Some(node_id.as_str());
            let (shape, _) = tool_status_meta(&step.status);
            let nid_click = node_id.clone();
            let bg = if selected {
                to_hsla(p.accent_soft)
            } else {
                to_hsla(p.surface)
            };
            let border = if selected {
                to_hsla(p.accent)
            } else {
                to_hsla(p.line.composite_over(p.surface))
            };
            let hover_border = to_hsla(p.accent_line.composite_over(p.surface));

            h_row()
                .id(SharedString::from(format!("wf-step-t-{}", step.call_id)))
                .w_full()
                .justify_between()
                .gap(px(10.0))
                .px(px(11.0))
                .py(px(7.0))
                .rounded(px(8.0))
                .bg(bg)
                .border_1()
                .border_color(border)
                .cursor_pointer()
                .hover(move |s| {
                    if selected {
                        s
                    } else {
                        s.border_color(hover_border)
                    }
                })
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_node(Some(nid_click.clone()), cx);
                }))
                .child(
                    h_row()
                        .flex_1()
                        .min_w_0()
                        .gap(px(8.0))
                        .child(
                            div()
                                .w(px(20.0))
                                .text_xs()
                                .text_color(to_hsla(p.faint))
                                .child(format!("{seq}.")),
                        )
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
                        )
                        .children((step.file_change_id.is_some()).then(|| {
                            badge(
                                format!("+{} -{}", step.file_added, step.file_removed),
                                Tone::Success,
                                cx,
                            )
                        })),
                )
                .children(step.duration_ms.map(|ms| {
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(format_elapsed_short(ms)))
                }))
                .into_any_element()
        }
    }
}
