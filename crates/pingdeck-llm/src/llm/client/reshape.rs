//! Reshaping an in-process request for the provider model an attempt calls.
//!
//! Single responsibility: a caller builds its request for the model it asked for, but failover may
//! hand it to a fallback that reads no images or has a smaller window. A caller that passes a
//! [`Reshape`] gets each attempt's request adapted before it is sent: image parts become a text
//! note for a model known not to read them (so planning keeps such a target, after every target
//! that reads them), then the caller adapts the rest. This happens before the first content event
//! only; the commit point of `flow` does not move.

use std::sync::Arc;

use crate::llm::capabilities::{Capability, ModelCapabilities};
use crate::llm::hub::{HubPart, HubRequest};
use crate::llm::router::{RoutingSnapshot, Target};

/// What the store knows about the provider model an attempt calls.
#[derive(Debug, Clone, Default)]
pub struct TargetFacts {
    /// Provider id.
    pub provider_id: String,
    /// Upstream model id.
    pub model: String,
    /// Context window in tokens (declared > measured > listed), `None` when unknown.
    pub context_limit: Option<u64>,
    /// What the model can do.
    pub capabilities: ModelCapabilities,
}

/// A caller's adaptation of its request to the target of one attempt.
pub type Reshape = Arc<dyn Fn(&TargetFacts, &mut HubRequest) + Send + Sync>;

impl TargetFacts {
    /// The facts of `target` in `snapshot`; a model that is not stored has unknown facts.
    pub(super) fn of(snapshot: &RoutingSnapshot, target: &Target) -> Self {
        let stored = snapshot.models.iter().find(|model| {
            model.provider_id == target.provider_id && model.model_id == target.model
        });
        Self {
            provider_id: target.provider_id.clone(),
            model: target.model.clone(),
            context_limit: stored.and_then(|model| model.context),
            capabilities: stored.map(|model| model.capabilities).unwrap_or_default(),
        }
    }
}

/// Whether `snapshot` knows `provider_id`/`model` cannot read images.
pub(super) fn reads_no_images(snapshot: &RoutingSnapshot, provider_id: &str, model: &str) -> bool {
    snapshot.models.iter().any(|stored| {
        stored.provider_id == provider_id
            && stored.model_id == model
            && stored.capabilities.known_absent(Capability::Vision)
    })
}

/// Adapts `request` to `facts`: images out for a model known not to read them, then `reshape`.
pub fn shape_for(reshape: &Reshape, facts: &TargetFacts, request: &mut HubRequest) {
    if facts.capabilities.known_absent(Capability::Vision) {
        let note = format!("[Image omitted: {} cannot read images.]", facts.model);
        for message in &mut request.messages {
            drop_images(&mut message.content, &note);
        }
    }
    reshape(facts, request);
}

fn drop_images(parts: &mut [HubPart], note: &str) {
    for part in parts {
        match part {
            HubPart::Image { .. } => *part = HubPart::text(note),
            HubPart::ToolResult { content, .. } => drop_images(content, note),
            _ => {}
        }
    }
}
