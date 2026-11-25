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
    pub const fn label(self, zh: bool) -> &'static str {
        match (self, zh) {
            (Self::Body, true) => "响应正文",
            (Self::Body, false) => "Body",
            (Self::Headers, true) => "响应头",
            (Self::Headers, false) => "Headers",
            (Self::Sent, true) => "实发请求",
            (Self::Sent, false) => "Sent",
            (Self::Snapshot, true) => "请求快照",
            (Self::Snapshot, false) => "Snapshot",
            (Self::Trace, true) => "耗时与链路",
            (Self::Trace, false) => "Trace",
            (Self::Checks, true) => "校验与批次",
            (Self::Checks, false) => "Checks",
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
    pub fn is_zh(&self) -> bool {
        self.lang == Language::ZhCn
    }

    #[must_use]
    pub fn t(&self, zh: &'static str, en: &'static str) -> &'static str {
        if self.is_zh() { zh } else { en }
    }

    #[must_use]
    pub fn active_entry(&self) -> Option<&ExecutionHistoryEntry> {
        self.selected_entry.as_ref().or_else(|| {
            let id = self.selected_id.as_deref()?;
            self.entries.iter().find(|e| e.id.as_str() == id)
        })
    }
}
