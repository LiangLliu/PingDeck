//! The failover-ready resolution of a requested model name.
//!
//! Single responsibility: [`RoutingSnapshot::plan`] turns a requested name into ordered
//! [`TargetGroup`]s (one per provider and model, each with its usable keys), so the failover rules
//! can tell "next key" from "next target", plus the list of targets it skipped and why (the text
//! of the log's attempt list). It applies the disabled parts and models, the
//! capability pre-filter, the open circuits, and answers an empty result with the typed
//! [`PlanError`] the gateway maps to a status.

use crate::llm::capabilities::Capability;
use crate::llm::health::HealthBook;
use crate::llm::hub::{HubPart, HubRequest};
use crate::llm::key_book::KeyBook;
use crate::llm::store::{MAX_FALLBACKS, ModelRef, Provider, ProviderModel};
use crate::llm::wire::Protocol;
use pingdeck_base::error::AppError;

use super::{RoutingSnapshot, Target};

/// The skip reason of a gateway fallback the calling client's allowed models leave out.
pub const OUTSIDE_CLIENT_SCOPE: &str = "not allowed for this client";

/// The skip reason of a fallback whose provider an import brought in and the user has not
/// reviewed yet: called by name it works, as a fallback it does not.
pub const NOT_REVIEWED: &str = "not reviewed";

/// What a request needs from its target, for the capability pre-filter.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub struct RequestNeeds {
    /// An image part.
    pub vision: bool,
    /// A tool definition.
    pub tools: bool,
    /// A document part (PDF, text or code file).
    pub document: bool,
    /// For calls that are not chat (embeddings, decisions): the protocols a target may speak, any
    /// one of them. Empty for chat, where a target needs a chat protocol.
    pub endpoints: &'static [Protocol],
    /// The conversation the request belongs to (a hash of its stable prefix): a round robin keeps
    /// it on the key it started on, so the upstream's prompt cache of that account is reused.
    pub affinity: Option<u64>,
}

/// A hash of what stays the same for every turn of one conversation: the system prompt and the
/// text of the first message. `None` when both are empty.
fn conversation_affinity(request: &HubRequest) -> Option<u64> {
    use std::hash::{Hash, Hasher};
    let system = request.system.as_deref().unwrap_or_default();
    let first = request.messages.first();
    let texts = first
        .into_iter()
        .flat_map(|message| &message.content)
        .filter_map(|part| match part {
            HubPart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        });
    let mut hasher = std::collections::hash_map::DefaultHasher::new();
    system.hash(&mut hasher);
    let mut any = !system.is_empty();
    for text in texts {
        any |= !text.is_empty();
        text.hash(&mut hasher);
    }
    any.then(|| hasher.finish())
}

fn scan(parts: &[HubPart], needs: &mut RequestNeeds) {
    for part in parts {
        match part {
            HubPart::Image { .. } => needs.vision = true,
            HubPart::Document { .. } => needs.document = true,
            HubPart::ToolResult { content, .. } => scan(content, needs),
            _ => {}
        }
    }
}

impl RequestNeeds {
    /// The needs of a hub request: vision for image parts (also inside tool results), tools for
    /// tool definitions, document for document parts, and the conversation's affinity.
    #[must_use]
    pub fn from_hub(request: &HubRequest) -> Self {
        let mut needs = Self {
            tools: !request.tools.is_empty(),
            affinity: conversation_affinity(request),
            ..Self::default()
        };
        for message in &request.messages {
            scan(&message.content, &mut needs);
        }
        needs
    }

    /// The first capability the request needs that `model` is *known* not to have (measured or
    /// set by the user; a model that was never probed lacks nothing).
    fn missing(self, model: Option<&ProviderModel>) -> Option<Capability> {
        let model = model?;
        [
            (self.vision, Capability::Vision),
            (self.document, Capability::Document),
            (self.tools, Capability::Tools),
        ]
        .into_iter()
        .find(|(needed, capability)| *needed && model.capabilities.known_absent(*capability))
        .map(|(_, capability)| capability)
    }
}

fn capability_word(capability: Capability) -> &'static str {
    match capability {
        Capability::Vision => "vision",
        Capability::Document => "document",
        Capability::Tools => "tools",
        _ => "that capability",
    }
}

