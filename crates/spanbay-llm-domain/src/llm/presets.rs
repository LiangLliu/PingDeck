//! The bundled provider presets (`fixtures/llm-presets.json`), parsed once.
//!
//! Single responsibility: expose the preset catalog as typed data. The JSON is embedded at build
//! time; unknown fields, protocols, auth styles or roles fail the parse (and the unit test) so the
//! file and the code cannot drift apart. A preset is only a starting point: creating a provider
//! copies it into the database, where the user may edit every field.

use std::sync::LazyLock;

use serde::{Deserialize, Serialize};

use crate::llm::wire::{AuthStyle, Protocol, ProviderEndpoint, Quirks};
use spanbay_base::error::AppError;

const PRESETS_JSON: &str = include_str!("../../../../fixtures/llm-presets.json");

static CATALOG: LazyLock<Result<PresetCatalog, String>> =
    LazyLock::new(|| serde_json::from_str(PRESETS_JSON).map_err(|error| error.to_string()));

/// Kind of provider, for grouping in the UI.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PresetCategory {
    /// The model vendor's own API.
    Official,
    /// A router or hosting service for many vendors' models.
    Aggregator,
    /// A runtime on the user's machine or network.
    Local,
    /// A hyperscaler cloud endpoint.
    Cloud,
    /// A decision-model service.
    Decision,
}

/// What a preset model is meant for.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ModelRole {
    /// The vendor's top model.
    Flagship,
    /// Quality and cost in balance.
    Balanced,
    /// Low latency.
    Fast,
    /// Lowest price.
    Cheap,
    /// A reasoning-tuned model.
    Reasoning,
    /// An embedding model.
    Embedding,
    /// A decision model.
    Decision,
    /// A coding model.
    Code,
}

/// A selectable endpoint host of a provider (a region, a plan, a resource).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetRegion {
    /// Stable id (`global`, `china`, …).
    pub id: String,
    /// Display label.
    pub label: String,
    /// Host root; may hold `{resource}` or `{region}` placeholders to fill at creation time.
    pub base_url: String,
    /// The models.dev provider id that prices this host when it differs from the preset's
    /// (`zhipuai` for BigModel next to Z.ai's `zai`).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_family: Option<String>,
}

/// A model a preset lists as a hint; the live `/models` answer stays the source of truth.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetModel {
    /// Model id as the provider spells it.
    pub id: String,
    /// Intended use.
    pub role: ModelRole,
    /// Context window in tokens.
    #[serde(default)]
    pub context: Option<u64>,
    /// Maximum output tokens.
    #[serde(default)]
    pub max_output: Option<u64>,
    /// Accepts images.
    #[serde(default)]
    pub vision: bool,
    /// Supports tool calls.
    #[serde(default)]
    pub tools: bool,
    /// Caveats from the docs.
    #[serde(default)]
    pub notes: Option<String>,
}

/// One provider preset.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ProviderPreset {
    /// Stable id (`openai`, `deepseek`, …).
    pub id: String,
    /// Display name.
    pub name: String,
    /// Grouping.
    pub category: PresetCategory,
    /// Other names people search for it by (`智谱`, `豆包`, `文心`).
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub search_terms: Vec<String>,
    /// The models.dev provider id whose catalog prices it; a region may override it.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_family: Option<String>,
    /// Documentation link.
    #[serde(default)]
    pub docs_url: Option<String>,
    /// Default authentication for every endpoint.
    pub auth: AuthStyle,
    /// Selectable hosts; the first is the default.
    pub regions: Vec<PresetRegion>,
    /// Protocols the provider speaks, with their path prefixes.
    pub endpoints: Vec<ProviderEndpoint>,
    /// Model-list path relative to the base URL (prefix included); `None` when the provider has
    /// no list endpoint, so models are entered by hand.
    pub models_endpoint: Option<String>,
    /// Behaviour differences.
    #[serde(default)]
    pub quirks: Quirks,
    /// Suggested models; may be empty (Azure deployments, vLLM).
    #[serde(default)]
    pub models: Vec<PresetModel>,
}

/// The whole bundled catalog.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct PresetCatalog {
    /// Format version of the file.
    pub version: u32,
    /// Date the data was researched.
    pub fetched_at: String,
    /// Every provider preset.
    pub providers: Vec<ProviderPreset>,
}

