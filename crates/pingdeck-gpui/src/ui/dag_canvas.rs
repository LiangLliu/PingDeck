//! Reusable GPU-accelerated layered DAG edge & port renderer inside the `src/ui/` boundary.
//!
//! Renders GitHub-Actions-style smooth cubic Bézier dependency connectors (both adjacent-stage
//! curves and multi-stage corridor waypoints) plus crisp port anchor dots via GPUI's native
//! [`gpui_kit::canvas`] and [`gpui_kit::PathBuilder`] primitives. Shared by Chat's Execution Graph
//! (`执行图`) and reusable by Plan task-graph previews and future HTTP Flow DAG views.

use gpui_kit::{
    AnyElement, Bounds, Hsla, IntoElement, PathBuilder, Pixels, Point, Styled as _, canvas, fill,
    point, px, size,
};

/// One visual connector path in unscaled logical DAG coordinates.
#[derive(Debug, Clone)]
pub struct DagEdgeDraw {
    /// Ordered anchor coordinates: `[start, end]` for adjacent stages or `[start, w1, w2, end]`
    /// for multi-stage skip corridors.
    pub points: Vec<(f32, f32)>,
    pub color: Hsla,
    pub stroke_width: f32,
    pub dashed: bool,
    /// When `true`, renders crisp port dots at the start and end anchors.
    pub draw_ports: bool,
    pub port_color: Hsla,
}

/// Renders all dependency edges and port dots into an absolutely positioned full-canvas GPU layer.
#[must_use]
pub fn dag_edge_canvas(
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
                    let dx = (p1.x - p0.x).abs().max(px(24.0 * scale));
                    let ctrl_offset = dx * 0.54;
                    let c1 = point(p0.x + ctrl_offset, p0.y);
                    let c2 = point(p1.x - ctrl_offset, p1.y);
                    pb.move_to(p0);
                    pb.cubic_bezier_to(p1, c1, c2);
                } else {
                    // Multi-stage skip corridor: smooth cubic drop into the corridor, clean
                    // horizontal travel across intermediate columns, smooth rise into target port.
                    let pts: Vec<Point<Pixels>> =
                        edge.points.iter().map(|(x, y)| to_pt(*x, *y)).collect();
                    pb.move_to(pts[0]);
                    let first_corner = pts[1];
                    let drop_c1 = point(first_corner.x, pts[0].y);
                    let drop_c2 =
                        point(pts[0].x + (first_corner.x - pts[0].x) * 0.4, first_corner.y);
                    pb.cubic_bezier_to(first_corner, drop_c1, drop_c2);

                    for seg in pts.windows(2).skip(1).take(pts.len().saturating_sub(3)) {
                        pb.line_to(seg[1]);
                    }

                    let prev_corner = pts[pts.len() - 2];
                    let last = pts[pts.len() - 1];
                    let rise_c1 = point(last.x - (last.x - prev_corner.x) * 0.4, prev_corner.y);
                    let rise_c2 = point(prev_corner.x, last.y);
                    pb.cubic_bezier_to(last, rise_c1, rise_c2);
                }

                if let Ok(path) = pb.build() {
                    window.paint_path(path, edge.color);
                }

                if edge.draw_ports {
                    let dot_d = (5.5 * scale).clamp(4.0, 7.0);
                    let dot_r = dot_d * 0.5;
                    let p_start = to_pt(edge.points[0].0, edge.points[0].1);
                    let p_end = to_pt(
                        edge.points[edge.points.len() - 1].0,
                        edge.points[edge.points.len() - 1].1,
                    );
                    for center in [p_start, p_end] {
                        let quad = fill(
                            Bounds::centered_at(center, size(px(dot_d), px(dot_d))),
                            edge.port_color,
                        )
                        .corner_radii(px(dot_r));
                        window.paint_quad(quad);
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
