//! Pluggable multi-direction, alignment, swimlane, and hybrid-pinned layout & edge routing engine.
//!
//! Pure Layer 2 geometry kernel with zero UI framework dependencies:
//! - Kahn longest-path layering over `Control` edges (graceful cycle fallback)
//! - Barycenter sibling ordering to minimize crossings
//! - Heterogeneous node sizing (per-column max width / per-row max height)
//! - Multi-port distribution on input/output card sides (conditional multi-exit branches)
//! - Layout modes: `LeftToRight`, `TopToBottom`, `Serpentine`, optional `Swimlanes`
//! - Cross-axis alignment: `Start`, `Center`, `Barycenter`
//! - Hybrid manual drag overrides (`pinned_pos`) + alignment guide snapping + lane hit testing

use std::collections::{HashMap, VecDeque};

use super::model::{
    CrossAlignment, DataFlowVisibility, LayoutDirection, LayoutPolicy, PositionedLaneBand,
    PositionedPort, PositionedProcessNode, PositionedStageTrack, ProcessEdgeLayer, ProcessEdgeSpec,
    ProcessEdgeTone, ProcessGraphGeometry, ProcessGraphInput, ProcessLaneSpec, ProcessNodeSpec,
    ProcessPortDirection, ProcessPortSpec, RoutedProcessEdge,
};

pub const PROCESS_PAD_X: f32 = 28.0;
pub const PROCESS_PAD_Y: f32 = 44.0;
pub const PROCESS_PAD_BOTTOM: f32 = 36.0;
pub const PROCESS_STAGE_GAP_X: f32 = 72.0;
pub const PROCESS_STAGE_GAP_Y: f32 = 56.0;
pub const PROCESS_NODE_GAP_X: f32 = 24.0;
pub const PROCESS_NODE_GAP_Y: f32 = 16.0;
pub const PROCESS_LANE_HEADER_W: f32 = 136.0;
pub const PROCESS_LANE_HEADER_H: f32 = 36.0;
pub const PROCESS_LANE_PAD: f32 = 16.0;
pub const PROCESS_LANE_GAP: f32 = 10.0;
const EMPTY_LANE_THICKNESS: f32 = 58.0;
const STAGE_LEFT_GUTTER: f32 = 78.0;
const SERPENTINE_ROW_GAP: f32 = 64.0;
const LONG_EDGE_LANE_STEP: f32 = 18.0;
const DATA_EDGE_LANE_STEP: f32 = 14.0;
const SNAP_GRID_PX: f32 = 8.0;
const SNAP_GUIDE_THRESHOLD: f32 = 7.0;

/// Orientation of a live drag alignment guide line painted on the canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GuideOrientation {
    Horizontal,
    Vertical,
}

/// One live snapping reference line rendered while dragging a node.
#[derive(Debug, Clone, PartialEq)]
pub struct AlignmentGuide {
    pub orientation: GuideOrientation,
    /// Fixed Y (for horizontal) or fixed X (for vertical) coordinate in logical canvas space.
    pub coord: f32,
    pub span_start: f32,
    pub span_end: f32,
}

/// Computes deterministic positions, swimlane bands, ports, and routed Bézier edges for `input`.
#[must_use]
pub fn layout_process_graph(input: &ProcessGraphInput) -> ProcessGraphGeometry {
    let policy = input.policy;
    if input.nodes.is_empty() {
        return ProcessGraphGeometry {
            policy,
            lanes: Vec::new(),
            stages: Vec::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            canvas_width: 0.0,
            canvas_height: 0.0,
            has_pinned_nodes: false,
            node_index: HashMap::new(),
        };
    }

    let id_to_idx: HashMap<String, usize> = input
        .nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.clone(), i))
        .collect();

    // 1. Topological stage assignment using ONLY Control edges.
    let stage_of = assign_stages(&input.nodes, &input.edges, &id_to_idx);
    let max_stage = stage_of.iter().copied().max().unwrap_or(0);
    let mut columns: Vec<Vec<usize>> = vec![Vec::new(); max_stage + 1];
    for (idx, &st) in stage_of.iter().enumerate() {
        columns[st].push(idx);
    }

    // 2. Upstream barycenter ordering within each stage to minimize edge crossings.
    let upstream_map = build_upstream_control_map(&input.edges, &id_to_idx);
    let mut row_pos: HashMap<usize, f32> = HashMap::new();
    for (col_idx, col) in columns.iter_mut().enumerate() {
        if col_idx > 0 && col.len() > 1 {
            col.sort_by(|&a, &b| {
                let ba = stage_barycenter(a, &upstream_map, &row_pos);
                let bb = stage_barycenter(b, &upstream_map, &row_pos);
                ba.total_cmp(&bb).then_with(|| a.cmp(&b))
            });
        }
        for (pos, &node_idx) in col.iter().enumerate() {
            row_pos.insert(node_idx, pos as f32);
        }
    }

    // 3. Place nodes according to policy (Swimlanes vs Serpentine vs LR vs TB).
    let use_lanes = policy.swimlanes && !input.lanes.is_empty();
    let horizontal_ports = !policy.direction.is_vertical();
    let (mut placed, lanes, stages, mut bounds, serpentine_meta) = if use_lanes {
        if policy.direction.is_vertical() {
            place_vertical_swimlanes(input, &columns)
        } else {
            place_horizontal_swimlanes(input, &columns)
        }
    } else {
        match policy.direction {
            LayoutDirection::LeftToRight => place_lr(input, &columns, &upstream_map),
            LayoutDirection::TopToBottom => place_tb(input, &columns, &upstream_map),
            LayoutDirection::Serpentine { max_per_row } => {
                let cols = max_per_row.max(2);
                place_serpentine(input, &columns, cols)
            }
        }
    };

    // 4. Apply hybrid manual pinned positions and compute concrete multi-port anchors.
    let mut has_pinned_nodes = false;
    let min_pin_x = if use_lanes && !policy.direction.is_vertical() {
        PROCESS_PAD_X + PROCESS_LANE_HEADER_W + 8.0
    } else if policy.direction.is_vertical() && !use_lanes {
        STAGE_LEFT_GUTTER + 8.0
    } else {
        PROCESS_PAD_X
    };
    let min_pin_y = if use_lanes && policy.direction.is_vertical() {
        PROCESS_PAD_Y + PROCESS_LANE_HEADER_H + 8.0
    } else {
        PROCESS_PAD_Y
    };

    for (idx, pnode) in placed.iter_mut().enumerate() {
        let spec = &input.nodes[idx];
        if let Some((px, py)) = spec.pinned_pos {
            pnode.x = px.max(min_pin_x);
            pnode.y = py.max(min_pin_y);
            pnode.pinned = true;
            has_pinned_nodes = true;
        }
        let (ports, port_in, port_out) = compute_node_ports(spec, pnode, horizontal_ports);
        pnode.ports = ports;
        pnode.port_in = port_in;
        pnode.port_out = port_out;
        bounds.max_x = bounds.max_x.max(pnode.x + pnode.w);
        bounds.max_y = bounds.max_y.max(pnode.y + pnode.h);
    }

    let node_index: HashMap<String, usize> = placed
        .iter()
        .enumerate()
        .map(|(i, p)| (p.id.clone(), i))
        .collect();

    // 5. Route Control and DataFlow edges cleanly around intermediate obstacles.
    let (edges, edge_max_x, edge_max_y) = route_edges(
        &input.edges,
        &mut placed,
        &node_index,
        policy,
        use_lanes,
        horizontal_ports,
        &bounds,
        serpentine_meta.as_ref(),
    );

    let canvas_width = bounds.max_x.max(edge_max_x) + PROCESS_PAD_X;
    let canvas_height = bounds.max_y.max(edge_max_y) + PROCESS_PAD_BOTTOM;

    ProcessGraphGeometry {
        policy,
        lanes,
        stages,
        nodes: placed,
        edges,
        canvas_width,
        canvas_height,
        has_pinned_nodes,
        node_index,
    }
}

