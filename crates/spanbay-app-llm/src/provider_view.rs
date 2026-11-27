//! The provider record the Providers page reads: the stored provider plus what the page cannot
//! derive itself.
//!
//! Single responsibility: wrap [`Provider`] into a [`ProviderView`]:
//! masked hints of the write-only header secrets (never a value), the source of every
//! compatibility flag and what the last probe suggests. Writes stay with
//! `llm_provider_save_impl`; internal callers keep using the stored [`Provider`].

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;
use spanbay_store::DbConnection as Connection;

use super::providers::llm_provider_save_guarded_impl;
use super::with_connection;
use crate::app::AppState;
use crate::error::AppError;
use crate::llm::presets;
use crate::llm::probe::quirks::{QuirkSource, QuirkSuggestion, quirk_sources, quirk_suggestions};
use crate::llm::store::{self, ProbeResult, Provider};
use crate::llm::wire::Quirks;

/// A stored provider with its header-secret hints and compatibility derivations.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderView {
    /// The stored provider; its fields stay at the top level of the JSON.
    #[serde(flatten)]
    pub provider: Provider,
    /// Extra request header name -> masked hint of its secret (`SecretStore::hint`; empty when
    /// the secret is unset). Values are write-only.
    pub extra_header_hints: BTreeMap<String, String>,
    /// Custom balance query header name -> masked hint of its secret (empty when unset).
    pub balance_header_hints: BTreeMap<String, String>,
    /// Where each compatibility flag's value comes from, keyed by flag name.
    pub quirk_sources: BTreeMap<String, QuirkSource>,
    /// Flag values the last probe suggests, keyed by flag name; a flag that already holds the
    /// suggested value is left out.
    pub quirk_suggestions: BTreeMap<String, QuirkSuggestion>,
    /// The quirks of the provider's preset, which an unset flag follows: the panel shows
    /// "Auto: on, from the preset". `None` without a preset.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub preset_quirks: Option<Quirks>,
}

/// The probe results of every key of `provider`.
fn provider_results(
    connection: &Connection,
    provider: &Provider,
) -> Result<Vec<ProbeResult>, AppError> {
    let mut results = Vec::new();
    for key in store::list_keys(connection, Some(&provider.id))? {
        results.extend(store::list_probe_results(connection, Some(&key.id))?);
    }
    Ok(results)
}

/// The [`ProviderView`] of `provider`.
fn view_of(connection: &Connection, provider: Provider) -> Result<ProviderView, AppError> {
    let hints = store::provider_header_hints(connection, &provider)?;
    let preset_quirks = provider
        .preset_id
        .as_deref()
        .and_then(|id| presets::presets().ok()?.provider(id))
        .map(|preset| &preset.quirks);
    let current = serde_json::to_value(&provider.quirks).unwrap_or(Value::Null);
    let mut suggestions = quirk_suggestions(&provider_results(connection, &provider)?);
    suggestions.retain(|flag, suggestion| current.get(flag) != Some(&suggestion.value));
    Ok(ProviderView {
        quirk_sources: quirk_sources(&provider.quirks, preset_quirks),
        extra_header_hints: hints.extra,
        balance_header_hints: hints.balance,
        quirk_suggestions: suggestions,
        preset_quirks: preset_quirks.cloned(),
        provider,
    })
}

/// Every provider as a [`ProviderView`], in display order.
///
/// # Errors
/// [`AppError::Database`] on failure.
pub fn llm_provider_list_view_impl(state: &AppState) -> Result<Vec<ProviderView>, AppError> {
    with_connection(state, |connection| {
        store::list_providers(connection)?
            .into_iter()
            .map(|provider| view_of(connection, provider))
            .collect()
    })
}

/// One provider as a [`ProviderView`].
///
/// # Errors
/// [`AppError::NotFound`] when it does not exist, [`AppError::Database`] on failure.
pub fn llm_provider_get_view_impl(state: &AppState, id: &str) -> Result<ProviderView, AppError> {
    with_connection(state, |connection| {
        let provider = store::get_provider(connection, id)?
            .ok_or_else(|| AppError::not_found(format!("provider {id} not found")))?;
        view_of(connection, provider)
    })
}

/// [`llm_provider_save_guarded_impl`] returning the saved provider as a [`ProviderView`]: the page
/// gets the hints of the secrets it just wrote (an empty header value deletes its secret, a value
/// equal to its own placeholder leaves it unchanged).
///
/// # Errors
/// As [`llm_provider_save_guarded_impl`].
pub fn llm_provider_save_view_impl(
    state: &AppState,
    provider: Provider,
    options: store::SaveProviderOptions,
) -> Result<ProviderView, AppError> {
    let saved = llm_provider_save_guarded_impl(state, provider, options)?;
    with_connection(state, |connection| view_of(connection, saved))
}
