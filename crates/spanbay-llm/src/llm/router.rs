//! Turning a requested model name into an ordered list of upstream targets.
//!
//! Single responsibility: given a snapshot of the configuration, resolve what a client asked for
//! (a model reference `<provider name or id>/<model>`, or a legacy alias name) into concrete
//! [`Target`]s: which provider, which key, which upstream model, which protocol. The snapshot is
//! immutable and shared (`Arc`), so a request sees one consistent configuration from first to
//! last attempt.
//!
//! Order rules: the requested model comes first, then a fallback chain in the order the user set
//! (not recursive): for a gateway request the gateway rule of the requested model, for an in-app
//! caller the chain it brings; each model expands to the provider's enabled keys in list order,
//! started where the provider's key strategy says (first key, round robin or random) and
//! wrapping, so each key is tried at most once. [`RoutingSnapshot::plan`] (in `plan`) is the
//! resolver of the gateway, [`RoutingSnapshot::plan_with_fallbacks`] the one of the chat client:
//! they group the keys per provider model so the failover rules can tell "next key" from "next
//! model", and say why a target was skipped.

mod plan;
mod scope;

use std::collections::{BTreeMap, HashMap};

use crate::llm::capabilities::Capability;
use crate::llm::key_book::KeyBook;
use crate::llm::presets::ModelRole;
use crate::llm::probe::listing::{ModelKind, is_decision_model, model_kind};
use crate::llm::store::{ModelRef, Provider, ProviderModel, ResolvedKey};
use crate::llm::wire::{AuthStyle, Protocol, ProviderEndpoint, Quirks, endpoint_url};
use spanbay_base::error::AppError;

pub use plan::{
    NOT_REVIEWED, OUTSIDE_CLIENT_SCOPE, Plan, PlanAnswer, PlanError, PlanFailure, RequestNeeds,
    RuleStatus, SkippedTarget, TargetGroup,
};
pub use scope::KeyScopes;

/// Protocols tried, in order, when a provider does not speak the one the client used.
const CHAT_PREFERENCE: [Protocol; 4] = [
    Protocol::OpenaiChat,
    Protocol::AnthropicMessages,
    Protocol::OpenaiResponses,
    Protocol::Gemini,
];

/// One concrete upstream attempt: a provider key calling a model.
#[derive(Clone)]
pub struct Target {
    /// Provider id.
    pub provider_id: String,
    /// Provider display name.
    pub provider_name: String,
    /// Key id.
    pub key_id: String,
    /// The secret.
    pub secret: String,
    /// Host root.
    pub base_url: String,
    /// Protocols the provider speaks.
    pub endpoints: Vec<ProviderEndpoint>,
    /// Provider-level authentication.
    pub auth: AuthStyle,
    /// Behaviour differences.
    pub quirks: Quirks,
    /// The provider's preferred chat protocol.
    pub primary_protocol: Option<Protocol>,
    /// The provider's proxy (empty: global setting, then system).
    pub proxy_url: String,
    /// Upstream model id.
    pub model: String,
    /// Whether a rate-limited or rejected key of the provider rests (multi-key rotation).
    pub key_cooldown: bool,
    /// The model's resolved output cap (declared, measured, listed), the default
    /// `max_tokens` of a protocol that requires one.
    pub max_output: Option<u32>,
}

/// Leaves out the secret, the proxy URL (userinfo) and the quirks (extra headers), so a stray
/// `{:?}` never leaks a credential.
impl std::fmt::Debug for Target {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("Target")
            .field("provider_id", &self.provider_id)
            .field("provider_name", &self.provider_name)
            .field("key_id", &self.key_id)
            .field("base_url", &self.base_url)
            .field("endpoints", &self.endpoints)
            .field("auth", &self.auth)
            .field("model", &self.model)
            .finish_non_exhaustive()
    }
}