/// Returns the lane ID whose visual band contains logical coordinate `(x, y)`.
#[must_use]
pub fn hit_test_lane(geometry: &ProcessGraphGeometry, x: f32, y: f32) -> Option<String> {
    geometry
        .lanes
        .iter()
        .find(|band| x >= band.x && x <= band.x + band.w && y >= band.y && y <= band.y + band.h)
        .map(|band| band.id.clone())
}

/// Applies gentle 8px grid snap and neighbor edge/center alignment guides during node dragging.
#[must_use]
pub fn snap_drag_position(
    dragged_id: &str,
    raw_x: f32,
    raw_y: f32,
    dragged_w: f32,
    dragged_h: f32,
    nodes: &[PositionedProcessNode],
) -> (f32, f32, Vec<AlignmentGuide>) {
    let mut x = (raw_x / SNAP_GRID_PX).round() * SNAP_GRID_PX;
    let mut y = (raw_y / SNAP_GRID_PX).round() * SNAP_GRID_PX;
    let mut best_dx = SNAP_GUIDE_THRESHOLD + 1.0;
    let mut best_dy = SNAP_GUIDE_THRESHOLD + 1.0;
    let mut v_guide: Option<AlignmentGuide> = None;
    let mut h_guide: Option<AlignmentGuide> = None;

    let drag_cx = raw_x + dragged_w * 0.5;
    let drag_cy = raw_y + dragged_h * 0.5;

    for other in nodes {
        if other.id == dragged_id {
            continue;
        }
        // Horizontal alignment checks (matching top edges or vertical centers).
        let top_dy = (raw_y - other.y).abs();
        if top_dy < best_dy {
            best_dy = top_dy;
            y = other.y;
            h_guide = Some(AlignmentGuide {
                orientation: GuideOrientation::Horizontal,
                coord: other.y,
                span_start: raw_x.min(other.x) - 16.0,
                span_end: (raw_x + dragged_w).max(other.x + other.w) + 16.0,
            });
        }
        let other_cy = other.y + other.h * 0.5;
        let cy_dy = (drag_cy - other_cy).abs();
        if cy_dy < best_dy {
            best_dy = cy_dy;
            y = other_cy - dragged_h * 0.5;
            h_guide = Some(AlignmentGuide {
                orientation: GuideOrientation::Horizontal,
                coord: other_cy,
                span_start: raw_x.min(other.x) - 16.0,
                span_end: (raw_x + dragged_w).max(other.x + other.w) + 16.0,
            });
        }

        // Vertical alignment checks (matching left edges or horizontal centers).
        let left_dx = (raw_x - other.x).abs();
        if left_dx < best_dx {
            best_dx = left_dx;
            x = other.x;
            v_guide = Some(AlignmentGuide {
                orientation: GuideOrientation::Vertical,
                coord: other.x,
                span_start: raw_y.min(other.y) - 16.0,
                span_end: (raw_y + dragged_h).max(other.y + other.h) + 16.0,
            });
        }
        let other_cx = other.x + other.w * 0.5;
        let cx_dx = (drag_cx - other_cx).abs();
        if cx_dx < best_dx {
            best_dx = cx_dx;
            x = other_cx - dragged_w * 0.5;
            v_guide = Some(AlignmentGuide {
                orientation: GuideOrientation::Vertical,
                coord: other_cx,
                span_start: raw_y.min(other.y) - 16.0,
                span_end: (raw_y + dragged_h).max(other.y + other.h) + 16.0,
            });
        }
    }

    let mut guides = Vec::new();
    if let Some(g) = h_guide {
        guides.push(g);
    }
    if let Some(g) = v_guide {
        guides.push(g);
    }
    (x, y, guides)
}

#[derive(Debug, Default)]
struct PlacementBounds {
    max_x: f32,
    max_y: f32,
    stage_extents: Vec<f32>,
}

struct SerpentineMeta {
    max_per_row: usize,
    row_bottoms: Vec<f32>,
    row_tops: Vec<f32>,
}

