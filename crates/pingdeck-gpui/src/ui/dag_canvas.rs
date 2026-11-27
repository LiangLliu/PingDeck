//! Reusable GPU-accelerated Process Graph vector canvas inside the `src/ui/` boundary.
//!
//! Paints:
//! - Optional swimlane backdrop bands (horizontal or vertical) with subtle semantic accent bars
//!   and live drag drop-target highlights
//! - Smooth cubic Bézier dependency connectors across `LeftToRight`, `TopToBottom`, `Serpentine`
//!   turnaround corridors, multi-stage skip corridors, and secondary `DataFlow` arcs
//! - Crisp port anchor dots and directional terminal arrowheads
//! - Live snapping alignment guide lines while dragging nodes
//!
//! Shared by Agent Execution Graphs, Plan Task Graph previews, and HTTP Request Flows.

use gpui_kit::{
    AnyElement, Bounds, Hsla, IntoElement, PathBuilder, Pixels, Point, Styled as _, canvas, fill,
    point, px, size,
};

/// One visual connector path in unscaled logical Process Graph coordinates.
#[derive(Debug, Clone)]
pub struct DagEdgeDraw {
    /// Ordered anchor coordinates: `[start, end]` for adjacent stages or `[start, w1, w2, end]`
    /// for multi-stage / serpentine / data-flow corridors.
    pub points: Vec<(f32, f32)>,
    pub color: Hsla,
    pub stroke_width: f32,
    pub dashed: bool,
    /// `true` when ports emerge left/right (LR / Serpentine); `false` for top/bottom (TB).
    pub horizontal_ports: bool,
    /// When `true`, renders crisp port dots at the start and end anchors.
    pub draw_ports: bool,
    pub port_color: Hsla,
    /// When `true`, paints a compact directional chevron/arrow at the destination anchor.
    pub draw_arrow: bool,
}

/// One swimlane visual band painted beneath nodes and edges.
#[derive(Debug, Clone)]
pub struct ProcessLaneDraw {
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub header_w: f32,
    pub header_h: f32,
    pub horizontal: bool,
    pub fill_color: Hsla,
    pub header_fill: Hsla,
    pub border_color: Hsla,
    pub accent_color: Hsla,
    pub highlighted: bool,
}

/// One dashed alignment reference line rendered during manual node dragging.
#[derive(Debug, Clone)]
pub struct ProcessGuideDraw {
    pub horizontal: bool,
    pub coord: f32,
    pub span_start: f32,
    pub span_end: f32,
    pub color: Hsla,
}

/// Renders only dependency edges and port dots into an absolutely positioned full-canvas GPU layer.
#[must_use]
pub fn dag_edge_canvas(
    edges: Vec<DagEdgeDraw>,
    width_px: f32,
    height_px: f32,
    scale: f32,
) -> AnyElement {
    process_stage_canvas(Vec::new(), Vec::new(), edges, width_px, height_px, scale)
}

