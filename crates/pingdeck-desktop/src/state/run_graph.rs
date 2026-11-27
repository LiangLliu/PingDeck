//! Headless, UI-framework-agnostic Execution Graph layout engine & adaptive 3-view router.
//!
//! Designed as a reusable Layer 2 layout foundation for:
//! - Phase 1: Runtime [`AgentRunTopology`] rendered as a left-to-right stage Execution Graph
//!   (`执行图`), alongside adaptive routing between `Steps`, `Graph`, and `Timeline`.
//! - Phase 2: Structured `TaskGraph` blueprints (`depends_on`, multi-stage skip edges, parallel
//!   groups, and live run overlay) via the exact same [`layout_dag_nodes`] engine.
//! - Future HTTP `Flows` visual DAG rendering without duplicating layout geometry.

use std::collections::{HashMap, HashSet, VecDeque};

use pingdeck_core::agent::dto::{
    AgentRunTopology, ExecutionWave, TopologySubagent, TopologyToolStep,
};
use pingdeck_core::agent::model::{AgentRunStatus, SubagentRole};

/// Default card width in unscaled logical pixels.
pub const GRAPH_NODE_W: f32 = 240.0;
/// Horizontal corridor between adjacent stage columns (gives generous room for smooth Bézier buses).
pub const GRAPH_STAGE_GAP: f32 = 72.0;
/// Vertical gap between sibling cards in the same stage column.
pub const GRAPH_NODE_GAP_Y: f32 = 16.0;
/// Canvas outer padding.
pub const GRAPH_PAD_X: f32 = 28.0;
pub const GRAPH_PAD_TOP: f32 = 44.0;
pub const GRAPH_PAD_BOTTOM: f32 = 32.0;
/// Height of the top tab flap on GitHub-Actions-style parallel group cards.
pub const GRAPH_GROUP_TAB_H: f32 = 22.0;
/// Standard single-node card height (compact).
pub const GRAPH_NODE_BASE_H: f32 = 64.0;
/// Taller single-node card height when showing an extra warning/error strip.
pub const GRAPH_NODE_ALERT_H: f32 = 88.0;
/// Collapsed parallel group card body height (excluding tab flap).
pub const GRAPH_GROUP_COLLAPSED_BODY_H: f32 = 74.0;
/// Expanded parallel group card header height (excluding tab flap).
pub const GRAPH_GROUP_EXPANDED_HEADER_H: f32 = 46.0;
/// Height of one member row inside an expanded parallel group card.
pub const GRAPH_MEMBER_ROW_H: f32 = 34.0;
/// Bottom padding inside an expanded parallel group card.
pub const GRAPH_GROUP_FOOTER_PAD_H: f32 = 10.0;
/// Vertical clearance below intermediate stages for cross-stage skip edges.
const LONG_EDGE_LANE_STEP: f32 = 18.0;

/// Orthogonal single-run presentation modes inside the Run Monitor detail canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum RunViewMode {
    /// Direct-answer summary card (0 tools) or high-density linear/round-grouped step trace.
    Steps,
    /// GitHub-Actions-style left-to-right layered Execution Graph (`执行图`).
    Graph,
    /// Hierarchical wave tree + millisecond waterfall profiler (`时间轴`).
    Timeline,
}

impl RunViewMode {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Steps => 0,
            Self::Graph => 1,
            Self::Timeline => 2,
        }
    }

    #[must_use]
    pub const fn from_index(index: usize) -> Self {
        match index {
            1 => Self::Graph,
            2 => Self::Timeline,
            _ => Self::Steps,
        }
    }
}

/// Normalized visual lifecycle state for any Execution Graph node or group member.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphNodeStatus {
    Pending,
    Queued,
    Running,
    Waiting,
    Completed,
    Failed,
    Cancelled,
    Interrupted,
    Skipped,
}

impl GraphNodeStatus {
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Waiting)
    }

    #[must_use]
    pub const fn is_terminal_error(self) -> bool {
        matches!(self, Self::Failed | Self::Interrupted)
    }
}

/// Semantic category of an Execution Graph node (extensible for Phase 2 `TaskGraph` plans).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphNodeKind {
    Main,
    Explore,
    Implement,
    Verify,
    Gate,
    Tool,
    Group,
}

/// Semantic styling tone for a routed dependency edge.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum GraphEdgeTone {
    Neutral,
    Active,
    Completed,
    Warning,
    Danger,
    Skipped,
}

/// One selectable child item inside a collapsible Parallel Group card.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphNodeMember {
    pub id: String,
    pub title: String,
    pub subtitle: Option<String>,
    pub status: GraphNodeStatus,
    pub elapsed_ms: Option<u64>,
    pub role: Option<SubagentRole>,
    pub subagent_kind: Option<String>,
    pub exit_code: Option<i32>,
    pub stop_run_id: Option<String>,
    pub error: Option<String>,
    pub waiting_summary: Option<String>,
    pub payload: GraphNodePayload,
}

/// Typed inspection payload attached to a node or group member for the `NodeInspector`.
#[derive(Debug, Clone, PartialEq)]
pub enum GraphNodePayload {
    /// Entry trigger or terminal join/summary node representing the parent main agent run.
    MainRun { is_summary: bool },
    /// Concrete child sub-agent run with its tool calls, report, and optional pending approval.
    Subagent(Box<TopologySubagent>),
    /// Single serial tool barrier step.
    ToolStep(TopologyToolStep),
    /// Collapsible parallel group enclosing multiple concurrent tools or sub-agents.
    ParallelGroup {
        subagents: Vec<TopologySubagent>,
        tools: Vec<TopologyToolStep>,
    },
    /// Reserved for Phase 2 planned `TaskGraph` nodes that have not started executing yet.
    PlannedTask { objective: String },
}