impl PresetCatalog {
    /// The preset with `id`.
    #[must_use]
    pub fn provider(&self, id: &str) -> Option<&ProviderPreset> {
        self.providers.iter().find(|provider| provider.id == id)
    }
}

/// The bundled preset catalog.
///
/// # Errors
/// [`AppError::Internal`] if the embedded JSON no longer parses (guarded by a unit test).
pub fn presets() -> Result<&'static PresetCatalog, AppError> {
    CATALOG.as_ref().map_err(|error| {
        AppError::internal(format!("bundled provider presets are invalid: {error}"))
    })
}

/// Longest DNS label (RFC 1035).
const MAX_DNS_LABEL_BYTES: usize = 63;

/// Replaces `{name}` placeholders in a preset base URL with the user's values. Every placeholder
/// sits inside the host (`{resource}.openai.azure.com`), so a value must be a single DNS label
/// (`[A-Za-z0-9-]+`, at most 63 characters): it can then never add a dot, slash, port or `@` and
/// move the URL to another host. A blank value counts as missing.
///
/// # Errors
/// [`AppError::Validation`] naming the placeholder that has no value or whose value is not a
/// DNS label.
pub fn fill_placeholders(base_url: &str, values: &[(&str, &str)]) -> Result<String, AppError> {
    let mut out = base_url.to_owned();
    for (name, value) in values {
        let token = format!("{{{name}}}");
        let value = value.trim();
        if value.is_empty() || !out.contains(&token) {
            continue;
        }
        if value.len() > MAX_DNS_LABEL_BYTES
            || !value
                .bytes()
                .all(|b| b.is_ascii_alphanumeric() || b == b'-')
        {
            return Err(AppError::validation(format!(
                "{token} must be a single host label of letters, digits and hyphens"
            )));
        }
        out = out.replace(&token, value);
    }
    if let Some(start) = out.find('{') {
        let end = out[start..]
            .find('}')
            .map_or(out.len(), |end| start + end + 1);
        return Err(AppError::validation(format!(
            "fill in {} before saving the provider",
            &out[start..end]
        )));
    }
    Ok(out)
}

/// Preset id → balance adapter id (`balance::AdapterId::as_str`) for presets whose vendor has a
/// balance endpoint. Presets not listed here have no balance adapter. Public so that
/// `spanbay-llm-ops`, which owns `AdapterId`, can check that every adapter id here parses.
pub const BALANCE_ADAPTERS: &[(&str, &str)] = &[
    ("deepseek", "deepseek"),
    ("openrouter", "openrouter"),
    ("moonshot", "moonshot"),
    ("siliconflow", "siliconflow"),
    ("stepfun", "stepfun"),
    ("zhipu", "zhipu"),
];

/// A preset region host the sniffer recognises by domain.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct KnownHost {
    /// The preset that owns the host.
    pub preset_id: String,
    /// The preset region the host belongs to (`global`, `china`, …).
    pub region_id: String,
    /// The balance adapter of the preset (`balance::AdapterId::as_str`), when it has one.
    pub balance_adapter: Option<String>,
}

/// The balance adapter id of the preset `preset_id`, when it has one.
fn balance_adapter_of(preset_id: &str) -> Option<String> {
    BALANCE_ADAPTERS
        .iter()
        .find(|(preset, _)| *preset == preset_id)
        .map(|(_, adapter)| (*adapter).to_owned())
}

/// One concrete preset region host.
struct HostEntry {
    /// Host (punycode, lower-case).
    host: String,
    /// The explicit port of the region base URL.
    port: Option<u16>,
    owner: KnownHost,
}

/// Every preset region whose base URL has a concrete host, in preset then region order; derived
/// once from the frozen preset file.
static KNOWN_HOSTS: LazyLock<Vec<HostEntry>> = LazyLock::new(|| {
    let Ok(catalog) = presets() else {
        return Vec::new();
    };
    let mut table = Vec::new();
    for provider in &catalog.providers {
        let balance_adapter = balance_adapter_of(&provider.id);
        for region in &provider.regions {
            // A placeholder host (`{resource}.openai.azure.com`) does not parse and is skipped.
            let Ok(url) = url::Url::parse(&region.base_url) else {
                continue;
            };
            let Some(host) = url.host_str() else {
                continue;
            };
            table.push(HostEntry {
                host: host.to_owned(),
                port: url.port(),
                owner: KnownHost {
                    preset_id: provider.id.clone(),
                    region_id: region.id.clone(),
                    balance_adapter: balance_adapter.clone(),
                },
            });
        }
    }
    table
});

