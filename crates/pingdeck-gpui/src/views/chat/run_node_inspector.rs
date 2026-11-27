//! Docked right-hand `Node Inspector` (`节点详情`) panel for the Run Monitor.
//!
//! Opens whenever a user clicks any node or expanded Parallel Group member in the Execution Graph
//! (`执行图`) or selects a step in `Steps`. Displays status, stage, upstream dependencies, tool
//! execution trace, sub-agent report, pending approval link (jumping to the unified Chat gate per
//! `docs/decisions.md §41`), error details, and targeted stop / call-log actions.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::dto::{AgentRunTopology, TopologySubagent, TopologyToolStep};
use pingdeck_desktop::core::agent::model::NodeRecoveryAction;
use pingdeck_desktop::state::{
    ExecutionGraphLayout, GraphNodeKind, GraphNodeMember, GraphNodePayload, GraphNodeStatus,
    SelectedGraphTarget,
};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, alert_banner, badge, btn_danger, btn_ghost, btn_icon,
    btn_primary, btn_secondary, h_row, icon_xs, palette, section_heading, status_indicator,
    text_input, to_hsla, v_col,
};

use super::ChatView;
use super::run_graph::{graph_kind_label, graph_status_visual};
use super::run_status::{
    compact_tool_row, format_elapsed_short, graph_status_label, subagent_kind_label,
    subagent_role_label,
};
use super::types::ChatHandoff;

