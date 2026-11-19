//! Provider presets, providers, keys, models with their fallback chains and the model catalog
//! use-cases.

use std::collections::{BTreeMap, HashMap};

use serde::{Deserialize, Serialize};

use super::balance::llm_balance_refresh_if_due_impl;
use super::models::llm_models_refresh_impl;
use super::price_book::{EffectivePriceState, PriceBook};
use super::provider_draft::{self, DraftInput, Mounts, ProviderDraft};
use super::sniff_create::in_transaction;
use super::{
    check_id, ensure_catalog, next_provider_position, unique_provider_id, with_connection,
};
use crate::app::factory_service::new_entity_id;
use crate::app::{AppState, current_time_ms};
use crate::error::AppError;
use crate::llm::balance::RefreshTrigger;
use crate::llm::capabilities::{Capability, CapabilitySource};
use crate::llm::catalog::{self, CatalogEntry, CatalogIndex};
use crate::llm::events::LlmEvent;
use crate::llm::key_book::KeyState;
use crate::llm::presets::{self, PresetCatalog, PresetCategory, ProviderPreset};
use crate::llm::probe::listing::{ModelKind, model_kind, role_kind};
use crate::llm::router::KeyScopes;
use crate::llm::store::{
    self, FactSource, KeyRunState, KeyRuntime, ModelSource, ModelsListing, ModelsListingState,
    Provider, ProviderKey, ProviderModel,
};
use crate::llm::wire::{GatewayFamily, Protocol};

/// The text of a preset category as stored in `llm_providers.category`.
pub(super) fn category_text(category: PresetCategory) -> String {
    serde_json::to_value(category)
        .ok()
        .and_then(|value| value.as_str().map(str::to_owned))
        .unwrap_or_else(|| "official".to_owned())
}

// ------------------------------------------------------------------ presets

/// The bundled provider presets.
///
/// # Errors
/// [`AppError::Internal`] if the embedded data is invalid.
pub fn llm_presets_impl() -> Result<PresetCatalog, AppError> {
    presets::presets().cloned()
}

/// Input of [`llm_provider_create_from_preset_impl`].
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderFromPresetInput {
    /// Preset id (`openai`, `deepseek`, …).
    pub preset_id: String,
    /// Region to use; the preset's first region when absent.
    #[serde(default)]
    pub region_id: Option<String>,
    /// Values for `{placeholder}`s in the region's base URL (`resource`, `region`).
    #[serde(default)]
    pub placeholders: BTreeMap<String, String>,
    /// Display name; the preset's when absent.
    #[serde(default)]
    pub name: Option<String>,
    /// API key to store as the provider's first key.
    #[serde(default)]
    pub secret: Option<String>,
}

/// Prints `***` for the key, so a stray `{:?}` never leaks it.
impl std::fmt::Debug for ProviderFromPresetInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ProviderFromPresetInput")
            .field("preset_id", &self.preset_id)
            .field("region_id", &self.region_id)
            .field("placeholders", &self.placeholders)
            .field("name", &self.name)
            .field("secret", &self.secret.as_ref().map(|_| "***"))
            .finish()
    }
}

/// The draft of a preset direct-build: the region's base URL (placeholders filled), the preset's
/// auth, endpoints and quirks, the typed name and key.
fn preset_draft(
    preset: &ProviderPreset,
    input: &ProviderFromPresetInput,
    now: i64,
) -> Result<ProviderDraft, AppError> {
    let region = match input.region_id.as_deref() {
        Some(wanted) => preset
            .regions
            .iter()
            .find(|region| region.id == wanted)
            .ok_or_else(|| {
                AppError::validation(format!("{} has no region {wanted}", preset.name))
            })?,
        None => preset
            .regions
            .first()
            .ok_or_else(|| AppError::internal(format!("preset {} has no region", preset.id)))?,
    };
    let values: Vec<(&str, &str)> = input
        .placeholders
        .iter()
        .map(|(name, value)| (name.as_str(), value.as_str()))
        .collect();
    let base_url = presets::fill_placeholders(&region.base_url, &values)?;
    let mut draft = provider_draft::compose(DraftInput {
        preset: Some(preset),
        origin: &base_url,
        mounts: Mounts::Region(base_url.clone()),
        name: input.name.as_deref(),
        category: "official",
        models_endpoint: None,
        secret: input.secret.clone(),
        key_name: None,
        now_ms: now,
    })?;
    draft.id_base = Some(preset.id.clone());
    Ok(draft)
}

