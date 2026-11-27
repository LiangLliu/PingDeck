//! Unified Process Graph (`执行图`) renderer for the 90% viewport [`WorkflowMonitorModal`]
//! (supporting LR / TB / Serpentine wrap, Center / Start alignment, role swimlanes, and hybrid
//! manual drag-to-reposition with snapping guides), plus the standalone Plan approval Task Graph
//! preview reused by `views/chat/gates.rs`.

use std::collections::HashSet;

use gpui_kit::{
    AnyElement, App, Context, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    div, px,
};
use spanbay_desktop::core::agent::dto::AgentRunTopology;
use spanbay_desktop::core::agent::model::AgentTodoItem;
use spanbay_desktop::state::{
    ExecutionGraphLayout, GRAPH_GROUP_TAB_H, GraphBuildLabels, GraphEdgeTone, GuideOrientation,
    PositionedNode, PositionedProcessNode, ProcessEdgeTone, SelectedGraphTarget,
    build_graph_from_topology_with_policy, build_plan_preview_graph, snap_drag_position,
};

use crate::ui::{
    DagEdgeDraw, ProcessGuideDraw, ProcessLaneDraw, ScrollableElement as _, Tone, UiIcon, badge,
    dag_edge_canvas, h_row, icon_xs, lane_draw_spec, palette, process_edge_label_pill,
    process_lane_header, process_stage_canvas, status_indicator, to_hsla, v_col,
};

use super::graph_cards::render_modal_graph_card;
use super::modal::WorkflowMonitorModal;
use super::status::{graph_kind_label, graph_status_visual, subagent_role_label};
use super::types::GraphDragState;

const DRAG_ACTIVATION_PX: f32 = 4.0;

impl WorkflowMonitorModal {
    pub(super) fn begin_node_drag(
        &mut self,
        node_id: String,
        node_pos: (f32, f32),
        node_size: (f32, f32),
        scale: f32,
        ev: &MouseDownEvent,
    ) {
        self.drag_suppress_click = false;
        self.drag_state = Some(GraphDragState {
            node_id,
            start_mouse: (f32::from(ev.position.x), f32::from(ev.position.y)),
            start_node_pos: node_pos,
            node_size,
            scale: scale.clamp(0.5, 1.5),
            dragging: false,
        });
    }

    pub(super) fn update_node_drag(
        &mut self,
        ev: &MouseMoveEvent,
        neighbors: &[PositionedProcessNode],
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = &mut self.drag_state else {
            return;
        };
        let mx = f32::from(ev.position.x);
        let my = f32::from(ev.position.y);
        let screen_dx = mx - drag.start_mouse.0;
        let screen_dy = my - drag.start_mouse.1;
        if !drag.dragging && screen_dx.hypot(screen_dy) < DRAG_ACTIVATION_PX {
            return;
        }
        drag.dragging = true;
        let raw_x = (drag.start_node_pos.0 + screen_dx / drag.scale).max(20.0);
        let raw_y = (drag.start_node_pos.1 + screen_dy / drag.scale).max(24.0);
        let (sx, sy, guides) = snap_drag_position(
            &drag.node_id,
            raw_x,
            raw_y,
            drag.node_size.0,
            drag.node_size.1,
            neighbors,
        );
        self.pinned_positions.insert(drag.node_id.clone(), (sx, sy));
        self.alignment_guides = guides;
        cx.notify();
    }

    pub(super) fn end_node_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag_state.take() else {
            return;
        };
        if drag.dragging {
            self.drag_suppress_click = true;
        }
        self.alignment_guides.clear();
        cx.notify();
    }
}

fn graph_labels(ch: spanbay_desktop::i18n::Chat) -> GraphBuildLabels<'static> {
    GraphBuildLabels {
        trigger_title: ch.graph_trigger_node(),
        summary_title: ch.graph_summary_node(),
        joining_title: ch.graph_joining_node(),
        parallel_group: ch.graph_parallel_group(),
        kind_tool: ch.graph_kind_tool(),
        lane_main: ch.monitor_main_agent(),
        lane_explore: ch.graph_kind_explore(),
        lane_implement: ch.graph_kind_implement(),
        lane_verify: ch.graph_kind_verify(),
    }
}

