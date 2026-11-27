//! Domain-agnostic intermediate representation (IR) for dependency-aware process graphs.
//!
//! Designed around four principles:
//! 1. **Heterogeneous nodes**: nodes declare their own `(w, h)` size hints and port specs rather
//!    than assuming uniform fixed boxes.
//! 2. **First-class layered edges**: `Control` edges drive topology and main connectors;
//!    `DataFlow` edges render soft variable/artifact arcs without distorting layout stages.
//! 3. **Pluggable layout policies**: callers choose direction (`LR` / `TB` / `Serpentine`),
//!    cross-axis alignment (`Start` / `Center` / `Barycenter`), optional swimlane bands, and
//!    optional manual pinned coordinates.
//! 4. **Blueprint + Execution overlay**: the same graph model represents pure design-time
//!    blueprints and live/historical execution overlays.

use std::collections::HashMap;

use serde::{Deserialize, Serialize};

/// Primary flow direction of a process graph canvas.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum LayoutDirection {
    /// Classic left-to-right stage columns (GitHub Actions style).
    #[default]
    LeftToRight,
    /// Top-to-bottom vertical stages (ideal beside a docked editor inspector).
    TopToBottom,
    /// Multi-row horizontal serpentine wrap (ideal for 5-16 step pipelines without wide panning).
    Serpentine { max_per_row: usize },
}

impl LayoutDirection {
    /// Default serpentine wrap width (4 stages per row fits comfortably on 1280-1600px screens).
    pub const DEFAULT_SERPENTINE_COLS: usize = 4;

    #[must_use]
    pub const fn is_vertical(self) -> bool {
        matches!(self, Self::TopToBottom)
    }

    #[must_use]
    pub const fn is_serpentine(self) -> bool {
        matches!(self, Self::Serpentine { .. })
    }
}

/// How sibling nodes within a stage track align along the cross axis.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum CrossAlignment {
    /// Top-aligned (in LR) or left-aligned (in TB).
    Start,
    /// Symmetrically centered around the graph's cross-axis midline (best for diamond fan-out/join).
    #[default]
    Center,
    /// Pulls nodes toward the barycenter of their upstream dependencies while preventing overlap.
    Barycenter,
}

/// Visibility mode for secondary data-flow dependency arcs (e.g. `{{token}}` extraction edges).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize, Default)]
#[serde(rename_all = "snake_case")]
pub enum DataFlowVisibility {
    /// Hidden by default, illuminated when hovering/selecting a connected node.
    #[default]
    OnFocus,
    /// Always rendered softly on the canvas.
    Always,
    /// Disabled completely.
    Hidden,
}

/// Complete layout strategy controlling direction, alignment, swimlanes, and data-flow display.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
pub struct LayoutPolicy {
    #[serde(default)]
    pub direction: LayoutDirection,
    #[serde(default)]
    pub alignment: CrossAlignment,
    #[serde(default)]
    pub swimlanes: bool,
    #[serde(default)]
    pub data_flow: DataFlowVisibility,
}

impl Default for LayoutPolicy {
    fn default() -> Self {
        Self {
            direction: LayoutDirection::LeftToRight,
            alignment: CrossAlignment::Center,
            swimlanes: false,
            data_flow: DataFlowVisibility::OnFocus,
        }
    }
}

/// Semantic shape archetype for heterogeneous workflow nodes.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ProcessNodeArchetype {
    Trigger,
    #[default]
    Action,
    Decision,
    Join,
    Group,
    Summary,
}

/// Unified execution lifecycle status across all SpanBay process workflows.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ProcessNodeStatus {
    #[default]
    Idle,
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

impl ProcessNodeStatus {
    #[must_use]
    pub const fn is_active(self) -> bool {
        matches!(self, Self::Queued | Self::Running | Self::Waiting)
    }

    #[must_use]
    pub const fn is_terminal_error(self) -> bool {
        matches!(self, Self::Failed | Self::Interrupted)
    }
}