fn assign_stages(
    nodes: &[ProcessNodeSpec],
    edges: &[ProcessEdgeSpec],
    id_to_idx: &HashMap<String, usize>,
) -> Vec<usize> {
    let n = nodes.len();
    let mut indeg = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for edge in edges {
        if edge.layer != ProcessEdgeLayer::Control {
            continue;
        }
        if let (Some(&u), Some(&v)) = (id_to_idx.get(&edge.from_node), id_to_idx.get(&edge.to_node))
            && u != v
        {
            adj[u].push(v);
            indeg[v] += 1;
        }
    }

    let mut stage = vec![0usize; n];
    for (idx, node) in nodes.iter().enumerate() {
        if let Some(hint) = node.stage_hint {
            stage[idx] = hint;
        }
    }

    let mut q: VecDeque<usize> = indeg
        .iter()
        .enumerate()
        .filter_map(|(i, &d)| (d == 0).then_some(i))
        .collect();
    while let Some(u) = q.pop_front() {
        let next_min = stage[u] + 1;
        for &v in &adj[u] {
            if stage[v] < next_min {
                stage[v] = next_min;
            }
            indeg[v] = indeg[v].saturating_sub(1);
            if indeg[v] == 0 {
                q.push_back(v);
            }
        }
    }
    stage
}

fn build_upstream_control_map(
    edges: &[ProcessEdgeSpec],
    id_to_idx: &HashMap<String, usize>,
) -> HashMap<usize, Vec<usize>> {
    let mut out: HashMap<usize, Vec<usize>> = HashMap::new();
    for edge in edges {
        if edge.layer != ProcessEdgeLayer::Control {
            continue;
        }
        if let (Some(&u), Some(&v)) = (id_to_idx.get(&edge.from_node), id_to_idx.get(&edge.to_node))
            && u != v
        {
            out.entry(v).or_default().push(u);
        }
    }
    out
}

fn stage_barycenter(
    node_idx: usize,
    upstream_map: &HashMap<usize, Vec<usize>>,
    row_pos: &HashMap<usize, f32>,
) -> f32 {
    let Some(ups) = upstream_map.get(&node_idx) else {
        return f32::MAX;
    };
    let mut sum = 0.0f32;
    let mut count = 0usize;
    for &u in ups {
        if let Some(&pos) = row_pos.get(&u) {
            sum += pos;
            count += 1;
        }
    }
    if count == 0 {
        f32::MAX
    } else {
        sum / count as f32
    }
}

fn blank_placed(spec: &ProcessNodeSpec, stage: usize) -> PositionedProcessNode {
    let (w, h) = (spec.size_hint.0.max(80.0), spec.size_hint.1.max(36.0));
    PositionedProcessNode {
        id: spec.id.clone(),
        stage,
        lane_id: spec.lane_id.clone(),
        x: 0.0,
        y: 0.0,
        w,
        h,
        pinned: false,
        ports: Vec::new(),
        port_in: (0.0, 0.0),
        port_out: (0.0, 0.0),
        has_in_edge: false,
        has_out_edge: false,
    }
}

fn place_lr(
    input: &ProcessGraphInput,
    columns: &[Vec<usize>],
    upstream_map: &HashMap<usize, Vec<usize>>,
) -> (
    Vec<PositionedProcessNode>,
    Vec<PositionedLaneBand>,
    Vec<PositionedStageTrack>,
    PlacementBounds,
    Option<SerpentineMeta>,
) {
    let mut placed: Vec<PositionedProcessNode> =
        input.nodes.iter().map(|s| blank_placed(s, 0)).collect();
    let col_widths: Vec<f32> = columns
        .iter()
        .map(|col| {
            col.iter()
                .map(|&i| input.nodes[i].size_hint.0.max(80.0))
                .fold(0.0f32, f32::max)
                .max(160.0)
        })
        .collect();
    let col_heights: Vec<f32> = columns
        .iter()
        .map(|col| {
            let sum_h: f32 = col
                .iter()
                .map(|&i| input.nodes[i].size_hint.1.max(36.0))
                .sum();
            sum_h + PROCESS_NODE_GAP_Y * col.len().saturating_sub(1) as f32
        })
        .collect();
    let max_col_h = col_heights.iter().copied().fold(0.0f32, f32::max);

    let mut stages = Vec::with_capacity(columns.len());
    let mut stage_extents = vec![PROCESS_PAD_Y; columns.len()];
    let mut cursor_x = PROCESS_PAD_X;
    let mut max_x = PROCESS_PAD_X;
    let mut max_y = PROCESS_PAD_Y;

    for (stage_idx, col) in columns.iter().enumerate() {
        let cw = col_widths[stage_idx];
        let has_parallel = col.len() >= 2 || col.iter().any(|&i| input.nodes[i].is_group_container);
        stages.push(PositionedStageTrack {
            stage_index: stage_idx,
            x: cursor_x,
            y: 12.0,
            w: cw,
            h: 20.0,
            node_count: col.len(),
            has_parallel,
            vertical: false,
        });

        let mut cursor_y = match input.policy.alignment {
            CrossAlignment::Start => PROCESS_PAD_Y,
            CrossAlignment::Center => {
                PROCESS_PAD_Y + ((max_col_h - col_heights[stage_idx]) * 0.5).max(0.0)
            }
            CrossAlignment::Barycenter => PROCESS_PAD_Y,
        };

        for &node_idx in col {
            let spec = &input.nodes[node_idx];
            let nw = spec.size_hint.0.max(80.0);
            let nh = spec.size_hint.1.max(36.0);
            if input.policy.alignment == CrossAlignment::Barycenter
                && stage_idx > 0
                && let Some(ups) = upstream_map.get(&node_idx)
                && !ups.is_empty()
            {
                let avg_cy: f32 = ups
                    .iter()
                    .map(|&u| placed[u].y + placed[u].h * 0.5)
                    .sum::<f32>()
                    / ups.len() as f32;
                let desired_y = (avg_cy - nh * 0.5).max(PROCESS_PAD_Y);
                if desired_y > cursor_y {
                    cursor_y = desired_y;
                }
            }
            let nx = cursor_x + (cw - nw) * 0.5;
            placed[node_idx].stage = stage_idx;
            placed[node_idx].x = nx;
            placed[node_idx].y = cursor_y;
            placed[node_idx].w = nw;
            placed[node_idx].h = nh;
            let bottom = cursor_y + nh;
            stage_extents[stage_idx] = stage_extents[stage_idx].max(bottom);
            max_y = max_y.max(bottom);
            cursor_y = bottom + PROCESS_NODE_GAP_Y;
        }

        max_x = max_x.max(cursor_x + cw);
        cursor_x += cw + PROCESS_STAGE_GAP_X;
    }

    (
        placed,
        Vec::new(),
        stages,
        PlacementBounds {
            max_x,
            max_y,
            stage_extents,
        },
        None,
    )
}