/// Logical DAG node fed into [`layout_dag_nodes`].
#[derive(Debug, Clone, PartialEq)]
pub struct GraphNode {
    pub id: String,
    /// Optional explicit stage column; when `None`, computed from `depends_on` via longest-path
    /// Kahn topological layering.
    pub stage_hint: Option<usize>,
    pub kind: GraphNodeKind,
    pub title: String,
    pub subtitle: Option<String>,
    pub status: GraphNodeStatus,
    pub elapsed_ms: Option<u64>,
    pub role: Option<SubagentRole>,
    pub subagent_kind: Option<String>,
    pub resumed: bool,
    pub step_count: Option<u32>,
    pub exit_code: Option<i32>,
    pub error: Option<String>,
    pub waiting_summary: Option<String>,
    pub stop_run_id: Option<String>,
    /// When `Some`, renders the GitHub-Actions-style top tab flap (`Parallel group · ...`).
    pub group_tab: Option<String>,
    /// Collapsible sub-items (sub-agent tools, parallel tool batch, or matrix sub-agents).
    pub members: Vec<GraphNodeMember>,
    pub expanded: bool,
    /// Upstream dependency node IDs (`from_id -> self.id`).
    pub depends_on: Vec<String>,
    pub payload: GraphNodePayload,
}

impl GraphNode {
    /// Deterministic card height in unscaled logical pixels so ports align without layout jitter.
    #[must_use]
    pub fn computed_height(&self) -> f32 {
        let has_alert = self.error.is_some() || self.waiting_summary.is_some();
        if self.group_tab.is_some() && !self.members.is_empty() {
            if self.expanded {
                let alert_extra = if has_alert { 22.0 } else { 0.0 };
                GRAPH_GROUP_TAB_H
                    + GRAPH_GROUP_EXPANDED_HEADER_H
                    + alert_extra
                    + (self.members.len() as f32 * GRAPH_MEMBER_ROW_H)
                    + GRAPH_GROUP_FOOTER_PAD_H
            } else {
                GRAPH_GROUP_TAB_H
                    + GRAPH_GROUP_COLLAPSED_BODY_H
                    + if has_alert { 20.0 } else { 0.0 }
            }
        } else if has_alert {
            GRAPH_NODE_ALERT_H
        } else {
            GRAPH_NODE_BASE_H
        }
    }

    /// Y offset from card top to the visual vertical center of the main card body (below any tab).
    #[must_use]
    pub fn port_offset_y(&self) -> f32 {
        if self.group_tab.is_some() {
            // Anchor near the upper summary header so expanding/collapsing keeps the entry bus steady.
            GRAPH_GROUP_TAB_H + 34.0
        } else {
            self.computed_height() * 0.5
        }
    }

    /// Completed and total member counts for a parallel group card.
    #[must_use]
    pub fn member_progress(&self) -> (usize, usize) {
        let done = self
            .members
            .iter()
            .filter(|m| matches!(m.status, GraphNodeStatus::Completed))
            .count();
        (done, self.members.len())
    }
}

/// Positioned card box with exact logical coordinates and port anchors.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionedNode {
    pub node: GraphNode,
    pub stage: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub port_in: (f32, f32),
    pub port_out: (f32, f32),
    pub has_in_edge: bool,
    pub has_out_edge: bool,
}

/// Routed dependency edge between two positioned nodes.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphEdge {
    pub id: String,
    pub from_id: String,
    pub to_id: String,
    pub tone: GraphEdgeTone,
    pub dashed: bool,
    /// Ordered polyline/curve control anchors: `[start_port, ...waypoints, end_port]`.
    pub waypoints: Vec<(f32, f32)>,
}

/// Metadata for one left-to-right stage column header.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphStageHeader {
    pub stage_index: usize,
    pub x: f32,
    pub w: f32,
    pub node_count: usize,
    pub has_parallel: bool,
}

/// Complete, deterministically laid-out Execution Graph ready for canvas + card rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionGraphLayout {
    pub stages: Vec<GraphStageHeader>,
    pub nodes: Vec<PositionedNode>,
    pub edges: Vec<GraphEdge>,
    pub canvas_width: f32,
    pub canvas_height: f32,
    pub collapsible_group_ids: Vec<String>,
    pub total_nodes: usize,
    pub completed_nodes: usize,
    pub active_nodes: usize,
    pub failed_nodes: usize,
}

/// Resolved selection target when a user clicks either a top-level node or an expanded group member.
#[derive(Debug, Clone, Copy)]
pub enum SelectedGraphTarget<'a> {
    Node(&'a PositionedNode),
    Member {
        parent: &'a PositionedNode,
        member: &'a GraphNodeMember,
    },
}

impl ExecutionGraphLayout {
    /// Resolves `selected_id` against both top-level nodes and inner group members.
    #[must_use]
    pub fn find_selection(&self, selected_id: &str) -> Option<SelectedGraphTarget<'_>> {
        for pnode in &self.nodes {
            if pnode.node.id == selected_id {
                return Some(SelectedGraphTarget::Node(pnode));
            }
            if let Some(member) = pnode.node.members.iter().find(|m| m.id == selected_id) {
                return Some(SelectedGraphTarget::Member {
                    parent: pnode,
                    member,
                });
            }
        }
        None
    }

    /// Returns titles of immediate upstream dependencies of `node_id`.
    #[must_use]
    pub fn upstream_titles(&self, node_id: &str) -> Vec<String> {
        let Some(target) = self.nodes.iter().find(|n| n.node.id == node_id) else {
            return Vec::new();
        };
        target
            .node
            .depends_on
            .iter()
            .filter_map(|dep_id| {
                self.nodes
                    .iter()
                    .find(|n| n.node.id == *dep_id)
                    .map(|n| n.node.title.clone())
            })
            .collect()
    }
}

/// Whether the Execution Graph (`执行图`) view is meaningful for `topo`.
///
/// Zero-tool direct answers have no stages or edges to visualize, so the UI disables `Graph` and
/// routes directly to the concise `Steps` summary card instead of showing a lonely single box.
#[must_use]
pub fn is_graph_available(topo: &AgentRunTopology) -> bool {
    !topo.waves.is_empty()
}