impl Target {
    /// The target of `key` calling `model` on `provider`; `quirks` are the provider's with the
    /// extra headers resolved. The endpoints are the ones `scopes` lets this key use for this
    /// model, and the key's native protocol, when it is one of them, is the preferred one.
    fn new(
        provider: &Provider,
        quirks: &Quirks,
        key: &ResolvedKey,
        model: &str,
        snapshot: &RoutingSnapshot,
    ) -> Self {
        let scopes = &snapshot.key_scopes;
        let endpoints = scopes.endpoints_for(&provider.endpoints, &key.key, model);
        let speaks = |protocol: &Protocol| {
            endpoints
                .iter()
                .any(|endpoint| endpoint.enabled && endpoint.protocol == *protocol)
        };
        let primary_protocol = key
            .key
            .preferred_protocol
            .filter(speaks)
            .or(provider.primary_protocol);
        Self {
            provider_id: provider.id.clone(),
            provider_name: provider.name.clone(),
            key_id: key.key.id.clone(),
            secret: key.secret.clone(),
            base_url: provider.base_url.clone(),
            endpoints,
            auth: provider.auth,
            quirks: quirks.clone(),
            primary_protocol,
            proxy_url: provider.proxy_url.clone(),
            model: model.to_owned(),
            key_cooldown: provider.key_cooldown.unwrap_or(true),
            max_output: snapshot.max_output_of(&provider.id, model),
        }
    }

    /// The enabled endpoint for `protocol`, if the provider speaks it.
    #[must_use]
    pub fn endpoint(&self, protocol: Protocol) -> Option<&ProviderEndpoint> {
        self.endpoints
            .iter()
            .find(|endpoint| endpoint.enabled && endpoint.protocol == protocol)
    }

    /// Authentication to use on `protocol`: the endpoint's override, else the provider's.
    #[must_use]
    pub fn auth_for(&self, protocol: Protocol) -> AuthStyle {
        self.endpoint(protocol)
            .and_then(|endpoint| endpoint.auth)
            .unwrap_or(self.auth)
    }

    /// Request URL for `protocol`.
    ///
    /// # Errors
    /// [`AppError::Unsupported`] when the provider does not speak `protocol`.
    pub fn url(&self, protocol: Protocol, stream: bool) -> Result<String, AppError> {
        let endpoint = self.endpoint(protocol).ok_or_else(|| {
            AppError::unsupported(format!(
                "provider {} does not speak {}",
                self.provider_name,
                protocol.as_str()
            ))
        })?;
        endpoint_url(
            &self.base_url,
            &endpoint.path_prefix,
            endpoint.url_mode,
            protocol,
            Some(&self.model),
            stream,
        )
    }

    /// The chat protocol to use: the caller's preferred one when the provider has it
    /// (no conversion), else the provider's `primary_protocol` when it is chat-capable, else the
    /// first of the fixed preference order. Embeddings and decision protocols are never chosen.
    #[must_use]
    pub fn chat_protocol(&self, preferred: Option<Protocol>) -> Option<Protocol> {
        let spoken = |protocol: &Protocol| protocol.is_chat() && self.endpoint(*protocol).is_some();
        preferred
            .filter(spoken)
            .or_else(|| self.primary_protocol.filter(spoken))
            .or_else(|| CHAT_PREFERENCE.into_iter().find(spoken))
    }

    /// The chat protocol an in-app request with these `needs` uses: Responses for a request with
    /// tools when the quirks say so for this model and the provider speaks it, else
    /// [`Target::chat_protocol`] without a preference.
    #[must_use]
    pub fn chat_protocol_for(&self, needs: &RequestNeeds) -> Option<Protocol> {
        if needs.tools
            && self.endpoint(Protocol::OpenaiResponses).is_some()
            && self.quirks.tools_via_responses_for(&self.model)
        {
            return Some(Protocol::OpenaiResponses);
        }
        self.chat_protocol(None)
    }
}

