//! Shared chat UI value types: inspector tab enum, sidebar mode, modals, handoffs,
//! pending attachments, configured model options, and cached provider metadata.

use pingdeck_desktop::core::agent::model::AgentTarget;
use pingdeck_desktop::core::llm::wire::{Protocol, ReasoningStyle};
use pingdeck_desktop::state::{LiveApproval, SettingsSection};

/// Sidebar / center canvas mode in Chat (`Chats`, full-canvas `Run Monitor`, or `Trash`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatSidebarMode {
    #[default]
    Time,
    Board,
    Trash,
}

/// Filter pill active inside the full-canvas Run Monitor feed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum MonitorFilter {
    #[default]
    All,
    Waiting,
    Running,
    Failed,
    Done,
}

/// Active tab inside the right-hand Chat Details inspector.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum DetailsTab {
    #[default]
    Progress,
    Changes,
    Context,
    Options,
}

impl DetailsTab {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Progress => 0,
            Self::Changes => 1,
            Self::Context => 2,
            Self::Options => 3,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Changes,
            2 => Self::Context,
            3 => Self::Options,
            _ => Self::Progress,
        }
    }
}

/// Cross-module navigation handoff requested from Chat.
#[derive(Debug, Clone)]
pub enum ChatHandoff {
    OpenSettings(SettingsSection),
    OpenProviders,
    OpenUsageRun(String),
}

/// Staged file attachment waiting to be sent with the next prompt.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PendingAttachment {
    pub id: String,
    pub name: String,
    pub size_bytes: u64,
}

/// Active modal dialog over `ChatView`.
#[derive(Debug, Clone)]
pub enum ChatModal {
    FullAccessConfirm {
        acknowledged: bool,
    },
    DeleteRunningConfirm {
        session_id: String,
        title: String,
    },
    RenameSession {
        session_id: String,
    },
    ScopeApproval {
        appr: Box<LiveApproval>,
    },
    DiffPreview {
        path: String,
        diff: String,
        binary: bool,
    },
    ToolOutput {
        call_id: String,
        name: String,
        content: String,
        has_more: bool,
        next_offset: u64,
    },
    EditMessage {
        message_id: String,
    },
    StopAllConfirm {
        active_runs: Vec<(String, String)>,
    },
}

/// Real configured model entry returned by `llm_provider_list_impl` + `llm_model_list_impl`.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ModelOption {
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    pub enabled: bool,
}

impl ModelOption {
    #[must_use]
    pub fn display_label(&self) -> String {
        format!("{} / {}", self.provider_name, self.model_id)
    }

    #[must_use]
    pub fn target(&self) -> AgentTarget {
        AgentTarget::Pinned {
            provider_id: self.provider_id.clone(),
            key_id: None,
            model: self.model_id.clone(),
        }
    }
}

/// Cached provider metadata used for display names and reasoning diagnostics in Chat.
#[derive(Debug, Clone)]
pub struct ProviderChatMeta {
    pub name: String,
    pub primary_protocol: Option<Protocol>,
    pub preset_id: Option<String>,
    pub reasoning_request: Option<ReasoningStyle>,
}

impl ProviderChatMeta {
    #[must_use]
    pub fn missing_reasoning_quirk(&self) -> bool {
        let is_chat_compat = self
            .primary_protocol
            .is_none_or(|pr| pr == Protocol::OpenaiChat);
        is_chat_compat && self.preset_id.is_none() && self.reasoning_request.is_none()
    }
}