/// Deterministic adaptive default view mode for an agent run.
///
/// Priority order:
/// 1. Parallel sub-agents, multiple sub-agents (`>= 2`), or multi-stage runs (`>= 3` waves) with
///    any parallel batch or sub-agent delegation -> [`RunViewMode::Graph`].
/// 2. Long iterative serial debug loops (`iterations >= 5` or `waves >= 8`) ->
///    [`RunViewMode::Timeline`].
/// 3. Direct answers (0 tools) or short linear runs (1-4 serial steps) -> [`RunViewMode::Steps`].
#[must_use]
pub fn select_default_view_mode(topo: &AgentRunTopology) -> RunViewMode {
    if topo.waves.is_empty() {
        return RunViewMode::Steps;
    }
    let has_parallel_subs = topo
        .waves
        .iter()
        .any(|w| matches!(w, ExecutionWave::ParallelSubagents { .. }));
    let has_parallel_or_sub = topo.waves.iter().any(|w| {
        matches!(
            w,
            ExecutionWave::ParallelSubagents { .. }
                | ExecutionWave::SingleSubagent { .. }
                | ExecutionWave::ParallelTools { .. }
        )
    });
    if has_parallel_subs
        || topo.subagent_counts.total >= 2
        || (topo.waves.len() >= 3 && has_parallel_or_sub)
    {
        return RunViewMode::Graph;
    }
    if topo.run.iterations >= 5 || topo.waves.len() >= 8 {
        return RunViewMode::Timeline;
    }
    RunViewMode::Steps
}

/// Converts a runtime [`AgentRunTopology`] into a laid-out GitHub-Actions-style Execution Graph.
#[must_use]
pub fn build_graph_from_topology(
    topo: &AgentRunTopology,
    expanded_groups: &HashSet<String>,
    now_ms: i64,
    labels: &GraphBuildLabels<'_>,
) -> ExecutionGraphLayout {
    let mut nodes: Vec<GraphNode> = Vec::new();
    let run_start = topo.run.started_at_ms;
    let first_wave_start = topo
        .waves
        .first()
        .and_then(wave_started_ms)
        .unwrap_or(run_start);
    let trigger_ms = (first_wave_start - run_start).max(0) as u64;

    // Stage 0: Entry trigger node (Main Agent start).
    let trigger_id = format!("main-start-{}", topo.run.run_id);
    let trigger_status = if topo.waves.is_empty() {
        map_run_status(topo.run.status)
    } else {
        GraphNodeStatus::Completed
    };
    nodes.push(GraphNode {
        id: trigger_id.clone(),
        stage_hint: Some(0),
        kind: GraphNodeKind::Main,
        title: labels.trigger_title.to_owned(),
        subtitle: Some(topo.run.model_label.clone()),
        status: trigger_status,
        elapsed_ms: (trigger_ms > 0).then_some(trigger_ms),
        role: None,
        subagent_kind: None,
        resumed: false,
        step_count: None,
        exit_code: None,
        error: None,
        waiting_summary: None,
        stop_run_id: None,
        group_tab: None,
        members: Vec::new(),
        expanded: false,
        depends_on: Vec::new(),
        payload: GraphNodePayload::MainRun { is_summary: false },
    });

    let mut prev_stage_ids = vec![trigger_id];
    let mut stage_cursor = 1usize;

    for wave in &topo.waves {
        let mut current_ids = Vec::new();
        match wave {
            ExecutionWave::ParallelSubagents {
                wave_index,
                started_ms,
                ended_ms,
                subagents,
            } => {
                if subagents.len() <= 4 {
                    // Fan out into stacked branch cards inside this stage column (GitHub Actions
                    // parallel matrix look). Each branch card embeds its tool calls as an optional
                    // collapsible list when non-empty.
                    for sub in subagents {
                        let nid = format!("sub:{}", sub.run.run_id);
                        let expanded = expanded_groups.contains(&nid);
                        let sub_node = subagent_to_graph_node(
                            nid.clone(),
                            stage_cursor,
                            sub,
                            &prev_stage_ids,
                            expanded,
                            now_ms,
                            Some(labels.parallel_group.to_owned()),
                        );
                        nodes.push(sub_node);
                        current_ids.push(nid);
                    }
                } else {
                    // Large fan-out (>4): fold into a single collapsible Parallel Group card.
                    let gid = format!("grp-subs-{}-{}", topo.run.run_id, wave_index);
                    let expanded = expanded_groups.contains(&gid);
                    let members: Vec<GraphNodeMember> = subagents
                        .iter()
                        .map(|s| subagent_to_member(s, now_ms))
                        .collect();
                    let status = aggregate_member_status(&members);
                    let elapsed = span_duration_ms(*started_ms, *ended_ms, now_ms);
                    let waiting = subagents
                        .iter()
                        .find_map(|s| s.pending_approval.as_ref().map(|a| a.summary.clone()));
                    let error = subagents.iter().find_map(|s| s.run.error.clone());
                    nodes.push(GraphNode {
                        id: gid.clone(),
                        stage_hint: Some(stage_cursor),
                        kind: GraphNodeKind::Group,
                        title: labels.parallel_group.to_owned(),
                        subtitle: None,
                        status,
                        elapsed_ms: elapsed,
                        role: None,
                        subagent_kind: None,
                        resumed: false,
                        step_count: Some(subagents.len() as u32),
                        exit_code: None,
                        error,
                        waiting_summary: waiting,
                        stop_run_id: None,
                        group_tab: Some(format!(
                            "{} · ×{}",
                            labels.parallel_group,
                            subagents.len()
                        )),
                        members,
                        expanded,
                        depends_on: prev_stage_ids.clone(),
                        payload: GraphNodePayload::ParallelGroup {
                            subagents: subagents.clone(),
                            tools: Vec::new(),
                        },
                    });
                    current_ids.push(gid);
                }
            }
            ExecutionWave::SingleSubagent { subagent, .. } => {
                let nid = format!("sub:{}", subagent.run.run_id);
                let expanded = expanded_groups.contains(&nid);
                let sub_node = subagent_to_graph_node(
                    nid.clone(),
                    stage_cursor,
                    subagent,
                    &prev_stage_ids,
                    expanded,
                    now_ms,
                    None,
                );
                nodes.push(sub_node);
                current_ids.push(nid);
            }
            ExecutionWave::ParallelTools {
                wave_index,
                started_ms,
                ended_ms,
                tools,
            } => {
                let gid = format!("grp-tools-{}-{}", topo.run.run_id, wave_index);
                let expanded = expanded_groups.contains(&gid);
                let members: Vec<GraphNodeMember> =
                    tools.iter().map(|t| tool_to_member(t, now_ms)).collect();
                let status = aggregate_member_status(&members);
                let elapsed = span_duration_ms(*started_ms, *ended_ms, now_ms);
                let error = tools
                    .iter()
                    .find(|t| t.status == "error")
                    .map(|t| format!("{} · {}", t.name, t.summary));
                nodes.push(GraphNode {
                    id: gid.clone(),
                    stage_hint: Some(stage_cursor),
                    kind: GraphNodeKind::Group,
                    title: format!("{} ×{}", labels.kind_tool, tools.len()),
                    subtitle: None,
                    status,
                    elapsed_ms: elapsed,
                    role: None,
                    subagent_kind: None,
                    resumed: false,
                    step_count: Some(tools.len() as u32),
                    exit_code: tools.iter().find_map(|t| t.exit_code.filter(|c| *c != 0)),
                    error,
                    waiting_summary: None,
                    stop_run_id: None,
                    group_tab: Some(format!("{} · {}", labels.parallel_group, labels.kind_tool)),
                    members,
                    expanded,
                    depends_on: prev_stage_ids.clone(),
                    payload: GraphNodePayload::ParallelGroup {
                        subagents: Vec::new(),
                        tools: tools.clone(),
                    },
                });
                current_ids.push(gid);
            }
            ExecutionWave::SerialTool { tool, .. } => {
                let nid = format!("tool:{}", tool.call_id);
                let status = map_tool_status(&tool.status);
                let elapsed = tool
                    .duration_ms
                    .or_else(|| span_duration_ms(tool.started_ms, tool.ended_ms, now_ms));
                let kind = infer_tool_node_kind(&tool.name, &tool.class);
                nodes.push(GraphNode {
                    id: nid.clone(),
                    stage_hint: Some(stage_cursor),
                    kind,
                    title: tool.name.clone(),
                    subtitle: (!tool.summary.is_empty()).then(|| tool.summary.clone()),
                    status,
                    elapsed_ms: elapsed,
                    role: None,
                    subagent_kind: None,
                    resumed: false,
                    step_count: None,
                    exit_code: tool.exit_code,
                    error: (status == GraphNodeStatus::Failed).then(|| tool.summary.clone()),
                    waiting_summary: (status == GraphNodeStatus::Waiting)
                        .then(|| tool.summary.clone()),
                    stop_run_id: None,
                    group_tab: None,
                    members: Vec::new(),
                    expanded: false,
                    depends_on: prev_stage_ids.clone(),
                    payload: GraphNodePayload::ToolStep(tool.clone()),
                });
                current_ids.push(nid);
            }
        }
        if !current_ids.is_empty() {
            prev_stage_ids = current_ids;
            stage_cursor += 1;
        }
    }

    // Terminal fan-in / summary node (like GitHub Actions' trailing `CI Gate` column) when the
    // run has multiple branches or completed with a final summary.
    let needs_terminal =
        prev_stage_ids.len() >= 2 || (!topo.run.status.is_active() && !topo.waves.is_empty());
    if needs_terminal {
        let term_id = format!("main-end-{}", topo.run.run_id);
        let term_active = topo.run.status.is_active();
        let term_status = if term_active {
            if prev_stage_ids.len() >= 2 {
                GraphNodeStatus::Queued
            } else {
                map_run_status(topo.run.status)
            }
        } else {
            map_run_status(topo.run.status)
        };
        let total_elapsed = if term_active {
            (now_ms - topo.run.started_at_ms).max(topo.awake_ms as i64) as u64
        } else {
            topo.awake_ms
        };
        nodes.push(GraphNode {
            id: term_id,
            stage_hint: Some(stage_cursor),
            kind: GraphNodeKind::Main,
            title: if term_active {
                labels.joining_title.to_owned()
            } else {
                labels.summary_title.to_owned()
            },
            subtitle: topo.last_answer.clone(),
            status: term_status,
            elapsed_ms: (!term_active).then_some(total_elapsed),
            role: None,
            subagent_kind: None,
            resumed: false,
            step_count: Some(topo.run.iterations),
            exit_code: None,
            error: topo.run.error.clone(),
            waiting_summary: topo.pending_approval.as_ref().map(|a| a.summary.clone()),
            stop_run_id: term_active.then(|| topo.run.run_id.clone()),
            group_tab: None,
            members: Vec::new(),
            expanded: false,
            depends_on: prev_stage_ids,
            payload: GraphNodePayload::MainRun { is_summary: true },
        });
    }

    layout_dag_nodes(nodes)
}