/// One provider model with the keys that may call it, in the order the key strategy gives.
#[derive(Debug, Clone)]
pub struct TargetGroup {
    /// Provider id.
    pub provider_id: String,
    /// Provider display name.
    pub provider_name: String,
    /// Upstream model id.
    pub model: String,
    /// One target per usable key; never empty.
    pub targets: Vec<Target>,
}

/// A target (or one key of it) left out of the plan, for the attempt list of the log.
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct SkippedTarget {
    /// `Provider/model`, with ` (key name)` when only one key was skipped.
    pub label: String,
    /// `disabled`, `no key`, `no chat protocol`, `circuit open`, `cooling` (the key rests after a
    /// rate limit or rejection), `no vision`, `no document`, `no tools`, [`OUTSIDE_CLIENT_SCOPE`],
    /// or the reason the provider's extra headers cannot be sent.
    pub reason: String,
    /// Provider id.
    pub provider_id: String,
    /// Provider display name.
    pub provider_name: String,
    /// The key that was skipped, when the skip is about one key.
    pub key_id: Option<String>,
    /// Upstream model id.
    pub model: String,
}

/// The ordered targets of one request.
#[derive(Debug, Clone)]
pub struct Plan {
    /// The name the client asked for, trimmed.
    pub requested: String,
    /// Usable targets: the requested model, then its fallbacks.
    pub groups: Vec<TargetGroup>,
    /// What was left out and why, in plan order.
    pub skipped: Vec<SkippedTarget>,
}

impl Plan {
    /// Keys per group, the shape [`crate::llm::failover::AttemptPlan`] walks.
    #[must_use]
    pub fn key_counts(&self) -> Vec<usize> {
        self.groups
            .iter()
            .map(|group| group.targets.len())
            .collect()
    }

    /// Every `(key id, model)` pair of the plan.
    #[must_use]
    pub fn pairs(&self) -> Vec<(&str, &str)> {
        self.groups
            .iter()
            .flat_map(|group| group.targets.iter())
            .map(|target| (target.key_id.as_str(), target.model.as_str()))
            .collect()
    }

    /// The answer when every target of the plan refused at attempt time (an open circuit that
    /// closed the door after the plan was made): [`PlanError::NoTarget`] with the earliest retry.
    #[must_use]
    pub fn unavailable(&self, health: &HealthBook) -> PlanError {
        PlanError::no_target(
            &self.requested,
            health.earliest_retry_s(&self.pairs()).unwrap_or(1),
        )
    }
}

/// Why a name could not be planned.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum PlanError {
    /// No enabled provider model matches (404).
    UnknownModel(String),
    /// The name matches, but nothing is usable (disabled, no key, no chat protocol).
    NoUsableTarget(String),
    /// Every usable target is cooling down (503 with `Retry-After`).
    NoTarget {
        /// The requested name.
        requested: String,
        /// Whole seconds until the earliest circuit may be probed, at least 1.
        retry_after_s: u64,
        /// "No target available for 'x'. The earliest retry is in 8 s."
        message: String,
    },
    /// Every other target lacks a capability the request needs (400).
    MissingCapability {
        /// The requested name.
        requested: String,
        /// What is missing.
        capability: Capability,
        /// The sentence for the client.
        message: String,
    },
    /// The calling client may not use this name (403).
    NotAllowed(String),
}

/// How a gateway answers a [`PlanError`].
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanAnswer {
    /// HTTP status.
    pub status: u16,
    /// The error `type` of the client's protocol (`not_found_error`, `overloaded_error`...).
    pub kind: &'static str,
    /// The message.
    pub message: String,
    /// `Retry-After` in whole seconds.
    pub retry_after_s: Option<u64>,
}

impl PlanError {
    /// The error of an empty plan whose targets are all cooling down.
    #[must_use]
    pub fn no_target(requested: &str, retry_after_s: u64) -> Self {
        let retry_after_s = retry_after_s.max(1);
        Self::NoTarget {
            requested: requested.to_owned(),
            retry_after_s,
            message: format!(
                "No target available for '{requested}'. The earliest retry is in {retry_after_s} s."
            ),
        }
    }

