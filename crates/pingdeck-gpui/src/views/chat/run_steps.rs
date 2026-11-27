//! High-density `Steps` (`步骤`) view for the Run Monitor detail canvas.
//!
//! Optimized for the two cases where a horizontal Execution Graph adds whitespace rather than
//! signal:
//! 1. Zero-tool direct answers: renders a clean answer + telemetry summary card instead of an
//!    empty graph or placeholder box.
//! 2. Short linear tool runs (or detailed payload inspection): renders full-width step cards with
//!    clear parameters, exit codes, and durations, automatically grouping long iterative loops
//!    into collapsible rounds.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::dto::{
    AgentRunTopology, ExecutionWave, TopologySubagent, TopologyToolStep,
};

use crate::ui::{
    Tone, UiIcon, badge, btn_danger, btn_ghost, btn_primary, card, h_row, icon_xs, palette,
    section_heading, status_indicator, to_hsla, v_col,
};

use super::ChatView;
use super::run_status::{
    format_elapsed_short, format_tokens_compact, run_status_meta, subagent_kind_label,
    subagent_role_label, tool_status_meta,
};
use super::types::ChatHandoff;

/// Threshold above which linear steps are grouped into collapsible round sections.
const ROUND_GROUP_THRESHOLD: usize = 8;
const STEPS_PER_ROUND: usize = 4;

enum FlatStep<'a> {
    Subagent(&'a TopologySubagent),
    Tool(&'a TopologyToolStep),
}

/// Renders the `Steps` (`步骤`) view for `topo`.
pub fn render_steps_view(
    view: &ChatView,
    topo: &AgentRunTopology,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    if topo.waves.is_empty() {
        return render_direct_answer_summary(view, topo, cx);
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
            // Keep the latest round expanded by default; older rounds expand on click.
            let collapsed = !is_last && !view.expanded_topology_nodes.contains(&key);
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

    if let Some(ans) = &topo.last_answer {
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
                    .text_xs()
                    .text_color(to_hsla(p.ink_2))
                    .child(SharedString::from(ans.clone())),
            );
    }

    container.into_any_element()
}

fn render_direct_answer_summary(
    view: &ChatView,
    topo: &AgentRunTopology,
    cx: &mut Context<ChatView>,
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
        let now = pingdeck_desktop::core::app::current_time_ms();
        (now - topo.run.started_at_ms).max(topo.awake_ms as i64) as u64
    } else {
        topo.awake_ms
    };
    let total_tok =
        topo.run.usage.input_tokens.unwrap_or(0) + topo.run.usage.output_tokens.unwrap_or(0);
    let model_lbl = view.format_raw_model_label(&topo.run.model_label);
    let sid_open = topo.run.session_id.clone();
    let rid_logs = topo.run.run_id.clone();

    let answer_text = topo
        .last_answer
        .clone()
        .unwrap_or_else(|| ch.monitor_no_waves_yet().to_owned());

    card(cx)
        .w_full()
        .p(px(14.0))
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
                        .child(SharedString::from(model_lbl))
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
                .child(
                    v_col()
                        .w_full()
                        .gap(px(5.0))
                        .child(section_heading(ch.monitor_answer_summary(), cx))
                        .child(
                            div()
                                .w_full()
                                .p(px(11.0))
                                .rounded(px(7.0))
                                .bg(to_hsla(p.sunken))
                                .text_xs()
                                .text_color(to_hsla(p.ink_2))
                                .child(SharedString::from(answer_text)),
                        ),
                )
                .child(
                    h_row()
                        .w_full()
                        .justify_end()
                        .gap(px(6.0))
                        .child(
                            btn_ghost("steps-direct-logs", ch.monitor_view_logs())
                                .icon(icon_xs(UiIcon::Usage))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.pending_handoff
                                        .set(Some(ChatHandoff::OpenUsageRun(rid_logs.clone())));
                                    cx.notify();
                                })),
                        )
                        .child(
                            btn_primary("steps-direct-open", ch.monitor_open_chat())
                                .icon(icon_xs(UiIcon::ExternalLink))
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.open_session_from_monitor(sid_open.clone(), cx);
                                })),
                        ),
                ),
        )
        .into_any_element()
}