/// Creates a provider from a preset: base URL from the region, the preset's models, and an
/// optional first key.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown preset, [`AppError::Validation`] for an unknown region
/// or an unfilled placeholder.
pub fn llm_provider_create_from_preset_impl(
    state: &AppState,
    input: ProviderFromPresetInput,
) -> Result<Provider, AppError> {
    let catalog = presets::presets()?;
    let preset = catalog
        .provider(&input.preset_id)
        .ok_or_else(|| AppError::not_found(format!("preset {} not found", input.preset_id)))?;
    let now = current_time_ms();
    let mut draft = preset_draft(preset, &input, now)?;
    let provider = with_connection(state, |connection| {
        // One transaction: a failure after the provider row leaves nothing behind, so a retry
        // does not create `<preset>-2`.
        in_transaction(connection, |connection| {
            provider_draft::place(connection, &mut draft)?;
            let saved = provider_draft::save(connection, &draft)?;
            for model in &preset.models {
                store::save_model(
                    connection,
                    &ProviderModel {
                        provider_id: saved.id.clone(),
                        model_id: model.id.clone(),
                        source: ModelSource::Preset,
                        role: Some(model.role),
                        context: model.context,
                        max_output: model.max_output,
                        vision: model.vision,
                        tools: model.tools,
                        notes: model.notes.clone(),
                        enabled: true,
                        ..ProviderModel::default()
                    },
                )?;
            }
            if preset.category == PresetCategory::Official {
                store::set_gateway_family(connection, &saved.id, GatewayFamily::Official)?;
            }
            store::get_provider(connection, &saved.id)?
                .ok_or_else(|| AppError::internal("provider vanished after create"))
        })
    })?;
    state.llm.touch();
    state.sync.notify_changed();
    state.llm.emit(LlmEvent::ProvidersChanged);
    state.llm.emit(LlmEvent::KeysChanged {
        provider_id: provider.id.clone(),
    });
    state.llm.emit(LlmEvent::ModelsChanged {
        provider_id: provider.id.clone(),
    });
    Ok(provider)
}

// ---------------------------------------------------------------- providers

/// Every provider.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_provider_list_impl(state: &AppState) -> Result<Vec<Provider>, AppError> {
    with_connection(state, store::list_providers)
}

/// Creates (empty `id`: an id is made from the name) or updates a provider. The server-owned
/// fields (family, quota, evidence, drift, balance query) are kept as stored; extra header values
/// go to the secret store.
///
/// # Errors
/// [`AppError::Validation`] for an unusable provider.
pub fn llm_provider_save_impl(state: &AppState, provider: Provider) -> Result<Provider, AppError> {
    llm_provider_save_guarded_impl(state, provider, store::SaveProviderOptions::default())
}

/// [`llm_provider_save_impl`] refused with `stale_write` when the stored provider is not the
/// version the page read (`expected_updated_at_ms`), and with `host_change_needs_confirm`
/// when it moves to a new outbound host without `confirm_host_change`. A confirmed move
/// switches the provider's keys off.
///
/// # Errors
/// As [`llm_provider_save_impl`], plus [`AppError::Conflict`] for a stale version or an
/// unconfirmed host change.
pub fn llm_provider_save_guarded_impl(
    state: &AppState,
    mut provider: Provider,
    options: store::SaveProviderOptions,
) -> Result<Provider, AppError> {
    if !provider.id.trim().is_empty() {
        check_id("provider", &provider.id)?;
    }
    let now = current_time_ms();
    let saved = with_connection(state, |connection| {
        if provider.id.trim().is_empty() {
            let existing = store::list_providers(connection)?;
            let buried = store::tombstone_ids(connection)?;
            provider.id = unique_provider_id(&provider.name, &existing, &buried);
            provider.position = next_provider_position(&existing);
        }
        store::save_provider_with(connection, &provider, options, now)
    })?;
    state.llm.touch();
    state.sync.notify_changed();
    state.llm.emit(LlmEvent::ProvidersChanged);
    if options.confirm_host_change {
        emit_key_change(state, &saved.id);
    }
    Ok(saved)
}

/// Clears `ai.agent.default_target` when `names_it` says it points at what was just deleted (a
/// provider): a dangling default would fail every new chat. A failed settings write is logged,
/// never fatal: the deletion already happened.
fn clear_default_target(state: &AppState, names_it: impl Fn(&serde_json::Value) -> bool) {
    let current = serde_json::to_value(&state.settings().ai.agent.default_target)
        .unwrap_or(serde_json::Value::Null);
    if names_it(&current)
        && let Err(error) = crate::app::settings_update_impl(
            state,
            crate::settings::SettingsSection::Ai,
            &serde_json::json!({"agent": {"default_target": null}}),
        )
    {
        crate::logging::log_line(
            crate::logging::LogLevel::Warn,
            "llm.providers",
            &format!(
                "could not clear the agent default target: {}",
                error.message()
            ),
        );
    }
}