/// The extra request headers of every provider with their secrets resolved (the store keeps
/// placeholders), or the reason a provider's headers cannot be resolved. `Debug` shows names only.
#[derive(Clone, Default)]
pub struct ProviderHeaders {
    resolved: HashMap<String, BTreeMap<String, String>>,
    errors: HashMap<String, String>,
}

impl ProviderHeaders {
    /// Records the outcome of resolving `provider_id`'s headers: the values, or the error text
    /// (which names the header and never its value).
    pub fn insert(
        &mut self,
        provider_id: &str,
        outcome: Result<BTreeMap<String, String>, AppError>,
    ) {
        match outcome {
            Ok(headers) => {
                self.resolved.insert(provider_id.to_owned(), headers);
            }
            Err(error) => {
                self.errors
                    .insert(provider_id.to_owned(), error.message().to_owned());
            }
        }
    }
}

impl std::fmt::Debug for ProviderHeaders {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let names: BTreeMap<&String, Vec<&String>> = self
            .resolved
            .iter()
            .map(|(provider, headers)| (provider, headers.keys().collect()))
            .collect();
        f.debug_struct("ProviderHeaders")
            .field("resolved_names", &names)
            .field("errors", &self.errors)
            .finish()
    }
}

/// An immutable copy of everything routing needs.
#[derive(Debug, Clone, Default)]
pub struct RoutingSnapshot {
    /// Providers by id.
    pub providers: HashMap<String, Provider>,
    /// Enabled keys per provider id, in configured order.
    pub keys: HashMap<String, Vec<ResolvedKey>>,
    /// Provider models.
    pub models: Vec<ProviderModel>,
    /// Gateway rules: the fallback chain of a requested model (also for models not stored in the
    /// list). Only gateway requests read them; in-app callers bring their own chain
    /// ([`RoutingSnapshot::plan_with_fallbacks`]).
    pub gateway_fallbacks: HashMap<ModelRef, Vec<ModelRef>>,
    /// Old alias names that still resolve, to the model each named.
    pub legacy_names: HashMap<String, ModelRef>,
    /// Former provider names (lower case) that still resolve after a rename, to the
    /// provider id and the time they stop.
    pub former_names: HashMap<String, (String, i64)>,
    /// Resolved extra headers of the providers (see [`ProviderHeaders`]).
    pub headers: ProviderHeaders,
    /// Which key may call which model (see [`KeyScopes`]).
    pub key_scopes: KeyScopes,
}

/// What a model answers: fallbacks must answer the same kind of call.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum CallKind {
    /// Any chat protocol (converted through the hub).
    Chat,
    /// Embeddings: only the same model on another provider may stand in.
    Embedding,
    /// A decision API: only the same protocol.
    Decision(Protocol),
}

/// Why a model may not be a fallback of another, as the UI's reason codes.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum Incompatible {
    /// The model itself.
    SelfRef,
    /// Another kind of call (chat, embedding, decision).
    Kind,
    /// Another embedding model (other vectors).
    EmbeddingModel,
    /// Another decision protocol.
    DecisionProtocol,
}

impl Incompatible {
    /// The wire code (`self`, `kind`, `embedding_model`, `decision_protocol`).
    #[must_use]
    pub const fn code(self) -> &'static str {
        match self {
            Self::SelfRef => "self",
            Self::Kind => "kind",
            Self::EmbeddingModel => "embedding_model",
            Self::DecisionProtocol => "decision_protocol",
        }
    }
}

/// The model id an embedding fallback must share: lowercase, without any `vendor/` prefix.
fn normalized_model(model: &str) -> String {
    model.rsplit('/').next().unwrap_or(model).to_lowercase()
}

