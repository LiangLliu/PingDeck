//! Headless, UI-framework-agnostic application ViewModels, routing, command palette engine,
//! and stream reducers.

pub mod chat_reducer;
pub mod flow_graph;
pub mod palette;
pub mod process_graph;
pub mod run_graph;
pub mod shell;

pub use chat_reducer::{
    ChatThreadState, LiveApproval, LiveCostPause, LiveFileChange, LiveSubagent, LiveSubagentTool,
    LiveToolCall,
};
pub use flow_graph::{
    FLOW_CARD_ALERT_H, FLOW_CARD_H, FLOW_CARD_W, FLOW_COND_W, FLOW_JOIN_H, FLOW_JOIN_W,
    FlowGraphLayout, FlowGraphNode, FlowOverlaySource, FlowStepRunOverlay, FlowViewMode,
    build_flow_graph_layout, effective_flow_layout_policy,
};
pub use palette::{
    PaletteAction, PaletteCategory, PaletteDynamicItems, PaletteEngine, PaletteItem,
};
pub use process_graph::{
    AlignmentGuide, CrossAlignment, DataFlowVisibility, GuideOrientation, InteractionCapabilities,
    LayoutDirection, LayoutPolicy, PositionedLaneBand, PositionedPort, PositionedProcessNode,
    PositionedStageTrack, ProcessEdgeKind, ProcessEdgeLayer, ProcessEdgeSpec, ProcessEdgeTone,
    ProcessGraphGeometry, ProcessGraphInput, ProcessLaneSpec, ProcessNodeArchetype,
    ProcessNodeSpec, ProcessNodeStatus, ProcessPortDirection, ProcessPortSpec, ProcessTimingPhase,
    ProcessTimingSpan, RoutedProcessEdge, hit_test_lane, layout_process_graph, snap_drag_position,
};
pub use run_graph::{
    ExecutionGraphLayout, GRAPH_GROUP_TAB_H, GRAPH_MAX_INLINE_MEMBERS, GRAPH_MEMBER_ROW_H,
    GRAPH_NODE_W, GRAPH_OVERFLOW_ROW_H, GraphBuildLabels, GraphEdge, GraphEdgeTone, GraphNode,
    GraphNodeKind, GraphNodeMember, GraphNodePayload, GraphNodeStatus, GraphStageHeader,
    PositionedNode, RunViewMode, SelectedGraphTarget, build_graph_from_topology,
    build_graph_from_topology_with_policy, build_plan_preview_graph, has_structured_task_graph,
    is_graph_available, layout_dag_nodes, layout_dag_nodes_with_policy, map_run_status,
    map_todo_status, map_tool_status, select_default_view_mode,
};
pub use shell::{
    EnvironmentResourceTarget, NavGroup, NavModule, SettingsGroup, SettingsSection, ShellState,
    ToastKind, ToastMessage, UtilityDialog,
};
