//! Specialized Tier 2 payload renderers for Tool Step, Parallel Group, Planned Task nodes, and
//! shared collapsible text blocks inside `Node Inspector`.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::dto::{AgentRunTopology, TopologySubagent, TopologyToolStep};
use pingdeck_desktop::state::{GraphNodeMember, SelectedGraphTarget};

use crate::ui::{
    PayloadBlockOptions, Tone, UiIcon, badge, btn_danger, btn_ghost, btn_secondary, h_row, icon_xs,
    palette, payload_block, section_heading, status_indicator, to_hsla, v_col,
};

use super::modal::WorkflowMonitorModal;
use super::status::{compact_tool_row, format_elapsed_short, graph_status_visual, waterfall_bar};
use super::types::WorkflowTab;

pub(super) fn render_tool_payload(
    mut col: gpui_kit::Div,
    view: &WorkflowMonitorModal,
    topo: &AgentRunTopology,
    step: &TopologyToolStep,
    key_prefix: &str,
    cx: &mut Context<WorkflowMonitorModal>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();

    // Audit badges: exit code, risk, permission decision, sandbox tier.
    let mut audit_row = h_row().w_full().gap(px(4.0)).flex_wrap();
    let mut has_audit = false;
    if let Some(code) = step.exit_code {
        has_audit = true;
        audit_row = audit_row.child(badge(
            format!("exit {code}"),
            if code == 0 {
                Tone::Success
            } else {
                Tone::Danger
            },
            cx,
        ));
    }
    if let Some(risk) = &step.risk {
        has_audit = true;
        let rtone = if risk == "high" {
            Tone::Danger
        } else if risk == "medium" {
            Tone::Warning
        } else {
            Tone::Neutral
        };
        audit_row = audit_row.child(badge(risk.clone(), rtone, cx));
    }
    if let Some(dec) = &step.decision {
        has_audit = true;
        let lbl = match &step.decided_by {
            Some(by) if !by.is_empty() => format!("{dec} ({by})"),
            _ => dec.clone(),
        };
        audit_row = audit_row.child(badge(lbl, Tone::Neutral, cx));
    }
    if let Some(sb) = &step.sandbox {
        has_audit = true;
        audit_row = audit_row.child(badge(sb.clone(), Tone::Accent, cx));
    }
    if has_audit {
        col = col.child(
            v_col()
                .w_full()
                .gap(px(4.0))
                .child(section_heading(ch.graph_inspector_audit(), cx))
                .child(audit_row),
        );
    }

    // Linked file change shortcut (Edit / Write tools).
    if let Some(fc_id) = &step.file_change_id {
        let cid = fc_id.clone();
        let rid = topo.run.run_id.clone();
        let fpath = step
            .file_path
            .clone()
            .unwrap_or_else(|| step.summary.clone());
        col = col.child(
            v_col()
                .w_full()
                .gap(px(6.0))
                .p(px(9.0))
                .rounded(px(7.0))
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
                                .gap(px(5.0))
                                .child(icon_xs(UiIcon::FileDiff))
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .text_color(to_hsla(p.ink))
                                        .truncate()
                                        .child(SharedString::from(fpath)),
                                ),
                        )
                        .child(
                            h_row()
                                .gap(px(5.0))
                                .text_xs()
                                .child(
                                    div()
                                        .text_color(to_hsla(p.ok))
                                        .child(format!("+{}", step.file_added)),
                                )
                                .child(
                                    div()
                                        .text_color(to_hsla(p.bad))
                                        .child(format!("-{}", step.file_removed)),
                                ),
                        ),
                )
                .child(
                    btn_secondary(
                        SharedString::from(format!("wf-t-diff-{}", step.call_id)),
                        ch.graph_inspector_open_changes(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_change(cid.clone(), cx);
                        this.set_tab(rid.clone(), WorkflowTab::Changes, cx);
                    })),
                ),
        );
    }

    // Arguments / Command block with Copy.
    if let Some(args) = &step.args_preview {
        col = col.child(collapsible_text_block(
            view,
            format!("args-{key_prefix}"),
            ch.graph_inspector_args(),
            args,
            true,
            true,
            cx,
        ));
    } else if !step.summary.is_empty() {
        col = col.child(
            div()
                .w_full()
                .p(px(9.0))
                .rounded(px(7.0))
                .bg(to_hsla(p.sunken))
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .child(SharedString::from(step.summary.clone())),
        );
    }

    // Output preview (default expanded for exec/errors, collapsible for bulky read/search tools).
    let default_open_out = step.class == "exec" || step.status == "error";
    if let Some(out) = &step.output_preview {
        col = col.child(collapsible_text_block(
            view,
            format!("out-{key_prefix}"),
            ch.graph_inspector_output(),
            out,
            default_open_out,
            true,
            cx,
        ));
    }

    if step.truncated || step.output_bytes > 4_096 {
        let cid = step.call_id.clone();
        let name = step.name.clone();
        let fallback_out = step.output_preview.clone();
        let is_trunc = step.truncated;
        col = col.child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(6.0))
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(
                            ch.graph_inspector_truncated(step.output_bytes),
                        )),
                )
                .child(
                    btn_ghost(
                        SharedString::from(format!("wf-full-out-{}", step.call_id)),
                        ch.trace_view_full_output(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if is_trunc {
                            this.open_full_tool_output(cid.clone(), name.clone(), cx);
                        } else if let Some(txt) = &fallback_out {
                            this.open_text_dialog(name.clone(), txt.clone(), cx);
                        }
                    })),
                ),
        );
    }
    col
}