fn place_tb(
    input: &ProcessGraphInput,
    columns: &[Vec<usize>],
    upstream_map: &HashMap<usize, Vec<usize>>,
) -> (
    Vec<PositionedProcessNode>,
    Vec<PositionedLaneBand>,
    Vec<PositionedStageTrack>,
    PlacementBounds,
    Option<SerpentineMeta>,
) {
    let mut placed: Vec<PositionedProcessNode> =
        input.nodes.iter().map(|s| blank_placed(s, 0)).collect();
    let row_heights: Vec<f32> = columns
        .iter()
        .map(|row| {
            row.iter()
                .map(|&i| input.nodes[i].size_hint.1.max(36.0))
                .fold(0.0f32, f32::max)
                .max(48.0)
        })
        .collect();
    let row_widths: Vec<f32> = columns
        .iter()
        .map(|row| {
            let sum_w: f32 = row
                .iter()
                .map(|&i| input.nodes[i].size_hint.0.max(80.0))
                .sum();
            sum_w + PROCESS_NODE_GAP_X * row.len().saturating_sub(1) as f32
        })
        .collect();
    let max_row_w = row_widths.iter().copied().fold(0.0f32, f32::max);

    let start_x = PROCESS_PAD_X + STAGE_LEFT_GUTTER;
    let mut stages = Vec::with_capacity(columns.len());
    let mut stage_extents = vec![start_x; columns.len()];
    let mut cursor_y = PROCESS_PAD_Y;
    let mut max_x = start_x;
    let mut max_y = PROCESS_PAD_Y;

    for (stage_idx, row) in columns.iter().enumerate() {
        let rh = row_heights[stage_idx];
        let has_parallel = row.len() >= 2 || row.iter().any(|&i| input.nodes[i].is_group_container);
        stages.push(PositionedStageTrack {
            stage_index: stage_idx,
            x: PROCESS_PAD_X,
            y: cursor_y + (rh * 0.5 - 10.0).max(0.0),
            w: STAGE_LEFT_GUTTER - 12.0,
            h: 20.0,
            node_count: row.len(),
            has_parallel,
            vertical: true,
        });

        let mut cursor_x = match input.policy.alignment {
            CrossAlignment::Start => start_x,
            CrossAlignment::Center => {
                start_x + ((max_row_w - row_widths[stage_idx]) * 0.5).max(0.0)
            }
            CrossAlignment::Barycenter => start_x,
        };

        for &node_idx in row {
            let spec = &input.nodes[node_idx];
            let nw = spec.size_hint.0.max(80.0);
            let nh = spec.size_hint.1.max(36.0);
            if input.policy.alignment == CrossAlignment::Barycenter
                && stage_idx > 0
                && let Some(ups) = upstream_map.get(&node_idx)
                && !ups.is_empty()
            {
                let avg_cx: f32 = ups
                    .iter()
                    .map(|&u| placed[u].x + placed[u].w * 0.5)
                    .sum::<f32>()
                    / ups.len() as f32;
                let desired_x = (avg_cx - nw * 0.5).max(start_x);
                if desired_x > cursor_x {
                    cursor_x = desired_x;
                }
            }
            let ny = cursor_y + (rh - nh) * 0.5;
            placed[node_idx].stage = stage_idx;
            placed[node_idx].x = cursor_x;
            placed[node_idx].y = ny;
            placed[node_idx].w = nw;
            placed[node_idx].h = nh;
            let right = cursor_x + nw;
            stage_extents[stage_idx] = stage_extents[stage_idx].max(right);
            max_x = max_x.max(right);
            cursor_x = right + PROCESS_NODE_GAP_X;
        }

        max_y = max_y.max(cursor_y + rh);
        cursor_y += rh + PROCESS_STAGE_GAP_Y;
    }

    (
        placed,
        Vec::new(),
        stages,
        PlacementBounds {
            max_x,
            max_y,
            stage_extents,
        },
        None,
    )
}

