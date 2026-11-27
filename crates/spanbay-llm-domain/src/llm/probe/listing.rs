//! What a provider's model list says about its models: the model `kind` and the drift between
//! the stored models and a fresh list.
//!
//! Single responsibility: pure derivations over ids and list entries; the refresh stores their
//! output, the Models tab shows it.

use std::collections::HashSet;

use serde::{Deserialize, Serialize};

use crate::llm::presets::ModelRole;

/// What a model is for (`ProviderModelDto.kind`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelKind {
    /// Chat, responses and completion models.
    Chat,
    /// Embedding models.
    Embedding,
    /// Image generation models.
    Image,
    /// Speech and audio models.
    Audio,
    /// Rerank models.
    Rerank,
    /// Moderation models.
    Moderation,
    /// Anything else the catalogue knows (OCR, video, search).
    Other,
}

impl ModelKind {
    /// The snake_case identifier stored in `capabilities_json`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Embedding => "embedding",
            Self::Image => "image",
            Self::Audio => "audio",
            Self::Rerank => "rerank",
            Self::Moderation => "moderation",
            Self::Other => "other",
        }
    }

    /// The five-value kind the Providers UI draws (`ProviderModelDto.kind`): rerank and
    /// moderation models have no tag of their own there and read as [`ModelKind::Other`].
    #[must_use]
    pub const fn coarse(self) -> Self {
        match self {
            Self::Rerank | Self::Moderation => Self::Other,
            kind => kind,
        }
    }
}

/// The kind a catalogue `mode` names; `None` for a mode this build does not know.
fn kind_from_catalog_mode(mode: &str) -> Option<ModelKind> {
    Some(match mode {
        "chat" | "responses" | "completion" => ModelKind::Chat,
        "embedding" => ModelKind::Embedding,
        "image_generation" => ModelKind::Image,
        "audio_transcription" | "audio_speech" => ModelKind::Audio,
        "rerank" => ModelKind::Rerank,
        "moderation" => ModelKind::Moderation,
        "ocr" | "video_generation" | "search" | "vector_store" => ModelKind::Other,
        _ => return None,
    })
}

/// Whether a model id names a decision model: TypeSafe's `jev-*` and `systemone`, and Kev.
///
/// A provider's list declares no role for them (TypeSafe answers `{"models":[{"name":…}]}`), so
/// on a relay that also speaks chat the id is the only signal that the model answers decision
/// requests rather than chat ones.
#[must_use]
pub fn is_decision_model(id: &str) -> bool {
    let id = id.to_ascii_lowercase();
    id.contains("jev") || id.starts_with("kev") || id.contains("systemone")
}

/// Whether a model of `kind` runs the decision probe instead of a chat check: a chat model of a
/// provider that speaks a decision protocol and either speaks no chat protocol or serves `model_id`
/// as a decision model ([`is_decision_model`]).
pub(crate) fn runs_decision(
    kind: ModelKind,
    model_id: &str,
    speaks_decision: bool,
    speaks_chat: bool,
) -> bool {
    kind == ModelKind::Chat && speaks_decision && (!speaks_chat || is_decision_model(model_id))
}

/// The kind a stored preset role fixes (the role is read before anything else): an embedding
/// role is an embedding model, a decision role says nothing, every other role is chat.
#[must_use]
pub fn role_kind(role: Option<ModelRole>) -> Option<ModelKind> {
    match role? {
        ModelRole::Embedding => Some(ModelKind::Embedding),
        ModelRole::Decision => None,
        _ => Some(ModelKind::Chat),
    }
}

/// Id fragments of image models.
const IMAGE_ID_PATTERNS: [&str; 7] = [
    "dall-e",
    "gpt-image",
    "imagen",
    "flux",
    "stable-diffusion",
    "sdxl",
    "-image",
];

/// Id fragments of speech and audio models.
const AUDIO_ID_PATTERNS: [&str; 3] = ["whisper", "tts", "audio"];

