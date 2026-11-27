//! What a model can do, and how sure we are: catalogue, declared, measured, or user override.
//!
//! Single responsibility: the [`ModelCapabilities`] value stored in
//! `llm_provider_models.capabilities_json`, the precedence rule between its sources, and the
//! derivations that fill it (catalogue entry, provider-declared fields, an applied probe outcome).
//! A probe result alone never lands here: it stays evidence per key in
//! `llm_probe_results` until the user applies the run, because routing filters on this matrix.
//! Pills in the UI are solid only for [`CapabilitySource::Measured`] and
//! [`CapabilitySource::UserOverride`] (D-43); the rest is dashed.

use serde::{Deserialize, Serialize};

use crate::catalog_entry::CatalogEntry;

/// Where a capability bit came from, weakest first (the order is the precedence).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CapabilitySource {
    /// Nothing is known.
    #[default]
    Unknown,
    /// The bundled model catalogue says so.
    Catalog,
    /// The provider's own model list declares it.
    Declared,
    /// A probe observed it.
    Measured,
    /// The user set it.
    UserOverride,
}

impl CapabilitySource {
    /// Whether a pill from this source is drawn solid (`Measured` or `UserOverride`).
    #[must_use]
    pub const fn is_solid(self) -> bool {
        matches!(self, Self::Measured | Self::UserOverride)
    }
}

/// One capability bit with its provenance.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct CapabilityState {
    /// Whether the model supports it (meaningless while the source is `Unknown`).
    pub supported: bool,
    /// Who says so.
    pub source: CapabilitySource,
    /// When a probe last wrote this bit, Unix milliseconds.
    #[serde(default)]
    pub measured_at_ms: Option<i64>,
    /// Probe samples behind a `not_observed` bit (accumulates over runs).
    #[serde(default)]
    pub samples: u32,
    /// A content check failed on a model that otherwise answered: the capability was *not
    /// observed*, which is weaker than measured-absent (a model can have a capability it does
    /// not show). Drawn dashed, never used to filter routing.
    #[serde(default)]
    pub not_observed: bool,
}

impl CapabilityState {
    /// Whether a pill for this state is drawn solid: measured or set by the user, and not a mere
    /// "not observed".
    #[must_use]
    pub const fn is_solid(&self) -> bool {
        self.source.is_solid() && !self.not_observed
    }
}

/// What one probe check says about one capability.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Measurement {
    /// The check passed.
    Yes,
    /// A checkable answer was wrong (the vision probe returned "blue"): measured absent.
    No,
    /// The content test failed on an answering model (no tool call, no reasoning text, no cached
    /// tokens): not observed, never recorded as measured absent.
    NotObserved,
}

/// The capabilities a pill or a routing filter can ask about.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Hash, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum Capability {
    /// Image input.
    Vision,
    /// PDF / document input.
    Document,
    /// Audio input.
    AudioIn,
    /// Function calling.
    Tools,
    /// Structured output by JSON schema.
    JsonSchema,
    /// Reasoning output.
    Reasoning,
    /// Streaming responses.
    Streaming,
    /// Prompt caching.
    PromptCaching,
    /// Provider-native web search.
    WebSearch,
    /// Embeddings output.
    Embeddings,
}

impl Capability {
    /// Every capability, in pill order.
    pub const ALL: [Self; 10] = [
        Self::Vision,
        Self::Document,
        Self::AudioIn,
        Self::Tools,
        Self::JsonSchema,
        Self::Reasoning,
        Self::Streaming,
        Self::PromptCaching,
        Self::WebSearch,
        Self::Embeddings,
    ];
}