fn place_serpentine(
    input: &ProcessGraphInput,
    columns: &[Vec<usize>],
    max_per_row: usize,
) -> (
    Vec<PositionedProcessNode>,
    Vec<PositionedLaneBand>,
    Vec<PositionedStageTrack>,
    PlacementBounds,
    Option<SerpentineMeta>,
) {
    let mut placed: Vec<PositionedProcessNode> =
        input.nodes.iter().map(|s| blank_placed(s, 0)).collect();
    let num_rows = columns.len().div_ceil(max_per_row);
    let mut slot_widths = vec![160.0f32; max_per_row];
    for (stage_idx, col) in columns.iter().enumerate() {
        let slot = stage_idx % max_per_row;
        let w = col
            .iter()
            .map(|&i| input.nodes[i].size_hint.0.max(80.0))
            .fold(0.0f32, f32::max);
        slot_widths[slot] = slot_widths[slot].max(w);
    }

    let mut slot_x = vec![PROCESS_PAD_X; max_per_row];
    for s in 1..max_per_row {
        slot_x[s] = slot_x[s - 1] + slot_widths[s - 1] + PROCESS_STAGE_GAP_X;
    }

    let mut row_heights = vec![0.0f32; num_rows];
    for (stage_idx, col) in columns.iter().enumerate() {
        let r = stage_idx / max_per_row;
        let h: f32 = col
            .iter()
            .map(|&i| input.nodes[i].size_hint.1.max(36.0))
            .sum::<f32>()
            + PROCESS_NODE_GAP_Y * col.len().saturating_sub(1) as f32;
        row_heights[r] = row_heights[r].max(h.max(64.0));
    }

    let mut row_tops = vec![PROCESS_PAD_Y; num_rows];
    let mut row_bottoms = vec![PROCESS_PAD_Y; num_rows];
    let mut cursor_y = PROCESS_PAD_Y;
    for r in 0..num_rows {
        row_tops[r] = cursor_y;
        row_bottoms[r] = cursor_y + row_heights[r];
        cursor_y = row_bottoms[r] + SERPENTINE_ROW_GAP;
    }

    let mut stages = Vec::with_capacity(columns.len());
    let mut stage_extents = vec![PROCESS_PAD_Y; columns.len()];
    let mut max_x = PROCESS_PAD_X;
    let mut max_y = PROCESS_PAD_Y;

    for (stage_idx, col) in columns.iter().enumerate() {
        let r = stage_idx / max_per_row;
        let slot = stage_idx % max_per_row;
        let sx = slot_x[slot];
        let sw = slot_widths[slot];
        let col_h: f32 = col
            .iter()
            .map(|&i| input.nodes[i].size_hint.1.max(36.0))
            .sum::<f32>()
            + PROCESS_NODE_GAP_Y * col.len().saturating_sub(1) as f32;
        let has_parallel = col.len() >= 2 || col.iter().any(|&i| input.nodes[i].is_group_container);

        stages.push(PositionedStageTrack {
            stage_index: stage_idx,
            x: sx,
            y: row_tops[r] - 24.0,
            w: sw,
            h: 18.0,
            node_count: col.len(),
            has_parallel,
            vertical: false,
        });

        let mut ny = match input.policy.alignment {
            CrossAlignment::Start => row_tops[r],
            CrossAlignment::Center | CrossAlignment::Barycenter => {
                row_tops[r] + ((row_heights[r] - col_h) * 0.5).max(0.0)
            }
        };

        for &node_idx in col {
            let spec = &input.nodes[node_idx];
            let nw = spec.size_hint.0.max(80.0);
            let nh = spec.size_hint.1.max(36.0);
            placed[node_idx].stage = stage_idx;
            placed[node_idx].x = sx + (sw - nw) * 0.5;
            placed[node_idx].y = ny;
            placed[node_idx].w = nw;
            placed[node_idx].h = nh;
            let bottom = ny + nh;
            stage_extents[stage_idx] = stage_extents[stage_idx].max(bottom);
            max_x = max_x.max(sx + sw);
            max_y = max_y.max(bottom);
            ny = bottom + PROCESS_NODE_GAP_Y;
        }
    }

    (
        placed,
        Vec::new(),
        stages,
        PlacementBounds {
            max_x,
            max_y,
            stage_extents,
        },
        Some(SerpentineMeta {
            max_per_row,
            row_bottoms,
            row_tops,
        }),
    )
}

fn resolve_lane_index(spec: &ProcessNodeSpec, lane_id_to_idx: &HashMap<&str, usize>) -> usize {
    spec.lane_id
        .as_deref()
        .and_then(|id| lane_id_to_idx.get(id).copied())
        .unwrap_or(0)
}

fn place_horizontal_swimlanes(
    input: &ProcessGraphInput,
    columns: &[Vec<usize>],
) -> (
    Vec<PositionedProcessNode>,
    Vec<PositionedLaneBand>,
    Vec<PositionedStageTrack>,
    PlacementBounds,
    Option<SerpentineMeta>,
) {
    let lanes: &[ProcessLaneSpec] = &input.lanes;
    let lane_id_to_idx: HashMap<&str, usize> = lanes
        .iter()
        .enumerate()
        .map(|(i, l)| (l.id.as_str(), i))
        .collect();
    let num_lanes = lanes.len();
    let num_stages = columns.len();

    let mut placed: Vec<PositionedProcessNode> =
        input.nodes.iter().map(|s| blank_placed(s, 0)).collect();

    // Group nodes into cells[lane][stage].
    let mut cells: Vec<Vec<Vec<usize>>> = vec![vec![Vec::new(); num_stages]; num_lanes];
    let mut lane_counts = vec![0usize; num_lanes];
    for (stage_idx, col) in columns.iter().enumerate() {
        for &node_idx in col {
            let li = resolve_lane_index(&input.nodes[node_idx], &lane_id_to_idx);
            cells[li][stage_idx].push(node_idx);
            lane_counts[li] += 1;
            placed[node_idx].lane_id = Some(lanes[li].id.clone());
        }
    }

    let col_widths: Vec<f32> = columns
        .iter()
        .map(|col| {
            col.iter()
                .map(|&i| input.nodes[i].size_hint.0.max(80.0))
                .fold(0.0f32, f32::max)
                .max(160.0)
        })
        .collect();

    let content_x0 = PROCESS_PAD_X + PROCESS_LANE_HEADER_W + PROCESS_LANE_PAD;
    let mut stage_x = Vec::with_capacity(num_stages);
    let mut cx = content_x0;
    for &w in &col_widths {
        stage_x.push(cx);
        cx += w + PROCESS_STAGE_GAP_X;
    }
    let total_band_w = (cx - PROCESS_STAGE_GAP_X + PROCESS_LANE_PAD - PROCESS_PAD_X)
        .max(PROCESS_LANE_HEADER_W + 260.0);

    let mut stages = Vec::with_capacity(num_stages);
    for (stage_idx, col) in columns.iter().enumerate() {
        let has_parallel = col.len() >= 2 || col.iter().any(|&i| input.nodes[i].is_group_container);
        stages.push(PositionedStageTrack {
            stage_index: stage_idx,
            x: stage_x[stage_idx],
            y: 12.0,
            w: col_widths[stage_idx],
            h: 20.0,
            node_count: col.len(),
            has_parallel,
            vertical: false,
        });
    }

    let mut lane_bands = Vec::with_capacity(num_lanes);
    let mut stage_extents = vec![PROCESS_PAD_Y; num_stages];
    let mut cursor_y = PROCESS_PAD_Y;

    for (li, lane) in lanes.iter().enumerate() {
        let max_cell_h = cells[li]
            .iter()
            .map(|cell| {
                let sum_h: f32 = cell
                    .iter()
                    .map(|&i| input.nodes[i].size_hint.1.max(36.0))
                    .sum();
                sum_h + PROCESS_NODE_GAP_Y * cell.len().saturating_sub(1) as f32
            })
            .fold(0.0f32, f32::max);
        let band_h = if lane_counts[li] == 0 {
            EMPTY_LANE_THICKNESS
        } else {
            (max_cell_h + PROCESS_LANE_PAD * 2.0).max(EMPTY_LANE_THICKNESS + 24.0)
        };

        lane_bands.push(PositionedLaneBand {
            id: lane.id.clone(),
            title: lane.title.clone(),
            accent_index: lane.accent_index,
            x: PROCESS_PAD_X,
            y: cursor_y,
            w: total_band_w,
            h: band_h,
            header_w: PROCESS_LANE_HEADER_W,
            header_h: band_h,
            node_count: lane_counts[li],
            horizontal: true,
        });

        for (stage_idx, cell) in cells[li].iter().enumerate() {
            if cell.is_empty() {
                continue;
            }
            let cell_h: f32 = cell
                .iter()
                .map(|&i| input.nodes[i].size_hint.1.max(36.0))
                .sum::<f32>()
                + PROCESS_NODE_GAP_Y * cell.len().saturating_sub(1) as f32;
            let mut ny = match input.policy.alignment {
                CrossAlignment::Start => cursor_y + PROCESS_LANE_PAD,
                CrossAlignment::Center | CrossAlignment::Barycenter => {
                    cursor_y + ((band_h - cell_h) * 0.5).max(PROCESS_LANE_PAD)
                }
            };
            let cw = col_widths[stage_idx];
            for &node_idx in cell {
                let spec = &input.nodes[node_idx];
                let nw = spec.size_hint.0.max(80.0);
                let nh = spec.size_hint.1.max(36.0);
                placed[node_idx].stage = stage_idx;
                placed[node_idx].x = stage_x[stage_idx] + (cw - nw) * 0.5;
                placed[node_idx].y = ny;
                placed[node_idx].w = nw;
                placed[node_idx].h = nh;
                ny += nh + PROCESS_NODE_GAP_Y;
            }
            stage_extents[stage_idx] = stage_extents[stage_idx].max(cursor_y + band_h);
        }

        cursor_y += band_h + PROCESS_LANE_GAP;
    }

    let max_x = PROCESS_PAD_X + total_band_w;
    let max_y = (cursor_y - PROCESS_LANE_GAP).max(PROCESS_PAD_Y);
    (
        placed,
        lane_bands,
        stages,
        PlacementBounds {
            max_x,
            max_y,
            stage_extents,
        },
        None,
    )
}

