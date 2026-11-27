//! GitHub-Actions-style left-to-right layered Execution Graph (`执行图`) renderer.
//!
//! Consumes the deterministic Layer 2 [`ExecutionGraphLayout`] and combines:
//! - A bottom GPU vector layer ([`crate::ui::dag_edge_canvas`]) for smooth cubic Bézier dependency
//!   connectors and port anchor dots.
//! - Absolutely positioned native GPUI cards ([`super::run_graph_cards`]) for individual nodes and
//!   collapsible tabbed `Parallel Group` cards (`2/3 completed · Show all`).
//! - Interactive selection synced with [`super::run_node_inspector`], zoom presets, and bulk
//!   group expand/collapse.

use gpui_kit::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::dto::AgentRunTopology;
use pingdeck_desktop::core::agent::model::AgentTodoItem;
use pingdeck_desktop::state::{
    ExecutionGraphLayout, GraphBuildLabels, GraphEdgeTone, GraphNodeKind, GraphNodeStatus,
    build_graph_from_topology, build_plan_preview_graph,
};

use crate::ui::{
    DagEdgeDraw, ScrollableElement as _, StatusShape, Tone, UiIcon, btn_ghost, choice_chip,
    dag_edge_canvas, h_row, icon_xs, palette, to_hsla, v_col,
};

use super::ChatView;
use super::run_graph_cards::render_graph_card;

/// Builds the live [`ExecutionGraphLayout`] for `topo` using localized Layer 3 labels.
#[must_use]
pub fn build_run_graph_layout(
    view: &ChatView,
    topo: &AgentRunTopology,
    cx: &App,
) -> ExecutionGraphLayout {
    let ch = crate::ui::tr(cx).chat();
    let now_ref = if topo.run.status.is_active() {
        pingdeck_desktop::core::app::current_time_ms().max(topo.snapshot_ms)
    } else {
        topo.run.finished_at_ms.unwrap_or(topo.snapshot_ms)
    };
    let labels = GraphBuildLabels {
        trigger_title: ch.graph_trigger_node(),
        summary_title: ch.graph_summary_node(),
        joining_title: ch.graph_joining_node(),
        parallel_group: ch.graph_parallel_group(),
        kind_tool: ch.graph_kind_tool(),
    };
    build_graph_from_topology(topo, &view.expanded_graph_groups, now_ref, &labels)
}

/// Renders a compact read-only GPU Task Graph preview inside the Chat `Plan` approval gate card.
pub fn render_plan_preview_graph(
    view: &ChatView,
    gate_id: &str,
    tasks: &[AgentTodoItem],
    cx: &mut Context<ChatView>,
) -> (usize, AnyElement) {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let labels = GraphBuildLabels {
        trigger_title: ch.graph_trigger_node(),
        summary_title: ch.graph_summary_node(),
        joining_title: ch.graph_joining_node(),
        parallel_group: ch.graph_parallel_group(),
        kind_tool: ch.graph_kind_tool(),
    };
    let layout = build_plan_preview_graph(tasks, &view.expanded_graph_groups, &labels);
    let stage_count = layout.stages.len();
    let scale = 0.82f32;
    let edges: Vec<DagEdgeDraw> = layout
        .edges
        .iter()
        .map(|edge| {
            let color = edge_color(edge.tone, &p);
            DagEdgeDraw {
                points: edge.waypoints.clone(),
                color,
                stroke_width: 1.5,
                dashed: edge.dashed,
                draw_ports: true,
                port_color: color,
            }
        })
        .collect();

    let canvas_w = (layout.canvas_width * scale).max(360.0);
    let canvas_h = (layout.canvas_height * scale).max(130.0);
    let mut stage_surface = div()
        .relative()
        .w(px(canvas_w))
        .h(px(canvas_h))
        .flex_shrink_0()
        .child(dag_edge_canvas(
            edges,
            layout.canvas_width,
            layout.canvas_height,
            scale,
        ));

    for stage in &layout.stages {
        stage_surface = stage_surface.child(
            h_row()
                .absolute()
                .left(px(stage.x * scale))
                .top(px(10.0 * scale))
                .w(px(stage.w * scale))
                .justify_between()
                .px(px(2.0))
                .child(
                    div()
                        .text_size(px(10.0))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(
                            ch.graph_stage_label(stage.stage_index + 1),
                        )),
                ),
        );
    }

    for pnode in &layout.nodes {
        stage_surface = stage_surface.child(render_graph_card(view, pnode, scale, cx));
    }

    let box_el = div()
        .id(SharedString::from(format!("plan-dag-scroll-{gate_id}")))
        .w_full()
        .h(px(canvas_h.clamp(150.0, 260.0)))
        .rounded(px(8.0))
        .bg(to_hsla(p.sunken))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.sunken)))
        .overflow_scrollbar()
        .child(stage_surface)
        .into_any_element();

    (stage_count, box_el)
}