/// Per-model capability matrix (`docs/architecture.md` §8, `docs/decisions.md` §12 L-11).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
pub struct ModelCapabilities {
    /// Image input.
    #[serde(default)]
    pub vision: CapabilityState,
    /// PDF / document input. Serialised as `documents` (the name of the Providers UI); rows
    /// stored before the rename say `document` and still load.
    #[serde(default, rename = "documents", alias = "document")]
    pub document: CapabilityState,
    /// Audio input.
    #[serde(default)]
    pub audio_in: CapabilityState,
    /// Function calling.
    #[serde(default)]
    pub tools: CapabilityState,
    /// Structured output by JSON schema.
    #[serde(default)]
    pub json_schema: CapabilityState,
    /// Reasoning output.
    #[serde(default)]
    pub reasoning: CapabilityState,
    /// Streaming responses.
    #[serde(default)]
    pub streaming: CapabilityState,
    /// Prompt caching.
    #[serde(default)]
    pub prompt_caching: CapabilityState,
    /// Provider-native web search.
    #[serde(default)]
    pub web_search: CapabilityState,
    /// Embeddings output.
    #[serde(default)]
    pub embeddings: CapabilityState,
}

impl ModelCapabilities {
    /// The state of one capability.
    #[must_use]
    pub const fn get(&self, capability: Capability) -> CapabilityState {
        match capability {
            Capability::Vision => self.vision,
            Capability::Document => self.document,
            Capability::AudioIn => self.audio_in,
            Capability::Tools => self.tools,
            Capability::JsonSchema => self.json_schema,
            Capability::Reasoning => self.reasoning,
            Capability::Streaming => self.streaming,
            Capability::PromptCaching => self.prompt_caching,
            Capability::WebSearch => self.web_search,
            Capability::Embeddings => self.embeddings,
        }
    }

    fn slot(&mut self, capability: Capability) -> &mut CapabilityState {
        match capability {
            Capability::Vision => &mut self.vision,
            Capability::Document => &mut self.document,
            Capability::AudioIn => &mut self.audio_in,
            Capability::Tools => &mut self.tools,
            Capability::JsonSchema => &mut self.json_schema,
            Capability::Reasoning => &mut self.reasoning,
            Capability::Streaming => &mut self.streaming,
            Capability::PromptCaching => &mut self.prompt_caching,
            Capability::WebSearch => &mut self.web_search,
            Capability::Embeddings => &mut self.embeddings,
        }
    }

    /// Records `supported` from `source` unless a stronger source already holds the bit (equal
    /// strength replaces: a newer measurement wins over an older one). Returns whether the bit
    /// changed.
    pub fn apply(
        &mut self,
        capability: Capability,
        supported: bool,
        source: CapabilitySource,
    ) -> bool {
        let slot = self.slot(capability);
        if source < slot.source {
            return false;
        }
        let next = CapabilityState {
            supported,
            source,
            ..CapabilityState::default()
        };
        let changed = *slot != next;
        *slot = next;
        changed
    }

    /// Records a probe outcome at `Measured` strength with its date and sample count. A user
    /// override is never replaced; a measurement older than the stored one is dropped (an old
    /// run applied late cannot undo a newer result); `NotObserved` never overturns a measured
    /// `Yes`, and its sample count accumulates across runs. Returns whether the bit changed.
    pub fn apply_measurement(
        &mut self,
        capability: Capability,
        outcome: Measurement,
        now_ms: i64,
    ) -> bool {
        let slot = self.slot(capability);
        if slot.source == CapabilitySource::UserOverride {
            return false;
        }
        if slot.source == CapabilitySource::Measured
            && slot.measured_at_ms.is_some_and(|at| at > now_ms)
        {
            return false;
        }
        let previous = *slot;
        let next = match outcome {
            Measurement::Yes => CapabilityState {
                supported: true,
                source: CapabilitySource::Measured,
                measured_at_ms: Some(now_ms),
                samples: 0,
                not_observed: false,
            },
            Measurement::No => CapabilityState {
                supported: false,
                source: CapabilitySource::Measured,
                measured_at_ms: Some(now_ms),
                samples: 0,
                not_observed: false,
            },
            Measurement::NotObserved => {
                if previous.source == CapabilitySource::Measured && previous.supported {
                    return false;
                }
                let earlier = if previous.not_observed {
                    previous.samples
                } else {
                    0
                };
                CapabilityState {
                    supported: false,
                    source: CapabilitySource::Measured,
                    measured_at_ms: Some(now_ms),
                    samples: earlier.saturating_add(1),
                    not_observed: true,
                }
            }
        };
        *slot = next;
        previous != next
    }