pub(super) fn render_group_payload(
    mut col: gpui_kit::Div,
    target: SelectedGraphTarget<'_>,
    subagents: &[TopologySubagent],
    tools: &[TopologyToolStep],
    cx: &mut Context<WorkflowMonitorModal>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();

    if let SelectedGraphTarget::Node(pnode) = target {
        let count = pnode.node.members.len();
        let max_ms = pnode
            .node
            .members
            .iter()
            .filter_map(|m| m.elapsed_ms)
            .max()
            .unwrap_or(1)
            .max(1);
        // Parallel branch timing comparison (mini Gantt bars).
        if count >= 2 {
            let mut gantt = v_col().w_full().gap(px(5.0));
            for m in &pnode.node.members {
                let (_, tone) = graph_status_visual(m.status);
                let frac = (m.elapsed_ms.unwrap_or(0) as f32 / max_ms as f32).clamp(0.06, 1.0);
                gantt = gantt.child(
                    h_row()
                        .w_full()
                        .justify_between()
                        .gap(px(6.0))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.ink_2))
                                .truncate()
                                .child(SharedString::from(m.title.clone())),
                        )
                        .child(waterfall_bar(0.0, frac, 92.0, tone, cx))
                        .child(
                            div()
                                .w(px(44.0))
                                .text_right()
                                .text_size(px(10.5))
                                .text_color(to_hsla(p.faint))
                                .child(SharedString::from(
                                    m.elapsed_ms
                                        .map_or_else(|| "—".to_owned(), format_elapsed_short),
                                )),
                        ),
                );
            }
            col = col
                .child(section_heading(ch.graph_inspector_group_timing(), cx))
                .child(
                    div()
                        .w_full()
                        .p(px(9.0))
                        .rounded(px(7.0))
                        .bg(to_hsla(p.surface))
                        .border_1()
                        .border_color(to_hsla(p.line.composite_over(p.surface)))
                        .child(gantt),
                );
        }

        col = col.child(section_heading(ch.graph_inspector_members(count), cx));
        let mut list = v_col().w_full().gap(px(4.0));
        for member in &pnode.node.members {
            list = list.child(clickable_member_row(member, cx));
        }
        return col.child(list);
    }

    let count = subagents.len() + tools.len();
    col = col.child(section_heading(ch.graph_inspector_members(count), cx));
    let mut fallback = v_col().w_full().gap(px(4.0));
    for step in tools {
        fallback = fallback.child(compact_tool_row(step, cx));
    }
    col.child(fallback)
}

pub(super) fn render_planned_task_payload(
    mut col: gpui_kit::Div,
    objective: &str,
    summary: Option<&str>,
    cx: &Context<WorkflowMonitorModal>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    col = col.child(
        div()
            .w_full()
            .p(px(9.0))
            .rounded(px(7.0))
            .bg(to_hsla(p.sunken))
            .text_xs()
            .text_color(to_hsla(p.ink_2))
            .child(SharedString::from(objective.to_owned())),
    );
    if let Some(sum) = summary {
        col = col
            .child(section_heading(ch.graph_node_summary_heading(), cx))
            .child(
                div()
                    .w_full()
                    .p(px(9.0))
                    .rounded(px(7.0))
                    .bg(to_hsla(p.sunken))
                    .text_xs()
                    .text_color(to_hsla(p.ink_2))
                    .child(SharedString::from(sum.to_owned())),
            );
    }
    col
}

fn clickable_member_row(
    member: &GraphNodeMember,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let (shape, _) = graph_status_visual(member.status);
    let mid = member.id.clone();
    let stop_id = member.stop_run_id.clone();

    h_row()
        .id(SharedString::from(format!("wf-imem-{}", member.id)))
        .w_full()
        .justify_between()
        .gap(px(6.0))
        .px(px(8.0))
        .py(px(6.0))
        .rounded(px(6.0))
        .bg(to_hsla(p.surface))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .cursor_pointer()
        .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.surface))))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.select_node(Some(mid.clone()), cx);
        }))
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
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(SharedString::from(member.title.clone())),
                ),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(5.0))
                .children(member.elapsed_ms.map(|ms| {
                    div()
                        .text_size(px(10.5))
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(format_elapsed_short(ms)))
                }))
                .children(stop_id.map(|rid| {
                    btn_danger(
                        SharedString::from(format!("wf-imem-stop-{}", member.id)),
                        ch.monitor_stop_subagent(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.stop_run_by_id(rid.clone(), cx);
                    }))
                })),
        )
        .into_any_element()
}

pub(super) fn collapsible_text_block(
    view: &WorkflowMonitorModal,
    key: String,
    heading: &'static str,
    content: &str,
    default_open: bool,
    with_copy: bool,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let toggled = view.expanded_sections.contains(&key);
    let open = if default_open { !toggled } else { toggled };
    let k_click = key.clone();

    let mut box_col = v_col().w_full().gap(px(4.0)).child(
        h_row()
            .id(SharedString::from(format!("wf-sec-{key}")))
            .w_full()
            .gap(px(4.0))
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_section(k_click.clone(), cx);
            }))
            .child(icon_xs(if open {
                UiIcon::ChevronDown
            } else {
                UiIcon::ChevronRight
            }))
            .child(section_heading(heading, cx)),
    );

    if open {
        box_col = box_col.child(payload_block(
            format!("wf-pb-{key}"),
            content,
            PayloadBlockOptions {
                max_height_px: Some(260.0),
                show_copy: with_copy,
                ..Default::default()
            },
            cx,
        ));
    }
    box_col.into_any_element()
}