/// Drops the entries of `ai.agent.fallbacks` and the `ai.agent.role_models` that `gone` names
/// (a model of a deleted provider, or a deleted model), logged like [`clear_default_target`]
/// when the write fails.
fn drop_agent_fallbacks_to(state: &AppState, gone: impl Fn(&str) -> bool) {
    let agent = state.settings().ai.agent.clone();
    let mut chains = agent.fallbacks;
    let mut changed = false;
    for chain in chains.lists_mut() {
        let before = chain.len();
        chain.retain(|model| !gone(model));
        changed |= chain.len() != before;
    }
    let mut roles = serde_json::Map::new();
    for (role, model) in [
        ("fast", &agent.role_models.fast),
        ("deep", &agent.role_models.deep),
        ("vision", &agent.role_models.vision),
    ] {
        if model.as_deref().is_some_and(&gone) {
            roles.insert(role.to_owned(), serde_json::Value::Null);
        }
    }
    if !changed && roles.is_empty() {
        return;
    }
    let mut agent_patch = serde_json::Map::new();
    if changed {
        // Every list explicitly: a merge patch leaves out keys it does not name.
        agent_patch.insert(
            "fallbacks".to_owned(),
            serde_json::json!({
                "main": chains.main, "fast": chains.fast, "deep": chains.deep, "vision": chains.vision,
            }),
        );
    }
    if !roles.is_empty() {
        agent_patch.insert("role_models".to_owned(), serde_json::Value::Object(roles));
    }
    let patch = serde_json::json!({ "agent": agent_patch });
    if let Err(error) =
        crate::app::settings_update_impl(state, crate::settings::SettingsSection::Ai, &patch)
    {
        crate::logging::log_line(
            crate::logging::LogLevel::Warn,
            "llm.providers",
            &format!(
                "could not drop the deleted models from the agent fallbacks and roles: {}",
                error.message()
            ),
        );
    }
}

/// Deletes a provider and everything that hangs off it: its keys, models, fallback chains and
/// probe results, the secrets of the provider, its header and balance values and its keys, and
/// every fallback entry of another model that named it. An agent default on the provider
/// (pinned, or a `provider_id/model` reference) is cleared, and its models leave the
/// agent fallback chains.
///
/// # Errors
/// [`AppError::NotFound`] when it does not exist.
pub fn llm_provider_delete_impl(state: &AppState, id: &str) -> Result<(), AppError> {
    let purged = with_connection(state, |connection| {
        in_transaction(connection, |connection| {
            store::delete_provider(connection, id)
        })
    })?;
    let touched = purged.chains;
    let prefix = format!("{id}/");
    clear_default_target(state, |target| {
        (target["kind"] == "pinned" && target["provider_id"] == id)
            || (target["kind"] == "named"
                && target["name"]
                    .as_str()
                    .is_some_and(|name| name.starts_with(&prefix)))
    });
    drop_agent_fallbacks_to(state, |model| model.starts_with(&prefix));
    if !purged.clients.is_empty() {
        state.llm.emit(LlmEvent::ClientsChanged);
    }
    state.llm.touch();
    state.sync.notify_changed();
    state.llm.emit(LlmEvent::ProvidersChanged);
    let mut changed: Vec<String> = touched.into_iter().map(|model| model.provider_id).collect();
    changed.dedup();
    for provider_id in changed {
        state.llm.emit(LlmEvent::ModelsChanged { provider_id });
    }
    Ok(())
}

// --------------------------------------------------------------------- keys

/// Keys of a provider (all keys when `provider_id` is `None`), with this session's rotation state
/// (cooldown, last error, last use; a key not called this session shows the request log's newest
/// use).
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_key_list_impl(
    state: &AppState,
    provider_id: Option<&str>,
) -> Result<Vec<ProviderKey>, AppError> {
    let (mut keys, logged, listed) = with_connection(state, |connection| {
        Ok((
            store::list_keys(connection, provider_id)?,
            store::key_last_used(connection, state.llm.tz_offset_minutes())?,
            store::list_key_models(connection, provider_id)?,
        ))
    })?;
    let groups = group_keys(&listed);
    let circuits = state.llm.health.open_circuits(None);
    let listing = state
        .llm
        .services
        .key_listing
        .lock()
        .unwrap_or_else(std::sync::PoisonError::into_inner)
        .clone();
    for key in &mut keys {
        let live = state.llm.keys.state(&key.id);
        let circuit_until = circuits
            .iter()
            .filter(|circuit| circuit.key_id == key.id)
            .map(|circuit| circuit.open_until_ms)
            .min();
        key.runtime = Some(key_runtime(key.enabled, &live, circuit_until));
        key.cooldown_until_ms = resting_until(&live);
        key.last_error_status = live.last_error_status;
        key.last_error = live.last_error;
        key.last_error_ms = live.last_error_ms;
        key.last_used_ms = live.last_used_ms.or_else(|| logged.get(&key.id).copied());
        key.group_key = key.models_listed_at_ms.map(|_| {
            groups
                .get(&key.id)
                .cloned()
                .unwrap_or_else(|| group_hash(&[]))
        });
        key.models_listing = Some(models_listing(key, listing.get(&key.id)));
    }
    Ok(keys)
}

