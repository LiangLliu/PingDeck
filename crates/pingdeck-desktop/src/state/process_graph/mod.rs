//! Domain-agnostic, headless Process Graph IR, multi-mode layout engine, swimlane packer,
//! hybrid manual positioning, and edge router.
//!
//! Shared Layer 2 foundation for all dependency-aware workflows in PingDeck:
//! - Agent Execution Graphs & Plan Task Graphs (`state/run_graph.rs`)
//! - HTTP Request Flows with conditional branches, parallel joins, swimlanes, and `{{var}}` data
//!   flow edges (Phase 2 & 3)
//! - Future pipelines (nested sub-flows, mixed HTTP+AI flows, scheduled chains)

pub mod layout;
pub mod model;

#[cfg(test)]
mod tests;

pub use layout::{
    AlignmentGuide, GuideOrientation, PROCESS_LANE_GAP, PROCESS_LANE_HEADER_H,
    PROCESS_LANE_HEADER_W, PROCESS_LANE_PAD, PROCESS_NODE_GAP_X, PROCESS_NODE_GAP_Y,
    PROCESS_PAD_BOTTOM, PROCESS_PAD_X, PROCESS_PAD_Y, PROCESS_STAGE_GAP_X, PROCESS_STAGE_GAP_Y,
    hit_test_lane, layout_process_graph, snap_drag_position,
};
pub use model::{
    CrossAlignment, DataFlowVisibility, InteractionCapabilities, LayoutDirection, LayoutPolicy,
    PositionedLaneBand, PositionedPort, PositionedProcessNode, PositionedStageTrack,
    ProcessEdgeKind, ProcessEdgeLayer, ProcessEdgeSpec, ProcessEdgeTone, ProcessGraphGeometry,
    ProcessGraphInput, ProcessLaneSpec, ProcessNodeArchetype, ProcessNodeSpec, ProcessNodeStatus,
    ProcessPortDirection, ProcessPortSpec, ProcessTimingPhase, ProcessTimingSpan,
    RoutedProcessEdge,
};
