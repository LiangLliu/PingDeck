//! The gateway page's read model: status, base URLs, endpoints, accepted Host names, today's
//! numbers and the clients table.
//!
//! Single responsibility: [`GatewayOverviewDto`] and the pure [`build_overview`] that folds the
//! facts the use case collected (status, settings, clients, routing snapshot, today's rollup
//! rows) into it. Nothing here reads state, so every rule — which amounts are summed — is
//! unit-testable. The alias panel types stay for the frozen DTO and are always empty.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};

use crate::llm::gateway::{GatewayStatus, LanAddress};
use crate::llm::router::RoutingSnapshot;
use crate::llm::store::GatewayClient;
use crate::llm::usage::{RollupRow, UsageToday};
use crate::llm::wire::Protocol;
use crate::settings::AiGatewaySettings;

/// The Host names the guard accepts in every mode.
const LOOPBACK_NAMES: [&str; 3] = ["127.0.0.1", "localhost", "[::1]"];

/// The state word of one alias target, from the circuit snapshot and the configuration.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum TargetState {
    /// Circuit closed: requests flow.
    Healthy,
    /// Circuit half-open, or open with the rest over: the next request is the probe.
    Recovering,
    /// Circuit open on every key; `retry_in_s` says when the next probe is allowed.
    CircuitOpen,
    /// The target, its provider or its alias is switched off.
    Disabled,
    /// The provider has no enabled key (for the key the target names).
    NoKey,
}

/// One ordered target of an alias.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TargetRow {
    /// Provider id.
    pub provider_id: String,
    /// Provider display name.
    pub provider_name: String,
    /// Model id as sent upstream.
    pub model_id: String,
    /// The key the target pins; `None` means any enabled key of the provider.
    pub key_id: Option<String>,
    /// The state.
    pub state: TargetState,
    /// The text next to the dot: `Healthy`, `Recovering`, `Circuit open, retry in 24 s`,
    /// `Disabled` or `No key`.
    pub state_text: String,
    /// Seconds until the next probe, while `state` is [`TargetState::CircuitOpen`].
    pub retry_in_s: Option<u64>,
    /// This target served the alias's latest successful request ("served now").
    pub served_now: bool,
}

/// One row of the "Aliases in use" panel.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct AliasRow {
    /// The alias name clients send.
    pub name: String,
    /// The alias is routable.
    pub enabled: bool,
    /// Targets in failover order.
    pub targets: Vec<TargetRow>,
    /// No target can take a request now ("No target available").
    pub no_target_available: bool,
    /// Gateway requests of the local day naming this alias.
    pub requests_today: u64,
    /// Names of the active clients that may use it.
    pub clients: Vec<String>,
}

/// One of the three "base URL to give a client" rows.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct BaseUrlRow {
    /// `openai`, `anthropic` or `gemini`.
    pub family: String,
    /// The URL to copy.
    pub url: String,
    /// The tools it fits.
    pub tools: Vec<String>,
    /// Models `GET /v1/models` lists for an allow-all client through this style: enabled aliases
    /// with a target on a provider speaking it, and the direct `provider/model` ids of providers
    /// with an endpoint of the family (OpenAI-style: chat, responses or embeddings).
    pub model_count: usize,
}

/// One endpoint of the gateway; its full URL is the family's base URL plus `path`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct EndpointRow {
    /// `openai`, `anthropic` or `gemini`: the base URL row it sits under.
    pub family: String,
    /// `GET` or `POST`.
    pub method: String,
    /// The path as text (`{model}` literally).
    pub path: String,
}

/// The "Accepted Host names" block.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct HostNames {
    /// The loopback names, always accepted.
    pub loopback: Vec<String>,
    /// `ai.gateway.extra_allowed_hosts`.
    pub extra: Vec<String>,
    /// "Serve on local network" is on.
    pub lan_enabled: bool,
    /// With LAN on: this Mac's addresses and `<hostname>.local`, private-range first (empty when
    /// LAN is off).
    pub lan: Vec<LanAddress>,
    /// The same addresses whether or not LAN is on (private-range first): what "Serve on local
    /// network" would expose, for its confirm.
    pub lan_candidates: Vec<LanAddress>,
}

/// One row of the read-only clients table.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ClientRow {
    /// Client id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// `pd_cc_••••9d2f`: the fixed prefix and the last four characters.
    pub token_hint: String,
    /// Every alias and model is allowed.
    pub allow_all: bool,
    /// Retired alias names: always empty for new data.
    pub allowed_aliases: Vec<String>,
    /// `provider_id/model` references allowed when `allow_all` is false.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub allowed_models: Vec<String>,
    /// Gateway requests of the local day by this client.
    pub requests_today: u64,
    /// Last authenticated request (Unix ms), `None` for "Never".
    pub last_used_ms: Option<i64>,
    /// The client is revoked ("Revoked" tag).
    pub revoked: bool,
}

/// What the Providers page shows empty states for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OverviewEmpty {
    /// No enabled provider has an enabled key.
    pub no_provider_key: bool,
    /// No alias exists: always `true` since aliases were retired (frozen field).
    pub no_aliases: bool,
    /// No active client exists (every request gets 401).
    pub no_active_clients: bool,
}

