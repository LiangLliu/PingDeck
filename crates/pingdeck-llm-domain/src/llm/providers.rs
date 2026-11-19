//! The provider record and the pure rules around it: the [`Provider`] DTO (with a `Debug` that
//! masks the proxy URL and an explicit `Default`), [`KeyStrategy`], the `/v1/models` direct-id
//! rule, the load-time repair of old scans ([`heal_new_api_tie`]), versioned saves
//! ([`SaveProviderOptions`], [`next_stamp`], [`origin_label`], [`outbound_hosts`]) and what a
//! scan persists ([`SniffPersist`]). Field validation lives in the `validate` submodule.

use std::collections::BTreeSet;

use serde::{Deserialize, Serialize};

use crate::llm::balance_state::CustomBalanceQuery;
use crate::llm::probe::listing::ModelDrift;
use crate::llm::sniff_summary::{
    EvidenceItem, PROXY_DNS_NOTE, RunnerUp, SYSTEM_NAME_NOTE, SniffSummary, clean_field,
};
use crate::llm::wire::{AuthStyle, GatewayFamily, Protocol, ProviderEndpoint, Quirks};
use pingdeck_base::error::{AppError, ErrorCode};

mod validate;

pub use validate::{
    MAX_EXTRA_HEADERS, MAX_HEADER_VALUE_BYTES, MAX_KEY_NOTES_CHARS, MAX_NAME_CHARS,
    redact_proxy_url, validate_base_url, validate_extra_headers, validate_name,
    validate_provider_chars, validate_proxy_url, validate_relative_path,
};

/// Which enabled key of a provider a request starts with (failover then walks forward, wrapping).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum KeyStrategy {
    /// Always the first key in list order.
    #[default]
    Primary,
    /// The next key after the one the previous request started with.
    RoundRobin,
    /// A random key.
    Random,
}

/// A configured upstream provider.
#[derive(Clone, PartialEq, Serialize, Deserialize)]
pub struct Provider {
    /// Stable id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// The preset this was created from.
    #[serde(default)]
    pub preset_id: Option<String>,
    /// Grouping label (`official`, `aggregator`, `local`, `cloud`, `decision`, `custom`).
    pub category: String,
    /// Host root.
    pub base_url: String,
    /// Default authentication of every endpoint.
    pub auth: AuthStyle,
    /// Protocols spoken, with prefixes and optional per-endpoint auth.
    pub endpoints: Vec<ProviderEndpoint>,
    /// Model-list path relative to `base_url`; `None` means models are entered by hand.
    #[serde(default)]
    pub models_endpoint: Option<String>,
    /// Behaviour differences.
    #[serde(default)]
    pub quirks: Quirks,
    /// Proxy for this provider's requests (`http(s)://` or `socks5(h)://`). Empty uses the global
    /// `network.proxy_url` setting and, when that is empty too, the system proxy settings.
    ///
    /// Serialized masked (`http://user:****@host`): the password lives in the secret store and
    /// only the in-memory copy a client is built from carries it.
    #[serde(default, serialize_with = "pingdeck_base::proxy_url::serialize_masked")]
    pub proxy_url: String,
    /// Whether the gateway and tests may use it.
    pub enabled: bool,
    /// Sort order.
    #[serde(default)]
    pub position: i64,
    /// Free-form notes.
    #[serde(default)]
    pub notes: String,
    /// Creation time.
    #[serde(default)]
    pub created_at_ms: i64,
    /// Last change.
    #[serde(default)]
    pub updated_at_ms: i64,
    /// The kind of gateway software behind the base URL. Server-owned.
    #[serde(default = "unknown_family")]
    pub gateway_family: GatewayFamily,
    /// Quota units per USD harvested from `/api/status` (server-owned).
    #[serde(default)]
    pub quota_per_unit: Option<f64>,
    /// Whether the gateway displays quota as currency (server-owned).
    #[serde(default)]
    pub display_in_currency: Option<bool>,
    /// The redacted request log of the last scan, at most [`MAX_SNIFF_EVIDENCE`] rows
    /// (server-owned).
    #[serde(default)]
    pub sniff_evidence: Vec<EvidenceItem>,
    /// The saved custom balance query: header names and secret references, never values.
    #[serde(default)]
    pub custom_balance: Option<CustomBalanceQuery>,
    /// The protocol chat traffic prefers; `None` falls back to the fixed preference order.
    #[serde(default)]
    pub primary_protocol: Option<Protocol>,
    /// The comparison of the last model-list refresh (server-owned).
    #[serde(default)]
    pub model_drift: Option<ModelDrift>,
    /// The host answered differently than when it was added (server-owned).
    #[serde(default)]
    pub needs_resniff: bool,
    /// Identity and confidence of the last scan (server-owned).
    #[serde(default)]
    pub sniff_summary: Option<SniffSummary>,
    /// Balance is switched off (the "Off" mode of the balance setting).
    #[serde(default)]
    pub balance_off: bool,
    /// Which enabled key a request starts with.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_strategy: Option<KeyStrategy>,
    /// Whether a rate-limited or rejected key rests before it is tried again.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub key_cooldown: Option<bool>,
    /// Set when an import brought the provider in (server-owned): until the user marks it
    /// reviewed it is no fallback target. Direct calls by name work.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub review_pending_since_ms: Option<i64>,
}