fn place_vertical_swimlanes(
    input: &ProcessGraphInput,
    columns: &[Vec<usize>],
) -> (
    Vec<PositionedProcessNode>,
    Vec<PositionedLaneBand>,
    Vec<PositionedStageTrack>,
    PlacementBounds,
    Option<SerpentineMeta>,
) {
    let lanes: &[ProcessLaneSpec] = &input.lanes;
    let lane_id_to_idx: HashMap<&str, usize> = lanes
        .iter()
        .enumerate()
        .map(|(i, l)| (l.id.as_str(), i))
        .collect();
    let num_lanes = lanes.len();
    let num_stages = columns.len();

    let mut placed: Vec<PositionedProcessNode> =
        input.nodes.iter().map(|s| blank_placed(s, 0)).collect();

    let mut cells: Vec<Vec<Vec<usize>>> = vec![vec![Vec::new(); num_stages]; num_lanes];
    let mut lane_counts = vec![0usize; num_lanes];
    for (stage_idx, col) in columns.iter().enumerate() {
        for &node_idx in col {
            let li = resolve_lane_index(&input.nodes[node_idx], &lane_id_to_idx);
            cells[li][stage_idx].push(node_idx);
            lane_counts[li] += 1;
            placed[node_idx].lane_id = Some(lanes[li].id.clone());
        }
    }

    let row_heights: Vec<f32> = columns
        .iter()
        .map(|row| {
            row.iter()
                .map(|&i| input.nodes[i].size_hint.1.max(36.0))
                .fold(0.0f32, f32::max)
                .max(56.0)
        })
        .collect();

    let content_y0 = PROCESS_PAD_Y + PROCESS_LANE_HEADER_H + PROCESS_LANE_PAD;
    let mut stage_y = Vec::with_capacity(num_stages);
    let mut cy = content_y0;
    for &h in &row_heights {
        stage_y.push(cy);
        cy += h + PROCESS_STAGE_GAP_Y;
    }
    let total_band_h = (cy - PROCESS_STAGE_GAP_Y + PROCESS_LANE_PAD - PROCESS_PAD_Y)
        .max(PROCESS_LANE_HEADER_H + 180.0);

    let mut stages = Vec::with_capacity(num_stages);
    for (stage_idx, row) in columns.iter().enumerate() {
        let has_parallel = row.len() >= 2 || row.iter().any(|&i| input.nodes[i].is_group_container);
        stages.push(PositionedStageTrack {
            stage_index: stage_idx,
            x: PROCESS_PAD_X,
            y: stage_y[stage_idx] + (row_heights[stage_idx] * 0.5 - 10.0).max(0.0),
            w: STAGE_LEFT_GUTTER - 12.0,
            h: 20.0,
            node_count: row.len(),
            has_parallel,
            vertical: true,
        });
    }

    let mut lane_bands = Vec::with_capacity(num_lanes);
    let mut stage_extents = vec![PROCESS_PAD_X + STAGE_LEFT_GUTTER; num_stages];
    let mut cursor_x = PROCESS_PAD_X + STAGE_LEFT_GUTTER;

    for (li, lane) in lanes.iter().enumerate() {
        let max_cell_w = cells[li]
            .iter()
            .map(|cell| {
                let sum_w: f32 = cell
                    .iter()
                    .map(|&i| input.nodes[i].size_hint.0.max(80.0))
                    .sum();
                sum_w + PROCESS_NODE_GAP_X * cell.len().saturating_sub(1) as f32
            })
            .fold(0.0f32, f32::max);
        let band_w = if lane_counts[li] == 0 {
            160.0
        } else {
            (max_cell_w + PROCESS_LANE_PAD * 2.0).max(200.0)
        };

        lane_bands.push(PositionedLaneBand {
            id: lane.id.clone(),
            title: lane.title.clone(),
            accent_index: lane.accent_index,
            x: cursor_x,
            y: PROCESS_PAD_Y,
            w: band_w,
            h: total_band_h,
            header_w: band_w,
            header_h: PROCESS_LANE_HEADER_H,
            node_count: lane_counts[li],
            horizontal: false,
        });

        for (stage_idx, cell) in cells[li].iter().enumerate() {
            if cell.is_empty() {
                continue;
            }
            let cell_w: f32 = cell
                .iter()
                .map(|&i| input.nodes[i].size_hint.0.max(80.0))
                .sum::<f32>()
                + PROCESS_NODE_GAP_X * cell.len().saturating_sub(1) as f32;
            let mut nx = match input.policy.alignment {
                CrossAlignment::Start => cursor_x + PROCESS_LANE_PAD,
                CrossAlignment::Center | CrossAlignment::Barycenter => {
                    cursor_x + ((band_w - cell_w) * 0.5).max(PROCESS_LANE_PAD)
                }
            };
            let rh = row_heights[stage_idx];
            for &node_idx in cell {
                let spec = &input.nodes[node_idx];
                let nw = spec.size_hint.0.max(80.0);
                let nh = spec.size_hint.1.max(36.0);
                placed[node_idx].stage = stage_idx;
                placed[node_idx].x = nx;
                placed[node_idx].y = stage_y[stage_idx] + (rh - nh) * 0.5;
                placed[node_idx].w = nw;
                placed[node_idx].h = nh;
                nx += nw + PROCESS_NODE_GAP_X;
            }
            stage_extents[stage_idx] = stage_extents[stage_idx].max(cursor_x + band_w);
        }

        cursor_x += band_w + PROCESS_LANE_GAP;
    }

    let max_x = (cursor_x - PROCESS_LANE_GAP).max(PROCESS_PAD_X + STAGE_LEFT_GUTTER);
    let max_y = PROCESS_PAD_Y + total_band_h;
    (
        placed,
        lane_bands,
        stages,
        PlacementBounds {
            max_x,
            max_y,
            stage_extents,
        },
        None,
    )
}

