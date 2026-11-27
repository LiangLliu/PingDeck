//! The one place a new provider record is put together.
//!
//! Single responsibility: turn what an entry knows (a preset, a scan's verified endpoints, or a
//! typed address) plus the typed name and key into a [`ProviderDraft`], then give it an id, a
//! name and a position against the stored providers. Scan results, "Add anyway as Unverified"
//! and preset direct-build all go through it, so auth, mounts, names and the first key follow one
//! rule: auth comes from the preset (an endpoint that worked with another style declares it),
//! mounts come from the preset unless a scan verified others, and the typed key rides along.

use spanbay_store::DbConnection as Connection;

use super::providers::category_text;
use super::{next_provider_position, unique_provider_id};
use crate::app::factory_service::new_entity_id;
use crate::error::AppError;
use crate::llm::presets::ProviderPreset;
use crate::llm::store::{self, Provider, ProviderKey};
use crate::llm::wire::{AuthStyle, EndpointUrlMode, Protocol, ProviderEndpoint};

/// The authentication the protocol's own vendor uses when nothing says otherwise.
pub(super) const fn native_auth(protocol: Protocol) -> AuthStyle {
    match protocol {
        Protocol::AnthropicMessages => AuthStyle::XApiKey,
        Protocol::Gemini => AuthStyle::XGoogApiKey,
        _ => AuthStyle::Bearer,
    }
}

/// Endpoints a scan verified, with the auth that worked on its primary protocol.
pub(super) struct Scanned {
    pub(super) auth: AuthStyle,
    pub(super) endpoints: Vec<ProviderEndpoint>,
    pub(super) primary: Option<Protocol>,
}

/// Where the endpoints of a draft come from.
pub(super) enum Mounts {
    /// A scan verified these; the base URL is the scan's root.
    Scanned(Scanned),
    /// Nothing was verified: the preset's endpoints, else one endpoint of `protocol` at the
    /// typed mount.
    Typed {
        protocol: Protocol,
        mount: String,
        url_mode: EndpointUrlMode,
    },
    /// The preset's own region base URL and endpoints (preset direct-build).
    Region(String),
}

/// What an entry knows about the provider to create.
pub(super) struct DraftInput<'a> {
    pub(super) preset: Option<&'a ProviderPreset>,
    /// The origin (`scheme://host[:port]`) the user typed or the scan found.
    pub(super) origin: &'a str,
    pub(super) mounts: Mounts,
    /// The name the user typed; `None` lets the draft pick one.
    pub(super) name: Option<&'a str>,
    /// The category when no preset says one.
    pub(super) category: &'a str,
    pub(super) models_endpoint: Option<String>,
    /// The typed key and its row name.
    pub(super) secret: Option<String>,
    pub(super) key_name: Option<&'a str>,
    pub(super) now_ms: i64,
}

/// A provider ready to save, with its first key.
pub(super) struct ProviderDraft {
    pub(super) provider: Provider,
    /// The first key and its secret; position 0, the store puts it last anyway.
    pub(super) key: Option<(ProviderKey, String)>,
    /// The name was the user's: a clash is their conflict, not a reason to number it.
    pub(super) name_typed: bool,
    /// What the id is made from; the name when `None`.
    pub(super) id_base: Option<String>,
}

/// A preset name made usable as a provider name: `/` and quotes would break `<name>/<model>`
/// references, so a brand name `Acme / Relay` becomes `Acme - Relay`.
fn usable_name(name: &str) -> String {
    name.chars()
        .map(|ch| match ch {
            '/' | '\\' => '-',
            '"' | '\'' | '`' => ' ',
            other if other.is_control() => ' ',
            other => other,
        })
        .collect::<String>()
        .split_whitespace()
        .collect::<Vec<_>>()
        .join(" ")
}

/// The base URL of a preset provider at `origin` with the typed `mount`: the preset region on
/// the same origin, else the origin plus what the user typed before the preset's own mount
/// (`http://box:11434/v1` with Ollama's `/v1` is `http://box:11434`).
fn preset_base(preset: &ProviderPreset, origin: &str, mount: &str) -> String {
    if let Some(region) = preset.regions.iter().find(|region| {
        let base = region.base_url.trim_end_matches('/');
        !base.contains('{')
            && (base == origin || (mount.is_empty() && base.starts_with(&format!("{origin}/"))))
    }) {
        return region.base_url.trim_end_matches('/').to_owned();
    }
    let prefix = preset
        .endpoints
        .iter()
        .find(|endpoint| endpoint.protocol.is_chat())
        .or_else(|| preset.endpoints.first())
        .map_or("", |endpoint| endpoint.path_prefix.trim_end_matches('/'));
    let mount = mount.trim_end_matches('/');
    let path = if prefix.is_empty() {
        mount
    } else {
        mount.strip_suffix(prefix).unwrap_or(mount)
    };
    format!("{origin}{path}")
}

