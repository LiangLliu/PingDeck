//! Tests of the LLM provider-catalogue persistence (providers, keys, models, routing, probe
//! results); one module per area.

mod facts;
mod keys;
mod models;
mod ops;
mod probe;
mod providers;
mod validation;
mod versions;

use rusqlite::Connection;

use super::{ModelSource, Provider, ProviderKey, ProviderModel, save_key, save_provider};
use crate::llm::wire::{AuthStyle, Protocol, ProviderEndpoint, Quirks};
use crate::storage::secrets;
use pingdeck_llm_domain::llm::presets::ModelRole;

pub(super) use super::test_support::connection;

/// A saved-ready provider speaking OpenAI chat and Anthropic messages.
pub(super) fn provider(id: &str) -> Provider {
    Provider {
        id: id.into(),
        name: format!("Provider {id}"),
        base_url: "https://example.test".into(),
        models_endpoint: Some("/v1/models".into()),
        endpoints: vec![
            ProviderEndpoint {
                protocol: Protocol::OpenaiChat,
                path_prefix: "/v1".into(),
                auth: None,
                url_mode: Default::default(),
                enabled: true,
            },
            ProviderEndpoint {
                protocol: Protocol::AnthropicMessages,
                path_prefix: "/v1".into(),
                auth: None,
                url_mode: Default::default(),
                enabled: true,
            },
        ],
        ..Provider::default()
    }
}

/// A provider speaking OpenAI chat only, with every field spelled out.
pub(super) fn single_endpoint_provider(id: &str) -> Provider {
    Provider {
        id: id.into(),
        name: format!("Provider {id}"),
        preset_id: None,
        category: "custom".into(),
        base_url: "https://example.test".into(),
        auth: AuthStyle::Bearer,
        endpoints: vec![ProviderEndpoint {
            protocol: Protocol::OpenaiChat,
            path_prefix: "/v1".into(),
            auth: None,
            url_mode: Default::default(),
            enabled: true,
        }],
        models_endpoint: Some("/v1/models".into()),
        quirks: Quirks::default(),
        proxy_url: String::new(),
        enabled: true,
        position: 0,
        notes: String::new(),
        created_at_ms: 0,
        updated_at_ms: 0,
        ..Default::default()
    }
}

/// A fast-role model with a context, vision on and tools off.
pub(super) fn fast_model(provider_id: &str, id: &str, source: ModelSource) -> ProviderModel {
    ProviderModel {
        provider_id: provider_id.into(),
        model_id: id.into(),
        source,
        role: Some(ModelRole::Fast),
        context: Some(1000),
        max_output: None,
        vision: true,
        tools: false,
        notes: None,
        enabled: true,
        ..Default::default()
    }
}

pub(super) fn key(id: &str, provider_id: &str) -> ProviderKey {
    ProviderKey {
        id: id.into(),
        provider_id: provider_id.into(),
        name: id.into(),
        ..ProviderKey::default()
    }
}

pub(super) fn model(provider_id: &str, id: &str) -> ProviderModel {
    ProviderModel {
        provider_id: provider_id.into(),
        model_id: id.into(),
        source: ModelSource::Live,
        ..ProviderModel::default()
    }
}

/// A provider with one key, ready for every test that needs a row to hang things on.
pub(super) fn seeded(c: &Connection, provider_id: &str, key_id: &str) {
    save_provider(c, &provider(provider_id), 1).unwrap();
    save_key(
        c,
        &key(key_id, provider_id),
        Some("sk-seeded-secret-000"),
        1,
    )
    .unwrap();
}

pub(super) fn secret(c: &Connection, secret_ref: &str) -> Option<String> {
    secrets::get(c, secret_ref).unwrap()
}

/// One text cell of a raw query, for assertions about what is *stored*.
pub(super) fn raw_text(c: &Connection, sql: &str) -> String {
    c.query_row(sql, [], |row| {
        Ok(match row.get::<_, rusqlite::types::Value>(0)? {
            rusqlite::types::Value::Integer(n) => n.to_string(),
            rusqlite::types::Value::Text(text) => text,
            _ => String::new(),
        })
    })
    .unwrap()
}