/// The kind of a model: the catalogue `mode`, then what the provider's list `declared`, then
/// patterns in the id (embedding, image, audio, rerank, moderation, in that order), else chat.
#[must_use]
pub fn model_kind(id: &str, catalog_mode: Option<&str>, declared: Option<ModelKind>) -> ModelKind {
    if let Some(kind) = catalog_mode.and_then(kind_from_catalog_mode) {
        return kind;
    }
    if let Some(kind) = declared {
        return kind;
    }
    let id = id.to_ascii_lowercase();
    let has = |needle: &str| id.contains(needle);
    if has("embed") {
        ModelKind::Embedding
    } else if IMAGE_ID_PATTERNS.iter().any(|needle| has(needle)) {
        ModelKind::Image
    } else if AUDIO_ID_PATTERNS.iter().any(|needle| has(needle)) {
        ModelKind::Audio
    } else if has("rerank") {
        ModelKind::Rerank
    } else if has("moderation") {
        ModelKind::Moderation
    } else {
        ModelKind::Chat
    }
}

/// Whether the provider's list still contains a stored model (`presence`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Presence {
    /// The last list contained it.
    Listed,
    /// The last list did not.
    NotInProvider,
}

/// The comparison a refresh stores in `model_drift_json`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelDrift {
    /// When the list was fetched, Unix ms.
    pub checked_at_ms: i64,
    /// Listed ids that are not stored (the "From provider" ghost rows).
    pub added: Vec<String>,
    /// Stored ids the provider no longer lists.
    pub removed: Vec<String>,
    /// The keys whose own list could not be read this time (a refresh answer only, never
    /// stored); their models of the previous read still count as listed.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key_failures: Vec<KeyListFailure>,
    /// False when the list was truncated or empty: `removed` is then left empty, since an
    /// incomplete list cannot say what the provider dropped. Left out while true, so the frozen
    /// `ModelDriftDto` samples keep their bytes.
    #[serde(default = "complete", skip_serializing_if = "is_complete")]
    pub listing_complete: bool,
}

const fn complete() -> bool {
    true
}

const fn is_complete(value: &bool) -> bool {
    *value
}

/// A key whose model list could not be read (`KeyListFailureDto`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KeyListFailure {
    /// The key.
    pub key_id: String,
    /// One line in words; never the key.
    pub message: String,
}

impl ModelDrift {
    /// Whether there is anything to review (the drift bar shows only then).
    #[must_use]
    pub fn is_empty(&self) -> bool {
        self.added.is_empty() && self.removed.is_empty()
    }

    /// Keeps only the removals the list can decide: none when the list is incomplete
    /// (truncated or empty), else only the ids `from_list` says came from the provider's list (a
    /// manual or preset model is the user's word, never "removed by the provider").
    pub fn limit_removals(&mut self, complete: bool, from_list: impl Fn(&str) -> bool) {
        self.listing_complete = complete;
        if complete {
            self.removed.retain(|id| from_list(id));
        } else {
            self.removed.clear();
        }
    }
}

/// Compares the stored model ids with a fresh list. Both outputs keep first-seen order and hold
/// no duplicate; ids compare exactly as the provider spells them.
#[must_use]
pub fn compute_drift(stored: &[String], listed: &[String], checked_at_ms: i64) -> ModelDrift {
    ModelDrift {
        checked_at_ms,
        added: difference(listed, stored),
        removed: difference(stored, listed),
        key_failures: Vec::new(),
        listing_complete: true,
    }
}

/// Ids of `from` that are not in `other`, first occurrence only.
fn difference(from: &[String], other: &[String]) -> Vec<String> {
    let other: HashSet<&str> = other.iter().map(String::as_str).collect();
    let mut seen: HashSet<&str> = HashSet::new();
    from.iter()
        .filter(|id| !other.contains(id.as_str()) && seen.insert(id.as_str()))
        .cloned()
        .collect()
}

#[cfg(test)]
mod tests;