/// Renders the docked `Node Inspector` when `view.selected_graph_node_id` matches a node/member.
pub fn render_node_inspector(
    view: &ChatView,
    topo: &AgentRunTopology,
    layout: &ExecutionGraphLayout,
    cx: &mut Context<ChatView>,
) -> Option<AnyElement> {
    let selected_id = view.selected_graph_node_id.as_deref()?;
    let target = layout.find_selection(selected_id)?;
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();

    let (
        title,
        status,
        kind,
        stage_idx,
        elapsed_ms,
        upstream,
        stop_id,
        task_id,
        attempt,
        waiting,
        error,
        payload,
    ) = match target {
        SelectedGraphTarget::Node(pnode) => {
            let n = &pnode.node;
            (
                n.title.clone(),
                n.status,
                n.kind,
                pnode.stage,
                n.elapsed_ms,
                layout.upstream_titles(&n.id),
                n.stop_run_id.clone(),
                n.task_id.clone(),
                n.attempt,
                n.waiting_summary.clone(),
                n.error.clone(),
                &n.payload,
            )
        }
        SelectedGraphTarget::Member { parent, member } => (
            member.title.clone(),
            member.status,
            if matches!(member.payload, GraphNodePayload::Subagent(_)) {
                GraphNodeKind::Explore
            } else {
                GraphNodeKind::Tool
            },
            parent.stage,
            member.elapsed_ms,
            vec![parent.node.title.clone()],
            member.stop_run_id.clone(),
            member.task_id.clone(),
            member.attempt,
            member.waiting_summary.clone(),
            member.error.clone(),
            &member.payload,
        ),
    };

    let (shape, tone) = graph_status_visual(status);
    let sid_chat = topo.run.session_id.clone();
    let log_run_id = match payload {
        GraphNodePayload::Subagent(sub) => sub.run.run_id.clone(),
        _ => topo.run.run_id.clone(),
    };

    let header = h_row()
        .w_full()
        .flex_shrink_0()
        .justify_between()
        .gap(px(8.0))
        .px(px(14.0))
        .py(px(10.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            h_row()
                .gap(px(6.0))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(ch.graph_inspector_title()),
                )
                .child(badge(
                    ch.graph_stage_label(stage_idx + 1),
                    Tone::Neutral,
                    cx,
                )),
        )
        .child(
            btn_icon("node-insp-close", UiIcon::Close)
                .tooltip(ch.graph_inspector_close())
                .on_click(cx.listener(|this, _, _, cx| {
                    this.select_graph_node(None, cx);
                })),
        );

    let mut body = v_col()
        .w_full()
        .gap(px(10.0))
        .p(px(14.0))
        .child(
            v_col()
                .w_full()
                .gap(px(6.0))
                .child(
                    h_row()
                        .w_full()
                        .gap(px(7.0))
                        .child(status_indicator(shape, cx))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.ink))
                                .child(SharedString::from(title)),
                        ),
                )
                .child(
                    h_row()
                        .w_full()
                        .gap(px(5.0))
                        .flex_wrap()
                        .child(badge(graph_status_label(status, cx), tone, cx))
                        .child(badge(graph_kind_label(kind, cx), Tone::Neutral, cx))
                        .children(
                            (attempt > 0)
                                .then(|| badge(ch.graph_attempt_badge(attempt), Tone::Warning, cx)),
                        )
                        .children(
                            elapsed_ms.map(|ms| badge(format_elapsed_short(ms), Tone::Neutral, cx)),
                        ),
                ),
        )
        .child(
            h_row()
                .w_full()
                .gap(px(6.0))
                .flex_wrap()
                .child(
                    btn_ghost("node-insp-logs", ch.monitor_view_logs())
                        .icon(icon_xs(UiIcon::Usage))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.pending_handoff
                                .set(Some(ChatHandoff::OpenUsageRun(log_run_id.clone())));
                            cx.notify();
                        })),
                )
                .children(stop_id.map(|rid| {
                    btn_danger("node-insp-stop", ch.monitor_stop_subagent()).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.stop_run_by_id(rid.clone(), cx);
                        },
                    ))
                })),
        );

    if let Some(wait_msg) = waiting {
        body = body.child(
            v_col()
                .w_full()
                .gap(px(6.0))
                .p(px(10.0))
                .rounded(px(8.0))
                .bg(to_hsla(p.warn_soft))
                .border_1()
                .border_color(to_hsla(p.warn_line.composite_over(p.warn_soft)))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.warn))
                        .child(SharedString::from(wait_msg)),
                )
                .child(
                    btn_secondary("node-insp-handle", ch.monitor_handle_in_chat()).on_click(
                        cx.listener(move |this, _, _, cx| {
                            this.open_session_from_monitor(sid_chat.clone(), cx);
                        }),
                    ),
                ),
        );
    } else if let Some(err_msg) = error {
        body = body.child(alert_banner(
            Tone::Danger,
            format!("{} · {err_msg}", ch.monitor_error_heading()),
            None,
            cx,
        ));
    }

    if !upstream.is_empty() {
        let mut deps_row = h_row().w_full().gap(px(4.0)).flex_wrap();
        for dep in upstream {
            deps_row = deps_row.child(badge(dep, Tone::Neutral, cx));
        }
        body = body.child(
            v_col()
                .w_full()
                .gap(px(4.0))
                .child(section_heading(ch.graph_inspector_depends_on(), cx))
                .child(deps_row),
        );
    }

    if !topo.run.status.is_active()
        && let Some(tid) = task_id
        && matches!(
            status,
            GraphNodeStatus::Failed
                | GraphNodeStatus::Interrupted
                | GraphNodeStatus::Cancelled
                | GraphNodeStatus::Skipped
        )
    {
        let sid_r = topo.run.session_id.clone();
        let sid_u = topo.run.session_id.clone();
        let sid_s = topo.run.session_id.clone();
        let tid_r = tid.clone();
        let tid_u = tid.clone();
        let tid_s = tid;
        let has_changes = topo.pending_changes.files > 0;
        body = body.child(
            v_col()
                .w_full()
                .gap(px(6.0))
                .p(px(10.0))
                .rounded(px(8.0))
                .bg(to_hsla(p.surface))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(section_heading(ch.graph_node_recovery_heading(), cx))
                .child(text_input(&view.node_recovery_input))
                .child(
                    h_row()
                        .w_full()
                        .gap(px(5.0))
                        .flex_wrap()
                        .child(
                            btn_primary("node-rec-retry", ch.graph_action_retry()).on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.recover_task_node(
                                        sid_r.clone(),
                                        tid_r.clone(),
                                        NodeRecoveryAction::Retry,
                                        window,
                                        cx,
                                    );
                                }),
                            ),
                        )
                        .children(has_changes.then(|| {
                            btn_secondary("node-rec-undo", ch.graph_action_undo_retry()).on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.recover_task_node(
                                        sid_u.clone(),
                                        tid_u.clone(),
                                        NodeRecoveryAction::UndoAndRetry,
                                        window,
                                        cx,
                                    );
                                }),
                            )
                        }))
                        .child(btn_ghost("node-rec-skip", ch.graph_action_skip()).on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.recover_task_node(
                                    sid_s.clone(),
                                    tid_s.clone(),
                                    NodeRecoveryAction::Skip,
                                    window,
                                    cx,
                                );
                            }),
                        )),
                ),
        );
    }

    body = match payload {
        GraphNodePayload::MainRun { is_summary } => {
            render_main_payload(body, view, topo, *is_summary, cx)
        }
        GraphNodePayload::Subagent(sub) => render_subagent_payload(body, view, sub, cx),
        GraphNodePayload::ToolStep(step) => render_tool_payload(body, step, cx),
        GraphNodePayload::ParallelGroup { subagents, tools } => {
            render_group_payload(body, target, subagents, tools, cx)
        }
        GraphNodePayload::PlannedTask { objective, summary } => {
            let mut c = body.child(
                div()
                    .w_full()
                    .p(px(9.0))
                    .rounded(px(7.0))
                    .bg(to_hsla(p.sunken))
                    .text_xs()
                    .text_color(to_hsla(p.ink_2))
                    .child(SharedString::from(objective.clone())),
            );
            if let Some(sum) = summary {
                c = c
                    .child(section_heading(ch.graph_node_summary_heading(), cx))
                    .child(
                        div()
                            .w_full()
                            .p(px(9.0))
                            .rounded(px(7.0))
                            .bg(to_hsla(p.sunken))
                            .text_xs()
                            .text_color(to_hsla(p.ink_2))
                            .child(SharedString::from(sum.clone())),
                    );
            }
            c
        }
    };

    Some(
        v_col()
            .w(px(336.0))
            .flex_shrink_0()
            .h_full()
            .bg(to_hsla(p.pane))
            .border_l_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .child(header)
            .child(
                div()
                    .id("chat-node-inspector-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(body),
            )
            .into_any_element(),
    )
}

