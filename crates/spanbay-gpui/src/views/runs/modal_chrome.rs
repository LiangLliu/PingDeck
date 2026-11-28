//! Single-line telemetry ribbon, collapsible background processes tray, 4-tab view switcher with
//! canvas zoom controls, and footer bar for [`WorkflowMonitorModal`].

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::agent::dto::AgentRunTopology;
use spanbay_desktop::core::agent::tools::process::ProcessState;
use spanbay_desktop::state::{
    CrossAlignment, ExecutionGraphLayout, GraphNodePayload, LayoutDirection,
};

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, btn_danger, btn_ghost, btn_primary, btn_secondary, card,
    choice_chip, h_row, icon_xs, palette, section_heading, status_indicator, text_tooltip, to_hsla,
    v_col,
};

use super::modal::WorkflowMonitorModal;
use super::status::{
    folder_basename, format_elapsed_short, format_model_short, format_tokens_compact,
};
use super::types::WorkflowTab;
use crate::ui::run_status_meta;

impl WorkflowMonitorModal {
    #[must_use]
    pub fn is_subagent_selected(&self, sub_run_id: &str) -> bool {
        let Some(sel) = self.selected_node_id.as_deref() else {
            return false;
        };
        if sel.strip_prefix("sub:") == Some(sub_run_id) {
            return true;
        }
        if let Some(tid) = sel.strip_prefix("task:")
            && let Some(topo) = &self.topology
        {
            return topo
                .task_graph
                .iter()
                .any(|t| t.id == tid && t.run_id.as_deref() == Some(sub_run_id));
        }
        false
    }

    #[must_use]
    pub fn is_node_selected(&self, node_id: &str, payload: &GraphNodePayload) -> bool {
        let Some(sel) = self.selected_node_id.as_deref() else {
            return false;
        };
        if sel == node_id {
            return true;
        }
        if let GraphNodePayload::Subagent(sub) = payload
            && sel.strip_prefix("sub:") == Some(sub.run.run_id.as_str())
        {
            return true;
        }
        false
    }
}
pub fn render_modal_telemetry(
    view: &WorkflowMonitorModal,
    topo: &AgentRunTopology,
    layout: Option<&ExecutionGraphLayout>,
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
    let proc_count = view.active_process_count();

    h_row()
        .w_full()
        .flex_shrink_0()
        .justify_between()
        .gap(px(10.0))
        .px(px(16.0))
        .py(px(6.0))
        .bg(to_hsla(p.surface))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(10.0))
                .flex_wrap()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(
                    h_row()
                        .gap(px(5.0))
                        .child(status_indicator(shape, cx))
                        .child(badge(status_lbl, tone, cx)),
                )
                .child(SharedString::from(format_model_short(
                    &topo.run.model_label,
                )))
                .children(topo.folder.as_deref().map(|f| {
                    h_row()
                        .gap(px(3.0))
                        .child(icon_xs(UiIcon::Folder))
                        .child(SharedString::from(folder_basename(f).to_owned()))
                }))
                .children(layout.map(|l| {
                    SharedString::from(ch.graph_group_progress(l.completed_nodes, l.total_nodes))
                }))
                .child(SharedString::from(format_elapsed_short(elapsed_ms)))
                .child(SharedString::from(
                    ch.monitor_steps_short(topo.run.iterations),
                ))
                .children((total_tok > 0).then(|| {
                    crate::ui::tr(cx)
                        .chat()
                        .context_used(format_tokens_compact(total_tok))
                }))
                // No amount without a known currency.
                .children(
                    topo.cost_currency
                        .as_deref()
                        .filter(|_| topo.run.cost_micro_usd > 0)
                        .map(|cur| {
                            SharedString::from(format!(
                                "{cur} {:.4}",
                                topo.run.cost_micro_usd as f64 / 1_000_000.0
                            ))
                        }),
                )
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
        )
        .children((!view.processes.is_empty()).then(|| {
            choice_chip(
                "wf-bg-procs-btn",
                ch.monitor_bg_processes_btn(proc_count),
                view.show_processes,
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.show_processes = !this.show_processes;
                cx.notify();
            }))
        }))
        .into_any_element()
}

