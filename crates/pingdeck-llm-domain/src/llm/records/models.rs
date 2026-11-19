//! Provider models, model references and the retired alias shapes the frozen DTOs still carry.

use serde::{Deserialize, Serialize};

use crate::llm::capabilities::ModelCapabilities;
use crate::llm::presets::ModelRole;
use crate::llm::probe::listing::Presence;

/// Where a provider model entry came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelSource {
    /// Copied from a bundled preset.
    Preset,
    /// Returned by the provider's live model list.
    Live,
    /// Typed in by the user.
    Manual,
}

impl ModelSource {
    /// Text representation persisted in `llm_models.source`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Preset => "preset",
            Self::Live => "live",
            Self::Manual => "manual",
        }
    }

    /// Parses [`ModelSource::as_str`] output, falling back to `Preset` for unknown values.
    #[must_use]
    pub fn parse(text: &str) -> Self {
        match text {
            "live" => Self::Live,
            "manual" => Self::Manual,
            _ => Self::Preset,
        }
    }
}

/// Who a model fact (context window, maximum output) comes from, strongest first.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum FactSource {
    /// The user set it.
    Declared,
    /// A full context probe measured it.
    Measured,
    /// The provider's model list (or the preset) gave it.
    Listed,
    /// The bundled catalogue gave it.
    Catalog,
}

/// One fact as stored: the user's value (when `declared`), the measured and the listed one.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct FactColumns {
    /// The user's value; meaningful only when `declared`.
    pub declared_value: Option<u64>,
    pub declared: bool,
    /// What a context probe measured (context only).
    pub measured: Option<u64>,
    pub listed: Option<u64>,
}

impl FactColumns {
    /// The fact with its source: declared, then measured, then listed. The catalogue comes last
    /// and is the caller's.
    #[must_use]
    pub fn resolve(self) -> (Option<u64>, Option<FactSource>) {
        if self.declared && self.declared_value.is_some() {
            return (self.declared_value, Some(FactSource::Declared));
        }
        if let Some(value) = self.measured {
            return (Some(value), Some(FactSource::Measured));
        }
        match self.listed {
            Some(value) => (Some(value), Some(FactSource::Listed)),
            None => (None, None),
        }
    }
}

#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderModel {
    pub provider_id: String,
    /// Model id as the provider spells it.
    pub model_id: String,
    pub source: ModelSource,
    /// Intended use (presets only).
    #[serde(default)]
    pub role: Option<ModelRole>,
    /// Context window: on read the resolved value (declared > measured > listed); on save the
    /// user's value when `context_source` is `declared`.
    #[serde(default)]
    pub context: Option<u64>,
    /// Maximum output tokens, resolved and saved like `context`.
    #[serde(default)]
    pub max_output: Option<u64>,
    /// Who `context` comes from. On save, `declared` keeps it as the user's value and any other
    /// value returns the fact to automatic; absent on a manual row with a value counts as declared.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub context_source: Option<FactSource>,
    /// Who `max_output` comes from (as `context_source`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub max_output_source: Option<FactSource>,
    /// The provider's listed context window (read-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listed_context: Option<u64>,
    /// The provider's listed maximum output (read-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listed_max_output: Option<u64>,
    /// The context window a full context probe measured (read-only).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub measured_context: Option<u64>,
    /// When some key's list first named the model (read-only); a model never listed is callable
    /// by every key.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub listed_confirmed_at_ms: Option<i64>,
    /// Accepts images. Legacy flag: derived from `capabilities` on read, and on save only a hint
    /// for the matrix.
    #[serde(default)]
    pub vision: bool,
    /// Supports tool calls. Legacy flag, as `vision`.
    #[serde(default)]
    pub tools: bool,
    #[serde(default)]
    pub notes: Option<String>,
    /// Whether the gateway lists and routes it.
    pub enabled: bool,
    /// What the model can do, with the source of every bit. Kept consistent with the legacy
    /// `vision` and `tools` flags; changed only by the capability functions.
    #[serde(default)]
    pub capabilities: ModelCapabilities,
    /// Whether the provider's last model list still contained it.
    #[serde(default = "listed")]
    pub presence: Presence,
    /// When the model was first seen, Unix ms (drives the "New" chip).
    #[serde(default)]
    pub first_seen_ms: Option<i64>,
    /// When "Mark as seen" cleared the "New" flag, Unix ms.
    #[serde(default)]
    pub new_cleared_at_ms: Option<i64>,
    /// Models tried, in order, when this one fails (written only by `set_model_fallbacks`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub fallbacks: Vec<ModelRef>,
}

/// One provider model in its stored form (`provider_id/model`); shown as `<provider name>/model`.
#[derive(Debug, Clone, PartialEq, Eq, Hash, PartialOrd, Ord, Serialize, Deserialize)]
pub struct ModelRef {
    pub provider_id: String,
    /// Upstream model id (may contain `/`).
    pub model: String,
}

impl ModelRef {
    /// The stored `provider_id/model` form.
    #[must_use]
    pub fn id_form(&self) -> String {
        format!("{}/{}", self.provider_id, self.model)
    }
}

fn listed() -> Presence {
    Presence::Listed
}

impl Default for ProviderModel {
    /// An empty enabled manual entry (callers fill what they know).
    fn default() -> Self {
        Self {
            provider_id: String::new(),
            model_id: String::new(),
            source: ModelSource::Manual,
            role: None,
            context: None,
            max_output: None,
            context_source: None,
            max_output_source: None,
            listed_context: None,
            listed_max_output: None,
            measured_context: None,
            listed_confirmed_at_ms: None,
            vision: false,
            tools: false,
            notes: None,
            enabled: true,
            capabilities: ModelCapabilities::default(),
            presence: Presence::Listed,
            first_seen_ms: None,
            new_cleared_at_ms: None,
            fallbacks: Vec::new(),
        }
    }
}

/// One ordered failover target of a retired alias: kept only because frozen DTOs embed the
/// type (`AiConfigDto.aliases` is always empty).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasTarget {
    pub provider_id: String,
    /// A specific key, or `None` for "any enabled key of the provider, in order".
    #[serde(default)]
    pub key_id: Option<String>,
    pub model_id: String,
    pub enabled: bool,
}

/// A retired client-facing alias (see [`AliasTarget`]).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Alias {
    pub id: String,
    /// The model name clients send.
    pub name: String,
    pub enabled: bool,
    #[serde(default)]
    pub notes: String,
    /// Targets in failover order.
    pub targets: Vec<AliasTarget>,
}