/// Renders the Execution Graph toolbar + scrollable GPU-connected stage canvas.
pub fn render_execution_graph(
    view: &ChatView,
    layout: &ExecutionGraphLayout,
    cx: &mut Context<ChatView>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let scale = view.graph_zoom.clamp(0.75, 1.15);
    let selected_id = view.selected_graph_node_id.as_deref();

    let all_groups_expanded = !layout.collapsible_group_ids.is_empty()
        && layout
            .collapsible_group_ids
            .iter()
            .all(|id| view.expanded_graph_groups.contains(id));
    let group_ids_toggle = layout.collapsible_group_ids.clone();

    let toolbar = h_row()
        .w_full()
        .flex_shrink_0()
        .justify_between()
        .gap(px(8.0))
        .px(px(14.0))
        .py(px(7.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            h_row().flex_1().min_w_0().gap(px(8.0)).child(
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .truncate()
                    .child(SharedString::from(ch.graph_inspector_hint())),
            ),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(4.0))
                .children((!layout.collapsible_group_ids.is_empty()).then(|| {
                    let lbl = if all_groups_expanded {
                        ch.graph_collapse_all()
                    } else {
                        ch.graph_expand_all()
                    };
                    btn_ghost("graph-toggle-groups", lbl).on_click(cx.listener(
                        move |this, _, _, cx| {
                            this.toggle_all_graph_groups(&group_ids_toggle, cx);
                        },
                    ))
                }))
                .child(zoom_pill("graph-zoom-75", "75%", 0.75, scale, cx))
                .child(zoom_pill("graph-zoom-100", "100%", 1.0, scale, cx))
                .child(zoom_pill("graph-zoom-115", "115%", 1.15, scale, cx))
                .children(((scale - 1.0).abs() > 0.01).then(|| {
                    btn_ghost("graph-zoom-reset", ch.graph_zoom_fit()).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.set_graph_zoom(1.0, cx);
                        },
                    ))
                })),
        );

    let edges: Vec<DagEdgeDraw> = layout
        .edges
        .iter()
        .map(|edge| {
            let highlighted =
                selected_id.is_some_and(|sel| sel == edge.from_id || sel == edge.to_id);
            let base_color = edge_color(edge.tone, &p);
            let color = if highlighted {
                to_hsla(p.accent)
            } else {
                base_color
            };
            DagEdgeDraw {
                points: edge.waypoints.clone(),
                color,
                stroke_width: if highlighted { 2.2 } else { 1.5 },
                dashed: edge.dashed,
                draw_ports: true,
                port_color: color,
            }
        })
        .collect();

    let canvas_w = (layout.canvas_width * scale).max(480.0);
    let canvas_h = (layout.canvas_height * scale).max(260.0);

    let mut stage_surface = div()
        .relative()
        .w(px(canvas_w))
        .h(px(canvas_h))
        .flex_shrink_0()
        .child(dag_edge_canvas(
            edges,
            layout.canvas_width,
            layout.canvas_height,
            scale,
        ));

    for stage in &layout.stages {
        stage_surface = stage_surface.child(
            h_row()
                .absolute()
                .left(px(stage.x * scale))
                .top(px(12.0 * scale))
                .w(px(stage.w * scale))
                .justify_between()
                .px(px(2.0))
                .child(
                    div()
                        .text_size(px((11.0 * scale).clamp(9.5, 12.0)))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(
                            ch.graph_stage_label(stage.stage_index + 1),
                        )),
                )
                .children(stage.has_parallel.then(|| {
                    div()
                        .text_color(to_hsla(p.faint))
                        .child(icon_xs(UiIcon::Branch))
                })),
        );
    }

    for pnode in &layout.nodes {
        stage_surface = stage_surface.child(render_graph_card(view, pnode, scale, cx));
    }

    v_col()
        .flex_1()
        .min_w_0()
        .h_full()
        .bg(to_hsla(p.sunken))
        .child(toolbar)
        .child(
            div()
                .id("chat-monitor-graph-scroll")
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_scrollbar()
                .child(stage_surface),
        )
        .into_any_element()
}