pub fn render_processes_tray(
    view: &WorkflowMonitorModal,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let mut row = h_row().w_full().gap(px(8.0)).flex_wrap();
    for proc_item in &view.processes {
        let pid_kill = proc_item.id.clone();
        let running = proc_item.state == ProcessState::Running;
        row = row.child(
            card(cx).px(px(9.0)).py(px(5.0)).child(
                h_row()
                    .gap(px(8.0))
                    .child(status_indicator(
                        if running {
                            StatusShape::Run
                        } else {
                            StatusShape::Idle
                        },
                        cx,
                    ))
                    .child(
                        div()
                            .max_w(px(300.0))
                            .text_xs()
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(SharedString::from(proc_item.command.clone())),
                    )
                    .children(proc_item.pid.map(|pid| {
                        div()
                            .text_size(px(10.5))
                            .text_color(to_hsla(p.faint))
                            .child(format!("PID {pid}"))
                    }))
                    .children(running.then(|| {
                        btn_danger(
                            SharedString::from(format!("wf-pk-{}", proc_item.id)),
                            ch.inspector_kill(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.kill_process(pid_kill.clone(), cx);
                        }))
                    })),
            ),
        );
    }
    v_col()
        .w_full()
        .flex_shrink_0()
        .gap(px(5.0))
        .px(px(16.0))
        .py(px(8.0))
        .bg(to_hsla(p.sunken))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.sunken)))
        .child(section_heading(ch.inspector_background_processes(), cx))
        .child(row)
        .into_any_element()
}

pub fn render_modal_view_bar(
    view: &WorkflowMonitorModal,
    topo: &AgentRunTopology,
    active_tab: WorkflowTab,
    is_auto: bool,
    graph_ok: bool,
    layout: Option<&ExecutionGraphLayout>,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let rid_s = topo.run.run_id.clone();
    let rid_g = topo.run.run_id.clone();
    let rid_t = topo.run.run_id.clone();
    let rid_c = topo.run.run_id.clone();
    let change_count = view.changes.len();
    let changes_lbl = if change_count > 0 {
        ch.monitor_view_changes(change_count)
    } else {
        ch.monitor_view_changes_empty().to_owned()
    };

    let graph_btn: AnyElement = if graph_ok {
        choice_chip(
            "wf-tab-graph",
            ch.monitor_view_graph(),
            active_tab == WorkflowTab::Graph,
            cx,
        )
        .on_click(cx.listener(move |this, _, _, cx| {
            this.set_tab(rid_g.clone(), WorkflowTab::Graph, cx);
        }))
        .into_any_element()
    } else {
        div()
            .id("wf-tab-graph-disabled")
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

    let graph_tools = (active_tab == WorkflowTab::Graph).then(|| {
        let scale = view.graph_zoom.clamp(0.75, 1.15);
        let policy = view.layout_policy;
        let has_pinned = !view.pinned_positions.is_empty();
        let group_ids = layout
            .map(|l| l.collapsible_group_ids.clone())
            .unwrap_or_default();
        let all_open =
            !group_ids.is_empty() && group_ids.iter().all(|id| view.expanded_groups.contains(id));
        let align_lbl = match policy.alignment {
            CrossAlignment::Start => ch.graph_layout_align_start(),
            CrossAlignment::Center | CrossAlignment::Barycenter => ch.graph_layout_align_center(),
        };
        h_row()
            .flex_shrink_0()
            .gap(px(4.0))
            .flex_wrap()
            .child(
                choice_chip(
                    "wf-dir-lr",
                    ch.graph_layout_lr(),
                    policy.direction == LayoutDirection::LeftToRight,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.set_layout_direction(LayoutDirection::LeftToRight, cx);
                })),
            )
            .child(
                choice_chip(
                    "wf-dir-tb",
                    ch.graph_layout_tb(),
                    policy.direction == LayoutDirection::TopToBottom,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.set_layout_direction(LayoutDirection::TopToBottom, cx);
                })),
            )
            .child(
                choice_chip(
                    "wf-dir-wrap",
                    ch.graph_layout_wrap(),
                    policy.direction.is_serpentine(),
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.set_layout_direction(
                        LayoutDirection::Serpentine {
                            max_per_row: LayoutDirection::DEFAULT_SERPENTINE_COLS,
                        },
                        cx,
                    );
                })),
            )
            .child(
                choice_chip(
                    "wf-lanes-toggle",
                    ch.graph_layout_swimlanes(),
                    policy.swimlanes,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_swimlanes(cx))),
            )
            .child(
                choice_chip(
                    "wf-align-toggle",
                    align_lbl,
                    policy.alignment == CrossAlignment::Center,
                    cx,
                )
                .on_click(cx.listener(|this, _, _, cx| this.toggle_alignment(cx))),
            )
            .children(has_pinned.then(|| {
                btn_ghost("wf-layout-reset", ch.graph_layout_reset())
                    .icon(icon_xs(UiIcon::RotateCcw))
                    .on_click(cx.listener(|this, _, _, cx| this.reset_pinned_layout(cx)))
            }))
            .children((!group_ids.is_empty()).then(|| {
                let lbl = if all_open {
                    ch.graph_collapse_all()
                } else {
                    ch.graph_expand_all()
                };
                btn_ghost("wf-graph-toggle-all", lbl).on_click(cx.listener(
                    move |this, _, _, cx| {
                        this.toggle_all_groups(&group_ids, cx);
                    },
                ))
            }))
            .child(zoom_pill("wf-zoom-75", "75%", 0.75, scale, cx))
            .child(zoom_pill("wf-zoom-100", "100%", 1.0, scale, cx))
            .child(zoom_pill("wf-zoom-115", "115%", 1.15, scale, cx))
            .children(((scale - 1.0).abs() > 0.01).then(|| {
                btn_ghost("wf-zoom-reset", ch.graph_zoom_fit())
                    .on_click(cx.listener(|this, _, _, cx| this.set_zoom(1.0, cx)))
            }))
    });

    h_row()
        .w_full()
        .flex_shrink_0()
        .justify_between()
        .gap(px(10.0))
        .px(px(16.0))
        .py(px(7.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            h_row()
                .gap(px(5.0))
                .child(
                    choice_chip(
                        "wf-tab-steps",
                        ch.monitor_view_steps(),
                        active_tab == WorkflowTab::Steps,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_tab(rid_s.clone(), WorkflowTab::Steps, cx);
                    })),
                )
                .child(graph_btn)
                .child(
                    choice_chip(
                        "wf-tab-timeline",
                        ch.monitor_view_timeline(),
                        active_tab == WorkflowTab::Timeline,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_tab(rid_t.clone(), WorkflowTab::Timeline, cx);
                    })),
                )
                .child(
                    choice_chip(
                        "wf-tab-changes",
                        changes_lbl,
                        active_tab == WorkflowTab::Changes,
                        cx,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.set_tab(rid_c.clone(), WorkflowTab::Changes, cx);
                    })),
                )
                .children(is_auto.then(|| badge(ch.monitor_view_auto_badge(), Tone::Neutral, cx))),
        )
        .children(graph_tools)
        .into_any_element()
}