/// The group of a set of listed model ids: the first 16 hex digits of the SHA-256 of the
/// sorted ids, so keys that list the same models share it whatever order the relay sent.
fn group_hash(ids: &[&str]) -> String {
    let mut sorted = ids.to_vec();
    sorted.sort_unstable();
    sorted.dedup();
    let mut hex = pingdeck_base::crypto::sha256_hex(sorted.join("\n").as_bytes());
    hex.truncate(16);
    hex
}

/// Every key's group from the rows of [`store::list_key_models`] (disabled keys included, as
/// routing counts them).
fn group_keys(rows: &[store::KeyModel]) -> HashMap<String, String> {
    let mut by_key: HashMap<&str, Vec<&str>> = HashMap::new();
    for row in rows {
        by_key
            .entry(row.key_id.as_str())
            .or_default()
            .push(row.model_id.as_str());
    }
    by_key
        .into_iter()
        .map(|(key, ids)| (key.to_owned(), group_hash(&ids)))
        .collect()
}

/// Until when the key rests: its own cooldown, else the soonest end of a model's rate-limit rest
/// (routing skips the key for that model meanwhile, so the row shows it as cooling).
fn resting_until(live: &KeyState) -> Option<i64> {
    live.cooldown_until_ms
        .or_else(|| live.model_rest.as_ref().map(|(_, until)| *until))
}

/// A key's run state from the key book and the earliest end of its open circuits.
fn key_runtime(enabled: bool, live: &KeyState, circuit_until: Option<Option<i64>>) -> KeyRuntime {
    let (state, until_ms) = if !enabled {
        (KeyRunState::Disabled, None)
    } else if let Some(until) = resting_until(live) {
        (KeyRunState::Cooling, Some(until))
    } else if let Some(until) = circuit_until {
        (KeyRunState::CircuitOpen, until)
    } else {
        (KeyRunState::Ready, None)
    };
    KeyRuntime {
        state,
        kind: live.last_error_kind,
        scope: live.last_error_scope,
        until_ms,
    }
}

/// Where reading `key`'s own list stands, from the stored stamp and this session's reads.
fn models_listing(key: &ProviderKey, memo: Option<&Option<String>>) -> ModelsListing {
    let (state, failure_code) = match memo {
        Some(None) => (ModelsListingState::Reading, None),
        Some(Some(code)) => (
            ModelsListingState::Failed,
            Some(code.clone()).filter(|code| !code.is_empty()),
        ),
        None if key.models_listed_at_ms.is_some() => (ModelsListingState::Listed, None),
        None => (ModelsListingState::Unread, None),
    };
    ModelsListing {
        state,
        at_ms: key.models_listed_at_ms,
        failure_code,
    }
}

/// The plaintext secret of a key, for the user who asked to see it. Never logged.
///
/// # Errors
/// [`AppError::NotFound`] when the key does not exist or holds no secret.
pub fn llm_key_reveal_impl(state: &AppState, key_id: &str) -> Result<String, AppError> {
    with_connection(state, |connection| store::reveal_key(connection, key_id))
}

/// Ends a key's cooldown now, so the next request may use it.
pub fn llm_key_clear_cooldown_impl(state: &AppState, key_id: &str) {
    if state.llm.keys.clear_cooldown(key_id) {
        announce_key_runtime(state, key_id);
    }
}

/// Tells the UI the run state of `key_id`'s provider changed.
pub(crate) fn announce_key_runtime(state: &AppState, key_id: &str) {
    let provider = with_connection(state, |connection| store::get_key(connection, key_id))
        .ok()
        .flatten();
    if let Some(key) = provider {
        state.llm.emit(LlmEvent::KeysChanged {
            provider_id: key.provider_id,
        });
    }
}

/// Input of [`llm_key_save_impl`]. The secret and the management token are write-only: they go
/// to the secret store and the key shows only masked hints afterwards.
#[derive(Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SaveProviderKeyInput {
    /// Key id; empty creates a key.
    #[serde(default)]
    pub id: String,
    /// Owning provider.
    pub provider_id: String,
    /// Display name.
    pub name: String,
    /// New secret; `None` keeps the stored one, an empty string clears it.
    #[serde(default)]
    pub secret: Option<String>,
    /// Whether the key may be used.
    pub enabled: bool,
    /// Sort order within the provider.
    #[serde(default)]
    pub position: i64,
    /// The account id some gateways need beside the key (`New-Api-User`); `None` keeps the stored
    /// one, an empty string clears it.
    #[serde(default)]
    pub api_user_id: Option<String>,
    /// New management (console) token; `None` keeps the stored one, an empty string clears it.
    #[serde(default)]
    pub management_token: Option<String>,
    /// Free-form notes (at most 200 characters); `None` keeps the stored ones.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub notes: Option<String>,
    /// The relay group the key belongs to (display only, at most 60 characters); `None` keeps
    /// the stored one, an empty string clears it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub group_name: Option<String>,
    /// The protocols the key may use ("this key only speaks Responses"); `None` keeps the stored
    /// limit, an empty list removes it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub protocols: Option<Vec<Protocol>>,
}