    /// Status, error type, message and `Retry-After` of the gateway answer.
    #[must_use]
    pub fn answer(&self) -> PlanAnswer {
        let plain = |status, kind, message: String| PlanAnswer {
            status,
            kind,
            message,
            retry_after_s: None,
        };
        match self {
            Self::UnknownModel(name) => plain(
                404,
                "not_found_error",
                format!("model {name} is not a provider/model of this gateway"),
            ),
            Self::NoUsableTarget(name) => plain(
                400,
                "invalid_request_error",
                format!("model {name} has no enabled provider key to call"),
            ),
            Self::NoTarget {
                retry_after_s,
                message,
                ..
            } => PlanAnswer {
                retry_after_s: Some(*retry_after_s),
                ..plain(503, "overloaded_error", message.clone())
            },
            Self::MissingCapability { message, .. } => {
                plain(400, "invalid_request_error", message.clone())
            }
            Self::NotAllowed(name) => plain(
                403,
                "permission_error",
                format!("this client may not use model {name}"),
            ),
        }
    }
}

/// A planning error with what the plan left out before it failed, so the log row of a request
/// that could not start still lists "circuit open", "disabled", "no vision"...
#[derive(Debug, Clone, PartialEq, Eq)]
pub struct PlanFailure {
    /// Why nothing was usable.
    pub error: PlanError,
    /// The targets (or keys) that were skipped, in plan order; empty for an unknown name.
    pub skipped: Vec<SkippedTarget>,
}

impl From<PlanError> for PlanFailure {
    fn from(error: PlanError) -> Self {
        Self {
            error,
            skipped: Vec::new(),
        }
    }
}

impl From<PlanError> for AppError {
    fn from(error: PlanError) -> Self {
        let message = error.answer().message;
        match error {
            PlanError::UnknownModel(_) => Self::not_found(message),
            PlanError::NoTarget { .. } => Self::network(message),
            _ => Self::validation(message),
        }
    }
}

/// One model of a plan (the requested one or a fallback) before it is checked.
struct Candidate<'a> {
    provider_id: &'a str,
    key_id: Option<&'a str>,
    model: &'a str,
    /// A stored model whose `enabled` flag is off is skipped (names resolved by the gateway); a
    /// caller that pinned the model itself is not second-guessed.
    respect_model_flag: bool,
}

/// What `plan_candidate` leaves behind besides the groups and the skip list.
#[derive(Default)]
struct Leftovers {
    /// The first capability a skipped target lacks.
    capability_skip: Option<Capability>,
    /// `(key id, model)` of every key skipped for an open circuit.
    cooling: Vec<(String, String)>,
    /// Where each key cursor starts in this plan: a cursor moves once per request even when the
    /// chain names the same provider twice.
    starts: std::collections::HashMap<String, usize>,
    /// Groups whose every key rests: tried after every other group.
    resting: Vec<TargetGroup>,
    /// A preview: key cursors are read, never moved.
    peek: bool,
}

impl RoutingSnapshot {
    /// The stored model `provider_id`/`model_id`, if the provider has it in its list.
    fn stored_model(&self, provider_id: &str, model_id: &str) -> Option<&ProviderModel> {
        self.models
            .iter()
            .find(|model| model.provider_id == provider_id && model.model_id == model_id)
    }

    /// Whether a client scope admits the resolved `model`: its id form, its name form, or a legacy
    /// name that maps to it (the ways [`RoutingSnapshot::allowed_by`] admits a requested name).
    #[must_use]
    pub fn admits(&self, model: &ModelRef, allows: &dyn Fn(&str) -> bool) -> bool {
        allows(&model.id_form())
            || allows(&self.display_ref(&model.provider_id, &model.model))
            || self
                .legacy_names
                .iter()
                .any(|(name, target)| target == model && allows(name))
    }