fn compute_node_ports(
    spec: &ProcessNodeSpec,
    pnode: &PositionedProcessNode,
    horizontal_ports: bool,
) -> (Vec<PositionedPort>, (f32, f32), (f32, f32)) {
    let mut in_specs: Vec<ProcessPortSpec> = spec
        .ports
        .iter()
        .filter(|p| p.direction == ProcessPortDirection::Input)
        .cloned()
        .collect();
    let mut out_specs: Vec<ProcessPortSpec> = spec
        .ports
        .iter()
        .filter(|p| p.direction == ProcessPortDirection::Output)
        .cloned()
        .collect();
    if in_specs.is_empty() {
        in_specs.push(ProcessPortSpec::input("in"));
    }
    if out_specs.is_empty() {
        out_specs.push(ProcessPortSpec::output("out"));
    }

    let mut ports = Vec::with_capacity(in_specs.len() + out_specs.len());
    let primary_offset = spec.primary_port_offset;

    let place_group = |list: &[ProcessPortSpec], is_output: bool, out: &mut Vec<PositionedPort>| {
        let n = list.len();
        for (i, ps) in list.iter().enumerate() {
            let pos = if horizontal_ports {
                let px = if is_output {
                    pnode.x + pnode.w
                } else {
                    pnode.x
                };
                let py = if n == 1 {
                    pnode.y + primary_offset.unwrap_or(pnode.h * 0.5)
                } else {
                    let frac = 0.22 + (0.56 * i as f32 / (n - 1) as f32);
                    pnode.y + pnode.h * frac
                };
                (px, py)
            } else {
                let py = if is_output {
                    pnode.y + pnode.h
                } else {
                    pnode.y
                };
                let px = if n == 1 {
                    pnode.x + pnode.w * 0.5
                } else {
                    let frac = 0.22 + (0.56 * i as f32 / (n - 1) as f32);
                    pnode.x + pnode.w * frac
                };
                (px, py)
            };
            out.push(PositionedPort {
                id: ps.id.clone(),
                direction: ps.direction,
                label: ps.label.clone(),
                tone: ps.tone,
                pos,
                allow_quick_add: ps.allow_quick_add,
            });
        }
    };

    place_group(&in_specs, false, &mut ports);
    place_group(&out_specs, true, &mut ports);

    let default_in = ports
        .iter()
        .find(|p| p.direction == ProcessPortDirection::Input)
        .map(|p| p.pos)
        .unwrap_or((pnode.x, pnode.y + pnode.h * 0.5));
    let default_out = ports
        .iter()
        .find(|p| p.direction == ProcessPortDirection::Output)
        .map(|p| p.pos)
        .unwrap_or((pnode.x + pnode.w, pnode.y + pnode.h * 0.5));

    (ports, default_in, default_out)
}