/// Builds the live [`ExecutionGraphLayout`] for `topo` using the active layout policy and manual
/// drag overrides.
#[must_use]
pub fn build_modal_graph_layout(
    view: &WorkflowMonitorModal,
    topo: &AgentRunTopology,
    cx: &App,
) -> ExecutionGraphLayout {
    let ch = crate::ui::tr(cx).chat();
    let now_ref = if topo.run.status.is_active() {
        spanbay_desktop::core::app::current_time_ms().max(topo.snapshot_ms)
    } else {
        topo.run.finished_at_ms.unwrap_or(topo.snapshot_ms)
    };
    let labels = graph_labels(ch);
    build_graph_from_topology_with_policy(
        topo,
        &view.expanded_groups,
        view.layout_policy,
        &view.pinned_positions,
        now_ref,
        &labels,
    )
}

/// Renders a compact read-only GPU Task Graph preview inside the Chat `Plan` approval gate card.
pub fn render_plan_preview_graph(
    gate_id: &str,
    tasks: &[AgentTodoItem],
    expanded_groups: &HashSet<String>,
    cx: &App,
) -> (usize, AnyElement) {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let labels = graph_labels(ch);
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
                horizontal_ports: edge.horizontal_ports,
                draw_ports: true,
                port_color: color,
                draw_arrow: true,
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
                .top(px(stage.y * scale))
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

/// Renders the interactive Process Graph canvas inside the 90% modal with swimlane bands,
/// multi-direction Bézier curves, focused causal path highlighting, and live drag-to-reposition.
pub fn render_execution_graph(
    view: &WorkflowMonitorModal,
    layout: &ExecutionGraphLayout,
    cx: &mut Context<WorkflowMonitorModal>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let scale = view.graph_zoom.clamp(0.75, 1.15);

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
                horizontal_ports: edge.horizontal_ports,
                draw_ports: true,
                port_color: color,
                draw_arrow: true,
            }
        })
        .collect();

    let lane_draws: Vec<ProcessLaneDraw> = layout
        .lanes
        .iter()
        .map(|band| lane_draw_spec(band, false, cx))
        .collect();

    let mut guide_color = to_hsla(p.accent);
    guide_color.a = 0.72;
    let guide_draws: Vec<ProcessGuideDraw> = view
        .alignment_guides
        .iter()
        .map(|g| ProcessGuideDraw {
            horizontal: g.orientation == GuideOrientation::Horizontal,
            coord: g.coord,
            span_start: g.span_start,
            span_end: g.span_end,
            color: guide_color,
        })
        .collect();

    let neighbors_for_snap: Vec<PositionedProcessNode> = layout
        .nodes
        .iter()
        .map(|n| PositionedProcessNode {
            id: n.node.id.clone(),
            stage: n.stage,
            lane_id: n.lane_id.clone(),
            x: n.x,
            y: n.y,
            w: n.w,
            h: n.h,
            pinned: n.pinned,
            ports: Vec::new(),
            port_in: n.port_in,
            port_out: n.port_out,
            has_in_edge: n.has_in_edge,
            has_out_edge: n.has_out_edge,
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
        .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
            if this.drag_state.is_some() {
                this.update_node_drag(ev, &neighbors_for_snap, cx);
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| this.end_node_drag(cx)),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| this.end_node_drag(cx)),
        )
        .on_click(cx.listener(|this, _, _, cx| {
            if this.drag_suppress_click {
                this.drag_suppress_click = false;
                return;
            }
            if this.selected_node_id.is_some() {
                this.select_node(None, cx);
            }
        }))
        .child(process_stage_canvas(
            lane_draws,
            guide_draws,
            edges,
            layout.canvas_width,
            layout.canvas_height,
            scale,
        ));

    for band in &layout.lanes {
        stage_surface = stage_surface.child(process_lane_header(band, scale, cx));
    }

    for stage in &layout.stages {
        stage_surface = stage_surface.child(
            h_row()
                .absolute()
                .left(px(stage.x * scale))
                .top(px(stage.y * scale))
                .w(px(stage.w * scale))
                .justify_between()
                .px(px(2.0))
                .child(
                    div()
                        .text_size(px((11.0 * scale).clamp(9.5, 12.0)))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.faint))
                        .truncate()
                        .child(SharedString::from(
                            ch.graph_stage_label(stage.stage_index + 1),
                        )),
                )
                .children((stage.has_parallel && !stage.vertical).then(|| {
                    div()
                        .text_color(to_hsla(p.faint))
                        .child(icon_xs(UiIcon::Branch))
                })),
        );
    }

    for edge in &layout.edges {
        if let (Some(lbl), Some(pos)) = (&edge.label, edge.label_pos) {
            let ptone = match edge.tone {
                GraphEdgeTone::Completed => ProcessEdgeTone::Completed,
                GraphEdgeTone::Active => ProcessEdgeTone::Active,
                GraphEdgeTone::Warning => ProcessEdgeTone::Warning,
                GraphEdgeTone::Danger => ProcessEdgeTone::Danger,
                GraphEdgeTone::Skipped => ProcessEdgeTone::Skipped,
                GraphEdgeTone::Neutral => ProcessEdgeTone::Neutral,
            };
            stage_surface =
                stage_surface.child(process_edge_label_pill(lbl, pos, ptone, scale, cx));
        }
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

fn render_preview_graph_card(pnode: &PositionedNode, scale: f32, cx: &App) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let node = &pnode.node;
    let (shape, tone) = graph_status_visual(node.status);
    let border = card_border_color(false, tone, &p);

    if let Some(tab_text) = &node.group_tab
        && !node.members.is_empty()
    {
        let (done, total) = node.member_progress();
        let tab_h = GRAPH_GROUP_TAB_H * scale;
        let body_h = (pnode.h - GRAPH_GROUP_TAB_H) * scale;
        return v_col()
            .absolute()
            .left(px(pnode.x * scale))
            .top(px(pnode.y * scale))
            .w(px(pnode.w * scale))
            .h(px(pnode.h * scale))
            .items_start()
            .child(
                h_row()
                    .h(px(tab_h))
                    .max_w(px((pnode.w * 0.78) * scale))
                    .px(px(8.0 * scale))
                    .bg(to_hsla(p.pane))
                    .border_t_1()
                    .border_l_1()
                    .border_r_1()
                    .border_color(border)
                    .rounded_t(px(6.0 * scale))
                    .child(
                        div()
                            .text_size(px((10.0 * scale).clamp(9.0, 11.0)))
                            .text_color(to_hsla(p.muted))
                            .truncate()
                            .child(SharedString::from(tab_text.clone())),
                    ),
            )
            .child(
                v_col()
                    .w_full()
                    .h(px(body_h))
                    .px(px(9.0 * scale))
                    .py(px(7.0 * scale))
                    .gap(px(4.0 * scale))
                    .bg(to_hsla(p.surface))
                    .border_1()
                    .border_color(border)
                    .rounded_b(px(8.0 * scale))
                    .rounded_tr(px(8.0 * scale))
                    .overflow_hidden()
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(status_indicator(shape, cx))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_size(px((11.5 * scale).clamp(9.5, 12.5)))
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(SharedString::from(node.title.clone())),
                            ),
                    )
                    .child(
                        div()
                            .text_size(px(10.0))
                            .text_color(to_hsla(p.muted))
                            .child(SharedString::from(ch.graph_group_progress(done, total))),
                    ),
            )
            .into_any_element();
    }

    v_col()
        .absolute()
        .left(px(pnode.x * scale))
        .top(px(pnode.y * scale))
        .w(px(pnode.w * scale))
        .h(px(pnode.h * scale))
        .px(px(9.0 * scale))
        .py(px(7.0 * scale))
        .gap(px(4.0 * scale))
        .rounded(px(8.0 * scale))
        .bg(to_hsla(p.surface))
        .border_1()
        .border_color(border)
        .overflow_hidden()
        .child(
            h_row()
                .gap(px(6.0))
                .child(status_indicator(shape, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_size(px((11.5 * scale).clamp(9.5, 12.5)))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(SharedString::from(node.title.clone())),
                ),
        )
        .child(
            h_row()
                .gap(px(4.0))
                .child(badge(graph_kind_label(node.kind, cx), Tone::Neutral, cx))
                .children(
                    node.role
                        .map(|r| badge(subagent_role_label(r, cx), Tone::Accent, cx)),
                ),
        )
        .into_any_element()
}

fn edge_color(tone: GraphEdgeTone, p: &spanbay_desktop::theme::ResolvedPalette) -> gpui_kit::Hsla {
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
    p: &spanbay_desktop::theme::ResolvedPalette,
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
