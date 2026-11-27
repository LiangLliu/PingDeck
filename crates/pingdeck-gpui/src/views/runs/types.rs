//! Shared UI value types for the standalone Run Monitor (`NavModule::Runs`) and the
//! global 90% viewport Workflow Monitor modal (`WorkflowMonitorModal`).

/// Lifecycle filter active inside the global Run Monitor list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunsFilter {
    #[default]
    All,
    Waiting,
    Running,
    Failed,
    Done,
}

/// Time/source scope inside the global Run Monitor list.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RunsScope {
    #[default]
    Recent24h,
    AllHistory,
}

/// Active view tab inside the 90% viewport Workflow Monitor modal.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash)]
pub enum WorkflowTab {
    Steps,
    Graph,
    Timeline,
    Changes,
}

/// History picker filter inside the 90% Workflow Monitor modal header popover.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum HistoryPickerFilter {
    #[default]
    All,
    Complex,
    Failed,
}

/// Cross-module handoff emitted by the standalone `RunsView`.
#[derive(Debug, Clone)]
pub enum RunsHandoff {
    OpenWorkflowModal {
        session_id: String,
        run_id: Option<String>,
        initial_tab: Option<WorkflowTab>,
    },
    OpenChatSession(String),
    OpenUsageRun(String),
}

/// Cross-module handoff emitted by the 90% `WorkflowMonitorModal`.
#[derive(Debug, Clone)]
pub enum WorkflowModalHandoff {
    OpenChatSession(String),
    OpenUsageRun(String),
}
