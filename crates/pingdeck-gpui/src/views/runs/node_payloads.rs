//! Archetype-specialized Tier 2 payload renderers for the 7 node types inside `Node Inspector`:
//! 1. Main Start / Planning (collapsible trigger prompt, reasoning, TaskGraph progress)
//! 2. Parallel Group (mini duration comparison Gantt bars + clickable members with per-member stop)
//! 3. Subagent (collapsible task brief + interactive per-tool accordion + Markdown final report)
//! 4. Tool Step (class-specialized Shell/Exec, File Edit Diff shortcut, Read/Search/HTTP/MCP)
//! 5. Planned Task (objective + checkpoint summary)
//! 6. Human Gate (handled prominently in `node_inspector.rs`)
//! 7. Main Summary (file changes shortcut + Markdown final answer + Copy)

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::dto::{AgentRunTopology, TopologySubagent, TopologyToolStep};
use pingdeck_desktop::services::clipboard;

use crate::ui::{
    Tone, UiIcon, badge, btn_ghost, h_row, icon_xs, markdown_view, palette, progress_bar,
    section_heading, status_indicator, to_hsla, v_col,
};

use super::modal::WorkflowMonitorModal;
pub(super) use super::node_payloads_tools::{
    collapsible_text_block, render_group_payload, render_planned_task_payload, render_tool_payload,
};
use super::status::{
    format_elapsed_short, format_model_short, format_tokens_compact, subagent_kind_label,
    subagent_role_label, tool_status_meta,
};
use super::types::WorkflowTab;

pub(super) fn render_main_payload(
    mut col: gpui_kit::Div,
    view: &WorkflowMonitorModal,
    topo: &AgentRunTopology,
    is_summary: bool,
    cx: &mut Context<WorkflowMonitorModal>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let total_tok =
        topo.run.usage.input_tokens.unwrap_or(0) + topo.run.usage.output_tokens.unwrap_or(0);

    col =
        col.child(
            h_row()
                .w_full()
                .gap(px(6.0))
                .flex_wrap()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(format_model_short(
                    &topo.run.model_label,
                )))
                .child(SharedString::from(
                    ch.monitor_steps_short(topo.run.iterations),
                ))
                .children((total_tok > 0).then(|| {
                    SharedString::from(format!("{} tok", format_tokens_compact(total_tok)))
                })),
        );

    if !is_summary && topo.plan_total > 0 {
        let pct = (topo.plan_done as f32 / topo.plan_total as f32) * 100.0;
        col = col.child(
            v_col()
                .w_full()
                .gap(px(4.0))
                .child(
                    h_row()
                        .justify_between()
                        .child(section_heading(ch.gates_plan_view_graph(), cx))
                        .child(badge(
                            ch.sidebar_board_plan(topo.plan_done, topo.plan_total),
                            Tone::Accent,
                            cx,
                        )),
                )
                .child(progress_bar("wf-insp-plan-bar", pct)),
        );
    }

    if !is_summary && let Some(prompt) = &topo.user_prompt {
        col = col.child(collapsible_text_block(
            view,
            format!("prompt-{}", topo.run.run_id),
            ch.graph_inspector_prompt(),
            prompt,
            true,
            false,
            cx,
        ));
    }

    if !is_summary && let Some(reasoning) = &topo.reasoning_preview {
        col = col.child(collapsible_text_block(
            view,
            format!("reason-{}", topo.run.run_id),
            ch.graph_inspector_reasoning(),
            reasoning,
            false,
            false,
            cx,
        ));
    }

    if is_summary && topo.pending_changes.files > 0 {
        let rid = topo.run.run_id.clone();
        col = col.child(
            h_row()
                .w_full()
                .justify_between()
                .p(px(9.0))
                .rounded(px(7.0))
                .bg(to_hsla(p.surface))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(
                    h_row()
                        .gap(px(6.0))
                        .text_xs()
                        .child(icon_xs(UiIcon::FileDiff))
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
                        ),
                )
                .child(
                    btn_ghost("wf-sum-changes", ch.graph_inspector_open_changes()).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.set_tab(rid.clone(), WorkflowTab::Changes, cx);
                        }),
                    ),
                ),
        );
    }

    let answer_md = topo.full_answer.as_deref().or(topo.last_answer.as_deref());
    if is_summary && let Some(ans) = answer_md {
        let copy_txt = ans.to_owned();
        col = col
            .child(
                h_row()
                    .w_full()
                    .justify_between()
                    .child(section_heading(ch.monitor_answer_summary(), cx))
                    .child(
                        btn_ghost("wf-sum-copy", ch.thread_copy_2())
                            .icon(icon_xs(UiIcon::Copy))
                            .on_click(cx.listener(move |_, _, _, _| {
                                let _ = clipboard::set_text(&copy_txt);
                            })),
                    ),
            )
            .child(
                div()
                    .w_full()
                    .p(px(10.0))
                    .rounded(px(7.0))
                    .bg(to_hsla(p.sunken))
                    .child(markdown_view(
                        SharedString::from(format!("wf-insp-sum-{}", topo.run.run_id)),
                        ans.to_owned(),
                    )),
            );
    }
    col
}

