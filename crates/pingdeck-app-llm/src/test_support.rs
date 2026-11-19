//! Shared fixtures of the balance, status and usage use-case tests: an in-memory state, providers
//! and keys, an event recorder and a scanned-provider marker.

#![allow(clippy::derivable_impls)]

use std::collections::BTreeMap;
use std::sync::{Arc, Mutex};

use super::AppState;
use crate::app::current_time_ms;
use crate::{
    ProviderFromPresetInput, SaveProviderKeyInput, llm_key_list_impl, llm_key_save_impl,
    llm_model_save_impl, llm_provider_save_impl,
};

impl Default for SaveProviderKeyInput {
    fn default() -> Self {
        Self {
            id: String::new(),
            provider_id: String::new(),
            name: String::new(),
            secret: None,
            enabled: true,
            position: 0,
            api_user_id: None,
            management_token: None,
            notes: None,
            group_name: None,
            protocols: None,
        }
    }
}

impl Default for ProviderFromPresetInput {
    fn default() -> Self {
        Self {
            preset_id: String::new(),
            region_id: None,
            placeholders: BTreeMap::new(),
            name: None,
            secret: None,
        }
    }
}
use crate::events::{EventSink, Sequenced};
use crate::llm::events::LlmEvent;
use crate::llm::safe_http::SafeRequest;
use crate::llm::sniff_summary::SniffSummary;
use crate::llm::store::{
    self, LogWrite, ModelSource, Provider, ProviderKey, ProviderModel, RequestLogEntry,
    SniffPersist,
};
use crate::llm::wire::{AuthStyle, GatewayFamily, Protocol, ProviderEndpoint};

pub(super) const NOW: i64 = 1_800_000_000_000;
pub(super) const MINUTE: i64 = 60_000;
pub(super) const SECRET: &str = "sk-balance-secret-0123456789abcdef";

pub(super) fn open() -> AppState {
    AppState::open_in_memory().expect("state")
}

/// An enabled provider that asks for a bearer key, speaking OpenAI chat under `/v1`.
pub(super) fn provider_template(name: &str, base_url: &str, preset: Option<&str>) -> Provider {
    Provider {
        name: name.to_owned(),
        preset_id: preset.map(str::to_owned),
        category: "custom".to_owned(),
        base_url: base_url.to_owned(),
        auth: AuthStyle::Bearer,
        endpoints: vec![ProviderEndpoint {
            protocol: Protocol::OpenaiChat,
            path_prefix: "/v1".to_owned(),
            auth: None,
            url_mode: Default::default(),
            enabled: true,
        }],
        models_endpoint: Some("/v1/models".to_owned()),
        ..Provider::default()
    }
}

pub(super) fn add_provider(
    state: &AppState,
    name: &str,
    base_url: &str,
    preset: Option<&str>,
) -> Provider {
    llm_provider_save_impl(state, provider_template(name, base_url, preset)).expect("provider")
}

/// An enabled manual model.
pub(super) fn add_model(state: &AppState, provider_id: &str, model_id: &str, enabled: bool) {
    llm_model_save_impl(
        state,
        ProviderModel {
            provider_id: provider_id.to_owned(),
            model_id: model_id.to_owned(),
            source: ModelSource::Manual,
            enabled,
            ..ProviderModel::default()
        },
    )
    .expect("model");
}

/// Stores one request-log row for `provider_id` (a successful gateway request now, unless
/// `edit` changes it).
pub(super) fn insert_request(
    state: &AppState,
    provider_id: &str,
    edit: impl FnOnce(&mut RequestLogEntry),
) {
    let mut entry = RequestLogEntry {
        ts_ms: current_time_ms(),
        protocol_in: "openai_chat".to_owned(),
        requested_model: "m".to_owned(),
        provider_id: Some(provider_id.to_owned()),
        status: 200,
        outcome: "success".to_owned(),
        ..RequestLogEntry::default()
    };
    edit(&mut entry);
    store::insert_log(
        state.store().connection(),
        &LogWrite {
            entry: &entry,
            capture: None,
            rollup: None,
        },
    )
    .expect("log row");
}

pub(super) fn add_key(
    state: &AppState,
    provider_id: &str,
    name: &str,
    secret: &str,
    position: i64,
    enabled: bool,
) -> ProviderKey {
    llm_key_save_impl(
        state,
        SaveProviderKeyInput {
            provider_id: provider_id.to_owned(),
            name: name.to_owned(),
            secret: Some(secret.to_owned()),
            enabled,
            position,
            ..Default::default()
        },
    )
    .expect("key")
}

pub(super) fn deepseek(state: &AppState) -> (Provider, ProviderKey) {
    let provider = add_provider(
        state,
        "DeepSeek",
        "https://api.deepseek.com",
        Some("deepseek"),
    );
    let key = add_key(state, &provider.id, "main", SECRET, 0, true);
    (provider, key)
}

pub(super) fn events_of(state: &AppState) -> Arc<Mutex<Vec<LlmEvent>>> {
    let seen: Arc<Mutex<Vec<LlmEvent>>> = Arc::default();
    let sink_seen = Arc::clone(&seen);
    let sink: Arc<dyn EventSink<Sequenced<LlmEvent>>> =
        Arc::new(move |event: Sequenced<LlmEvent>| {
            sink_seen.lock().expect("events").push(event.event);
            true
        });
    state.llm.events().subscribe_live(sink).expect("subscribe");
    seen
}

pub(super) fn stored_keys(state: &AppState, provider_id: &str) -> Vec<ProviderKey> {
    llm_key_list_impl(state, Some(provider_id)).expect("keys")
}

pub(super) fn header(request: &SafeRequest, name: &str) -> Option<String> {
    request
        .headers
        .iter()
        .find(|(own, _)| own.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.clone())
}

/// Marks a provider as scanned: its family, divisor and the done-hub quota meaning.
pub(super) fn set_family(
    state: &AppState,
    provider_id: &str,
    family: GatewayFamily,
    remaining: bool,
) {
    let store = state.store();
    store::set_provider_sniff(
        store.connection(),
        provider_id,
        &SniffPersist {
            family,
            quota_per_unit: Some(500_000.0),
            display_in_currency: Some(true),
            evidence: Vec::new(),
            summary: SniffSummary {
                family,
                brand_name: None,
                version: None,
                confidence_score: 8,
                confirmed: true,
                runner_up: None,
                requests_used: 1,
                elapsed_ms: 1,
                tokens_spent: 0,
                quota_is_remaining: remaining,
                scanned_at_ms: NOW,
            },
        },
    )
    .expect("sniff");
}