/// Everything the gateway page renders, in one read cheap enough to poll every 5 s.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct GatewayOverviewDto {
    /// State, address, counters and the last start error.
    pub status: GatewayStatus,
    /// The three base URLs.
    pub base_urls: Vec<BaseUrlRow>,
    /// The six endpoints in display order.
    pub endpoints: Vec<EndpointRow>,
    /// The accepted Host names.
    pub hosts: HostNames,
    /// Retired with aliases: always empty (frozen field).
    pub aliases: Vec<AliasRow>,
    /// Today's gateway rows from the rollup; `None` with `today_error` when
    /// the rollup could not be read.
    pub today: Option<UsageToday>,
    /// Why `today` is missing.
    pub today_error: Option<String>,
    /// The clients table.
    pub clients: Vec<ClientRow>,
    /// Empty-state flags.
    pub empty: OverviewEmpty,
}

/// What [`build_overview`] folds.
pub(super) struct OverviewInputs<'a> {
    pub status: GatewayStatus,
    pub settings: &'a AiGatewaySettings,
    /// This machine's LAN names, looked up whether or not LAN is on (`hosts.lan` lists them only
    /// when it is, `hosts.lan_candidates` always).
    pub lan: Vec<LanAddress>,
    pub clients: &'a [GatewayClient],
    pub snapshot: &'a RoutingSnapshot,
    /// Today's gateway rollup rows and the summary of them, or why they are unavailable.
    pub today: Result<(Vec<RollupRow>, UsageToday), String>,
}

/// `pd_cc_••••9d2f` for a client; a legacy client (no last four) shows the prefix and bullets.
pub(super) fn token_hint(client: &GatewayClient) -> String {
    format!("{}••••{}", client.token_prefix, client.token_last4)
}

fn base_urls(
    status: &GatewayStatus,
    settings: &AiGatewaySettings,
    snapshot: &RoutingSnapshot,
) -> Vec<BaseUrlRow> {
    let base = if status.base_url.is_empty() {
        format!("http://127.0.0.1:{}", settings.port)
    } else {
        status.base_url.clone()
    };
    let row = |family: &str, url: String, tools: &[&str], protocols: &[Protocol]| BaseUrlRow {
        family: family.to_owned(),
        url,
        tools: tools.iter().map(|tool| (*tool).to_owned()).collect(),
        model_count: snapshot.listed_models_via(protocols).len(),
    };
    vec![
        row(
            "openai",
            format!("{base}/v1"),
            &["Codex", "OpenCode", "Zed", "Cherry Studio", "Most SDKs"],
            &[
                Protocol::OpenaiChat,
                Protocol::OpenaiResponses,
                Protocol::Embeddings,
            ],
        ),
        row(
            "anthropic",
            base.clone(),
            &["Claude Code", "Anthropic SDKs"],
            &[Protocol::AnthropicMessages],
        ),
        row(
            "gemini",
            base,
            &["Gemini CLI", "Google SDKs"],
            &[Protocol::Gemini],
        ),
    ]
}

fn endpoints() -> Vec<EndpointRow> {
    let row = |family: &str, method: &str, path: &str| EndpointRow {
        family: family.to_owned(),
        method: method.to_owned(),
        path: path.to_owned(),
    };
    vec![
        row("openai", "GET", "/v1/models"),
        row("openai", "POST", "/v1/chat/completions"),
        row("openai", "POST", "/v1/responses"),
        row("openai", "POST", "/v1/embeddings"),
        row("anthropic", "POST", "/v1/messages"),
        row("gemini", "POST", "/v1beta/models/{model}:generateContent"),
    ]
}

fn client_rows(clients: &[GatewayClient], today_rows: &[RollupRow]) -> Vec<ClientRow> {
    let mut today: BTreeMap<&str, u64> = BTreeMap::new();
    for row in today_rows {
        *today.entry(row.client_id.as_str()).or_default() += row.requests;
    }
    clients
        .iter()
        .map(|client| ClientRow {
            id: client.id.clone(),
            name: client.name.clone(),
            token_hint: token_hint(client),
            allow_all: client.allow_all,
            allowed_aliases: client.allowed_aliases.clone(),
            allowed_models: client.allowed_models.clone(),
            requests_today: today.get(client.id.as_str()).copied().unwrap_or(0),
            last_used_ms: client.last_used_ms,
            revoked: !client.is_active(),
        })
        .collect()
}

/// Builds the page model.
pub(super) fn build_overview(inputs: OverviewInputs<'_>) -> GatewayOverviewDto {
    let (today_rows, today, today_error) = match &inputs.today {
        Ok((rows, today)) => (rows.as_slice(), Some(today.clone()), None),
        Err(message) => (&[][..], None, Some(message.clone())),
    };
    let snapshot = inputs.snapshot;
    let empty = OverviewEmpty {
        no_provider_key: !snapshot.providers.values().any(|provider| {
            provider.enabled
                && snapshot
                    .keys
                    .get(&provider.id)
                    .is_some_and(|k| !k.is_empty())
        }),
        no_aliases: true,
        no_active_clients: !inputs.clients.iter().any(GatewayClient::is_active),
    };
    GatewayOverviewDto {
        base_urls: base_urls(&inputs.status, inputs.settings, snapshot),
        endpoints: endpoints(),
        hosts: HostNames {
            loopback: LOOPBACK_NAMES
                .iter()
                .map(|name| (*name).to_owned())
                .collect(),
            extra: inputs.settings.extra_allowed_hosts.clone(),
            lan_enabled: inputs.settings.serve_lan,
            lan: if inputs.settings.serve_lan {
                inputs.lan.clone()
            } else {
                Vec::new()
            },
            lan_candidates: inputs.lan.clone(),
        },
        aliases: Vec::new(),
        clients: client_rows(inputs.clients, today_rows),
        empty,
        today,
        today_error,
        status: inputs.status,
    }
}