/// Prints `***` for the secrets, so a stray `{:?}` never leaks them.
impl std::fmt::Debug for SaveProviderKeyInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("SaveProviderKeyInput")
            .field("id", &self.id)
            .field("provider_id", &self.provider_id)
            .field("name", &self.name)
            .field("secret", &self.secret.as_ref().map(|_| "***"))
            .field("enabled", &self.enabled)
            .field("position", &self.position)
            .field("api_user_id", &self.api_user_id)
            .field(
                "management_token",
                &self.management_token.as_ref().map(|_| "***"),
            )
            .field("notes", &self.notes)
            .field("group_name", &self.group_name)
            .field("protocols", &self.protocols)
            .finish()
    }
}

/// Creates (empty `id`) or updates a key; the secret and the management token go to the secret
/// store in the same transaction.
///
/// # Errors
/// [`AppError::NotFound`] when the provider does not exist.
pub fn llm_key_save_impl(
    state: &AppState,
    input: SaveProviderKeyInput,
) -> Result<ProviderKey, AppError> {
    llm_key_save_guarded_impl(state, input, None)
}

/// [`llm_key_save_impl`] refused with `stale_write` when the stored key is not the version the
/// page read (`expected_updated_at_ms`).
///
/// # Errors
/// As [`llm_key_save_impl`], plus [`AppError::Conflict`] for a stale version.
pub fn llm_key_save_guarded_impl(
    state: &AppState,
    input: SaveProviderKeyInput,
    expected_updated_at_ms: Option<i64>,
) -> Result<ProviderKey, AppError> {
    let now = current_time_ms();
    let creating = input.id.trim().is_empty();
    let id = if creating {
        new_entity_id("key")
    } else {
        input.id.clone()
    };
    check_id("key", &id)?;
    let mut repeated = false;
    let mut was_enabled: Option<bool> = None;
    let saved = with_connection(state, |connection| {
        // A repeated "Add" of the same secret (a double click) creates one key.
        if creating
            && let Some(secret) = input.secret.as_deref().map(str::trim)
            && !secret.is_empty()
            && let Some(held) =
                store::list_resolved_keys(connection)?
                    .into_iter()
                    .find(|resolved| {
                        resolved.key.provider_id == input.provider_id
                            && resolved.secret.trim() == secret
                    })
        {
            repeated = true;
            return Ok(held.key);
        }
        in_transaction(connection, |connection| {
            let stored = store::get_key(connection, &id)?;
            was_enabled = stored.as_ref().map(|stored| stored.enabled);
            let api_user_id = match input.api_user_id.clone() {
                Some(account) => Some(account),
                None => stored.and_then(|stored| stored.api_user_id),
            };
            let key = ProviderKey {
                id: id.clone(),
                provider_id: input.provider_id.clone(),
                name: input.name.clone(),
                enabled: input.enabled,
                position: input.position,
                created_at_ms: now,
                updated_at_ms: now,
                api_user_id,
                notes: input.notes.clone(),
                group_name: input.group_name.clone(),
                protocols: input.protocols.clone(),
                ..ProviderKey::default()
            };
            store::save_key_guarded(
                connection,
                &key,
                input.secret.as_deref(),
                expected_updated_at_ms,
                now,
            )?;
            if let Some(token) = input.management_token.as_deref() {
                store::set_key_management_token(connection, &id, Some(token), now)?;
            }
            store::get_key(connection, &id)?
                .ok_or_else(|| AppError::internal("key vanished after save"))
        })
    })?;
    if repeated {
        return Ok(saved);
    }
    state.llm.touch();
    state.sync.notify_changed();
    emit_key_change(state, &saved.provider_id);
    let replaced = input
        .secret
        .as_deref()
        .is_some_and(|s| !s.trim().is_empty());
    // A new, switched or re-valued key changes which models the provider can serve.
    if replaced || was_enabled != Some(saved.enabled) {
        state.llm.emit(LlmEvent::ModelsChanged {
            provider_id: saved.provider_id.clone(),
        });
    }
    if replaced {
        // Another value is another credential: what the old one earned does not apply.
        state.llm.keys.clear_cooldown(&saved.id);
        queue_balance_refresh(state, &saved.provider_id);
        // A new key of a relay may sit in another group than the others: until its own list is
        // read, routing gives it only the models no key lists (D-258), so the read is queued
        // here rather than left to the page.
        if was_enabled.is_some() || lists_models(state, &saved.provider_id) {
            queue_key_list_refresh(state, &saved.provider_id, &saved.id);
        }
    }
    Ok(saved)
}

/// Whether the provider has a model-list endpoint (a read of a key's own list can work).
fn lists_models(state: &AppState, provider_id: &str) -> bool {
    with_connection(state, |connection| {
        store::get_provider(connection, provider_id)
    })
    .ok()
    .flatten()
    .is_some_and(|provider| provider.models_endpoint.is_some())
}

