//! GitHub-Actions-style left-to-right layered Execution Graph (`执行图`) renderer for the 90%
//! viewport [`WorkflowMonitorModal`], plus the standalone Plan approval Task Graph preview
//! reused by `views/chat/gates.rs`.

use std::collections::HashSet;

use gpui_kit::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::agent::dto::AgentRunTopology;
use pingdeck_desktop::core::agent::model::AgentTodoItem;
use pingdeck_desktop::state::{
    ExecutionGraphLayout, GraphBuildLabels, GraphEdgeTone, SelectedGraphTarget,
    build_graph_from_topology, build_plan_preview_graph,
};

use crate::ui::{
    DagEdgeDraw, ScrollableElement as _, Tone, UiIcon, dag_edge_canvas, h_row, icon_xs, palette,
    to_hsla, v_col,
};

use super::graph_cards::{render_modal_graph_card, render_preview_graph_card};
use super::modal::WorkflowMonitorModal;

/// Builds the live [`ExecutionGraphLayout`] for `topo` using localized Layer 3 labels.
#[must_use]
pub fn build_modal_graph_layout(
    view: &WorkflowMonitorModal,
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
    build_graph_from_topology(topo, &view.expanded_groups, now_ref, &labels)
}

/// Renders a compact read-only GPU Task Graph preview inside the Chat `Plan` approval gate card.
/// Decoupled from `WorkflowMonitorModal` so `ChatView` renders plan previews cleanly with zero
/// coupling to modal state.
pub fn render_plan_preview_graph(
    gate_id: &str,
    tasks: &[AgentTodoItem],
    expanded_groups: &HashSet<String>,
    cx: &App,
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
    let layout = build_plan_preview_graph(tasks, expanded_groups, &labels);
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
        stage_surface = stage_surface.child(render_preview_graph_card(pnode, scale, cx));
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

/// Renders the scrollable GPU-connected Execution Graph canvas inside the 90% modal.
///
/// Features:
/// - Focused dependency path highlighting: edges connected to the selected node (or its parent
///   container) highlight in accent at 2.2px while unrelated edges soften to 35% opacity.
/// - Clicking blank canvas background collapses the right-hand Node Inspector for 100% full-width
///   graph viewing.
pub fn render_execution_graph(
    view: &WorkflowMonitorModal,
    layout: &ExecutionGraphLayout,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let scale = view.graph_zoom.clamp(0.75, 1.15);

    // Resolve member selections to their owning top-level canvas node id so incident edges highlight.
    let focus_owner_id: Option<&str> =
        view.selected_node_id
            .as_deref()
            .and_then(|sel| match layout.find_selection(sel) {
                Some(SelectedGraphTarget::Node(n)) => Some(n.node.id.as_str()),
                Some(SelectedGraphTarget::Member { parent, .. }) => Some(parent.node.id.as_str()),
                None => None,
            });

    let edges: Vec<DagEdgeDraw> = layout
        .edges
        .iter()
        .map(|edge| {
            let highlighted =
                focus_owner_id.is_some_and(|sel| sel == edge.from_id || sel == edge.to_id);
            let dimmed = focus_owner_id.is_some() && !highlighted;
            let mut color = if highlighted {
                to_hsla(p.accent)
            } else {
                edge_color(edge.tone, &p)
            };
            if dimmed {
                color.a *= 0.35;
            }
            DagEdgeDraw {
                points: edge.waypoints.clone(),
                color,
                stroke_width: if highlighted { 2.3 } else { 1.5 },
                dashed: edge.dashed,
                draw_ports: true,
                port_color: color,
            }
        })
        .collect();

    let canvas_w = (layout.canvas_width * scale).max(520.0);
    let canvas_h = (layout.canvas_height * scale).max(280.0);

    let mut stage_surface = div()
        .id("wf-graph-stage-surface")
        .relative()
        .w(px(canvas_w))
        .h(px(canvas_h))
        .flex_shrink_0()
        .on_click(cx.listener(|this, _, _, cx| {
            if this.selected_node_id.is_some() {
                this.select_node(None, cx);
            }
        }))
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
        stage_surface = stage_surface.child(render_modal_graph_card(view, pnode, scale, cx));
    }

    v_col()
        .size_full()
        .bg(to_hsla(p.sunken))
        .child(
            div()
                .id("wf-graph-scroll")
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_scrollbar()
                .child(stage_surface),
        )
        .into_any_element()
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
