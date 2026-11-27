//! Docked resizable `Node Inspector` (`节点详情`) panel for [`WorkflowMonitorModal`].
//!
//! Renders the shared inspector chrome (clickable breadcrumb trail, status/kind/attempt badges,
//! quick actions, waiting gate / failure diagnostics, clickable upstream & downstream dependency
//! jump chips, and Task Graph selective node recovery console), then delegates archetype-specific
//! body rendering to [`super::node_payloads`].

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::agent::dto::AgentRunTopology;
use spanbay_desktop::core::agent::model::NodeRecoveryAction;
use spanbay_desktop::state::{
    ExecutionGraphLayout, GraphNodeKind, GraphNodePayload, GraphNodeStatus, SelectedGraphTarget,
};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, alert_banner, badge, btn_danger, btn_ghost, btn_icon,
    btn_primary, btn_secondary, h_row, icon_xs, palette, section_heading, status_indicator,
    text_input, to_hsla, v_col,
};

use super::modal::WorkflowMonitorModal;
use super::node_payloads::{
    render_group_payload, render_main_payload, render_planned_task_payload,
    render_subagent_payload, render_tool_payload,
};
use super::status::{
    format_elapsed_short, graph_kind_label, graph_status_label, graph_status_visual,
};

pub fn render_node_inspector(
    view: &WorkflowMonitorModal,
    topo: &AgentRunTopology,
    layout: &ExecutionGraphLayout,
    cx: &mut Context<WorkflowMonitorModal>,
) -> Option<AnyElement> {
    let selected_id = view.selected_node_id.as_deref()?;
    let target = layout.find_selection(selected_id)?;
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();

    let (
        node_key,
        title,
        status,
        kind,
        stage_idx,
        elapsed_ms,
        parent_crumb,
        upstream,
        downstream,
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
                n.id.clone(),
                n.title.clone(),
                n.status,
                n.kind,
                pnode.stage,
                n.elapsed_ms,
                None,
                layout.upstream_nodes(&n.id),
                layout.downstream_nodes(&n.id),
                n.stop_run_id.clone(),
                n.task_id.clone(),
                n.attempt,
                n.waiting_summary.clone(),
                n.error.clone(),
                &n.payload,
            )
        }
        SelectedGraphTarget::Member { parent, member } => (
            member.id.clone(),
            member.title.clone(),
            member.status,
            if matches!(member.payload, GraphNodePayload::Subagent(_)) {
                GraphNodeKind::Explore
            } else {
                GraphNodeKind::Tool
            },
            parent.stage,
            member.elapsed_ms,
            Some((parent.node.id.clone(), parent.node.title.clone())),
            layout.upstream_nodes(&parent.node.id),
            layout.downstream_nodes(&parent.node.id),
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

    let mut crumbs = h_row().flex_1().min_w_0().gap(px(5.0)).child(badge(
        ch.graph_stage_label(stage_idx + 1),
        Tone::Neutral,
        cx,
    ));
    if let Some((pid, ptitle)) = parent_crumb {
        crumbs = crumbs
            .child(div().text_xs().text_color(to_hsla(p.faint)).child("›"))
            .child(
                div()
                    .id("wf-insp-crumb-parent")
                    .max_w(px(140.0))
                    .text_xs()
                    .text_color(to_hsla(p.accent_ink))
                    .truncate()
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.select_node(Some(pid.clone()), cx);
                    }))
                    .child(SharedString::from(ptitle)),
            );
    } else {
        crumbs = crumbs.child(
            div()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(p.ink))
                .child(ch.graph_inspector_title()),
        );
    }

    let header = h_row()
        .w_full()
        .flex_shrink_0()
        .justify_between()
        .gap(px(8.0))
        .px(px(14.0))
        .py(px(10.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(crumbs)
        .child(
            btn_icon("wf-insp-close", UiIcon::Close)
                .tooltip(ch.graph_inspector_close())
                .on_click(cx.listener(|this, _, _, cx| this.select_node(None, cx))),
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
                    btn_ghost("wf-insp-logs", ch.monitor_view_logs())
                        .icon(icon_xs(UiIcon::Usage))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_usage_and_close(log_run_id.clone(), cx);
                        })),
                )
                .children(stop_id.map(|rid| {
                    btn_danger("wf-insp-stop", ch.monitor_stop_subagent()).on_click(cx.listener(
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
                    btn_primary("wf-insp-handle", ch.monitor_handle_in_chat())
                        .icon(icon_xs(UiIcon::ExternalLink))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_chat_and_close(sid_chat.clone(), cx);
                        })),
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
        body = body.child(render_dep_chips(
            "up",
            ch.graph_inspector_depends_on(),
            upstream,
            cx,
        ));
    }
    if !downstream.is_empty() {
        body = body.child(render_dep_chips(
            "down",
            ch.graph_inspector_downstream(),
            downstream,
            cx,
        ));
    }

    // Selective node-level recovery console for failed / interrupted / cancelled / skipped Task Graph nodes.
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
                            btn_primary("wf-rec-retry", ch.graph_action_retry()).on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.recover_task_node(
                                        tid_r.clone(),
                                        NodeRecoveryAction::Retry,
                                        window,
                                        cx,
                                    );
                                }),
                            ),
                        )
                        .children(has_changes.then(|| {
                            btn_secondary("wf-rec-undo", ch.graph_action_undo_retry()).on_click(
                                cx.listener(move |this, _, window, cx| {
                                    this.recover_task_node(
                                        tid_u.clone(),
                                        NodeRecoveryAction::UndoAndRetry,
                                        window,
                                        cx,
                                    );
                                }),
                            )
                        }))
                        .child(btn_ghost("wf-rec-skip", ch.graph_action_skip()).on_click(
                            cx.listener(move |this, _, window, cx| {
                                this.recover_task_node(
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
        GraphNodePayload::Subagent(sub) => render_subagent_payload(body, view, topo, sub, cx),
        GraphNodePayload::ToolStep(step) => {
            render_tool_payload(body, view, topo, step, &node_key, cx)
        }
        GraphNodePayload::ParallelGroup { subagents, tools } => {
            render_group_payload(body, target, subagents, tools, cx)
        }
        GraphNodePayload::PlannedTask { objective, summary } => {
            render_planned_task_payload(body, objective, summary.as_deref(), cx)
        }
    };

    Some(
        v_col()
            .size_full()
            .bg(to_hsla(p.pane))
            .border_l_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .child(header)
            .child(
                div()
                    .id("wf-node-insp-scroll")
                    .flex_1()
                    .min_h_0()
                    .overflow_y_scrollbar()
                    .child(body),
            )
            .into_any_element(),
    )
}

fn render_dep_chips(
    prefix: &'static str,
    heading: &'static str,
    nodes: Vec<(String, String)>,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let mut row = h_row().w_full().gap(px(4.0)).flex_wrap();
    for (nid, title) in nodes {
        let target_id = nid.clone();
        row = row.child(
            div()
                .id(SharedString::from(format!("wf-dep-{prefix}-{nid}")))
                .px(px(7.0))
                .py(px(2.5))
                .rounded(px(5.0))
                .bg(to_hsla(p.surface))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .text_size(px(11.0))
                .text_color(to_hsla(p.ink_2))
                .cursor_pointer()
                .hover(|s| s.border_color(to_hsla(p.accent)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.select_node(Some(target_id.clone()), cx);
                }))
                .child(SharedString::from(title)),
        );
    }
    v_col()
        .w_full()
        .gap(px(4.0))
        .child(section_heading(heading, cx))
        .child(row)
        .into_any_element()
}