/// Whether an edge defines control-flow topology or secondary data-flow dependencies.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ProcessEdgeLayer {
    /// Drives topological stage placement and renders as primary connectors.
    #[default]
    Control,
    /// Does not distort topological layering; renders as secondary arc (e.g. `{{token}}`).
    DataFlow,
}

/// Semantic kind of an edge connector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ProcessEdgeKind {
    #[default]
    Sequence,
    Conditional,
    ParallelFork,
    ParallelJoin,
    OnError,
    DataFlow,
}

/// Semantic visual tone for a port, edge, or timeline segment.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Default)]
pub enum ProcessEdgeTone {
    #[default]
    Neutral,
    Active,
    Completed,
    Warning,
    Danger,
    Skipped,
    Data,
}

/// Direction of an anchor port on a node card.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum ProcessPortDirection {
    Input,
    Output,
}

/// Logical port specification attached to a node (supports multi-output conditional branches).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessPortSpec {
    pub id: String,
    pub direction: ProcessPortDirection,
    pub label: Option<String>,
    pub tone: ProcessEdgeTone,
    pub allow_quick_add: bool,
}

impl ProcessPortSpec {
    #[must_use]
    pub fn input(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            direction: ProcessPortDirection::Input,
            label: None,
            tone: ProcessEdgeTone::Neutral,
            allow_quick_add: false,
        }
    }

    #[must_use]
    pub fn output(id: impl Into<String>) -> Self {
        Self {
            id: id.into(),
            direction: ProcessPortDirection::Output,
            label: None,
            tone: ProcessEdgeTone::Neutral,
            allow_quick_add: false,
        }
    }

    #[must_use]
    pub fn branch(id: impl Into<String>, label: impl Into<String>, tone: ProcessEdgeTone) -> Self {
        Self {
            id: id.into(),
            direction: ProcessPortDirection::Output,
            label: Some(label.into()),
            tone,
            allow_quick_add: true,
        }
    }
}

/// Swimlane group declaration (e.g. microservice domain or agent role).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessLaneSpec {
    pub id: String,
    pub title: String,
    /// Stable palette tint selector (`0..N`) for subtle lane chrome accents.
    pub accent_index: usize,
}

/// One colored sub-phase inside a timeline waterfall bar (e.g. HTTP DNS/TLS/TTFB/Body).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTimingPhase {
    pub label: String,
    pub duration_ms: u64,
    pub tone: ProcessEdgeTone,
}

/// Unified timing span powering the shared Timeline waterfall view.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessTimingSpan {
    pub started_ms: Option<i64>,
    pub ended_ms: Option<i64>,
    pub duration_ms: Option<u64>,
    pub phases: Vec<ProcessTimingPhase>,
}

/// Capabilities declared by the hosting domain so the shared UI enables only valid controls.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct InteractionCapabilities {
    pub can_drag_reposition: bool,
    pub can_change_lane: bool,
    pub can_insert_on_edge: bool,
    pub can_edit_topology: bool,
    pub can_partial_run: bool,
    pub can_recover_node: bool,
}

/// Minimal geometry & topology descriptor for one node fed into [`super::layout_process_graph`].
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessNodeSpec {
    pub id: String,
    pub archetype: ProcessNodeArchetype,
    pub status: ProcessNodeStatus,
    pub lane_id: Option<String>,
    pub stage_hint: Option<usize>,
    /// Unscaled logical `(width, height)` declared by the node's visual card.
    pub size_hint: (f32, f32),
    /// Optional vertical/horizontal offset from card origin to the primary port anchor (e.g.
    /// keeps ports pinned to a collapsible group card's summary header).
    pub primary_port_offset: Option<f32>,
    /// Explicit ports; if empty, default `"in"` and `"out"` ports are synthesized automatically.
    pub ports: Vec<ProcessPortSpec>,
    /// Whether this node has collapsible inline members (used for stage parallelism hints).
    pub is_group_container: bool,
    /// Optional user-pinned `(x, y)` coordinate override in logical canvas space.
    pub pinned_pos: Option<(f32, f32)>,
}