impl std::fmt::Debug for Provider {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        let proxy_url = if self.proxy_url.is_empty() {
            String::new()
        } else {
            redact_proxy_url(&self.proxy_url)
        };
        f.debug_struct("Provider")
            .field("id", &self.id)
            .field("name", &self.name)
            .field("preset_id", &self.preset_id)
            .field("category", &self.category)
            .field("base_url", &self.base_url)
            .field("auth", &self.auth)
            .field("endpoints", &self.endpoints)
            .field("models_endpoint", &self.models_endpoint)
            .field("quirks", &self.quirks)
            .field("proxy_url", &proxy_url)
            .field("enabled", &self.enabled)
            .field("position", &self.position)
            .field("notes", &self.notes)
            .field("created_at_ms", &self.created_at_ms)
            .field("updated_at_ms", &self.updated_at_ms)
            .field("gateway_family", &self.gateway_family)
            .field("quota_per_unit", &self.quota_per_unit)
            .field("display_in_currency", &self.display_in_currency)
            .field("sniff_evidence", &self.sniff_evidence)
            .field("custom_balance", &self.custom_balance)
            .field("primary_protocol", &self.primary_protocol)
            .field("model_drift", &self.model_drift)
            .field("needs_resniff", &self.needs_resniff)
            .field("sniff_summary", &self.sniff_summary)
            .field("balance_off", &self.balance_off)
            .field("key_strategy", &self.key_strategy)
            .field("key_cooldown", &self.key_cooldown)
            .field("review_pending_since_ms", &self.review_pending_since_ms)
            .finish()
    }
}

/// The `gateway_family` of a provider no scan has identified: the serde default of
/// [`Provider::gateway_family`] (`GatewayFamily` has no `Default`, so serde needs a function) and
/// the value the store writes for a new row.
#[must_use]
pub const fn unknown_family() -> GatewayFamily {
    GatewayFamily::UnknownCompatible
}

/// Enabled models up to which a provider is listed by direct id in `GET /v1/models` unless its
/// `list_in_models` switch says otherwise.
pub const LIST_IN_MODELS_DEFAULT_MAX: usize = 50;

/// Whether the gateway lists the provider's enabled models by direct id in `GET /v1/models`.
#[must_use]
pub fn provider_lists_direct_ids(provider: &Provider, enabled_model_count: usize) -> bool {
    provider
        .quirks
        .list_in_models
        .unwrap_or(enabled_model_count <= LIST_IN_MODELS_DEFAULT_MAX)
}

impl Provider {
    /// Whether routing may use the provider: enabled, with at least one enabled endpoint.
    #[must_use]
    pub fn routable(&self) -> bool {
        self.enabled && self.endpoints.iter().any(|endpoint| endpoint.enabled)
    }
}

impl Default for Provider {
    fn default() -> Self {
        Self {
            id: String::new(),
            name: String::new(),
            preset_id: None,
            category: "custom".to_owned(),
            base_url: String::new(),
            auth: AuthStyle::default(),
            endpoints: Vec::new(),
            models_endpoint: None,
            quirks: Quirks::default(),
            proxy_url: String::new(),
            enabled: true,
            position: 0,
            notes: String::new(),
            created_at_ms: 0,
            updated_at_ms: 0,
            gateway_family: unknown_family(),
            quota_per_unit: None,
            display_in_currency: None,
            sniff_evidence: Vec::new(),
            custom_balance: None,
            primary_protocol: None,
            model_drift: None,
            needs_resniff: false,
            sniff_summary: None,
            balance_off: false,
            key_strategy: None,
            key_cooldown: None,
            review_pending_since_ms: None,
        }
    }
}

/// The brand a stored scan's evidence sentence names (`… system_name My Hub`).
///
/// The sniffer writes the note as [`SYSTEM_NAME_NOTE`] followed by the name and may append
/// [`PROXY_DNS_NOTE`]; both sides share those constants.
fn brand_from_evidence(evidence: &[EvidenceItem]) -> Option<String> {
    evidence
        .iter()
        .filter(|item| item.phase == "identify")
        .find_map(|item| {
            [&item.note, &item.conclusion].into_iter().find_map(|text| {
                let (_, rest) = text.split_once(SYSTEM_NAME_NOTE)?;
                Some(clean_field(
                    rest.split(PROXY_DNS_NOTE).next().unwrap_or(rest),
                ))
            })
        })
        .filter(|brand| !brand.is_empty())
}

/// Lowest score of the old tie: the former scorer gave `NewApi` and `OneApi` the same score of 6
/// (or more) for a New API status page.
const OLD_TIE_MIN_SCORE: i32 = 6;
/// The runner-up score the current scorer gives `OneApi` when `NewApi` wins on such a page (see
/// the `new_api_scores_eight_against_one_api_three` test of `sniff/identify.rs`).
const HEALED_ONE_API_SCORE: i32 = 3;