    /// Plans `requested` (a model reference, see [`RoutingSnapshot::resolve_ref`]) for a gateway
    /// request with `needs`.
    ///
    /// The requested model comes first, then its gateway rule chain
    /// ([`RoutingSnapshot::gateway_fallbacks`]) in the user's order (a fallback's
    /// own chain is not followed); the keys of a model start where the provider's key strategy
    /// says and wrap (see [`RoutingSnapshot`]). Skipped, in this order of checks: disabled
    /// provider or stored model (an unstored model id still routes), no enabled key,
    /// extra headers that cannot be resolved, no chat protocol, a capability the request needs
    /// that the model is known to lack, keys whose circuit is open, and resting keys
    /// (`cooling`, when the provider's cooldown is on) unless every key of the model rests: then
    /// they are all tried, the soonest to end first.
    /// `client_scope` says whether the calling client may use a name; it is asked with the
    /// resolved `provider_id/model` and, failing that, the name as sent. It bounds the rule chain
    /// too: a fallback it does not [admit](RoutingSnapshot::admits) is skipped as
    /// [`OUTSIDE_CLIENT_SCOPE`] and never called.
    ///
    /// # Errors
    /// [`PlanError::UnknownModel`] for no match (also a model of a disabled provider or a
    /// stored, disabled model when no fallback is usable either), [`PlanError::NotAllowed`],
    /// and, when no usable target remains:
    /// [`PlanError::NoTarget`] if some were skipped for an open circuit,
    /// [`PlanError::MissingCapability`] if some lack a needed capability,
    /// else [`PlanError::NoUsableTarget`].
    pub fn plan(
        &self,
        requested: &str,
        needs: &RequestNeeds,
        client_scope: Option<&dyn Fn(&str) -> bool>,
        health: &HealthBook,
        keys: &KeyBook,
    ) -> Result<Plan, PlanError> {
        self.plan_explained(requested, needs, client_scope, health, keys)
            .map_err(|failure| failure.error)
    }

    /// [`RoutingSnapshot::plan`], with the skipped targets on the failure.
    ///
    /// # Errors
    /// As [`RoutingSnapshot::plan`], as a [`PlanFailure`].
    pub fn plan_explained(
        &self,
        requested: &str,
        needs: &RequestNeeds,
        client_scope: Option<&dyn Fn(&str) -> bool>,
        health: &HealthBook,
        keys: &KeyBook,
    ) -> Result<Plan, PlanFailure> {
        self.plan_chain(requested, None, needs, client_scope, health, keys, false)
    }

    /// [`RoutingSnapshot::plan_explained`] for the route preview: the same plan the next
    /// request would get, but no round-robin cursor moves.
    ///
    /// # Errors
    /// As [`RoutingSnapshot::plan_explained`].
    pub fn plan_preview(
        &self,
        requested: &str,
        needs: &RequestNeeds,
        client_scope: Option<&dyn Fn(&str) -> bool>,
        health: &HealthBook,
        keys: &KeyBook,
    ) -> Result<Plan, PlanFailure> {
        self.plan_chain(requested, None, needs, client_scope, health, keys, true)
    }

    /// Plans `requested` for an in-app caller that brings its own fallback chain: the requested
    /// model (skipped when it no longer resolves or is disabled), then `fallbacks` (model
    /// references) in order, leaving out entries that do not
    /// resolve, the requested model itself and repeats, at most [`MAX_FALLBACKS`]. The gateway
    /// rules are not consulted. Skips and errors as [`RoutingSnapshot::plan`] (no client scope).
    ///
    /// # Errors
    /// As [`RoutingSnapshot::plan`].
    pub fn plan_with_fallbacks(
        &self,
        requested: &str,
        fallbacks: &[String],
        needs: &RequestNeeds,
        health: &HealthBook,
        keys: &KeyBook,
    ) -> Result<Plan, PlanError> {
        self.plan_with_fallbacks_explained(requested, fallbacks, needs, health, keys)
            .map_err(|failure| failure.error)
    }

    /// [`RoutingSnapshot::plan_with_fallbacks`], with the skipped targets on the failure.
    ///
    /// # Errors
    /// As [`RoutingSnapshot::plan_with_fallbacks`], as a [`PlanFailure`].
    pub fn plan_with_fallbacks_explained(
        &self,
        requested: &str,
        fallbacks: &[String],
        needs: &RequestNeeds,
        health: &HealthBook,
        keys: &KeyBook,
    ) -> Result<Plan, PlanFailure> {
        self.plan_chain(requested, Some(fallbacks), needs, None, health, keys, false)
    }