fn render_main_payload(
    mut col: gpui_kit::Div,
    view: &ChatView,
    topo: &AgentRunTopology,
    is_summary: bool,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    col = col.child(
        div()
            .text_xs()
            .text_color(to_hsla(p.muted))
            .child(SharedString::from(format!(
                "{} · {}",
                view.format_raw_model_label(&topo.run.model_label),
                ch.monitor_steps_short(topo.run.iterations)
            ))),
    );
    if is_summary && let Some(ans) = &topo.last_answer {
        col = col
            .child(section_heading(ch.monitor_answer_summary(), cx))
            .child(
                div()
                    .w_full()
                    .p(px(9.0))
                    .rounded(px(7.0))
                    .bg(to_hsla(p.sunken))
                    .text_xs()
                    .text_color(to_hsla(p.ink_2))
                    .child(SharedString::from(ans.clone())),
            );
    }
    col
}

fn render_subagent_payload(
    mut col: gpui_kit::Div,
    view: &ChatView,
    sub: &TopologySubagent,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();

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
            .child(SharedString::from(
                view.format_raw_model_label(&sub.run.model_label),
            )),
    );
    col = col.child(meta);

    if !sub.tool_calls.is_empty() {
        col = col.child(section_heading(
            ch.monitor_child_tools_heading(sub.tool_calls.len()),
            cx,
        ));
        let mut steps_box = v_col().w_full().gap(px(4.0));
        for step in &sub.tool_calls {
            steps_box = steps_box.child(compact_tool_row(step, cx));
        }
        col = col.child(steps_box);
    }

    if let Some(ans) = &sub.last_answer {
        col = col
            .child(section_heading(ch.monitor_child_report_heading(), cx))
            .child(
                div()
                    .w_full()
                    .p(px(9.0))
                    .rounded(px(7.0))
                    .bg(to_hsla(p.sunken))
                    .text_xs()
                    .text_color(to_hsla(p.ink_2))
                    .child(SharedString::from(ans.clone())),
            );
    }
    col
}

fn render_tool_payload(
    mut col: gpui_kit::Div,
    step: &TopologyToolStep,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    if let Some(code) = step.exit_code.filter(|c| *c != 0) {
        col = col.child(badge(format!("exit {code}"), Tone::Danger, cx));
    }
    if !step.summary.is_empty() {
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
    col
}

fn render_group_payload(
    mut col: gpui_kit::Div,
    target: SelectedGraphTarget<'_>,
    subagents: &[TopologySubagent],
    tools: &[TopologyToolStep],
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let count = subagents.len() + tools.len();
    col = col.child(section_heading(ch.graph_inspector_members(count), cx));

    if let SelectedGraphTarget::Node(pnode) = target {
        let mut list = v_col().w_full().gap(px(4.0));
        for member in &pnode.node.members {
            list = list.child(clickable_member_row(member, cx));
        }
        return col.child(list);
    }

    let mut fallback = v_col().w_full().gap(px(4.0));
    for step in tools {
        fallback = fallback.child(compact_tool_row(step, cx));
    }
    if !subagents.is_empty() {
        fallback = fallback.child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(ch.monitor_sub_badge(subagents.len()))),
        );
    }
    col.child(fallback)
}

fn clickable_member_row(member: &GraphNodeMember, cx: &mut Context<ChatView>) -> AnyElement {
    let p = palette(cx);
    let (shape, _) = graph_status_visual(member.status);
    let mid = member.id.clone();
    h_row()
        .id(SharedString::from(format!("insp-mem-{}", member.id)))
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
            this.select_graph_node(Some(mid.clone()), cx);
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
        .children(member.elapsed_ms.map(|ms| {
            div()
                .text_size(px(10.5))
                .text_color(to_hsla(p.faint))
                .child(SharedString::from(format_elapsed_short(ms)))
        }))
        .into_any_element()
}
