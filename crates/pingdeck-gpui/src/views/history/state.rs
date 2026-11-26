//! State container, tabs, modal specs, and handoff types for `HistoryView`.

use std::cell::Cell;

use gpui_kit::{Entity, Subscription, Task};
use pingdeck_desktop::core::app::{EnvironmentDto, UndoToken};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::core::model::{ExecutionHistoryEntry, HistoryFilter, Request};

use crate::ui::{InputState, Tone};

/// Detail inspector sub-tabs for a selected history entry.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum HistoryDetailTab {
    Body,
    Headers,
    Sent,
    Snapshot,
    Trace,
    Checks,
}

impl HistoryDetailTab {
    pub const ALL: [Self; 6] = [
        Self::Body,
        Self::Headers,
        Self::Sent,
        Self::Snapshot,
        Self::Trace,
        Self::Checks,
    ];

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Body => 0,
            Self::Headers => 1,
            Self::Sent => 2,
            Self::Snapshot => 3,
            Self::Trace => 4,
            Self::Checks => 5,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Headers,
            2 => Self::Sent,
            3 => Self::Snapshot,
            4 => Self::Trace,
            5 => Self::Checks,
            _ => Self::Body,
        }
    }

    #[must_use]
    pub const fn label(self, lang: Language) -> &'static str {
        let h = crate::ui::Tr(lang).history();
        match self {
            Self::Body => h.tab_body(),
            Self::Headers => h.tab_headers(),
            Self::Sent => h.tab_sent(),
            Self::Snapshot => h.tab_snapshot(),
            Self::Trace => h.tab_trace(),
            Self::Checks => h.tab_checks(),
        }
    }
}

/// Modal dialogs owned by `HistoryView`.
#[derive(Debug, Clone)]
pub enum HistoryModal {
    ClearFiltered { count: u64 },
}

/// Cross-module handoff signals emitted by `HistoryView` and polled by `AppShell`.
#[derive(Debug, Clone)]
pub enum HistoryHandoff {
    OpenInRequests(Box<Request>),
    AskAgent {
        title: String,
        context_markdown: String,
    },
}

/// Full stateful Execution History module (`NavModule::History`).
pub struct HistoryView {
    pub workspace_id: Option<String>,
    pub lang: Language,
    pub entries: Vec<ExecutionHistoryEntry>,
    pub page_cursors: Vec<Option<String>>,
    pub page_index: usize,
    pub next_cursor: Option<String>,
    pub total: Option<u64>,
    pub workspace_total: Option<u64>,
    pub mock_dropped: Option<u64>,
    pub environments: Vec<EnvironmentDto>,

    // Active server-side query filter
    pub filter: HistoryFilter,
    pub search_input: Entity<InputState>,
    pub loading: bool,

    // Selection & full record inspection
    pub selected_id: Option<String>,
    pub selected_entry: Option<ExecutionHistoryEntry>,
    pub run_children: Vec<ExecutionHistoryEntry>,
    pub run_children_next_cursor: Option<String>,
    pub run_children_total: Option<u64>,
    pub run_children_loading: bool,
    pub detail_tab: HistoryDetailTab,
    pub body_pretty: bool,

    // Undo & overlays
    pub last_undo: Option<(UndoToken, usize)>,
    pub active_modal: Option<HistoryModal>,
    pub status_msg: Option<(Tone, String)>,
    pub pending_handoff: Cell<Option<HistoryHandoff>>,
    pub(super) _subscriptions: Vec<Subscription>,
    pub(super) _tasks: Vec<Task<()>>,
}

impl HistoryView {
    #[must_use]
    pub fn active_entry(&self) -> Option<&ExecutionHistoryEntry> {
        self.selected_entry.as_ref().or_else(|| {
            let id = self.selected_id.as_deref()?;
            self.entries.iter().find(|e| e.id.as_str() == id)
        })
    }
}