    /// The one planning loop: the requested model, then the caller's chain when given (`Some`),
    /// else the gateway rule of the requested model. `peek` previews: no key cursor moves.
    #[allow(clippy::too_many_arguments)]
    fn plan_chain(
        &self,
        requested: &str,
        caller_chain: Option<&[String]>,
        needs: &RequestNeeds,
        client_scope: Option<&dyn Fn(&str) -> bool>,
        health: &HealthBook,
        keys: &KeyBook,
        peek: bool,
    ) -> Result<Plan, PlanFailure> {
        let requested = requested.trim();
        if client_scope.is_some_and(|allows| !self.allowed_by(requested, allows)) {
            return Err(PlanError::NotAllowed(requested.to_owned()).into());
        }
        // An unavailable primary (disabled, or gone from a caller's chain) is skipped
        // and the chain still runs; only when nothing in it is usable does the request fail as an
        // unknown model.
        let primary = self.resolve_ref(requested);
        if primary.is_none() && caller_chain.is_none() {
            return Err(PlanError::UnknownModel(requested.to_owned()).into());
        }
        let primary_usable = primary.as_ref().is_some_and(|primary| {
            self.providers
                .get(&primary.provider_id)
                .is_some_and(Provider::routable)
                && self
                    .stored_model(&primary.provider_id, &primary.model)
                    .is_none_or(|stored| stored.enabled)
        });
        let fallbacks: Vec<ModelRef> = match caller_chain {
            None => primary
                .as_ref()
                .and_then(|primary| self.gateway_fallbacks.get(primary))
                .cloned()
                .unwrap_or_default(),
            Some(chain) => {
                let mut out: Vec<ModelRef> = Vec::new();
                for model in chain.iter().filter_map(|name| self.resolve_ref(name)) {
                    if out.len() == MAX_FALLBACKS {
                        break;
                    }
                    if primary.as_ref() != Some(&model) && !out.contains(&model) {
                        out.push(model);
                    }
                }
                out
            }
        };
        let mut plan = Plan {
            requested: requested.to_owned(),
            groups: Vec::new(),
            skipped: Vec::new(),
        };
        let mut left = Leftovers {
            peek,
            ..Leftovers::default()
        };
        // A client scope also bounds the chain; a fallback it does not admit is never called.
        // Neither is a fallback on a provider waiting for review.
        let mut fallbacks = fallbacks;
        {
            let allows = client_scope;
            fallbacks.retain(|model| {
                let reviewed = self
                    .providers
                    .get(&model.provider_id)
                    .is_none_or(|provider| provider.review_pending_since_ms.is_none());
                let admitted = reviewed && allows.is_none_or(|allows| self.admits(model, allows));
                if !admitted {
                    plan.skipped.push(SkippedTarget {
                        provider_name: self
                            .providers
                            .get(&model.provider_id)
                            .map_or(&model.provider_id, |provider| &provider.name)
                            .clone(),
                        label: self.display_ref(&model.provider_id, &model.model),
                        reason: if reviewed {
                            OUTSIDE_CLIENT_SCOPE
                        } else {
                            NOT_REVIEWED
                        }
                        .to_owned(),
                        provider_id: model.provider_id.clone(),
                        key_id: None,
                        model: model.model.clone(),
                    });
                }
                admitted
            });
        }
        let candidates = primary.iter().chain(&fallbacks).map(|model| Candidate {
            provider_id: &model.provider_id,
            key_id: None,
            model: &model.model,
            respect_model_flag: true,
        });
        for candidate in candidates {
            self.plan_candidate(&mut plan, &candidate, needs, health, keys, &mut left);
        }
        if !primary_usable && plan.groups.is_empty() {
            return Err(PlanError::UnknownModel(requested.to_owned()).into());
        }
        Self::settle(plan, left, health)
    }