/// Localized labels passed into [`build_graph_from_topology`] so Layer 2 stays language-neutral.
pub struct GraphBuildLabels<'a> {
    pub trigger_title: &'a str,
    pub summary_title: &'a str,
    pub joining_title: &'a str,
    pub parallel_group: &'a str,
    pub kind_tool: &'a str,
}

/// Pure, reusable layered DAG layout engine.
///
/// Handles arbitrary acyclic `depends_on` graphs (Phase 1 runtime waves, Phase 2 `TaskGraph`
/// blueprints, and future HTTP `Flows`):
/// 1. Assigns stage columns via Kahn topological longest-path (honoring explicit `stage_hint` when
///    valid and falling back gracefully if an input graph accidentally contains a cycle).
/// 2. Orders nodes within each stage by upstream barycenter to minimize edge crossings.
/// 3. Places cards top-aligned per column (GitHub Actions style) using deterministic heights.
/// 4. Routes adjacent-stage edges directly and routes multi-stage skip edges along a clean bottom
///    corridor so edges never slice through intermediate cards.
#[must_use]
pub fn layout_dag_nodes(nodes: Vec<GraphNode>) -> ExecutionGraphLayout {
    if nodes.is_empty() {
        return ExecutionGraphLayout {
            stages: Vec::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            canvas_width: 0.0,
            canvas_height: 0.0,
            collapsible_group_ids: Vec::new(),
            total_nodes: 0,
            completed_nodes: 0,
            active_nodes: 0,
            failed_nodes: 0,
        };
    }

    let id_to_idx: HashMap<String, usize> = nodes
        .iter()
        .enumerate()
        .map(|(i, n)| (n.id.clone(), i))
        .collect();

    // 1. Stage assignment (Kahn longest-path + explicit stage_hint respect).
    let stage_of = assign_stages(&nodes, &id_to_idx);
    let max_stage = stage_of.iter().copied().max().unwrap_or(0);
    let mut columns: Vec<Vec<usize>> = vec![Vec::new(); max_stage + 1];
    for (idx, &st) in stage_of.iter().enumerate() {
        columns[st].push(idx);
    }

    // 2. Barycenter ordering within each stage column to reduce crossings.
    let mut row_pos: HashMap<usize, f32> = HashMap::new();
    for (col_idx, col) in columns.iter_mut().enumerate() {
        if col_idx > 0 && col.len() > 1 {
            col.sort_by(|&a, &b| {
                let ba = barycenter(&nodes[a], &id_to_idx, &row_pos);
                let bb = barycenter(&nodes[b], &id_to_idx, &row_pos);
                ba.total_cmp(&bb).then_with(|| a.cmp(&b))
            });
        }
        for (pos, &node_idx) in col.iter().enumerate() {
            row_pos.insert(node_idx, pos as f32);
        }
    }

    // 3. Assign concrete (x, y, w, h) coordinates (top-aligned columns like GitHub Actions).
    let mut positioned: Vec<Option<PositionedNode>> = vec![None; nodes.len()];
    let mut stage_headers: Vec<GraphStageHeader> = Vec::with_capacity(columns.len());
    let mut stage_bottoms: Vec<f32> = vec![GRAPH_PAD_TOP; columns.len()];
    let mut max_content_y: f32 = GRAPH_PAD_TOP;

    for (stage_idx, col) in columns.iter().enumerate() {
        let col_x = GRAPH_PAD_X + stage_idx as f32 * (GRAPH_NODE_W + GRAPH_STAGE_GAP);
        let mut cursor_y = GRAPH_PAD_TOP;
        let has_parallel = col.len() >= 2
            || col
                .iter()
                .any(|&i| !nodes[i].members.is_empty() && nodes[i].group_tab.is_some());
        stage_headers.push(GraphStageHeader {
            stage_index: stage_idx,
            x: col_x,
            w: GRAPH_NODE_W,
            node_count: col.len(),
            has_parallel,
        });

        for &node_idx in col {
            let h = nodes[node_idx].computed_height();
            let port_y = cursor_y + nodes[node_idx].port_offset_y();
            let pnode = PositionedNode {
                node: nodes[node_idx].clone(),
                stage: stage_idx,
                x: col_x,
                y: cursor_y,
                w: GRAPH_NODE_W,
                h,
                port_in: (col_x, port_y),
                port_out: (col_x + GRAPH_NODE_W, port_y),
                has_in_edge: false,
                has_out_edge: false,
            };
            positioned[node_idx] = Some(pnode);
            cursor_y += h + GRAPH_NODE_GAP_Y;
        }
        let col_bottom = (cursor_y - GRAPH_NODE_GAP_Y).max(GRAPH_PAD_TOP);
        stage_bottoms[stage_idx] = col_bottom;
        max_content_y = max_content_y.max(col_bottom);
    }

    let mut positioned: Vec<PositionedNode> = positioned.into_iter().flatten().collect();
    let pos_by_id: HashMap<String, usize> = positioned
        .iter()
        .enumerate()
        .map(|(i, p)| (p.node.id.clone(), i))
        .collect();

    // 4. Route edges (adjacent direct Bézier vs multi-stage bottom corridor waypoints).
    let mut edges: Vec<GraphEdge> = Vec::new();
    let mut long_edge_lane = 0usize;
    let mut max_edge_y = max_content_y;

    for to_idx in 0..positioned.len() {
        let deps = positioned[to_idx].node.depends_on.clone();
        let to_id = positioned[to_idx].node.id.clone();
        let to_stage = positioned[to_idx].stage;
        let to_port = positioned[to_idx].port_in;
        let to_status = positioned[to_idx].node.status;

        for dep_id in deps {
            let Some(&from_idx) = pos_by_id.get(&dep_id) else {
                continue;
            };
            if from_idx == to_idx {
                continue;
            }
            let from_stage = positioned[from_idx].stage;
            let from_port = positioned[from_idx].port_out;
            let from_status = positioned[from_idx].node.status;

            positioned[from_idx].has_out_edge = true;
            positioned[to_idx].has_in_edge = true;

            let (tone, dashed) = classify_edge_style(from_status, to_status);
            let waypoints = if to_stage > from_stage + 1 {
                // Route cleanly beneath every intermediate stage's bottom-most card.
                let obstacle_bottom = stage_bottoms[(from_stage + 1)..to_stage]
                    .iter()
                    .copied()
                    .fold(max_content_y.min(from_port.1.max(to_port.1)), f32::max);
                let lane_y = obstacle_bottom + 20.0 + long_edge_lane as f32 * LONG_EDGE_LANE_STEP;
                long_edge_lane += 1;
                max_edge_y = max_edge_y.max(lane_y);
                let exit_x = from_port.0 + GRAPH_STAGE_GAP * 0.42;
                let enter_x = to_port.0 - GRAPH_STAGE_GAP * 0.42;
                vec![from_port, (exit_x, lane_y), (enter_x, lane_y), to_port]
            } else {
                vec![from_port, to_port]
            };

            edges.push(GraphEdge {
                id: format!("edge:{dep_id}->{to_id}"),
                from_id: dep_id,
                to_id: to_id.clone(),
                tone,
                dashed,
                waypoints,
            });
        }
    }

    let collapsible_group_ids: Vec<String> = positioned
        .iter()
        .filter(|p| p.node.group_tab.is_some() && !p.node.members.is_empty())
        .map(|p| p.node.id.clone())
        .collect();

    let total_nodes = positioned.len();
    let completed_nodes = positioned
        .iter()
        .filter(|p| p.node.status == GraphNodeStatus::Completed)
        .count();
    let active_nodes = positioned
        .iter()
        .filter(|p| p.node.status.is_active())
        .count();
    let failed_nodes = positioned
        .iter()
        .filter(|p| p.node.status.is_terminal_error())
        .count();

    let canvas_width = (GRAPH_PAD_X * 2.0)
        + (columns.len() as f32 * GRAPH_NODE_W)
        + (columns.len().saturating_sub(1) as f32 * GRAPH_STAGE_GAP);
    let canvas_height = max_edge_y + GRAPH_PAD_BOTTOM;

    ExecutionGraphLayout {
        stages: stage_headers,
        nodes: positioned,
        edges,
        canvas_width,
        canvas_height,
        collapsible_group_ids,
        total_nodes,
        completed_nodes,
        active_nodes,
        failed_nodes,
    }
}

