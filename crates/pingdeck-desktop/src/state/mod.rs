//! Headless, UI-framework-agnostic application ViewModels, routing, command palette engine,
//! and stream reducers.

pub mod chat_reducer;
pub mod palette;
pub mod run_graph;
pub mod shell;

pub use chat_reducer::{
    ChatThreadState, LiveApproval, LiveCostPause, LiveFileChange, LiveSubagent, LiveSubagentTool,
    LiveToolCall,
};
pub use palette::{
    PaletteAction, PaletteCategory, PaletteDynamicItems, PaletteEngine, PaletteItem,
};
pub use run_graph::{
    ExecutionGraphLayout, GRAPH_GROUP_TAB_H, GRAPH_MEMBER_ROW_H, GRAPH_NODE_W, GraphBuildLabels,
    GraphEdge, GraphEdgeTone, GraphNode, GraphNodeKind, GraphNodeMember, GraphNodePayload,
    GraphNodeStatus, GraphStageHeader, PositionedNode, RunViewMode, SelectedGraphTarget,
    build_graph_from_topology, is_graph_available, layout_dag_nodes, map_run_status,
    map_tool_status, select_default_view_mode,
};
pub use shell::{
    EnvironmentResourceTarget, NavGroup, NavModule, SettingsGroup, SettingsSection, ShellState,
    ToastKind, ToastMessage, UtilityDialog,
};