/// Reads the model list of a key that was just added or whose value was just replaced: the
/// value may belong to another relay group, and the stored list of the old one would keep
/// routing to models the key no longer serves. Queued after the save on the ambient runtime (none, none
/// queued); a provider without a list endpoint or a failing read leaves the stored list as is.
fn queue_key_list_refresh(state: &AppState, provider_id: &str, key_id: &str) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    super::models::mark_listing(state, key_id, true);
    let (state, provider_id, key_id) = (state.clone(), provider_id.to_owned(), key_id.to_owned());
    runtime.spawn(async move {
        let _ = llm_models_refresh_impl(&state, &provider_id, Some(&key_id), None).await;
        super::models::mark_listing(&state, &key_id, false);
        emit_key_change(&state, &provider_id);
    });
}

/// Refreshes the balance of a provider whose key secret was just replaced (the new key
/// may belong to another account) or that a scan just created with keys. Fire and forget
/// on the ambient runtime (none, none queued), on the background token so shutdown stops it; a
/// failing request is the key's stored error record, never the save's.
pub(crate) fn queue_balance_refresh(state: &AppState, provider_id: &str) {
    let Ok(runtime) = tokio::runtime::Handle::try_current() else {
        return;
    };
    let cancel = super::balance::background_token(state);
    let (state, provider_id) = (state.clone(), provider_id.to_owned());
    runtime.spawn(async move {
        let _ = llm_balance_refresh_if_due_impl(
            &state,
            &provider_id,
            RefreshTrigger::AfterAdd,
            &cancel,
        )
        .await;
    });
}

/// Tells the UI a provider's keys changed (and with them its derived status).
fn emit_key_change(state: &AppState, provider_id: &str) {
    state.llm.emit(LlmEvent::KeysChanged {
        provider_id: provider_id.to_owned(),
    });
    state.llm.emit(LlmEvent::ProviderStatusChanged {
        provider_id: provider_id.to_owned(),
    });
}

/// Deletes a key together with its secrets.
///
/// # Errors
/// [`AppError::NotFound`] when it does not exist.
pub fn llm_key_delete_impl(state: &AppState, id: &str) -> Result<(), AppError> {
    let provider_id = with_connection(state, |connection| {
        let key = store::get_key(connection, id)?
            .ok_or_else(|| AppError::not_found(format!("key {id} not found")))?;
        store::delete_key(connection, id)?;
        Ok(key.provider_id)
    })?;
    state.llm.touch();
    state.sync.notify_changed();
    emit_key_change(state, &provider_id);
    state.llm.emit(LlmEvent::ModelsChanged { provider_id });
    Ok(())
}

// ------------------------------------------------------------------- models

/// Models of a provider (all when `provider_id` is `None`).
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_model_list_impl(
    state: &AppState,
    provider_id: Option<&str>,
) -> Result<Vec<ProviderModel>, AppError> {
    with_connection(state, |connection| {
        store::list_models(connection, provider_id)
    })
}

/// A stored model with the kind the Providers UI tags it with (`ProviderModelDto`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderModelView {
    /// The stored model; its fields stay at the top level of the JSON.
    #[serde(flatten)]
    pub model: ProviderModel,
    /// `chat`, `embedding`, `image`, `audio` or `other` ([`model_kind`], rerank and moderation
    /// folded into `other`).
    pub kind: ModelKind,
    /// The input modalities the model supports, in canonical order, each with the strongest
    /// evidence; left out when nothing is known.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub inputs: Vec<ModelInput>,
    /// The catalogue's context window, only while the model stores none of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_context: Option<u64>,
    /// The catalogue's maximum output tokens (models.dev `limit.output`), only while the model
    /// stores no `max_output` of its own.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_output: Option<u64>,
    /// The enabled keys that can call the model, when not every enabled key can (relay groups,
    /// D-258); empty when none can.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_ids: Option<Vec<String>>,
    /// What the model costs here, in three states (C-$1); read-only, set by the list.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub effective_price: Option<EffectivePriceState>,
}

/// An input a model accepts (`ModelInputDto`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelInput {
    /// The modality.
    pub modality: InputModality,
    /// Who says the model accepts it.
    pub source: CapabilitySource,
}

/// A kind of model input (`InputModalityDto`), in canonical order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum InputModality {
    /// Text.
    Text,
    /// Images.
    Image,
    /// Audio.
    Audio,
    /// Video.
    Video,
    /// PDF documents.
    Pdf,
}