/// Whether `fallback` (of kind `fallback_kind`) may stand in for `primary`: chat for chat, the
/// same embedding model for an embedding, the same decision protocol for a decision.
///
/// # Errors
/// The [`Incompatible`] reason.
pub fn check_fallback(
    primary: (&ModelRef, CallKind),
    fallback: (&ModelRef, CallKind),
) -> Result<(), Incompatible> {
    if primary.0 == fallback.0 {
        return Err(Incompatible::SelfRef);
    }
    match (primary.1, fallback.1) {
        (CallKind::Chat, CallKind::Chat) => Ok(()),
        (CallKind::Embedding, CallKind::Embedding) => {
            if normalized_model(&primary.0.model) == normalized_model(&fallback.0.model) {
                Ok(())
            } else {
                Err(Incompatible::EmbeddingModel)
            }
        }
        (CallKind::Decision(a), CallKind::Decision(b)) => {
            if a == b {
                Ok(())
            } else {
                Err(Incompatible::DecisionProtocol)
            }
        }
        _ => Err(Incompatible::Kind),
    }
}

/// Milliseconds since the epoch (former names expire on the wall clock).
fn wall_ms() -> i64 {
    std::time::SystemTime::now()
        .duration_since(std::time::UNIX_EPOCH)
        .map_or(0, |since| {
            i64::try_from(since.as_millis()).unwrap_or(i64::MAX)
        })
}

/// Levenshtein distance over chars (names are short).
fn edit_distance(a: &str, b: &str) -> usize {
    let b: Vec<char> = b.chars().collect();
    let mut row: Vec<usize> = (0..=b.len()).collect();
    for (i, ca) in a.chars().enumerate() {
        let mut diagonal = row[0];
        row[0] = i + 1;
        for (j, cb) in b.iter().enumerate() {
            let next = (diagonal + usize::from(ca != *cb))
                .min(row[j] + 1)
                .min(row[j + 1] + 1);
            diagonal = row[j + 1];
            row[j + 1] = next;
        }
    }
    row[b.len()]
}

/// A provider with more enabled models than this lists its direct ids in `GET /v1/models` only
/// when its `list_in_models` switch is on.
pub const LIST_ALL_UP_TO: usize = 50;

impl RoutingSnapshot {
    fn expand(&self, provider_id: &str, key_id: Option<&str>, model: &str, out: &mut Vec<Target>) {
        let Some(provider) = self
            .providers
            .get(provider_id)
            .filter(|provider| provider.routable())
        else {
            return;
        };
        let Ok(quirks) = self.quirks_of(provider) else {
            return;
        };
        for key in self.enabled_keys(provider_id, key_id) {
            if key_id.is_some() || self.key_scopes.serves(&key.key, model) {
                out.push(Target::new(provider, &quirks, key, model, self));
            }
        }
    }

    /// The provider's quirks with its extra headers resolved, or why they cannot be (a missing
    /// secret: the text names the header, never a value).
    fn quirks_of(&self, provider: &Provider) -> Result<Quirks, String> {
        if let Some(reason) = self.headers.errors.get(&provider.id) {
            return Err(reason.clone());
        }
        // An unset flag follows the provider's preset.
        let preset = provider
            .preset_id
            .as_deref()
            .and_then(|id| crate::llm::presets::presets().ok()?.provider(id));
        let mut quirks = match preset {
            Some(preset) => provider.quirks.clone().or_preset(&preset.quirks),
            None => provider.quirks.clone(),
        };
        if let Some(resolved) = self.headers.resolved.get(&provider.id) {
            quirks.extra_headers.clone_from(resolved);
        }
        Ok(quirks)
    }

    /// The resolved output cap of a stored model, if it has one.
    fn max_output_of(&self, provider_id: &str, model: &str) -> Option<u32> {
        self.models
            .iter()
            .find(|stored| stored.provider_id == provider_id && stored.model_id == model)
            .and_then(|stored| stored.max_output)
            .map(|cap| u32::try_from(cap).unwrap_or(u32::MAX))
    }

    /// The enabled keys of a provider (only `key_id` when given), in configured order.
    fn enabled_keys(&self, provider_id: &str, key_id: Option<&str>) -> Vec<&ResolvedKey> {
        self.keys
            .get(provider_id)
            .map(|keys| {
                keys.iter()
                    .filter(|key| {
                        key.key.enabled && key_id.is_none_or(|wanted| key.key.id == wanted)
                    })
                    .collect()
            })
            .unwrap_or_default()
    }