#[allow(clippy::too_many_arguments)]
fn route_edges(
    specs: &[ProcessEdgeSpec],
    placed: &mut [PositionedProcessNode],
    node_index: &HashMap<String, usize>,
    policy: LayoutPolicy,
    use_lanes: bool,
    horizontal_ports: bool,
    bounds: &PlacementBounds,
    serpentine: Option<&SerpentineMeta>,
) -> (Vec<RoutedProcessEdge>, f32, f32) {
    let mut routed = Vec::with_capacity(specs.len());
    let mut long_lane_idx = 0usize;
    let mut data_lane_idx = 0usize;
    let mut max_x = bounds.max_x;
    let mut max_y = bounds.max_y;

    for edge in specs {
        if edge.layer == ProcessEdgeLayer::DataFlow
            && policy.data_flow == DataFlowVisibility::Hidden
        {
            continue;
        }
        let (Some(&from_i), Some(&to_i)) = (
            node_index.get(&edge.from_node),
            node_index.get(&edge.to_node),
        ) else {
            continue;
        };
        if from_i == to_i {
            continue;
        }

        let from_stage = placed[from_i].stage;
        let to_stage = placed[to_i].stage;
        let from_pinned = placed[from_i].pinned;
        let to_pinned = placed[to_i].pinned;
        let p0 = placed[from_i].port_pos(&edge.from_port, ProcessPortDirection::Output);
        let p1 = placed[to_i].port_pos(&edge.to_port, ProcessPortDirection::Input);

        if edge.layer == ProcessEdgeLayer::Control {
            placed[from_i].has_out_edge = true;
            placed[to_i].has_in_edge = true;
        }

        let waypoints = if edge.layer == ProcessEdgeLayer::DataFlow {
            // Soft outer data-flow arc so variable lines never collide with control buses.
            let offset = 18.0 + data_lane_idx as f32 * DATA_EDGE_LANE_STEP;
            data_lane_idx += 1;
            if horizontal_ports {
                let corridor_y = bounds.max_y + offset;
                max_y = max_y.max(corridor_y);
                let exit_x = p0.0 + 22.0;
                let enter_x = p1.0 - 22.0;
                vec![p0, (exit_x, corridor_y), (enter_x, corridor_y), p1]
            } else {
                let corridor_x = bounds.max_x + offset;
                max_x = max_x.max(corridor_x);
                let exit_y = p0.1 + 18.0;
                let enter_y = p1.1 - 18.0;
                vec![p0, (corridor_x, exit_y), (corridor_x, enter_y), p1]
            }
        } else if let Some(serp) = serpentine {
            let from_row = from_stage / serp.max_per_row;
            let to_row = to_stage / serp.max_per_row;
            if !from_pinned && !to_pinned && from_row != to_row {
                // Smooth turnaround corridor between serpentine rows.
                let mid_y = if to_row > from_row {
                    (serp.row_bottoms[from_row] + serp.row_tops[to_row]) * 0.5
                } else {
                    (serp.row_bottoms[to_row] + serp.row_tops[from_row]) * 0.5
                };
                let exit_x = p0.0 + 26.0;
                let enter_x = (p1.0 - 26.0).max(8.0);
                max_x = max_x.max(exit_x);
                vec![p0, (exit_x, mid_y), (enter_x, mid_y), p1]
            } else if !from_pinned && !to_pinned && to_stage > from_stage + 1 {
                let lane_y = serp.row_bottoms[from_row]
                    + 14.0
                    + (long_lane_idx % 3) as f32 * LONG_EDGE_LANE_STEP;
                long_lane_idx += 1;
                max_y = max_y.max(lane_y);
                let exit_x = p0.0 + PROCESS_STAGE_GAP_X * 0.36;
                let enter_x = p1.0 - PROCESS_STAGE_GAP_X * 0.36;
                vec![p0, (exit_x, lane_y), (enter_x, lane_y), p1]
            } else {
                vec![p0, p1]
            }
        } else if !from_pinned && !to_pinned && !use_lanes && to_stage > from_stage + 1 {
            // Multi-stage skip corridor cleanly clearing every intermediate stage card.
            if horizontal_ports {
                let obstacle_bottom = bounds.stage_extents[(from_stage + 1)..to_stage]
                    .iter()
                    .copied()
                    .fold(bounds.max_y.min(p0.1.max(p1.1)), f32::max);
                let lane_y = obstacle_bottom + 20.0 + long_lane_idx as f32 * LONG_EDGE_LANE_STEP;
                long_lane_idx += 1;
                max_y = max_y.max(lane_y);
                let exit_x = p0.0 + PROCESS_STAGE_GAP_X * 0.42;
                let enter_x = p1.0 - PROCESS_STAGE_GAP_X * 0.42;
                vec![p0, (exit_x, lane_y), (enter_x, lane_y), p1]
            } else {
                let obstacle_right = bounds.stage_extents[(from_stage + 1)..to_stage]
                    .iter()
                    .copied()
                    .fold(bounds.max_x.min(p0.0.max(p1.0)), f32::max);
                let lane_x = obstacle_right + 22.0 + long_lane_idx as f32 * LONG_EDGE_LANE_STEP;
                long_lane_idx += 1;
                max_x = max_x.max(lane_x);
                let exit_y = p0.1 + PROCESS_STAGE_GAP_Y * 0.42;
                let enter_y = p1.1 - PROCESS_STAGE_GAP_Y * 0.42;
                vec![p0, (lane_x, exit_y), (lane_x, enter_y), p1]
            }
        } else {
            vec![p0, p1]
        };

        let mid = edge_midpoint(&waypoints);
        let label_pos = edge.label.as_ref().map(|_| {
            if waypoints.len() == 2 {
                // Place branch/edge labels closer to source exit port (32%) to reduce collisions.
                (p0.0 + (p1.0 - p0.0) * 0.32, p0.1 + (p1.1 - p0.1) * 0.32)
            } else {
                mid
            }
        });
        let insert_pos =
            (edge.insertable && edge.layer == ProcessEdgeLayer::Control).then_some(mid);

        routed.push(RoutedProcessEdge {
            id: edge.id.clone(),
            from_node: edge.from_node.clone(),
            from_port: edge.from_port.clone(),
            to_node: edge.to_node.clone(),
            to_port: edge.to_port.clone(),
            layer: edge.layer,
            kind: edge.kind,
            label: edge.label.clone(),
            label_pos,
            insert_pos,
            tone: edge.tone,
            dashed: edge.dashed,
            horizontal_ports,
            waypoints,
        });
    }

    (routed, max_x, max_y)
}

fn edge_midpoint(waypoints: &[(f32, f32)]) -> (f32, f32) {
    match waypoints {
        [] => (0.0, 0.0),
        [a] => *a,
        [a, b] => ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5),
        pts => {
            let mid_i = pts.len() / 2;
            let a = pts[mid_i - 1];
            let b = pts[mid_i];
            ((a.0 + b.0) * 0.5, (a.1 + b.1) * 0.5)
        }
    }
}

/// Helper mapping status transitions to semantic edge tone & dash styling.
#[must_use]
pub const fn classify_control_edge_tone(
    from: super::model::ProcessNodeStatus,
    to: super::model::ProcessNodeStatus,
) -> (ProcessEdgeTone, bool) {
    use super::model::ProcessNodeStatus as S;
    if matches!(to, S::Skipped) || matches!(from, S::Cancelled) {
        return (ProcessEdgeTone::Skipped, true);
    }
    if from.is_terminal_error() || to.is_terminal_error() {
        return (ProcessEdgeTone::Danger, false);
    }
    if matches!(from, S::Waiting) || matches!(to, S::Waiting) {
        return (ProcessEdgeTone::Warning, true);
    }
    if matches!(from, S::Running) || matches!(to, S::Running) {
        return (ProcessEdgeTone::Active, true);
    }
    if matches!(from, S::Completed) && matches!(to, S::Completed) {
        return (ProcessEdgeTone::Completed, false);
    }
    (ProcessEdgeTone::Neutral, false)
}