impl ProviderModelView {
    /// The view of `model`: its kind from the catalogue `mode` of its id, then the id patterns,
    /// exactly as the probe planner decides (the provider's own declaration is not stored); its
    /// inputs from the capability matrix (when the bit is known) and else the catalogue row; its
    /// reasoning and tool-call bits, when the matrix has no opinion, from the catalogue's
    /// positive flags at catalogue strength (as a sniffed model stores them).
    pub(super) fn new(mut model: ProviderModel, catalog: &CatalogIndex) -> Self {
        let record = catalog.lookup_record(&model.model_id);
        let mode = record.map(|record| record.kind.as_str());
        let kind = role_kind(model.role)
            .unwrap_or_else(|| model_kind(&model.model_id, mode, None))
            .coarse();
        let listed: &[String] = record.map_or(&[], |record| &record.input_modalities);
        let in_catalog = |word: &str| {
            listed
                .iter()
                .any(|m| m == word)
                .then_some(CapabilitySource::Catalog)
        };
        let caps = &model.capabilities;
        // The matrix bit when known (only if supported), else the catalogue.
        let bit = |capability: Capability, word: &str| {
            let state = caps.get(capability);
            if state.source == CapabilitySource::Unknown {
                in_catalog(word)
            } else {
                state.supported.then_some(state.source)
            }
        };
        let text = in_catalog("text").or_else(|| {
            Capability::ALL
                .iter()
                .map(|capability| caps.get(*capability).source)
                .filter(|source| *source >= CapabilitySource::Declared)
                .max()
        });
        let inputs = [
            (InputModality::Text, text),
            (InputModality::Image, bit(Capability::Vision, "image")),
            (InputModality::Audio, bit(Capability::AudioIn, "audio")),
            (InputModality::Video, in_catalog("video")),
            (InputModality::Pdf, bit(Capability::Document, "pdf")),
        ]
        .into_iter()
        .filter_map(|(modality, source)| source.map(|source| ModelInput { modality, source }))
        .collect();
        let catalog_context = record
            .and_then(|record| record.context_tokens)
            .filter(|_| model.context.is_none());
        let catalog_output = record
            .and_then(|record| record.max_output_tokens)
            .filter(|_| model.max_output.is_none());
        if catalog_context.is_some() {
            model.context_source = Some(FactSource::Catalog);
        }
        if catalog_output.is_some() {
            model.max_output_source = Some(FactSource::Catalog);
        }
        if let Some(record) = record {
            for (capability, flag) in [
                (Capability::Reasoning, record.reasoning),
                (Capability::Tools, record.tools),
            ] {
                if flag && model.capabilities.get(capability).source == CapabilitySource::Unknown {
                    model
                        .capabilities
                        .apply(capability, true, CapabilitySource::Catalog);
                }
            }
        }
        Self {
            model,
            kind,
            inputs,
            catalog_context,
            catalog_output,
            key_ids: None,
            effective_price: None,
        }
    }
}

/// [`llm_model_list_impl`] as [`ProviderModelView`]s.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_model_list_view_impl(
    state: &AppState,
    provider_id: Option<&str>,
) -> Result<Vec<ProviderModelView>, AppError> {
    with_connection(state, |connection| {
        ensure_catalog(connection)?;
        let models = store::list_models(connection, provider_id)?;
        if models.is_empty() {
            return Ok(Vec::new());
        }
        // One price book per list (C-$1), not one price lookup per row; reuses its preloaded
        // CatalogIndex.
        let book = PriceBook::load(connection)?;
        let index = book.catalog_index();
        let keys = store::list_keys(connection, provider_id)?;
        let owners = keys
            .iter()
            .map(|key| (key.id.clone(), key.provider_id.clone()))
            .collect();
        let scopes = KeyScopes::new(store::list_key_models(connection, provider_id)?, &owners)
            .with_confirmed(&models);
        Ok(models
            .into_iter()
            .map(|model| {
                let enabled = keys
                    .iter()
                    .filter(|key| key.enabled && key.provider_id == model.provider_id);
                let key_ids = scopes.listing_keys(enabled, &model.model_id);
                let effective_price = Some(book.price_state(&model.provider_id, &model.model_id));
                ProviderModelView {
                    key_ids,
                    effective_price,
                    ..ProviderModelView::new(model, index)
                }
            })
            .collect())
    })
}

/// Creates or updates a model entry.
///
/// # Errors
/// [`AppError::Validation`] for a blank id.
pub fn llm_model_save_impl(state: &AppState, model: ProviderModel) -> Result<(), AppError> {
    with_connection(state, |connection| store::save_model(connection, &model))?;
    state.llm.touch();
    state.sync.notify_changed();
    state.llm.emit(LlmEvent::ModelsChanged {
        provider_id: model.provider_id,
    });
    Ok(())
}

/// Deletes a model entry, with the prices, rules and allow-list entries that named it.
///
/// # Errors
/// [`AppError::NotFound`] when it does not exist.
pub fn llm_model_delete_impl(
    state: &AppState,
    provider_id: &str,
    model_id: &str,
) -> Result<(), AppError> {
    let purged = with_connection(state, |connection| {
        in_transaction(connection, |connection| {
            store::delete_model(connection, provider_id, model_id)?;
            store::purge_references(connection, provider_id, Some(model_id))
        })
    })?;
    let gone = format!("{provider_id}/{model_id}");
    drop_agent_fallbacks_to(state, |model| model == gone);
    if !purged.clients.is_empty() {
        state.llm.emit(LlmEvent::ClientsChanged);
    }
    for primary in purged
        .chains
        .iter()
        .filter(|m| m.provider_id != provider_id)
    {
        state.llm.emit(LlmEvent::ModelsChanged {
            provider_id: primary.provider_id.clone(),
        });
    }
    state.llm.touch();
    state.sync.notify_changed();
    state.llm.emit(LlmEvent::ModelsChanged {
        provider_id: provider_id.to_owned(),
    });
    Ok(())
}