    /// Replaces one state as a whole (the apply and undo of probe results write the exact
    /// state they planned).
    pub fn set(&mut self, capability: Capability, state: CapabilityState) {
        *self.slot(capability) = state;
    }

    /// Forgets a user override (the bit becomes unknown until the next measurement or refresh).
    pub fn clear_override(&mut self, capability: Capability) {
        let slot = self.slot(capability);
        if slot.source == CapabilitySource::UserOverride {
            *slot = CapabilityState::default();
        }
    }

    /// Whether the model is known to support `capability`: `Some(bool)` once any source has an
    /// opinion, `None` while it is unknown.
    #[must_use]
    pub const fn known(&self, capability: Capability) -> Option<bool> {
        let state = self.get(capability);
        match state.source {
            CapabilitySource::Unknown => None,
            _ => Some(state.supported),
        }
    }

    /// Whether the capability is *measured or set by the user* as absent: the only evidence the
    /// router's pre-filter acts on (D-43).
    #[must_use]
    pub const fn known_absent(&self, capability: Capability) -> bool {
        let state = self.get(capability);
        state.is_solid() && !state.supported
    }

    /// Fills the bits a bundled catalogue entry knows (`Catalog` strength). Only positive facts
    /// are recorded: the catalogue omits flags it does not know, so `false` means "unknown", not
    /// "unsupported".
    pub fn apply_catalog(&mut self, entry: &CatalogEntry) {
        let lists = |modality: &str| entry.input_modalities.iter().any(|m| m == modality);
        for (capability, flag) in [
            (Capability::Vision, entry.vision),
            (Capability::Tools, entry.tools),
            (Capability::Reasoning, entry.reasoning),
            (Capability::PromptCaching, entry.prompt_caching),
            (Capability::Embeddings, entry.mode == "embedding"),
            (Capability::Document, lists("pdf")),
            (Capability::AudioIn, lists("audio")),
        ] {
            if flag {
                self.apply(capability, true, CapabilitySource::Catalog);
            }
        }
    }