fn zoom_pill(
    id: &'static str,
    label: &'static str,
    target: f32,
    current: f32,
    cx: &mut Context<WorkflowMonitorModal>,
) -> impl IntoElement {
    choice_chip(id, label, (current - target).abs() < 0.02, cx)
        .on_click(cx.listener(move |this, _, _, cx| this.set_zoom(target, cx)))
}

pub fn render_modal_footer(
    view: &WorkflowMonitorModal,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let needs_gate = view
        .topology
        .as_ref()
        .is_some_and(|t| t.wait.is_some() || t.pending_approval.is_some());
    let sid = view.session_id.clone();

    h_row()
        .w_full()
        .flex_shrink_0()
        .justify_between()
        .gap(px(12.0))
        .px(px(16.0))
        .py(px(9.0))
        .bg(to_hsla(p.pane))
        .border_t_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .truncate()
                .child(ch.monitor_footer_hint()),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(8.0))
                .children((needs_gate && !sid.is_empty()).then(|| {
                    btn_primary("wf-foot-gate", ch.monitor_handle_in_chat())
                        .icon(icon_xs(UiIcon::ExternalLink))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.open_chat_and_close(sid.clone(), cx);
                        }))
                }))
                .child(
                    btn_secondary("wf-foot-close", ch.monitor_close())
                        .on_click(cx.listener(|this, _, _, cx| this.close(cx))),
                ),
        )
        .into_any_element()
}