    /// Plans one provider model the caller chose (a pinned chat): the given key or all enabled
    /// keys, with the same skips as [`RoutingSnapshot::plan`] except that a stored model's
    /// `enabled` flag is not consulted.
    ///
    /// # Errors
    /// As [`RoutingSnapshot::plan`], never [`PlanError::UnknownModel`]: a missing provider or key
    /// is [`PlanError::NoUsableTarget`].
    pub fn plan_pinned(
        &self,
        provider_id: &str,
        key_id: Option<&str>,
        model: &str,
        needs: &RequestNeeds,
        health: &HealthBook,
        keys: &KeyBook,
    ) -> Result<Plan, PlanError> {
        self.plan_pinned_explained(provider_id, key_id, model, needs, health, keys)
            .map_err(|failure| failure.error)
    }

    /// [`RoutingSnapshot::plan_pinned`], with the skipped targets on the failure.
    ///
    /// # Errors
    /// As [`RoutingSnapshot::plan_pinned`], as a [`PlanFailure`].
    pub fn plan_pinned_explained(
        &self,
        provider_id: &str,
        key_id: Option<&str>,
        model: &str,
        needs: &RequestNeeds,
        health: &HealthBook,
        keys: &KeyBook,
    ) -> Result<Plan, PlanFailure> {
        let mut plan = Plan {
            requested: format!("{provider_id}/{model}"),
            groups: Vec::new(),
            skipped: Vec::new(),
        };
        let mut left = Leftovers::default();
        let candidate = Candidate {
            provider_id,
            key_id,
            model,
            respect_model_flag: false,
        };
        self.plan_candidate(&mut plan, &candidate, needs, health, keys, &mut left);
        Self::settle(plan, left, health)
    }

    /// The plan, or the typed reason it is empty.
    fn settle(mut plan: Plan, left: Leftovers, health: &HealthBook) -> Result<Plan, PlanFailure> {
        plan.groups.extend(left.resting);
        if !plan.groups.is_empty() {
            return Ok(plan);
        }
        let Plan {
            requested, skipped, ..
        } = plan;
        let fail = |error: PlanError, skipped: Vec<SkippedTarget>| PlanFailure { error, skipped };
        if !left.cooling.is_empty() {
            let pairs: Vec<(&str, &str)> = left
                .cooling
                .iter()
                .map(|(key, model)| (key.as_str(), model.as_str()))
                .collect();
            return Err(fail(
                PlanError::no_target(&requested, health.earliest_retry_s(&pairs).unwrap_or(1)),
                skipped,
            ));
        }
        if let Some(capability) = left.capability_skip {
            let word = capability_word(capability);
            return Err(fail(
                PlanError::MissingCapability {
                    message: format!(
                        "No target of '{requested}' supports {word}, which this request needs."
                    ),
                    requested,
                    capability,
                },
                skipped,
            ));
        }
        Err(fail(PlanError::NoUsableTarget(requested), skipped))
    }