    /// Whether any bit was measured or set by the user.
    #[must_use]
    pub fn has_solid_bit(&self) -> bool {
        Capability::ALL
            .iter()
            .any(|capability| self.get(*capability).is_solid())
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn entry() -> CatalogEntry {
        CatalogEntry {
            model_id: "m".into(),
            provider: "p".into(),
            mode: "chat".into(),
            max_input_tokens: None,
            max_output_tokens: None,
            vision: true,
            tools: false,
            reasoning: true,
            prompt_caching: false,
            input_cost_per_mtok: None,
            output_cost_per_mtok: None,
            cache_read_cost_per_mtok: None,
            input_modalities: Vec::new(),
        }
    }

    #[test]
    fn stronger_source_wins_and_equal_replaces() {
        let mut caps = ModelCapabilities::default();
        assert!(caps.apply(Capability::Vision, true, CapabilitySource::Declared));
        assert!(!caps.apply(Capability::Vision, false, CapabilitySource::Catalog));
        assert!(caps.apply(Capability::Vision, false, CapabilitySource::Measured));
        assert!(caps.apply(Capability::Vision, true, CapabilitySource::Measured));
        assert!(caps.apply(Capability::Vision, false, CapabilitySource::UserOverride));
        // A later measurement never beats the user.
        assert!(!caps.apply(Capability::Vision, true, CapabilitySource::Measured));
        assert_eq!(caps.known(Capability::Vision), Some(false));
        caps.clear_override(Capability::Vision);
        assert_eq!(caps.known(Capability::Vision), None);
    }

    #[test]
    fn catalogue_records_only_positive_facts() {
        let mut caps = ModelCapabilities::default();
        caps.apply_catalog(&entry());
        assert_eq!(caps.known(Capability::Vision), Some(true));
        assert_eq!(caps.known(Capability::Reasoning), Some(true));
        // `tools: false` in the catalogue means "unknown", not "unsupported".
        assert_eq!(caps.known(Capability::Tools), None);
        assert!(!caps.known_absent(Capability::Tools));
        assert!(!caps.has_solid_bit());
    }

    #[test]
    fn catalogue_pdf_and_audio_inputs_become_document_and_audio_in() {
        let mut caps = ModelCapabilities::default();
        caps.apply_catalog(&CatalogEntry {
            input_modalities: vec!["text".into(), "audio".into(), "pdf".into()],
            ..entry()
        });
        assert_eq!(caps.known(Capability::Document), Some(true));
        assert_eq!(caps.known(Capability::AudioIn), Some(true));
        let mut plain = ModelCapabilities::default();
        plain.apply_catalog(&entry());
        assert_eq!(plain.known(Capability::Document), None);
    }

    #[test]
    fn only_solid_absence_filters_routing() {
        let mut caps = ModelCapabilities::default();
        caps.apply(Capability::Vision, false, CapabilitySource::Declared);
        assert!(!caps.known_absent(Capability::Vision));
        caps.apply(Capability::Vision, false, CapabilitySource::Measured);
        assert!(caps.known_absent(Capability::Vision));
        assert!(caps.has_solid_bit());
    }

    #[test]
    fn not_observed_accumulates_and_never_filters_or_beats_a_yes() {
        let mut caps = ModelCapabilities::default();
        assert!(caps.apply_measurement(Capability::Tools, Measurement::NotObserved, 10));
        assert!(caps.apply_measurement(Capability::Tools, Measurement::NotObserved, 20));
        let state = caps.get(Capability::Tools);
        assert_eq!((state.samples, state.measured_at_ms), (2, Some(20)));
        assert!(state.not_observed && !state.is_solid());
        assert!(!caps.known_absent(Capability::Tools));
        assert!(caps.apply_measurement(Capability::Tools, Measurement::Yes, 30));
        assert!(!caps.apply_measurement(Capability::Tools, Measurement::NotObserved, 40));
        assert_eq!(caps.known(Capability::Tools), Some(true));
        // A checkable "no" is measured absent and does filter.
        caps.apply_measurement(Capability::Vision, Measurement::No, 50);
        assert!(caps.known_absent(Capability::Vision));
    }

    #[test]
    fn an_older_measurement_never_replaces_a_newer_one() {
        let mut caps = ModelCapabilities::default();
        assert!(caps.apply_measurement(Capability::Vision, Measurement::Yes, 200));
        assert!(!caps.apply_measurement(Capability::Vision, Measurement::No, 100));
        assert_eq!(caps.known(Capability::Vision), Some(true));
        assert!(caps.apply_measurement(Capability::Vision, Measurement::No, 300));
        assert!(caps.known_absent(Capability::Vision));
    }

    #[test]
    fn a_user_override_is_never_replaced_by_a_measurement() {
        let mut caps = ModelCapabilities::default();
        caps.apply(Capability::Vision, true, CapabilitySource::UserOverride);
        assert!(!caps.apply_measurement(Capability::Vision, Measurement::No, 1));
        assert_eq!(caps.known(Capability::Vision), Some(true));
    }

    #[test]
    fn the_document_state_is_written_as_documents_and_the_old_name_still_loads() {
        let mut caps = ModelCapabilities::default();
        caps.apply(Capability::Document, true, CapabilitySource::Declared);
        let text = serde_json::to_string(&caps).unwrap_or_default();
        assert!(text.contains("\"documents\"") && !text.contains("\"document\""));
        let old: ModelCapabilities = serde_json::from_str(
            r#"{"document":{"supported":true,"source":"declared","measured_at_ms":null,"samples":0}}"#,
        )
        .unwrap_or_default();
        assert_eq!(old, caps);
    }
}