/// Puts the provider together (no id, name or position yet: see [`place`]).
///
/// # Errors
/// [`AppError::Validation`] when nothing names a protocol to speak.
pub(super) fn compose(input: DraftInput<'_>) -> Result<ProviderDraft, AppError> {
    let preset = input.preset;
    let scanned = matches!(input.mounts, Mounts::Scanned(_));
    let (base_url, auth, endpoints, primary) = match input.mounts {
        Mounts::Scanned(scanned) => {
            // The preset's auth is the provider's; an endpoint that worked with another style
            // declares it on itself.
            let auth = preset.map_or(scanned.auth, |preset| preset.auth);
            let endpoints = scanned
                .endpoints
                .into_iter()
                .map(|endpoint| {
                    let worked = endpoint.auth.unwrap_or(scanned.auth);
                    ProviderEndpoint {
                        auth: (worked != auth).then_some(worked),
                        ..endpoint
                    }
                })
                .collect();
            (input.origin.to_owned(), auth, endpoints, scanned.primary)
        }
        Mounts::Region(base_url) => {
            let preset = preset.ok_or_else(|| AppError::internal("a region needs its preset"))?;
            (base_url, preset.auth, preset.endpoints.clone(), None)
        }
        Mounts::Typed {
            protocol,
            mount,
            url_mode,
        } => match preset {
            Some(preset) => (
                preset_base(preset, input.origin, &mount),
                preset.auth,
                preset.endpoints.clone(),
                None,
            ),
            None => (
                input.origin.to_owned(),
                native_auth(protocol),
                vec![ProviderEndpoint {
                    protocol,
                    path_prefix: mount,
                    auth: None,
                    url_mode,
                    enabled: true,
                }],
                protocol.is_chat().then_some(protocol),
            ),
        },
    };
    if endpoints.is_empty() {
        return Err(AppError::validation("check at least one protocol"));
    }
    let typed = input.name.map(str::trim).filter(|name| !name.is_empty());
    let name = match (typed, preset) {
        (Some(name), _) => name.to_owned(),
        (None, Some(preset)) => usable_name(&preset.name),
        (None, None) => input
            .origin
            .split_once("://")
            .map_or(input.origin, |(_, host)| host)
            .to_owned(),
    };
    let now = input.now_ms;
    let key = input
        .secret
        .map(|secret| secret.trim().to_owned())
        .filter(|secret| !secret.is_empty())
        .map(|secret| {
            let name = input
                .key_name
                .map(str::trim)
                .filter(|name| !name.is_empty())
                .unwrap_or("Default");
            let key = ProviderKey {
                id: new_entity_id("key"),
                name: name.to_owned(),
                enabled: true,
                created_at_ms: now,
                updated_at_ms: now,
                ..ProviderKey::default()
            };
            (key, secret)
        });
    let provider = Provider {
        name,
        preset_id: preset.map(|preset| preset.id.clone()),
        category: preset.map_or_else(
            || input.category.to_owned(),
            |preset| category_text(preset.category),
        ),
        base_url: base_url.trim_end_matches('/').to_owned(),
        auth,
        endpoints,
        // A scan says whether the host lists models; otherwise the preset does.
        models_endpoint: input.models_endpoint.or_else(|| {
            preset
                .filter(|_| !scanned)
                .and_then(|preset| preset.models_endpoint.clone())
        }),
        quirks: preset
            .map(|preset| preset.quirks.clone())
            .unwrap_or_default(),
        enabled: true,
        created_at_ms: now,
        updated_at_ms: now,
        primary_protocol: primary,
        ..Provider::default()
    };
    Ok(ProviderDraft {
        provider,
        key,
        name_typed: typed.is_some(),
        id_base: None,
    })
}

/// Gives the draft its id (from the name), its name (numbered when the draft picked it and it
/// is taken: `Ollama 2`) and the position after the last provider.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub(super) fn place(connection: &Connection, draft: &mut ProviderDraft) -> Result<(), AppError> {
    let existing = store::list_providers(connection)?;
    let provider = &mut draft.provider;
    let base = draft.id_base.as_deref().unwrap_or(&provider.name);
    let buried = store::tombstone_ids(connection)?;
    provider.id = unique_provider_id(base, &existing, &buried);
    if !draft.name_typed {
        provider.name = store::unique_provider_name(connection, &provider.id, &provider.name)?;
    }
    provider.position = next_provider_position(&existing);
    if let Some((key, _)) = draft.key.as_mut() {
        key.provider_id.clone_from(&provider.id);
    }
    Ok(())
}

/// Saves a placed draft and its key (the caller holds the transaction).
///
/// # Errors
/// As `store::save_provider` and `store::save_key`.
pub(super) fn save(connection: &Connection, draft: &ProviderDraft) -> Result<Provider, AppError> {
    let now = draft.provider.updated_at_ms;
    let saved = store::save_provider(connection, &draft.provider, now)?;
    if let Some((key, secret)) = &draft.key {
        store::save_key(connection, key, Some(secret), now)?;
    }
    Ok(saved)
}