    fn plan_candidate(
        &self,
        plan: &mut Plan,
        candidate: &Candidate<'_>,
        needs: &RequestNeeds,
        health: &HealthBook,
        books: &KeyBook,
        left: &mut Leftovers,
    ) {
        let model = candidate.model;
        let provider = self.providers.get(candidate.provider_id);
        let provider_name =
            provider.map_or(candidate.provider_id, |provider| provider.name.as_str());
        let mut skip = |reason: &str, key: Option<&crate::llm::store::ResolvedKey>| {
            let label = match key {
                Some(key) => format!("{provider_name}/{model} (key {})", key.key.name),
                None => format!("{provider_name}/{model}"),
            };
            plan.skipped.push(SkippedTarget {
                label,
                reason: reason.to_owned(),
                provider_id: candidate.provider_id.to_owned(),
                provider_name: provider_name.to_owned(),
                key_id: key.map(|key| key.key.id.clone()),
                model: model.to_owned(),
            });
        };
        let stored = self.stored_model(candidate.provider_id, model);
        let Some(provider) = provider.filter(|provider| {
            provider.routable()
                && (!candidate.respect_model_flag || stored.is_none_or(|stored| stored.enabled))
        }) else {
            skip("disabled", None);
            return;
        };
        let keys = self.candidate_keys(
            provider,
            candidate.key_id,
            model,
            books,
            &mut left.starts,
            left.peek,
            needs.affinity,
        );
        if keys.is_empty() {
            skip("no key", None);
            return;
        }
        let quirks = match self.quirks_of(provider) {
            Ok(quirks) => quirks,
            Err(reason) => {
                skip(&reason, None);
                return;
            }
        };
        let speaks = |target: &Target| {
            if needs.endpoints.is_empty() {
                target.chat_protocol(None).is_some()
            } else {
                needs
                    .endpoints
                    .iter()
                    .any(|protocol| target.endpoint(*protocol).is_some())
            }
        };
        // Each key speaks what the user and the relay allow it for this model (relay groups).
        let (targets, keys): (Vec<Target>, Vec<_>) = keys
            .into_iter()
            .map(|key| (Target::new(provider, &quirks, key, model, self), key))
            .filter(|(target, _)| speaks(target))
            .unzip();
        if targets.is_empty() {
            skip(
                if needs.endpoints.is_empty() {
                    "no chat protocol"
                } else {
                    "no matching endpoint"
                },
                None,
            );
            return;
        }
        if let Some(capability) = needs.missing(stored) {
            left.capability_skip = left.capability_skip.or(Some(capability));
            skip(&format!("no {}", capability_word(capability)), None);
            return;
        }
        let rests = targets[0].key_cooldown;
        let mut usable = Vec::with_capacity(targets.len());
        let mut cooling = Vec::new();
        for (target, key) in targets.into_iter().zip(keys) {
            if health.blocked_for(&target.key_id, model).is_some() {
                left.cooling.push((target.key_id.clone(), model.to_owned()));
                skip("circuit open", Some(key));
            } else if let Some(until) = books.cooling_until(&target.key_id, model).filter(|_| rests)
            {
                cooling.push((until, target, key));
            } else {
                usable.push(target);
            }
        }
        let resting = usable.is_empty() && !cooling.is_empty();
        if resting {
            // Every key rests: try them anyway rather than refuse, the soonest to end first, but
            // after every group that has a key ready.
            cooling.sort_by_key(|(until, ..)| *until);
            usable.extend(cooling.into_iter().map(|(_, target, _)| target));
        } else {
            for (_, _, key) in cooling {
                skip("cooling", Some(key));
            }
        }
        if !usable.is_empty() {
            let group = TargetGroup {
                provider_id: provider.id.clone(),
                provider_name: provider.name.clone(),
                model: model.to_owned(),
                targets: usable,
            };
            if resting {
                left.resting.push(group);
            } else {
                plan.groups.push(group);
            }
        }
    }
}

/// How a model of a gateway rule stands right now, from the same checks planning skips on.
#[derive(Debug, Clone, Copy, PartialEq, Eq, serde::Serialize, serde::Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RuleStatus {
    /// Routable.
    Ok,
    /// Its provider or the stored model is switched off.
    Disabled,
    /// No enabled key may call it.
    NoKey,
    /// Every key that may call it has an open circuit.
    CircuitOpen,
    /// Its provider is gone (the model reference resolves to nothing).
    Unresolved,
}

impl RoutingSnapshot {
    /// The [`RuleStatus`] of one model of a gateway rule.
    #[must_use]
    pub fn rule_status(&self, model: &ModelRef, health: &HealthBook) -> RuleStatus {
        let Some(provider) = self.providers.get(&model.provider_id) else {
            return RuleStatus::Unresolved;
        };
        if !provider.routable()
            || self
                .stored_model(&model.provider_id, &model.model)
                .is_some_and(|stored| !stored.enabled)
        {
            return RuleStatus::Disabled;
        }
        let keys: Vec<_> = self
            .keys
            .get(&model.provider_id)
            .map(|keys| {
                keys.iter()
                    .filter(|key| key.key.enabled && self.key_scopes.serves(&key.key, &model.model))
                    .collect()
            })
            .unwrap_or_default();
        if keys.is_empty() {
            return RuleStatus::NoKey;
        }
        if keys
            .iter()
            .all(|key| health.blocked_for(&key.key.id, &model.model).is_some())
        {
            return RuleStatus::CircuitOpen;
        }
        RuleStatus::Ok
    }

    /// Whether `provider_id`/`model` is a stored model (a rule whose primary is not is an orphan).
    #[must_use]
    pub fn has_model(&self, model: &ModelRef) -> bool {
        self.stored_model(&model.provider_id, &model.model)
            .is_some()
    }
}