// ------------------------------------------------------------------ catalog

/// Catalog entries whose id contains `query`.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_catalog_search_impl(
    state: &AppState,
    query: &str,
    limit: usize,
) -> Result<Vec<CatalogEntry>, AppError> {
    with_connection(state, |connection| {
        ensure_catalog(connection)?;
        catalog::search(connection, query, limit.clamp(1, 200))
    })
}

/// The catalog entry that best matches `model_id`.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_catalog_lookup_impl(
    state: &AppState,
    model_id: &str,
) -> Result<Option<CatalogEntry>, AppError> {
    with_connection(state, |connection| {
        ensure_catalog(connection)?;
        catalog::lookup(connection, model_id)
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::catalog::{ModelRecord, apply_sync};

    fn index(modalities: &[&str], context: Option<u64>) -> CatalogIndex {
        let connection = crate::storage::migrated_memory_connection();
        let record = ModelRecord {
            family: "fam".into(),
            model_id: "m".into(),
            name: "m".into(),
            context_tokens: context,
            max_output_tokens: context.map(|_| 16_000),
            input_modalities: modalities.iter().map(|m| (*m).to_owned()).collect(),
            reasoning: true,
            tools: true,
            currency: "USD".into(),
            ..ModelRecord::default()
        };
        apply_sync(&connection, &[record], 1, 0).unwrap();
        CatalogIndex::load(&connection).unwrap()
    }

    fn model(id: &str) -> ProviderModel {
        ProviderModel {
            provider_id: "p".into(),
            model_id: id.into(),
            ..ProviderModel::default()
        }
    }

    fn inputs(view: &ProviderModelView) -> Vec<(InputModality, CapabilitySource)> {
        view.inputs.iter().map(|i| (i.modality, i.source)).collect()
    }

    #[test]
    fn inputs_take_the_matrix_over_the_catalogue_and_video_from_the_catalogue_only() {
        let catalog = index(&["text", "image", "video", "pdf"], Some(128_000));
        let mut stored = model("m");
        stored
            .capabilities
            .apply(Capability::Vision, false, CapabilitySource::Measured);
        stored
            .capabilities
            .apply(Capability::AudioIn, true, CapabilitySource::Declared);
        stored
            .capabilities
            .apply(Capability::Tools, false, CapabilitySource::Measured);
        let view = ProviderModelView::new(stored, &catalog);
        assert_eq!(
            inputs(&view),
            [
                (InputModality::Text, CapabilitySource::Catalog),
                (InputModality::Audio, CapabilitySource::Declared),
                (InputModality::Video, CapabilitySource::Catalog),
                (InputModality::Pdf, CapabilitySource::Catalog),
            ],
            "a measured no image beats the catalogue's image"
        );
        assert_eq!(view.catalog_context, Some(128_000));
        assert_eq!(view.catalog_output, Some(16_000));
        let caps = &view.model.capabilities;
        assert_eq!(
            (
                caps.get(Capability::Reasoning).source,
                caps.known(Capability::Reasoning)
            ),
            (CapabilitySource::Catalog, Some(true)),
            "no opinion on reasoning takes the catalogue's flag"
        );
        assert_eq!(
            caps.get(Capability::Tools).source,
            CapabilitySource::Measured,
            "a measured no tool call beats the catalogue's tool_call"
        );

        let mut own_limits = model("m");
        own_limits.context = Some(8_000);
        own_limits.max_output = Some(4_000);
        let own = ProviderModelView::new(own_limits, &catalog);
        assert_eq!(
            (own.catalog_context, own.catalog_output),
            (None, None),
            "the model's own limits win"
        );
    }

    #[test]
    fn inputs_are_empty_when_nothing_is_known_and_text_follows_solid_evidence() {
        let catalog = index(&[], None);
        let view = ProviderModelView::new(model("unknown-id"), &catalog);
        assert!(view.inputs.is_empty() && view.catalog_context.is_none());
        assert!(
            serde_json::to_value(&view).unwrap().get("inputs").is_none(),
            "left out of the JSON"
        );
        let mut measured = model("unknown-id");
        measured
            .capabilities
            .apply(Capability::Tools, true, CapabilitySource::Measured);
        assert_eq!(
            inputs(&ProviderModelView::new(measured, &catalog)),
            [(InputModality::Text, CapabilitySource::Measured)]
        );
    }
}
