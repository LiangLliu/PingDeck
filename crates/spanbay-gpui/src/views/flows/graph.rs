//! Unified Process Graph (`执行图`) canvas renderer for `FlowsView`.
//!
//! Shares the exact same Layer 3 GPU vector canvas (`process_stage_canvas`) and visual grammar as
//! Agent's `WorkflowMonitorModal`, while adding Flow-specific authoring ergonomics:
//! - Hoverable `+` insertion pills at control-edge midpoints (zero cumbersome port-to-port wiring)
//! - Automatic `{{var}}` data-flow dependency arc illumination on node selection
//! - Hybrid manual drag-to-reposition with 8px snapping, alignment guides, and cross-lane drop
//!   target highlights.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, MouseButton, MouseDownEvent,
    MouseMoveEvent, ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _,
    div, px,
};
use spanbay_desktop::state::{
    DataFlowVisibility, FlowGraphLayout, GuideOrientation, ProcessEdgeLayer, ProcessEdgeTone,
    ProcessGraphGeometry, hit_test_lane, snap_drag_position,
};

use crate::ui::{
    DagEdgeDraw, ProcessGuideDraw, ProcessLaneDraw, ScrollableElement as _, UiIcon, empty_inline,
    h_row, icon_xs, lane_draw_spec, palette, process_edge_label_pill, process_lane_header,
    process_stage_canvas, text_tooltip, to_hsla, v_col,
};

use super::graph_cards::render_flow_graph_card;
use super::state::{FlowDragState, FlowModal, FlowsView};

const DRAG_ACTIVATION_PX: f32 = 4.0;

impl FlowsView {
    pub(super) fn begin_step_drag(
        &mut self,
        step_id: String,
        pos: (f32, f32),
        size: (f32, f32),
        scale: f32,
        ev: &MouseDownEvent,
    ) {
        self.drag_suppress_click = false;
        self.drag_state = Some(FlowDragState {
            step_id,
            start_mouse: (f32::from(ev.position.x), f32::from(ev.position.y)),
            start_pos: pos,
            size,
            scale: scale.clamp(0.5, 1.5),
            dragging: false,
            hover_lane_id: None,
        });
    }

    pub(super) fn update_step_drag(
        &mut self,
        ev: &MouseMoveEvent,
        geometry: &ProcessGraphGeometry,
        cx: &mut Context<Self>,
    ) {
        let Some(drag) = &mut self.drag_state else {
            return;
        };
        let mx = f32::from(ev.position.x);
        let my = f32::from(ev.position.y);
        let sdx = mx - drag.start_mouse.0;
        let sdy = my - drag.start_mouse.1;
        if !drag.dragging && sdx.hypot(sdy) < DRAG_ACTIVATION_PX {
            return;
        }
        drag.dragging = true;
        let raw_x = (drag.start_pos.0 + sdx / drag.scale).max(20.0);
        let raw_y = (drag.start_pos.1 + sdy / drag.scale).max(24.0);
        let (sx, sy, guides) = snap_drag_position(
            &drag.step_id,
            raw_x,
            raw_y,
            drag.size.0,
            drag.size.1,
            &geometry.nodes,
        );
        let center_x = sx + drag.size.0 * 0.5;
        let center_y = sy + drag.size.1 * 0.5;
        drag.hover_lane_id = hit_test_lane(geometry, center_x, center_y);

        if let Some(flow) = self.draft.as_mut() {
            flow.options
                .pinned_positions
                .insert(drag.step_id.clone(), (sx.round() as i32, sy.round() as i32));
            self.dirty = true;
        }
        self.alignment_guides = guides;
        cx.notify();
    }

    pub(super) fn end_step_drag(&mut self, cx: &mut Context<Self>) {
        let Some(drag) = self.drag_state.take() else {
            return;
        };
        if drag.dragging {
            self.drag_suppress_click = true;
            if let Some(target_lane) = drag.hover_lane_id
                && let Some(flow) = self.draft.as_mut()
                && flow.options.layout.swimlanes
                && let Some(step) = flow.steps.iter_mut().find(|s| s.id == drag.step_id)
                && step.lane.as_deref() != Some(target_lane.as_str())
            {
                step.lane = Some(target_lane);
                flow.options.pinned_positions.remove(&drag.step_id);
                self.dirty = true;
            }
        }
        self.alignment_guides.clear();
        cx.notify();
    }
}