/// Logical edge fed into [`super::layout_process_graph`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ProcessEdgeSpec {
    pub id: String,
    pub from_node: String,
    pub from_port: String,
    pub to_node: String,
    pub to_port: String,
    pub layer: ProcessEdgeLayer,
    pub kind: ProcessEdgeKind,
    pub label: Option<String>,
    pub tone: ProcessEdgeTone,
    pub dashed: bool,
    pub insertable: bool,
}

/// Complete domain-neutral input bundle for [`super::layout_process_graph`].
#[derive(Debug, Clone, Default)]
pub struct ProcessGraphInput {
    pub nodes: Vec<ProcessNodeSpec>,
    pub edges: Vec<ProcessEdgeSpec>,
    pub lanes: Vec<ProcessLaneSpec>,
    pub policy: LayoutPolicy,
}

/// Concrete positioned port anchor in logical canvas coordinates.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionedPort {
    pub id: String,
    pub direction: ProcessPortDirection,
    pub label: Option<String>,
    pub tone: ProcessEdgeTone,
    pub pos: (f32, f32),
    pub allow_quick_add: bool,
}

/// Concrete laid-out node box with resolved ports and pin state.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionedProcessNode {
    pub id: String,
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

impl PositionedProcessNode {
    #[must_use]
    pub fn port_pos(&self, port_id: &str, dir: ProcessPortDirection) -> (f32, f32) {
        self.ports
            .iter()
            .find(|p| p.id == port_id && p.direction == dir)
            .map(|p| p.pos)
            .unwrap_or(match dir {
                ProcessPortDirection::Input => self.port_in,
                ProcessPortDirection::Output => self.port_out,
            })
    }
}

/// One laid-out swimlane band (horizontal row band in LR, vertical column band in TB).
#[derive(Debug, Clone, PartialEq)]
pub struct PositionedLaneBand {
    pub id: String,
    pub title: String,
    pub accent_index: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub header_w: f32,
    pub header_h: f32,
    pub node_count: usize,
    /// `true` when lane runs horizontally left-to-right; `false` when vertical top-to-bottom.
    pub horizontal: bool,
}

/// One laid-out stage column/row track header.
#[derive(Debug, Clone, PartialEq)]
pub struct PositionedStageTrack {
    pub stage_index: usize,
    pub x: f32,
    pub y: f32,
    pub w: f32,
    pub h: f32,
    pub node_count: usize,
    pub has_parallel: bool,
    pub vertical: bool,
}

/// One routed connector edge ready for GPU Bézier rendering and optional interactive overlays.
#[derive(Debug, Clone, PartialEq)]
pub struct RoutedProcessEdge {
    pub id: String,
    pub from_node: String,
    pub from_port: String,
    pub to_node: String,
    pub to_port: String,
    pub layer: ProcessEdgeLayer,
    pub kind: ProcessEdgeKind,
    pub label: Option<String>,
    pub label_pos: Option<(f32, f32)>,
    pub insert_pos: Option<(f32, f32)>,
    pub tone: ProcessEdgeTone,
    pub dashed: bool,
    /// `true` when endpoints emerge horizontally (LR/Serpentine); `false` for vertical TB ports.
    pub horizontal_ports: bool,
    pub waypoints: Vec<(f32, f32)>,
}

/// Complete deterministic geometry output produced by [`super::layout_process_graph`].
#[derive(Debug, Clone, PartialEq)]
pub struct ProcessGraphGeometry {
    pub policy: LayoutPolicy,
    pub lanes: Vec<PositionedLaneBand>,
    pub stages: Vec<PositionedStageTrack>,
    pub nodes: Vec<PositionedProcessNode>,
    pub edges: Vec<RoutedProcessEdge>,
    pub canvas_width: f32,
    pub canvas_height: f32,
    pub has_pinned_nodes: bool,
    pub node_index: HashMap<String, usize>,
}

impl ProcessGraphGeometry {
    #[must_use]
    pub fn node(&self, id: &str) -> Option<&PositionedProcessNode> {
        self.node_index.get(id).and_then(|&i| self.nodes.get(i))
    }
}
