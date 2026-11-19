//! Global settings use-cases: read, patch, reset, the search keys the settings point at,
//! subscribe.
//!
//! Single responsibility: tie the in-memory [`SettingsHub`](crate::settings::SettingsHub) to the
//! `app_settings` rows and the `ai.search.*` secrets. An update validates first, persists second and
//! only then swaps the snapshot and notifies subscribers, so a failed write never leaves readers
//! on unsaved values. The search keys live in the secret store; the settings keep only masked
//! hints, which [`settings_secret_set_impl`] (and nothing else) writes.

use std::sync::atomic::{AtomicU64, Ordering};
use std::sync::{Arc, Mutex, PoisonError};
use std::time::{Duration, Instant};

use serde_json::Value;

use crate::error::AppError;
use crate::events::EventSink;
use crate::mock::MockServerManager;
use crate::proxy_url;
use crate::secrets::{SEARCH_BRAVE, SEARCH_TAVILY, SecretHint};
use crate::settings::{
    SETTINGS_VERSION, Settings, SettingsEvent, SettingsHub, SettingsSection, put_path, remove_path,
};
use crate::workspace::{StorageError, WorkspaceStore};
use pingdeck_model::StoredSetting;

use super::{AppState, command_error, current_time_ms};

pub use pingdeck_model::settings::policy::{SettingsSecret, local_only_fields};

mod role_checks;
pub use role_checks::{load_routing_facts, role_kind_of};

#[cfg(test)]
mod registry_tests;

/// Reads the stored settings, applies the values the store itself needs and returns the hub.
/// Called once while the state is built.
pub(super) fn load_settings(store: &mut WorkspaceStore) -> Result<SettingsHub, StorageError> {
    let mut settings = Settings::from_rows(&store.list_app_settings()?);
    // The stored row holds the proxy URL masked; the password comes from the secret store.
    let url = settings.network.proxy_url.clone();
    if proxy_url::split(&url).is_masked
        && let Some(password) = store.secret_get(proxy_url::SETTINGS_PASSWORD_SECRET)?
    {
        settings.network.proxy_url = proxy_url::restore(&url, &password);
    }
    apply_to_store(store, &settings);
    Ok(SettingsHub::new(settings))
}

/// Pushes the settings the store owns (history retention) into it.
fn apply_to_store(store: &mut WorkspaceStore, settings: &Settings) {
    store.set_history_retention(settings.data.history_retention as usize);
}

/// Pushes the settings the mock server manager owns (log capacity, extra allowed hosts) into it.
pub(super) fn apply_to_mocks(mocks: &MockServerManager, settings: &Settings) {
    mocks.set_log_capacity(settings.mock.log_capacity as usize);
    mocks.set_extra_allowed_hosts(settings.mock.extra_allowed_hosts.clone());
}

fn stored_section(section: SettingsSection, settings: &Settings) -> StoredSetting {
    StoredSetting {
        section: section.as_str().to_owned(),
        version: SETTINGS_VERSION,
        value_json: settings.section_value(section).to_string(),
    }
}

const NO_PROXY_PASSWORD: &str =
    "network.proxy_url has no stored password: type the proxy URL again";

/// Joins the proxy password with `next.network.proxy_url` after a change: a URL with a typed
/// password stores it, one that carries the mask (the page sent back what it was shown) gets the
/// stored password, one without a password clears it. The secret store is called with no lock
/// held.
fn settle_proxy_password(state: &AppState, next: &mut Settings) -> Result<(), AppError> {
    if next.network.proxy_url == state.settings().network.proxy_url {
        return Ok(());
    }
    let url = next.network.proxy_url.clone();
    let split = proxy_url::split(&url);
    if let Some(password) = split.password {
        state
            .secrets()
            .put(proxy_url::SETTINGS_PASSWORD_SECRET, &password)?;
    } else if split.is_masked {
        let stored = state.secrets().get(proxy_url::SETTINGS_PASSWORD_SECRET)?;
        let password = match stored {
            Some(password) => password,
            // The Undo of a row, section or page restore sends back the masked URL it was shown,
            // after the reset deleted the password: the same URL gets it back within the window.
            None => {
                let password = state
                    .settings_undo
                    .take_cleared_proxy(&url)
                    .ok_or_else(|| AppError::validation(NO_PROXY_PASSWORD))?;
                state
                    .secrets()
                    .put(proxy_url::SETTINGS_PASSWORD_SECRET, &password)?;
                password
            }
        };
        next.network.proxy_url = proxy_url::restore(&url, &password);
    } else {
        let cleared = proxy_url::split(&state.settings().network.proxy_url);
        state
            .secrets()
            .delete(proxy_url::SETTINGS_PASSWORD_SECRET)?;
        if let Some(password) = cleared.password {
            state
                .settings_undo
                .keep_cleared_proxy(cleared.masked, password);
        }
    }
    Ok(())
}