fn render_round_group(
    view: &ChatView,
    round_num: usize,
    key: String,
    collapsed: bool,
    steps: &[FlatStep<'_>],
    cx: &mut Context<ChatView>,
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
                .id(SharedString::from(format!("steps-rnd-{key}")))
                .w_full()
                .justify_between()
                .px(px(10.0))
                .py(px(7.0))
                .cursor_pointer()
                .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.pane))))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_topology_node(k_click.clone(), cx);
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
    view: &ChatView,
    seq: usize,
    item: &FlatStep<'_>,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();

    match item {
        FlatStep::Subagent(sub) => {
            let node_id = format!("sub:{}", sub.run.run_id);
            let selected = view.selected_graph_node_id.as_deref() == Some(node_id.as_str());
            let (shape, tone, status_lbl) =
                run_status_meta(sub.run.status, None, sub.run.status_detail.as_deref(), cx);
            let stop_id = sub.run.run_id.clone();
            let is_active = sub.run.status.is_active();
            let nid = node_id.clone();

            h_row()
                .id(SharedString::from(format!(
                    "step-sub-{seq}-{}",
                    sub.run.run_id
                )))
                .w_full()
                .justify_between()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(8.0))
                .rounded(px(7.0))
                .bg(to_hsla(if selected { p.accent_soft } else { p.surface }))
                .border_1()
                .border_color(to_hsla(if selected {
                    p.accent
                } else {
                    p.line.composite_over(p.surface)
                }))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_graph_node(Some(nid.clone()), cx);
                }))
                .child(
                    h_row()
                        .flex_1()
                        .min_w_0()
                        .gap(px(7.0))
                        .child(
                            div()
                                .text_size(px(10.5))
                                .text_color(to_hsla(p.faint))
                                .child(format!("{seq}.")),
                        )
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
                        ),
                )
                .child(
                    h_row()
                        .flex_shrink_0()
                        .gap(px(6.0))
                        .child(badge(status_lbl, tone, cx))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.faint))
                                .child(SharedString::from(format_elapsed_short(sub.awake_ms))),
                        )
                        .children(is_active.then(|| {
                            btn_danger(
                                SharedString::from(format!("step-stop-{}", sub.run.run_id)),
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
        FlatStep::Tool(tool) => {
            let node_id = format!("tool:{}", tool.call_id);
            let selected = view.selected_graph_node_id.as_deref() == Some(node_id.as_str());
            let (shape, _) = tool_status_meta(&tool.status);
            let nid = node_id.clone();

            h_row()
                .id(SharedString::from(format!(
                    "step-tool-{seq}-{}",
                    tool.call_id
                )))
                .w_full()
                .justify_between()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(7.0))
                .rounded(px(7.0))
                .bg(to_hsla(if selected { p.accent_soft } else { p.surface }))
                .border_1()
                .border_color(to_hsla(if selected {
                    p.accent
                } else {
                    p.line.composite_over(p.surface)
                }))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_graph_node(Some(nid.clone()), cx);
                }))
                .child(
                    h_row()
                        .flex_1()
                        .min_w_0()
                        .gap(px(7.0))
                        .child(
                            div()
                                .text_size(px(10.5))
                                .text_color(to_hsla(p.faint))
                                .child(format!("{seq}.")),
                        )
                        .child(status_indicator(shape, cx))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .child(SharedString::from(tool.name.clone())),
                        )
                        .children((!tool.summary.is_empty()).then(|| {
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(SharedString::from(tool.summary.clone()))
                        }))
                        .children(
                            tool.exit_code
                                .filter(|c| *c != 0)
                                .map(|c| badge(format!("exit {c}"), Tone::Danger, cx)),
                        ),
                )
                .children(tool.duration_ms.map(|ms| {
                    div()
                        .flex_shrink_0()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(format_elapsed_short(ms)))
                }))
                .into_any_element()
        }
    }
}