    /// The enabled keys that may call `model` (see [`KeyScopes::serves`]), rotated to start where
    /// the provider's key strategy says; a pinned key is the only candidate and neither the scope
    /// nor the strategy applies. A round robin keeps one cursor per set of candidates, so each
    /// relay group rotates on its own, and keeps a conversation (`affinity`) on its first key.
    #[allow(clippy::too_many_arguments)] // one call site (`plan_candidate`); a struct would only rename them
    fn candidate_keys(
        &self,
        provider: &Provider,
        key_id: Option<&str>,
        model: &str,
        books: &KeyBook,
        starts: &mut HashMap<String, usize>,
        peek: bool,
        affinity: Option<u64>,
    ) -> Vec<&ResolvedKey> {
        let mut keys = self.enabled_keys(&provider.id, key_id);
        if key_id.is_some() {
            return keys;
        }
        keys.retain(|key| self.key_scopes.serves(&key.key, model));
        let strategy = provider.key_strategy.unwrap_or_default();
        let mut cursor = provider.id.clone();
        for key in &keys {
            cursor.push('|');
            cursor.push_str(&key.key.id);
        }
        let len = keys.len();
        let start = *starts.entry(cursor).or_insert_with_key(|cursor| {
            if peek {
                books.peek_start(cursor, strategy, len, affinity)
            } else {
                books.start(cursor, strategy, len, affinity)
            }
        });
        keys.rotate_left(start % len.max(1));
        keys
    }

    /// Model names the gateway offers, in name form (`<provider name>/<model>`): every enabled
    /// model of an enabled provider that has an enabled key. A provider with more than
    /// [`LIST_ALL_UP_TO`] enabled models is listed only when its `list_in_models` switch is on
    /// (`None` means on up to that many models, off above); direct ids resolve either way. Legacy
    /// alias names are never listed.
    #[must_use]
    pub fn listed_models(&self) -> Vec<String> {
        self.listed_via(None)
    }

    /// What [`Self::listed_models`] offers through one protocol family: only models of providers
    /// with an endpoint speaking one of `protocols` (the per-family "Models" count of the gateway
    /// page).
    #[must_use]
    pub fn listed_models_via(&self, protocols: &[Protocol]) -> Vec<String> {
        self.listed_via(Some(protocols))
    }

    fn listed_via(&self, protocols: Option<&[Protocol]>) -> Vec<String> {
        let speaks = |provider: &Provider| {
            protocols.is_none_or(|wanted| {
                provider
                    .endpoints
                    .iter()
                    .any(|endpoint| endpoint.enabled && wanted.contains(&endpoint.protocol))
            })
        };
        let mut names = Vec::new();
        let mut enabled_per_provider: HashMap<&str, usize> = HashMap::new();
        for model in self.models.iter().filter(|model| model.enabled) {
            *enabled_per_provider
                .entry(model.provider_id.as_str())
                .or_default() += 1;
        }
        for model in self.models.iter().filter(|model| model.enabled) {
            let Some(provider) = self
                .providers
                .get(&model.provider_id)
                .filter(|provider| provider.routable() && speaks(provider))
            else {
                continue;
            };
            let has_key = self.keys.get(&model.provider_id).is_some_and(|keys| {
                keys.iter().any(|key| {
                    key.key.enabled
                        && self.key_scopes.serves(&key.key, &model.model_id)
                        && protocols.is_none_or(|wanted| {
                            provider.endpoints.iter().any(|endpoint| {
                                endpoint.enabled
                                    && wanted.contains(&endpoint.protocol)
                                    && self.key_scopes.allows(
                                        &key.key,
                                        &model.model_id,
                                        endpoint.protocol,
                                    )
                            })
                        })
                })
            });
            let switch = provider
                .quirks
                .list_in_models
                .unwrap_or(enabled_per_provider[model.provider_id.as_str()] <= LIST_ALL_UP_TO);
            if has_key && switch {
                names.push(format!("{}/{}", provider.name, model.model_id));
            }
        }
        names
    }