/// How long the Undo of a reset-all stays available.
const RESET_UNDO_WINDOW: Duration = Duration::from_secs(60);

/// The settings before the last reset-all, kept in memory only, plus a counter of every settings
/// write (updates, resets, secret hints, sync imports) so an Undo can tell whether anything wrote
/// after the reset.
#[derive(Default)]
pub(crate) struct ResetUndoStore {
    writes: AtomicU64,
    last: Mutex<Option<ResetSnapshot>>,
    /// The proxy password the last write without one deleted, for the Undo of that write.
    cleared_proxy: Mutex<Option<ClearedProxy>>,
}

struct ResetSnapshot {
    before: Settings,
    /// The write count right after the reset.
    writes: u64,
    at: Instant,
}

struct ClearedProxy {
    /// The URL as the page was shown it (password masked): only this URL gets the password back.
    masked: String,
    password: String,
    at: Instant,
}

impl ResetUndoStore {
    fn keep_cleared_proxy(&self, masked: String, password: String) {
        *self
            .cleared_proxy
            .lock()
            .unwrap_or_else(PoisonError::into_inner) = Some(ClearedProxy {
            masked,
            password,
            at: Instant::now(),
        });
    }

    /// The password cleared from `masked` within the Undo window; it is handed out once.
    fn take_cleared_proxy(&self, masked: &str) -> Option<String> {
        let cleared = self
            .cleared_proxy
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .take()?;
        (cleared.masked == masked && cleared.at.elapsed() <= RESET_UNDO_WINDOW)
            .then_some(cleared.password)
    }
}

/// Stores `next` as the new state of every section in `sections` in one transaction, then
/// publishes it.
fn commit_many(
    state: &AppState,
    mut next: Settings,
    sections: &[SettingsSection],
) -> Result<Settings, AppError> {
    settle_proxy_password(state, &mut next)?;
    {
        let mut store = state.store();
        let rows: Vec<StoredSetting> = sections
            .iter()
            .map(|&section| stored_section(section, &next))
            .collect();
        store
            .save_app_settings(&rows, current_time_ms())
            .map_err(command_error)?;
        apply_to_store(&mut store, &next);
    }
    apply_to_mocks(&state.mocks, &next);
    state.settings_undo.writes.fetch_add(1, Ordering::SeqCst);
    let mut published = next.clone();
    for &section in sections {
        published = (*state.settings.publish(section, next.clone())).clone();
    }
    if sections != [SettingsSection::Sync] {
        state.sync.notify_changed();
    }
    Ok(published)
}

/// Stores `next` as the new state of `section`, then publishes it.
fn commit(
    state: &AppState,
    section: SettingsSection,
    next: Settings,
) -> Result<Settings, AppError> {
    commit_many(state, next, &[section])
}

/// The current settings of every section.
#[must_use]
pub fn settings_get_impl(state: &AppState) -> Settings {
    (*state.settings()).clone()
}

/// Merge-patches one section (RFC 7396: nested objects merge, `null` removes a key so it reads
/// as its default) and returns all settings. Fails with a validation error naming the field,
/// leaving the stored settings untouched.
pub fn settings_update_impl(
    state: &AppState,
    section: SettingsSection,
    patch: &Value,
) -> Result<Settings, AppError> {
    let _updating = state.settings.lock_updates();
    let next = state.settings().patched(section, patch)?;
    if section == SettingsSection::Ai {
        // Cross-checks settings validation cannot make: LAN serving needs a token and a client;
        // a sub-agent role's new model must exist and be able to do the role's job; a changed
        // agent fallback chain must list existing chat models.
        crate::llm::gateway::GatewayManager::validate_lan(&state.llm, &next.ai.gateway)?;
        let current = state.settings();
        role_checks::check_role_models(
            state,
            &current.ai.agent.role_models,
            &next.ai.agent.role_models,
        )?;
        role_checks::check_agent_fallbacks(
            state,
            &current.ai.agent.fallbacks,
            &next.ai.agent.fallbacks,
        )?;
    }
    commit(state, section, next)
}