fn assign_stages(nodes: &[GraphNode], id_to_idx: &HashMap<String, usize>) -> Vec<usize> {
    let n = nodes.len();
    let mut indeg = vec![0usize; n];
    let mut adj: Vec<Vec<usize>> = vec![Vec::new(); n];
    for (idx, node) in nodes.iter().enumerate() {
        for dep in &node.depends_on {
            if let Some(&u) = id_to_idx.get(dep)
                && u != idx
            {
                adj[u].push(idx);
                indeg[idx] += 1;
            }
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

fn barycenter(
    node: &GraphNode,
    id_to_idx: &HashMap<String, usize>,
    row_pos: &HashMap<usize, f32>,
) -> f32 {
    let mut sum = 0.0f32;
    let mut count = 0usize;
    for dep in &node.depends_on {
        if let Some(&u) = id_to_idx.get(dep)
            && let Some(&pos) = row_pos.get(&u)
        {
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

fn classify_edge_style(from: GraphNodeStatus, to: GraphNodeStatus) -> (GraphEdgeTone, bool) {
    if to == GraphNodeStatus::Skipped || from == GraphNodeStatus::Cancelled {
        return (GraphEdgeTone::Skipped, true);
    }
    if from.is_terminal_error() || to.is_terminal_error() {
        return (GraphEdgeTone::Danger, false);
    }
    if from == GraphNodeStatus::Waiting || to == GraphNodeStatus::Waiting {
        return (GraphEdgeTone::Warning, true);
    }
    if from == GraphNodeStatus::Running || to == GraphNodeStatus::Running {
        return (GraphEdgeTone::Active, true);
    }
    if from == GraphNodeStatus::Completed && to == GraphNodeStatus::Completed {
        return (GraphEdgeTone::Completed, false);
    }
    (GraphEdgeTone::Neutral, false)
}

fn subagent_to_graph_node(
    id: String,
    stage: usize,
    sub: &TopologySubagent,
    depends_on: &[String],
    expanded: bool,
    now_ms: i64,
    group_prefix: Option<String>,
) -> GraphNode {
    let status = map_run_status(sub.run.status);
    let elapsed_ms = if sub.run.status.is_active() {
        (now_ms - sub.run.started_at_ms).max(sub.awake_ms as i64) as u64
    } else {
        sub.awake_ms
    };
    let kind = if sub.subagent_kind.eq_ignore_ascii_case("explore") {
        GraphNodeKind::Explore
    } else {
        GraphNodeKind::Implement
    };
    let members: Vec<GraphNodeMember> = sub
        .tool_calls
        .iter()
        .map(|t| tool_to_member(t, now_ms))
        .collect();
    let group_tab = if !members.is_empty() {
        Some(match group_prefix {
            Some(prefix) => format!("{prefix} · {}", sub.subagent_kind),
            None => sub.subagent_kind.clone(),
        })
    } else {
        None
    };

    GraphNode {
        id,
        stage_hint: Some(stage),
        kind,
        title: sub.description.clone(),
        subtitle: Some(sub.run.model_label.clone()),
        status,
        elapsed_ms: Some(elapsed_ms),
        role: sub.run.role,
        subagent_kind: Some(sub.subagent_kind.clone()),
        resumed: sub.resumed,
        step_count: Some(sub.tool_calls.len() as u32),
        exit_code: None,
        error: sub.run.error.clone(),
        waiting_summary: sub.pending_approval.as_ref().map(|a| a.summary.clone()),
        stop_run_id: sub.run.status.is_active().then(|| sub.run.run_id.clone()),
        group_tab,
        members,
        expanded,
        depends_on: depends_on.to_vec(),
        payload: GraphNodePayload::Subagent(Box::new(sub.clone())),
    }
}

fn subagent_to_member(sub: &TopologySubagent, now_ms: i64) -> GraphNodeMember {
    let elapsed_ms = if sub.run.status.is_active() {
        (now_ms - sub.run.started_at_ms).max(sub.awake_ms as i64) as u64
    } else {
        sub.awake_ms
    };
    GraphNodeMember {
        id: format!("sub:{}", sub.run.run_id),
        title: sub.description.clone(),
        subtitle: Some(sub.run.model_label.clone()),
        status: map_run_status(sub.run.status),
        elapsed_ms: Some(elapsed_ms),
        role: sub.run.role,
        subagent_kind: Some(sub.subagent_kind.clone()),
        exit_code: None,
        stop_run_id: sub.run.status.is_active().then(|| sub.run.run_id.clone()),
        error: sub.run.error.clone(),
        waiting_summary: sub.pending_approval.as_ref().map(|a| a.summary.clone()),
        payload: GraphNodePayload::Subagent(Box::new(sub.clone())),
    }
}

fn tool_to_member(step: &TopologyToolStep, now_ms: i64) -> GraphNodeMember {
    let elapsed = step
        .duration_ms
        .or_else(|| span_duration_ms(step.started_ms, step.ended_ms, now_ms));
    let status = map_tool_status(&step.status);
    GraphNodeMember {
        id: format!("tool:{}", step.call_id),
        title: step.name.clone(),
        subtitle: (!step.summary.is_empty()).then(|| step.summary.clone()),
        status,
        elapsed_ms: elapsed,
        role: None,
        subagent_kind: None,
        exit_code: step.exit_code,
        stop_run_id: None,
        error: (status == GraphNodeStatus::Failed).then(|| step.summary.clone()),
        waiting_summary: (status == GraphNodeStatus::Waiting).then(|| step.summary.clone()),
        payload: GraphNodePayload::ToolStep(step.clone()),
    }
}

fn aggregate_member_status(members: &[GraphNodeMember]) -> GraphNodeStatus {
    if members.iter().any(|m| m.status == GraphNodeStatus::Waiting) {
        return GraphNodeStatus::Waiting;
    }
    if members.iter().any(|m| m.status == GraphNodeStatus::Running) {
        return GraphNodeStatus::Running;
    }
    if members.iter().any(|m| m.status.is_terminal_error()) {
        return GraphNodeStatus::Failed;
    }
    if members
        .iter()
        .any(|m| matches!(m.status, GraphNodeStatus::Queued | GraphNodeStatus::Pending))
    {
        return GraphNodeStatus::Queued;
    }
    if !members.is_empty()
        && members
            .iter()
            .all(|m| m.status == GraphNodeStatus::Cancelled)
    {
        return GraphNodeStatus::Cancelled;
    }
    GraphNodeStatus::Completed
}

fn wave_started_ms(wave: &ExecutionWave) -> Option<i64> {
    match wave {
        ExecutionWave::ParallelSubagents { started_ms, .. }
        | ExecutionWave::SingleSubagent { started_ms, .. }
        | ExecutionWave::ParallelTools { started_ms, .. }
        | ExecutionWave::SerialTool { started_ms, .. } => *started_ms,
    }
}

fn span_duration_ms(started_ms: Option<i64>, ended_ms: Option<i64>, now_ms: i64) -> Option<u64> {
    started_ms.map(|s| ended_ms.unwrap_or(now_ms).saturating_sub(s).max(0) as u64)
}

#[must_use]
pub const fn map_run_status(status: AgentRunStatus) -> GraphNodeStatus {
    match status {
        AgentRunStatus::Queued => GraphNodeStatus::Queued,
        AgentRunStatus::Running => GraphNodeStatus::Running,
        AgentRunStatus::WaitingApproval => GraphNodeStatus::Waiting,
        AgentRunStatus::Completed => GraphNodeStatus::Completed,
        AgentRunStatus::Failed => GraphNodeStatus::Failed,
        AgentRunStatus::Cancelled => GraphNodeStatus::Cancelled,
        AgentRunStatus::Interrupted => GraphNodeStatus::Interrupted,
    }
}

#[must_use]
pub fn map_tool_status(status: &str) -> GraphNodeStatus {
    match status {
        "ok" => GraphNodeStatus::Completed,
        "running" => GraphNodeStatus::Running,
        "waiting_approval" | "denied" | "timed_out" => GraphNodeStatus::Waiting,
        "error" => GraphNodeStatus::Failed,
        "cancelled" => GraphNodeStatus::Cancelled,
        "superseded" => GraphNodeStatus::Skipped,
        _ => GraphNodeStatus::Pending,
    }
}

fn infer_tool_node_kind(name: &str, class: &str) -> GraphNodeKind {
    match name {
        "ask_user" | "submit_plan" => GraphNodeKind::Gate,
        "shell" | "run_probe" => GraphNodeKind::Verify,
        "write_file" | "edit_file" | "delete_file" | "move_file" | "create_request" => {
            GraphNodeKind::Implement
        }
        _ if matches!(class, "read" | "pingdeck_read" | "network") => GraphNodeKind::Explore,
        _ => GraphNodeKind::Tool,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use pingdeck_core::agent::dto::{BoardChanges, BoardSubagentCounts};
    use pingdeck_core::agent::model::{AgentRunKind, AgentRunSummary};
    use pingdeck_core::llm::hub::HubUsage;

    fn sample_labels() -> GraphBuildLabels<'static> {
        GraphBuildLabels {
            trigger_title: "Main agent · start",
            summary_title: "Summary & finish",
            joining_title: "Waiting for join",
            parallel_group: "Parallel group",
            kind_tool: "Tool",
        }
    }

    fn sample_run(id: &str, status: AgentRunStatus, iterations: u32) -> AgentRunSummary {
        AgentRunSummary {
            run_id: id.to_owned(),
            session_id: "as_1".to_owned(),
            kind: AgentRunKind::Main,
            parent_run_id: None,
            parent_call_id: None,
            status,
            model_label: "openai/gpt-5".to_owned(),
            target: None,
            iterations,
            usage: HubUsage::default(),
            cost_micro_usd: 0,
            started_at_ms: 1_000,
            finished_at_ms: Some(11_000),
            last_event_ms: Some(11_000),
            last_step: None,
            stop_reason: None,
            error_kind: None,
            error: None,
            prompt_version: 1,
            last_seq: 0,
            role: None,
            status_detail: None,
            cost_currency: None,
            failure: None,
            cap_inapplicable: None,
        }
    }

    fn sample_tool(id: &str, name: &str, status: &str) -> TopologyToolStep {
        TopologyToolStep {
            call_id: id.to_owned(),
            name: name.to_owned(),
            class: "read".to_owned(),
            summary: format!("arg-{id}"),
            status: status.to_owned(),
            started_ms: Some(2_000),
            ended_ms: Some(3_000),
            duration_ms: Some(1_000),
            exit_code: None,
            parallel_safe: true,
        }
    }

    fn sample_sub(id: &str, desc: &str, status: AgentRunStatus) -> TopologySubagent {
        let mut run = sample_run(id, status, 2);
        run.kind = AgentRunKind::Subagent;
        TopologySubagent {
            run,
            parent_call_id: format!("call-{id}"),
            child_session_id: format!("cs-{id}"),
            description: desc.to_owned(),
            subagent_kind: "explore".to_owned(),
            resumed: false,
            awake_ms: 2_500,
            pending_approval: None,
            last_answer: Some("Findings ready".to_owned()),
            tool_calls: vec![
                sample_tool(&format!("{id}-t1"), "grep", "ok"),
                sample_tool(&format!("{id}-t2"), "read_file", "ok"),
            ],
        }
    }

    fn base_topology(
        waves: Vec<ExecutionWave>,
        iterations: u32,
        sub_total: u32,
    ) -> AgentRunTopology {
        AgentRunTopology {
            title: "Test run".to_owned(),
            folder: None,
            run: sample_run("run_main", AgentRunStatus::Completed, iterations),
            awake_ms: 10_000,
            snapshot_ms: 11_000,
            waiting_for_slot: false,
            wait: None,
            pending_approval: None,
            pending_changes: BoardChanges::default(),
            plan_done: 0,
            plan_total: 0,
            cost_currency: Some("USD".to_owned()),
            last_answer: Some("Done".to_owned()),
            queued_turns: 0,
            queue_paused: false,
            waves,
            subagent_counts: BoardSubagentCounts {
                total: sub_total,
                completed: sub_total,
                ..BoardSubagentCounts::default()
            },
        }
    }

    #[test]
    fn adaptive_router_selects_steps_for_direct_and_short_linear_runs() {
        let direct = base_topology(Vec::new(), 1, 0);
        assert!(!is_graph_available(&direct));
        assert_eq!(select_default_view_mode(&direct), RunViewMode::Steps);

        let linear = base_topology(
            vec![
                ExecutionWave::SerialTool {
                    wave_index: 1,
                    started_ms: Some(1_500),
                    ended_ms: Some(2_000),
                    tool: sample_tool("c1", "get_request", "ok"),
                },
                ExecutionWave::SerialTool {
                    wave_index: 2,
                    started_ms: Some(2_100),
                    ended_ms: Some(2_800),
                    tool: sample_tool("c2", "send_request", "ok"),
                },
            ],
            2,
            0,
        );
        assert!(is_graph_available(&linear));
        assert_eq!(select_default_view_mode(&linear), RunViewMode::Steps);
    }

    #[test]
    fn adaptive_router_selects_timeline_for_long_serial_debug_loops() {
        let waves: Vec<ExecutionWave> = (1..=6)
            .map(|i| ExecutionWave::SerialTool {
                wave_index: i,
                started_ms: Some(1_000 + i64::from(i) * 500),
                ended_ms: Some(1_400 + i64::from(i) * 500),
                tool: sample_tool(&format!("c{i}"), "shell", "ok"),
            })
            .collect();
        let debug_loop = base_topology(waves, 6, 0);
        assert_eq!(select_default_view_mode(&debug_loop), RunViewMode::Timeline);
    }

    #[test]
    fn adaptive_router_selects_graph_for_parallel_fanout_and_expands_groups_deterministically() {
        let topo = base_topology(
            vec![
                ExecutionWave::ParallelSubagents {
                    wave_index: 1,
                    started_ms: Some(2_000),
                    ended_ms: Some(5_000),
                    subagents: vec![
                        sample_sub("s1", "Audit router", AgentRunStatus::Completed),
                        sample_sub("s2", "Check tests", AgentRunStatus::Completed),
                    ],
                },
                ExecutionWave::ParallelTools {
                    wave_index: 2,
                    started_ms: Some(5_500),
                    ended_ms: Some(6_200),
                    tools: vec![
                        sample_tool("t1", "read_file", "ok"),
                        sample_tool("t2", "grep", "ok"),
                        sample_tool("t3", "glob", "ok"),
                    ],
                },
            ],
            3,
            2,
        );
        assert_eq!(select_default_view_mode(&topo), RunViewMode::Graph);

        let collapsed = build_graph_from_topology(&topo, &HashSet::new(), 11_000, &sample_labels());
        assert_eq!(collapsed.stages.len(), 4); // Start -> Parallel subs -> Parallel tools -> Summary
        assert_eq!(collapsed.nodes.len(), 5);
        assert!(collapsed.edges.len() >= 5);

        let mut expanded_set = HashSet::new();
        expanded_set.insert("sub:s1".to_owned());
        let expanded = build_graph_from_topology(&topo, &expanded_set, 11_000, &sample_labels());
        let s1_col = collapsed
            .nodes
            .iter()
            .find(|n| n.node.id == "sub:s1")
            .expect("s1 collapsed");
        let s1_exp = expanded
            .nodes
            .iter()
            .find(|n| n.node.id == "sub:s1")
            .expect("s1 expanded");
        let s2_exp = expanded
            .nodes
            .iter()
            .find(|n| n.node.id == "sub:s2")
            .expect("s2 expanded");
        assert!(s1_exp.h > s1_col.h);
        assert!(
            s2_exp.y >= s1_exp.y + s1_exp.h + GRAPH_NODE_GAP_Y - 0.1,
            "expanded card must push sibling down without overlap"
        );
        assert!(matches!(
            expanded.find_selection("tool:s1-t1"),
            Some(SelectedGraphTarget::Member { .. })
        ));
    }

    #[test]
    fn generic_dag_layout_routes_cross_stage_skip_edges_without_intersecting_middle_cards() {
        let mk = |id: &str, deps: &[&str]| GraphNode {
            id: id.to_owned(),
            stage_hint: None,
            kind: GraphNodeKind::Implement,
            title: id.to_owned(),
            subtitle: None,
            status: GraphNodeStatus::Completed,
            elapsed_ms: Some(500),
            role: None,
            subagent_kind: None,
            resumed: false,
            step_count: None,
            exit_code: None,
            error: None,
            waiting_summary: None,
            stop_run_id: None,
            group_tab: None,
            members: Vec::new(),
            expanded: false,
            depends_on: deps.iter().map(|s| (*s).to_owned()).collect(),
            payload: GraphNodePayload::PlannedTask {
                objective: id.to_owned(),
            },
        };
        // A (stage 0) -> B (stage 1) -> C (stage 2), plus direct skip edge A -> C.
        let layout = layout_dag_nodes(vec![mk("A", &[]), mk("B", &["A"]), mk("C", &["A", "B"])]);
        assert_eq!(layout.stages.len(), 3);
        let skip = layout
            .edges
            .iter()
            .find(|e| e.from_id == "A" && e.to_id == "C")
            .expect("skip edge A->C");
        let node_b = layout.nodes.iter().find(|n| n.node.id == "B").expect("B");
        assert_eq!(
            skip.waypoints.len(),
            4,
            "cross-stage skip edge must route via 2 corridor waypoints"
        );
        assert!(
            skip.waypoints[1].1 > node_b.y + node_b.h,
            "skip corridor y={} must clear middle card bottom={}",
            skip.waypoints[1].1,
            node_b.y + node_b.h
        );
    }
}