pub(super) fn render_subagent_payload(
    mut col: gpui_kit::Div,
    view: &WorkflowMonitorModal,
    topo: &AgentRunTopology,
    sub: &TopologySubagent,
    cx: &mut Context<WorkflowMonitorModal>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let tok = sub.run.usage.input_tokens.unwrap_or(0) + sub.run.usage.output_tokens.unwrap_or(0);

    let mut meta = h_row().w_full().gap(px(5.0)).flex_wrap().child(badge(
        subagent_kind_label(&sub.subagent_kind, cx),
        Tone::Neutral,
        cx,
    ));
    if let Some(role) = sub.run.role {
        meta = meta.child(badge(subagent_role_label(role, cx), Tone::Accent, cx));
    }
    meta = meta.child(
        div()
            .text_xs()
            .text_color(to_hsla(p.muted))
            .child(SharedString::from(format_model_short(&sub.run.model_label))),
    );
    if tok > 0 {
        meta = meta.child(
            div()
                .text_xs()
                .text_color(to_hsla(p.faint))
                .child(SharedString::from(format!(
                    "{} tok",
                    format_tokens_compact(tok)
                ))),
        );
    }
    col = col.child(meta);

    if let Some(brief) = &sub.brief {
        col = col.child(collapsible_text_block(
            view,
            format!("brief-{}", sub.run.run_id),
            ch.graph_inspector_brief(),
            brief,
            false,
            false,
            cx,
        ));
    }

    if !sub.tool_calls.is_empty() {
        col = col.child(section_heading(
            ch.monitor_child_tools_heading(sub.tool_calls.len()),
            cx,
        ));
        let mut accordion = v_col().w_full().gap(px(5.0));
        for step in &sub.tool_calls {
            accordion = accordion.child(render_subagent_tool_accordion(view, topo, step, cx));
        }
        col = col.child(accordion);
    }

    let report_md = sub.full_report.as_deref().or(sub.last_answer.as_deref());
    if let Some(rep) = report_md {
        col = col
            .child(section_heading(ch.monitor_child_report_heading(), cx))
            .child(
                div()
                    .w_full()
                    .p(px(10.0))
                    .rounded(px(7.0))
                    .bg(to_hsla(p.sunken))
                    .child(markdown_view(
                        SharedString::from(format!("wf-sub-rep-{}", sub.run.run_id)),
                        rep.to_owned(),
                    )),
            );
    }
    col
}

fn render_subagent_tool_accordion(
    view: &WorkflowMonitorModal,
    topo: &AgentRunTopology,
    step: &TopologyToolStep,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let key = format!("subtool-{}", step.call_id);
    let open = view.expanded_sections.contains(&key);
    let (shape, _) = tool_status_meta(&step.status);
    let k_toggle = key.clone();

    let mut card_col = v_col()
        .w_full()
        .rounded(px(7.0))
        .bg(to_hsla(p.surface))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .overflow_hidden()
        .child(
            h_row()
                .id(SharedString::from(format!("wf-acc-{}", step.call_id)))
                .w_full()
                .justify_between()
                .gap(px(6.0))
                .px(px(8.0))
                .py(px(6.0))
                .cursor_pointer()
                .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.surface))))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.toggle_section(k_toggle.clone(), cx);
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
                        .gap(px(4.0))
                        .children(step.duration_ms.map(|ms| {
                            div()
                                .text_size(px(10.5))
                                .text_color(to_hsla(p.faint))
                                .child(SharedString::from(format_elapsed_short(ms)))
                        }))
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
            .gap(px(6.0))
            .p(px(8.0))
            .border_t_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)));
        inner = render_tool_payload(inner, view, topo, step, &key, cx);
        card_col = card_col.child(inner);
    }
    card_col.into_any_element()
}
