//! Headless, UI-framework-agnostic Execution Graph layout engine & adaptive 3-view router.
//!
//! Designed as a reusable Layer 2 layout foundation for:
//! - Phase 1: Runtime [`AgentRunTopology`] rendered as a left-to-right stage Execution Graph
//!   (`执行图`), alongside adaptive routing between `Steps`, `Graph`, and `Timeline`.
//! - Phase 2: Structured `TaskGraph` blueprints (`depends_on`, multi-stage skip edges, parallel
//!   groups, and live run overlay) via the exact same [`layout_dag_nodes`] engine.
//! - Future HTTP `Flows` visual DAG rendering without duplicating layout geometry.

use std::collections::{HashMap, HashSet};

use spanbay_core::agent::dto::{
    AgentRunTopology, ExecutionWave, TopologySubagent, TopologyToolStep,
};
use spanbay_core::agent::model::{
    AgentRunStatus, AgentTodoItem, AgentTodoStatus, AgentToolCallRecord, SubagentRole,
    TaskNodeKind, ToolCallStatus,
};

use super::process_graph::{
    LayoutPolicy, PositionedLaneBand, PositionedPort, ProcessEdgeKind, ProcessEdgeLayer,
    ProcessEdgeSpec, ProcessEdgeTone, ProcessGraphInput, ProcessLaneSpec, ProcessNodeArchetype,
    ProcessNodeSpec, ProcessNodeStatus, layout_process_graph,
};

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
/// Height of one member row inside an expanded parallel group or sub-agent card.
pub const GRAPH_MEMBER_ROW_H: f32 = 30.0;
/// Maximum member rows rendered inline inside a single expanded canvas card (remaining rows are
/// inspected in the docked `Node Inspector`).
pub const GRAPH_MAX_INLINE_MEMBERS: usize = 6;
/// Height of the overflow footer row when a collapsible card has more than [`GRAPH_MAX_INLINE_MEMBERS`].
pub const GRAPH_OVERFLOW_ROW_H: f32 = 24.0;
/// Bottom padding inside an expanded parallel group card.
pub const GRAPH_GROUP_FOOTER_PAD_H: f32 = 10.0;

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
    /// The user said no.
    Denied,
    /// A rule or the mode refused the call.
    Blocked,
    /// The approval timed out; the call did not run.
    TimedOut,
    /// The approval became moot before an answer.
    Expired,
    /// A newer instruction replaced the call.
    Superseded,
}

impl GraphNodeStatus {
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Waiting)
    }

    /// Only a real failure; the "N failed" counts read this.
    #[must_use]
    pub const fn is_terminal_error(self) -> bool {
        matches!(self, Self::Failed)
    }

    /// Ended in a failure or a warning state (interrupted, blocked, timed out).
    #[must_use]
    pub const fn is_warning_or_error(self) -> bool {
        matches!(
            self,
            Self::Failed | Self::Interrupted | Self::Blocked | Self::TimedOut
        )
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
    /// When this member corresponds to a persisted Task Graph node, holds its `AgentTodoItem::id`.
    pub task_id: Option<String>,
    pub attempt: u32,
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
    ToolStep(Box<TopologyToolStep>),
    /// Collapsible parallel group enclosing multiple concurrent tools or sub-agents.
    ParallelGroup {
        subagents: Vec<TopologySubagent>,
        tools: Vec<TopologyToolStep>,
    },
    /// Planned or persisted Task Graph (`任务图`) node with optional checkpoint summary.
    PlannedTask {
        objective: String,
        summary: Option<String>,
    },
}

/// Logical DAG node fed into [`layout_dag_nodes`].
#[derive(Debug, Clone, PartialEq)]
pub struct GraphNode {
    pub id: String,
    /// When this node corresponds to a persisted Task Graph item, holds its `AgentTodoItem::id`.
    pub task_id: Option<String>,
    pub attempt: u32,
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
                let visible_rows = self.members.len().min(GRAPH_MAX_INLINE_MEMBERS);
                let overflow_extra = if self.members.len() > GRAPH_MAX_INLINE_MEMBERS {
                    GRAPH_OVERFLOW_ROW_H
                } else {
                    0.0
                };
                GRAPH_GROUP_TAB_H
                    + GRAPH_GROUP_EXPANDED_HEADER_H
                    + alert_extra
                    + (visible_rows as f32 * GRAPH_MEMBER_ROW_H)
                    + overflow_extra
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
    pub lane_id: Option<String>,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub pinned: bool,
    pub ports: Vec<PositionedPort>,
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
    pub layer: ProcessEdgeLayer,
    pub tone: GraphEdgeTone,
    pub dashed: bool,
    pub horizontal_ports: bool,
    pub label: Option<String>,
    pub label_pos: Option<(f32, f32)>,
    pub insert_pos: Option<(f32, f32)>,
    /// Ordered polyline/curve control anchors: `[start_port, ...waypoints, end_port]`.
    pub waypoints: Vec<(f32, f32)>,
}

/// Metadata for one stage column/row track header.
#[derive(Debug, Clone, PartialEq)]
pub struct GraphStageHeader {
    pub stage_index: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub node_count: usize,
    pub has_parallel: bool,
    pub vertical: bool,
}

/// Complete, deterministically laid-out Execution Graph ready for canvas + card rendering.
#[derive(Debug, Clone, PartialEq)]
pub struct ExecutionGraphLayout {
    pub policy: LayoutPolicy,
    pub lanes: Vec<PositionedLaneBand>,
    pub stages: Vec<GraphStageHeader>,
    pub nodes: Vec<PositionedNode>,
    pub edges: Vec<GraphEdge>,
    pub canvas_width: f32,
    pub canvas_height: f32,
    pub has_pinned: bool,
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
        if let Some(sub_id) = selected_id.strip_prefix("sub:") {
            for pnode in &self.nodes {
                if matches!(
                    &pnode.node.payload,
                    GraphNodePayload::Subagent(sub) if sub.run.run_id == sub_id
                ) {
                    return Some(SelectedGraphTarget::Node(pnode));
                }
                if let Some(member) = pnode.node.members.iter().find(|m| {
                    matches!(
                        &m.payload,
                        GraphNodePayload::Subagent(sub) if sub.run.run_id == sub_id
                    )
                }) {
                    return Some(SelectedGraphTarget::Member {
                        parent: pnode,
                        member,
                    });
                }
            }
        }
        None
    }

    /// Returns `(node_id, title)` pairs of immediate upstream dependencies of `node_id`.
    #[must_use]
    pub fn upstream_nodes(&self, node_id: &str) -> Vec<(String, String)> {
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
                    .map(|n| (n.node.id.clone(), n.node.title.clone()))
            })
            .collect()
    }

    /// Returns `(node_id, title)` pairs of immediate downstream dependents of `node_id`.
    #[must_use]
    pub fn downstream_nodes(&self, node_id: &str) -> Vec<(String, String)> {
        self.nodes
            .iter()
            .filter(|n| n.node.depends_on.iter().any(|dep| dep == node_id))
            .map(|n| (n.node.id.clone(), n.node.title.clone()))
            .collect()
    }

    /// Returns titles of immediate upstream dependencies of `node_id`.
    #[must_use]
    pub fn upstream_titles(&self, node_id: &str) -> Vec<String> {
        self.upstream_nodes(node_id)
            .into_iter()
            .map(|(_, title)| title)
            .collect()
    }

    /// Smart default selection when opening the 90% Workflow Monitor modal:
    /// automatically focuses the highest-priority anomaly node (`Waiting` > `Failed`/`Interrupted`),
    /// returning `None` when the run is healthy so the Execution Graph opens at 100% full width.
    #[must_use]
    pub fn default_focus_node_id(&self) -> Option<String> {
        for pnode in &self.nodes {
            if let Some(m) = pnode
                .node
                .members
                .iter()
                .find(|m| m.status == GraphNodeStatus::Waiting)
            {
                return Some(m.id.clone());
            }
            if pnode.node.status == GraphNodeStatus::Waiting {
                return Some(pnode.node.id.clone());
            }
        }
        for pnode in &self.nodes {
            if let Some(m) = pnode
                .node
                .members
                .iter()
                .find(|m| m.status.is_warning_or_error())
            {
                return Some(m.id.clone());
            }
            if pnode.node.status.is_warning_or_error() {
                return Some(pnode.node.id.clone());
            }
        }
        None
    }
}