pub fn render_flow_graph(
    view: &FlowsView,
    layout: &FlowGraphLayout,
    cx: &mut Context<FlowsView>,
) -> AnyElement {
    let p = palette(cx);
    let fl = crate::ui::Tr(view.lang).flows();
    if layout.nodes.is_empty() {
        return div()
            .size_full()
            .flex()
            .items_center()
            .justify_center()
            .bg(to_hsla(p.sunken))
            .child(empty_inline(
                fl.steps_pane_no_steps_in_this_flow(),
                Some(SharedString::from(fl.steps_pane_add_an_inline_http_step())),
                None,
                cx,
            ))
            .into_any_element();
    }

    let scale = view.graph_zoom.clamp(0.75, 1.15);
    let focus_id = view.selected_step_id.as_deref();
    let hover_lane = view
        .drag_state
        .as_ref()
        .filter(|d| d.dragging)
        .and_then(|d| d.hover_lane_id.as_deref());

    let mut edges: Vec<DagEdgeDraw> = Vec::with_capacity(layout.geometry.edges.len());
    let mut visible_labels: Vec<(String, (f32, f32), ProcessEdgeTone)> = Vec::new();
    let mut insert_handles: Vec<(String, String, (f32, f32))> = Vec::new();

    for edge in &layout.geometry.edges {
        let incident = focus_id.is_some_and(|sel| sel == edge.from_node || sel == edge.to_node);

        if edge.layer == ProcessEdgeLayer::DataFlow {
            let show = match layout.geometry.policy.data_flow {
                DataFlowVisibility::Hidden => false,
                DataFlowVisibility::OnFocus => incident,
                DataFlowVisibility::Always => true,
            };
            if !show {
                continue;
            }
            let mut color = to_hsla(p.accent);
            if !incident {
                color.a *= 0.42;
            }
            edges.push(DagEdgeDraw {
                points: edge.waypoints.clone(),
                color,
                stroke_width: if incident { 1.8 } else { 1.2 },
                dashed: true,
                horizontal_ports: edge.horizontal_ports,
                draw_ports: false,
                port_color: color,
                draw_arrow: true,
            });
            if incident && let (Some(lbl), Some(pos)) = (&edge.label, edge.label_pos) {
                visible_labels.push((lbl.clone(), pos, ProcessEdgeTone::Data));
            }
            continue;
        }

        let dimmed = focus_id.is_some() && !incident;
        let mut color = if incident {
            to_hsla(p.accent)
        } else {
            edge_tone_color(edge.tone, &p)
        };
        if dimmed {
            color.a *= 0.35;
        }
        edges.push(DagEdgeDraw {
            points: edge.waypoints.clone(),
            color,
            stroke_width: if incident { 2.3 } else { 1.55 },
            dashed: edge.dashed,
            horizontal_ports: edge.horizontal_ports,
            draw_ports: true,
            port_color: color,
            draw_arrow: true,
        });
        if let (Some(lbl), Some(pos)) = (&edge.label, edge.label_pos) {
            visible_labels.push((lbl.clone(), pos, edge.tone));
        }
        if let Some(pos) = edge.insert_pos {
            insert_handles.push((edge.from_node.clone(), edge.to_node.clone(), pos));
        }
    }

    let lane_draws: Vec<ProcessLaneDraw> = layout
        .geometry
        .lanes
        .iter()
        .map(|band| lane_draw_spec(band, hover_lane == Some(band.id.as_str()), cx))
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

    let geom_for_drag = layout.geometry.clone();
    let canvas_w = (layout.geometry.canvas_width * scale).max(560.0);
    let canvas_h = (layout.geometry.canvas_height * scale).max(320.0);

    let mut stage_surface = div()
        .id("flow-graph-stage-surface")
        .relative()
        .w(px(canvas_w))
        .h(px(canvas_h))
        .flex_shrink_0()
        .on_mouse_move(cx.listener(move |this, ev: &MouseMoveEvent, _, cx| {
            if this.drag_state.is_some() {
                this.update_step_drag(ev, &geom_for_drag, cx);
            }
        }))
        .on_mouse_up(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| this.end_step_drag(cx)),
        )
        .on_mouse_up_out(
            MouseButton::Left,
            cx.listener(|this, _, _, cx| this.end_step_drag(cx)),
        )
        .on_click(cx.listener(|this, _, _, cx| {
            if this.drag_suppress_click {
                this.drag_suppress_click = false;
                return;
            }
            if this.inspector_open || this.selected_step_id.is_some() {
                this.select_step(None, cx);
            }
        }))
        .child(process_stage_canvas(
            lane_draws,
            guide_draws,
            edges,
            layout.geometry.canvas_width,
            layout.geometry.canvas_height,
            scale,
        ));

    for band in &layout.geometry.lanes {
        stage_surface = stage_surface.child(process_lane_header(band, scale, cx));
    }

    for stage in &layout.geometry.stages {
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
                            fl.graph_stage_label(stage.stage_index + 1),
                        )),
                )
                .children((stage.has_parallel && !stage.vertical).then(|| {
                    div()
                        .text_color(to_hsla(p.faint))
                        .child(icon_xs(UiIcon::Branch))
                })),
        );
    }

    for (lbl, pos, tone) in visible_labels {
        stage_surface = stage_surface.child(process_edge_label_pill(&lbl, pos, tone, scale, cx));
    }

    // Hoverable `+` insertion pills on control edges (fast authoring without manual wiring).
    let btn_d = (18.0 * scale).clamp(15.0, 20.0);
    let btn_bg = to_hsla(p.surface);
    let btn_border = to_hsla(p.line_strong.composite_over(p.surface));
    let btn_hover_bg = to_hsla(p.accent_soft.composite_over(p.surface));
    let btn_hover_border = to_hsla(p.accent);
    for (idx, (from_id, to_id, pos)) in insert_handles.into_iter().enumerate() {
        stage_surface = stage_surface.child(
            div()
                .id(SharedString::from(format!("flow-edge-ins-{idx}")))
                .absolute()
                .left(px(pos.0 * scale - btn_d * 0.5))
                .top(px(pos.1 * scale - btn_d * 0.5))
                .w(px(btn_d))
                .h(px(btn_d))
                .flex()
                .items_center()
                .justify_center()
                .rounded_full()
                .bg(btn_bg)
                .border_1()
                .border_color(btn_border)
                .text_color(to_hsla(p.muted))
                .cursor_pointer()
                .hover(move |s| {
                    s.bg(btn_hover_bg)
                        .border_color(btn_hover_border)
                        .text_color(btn_hover_border)
                })
                .tooltip(text_tooltip(fl.graph_insert_between()))
                .on_mouse_down(MouseButton::Left, |_, _, cx| cx.stop_propagation())
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.active_modal = Some(FlowModal::InsertOnEdge {
                        from_step_id: from_id.clone(),
                        to_step_id: to_id.clone(),
                    });
                    cx.notify();
                }))
                .child(icon_xs(UiIcon::Plus)),
        );
    }

    for node in &layout.nodes {
        stage_surface = stage_surface.child(render_flow_graph_card(view, node, scale, cx));
    }

    v_col()
        .size_full()
        .bg(to_hsla(p.sunken))
        .child(
            div()
                .id("flow-graph-scroll")
                .flex_1()
                .min_h_0()
                .w_full()
                .overflow_scrollbar()
                .child(stage_surface),
        )
        .into_any_element()
}

fn edge_tone_color(
    tone: ProcessEdgeTone,
    p: &spanbay_desktop::theme::ResolvedPalette,
) -> gpui_kit::Hsla {
    match tone {
        ProcessEdgeTone::Active | ProcessEdgeTone::Data => to_hsla(p.accent),
        ProcessEdgeTone::Completed => to_hsla(p.ok),
        ProcessEdgeTone::Warning => to_hsla(p.warn),
        ProcessEdgeTone::Danger => to_hsla(p.bad),
        ProcessEdgeTone::Skipped => to_hsla(p.line.composite_over(p.sunken)),
        ProcessEdgeTone::Neutral => to_hsla(p.line_strong.composite_over(p.sunken)),
    }
}