    /// The provider `text` names, first hit wins: its current name, its id, an unexpired
    /// former name; all case-insensitive.
    fn provider_named(&self, text: &str) -> Option<&Provider> {
        let lower = text.to_lowercase();
        self.providers
            .values()
            .find(|provider| provider.name.trim().to_lowercase() == lower)
            .or_else(|| self.providers.get(text))
            .or_else(|| {
                self.providers
                    .values()
                    .find(|provider| provider.id.to_lowercase() == lower)
            })
            .or_else(|| {
                let (id, expires_at_ms) = self.former_names.get(&lower)?;
                (*expires_at_ms > wall_ms())
                    .then(|| self.providers.get(id))
                    .flatten()
            })
    }

    /// Up to three current provider names close to the provider part of `requested` (edit
    /// distance at most 2), for the answer to a name the gateway does not know.
    #[must_use]
    pub fn close_names(&self, requested: &str) -> Vec<String> {
        let Some((left, model)) = requested.trim().split_once('/') else {
            return Vec::new();
        };
        let left = left.trim().to_lowercase();
        let mut near: Vec<(usize, &str)> = self
            .providers
            .values()
            .map(|provider| {
                (
                    edit_distance(&left, &provider.name.trim().to_lowercase()),
                    provider.name.as_str(),
                )
            })
            .filter(|(distance, _)| *distance <= 2)
            .collect();
        near.sort_unstable();
        near.into_iter()
            .take(3)
            .map(|(_, name)| format!("{}/{model}", name.trim()))
            .collect()
    }

    /// Resolves a model reference to its stored id form: trimmed, split on the first `/`, the
    /// left side a provider name (case-insensitive) or else a provider id, the right side the
    /// model id verbatim (it may contain `/`). A string that is no reference is looked up in the
    /// legacy alias names. `None` when neither matches; whether the model is routable is
    /// [`RoutingSnapshot::plan`]'s business.
    #[must_use]
    pub fn resolve_ref(&self, text: &str) -> Option<ModelRef> {
        let text = text.trim();
        text.split_once('/')
            .and_then(|(provider, model)| {
                let provider = self.provider_named(provider.trim())?;
                (!model.is_empty()).then(|| ModelRef {
                    provider_id: provider.id.clone(),
                    model: model.to_owned(),
                })
            })
            .or_else(|| self.legacy_names.get(text).cloned())
    }

    /// The name of the provider `reference` resolves to when that provider came in by a sync
    /// import and waits for review: it is no default target or role model until then.
    #[must_use]
    pub fn awaiting_review(&self, reference: &str) -> Option<&str> {
        let model = self.resolve_ref(reference)?;
        self.providers
            .get(&model.provider_id)
            .filter(|provider| provider.review_pending_since_ms.is_some())
            .map(|provider| provider.name.as_str())
    }

    /// Whether a client scope admits `requested`: the name as sent, or its resolved
    /// `provider_id/model` (allowlists store the id form, so a name form or a legacy alias name
    /// passes too).
    #[must_use]
    pub fn allowed_by(&self, requested: &str, allows: &dyn Fn(&str) -> bool) -> bool {
        let requested = requested.trim();
        allows(requested)
            || self
                .resolve_ref(requested)
                .is_some_and(|model| allows(&model.id_form()))
    }

    /// The name form of a model (`<provider name>/<model>`); the id form when the provider is
    /// unknown.
    #[must_use]
    pub fn display_ref(&self, provider_id: &str, model: &str) -> String {
        let provider = self
            .providers
            .get(provider_id)
            .map_or(provider_id, |provider| provider.name.as_str());
        format!("{provider}/{model}")
    }