/// Replaces one section with `value` (fields it lacks take their defaults), validated like an
/// update; used by config sync, where the file is the whole section.
pub fn settings_replace_section_impl(
    state: &AppState,
    section: SettingsSection,
    value: &Value,
) -> Result<Settings, AppError> {
    let _updating = state.settings.lock_updates();
    let current = state.settings();
    let mut value = value.clone();
    keep_local_fields(&current, section, &mut value);
    let next = current.with_section_value(section, &value)?;
    if section == SettingsSection::Ai {
        // Cross-checks settings validation cannot make: LAN serving needs a token and a client.
        crate::llm::gateway::GatewayManager::validate_lan(&state.llm, &next.ai.gateway)?;
    }
    commit(state, section, next)
}

/// Longest secret value accepted, in bytes.
const MAX_SECRET_BYTES: usize = 4096;

/// Stores `value` as `secret` (an empty value deletes it), updates the matching key hint through
/// the normal commit path so subscribers hear about it, and returns the stored hint (empty
/// `has_secret: false` after a delete).
///
/// # Errors
/// A validation error for a value that is too long or holds control characters.
pub fn settings_secret_set_impl(
    state: &AppState,
    secret: SettingsSecret,
    value: &str,
) -> Result<SecretHint, AppError> {
    let value = value.trim();
    if value.len() > MAX_SECRET_BYTES || value.chars().any(char::is_control) {
        return Err(AppError::validation(
            "a secret must be at most 4096 bytes without control characters",
        ));
    }
    let _updating = state.settings.lock_updates();
    let secret_ref = match secret {
        SettingsSecret::SearchTavily => SEARCH_TAVILY,
        SettingsSecret::SearchBrave => SEARCH_BRAVE,
    };
    let hint = if value.is_empty() {
        state.secrets().delete(secret_ref)?;
        SecretHint {
            secret_ref: secret_ref.to_owned(),
            has_secret: false,
            hint: String::new(),
            updated_at_ms: 0,
        }
    } else {
        state.secrets().put(secret_ref, value)?
    };
    let mut next = (*state.settings()).clone();
    match secret {
        SettingsSecret::SearchTavily => next.ai.search.tavily_key_hint = hint.hint.clone(),
        SettingsSecret::SearchBrave => next.ai.search.brave_key_hint = hint.hint.clone(),
    }
    commit(state, SettingsSection::Ai, next)?;
    Ok(hint)
}

/// Pushes every later change to `sink`; returns the id for [`settings_unsubscribe_impl`].
pub fn settings_subscribe_impl(
    state: &AppState,
    sink: Arc<dyn EventSink<SettingsEvent>>,
) -> Result<u64, AppError> {
    state.settings.subscribe(sink)
}

/// Ends a settings subscription; returns whether it was still active.
pub fn settings_unsubscribe_impl(state: &AppState, subscription_id: u64) -> bool {
    state.settings.unsubscribe(subscription_id)
}

pub fn keep_local_fields(local: &Settings, section: SettingsSection, value: &mut Value) {
    let mine = local.section_value(section);
    if let Some(Value::Object(gateway)) = value.pointer_mut("/gateway") {
        // An older file names the LAN switch `bind_lan`; it must not survive next to `serve_lan`.
        gateway.remove("bind_lan");
    }
    // Settings an earlier build wrote and none reads now: dropped, not refused.
    if let Some(Value::Object(agent)) = value.pointer_mut("/agent") {
        agent.remove("tool_approval");
        agent.remove("shell_output_cap_kib");
    }
    for path in local_only_fields(section) {
        match mine.pointer(&format!("/{}", path.replace('.', "/"))) {
            Some(own) => put_path(value, path, own),
            // At its default the local field is not stored: the incoming value goes too, so the
            // field stays at the default instead of taking the file's.
            None => remove_path(value, path),
        }
    }
}