/// The preset region that owns `host` (lower-case punycode) with the explicit non-default
/// `port` of the typed address; a preset that names a port (`localhost:11434`) only matches it.
#[must_use]
pub fn known_host(host: &str, port: Option<u16>) -> Option<KnownHost> {
    KNOWN_HOSTS
        .iter()
        .find(|entry| entry.host == host && entry.port == port)
        .map(|entry| entry.owner.clone())
}

/// Every concrete host of the preset table (lower-case punycode), for the look-alike check.
pub fn known_hostnames() -> impl Iterator<Item = &'static str> {
    KNOWN_HOSTS.iter().map(|entry| entry.host.as_str())
}

/// `url` without its `scheme://` (unchanged when it has none).
fn after_scheme(url: &str) -> &str {
    url.split_once("://").map_or(url, |(_, rest)| rest)
}

/// Path part of a region base URL (`https://qianfan.baidubce.com/v2` → `/v2`), no trailing slash.
fn region_path(base_url: &str) -> &str {
    let rest = after_scheme(base_url);
    rest.find('/')
        .map_or("", |at| rest[at..].trim_end_matches('/'))
}

/// Whether `host`/`port` fit the authority of a region base URL: the same number of labels, each
/// equal, where a `{placeholder}` label (`{resource}.openai.azure.com`) matches any non-empty
/// label. Assumes the preset host is not a bracketed IPv6 address (the preset file has none).
fn authority_matches(base_url: &str, host: &str, port: Option<u16>) -> bool {
    let authority = after_scheme(base_url).split('/').next().unwrap_or_default();
    let (pattern, pattern_port) = match authority.rsplit_once(':') {
        Some((name, digits)) => (name, digits.parse::<u16>().ok()),
        None => (authority, None),
    };
    if pattern_port != port {
        return false;
    }
    let mut wanted = pattern.split('.');
    let mut actual = host.split('.');
    loop {
        match (wanted.next(), actual.next()) {
            (None, None) => return true,
            (Some(want), Some(got)) if (want.contains('{') && !got.is_empty()) || want == got => {}
            _ => return false,
        }
    }
}

/// The region of the user-picked preset that the typed host fits, including placeholder hosts
/// (`mine.openai.azure.com` for Azure), so the sniffer can narrow its scan to the preset.
#[must_use]
pub fn preset_region_for_host(
    preset_id: &str,
    region_id: Option<&str>,
    host: &str,
    port: Option<u16>,
) -> Option<KnownHost> {
    let provider = presets().ok()?.provider(preset_id)?;
    let region = provider.regions.iter().find(|region| {
        region_id.is_none_or(|wanted| region.id == wanted)
            && authority_matches(&region.base_url, host, port)
    })?;
    Some(KnownHost {
        preset_id: provider.id.clone(),
        region_id: region.id.clone(),
        balance_adapter: balance_adapter_of(&provider.id),
    })
}

/// Mount of `protocol` on a preset region, relative to the host: the region's own path (Qianfan
/// `/v2`) plus the endpoint's `path_prefix`. `None` when the preset does not list the protocol.
#[must_use]
pub fn region_mount(preset_id: &str, region_id: &str, protocol: Protocol) -> Option<String> {
    let provider = presets().ok()?.provider(preset_id)?;
    let region = provider
        .regions
        .iter()
        .find(|region| region.id == region_id)?;
    let endpoint = provider
        .endpoints
        .iter()
        .find(|endpoint| endpoint.protocol == protocol)?;
    Some(format!(
        "{}{}",
        region_path(&region.base_url),
        endpoint.path_prefix.trim_end_matches('/')
    ))
}

/// The preset's model-list path relative to the host (region path included); `None` when the
/// preset has no list endpoint.
#[must_use]
pub fn region_models_path(preset_id: &str, region_id: &str) -> Option<String> {
    let provider = presets().ok()?.provider(preset_id)?;
    let region = provider
        .regions
        .iter()
        .find(|region| region.id == region_id)?;
    let endpoint = provider.models_endpoint.as_deref()?;
    Some(format!("{}{endpoint}", region_path(&region.base_url)))
}

#[cfg(test)]
mod tests;
