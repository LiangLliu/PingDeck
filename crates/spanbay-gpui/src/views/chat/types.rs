//! Shared chat UI value types: sidebar mode, modals, handoffs, configured model options, and
//! cached provider metadata.

use std::sync::Arc;

use spanbay_desktop::core::agent::model::{AgentTarget, ApprovalDecision, ApprovalScope};
use spanbay_desktop::core::llm::wire::{Protocol, ReasoningStyle};
pub use spanbay_desktop::state::TranscriptDensity;
use spanbay_desktop::state::{LiveApproval, SettingsSection};

use crate::views::runs::WorkflowTab;

/// Sidebar mode in Chat (`Chats` list or `Trash`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatSidebarMode {
    #[default]
    Time,
    Trash,
}

/// Scope filter in the right-hand Review Panel (`Turn` vs `All`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReviewScope {
    #[default]
    Turn,
    All,
}

/// Diff presentation mode in the right-hand Review Panel (`Net` vs `PerEdit`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ReviewDiffMode {
    #[default]
    Net,
    PerEdit,
}

/// Inline diff review comment attached as a chip to the composer (`@path:line`).
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct DiffCommentChip {
    pub path: String,
    pub line: usize,
    pub comment: String,
}

/// Full tool output or file preview displayed in the right-hand Review Panel.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct ReviewToolOutput {
    pub call_id: String,
    pub name: String,
    pub content: String,
    pub has_more: bool,
    pub next_offset: u64,
}

/// Cross-module navigation handoff requested from Chat.
#[derive(Debug, Clone)]
pub enum ChatHandoff {
    OpenSettings(SettingsSection),
    OpenProviders,
    OpenUsageRun(String),
    /// A saved request a tool result points at (`ToolResultView::SavedRequest`).
    OpenRequest {
        workspace_id: Option<String>,
        request_id: String,
    },
    OpenWorkflowModal {
        session_id: String,
        run_id: Option<String>,
        initial_tab: Option<WorkflowTab>,
    },
}

/// How the last approval card was answered, shown in the dock until the next gate or turn.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct LastApprovalNotice {
    pub call_id: String,
    pub decision: ApprovalDecision,
    pub scope: ApprovalScope,
    pub reason: Option<String>,
}

/// Backing bytes or attachment id for "Open in system app" in the image viewer.
#[derive(Debug, Clone)]
pub enum ImageViewerSource {
    Attachment(String),
    Inline {
        bytes: Arc<[u8]>,
        media_type: String,
    },
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
    ChatInstructions,
    ScopeApproval {
        appr: Box<LiveApproval>,
        selected: ApprovalScope,
    },
    EditMessage {
        message_id: String,
    },
    /// A thumbnail opened full size; `key` names the decoded image in `ChatView::media_cache`.
    ImageViewer {
        key: String,
        title: String,
        meta: String,
        dims: Option<(u32, u32)>,
        actual: bool,
        source: Option<ImageViewerSource>,
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