/// Whether `tasks` forms a structured Task Graph (`任务图`) worth visualizing as a DAG.
#[must_use]
pub fn has_structured_task_graph(tasks: &[AgentTodoItem]) -> bool {
    if tasks.len() < 2 {
        return false;
    }
    tasks.iter().any(AgentTodoItem::has_graph_metadata) || tasks.len() >= 3
}

/// Whether the Execution Graph (`执行图`) view is meaningful for `topo`.
///
/// Zero-tool direct answers without a structured Task Graph have no stages or edges to visualize,
/// so the UI disables `Graph` and routes directly to the concise `Steps` summary card.
#[must_use]
pub fn is_graph_available(topo: &AgentRunTopology) -> bool {
    !topo.waves.is_empty() || has_structured_task_graph(&topo.task_graph)
}

/// Deterministic adaptive default view mode for an agent run.
///
/// Priority order:
/// 1. Structured Task Graph (`任务图`) with dependencies/groups or `>= 3` items -> [`RunViewMode::Graph`].
/// 2. Parallel sub-agents, multiple sub-agents (`>= 2`), or multi-stage runs (`>= 3` waves) with
///    any parallel batch or sub-agent delegation -> [`RunViewMode::Graph`].
/// 3. Long iterative serial debug loops (`iterations >= 5` or `waves >= 8`) ->
///    [`RunViewMode::Timeline`].
/// 4. Direct answers (0 tools) or short linear runs (1-4 serial steps) -> [`RunViewMode::Steps`].
#[must_use]
pub fn select_default_view_mode(topo: &AgentRunTopology) -> RunViewMode {
    if has_structured_task_graph(&topo.task_graph) {
        return RunViewMode::Graph;
    }
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

/// Builds a read-only Execution Graph preview directly from a structured `Plan` Task Graph before
/// execution starts (reused inside the Chat Plan approval gate).
#[must_use]
pub fn build_plan_preview_graph(
    tasks: &[AgentTodoItem],
    expanded_groups: &HashSet<String>,
    labels: &GraphBuildLabels<'_>,
) -> ExecutionGraphLayout {
    build_task_graph_layout(
        tasks,
        None,
        expanded_groups,
        LayoutPolicy::default(),
        &HashMap::new(),
        0,
        labels,
    )
}

/// Converts a runtime [`AgentRunTopology`] into a laid-out Execution Graph using the default
/// layout policy (`LeftToRight` + `Center`).
#[must_use]
pub fn build_graph_from_topology(
    topo: &AgentRunTopology,
    expanded_groups: &HashSet<String>,
    now_ms: i64,
    labels: &GraphBuildLabels<'_>,
) -> ExecutionGraphLayout {
    build_graph_from_topology_with_policy(
        topo,
        expanded_groups,
        LayoutPolicy::default(),
        &HashMap::new(),
        now_ms,
        labels,
    )
}

/// Converts a runtime [`AgentRunTopology`] into a laid-out Execution Graph honoring the caller's
/// [`LayoutPolicy`] (LR / TB / Serpentine, alignment, role swimlanes) and optional manual drag
/// `pinned_positions`.
#[must_use]
pub fn build_graph_from_topology_with_policy(
    topo: &AgentRunTopology,
    expanded_groups: &HashSet<String>,
    policy: LayoutPolicy,
    pinned_positions: &HashMap<String, (f32, f32)>,
    now_ms: i64,
    labels: &GraphBuildLabels<'_>,
) -> ExecutionGraphLayout {
    if has_structured_task_graph(&topo.task_graph) {
        return build_task_graph_layout(
            &topo.task_graph,
            Some(topo),
            expanded_groups,
            policy,
            pinned_positions,
            now_ms,
            labels,
        );
    }

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
        task_id: None,
        attempt: 0,
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
                        task_id: None,
                        attempt: 0,
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
                let error = members
                    .iter()
                    .position(|m| m.status.is_terminal_error())
                    .and_then(|i| tools.get(i))
                    .map(|t| format!("{} · {}", t.name, t.summary));
                let waiting_summary = members.iter().find_map(|m| m.waiting_summary.clone());
                nodes.push(GraphNode {
                    id: gid.clone(),
                    task_id: None,
                    attempt: 0,
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
                    waiting_summary,
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
                    task_id: None,
                    attempt: 0,
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
                    error: status.is_terminal_error().then(|| {
                        tool.output_preview
                            .clone()
                            .unwrap_or_else(|| tool.summary.clone())
                    }),
                    waiting_summary: (status == GraphNodeStatus::Waiting).then(|| {
                        if tool.summary.is_empty() {
                            tool.name.clone()
                        } else {
                            tool.summary.clone()
                        }
                    }),
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
            task_id: None,
            attempt: 0,
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

    layout_dag_nodes_with_policy(nodes, policy, &agent_role_lanes(labels), pinned_positions)
}

/// Localized labels passed into [`build_graph_from_topology`] so Layer 2 stays language-neutral.
pub struct GraphBuildLabels<'a> {
    pub trigger_title: &'a str,
    pub summary_title: &'a str,
    pub joining_title: &'a str,
    pub parallel_group: &'a str,
    pub kind_tool: &'a str,
    pub lane_main: &'a str,
    pub lane_explore: &'a str,
    pub lane_implement: &'a str,
    pub lane_verify: &'a str,
}

fn agent_role_lanes(labels: &GraphBuildLabels<'_>) -> Vec<ProcessLaneSpec> {
    vec![
        ProcessLaneSpec {
            id: "main".to_owned(),
            title: labels.lane_main.to_owned(),
            accent_index: 0,
        },
        ProcessLaneSpec {
            id: "explore".to_owned(),
            title: labels.lane_explore.to_owned(),
            accent_index: 1,
        },
        ProcessLaneSpec {
            id: "implement".to_owned(),
            title: labels.lane_implement.to_owned(),
            accent_index: 2,
        },
        ProcessLaneSpec {
            id: "verify".to_owned(),
            title: labels.lane_verify.to_owned(),
            accent_index: 3,
        },
    ]
}

fn infer_agent_lane_id(node: &GraphNode) -> &'static str {
    match node.kind {
        GraphNodeKind::Main | GraphNodeKind::Gate => "main",
        GraphNodeKind::Explore => "explore",
        GraphNodeKind::Implement => "implement",
        GraphNodeKind::Verify | GraphNodeKind::Tool => "verify",
        GraphNodeKind::Group => {
            if node.members.iter().any(|m| {
                m.subagent_kind
                    .as_deref()
                    .is_some_and(|k| k.eq_ignore_ascii_case("explore"))
            }) {
                "explore"
            } else if node.members.iter().any(|m| m.subagent_kind.is_some()) {
                "implement"
            } else {
                "verify"
            }
        }
    }
}

const fn map_to_process_status(status: GraphNodeStatus) -> ProcessNodeStatus {
    match status {
        GraphNodeStatus::Pending => ProcessNodeStatus::Pending,
        GraphNodeStatus::Queued => ProcessNodeStatus::Queued,
        GraphNodeStatus::Running => ProcessNodeStatus::Running,
        GraphNodeStatus::Waiting => ProcessNodeStatus::Waiting,
        GraphNodeStatus::Completed => ProcessNodeStatus::Completed,
        GraphNodeStatus::Failed => ProcessNodeStatus::Failed,
        GraphNodeStatus::Cancelled => ProcessNodeStatus::Cancelled,
        GraphNodeStatus::Interrupted | GraphNodeStatus::Blocked | GraphNodeStatus::TimedOut => {
            ProcessNodeStatus::Interrupted
        }
        GraphNodeStatus::Skipped
        | GraphNodeStatus::Denied
        | GraphNodeStatus::Expired
        | GraphNodeStatus::Superseded => ProcessNodeStatus::Skipped,
    }
}

const fn map_to_process_archetype(kind: GraphNodeKind) -> ProcessNodeArchetype {
    match kind {
        GraphNodeKind::Main => ProcessNodeArchetype::Trigger,
        GraphNodeKind::Gate => ProcessNodeArchetype::Decision,
        GraphNodeKind::Group => ProcessNodeArchetype::Group,
        GraphNodeKind::Explore
        | GraphNodeKind::Implement
        | GraphNodeKind::Verify
        | GraphNodeKind::Tool => ProcessNodeArchetype::Action,
    }
}

const fn map_to_process_tone(tone: GraphEdgeTone) -> ProcessEdgeTone {
    match tone {
        GraphEdgeTone::Neutral => ProcessEdgeTone::Neutral,
        GraphEdgeTone::Active => ProcessEdgeTone::Active,
        GraphEdgeTone::Completed => ProcessEdgeTone::Completed,
        GraphEdgeTone::Warning => ProcessEdgeTone::Warning,
        GraphEdgeTone::Danger => ProcessEdgeTone::Danger,
        GraphEdgeTone::Skipped => ProcessEdgeTone::Skipped,
    }
}

const fn map_from_process_tone(tone: ProcessEdgeTone) -> GraphEdgeTone {
    match tone {
        ProcessEdgeTone::Neutral | ProcessEdgeTone::Data => GraphEdgeTone::Neutral,
        ProcessEdgeTone::Active => GraphEdgeTone::Active,
        ProcessEdgeTone::Completed => GraphEdgeTone::Completed,
        ProcessEdgeTone::Warning => GraphEdgeTone::Warning,
        ProcessEdgeTone::Danger => GraphEdgeTone::Danger,
        ProcessEdgeTone::Skipped => GraphEdgeTone::Skipped,
    }
}

/// Pure, reusable layered DAG layout engine backed by [`super::process_graph::layout_process_graph`].
#[must_use]
pub fn layout_dag_nodes(nodes: Vec<GraphNode>) -> ExecutionGraphLayout {
    layout_dag_nodes_with_policy(nodes, LayoutPolicy::default(), &[], &HashMap::new())
}

/// Lays out `nodes` using the unified [`super::process_graph`] engine with custom [`LayoutPolicy`],
/// optional swimlanes, and optional manual drag `pinned_positions`.
#[must_use]
pub fn layout_dag_nodes_with_policy(
    nodes: Vec<GraphNode>,
    policy: LayoutPolicy,
    lanes: &[ProcessLaneSpec],
    pinned_positions: &HashMap<String, (f32, f32)>,
) -> ExecutionGraphLayout {
    if nodes.is_empty() {
        return ExecutionGraphLayout {
            policy,
            lanes: Vec::new(),
            stages: Vec::new(),
            nodes: Vec::new(),
            edges: Vec::new(),
            canvas_width: 0.0,
            canvas_height: 0.0,
            has_pinned: false,
            collapsible_group_ids: Vec::new(),
            total_nodes: 0,
            completed_nodes: 0,
            active_nodes: 0,
            failed_nodes: 0,
        };
    }

    let status_by_id: HashMap<&str, GraphNodeStatus> =
        nodes.iter().map(|n| (n.id.as_str(), n.status)).collect();

    let node_specs: Vec<ProcessNodeSpec> = nodes
        .iter()
        .map(|node| ProcessNodeSpec {
            id: node.id.clone(),
            archetype: map_to_process_archetype(node.kind),
            status: map_to_process_status(node.status),
            lane_id: Some(infer_agent_lane_id(node).to_owned()),
            stage_hint: node.stage_hint,
            size_hint: (GRAPH_NODE_W, node.computed_height()),
            primary_port_offset: Some(node.port_offset_y()),
            ports: Vec::new(),
            is_group_container: !node.members.is_empty() && node.group_tab.is_some(),
            pinned_pos: pinned_positions.get(&node.id).copied(),
        })
        .collect();

    let mut edge_specs: Vec<ProcessEdgeSpec> = Vec::new();
    for node in &nodes {
        for dep_id in &node.depends_on {
            let Some(&from_status) = status_by_id.get(dep_id.as_str()) else {
                continue;
            };
            if dep_id == &node.id {
                continue;
            }
            let (tone, dashed) = classify_edge_style(from_status, node.status);
            edge_specs.push(ProcessEdgeSpec {
                id: format!("edge:{dep_id}->{}", node.id),
                from_node: dep_id.clone(),
                from_port: "out".to_owned(),
                to_node: node.id.clone(),
                to_port: "in".to_owned(),
                layer: ProcessEdgeLayer::Control,
                kind: ProcessEdgeKind::Sequence,
                label: None,
                tone: map_to_process_tone(tone),
                dashed,
                insertable: false,
            });
        }
    }

    let geom = layout_process_graph(&ProcessGraphInput {
        nodes: node_specs,
        edges: edge_specs,
        lanes: lanes.to_vec(),
        policy,
    });

    let mut node_map: HashMap<String, GraphNode> =
        nodes.into_iter().map(|n| (n.id.clone(), n)).collect();
    let positioned: Vec<PositionedNode> = geom
        .nodes
        .into_iter()
        .filter_map(|pn| {
            let node = node_map.remove(&pn.id)?;
            Some(PositionedNode {
                node,
                stage: pn.stage,
                lane_id: pn.lane_id,
                x: pn.x,
                y: pn.y,
                w: pn.w,
                h: pn.h,
                pinned: pn.pinned,
                ports: pn.ports,
                port_in: pn.port_in,
                port_out: pn.port_out,
                has_in_edge: pn.has_in_edge,
                has_out_edge: pn.has_out_edge,
            })
        })
        .collect();

    let stages: Vec<GraphStageHeader> = geom
        .stages
        .into_iter()
        .map(|s| GraphStageHeader {
            stage_index: s.stage_index,
            x: s.x,
            y: s.y,
            w: s.w,
            h: s.h,
            node_count: s.node_count,
            has_parallel: s.has_parallel,
            vertical: s.vertical,
        })
        .collect();

    let edges: Vec<GraphEdge> = geom
        .edges
        .into_iter()
        .map(|e| GraphEdge {
            id: e.id,
            from_id: e.from_node,
            to_id: e.to_node,
            layer: e.layer,
            tone: map_from_process_tone(e.tone),
            dashed: e.dashed,
            horizontal_ports: e.horizontal_ports,
            label: e.label,
            label_pos: e.label_pos,
            insert_pos: e.insert_pos,
            waypoints: e.waypoints,
        })
        .collect();

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

    ExecutionGraphLayout {
        policy: geom.policy,
        lanes: geom.lanes,
        stages,
        nodes: positioned,
        edges,
        canvas_width: geom.canvas_width,
        canvas_height: geom.canvas_height,
        has_pinned: geom.has_pinned_nodes,
        collapsible_group_ids,
        total_nodes,
        completed_nodes,
        active_nodes,
        failed_nodes,
    }
}

fn classify_edge_style(from: GraphNodeStatus, to: GraphNodeStatus) -> (GraphEdgeTone, bool) {
    if matches!(
        to,
        GraphNodeStatus::Skipped
            | GraphNodeStatus::Denied
            | GraphNodeStatus::Expired
            | GraphNodeStatus::Superseded
    ) || from == GraphNodeStatus::Cancelled
    {
        return (GraphEdgeTone::Skipped, true);
    }
    if from.is_terminal_error() || to.is_terminal_error() {
        return (GraphEdgeTone::Danger, false);
    }
    if from == GraphNodeStatus::Waiting
        || to == GraphNodeStatus::Waiting
        || from.is_warning_or_error()
        || to.is_warning_or_error()
    {
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
        task_id: None,
        attempt: 0,
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
        task_id: None,
        attempt: 0,
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
        task_id: None,
        attempt: 0,
        title: step.name.clone(),
        subtitle: (!step.summary.is_empty()).then(|| step.summary.clone()),
        status,
        elapsed_ms: elapsed,
        role: None,
        subagent_kind: None,
        exit_code: step.exit_code,
        stop_run_id: None,
        error: status.is_terminal_error().then(|| {
            step.output_preview
                .clone()
                .unwrap_or_else(|| step.summary.clone())
        }),
        waiting_summary: (status == GraphNodeStatus::Waiting).then(|| {
            if step.summary.is_empty() {
                step.name.clone()
            } else {
                step.summary.clone()
            }
        }),
        payload: GraphNodePayload::ToolStep(Box::new(step.clone())),
    }
}

/// Aggregates child statuses into one group/wave/card status (`docs/design.md` §2.2).
#[must_use]
pub fn aggregate_statuses(statuses: &[GraphNodeStatus]) -> GraphNodeStatus {
    if statuses.contains(&GraphNodeStatus::Waiting) {
        return GraphNodeStatus::Waiting;
    }
    if statuses.contains(&GraphNodeStatus::Running) {
        return GraphNodeStatus::Running;
    }
    if statuses.iter().any(|s| s.is_terminal_error()) {
        return GraphNodeStatus::Failed;
    }
    // The dominant warning: an interruption outranks a timeout, which outranks a block.
    for warn in [
        GraphNodeStatus::Interrupted,
        GraphNodeStatus::TimedOut,
        GraphNodeStatus::Blocked,
    ] {
        if statuses.contains(&warn) {
            return warn;
        }
    }
    if statuses.contains(&GraphNodeStatus::Queued) {
        return GraphNodeStatus::Queued;
    }
    if statuses.contains(&GraphNodeStatus::Pending) {
        return GraphNodeStatus::Pending;
    }
    if !statuses.is_empty() && !statuses.contains(&GraphNodeStatus::Completed) {
        for stopped in [
            GraphNodeStatus::Denied,
            GraphNodeStatus::Cancelled,
            GraphNodeStatus::Expired,
            GraphNodeStatus::Superseded,
            GraphNodeStatus::Skipped,
        ] {
            if statuses.contains(&stopped) {
                return stopped;
            }
        }
    }
    GraphNodeStatus::Completed
}

fn aggregate_member_status(members: &[GraphNodeMember]) -> GraphNodeStatus {
    let statuses: Vec<GraphNodeStatus> = members.iter().map(|m| m.status).collect();
    aggregate_statuses(&statuses)
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

/// Maps a persisted [`AgentTodoStatus`] to [`GraphNodeStatus`].
#[must_use]
pub const fn map_todo_status(status: AgentTodoStatus) -> GraphNodeStatus {
    match status {
        AgentTodoStatus::Pending => GraphNodeStatus::Pending,
        AgentTodoStatus::InProgress => GraphNodeStatus::Running,
        AgentTodoStatus::Completed => GraphNodeStatus::Completed,
        AgentTodoStatus::Cancelled => GraphNodeStatus::Cancelled,
        AgentTodoStatus::Failed => GraphNodeStatus::Failed,
        AgentTodoStatus::Blocked => GraphNodeStatus::Blocked,
    }
}

const fn map_task_kind(kind: Option<TaskNodeKind>) -> GraphNodeKind {
    match kind {
        Some(TaskNodeKind::Explore) => GraphNodeKind::Explore,
        Some(TaskNodeKind::Implement) | None => GraphNodeKind::Implement,
        Some(TaskNodeKind::Verify) => GraphNodeKind::Verify,
        Some(TaskNodeKind::Gate) => GraphNodeKind::Gate,
    }
}

/// Converts a structured [`AgentTodoItem`] Task Graph into a laid-out [`ExecutionGraphLayout`],
/// optionally overlaying live sub-agent runtime telemetry from `topo_opt`.
fn build_task_graph_layout(
    tasks: &[AgentTodoItem],
    topo_opt: Option<&AgentRunTopology>,
    expanded_groups: &HashSet<String>,
    policy: LayoutPolicy,
    pinned_positions: &HashMap<String, (f32, f32)>,
    now_ms: i64,
    labels: &GraphBuildLabels<'_>,
) -> ExecutionGraphLayout {
    let mut sub_by_id: HashMap<&str, &TopologySubagent> = HashMap::new();
    let mut sub_by_desc: HashMap<String, &TopologySubagent> = HashMap::new();
    if let Some(topo) = topo_opt {
        for wave in &topo.waves {
            match wave {
                ExecutionWave::ParallelSubagents { subagents, .. } => {
                    for sub in subagents {
                        sub_by_id.insert(sub.run.run_id.as_str(), sub);
                        sub_by_desc.insert(sub.description.trim().to_lowercase(), sub);
                    }
                }
                ExecutionWave::SingleSubagent { subagent, .. } => {
                    sub_by_id.insert(subagent.run.run_id.as_str(), subagent);
                    sub_by_desc.insert(subagent.description.trim().to_lowercase(), subagent);
                }
                _ => {}
            }
        }
    }

    // Group sibling tasks that share both the same `group` label and identical normalized
    // `depends_on` set when there are >= 2 members.
    let mut bucket_order: Vec<(String, Vec<String>)> = Vec::new();
    let mut buckets: HashMap<(String, Vec<String>), Vec<usize>> = HashMap::new();
    for (idx, item) in tasks.iter().enumerate() {
        if let Some(group_lbl) = &item.group {
            let mut deps = item.depends_on.clone();
            deps.sort();
            deps.dedup();
            let key = (group_lbl.clone(), deps);
            if !buckets.contains_key(&key) {
                bucket_order.push(key.clone());
            }
            buckets.entry(key).or_default().push(idx);
        }
    }

    let mut owner_node_id: HashMap<String, String> = HashMap::with_capacity(tasks.len());
    let mut grouped_indices: HashSet<usize> = HashSet::new();
    let mut group_specs: Vec<(String, String, Vec<String>, Vec<usize>)> = Vec::new();

    for (g_ord, key) in bucket_order.into_iter().enumerate() {
        if let Some(indices) = buckets.remove(&key)
            && indices.len() >= 2
        {
            let gid = format!("tgrp:{}:{g_ord}", key.0);
            for &i in &indices {
                grouped_indices.insert(i);
                owner_node_id.insert(tasks[i].id.clone(), gid.clone());
            }
            group_specs.push((gid, key.0, key.1, indices));
        }
    }
    for (idx, item) in tasks.iter().enumerate() {
        if !grouped_indices.contains(&idx) {
            owner_node_id.insert(item.id.clone(), format!("task:{}", item.id));
        }
    }

    let resolve_deps = |raw_deps: &[String], self_node_id: &str| -> Vec<String> {
        let mut seen = HashSet::new();
        let mut out = Vec::new();
        for dep in raw_deps {
            if let Some(mapped) = owner_node_id.get(dep)
                && mapped != self_node_id
                && seen.insert(mapped.clone())
            {
                out.push(mapped.clone());
            }
        }
        out
    };

    let match_sub = |item: &AgentTodoItem| -> Option<&TopologySubagent> {
        item.run_id
            .as_deref()
            .and_then(|rid| sub_by_id.get(rid).copied())
            .or_else(|| {
                sub_by_desc
                    .get(&item.content.trim().to_lowercase())
                    .copied()
            })
    };

    let mut nodes: Vec<GraphNode> = Vec::new();
    let mut emitted_groups: HashSet<String> = HashSet::new();
    let group_by_first_idx: HashMap<usize, usize> = group_specs
        .iter()
        .enumerate()
        .filter_map(|(gi, (_, _, _, idxs))| idxs.first().map(|&first| (first, gi)))
        .collect();

    for (idx, item) in tasks.iter().enumerate() {
        if let Some(&gi) = group_by_first_idx.get(&idx) {
            let (gid, group_lbl, raw_deps, indices) = &group_specs[gi];
            if emitted_groups.insert(gid.clone()) {
                let expanded = expanded_groups.contains(gid);
                let mut members = Vec::with_capacity(indices.len());
                let mut matched_subs = Vec::new();
                for &mi in indices {
                    let m_item = &tasks[mi];
                    let sub_opt = match_sub(m_item);
                    if let Some(s) = sub_opt {
                        matched_subs.push((*s).clone());
                    }
                    let (status, elapsed_ms, stop_run_id, waiting, error, payload) =
                        task_runtime_overlay(m_item, sub_opt, now_ms);
                    members.push(GraphNodeMember {
                        id: format!("task:{}", m_item.id),
                        task_id: Some(m_item.id.clone()),
                        attempt: m_item.attempt,
                        title: m_item.content.clone(),
                        subtitle: m_item.summary.clone(),
                        status,
                        elapsed_ms,
                        role: m_item.role.or_else(|| sub_opt.and_then(|s| s.run.role)),
                        subagent_kind: sub_opt.map(|s| s.subagent_kind.clone()),
                        exit_code: None,
                        stop_run_id,
                        error,
                        waiting_summary: waiting,
                        payload,
                    });
                }
                let status = aggregate_member_status(&members);
                let elapsed_ms = members.iter().filter_map(|m| m.elapsed_ms).max();
                let waiting = members.iter().find_map(|m| m.waiting_summary.clone());
                let error = members.iter().find_map(|m| m.error.clone());
                nodes.push(GraphNode {
                    id: gid.clone(),
                    task_id: None,
                    attempt: 0,
                    stage_hint: None,
                    kind: GraphNodeKind::Group,
                    title: group_lbl.clone(),
                    subtitle: None,
                    status,
                    elapsed_ms,
                    role: None,
                    subagent_kind: None,
                    resumed: false,
                    step_count: Some(members.len() as u32),
                    exit_code: None,
                    error,
                    waiting_summary: waiting,
                    stop_run_id: None,
                    group_tab: Some(format!("{} · {group_lbl}", labels.parallel_group)),
                    members,
                    expanded,
                    depends_on: resolve_deps(raw_deps, gid),
                    payload: GraphNodePayload::ParallelGroup {
                        subagents: matched_subs,
                        tools: Vec::new(),
                    },
                });
            }
            continue;
        }
        if grouped_indices.contains(&idx) {
            continue;
        }

        let nid = format!("task:{}", item.id);
        let sub_opt = match_sub(item);
        let (status, elapsed_ms, stop_run_id, waiting, error, payload) =
            task_runtime_overlay(item, sub_opt, now_ms);
        let members: Vec<GraphNodeMember> = sub_opt
            .map(|s| {
                s.tool_calls
                    .iter()
                    .map(|t| tool_to_member(t, now_ms))
                    .collect()
            })
            .unwrap_or_default();
        let expanded = expanded_groups.contains(&nid);
        let group_tab = if !members.is_empty() {
            item.group
                .clone()
                .or_else(|| sub_opt.map(|s| s.subagent_kind.clone()))
        } else {
            None
        };
        let subtitle = item
            .summary
            .clone()
            .or_else(|| sub_opt.map(|s| s.run.model_label.clone()));

        nodes.push(GraphNode {
            id: nid.clone(),
            task_id: Some(item.id.clone()),
            attempt: item.attempt,
            stage_hint: None,
            kind: map_task_kind(item.kind),
            title: item.content.clone(),
            subtitle,
            status,
            elapsed_ms,
            role: item.role.or_else(|| sub_opt.and_then(|s| s.run.role)),
            subagent_kind: sub_opt.map(|s| s.subagent_kind.clone()),
            resumed: sub_opt.is_some_and(|s| s.resumed),
            step_count: sub_opt.map(|s| s.tool_calls.len() as u32),
            exit_code: None,
            error,
            waiting_summary: waiting,
            stop_run_id,
            group_tab,
            members,
            expanded,
            depends_on: resolve_deps(&item.depends_on, &nid),
            payload,
        });
    }

    // For legacy linear `todos` with no explicit `depends_on` edges, chain them sequentially
    // so the Execution Graph shows clean stage progression rather than stacking all items in Stage 0.
    let any_edge = nodes.iter().any(|n| !n.depends_on.is_empty());
    if !any_edge && nodes.len() >= 2 {
        for i in 1..nodes.len() {
            let prev = nodes[i - 1].id.clone();
            nodes[i].depends_on.push(prev);
        }
    }

    layout_dag_nodes_with_policy(nodes, policy, &agent_role_lanes(labels), pinned_positions)
}

fn task_runtime_overlay(
    item: &AgentTodoItem,
    sub_opt: Option<&TopologySubagent>,
    now_ms: i64,
) -> (
    GraphNodeStatus,
    Option<u64>,
    Option<String>,
    Option<String>,
    Option<String>,
    GraphNodePayload,
) {
    if let Some(sub) = sub_opt {
        let status = match item.status {
            AgentTodoStatus::Completed => GraphNodeStatus::Completed,
            AgentTodoStatus::Failed => GraphNodeStatus::Failed,
            AgentTodoStatus::Cancelled => GraphNodeStatus::Cancelled,
            AgentTodoStatus::Blocked => GraphNodeStatus::Blocked,
            _ => map_run_status(sub.run.status),
        };
        let elapsed = if sub.run.status.is_active() {
            (now_ms - sub.run.started_at_ms).max(sub.awake_ms as i64) as u64
        } else {
            sub.awake_ms
        };
        return (
            status,
            Some(elapsed),
            sub.run.status.is_active().then(|| sub.run.run_id.clone()),
            sub.pending_approval.as_ref().map(|a| a.summary.clone()),
            item.error.clone().or_else(|| sub.run.error.clone()),
            GraphNodePayload::Subagent(Box::new(sub.clone())),
        );
    }
    (
        map_todo_status(item.status),
        None,
        None,
        None,
        item.error.clone(),
        GraphNodePayload::PlannedTask {
            objective: item.content.clone(),
            summary: item.summary.clone(),
        },
    )
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

/// Maps a topology tool status string (`spanbay-app-agent` `topology.rs`).
#[must_use]
pub fn map_tool_status(status: &str) -> GraphNodeStatus {
    match status {
        "ok" => GraphNodeStatus::Completed,
        "running" => GraphNodeStatus::Running,
        "waiting_approval" => GraphNodeStatus::Waiting,
        "denied" => GraphNodeStatus::Denied,
        "blocked" => GraphNodeStatus::Blocked,
        "timed_out" => GraphNodeStatus::TimedOut,
        "error" => GraphNodeStatus::Failed,
        "interrupted" => GraphNodeStatus::Interrupted,
        "cancelled" => GraphNodeStatus::Cancelled,
        "expired" => GraphNodeStatus::Expired,
        "superseded" => GraphNodeStatus::Superseded,
        _ => GraphNodeStatus::Pending,
    }
}

/// Maps how a live tool call ended.
#[must_use]
pub const fn map_tool_call_status(status: ToolCallStatus) -> GraphNodeStatus {
    match status {
        ToolCallStatus::Ok => GraphNodeStatus::Completed,
        ToolCallStatus::Error => GraphNodeStatus::Failed,
        ToolCallStatus::Interrupted => GraphNodeStatus::Interrupted,
        ToolCallStatus::Denied => GraphNodeStatus::Denied,
        ToolCallStatus::Blocked => GraphNodeStatus::Blocked,
        ToolCallStatus::Cancelled => GraphNodeStatus::Cancelled,
        ToolCallStatus::ApprovalTimedOut => GraphNodeStatus::TimedOut,
        ToolCallStatus::Expired => GraphNodeStatus::Expired,
        ToolCallStatus::Superseded => GraphNodeStatus::Superseded,
    }
}

/// Whether `decided_by` names a policy/rule/mode block rather than a user denial.
#[must_use]
pub fn is_blocked_decider(decided_by: Option<&str>) -> bool {
    matches!(decided_by, Some("mode" | "plan"))
        || decided_by.is_some_and(|by| by.starts_with("hard_block:") || by.starts_with("block:"))
}

/// Maps a stored audit row. The table keeps Interrupted as `output-error` (or an unfinished state
/// when the process crashed) and Blocked as `output-denied`; `placeholder` (the result is the
/// crash-recovery marker) and `decided_by` tell them apart.
#[must_use]
pub fn map_audit_tool_state(record: &AgentToolCallRecord, placeholder: bool) -> GraphNodeStatus {
    match record.state.as_str() {
        "output-available" => GraphNodeStatus::Completed,
        "output-error" if placeholder => GraphNodeStatus::Interrupted,
        "output-error" => GraphNodeStatus::Failed,
        "output-denied" if is_blocked_decider(record.decided_by.as_deref()) => {
            GraphNodeStatus::Blocked
        }
        "output-denied" => GraphNodeStatus::Denied,
        "output-cancelled" => GraphNodeStatus::Cancelled,
        "approval-timed-out" => GraphNodeStatus::TimedOut,
        "approval-expired" => GraphNodeStatus::Expired,
        "output-superseded" => GraphNodeStatus::Superseded,
        _ if placeholder => GraphNodeStatus::Interrupted,
        "approval-requested" => GraphNodeStatus::Waiting,
        _ => GraphNodeStatus::Running,
    }
}

/// Fallback node synthesized from [`AgentRunTopology`] when a `Steps` or `Timeline` selection is
/// not directly present in the active graph layout.
#[derive(Debug, Clone, PartialEq)]
pub struct FallbackGraphSelection {
    pub node: GraphNode,
    pub stage: usize,
    pub parent_crumb: Option<(String, String)>,
}

/// Synthesizes a standalone [`GraphNode`] (with its stage index and optional parent breadcrumb)
/// from `topo` when `selected_id` comes from `Steps` or `Timeline` (`main-start-*`,
/// `main-end-*`, `sub:*`, `tool:*`) and is not directly present in the active graph layout
/// (e.g. structured Task Graph mode or >4 parallel sub-agents).
#[must_use]
pub fn synthesize_fallback_node(
    topo: &AgentRunTopology,
    selected_id: &str,
    now_ms: i64,
    labels: &GraphBuildLabels<'_>,
) -> Option<FallbackGraphSelection> {
    if selected_id == format!("main-start-{}", topo.run.run_id) {
        let run_start = topo.run.started_at_ms;
        let first_wave_start = topo
            .waves
            .first()
            .and_then(wave_started_ms)
            .unwrap_or(run_start);
        let trigger_ms = (first_wave_start - run_start).max(0) as u64;
        let status = if topo.waves.is_empty() {
            map_run_status(topo.run.status)
        } else {
            GraphNodeStatus::Completed
        };
        return Some(FallbackGraphSelection {
            node: GraphNode {
                id: selected_id.to_owned(),
                task_id: None,
                attempt: 0,
                stage_hint: Some(0),
                kind: GraphNodeKind::Main,
                title: labels.trigger_title.to_owned(),
                subtitle: Some(topo.run.model_label.clone()),
                status,
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
            },
            stage: 0,
            parent_crumb: None,
        });
    }
    if selected_id == format!("main-end-{}", topo.run.run_id) {
        let term_active = topo.run.status.is_active();
        let total_elapsed = if term_active {
            (now_ms - topo.run.started_at_ms).max(topo.awake_ms as i64) as u64
        } else {
            topo.awake_ms
        };
        let stage = topo.waves.len() + 1;
        return Some(FallbackGraphSelection {
            node: GraphNode {
                id: selected_id.to_owned(),
                task_id: None,
                attempt: 0,
                stage_hint: Some(stage),
                kind: GraphNodeKind::Main,
                title: if term_active {
                    labels.joining_title.to_owned()
                } else {
                    labels.summary_title.to_owned()
                },
                subtitle: topo.last_answer.clone(),
                status: map_run_status(topo.run.status),
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
                depends_on: Vec::new(),
                payload: GraphNodePayload::MainRun { is_summary: true },
            },
            stage,
            parent_crumb: None,
        });
    }
    if let Some(sub_id) = selected_id.strip_prefix("sub:") {
        for (w_idx, wave) in topo.waves.iter().enumerate() {
            match wave {
                ExecutionWave::ParallelSubagents { subagents, .. } => {
                    if let Some(sub) = subagents.iter().find(|s| s.run.run_id == sub_id) {
                        let node = subagent_to_graph_node(
                            selected_id.to_owned(),
                            w_idx + 1,
                            sub,
                            &[],
                            false,
                            now_ms,
                            None,
                        );
                        return Some(FallbackGraphSelection {
                            node,
                            stage: w_idx + 1,
                            parent_crumb: None,
                        });
                    }
                }
                ExecutionWave::SingleSubagent { subagent, .. } if subagent.run.run_id == sub_id => {
                    let node = subagent_to_graph_node(
                        selected_id.to_owned(),
                        w_idx + 1,
                        subagent,
                        &[],
                        false,
                        now_ms,
                        None,
                    );
                    return Some(FallbackGraphSelection {
                        node,
                        stage: w_idx + 1,
                        parent_crumb: None,
                    });
                }
                _ => {}
            }
        }
    }
    if let Some(call_id) = selected_id.strip_prefix("tool:") {
        let build_tool_node = |step: &TopologyToolStep, stage: usize| -> GraphNode {
            let status = map_tool_status(&step.status);
            let elapsed = step
                .duration_ms
                .or_else(|| span_duration_ms(step.started_ms, step.ended_ms, now_ms));
            GraphNode {
                id: selected_id.to_owned(),
                task_id: None,
                attempt: 0,
                stage_hint: Some(stage),
                kind: infer_tool_node_kind(&step.name, &step.class),
                title: step.name.clone(),
                subtitle: (!step.summary.is_empty()).then(|| step.summary.clone()),
                status,
                elapsed_ms: elapsed,
                role: None,
                subagent_kind: None,
                resumed: false,
                step_count: None,
                exit_code: step.exit_code,
                error: status.is_terminal_error().then(|| {
                    step.output_preview
                        .clone()
                        .unwrap_or_else(|| step.summary.clone())
                }),
                waiting_summary: (status == GraphNodeStatus::Waiting).then(|| {
                    if step.summary.is_empty() {
                        step.name.clone()
                    } else {
                        step.summary.clone()
                    }
                }),
                stop_run_id: None,
                group_tab: None,
                members: Vec::new(),
                expanded: false,
                depends_on: Vec::new(),
                payload: GraphNodePayload::ToolStep(Box::new(step.clone())),
            }
        };
        for (w_idx, wave) in topo.waves.iter().enumerate() {
            let stage = w_idx + 1;
            match wave {
                ExecutionWave::SerialTool { tool, .. } if tool.call_id == call_id => {
                    return Some(FallbackGraphSelection {
                        node: build_tool_node(tool, stage),
                        stage,
                        parent_crumb: None,
                    });
                }
                ExecutionWave::ParallelTools { tools, .. } => {
                    if let Some(step) = tools.iter().find(|t| t.call_id == call_id) {
                        return Some(FallbackGraphSelection {
                            node: build_tool_node(step, stage),
                            stage,
                            parent_crumb: None,
                        });
                    }
                }
                ExecutionWave::ParallelSubagents { subagents, .. } => {
                    for sub in subagents {
                        if let Some(step) = sub.tool_calls.iter().find(|t| t.call_id == call_id) {
                            let crumb =
                                (format!("sub:{}", sub.run.run_id), sub.description.clone());
                            return Some(FallbackGraphSelection {
                                node: build_tool_node(step, stage),
                                stage,
                                parent_crumb: Some(crumb),
                            });
                        }
                    }
                }
                ExecutionWave::SingleSubagent { subagent, .. } => {
                    if let Some(step) = subagent.tool_calls.iter().find(|t| t.call_id == call_id) {
                        let crumb = (
                            format!("sub:{}", subagent.run.run_id),
                            subagent.description.clone(),
                        );
                        return Some(FallbackGraphSelection {
                            node: build_tool_node(step, stage),
                            stage,
                            parent_crumb: Some(crumb),
                        });
                    }
                }
                _ => {}
            }
        }
    }
    None
}

fn infer_tool_node_kind(name: &str, class: &str) -> GraphNodeKind {
    match name {
        "ask_user" | "submit_plan" => GraphNodeKind::Gate,
        "shell" | "run_probe" => GraphNodeKind::Verify,
        "write_file" | "edit_file" | "delete_file" | "move_file" | "create_request" => {
            GraphNodeKind::Implement
        }
        _ if matches!(class, "read" | "spanbay_read" | "network") => GraphNodeKind::Explore,
        _ => GraphNodeKind::Tool,
    }
}

#[cfg(test)]
mod tests {
    use super::*;
    use spanbay_core::agent::dto::{BoardChanges, BoardSubagentCounts};
    use spanbay_core::agent::model::{AgentRunKind, AgentRunSummary};
    use spanbay_core::llm::hub::HubUsage;

    fn sample_labels() -> GraphBuildLabels<'static> {
        GraphBuildLabels {
            trigger_title: "Main agent · start",
            summary_title: "Summary & finish",
            joining_title: "Waiting for join",
            parallel_group: "Parallel group",
            kind_tool: "Tool",
            lane_main: "Main",
            lane_explore: "Explore",
            lane_implement: "Implement",
            lane_verify: "Verify",
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
            ..TopologyToolStep::default()
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
            brief: Some(format!("Investigate {desc}")),
            full_report: Some(format!("## Findings\nAll clear for {desc}")),
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
            task_graph: Vec::new(),
            user_prompt: Some("Audit auth".to_owned()),
            reasoning_preview: None,
            full_answer: Some("Done".to_owned()),
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
                    tool: Box::new(sample_tool("c1", "get_request", "ok")),
                },
                ExecutionWave::SerialTool {
                    wave_index: 2,
                    started_ms: Some(2_100),
                    ended_ms: Some(2_800),
                    tool: Box::new(sample_tool("c2", "send_request", "ok")),
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
                tool: Box::new(sample_tool(&format!("c{i}"), "shell", "ok")),
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
            task_id: None,
            attempt: 0,
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
                summary: None,
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

    #[test]
    fn task_graph_groups_parallel_siblings_routes_default_to_graph_and_overlays_subagents() {
        let tasks = vec![
            AgentTodoItem {
                id: "n1".to_owned(),
                content: "Audit router".to_owned(),
                status: AgentTodoStatus::Completed,
                kind: Some(TaskNodeKind::Explore),
                role: Some(SubagentRole::Fast),
                group: Some("Audit".to_owned()),
                ..AgentTodoItem::default()
            },
            AgentTodoItem {
                id: "n2".to_owned(),
                content: "Audit store".to_owned(),
                status: AgentTodoStatus::InProgress,
                kind: Some(TaskNodeKind::Explore),
                role: Some(SubagentRole::Fast),
                group: Some("Audit".to_owned()),
                ..AgentTodoItem::default()
            },
            AgentTodoItem {
                id: "n3".to_owned(),
                content: "Implement fix".to_owned(),
                status: AgentTodoStatus::Pending,
                depends_on: vec!["n1".to_owned(), "n2".to_owned()],
                kind: Some(TaskNodeKind::Implement),
                ..AgentTodoItem::default()
            },
            AgentTodoItem {
                id: "n4".to_owned(),
                content: "Run tests".to_owned(),
                status: AgentTodoStatus::Pending,
                depends_on: vec!["n3".to_owned()],
                kind: Some(TaskNodeKind::Verify),
                ..AgentTodoItem::default()
            },
        ];

        // Even before any runtime waves arrive, structured TaskGraph enables & defaults to Graph.
        let mut topo = base_topology(
            vec![
                ExecutionWave::SingleSubagent {
                    wave_index: 1,
                    started_ms: Some(2_000),
                    ended_ms: Some(4_500),
                    subagent: Box::new(sample_sub("s1", "Audit router", AgentRunStatus::Completed)),
                },
                ExecutionWave::SerialTool {
                    wave_index: 2,
                    started_ms: Some(5_000),
                    ended_ms: Some(5_500),
                    tool: Box::new(sample_tool("parent_sh", "shell", "ok")),
                },
            ],
            2,
            1,
        );
        topo.task_graph = tasks.clone();
        assert!(is_graph_available(&topo));
        assert_eq!(select_default_view_mode(&topo), RunViewMode::Graph);

        let preview = build_plan_preview_graph(&tasks, &HashSet::new(), &sample_labels());
        assert_eq!(preview.stages.len(), 3); // [Group(Audit)] -> [Implement] -> [Verify]
        assert_eq!(preview.nodes.len(), 3);
        assert_eq!(preview.collapsible_group_ids.len(), 1);
        assert!(matches!(
            preview.find_selection("task:n1"),
            Some(SelectedGraphTarget::Member { member, .. }) if member.task_id.as_deref() == Some("n1")
        ));

        let live_graph =
            build_graph_from_topology(&topo, &HashSet::new(), 11_000, &sample_labels());
        assert!(matches!(
            live_graph.find_selection("sub:s1"),
            Some(SelectedGraphTarget::Member { member, .. }) if member.task_id.as_deref() == Some("n1")
        ));
        assert!(
            synthesize_fallback_node(&topo, "main-start-run_main", 11_000, &sample_labels())
                .is_some()
        );
        assert!(
            synthesize_fallback_node(&topo, "tool:parent_sh", 11_000, &sample_labels()).is_some()
        );
        assert!(
            synthesize_fallback_node(&topo, "tool:s1-t1", 11_000, &sample_labels())
                .is_some_and(|fb| fb.parent_crumb.is_some())
        );
    }

    #[test]
    fn audit_rows_and_topology_strings_keep_every_tool_outcome_apart() {
        let row = |state: &str, decided_by: Option<&str>| AgentToolCallRecord {
            session_id: "s".into(),
            call_id: "c".into(),
            run_id: "r".into(),
            message_id: None,
            name: "shell".into(),
            class: "exec".into(),
            state: state.into(),
            args_json: "{}".into(),
            risk: "low".into(),
            decided_by: decided_by.map(str::to_owned),
            decision: None,
            decided_ms: None,
            started_ms: None,
            ended_ms: None,
            exit_code: None,
            sandbox: None,
            truncated: false,
            output_bytes: 0,
            spill_ref: None,
            requested_ms: None,
        };
        let cases = [
            ("output-available", None, false, GraphNodeStatus::Completed),
            ("output-error", None, false, GraphNodeStatus::Failed),
            ("output-error", None, true, GraphNodeStatus::Interrupted),
            (
                "output-denied",
                Some("user"),
                false,
                GraphNodeStatus::Denied,
            ),
            (
                "output-denied",
                Some("hard_block:sudo"),
                false,
                GraphNodeStatus::Blocked,
            ),
            (
                "output-denied",
                Some("block:tool"),
                false,
                GraphNodeStatus::Blocked,
            ),
            (
                "output-denied",
                Some("block:rule:r1"),
                false,
                GraphNodeStatus::Blocked,
            ),
            (
                "output-denied",
                Some("plan"),
                false,
                GraphNodeStatus::Blocked,
            ),
            (
                "output-denied",
                Some("mode"),
                false,
                GraphNodeStatus::Blocked,
            ),
            ("output-cancelled", None, false, GraphNodeStatus::Cancelled),
            ("approval-timed-out", None, false, GraphNodeStatus::TimedOut),
            ("approval-expired", None, false, GraphNodeStatus::Expired),
            (
                "output-superseded",
                None,
                false,
                GraphNodeStatus::Superseded,
            ),
            ("approval-requested", None, false, GraphNodeStatus::Waiting),
            (
                "approval-requested",
                None,
                true,
                GraphNodeStatus::Interrupted,
            ),
            ("input-available", None, true, GraphNodeStatus::Interrupted),
            ("input-streaming", None, false, GraphNodeStatus::Running),
        ];
        for (state, by, placeholder, want) in cases {
            assert_eq!(
                map_audit_tool_state(&row(state, by), placeholder),
                want,
                "{state} {by:?} placeholder={placeholder}"
            );
        }
        for (s, want) in [
            ("denied", GraphNodeStatus::Denied),
            ("blocked", GraphNodeStatus::Blocked),
            ("timed_out", GraphNodeStatus::TimedOut),
            ("expired", GraphNodeStatus::Expired),
            ("superseded", GraphNodeStatus::Superseded),
            ("interrupted", GraphNodeStatus::Interrupted),
        ] {
            assert_eq!(map_tool_status(s), want);
        }
    }

    #[test]
    fn an_interrupted_tool_marks_its_group_without_counting_as_a_failure() {
        let members: Vec<GraphNodeMember> = [
            sample_tool("a", "grep", "ok"),
            sample_tool("b", "shell", "interrupted"),
        ]
        .iter()
        .map(|t| tool_to_member(t, 0))
        .collect();
        let status = aggregate_member_status(&members);
        assert_eq!(status, GraphNodeStatus::Interrupted);
        assert!(!status.is_terminal_error());
        assert!(
            members[1].error.is_none(),
            "warning states do not populate the red failure banner"
        );
        assert_eq!(
            aggregate_statuses(&[GraphNodeStatus::Pending, GraphNodeStatus::Pending]),
            GraphNodeStatus::Pending
        );
        for stopped in [
            GraphNodeStatus::Denied,
            GraphNodeStatus::Expired,
            GraphNodeStatus::Superseded,
            GraphNodeStatus::Skipped,
            GraphNodeStatus::Cancelled,
        ] {
            assert_eq!(aggregate_statuses(&[stopped, stopped]), stopped);
        }
    }
}
