//! Shared chat UI value types: sidebar mode, modals, handoffs, pending attachments,
//! configured model options, and cached provider metadata.

use spanbay_desktop::core::agent::model::AgentTarget;
use spanbay_desktop::core::llm::wire::{Protocol, ReasoningStyle};
use spanbay_desktop::state::{LiveApproval, SettingsSection};

use crate::views::runs::WorkflowTab;

/// Sidebar mode in Chat (`Chats` list or `Trash`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChatSidebarMode {
    #[default]
    Time,
    Trash,
}

/// Cross-module navigation handoff requested from Chat.
#[derive(Debug, Clone)]
pub enum ChatHandoff {
    OpenSettings(SettingsSection),
    OpenProviders,
    OpenUsageRun(String),
    OpenWorkflowModal {
        session_id: String,
        run_id: Option<String>,
        initial_tab: Option<WorkflowTab>,
    },
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
    ChatInstructions,
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