/// Puts one section back to its defaults, or only the dotted `keys` of it (relative to the
/// section, e.g. `gateway.port`) when given, and returns all settings. The `ai` section keeps the
/// stored search keys (and so their hints): only settings fields reset.
pub fn settings_reset_section_impl(
    state: &AppState,
    section: SettingsSection,
    keys: Option<Vec<String>>,
) -> Result<Settings, AppError> {
    let _updating = state.settings.lock_updates();
    let next = match keys {
        None => state.settings().reset(section)?,
        Some(keys) => state.settings().reset_keys(section, &keys)?,
    };
    commit(state, section, next)
}

/// Puts every section back to its defaults in one write and returns all settings. Like a section
/// reset it keeps the stored search keys, and it touches no record, secret, chat or history:
/// the retention limits (whose lowering deletes data) and the sync folder binding stay.
/// The settings before it stay in memory for [`settings_undo_last_reset_impl`].
pub fn settings_reset_all_impl(state: &AppState) -> Result<Settings, AppError> {
    let _updating = state.settings.lock_updates();
    let current = state.settings();
    let mut next = (*current).clone();
    for &section in SettingsSection::ALL {
        next = next.reset(section)?;
    }
    keep_data_bindings_of(&mut next, &current);
    let published = commit_many(state, next, SettingsSection::ALL)?;
    let undo = &state.settings_undo;
    *undo.last.lock().unwrap_or_else(PoisonError::into_inner) = Some(ResetSnapshot {
        before: (*current).clone(),
        writes: undo.writes.load(Ordering::SeqCst),
        at: Instant::now(),
    });
    Ok(published)
}

/// Writes back the settings the last reset-all replaced, within 60 s of it and only when no
/// settings write happened since; returns all settings. The snapshot is the core's own copy: the
/// page sends nothing.
///
/// # Errors
/// [`AppError::Conflict`] when there is nothing to undo, the window passed, or settings changed
/// after the reset.
pub fn settings_undo_last_reset_impl(state: &AppState) -> Result<Settings, AppError> {
    let _updating = state.settings.lock_updates();
    let undo = &state.settings_undo;
    let snapshot = undo
        .last
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
        .ok_or_else(|| AppError::conflict("there is no reset to undo"))?;
    if snapshot.at.elapsed() > RESET_UNDO_WINDOW {
        return Err(AppError::conflict("the reset is too old to undo"));
    }
    if snapshot.writes != undo.writes.load(Ordering::SeqCst) {
        return Err(AppError::conflict(
            "settings changed after the reset: it can no longer be undone",
        ));
    }
    restore_locked(state, &snapshot.before)
}

/// Takes the fields a reset-all keeps from `from`: retention limits and the sync folder binding.
fn keep_data_bindings_of(next: &mut Settings, from: &Settings) {
    next.data.history_retention = from.data.history_retention;
    next.data.chat_retention_days = from.data.chat_retention_days;
    next.ai.providers.usage_retention_days = from.ai.providers.usage_retention_days;
    next.mock.log_capacity = from.mock.log_capacity;
    next.sync.directory.clone_from(&from.sync.directory);
    next.sync.auto_export = from.sync.auto_export;
}

/// Writes `previous` (a [`Settings`] the page read before a reset) back in one write: the Undo of
/// a reset. Every section is validated first; the search-key hints stay as they are.
///
/// # Errors
/// A validation error when a section of `previous` is not valid any more.
pub fn settings_restore_impl(state: &AppState, previous: &Settings) -> Result<Settings, AppError> {
    let _updating = state.settings.lock_updates();
    restore_locked(state, previous)
}

/// [`settings_restore_impl`] for a caller that holds the update lock.
fn restore_locked(state: &AppState, previous: &Settings) -> Result<Settings, AppError> {
    let current = state.settings();
    let mut previous = previous.clone();
    previous.ai.keep_hints_of(&current.ai);
    let mut next = (*current).clone();
    for &section in SettingsSection::ALL {
        next = next.with_section_value(section, &previous.section_value(section))?;
    }
    commit_many(state, next, SettingsSection::ALL)
}