/// Repairs a provider stored by a scan that hit the old 6:6 tie between `NewApi` and `OneApi`.
pub fn heal_new_api_tie(provider: &mut Provider) {
    let Some(summary) = provider.sniff_summary.as_mut() else {
        return;
    };
    let tied = provider.gateway_family == GatewayFamily::UnknownCompatible
        && summary.family == GatewayFamily::UnknownCompatible
        && !summary.confirmed
        && summary.confidence_score >= OLD_TIE_MIN_SCORE
        && summary.runner_up.is_some_and(|runner_up| {
            runner_up.family == GatewayFamily::OneApi && runner_up.score == summary.confidence_score
        });
    if !tied {
        return;
    }
    provider.gateway_family = GatewayFamily::NewApi;
    summary.family = GatewayFamily::NewApi;
    summary.confirmed = true;
    summary.runner_up = Some(RunnerUp {
        family: GatewayFamily::OneApi,
        score: HEALED_ONE_API_SCORE,
    });
    if let Some(brand) = brand_from_evidence(&provider.sniff_evidence) {
        summary.brand_name = Some(brand);
    }
}

/// What a provider save checks against the stored row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq)]
pub struct SaveProviderOptions {
    /// The version the caller read; `None` saves whatever is stored.
    pub expected_updated_at_ms: Option<i64>,
    /// The user saw the provider move to new outbound hosts and agreed.
    pub confirm_host_change: bool,
    /// A rename keeps the old name routing for [`FORMER_NAME_DAYS`] days.
    pub keep_former_name: bool,
}

/// Days an old provider name keeps routing after a rename (a constant, not a setting).
pub const FORMER_NAME_DAYS: i64 = 30;

/// `scheme://host[:port]` of `url` (lower case, default port dropped); text that is not an
/// absolute URL with a host is its own label, so only unchanged text compares equal.
#[must_use]
pub fn origin_label(url: &str) -> String {
    url::Url::parse(url.trim())
        .ok()
        .and_then(|parsed| {
            let host = parsed.host_str()?.to_ascii_lowercase();
            let port = parsed
                .port()
                .map(|port| format!(":{port}"))
                .unwrap_or_default();
            Some(format!("{}://{host}{port}", parsed.scheme()))
        })
        .unwrap_or_else(|| url.trim().to_owned())
}

/// Every origin a provider sends requests or keys to: its base URL and its custom balance URL.
#[must_use]
pub fn outbound_hosts(provider: &Provider) -> BTreeSet<String> {
    std::iter::once(provider.base_url.as_str())
        .chain(
            provider
                .custom_balance
                .as_ref()
                .map(|query| query.url.as_str()),
        )
        .map(origin_label)
        .collect()
}

/// The `updated_at_ms` a write gives a row now stamped `stored` (`None`: a new row): `now_ms`, or
/// one past `stored` when the clock has not passed it, so two writes never leave the same stamp.
///
/// # Errors
/// [`AppError::Conflict`] coded [`ErrorCode::StaleWrite`] when the caller read another version.
pub fn next_stamp(
    what: &str,
    stored: Option<i64>,
    expected: Option<i64>,
    now_ms: i64,
) -> Result<i64, AppError> {
    if expected.is_some() && expected != stored {
        return Err(
            AppError::conflict(format!("this {what} was changed elsewhere; reload it"))
                .coded(ErrorCode::StaleWrite, None),
        );
    }
    Ok(stored.map_or(now_ms, |stored| now_ms.max(stored.saturating_add(1))))
}

/// Most evidence rows of the last scan a provider keeps.
pub const MAX_SNIFF_EVIDENCE: usize = 20;

/// What a scan writes to a provider: the server-owned identity columns.
#[derive(Debug, Clone, PartialEq)]
pub struct SniffPersist {
    /// The detected family.
    pub family: GatewayFamily,
    /// `quota_per_unit` harvested from `/api/status`.
    pub quota_per_unit: Option<f64>,
    /// `display_in_currency` harvested from `/api/status`.
    pub display_in_currency: Option<bool>,
    /// The redacted request log; the first [`MAX_SNIFF_EVIDENCE`] rows are kept.
    pub evidence: Vec<EvidenceItem>,
    /// Identity and confidence of the scan.
    pub summary: SniffSummary,
}

#[cfg(test)]
mod tests {
    use super::{Provider, provider_lists_direct_ids};

    #[test]
    fn direct_ids_follow_the_quirk_or_the_model_count() {
        let mut p = Provider::default();
        assert!(provider_lists_direct_ids(&p, 50) && !provider_lists_direct_ids(&p, 51));
        p.quirks.list_in_models = Some(true);
        assert!(provider_lists_direct_ids(&p, 300));
        p.quirks.list_in_models = Some(false);
        assert!(!provider_lists_direct_ids(&p, 3));
    }
}