fn zoom_pill(
    id: &'static str,
    label: &'static str,
    target: f32,
    current: f32,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let active = (current - target).abs() < 0.02;
    choice_chip(id, label, active, cx).on_click(cx.listener(move |this, _, _, cx| {
        this.set_graph_zoom(target, cx);
    }))
}

/// Maps a [`GraphNodeStatus`] to accessible geometric shape + semantic tone.
#[must_use]
pub const fn graph_status_visual(status: GraphNodeStatus) -> (StatusShape, Tone) {
    match status {
        GraphNodeStatus::Completed => (StatusShape::Ok, Tone::Success),
        GraphNodeStatus::Running => (StatusShape::Run, Tone::Accent),
        GraphNodeStatus::Waiting | GraphNodeStatus::Interrupted => {
            (StatusShape::Warn, Tone::Warning)
        }
        GraphNodeStatus::Failed => (StatusShape::Bad, Tone::Danger),
        GraphNodeStatus::Pending
        | GraphNodeStatus::Queued
        | GraphNodeStatus::Cancelled
        | GraphNodeStatus::Skipped => (StatusShape::Idle, Tone::Neutral),
    }
}

/// Localized label for a [`GraphNodeKind`].
#[must_use]
pub fn graph_kind_label(kind: GraphNodeKind, cx: &App) -> &'static str {
    let ch = crate::ui::tr(cx).chat();
    match kind {
        GraphNodeKind::Main => ch.monitor_main_agent(),
        GraphNodeKind::Explore => ch.graph_kind_explore(),
        GraphNodeKind::Implement => ch.graph_kind_implement(),
        GraphNodeKind::Verify => ch.graph_kind_verify(),
        GraphNodeKind::Gate => ch.graph_kind_gate(),
        GraphNodeKind::Tool => ch.graph_kind_tool(),
        GraphNodeKind::Group => ch.graph_parallel_group(),
    }
}

fn edge_color(tone: GraphEdgeTone, p: &pingdeck_desktop::theme::ResolvedPalette) -> gpui_kit::Hsla {
    match tone {
        GraphEdgeTone::Active => to_hsla(p.accent),
        GraphEdgeTone::Completed | GraphEdgeTone::Neutral => {
            to_hsla(p.line_strong.composite_over(p.sunken))
        }
        GraphEdgeTone::Warning => to_hsla(p.warn),
        GraphEdgeTone::Danger => to_hsla(p.bad),
        GraphEdgeTone::Skipped => to_hsla(p.line.composite_over(p.sunken)),
    }
}

pub(super) fn card_border_color(
    selected: bool,
    tone: Tone,
    p: &pingdeck_desktop::theme::ResolvedPalette,
) -> gpui_kit::Hsla {
    if selected {
        return to_hsla(p.accent);
    }
    match tone {
        Tone::Danger => to_hsla(p.bad_line.composite_over(p.surface)),
        Tone::Warning => to_hsla(p.warn_line.composite_over(p.surface)),
        Tone::Accent => to_hsla(p.accent_line.composite_over(p.surface)),
        _ => to_hsla(p.line_strong.composite_over(p.surface)),
    }
}