/// Renders swimlane bands, alignment guides, dependency curves, ports, and arrows in a single
/// GPU-accelerated full-canvas pass.
#[must_use]
pub fn process_stage_canvas(
    lanes: Vec<ProcessLaneDraw>,
    guides: Vec<ProcessGuideDraw>,
    edges: Vec<DagEdgeDraw>,
    width_px: f32,
    height_px: f32,
    scale: f32,
) -> AnyElement {
    let scale = scale.clamp(0.5, 1.5);
    let w = (width_px * scale).max(1.0);
    let h = (height_px * scale).max(1.0);

    canvas(
        |_bounds, _window, _cx| (),
        move |bounds, _, window, _cx| {
            let origin = bounds.origin;
            let to_pt = |x: f32, y: f32| -> Point<Pixels> {
                point(origin.x + px(x * scale), origin.y + px(y * scale))
            };

            // 1. Swimlane backdrop bands (beneath edges & cards).
            for lane in &lanes {
                let band_bounds = Bounds::new(
                    to_pt(lane.x, lane.y),
                    size(px(lane.w * scale), px(lane.h * scale)),
                );
                let radius = px(8.0 * scale);
                let border_w = if lane.highlighted { 1.5 } else { 1.0 };
                let border_c = if lane.highlighted {
                    lane.accent_color
                } else {
                    lane.border_color
                };
                window.paint_quad(
                    fill(band_bounds, lane.fill_color)
                        .corner_radii(radius)
                        .border_color(border_c)
                        .border_widths(px(border_w)),
                );

                let hdr_bounds = Bounds::new(
                    to_pt(lane.x, lane.y),
                    size(px(lane.header_w * scale), px(lane.header_h * scale)),
                );
                window.paint_quad(fill(hdr_bounds, lane.header_fill).corner_radii(radius));

                // Thin semantic accent bar along the lane header edge.
                let bar_bounds = if lane.horizontal {
                    Bounds::new(
                        to_pt(lane.x, lane.y + 6.0),
                        size(px(3.0 * scale), px((lane.h - 12.0).max(8.0) * scale)),
                    )
                } else {
                    Bounds::new(
                        to_pt(lane.x + 8.0, lane.y),
                        size(px((lane.w - 16.0).max(8.0) * scale), px(3.0 * scale)),
                    )
                };
                window.paint_quad(fill(bar_bounds, lane.accent_color).corner_radii(px(2.0)));
            }

            // 2. Live drag snapping alignment guides.
            for guide in &guides {
                let mut pb = PathBuilder::stroke(px(1.1));
                pb = pb.dash_array(&[px(4.0), px(4.0)]);
                if guide.horizontal {
                    pb.move_to(to_pt(guide.span_start, guide.coord));
                    pb.line_to(to_pt(guide.span_end, guide.coord));
                } else {
                    pb.move_to(to_pt(guide.coord, guide.span_start));
                    pb.line_to(to_pt(guide.coord, guide.span_end));
                }
                if let Ok(path) = pb.build() {
                    window.paint_path(path, guide.color);
                }
            }

            // 3. Dependency edges, port dots, and directional terminal arrowheads.
            for edge in &edges {
                if edge.points.len() < 2 {
                    continue;
                }
                let stroke = (edge.stroke_width * scale.sqrt()).max(1.1);
                let mut pb = PathBuilder::stroke(px(stroke));
                if edge.dashed {
                    let dash = (5.0 * scale).max(3.5);
                    let gap = (4.0 * scale).max(3.0);
                    pb = pb.dash_array(&[px(dash), px(gap)]);
                }

                if edge.points.len() == 2 {
                    let p0 = to_pt(edge.points[0].0, edge.points[0].1);
                    let p1 = to_pt(edge.points[1].0, edge.points[1].1);
                    pb.move_to(p0);
                    if edge.horizontal_ports {
                        let dx = (p1.x - p0.x).abs().max(px(26.0 * scale));
                        let ctrl = dx * 0.54;
                        pb.cubic_bezier_to(p1, point(p0.x + ctrl, p0.y), point(p1.x - ctrl, p1.y));
                    } else {
                        let dy = (p1.y - p0.y).abs().max(px(24.0 * scale));
                        let ctrl = dy * 0.54;
                        pb.cubic_bezier_to(p1, point(p0.x, p0.y + ctrl), point(p1.x, p1.y - ctrl));
                    }
                } else {
                    let pts: Vec<Point<Pixels>> =
                        edge.points.iter().map(|(x, y)| to_pt(*x, *y)).collect();
                    pb.move_to(pts[0]);
                    let first = pts[1];
                    if edge.horizontal_ports {
                        let c1 = point(first.x, pts[0].y);
                        let c2 = point(pts[0].x + (first.x - pts[0].x) * 0.4, first.y);
                        pb.cubic_bezier_to(first, c1, c2);
                    } else {
                        let c1 = point(pts[0].x, first.y);
                        let c2 = point(first.x, pts[0].y + (first.y - pts[0].y) * 0.4);
                        pb.cubic_bezier_to(first, c1, c2);
                    }

                    for seg in pts.windows(2).skip(1).take(pts.len().saturating_sub(3)) {
                        pb.line_to(seg[1]);
                    }

                    let prev = pts[pts.len() - 2];
                    let last = pts[pts.len() - 1];
                    if edge.horizontal_ports {
                        let c1 = point(last.x - (last.x - prev.x) * 0.4, prev.y);
                        let c2 = point(prev.x, last.y);
                        pb.cubic_bezier_to(last, c1, c2);
                    } else {
                        let c1 = point(prev.x, last.y - (last.y - prev.y) * 0.4);
                        let c2 = point(last.x, prev.y);
                        pb.cubic_bezier_to(last, c1, c2);
                    }
                }

                if let Ok(path) = pb.build() {
                    window.paint_path(path, edge.color);
                }

                let p_start = to_pt(edge.points[0].0, edge.points[0].1);
                let p_end = to_pt(
                    edge.points[edge.points.len() - 1].0,
                    edge.points[edge.points.len() - 1].1,
                );

                if edge.draw_ports {
                    let dot_d = (5.2 * scale).clamp(3.8, 6.8);
                    let dot_r = dot_d * 0.5;
                    let quad = fill(
                        Bounds::centered_at(p_start, size(px(dot_d), px(dot_d))),
                        edge.port_color,
                    )
                    .corner_radii(px(dot_r));
                    window.paint_quad(quad);
                    if !edge.draw_arrow {
                        let end_quad = fill(
                            Bounds::centered_at(p_end, size(px(dot_d), px(dot_d))),
                            edge.port_color,
                        )
                        .corner_radii(px(dot_r));
                        window.paint_quad(end_quad);
                    }
                }

                if edge.draw_arrow {
                    let a_len = px((5.5 * scale).clamp(4.0, 7.0));
                    let a_half = px((3.2 * scale).clamp(2.4, 4.2));
                    let mut ab = PathBuilder::stroke(px((stroke * 1.05).max(1.25)));
                    if edge.horizontal_ports {
                        ab.move_to(point(p_end.x - a_len, p_end.y - a_half));
                        ab.line_to(p_end);
                        ab.line_to(point(p_end.x - a_len, p_end.y + a_half));
                    } else {
                        ab.move_to(point(p_end.x - a_half, p_end.y - a_len));
                        ab.line_to(p_end);
                        ab.line_to(point(p_end.x + a_half, p_end.y - a_len));
                    }
                    if let Ok(arrow_path) = ab.build() {
                        window.paint_path(arrow_path, edge.port_color);
                    }
                }
            }
        },
    )
    .absolute()
    .top_0()
    .left_0()
    .w(px(w))
    .h(px(h))
    .into_any_element()
}
