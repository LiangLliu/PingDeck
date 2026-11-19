//! Shared chat UI value types: inspector tab enum, configured model options, and cached
//! provider metadata for display formatting and reasoning diagnostics.

use pingdeck_desktop::core::agent::model::AgentTarget;
use pingdeck_desktop::core::llm::wire::{Protocol, ReasoningStyle};

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