    /// What `provider_id`/`model` answers, for fallback compatibility (`None` for an unknown
    /// provider). Decision: the stored role says so, or the id names a decision model, or the
    /// provider speaks no chat protocol — and the provider has a decision endpoint (its first
    /// one is the protocol). Embedding: the stored role, a known `embeddings` capability, the
    /// catalogue `catalog_mode`, or the id (`model_kind`). Chat otherwise.
    #[must_use]
    pub fn call_kind(
        &self,
        provider_id: &str,
        model: &str,
        catalog_mode: Option<&str>,
    ) -> Option<CallKind> {
        let provider = self.providers.get(provider_id)?;
        let stored = self
            .models
            .iter()
            .find(|stored| stored.provider_id == provider_id && stored.model_id == model);
        let role = stored.and_then(|stored| stored.role);
        let spoken = provider
            .endpoints
            .iter()
            .filter(|endpoint| endpoint.enabled);
        let decision = spoken
            .clone()
            .map(|endpoint| endpoint.protocol)
            .find(|protocol| protocol.is_decision());
        let chat = spoken.clone().any(|endpoint| endpoint.protocol.is_chat());
        if let Some(protocol) = decision
            && (role == Some(ModelRole::Decision) || is_decision_model(model) || !chat)
        {
            return Some(CallKind::Decision(protocol));
        }
        let embeddings = stored
            .is_some_and(|stored| stored.capabilities.known(Capability::Embeddings) == Some(true));
        if role == Some(ModelRole::Embedding)
            || embeddings
            || model_kind(model, catalog_mode, None) == ModelKind::Embedding
        {
            return Some(CallKind::Embedding);
        }
        Some(CallKind::Chat)
    }

    /// Targets of one provider model for direct use: the given key, or every enabled key that may
    /// call the model, in configured order. `None` when the provider or key is missing or disabled.
    #[must_use]
    pub fn direct_targets(
        &self,
        provider_id: &str,
        key_id: Option<&str>,
        model: &str,
    ) -> Option<Vec<Target>> {
        let mut out = Vec::new();
        self.expand(provider_id, key_id, model, &mut out);
        (!out.is_empty()).then_some(out)
    }

    /// The one target for an AI run, probe or pinned chat: provider `provider_id`, `key_id` or
    /// else its first enabled key in the configured order that may call `model` (and use
    /// `protocol`), calling `model`. No health or capability filtering (the caller chose);
    /// `protocol` demands that the provider speaks it.
    ///
    /// # Errors
    /// [`AppError::NotFound`] when the provider is unknown or disabled, [`AppError::Validation`]
    /// when it has no enabled key (or not the wanted one) or its extra headers cannot be
    /// resolved, [`AppError::Unsupported`] when it does not speak `protocol`.
    pub fn target_for(
        &self,
        provider_id: &str,
        key_id: Option<&str>,
        model: &str,
        protocol: Option<Protocol>,
    ) -> Result<Target, AppError> {
        let provider = self
            .providers
            .get(provider_id)
            .filter(|provider| provider.routable())
            .ok_or_else(|| AppError::not_found(format!("provider {provider_id} is not enabled")))?;
        let quirks = self.quirks_of(provider).map_err(AppError::validation)?;
        let usable = |key: &&ResolvedKey| {
            key.key.enabled
                && match key_id {
                    Some(wanted) => key.key.id == wanted,
                    None => {
                        self.key_scopes.serves(&key.key, model)
                            && protocol.is_none_or(|protocol| {
                                self.key_scopes.allows(&key.key, model, protocol)
                            })
                    }
                }
        };
        let key = self
            .keys
            .get(provider_id)
            .and_then(|keys| keys.iter().find(usable))
            .ok_or_else(|| {
                AppError::validation(format!(
                    "provider {} has no enabled key that can call {model}",
                    provider.name
                ))
            })?;
        let target = Target::new(provider, &quirks, key, model, self);
        if let Some(protocol) = protocol {
            target.url(protocol, false)?;
        }
        Ok(target)
    }
}

#[cfg(test)]
#[path = "router_tests.rs"]
mod tests;
