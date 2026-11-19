//! Saving the outcome of a scan: "Add provider", "Add anyway as Unverified", "Add this key to it"
//! and "Apply changes" of a re-sniff.
//!
//! Single responsibility: turn a cached [`SniffReport`] plus the user's selection into one
//! `llm::store` transaction. The report comes from core's own cache, so a forged report cannot
//! be submitted; the selection is validated against it. Duplicate detection (address and key
//! hash) happens here and never leaves core.

use std::collections::HashSet;

use pingdeck_store::DbConnection as Connection;
use serde::{Deserialize, Serialize};

use super::provider_draft::{self, DraftInput, Mounts, Scanned};
use super::{AppState, with_connection};
use crate::app::current_time_ms;
use crate::app::factory_service::new_entity_id;
use crate::crypto::sha256_hex;
use crate::error::AppError;
use crate::llm::balance_state::StoredBalance;
use crate::llm::events::LlmEvent;
use crate::llm::presets;
use crate::llm::sniff::{
    KeyCheckStatus, RouteVerdict, SniffError, SniffReport, SniffWarning, SniffedEndpoint,
    UrlHostClass, duplicate_key, normalize_base_url,
};
use crate::llm::sniff_summary::SniffSummary;
use crate::llm::store::{
    self, KeyCheck, ListedModel, MAX_SNIFF_EVIDENCE, ModelSource, NewKeyFromSniff,
    NewProviderFromSniff, Provider, ProviderKey, ProviderModel, SniffChecks, SniffPersist,
};
use crate::llm::wire::{EndpointUrlMode, GatewayFamily, Protocol, ProviderEndpoint};
use crate::sniff::{ExtraInput, sniff_error};
use pingdeck_model::model_catalog::{ModelPricing, PricingMode};

/// Most hand-typed models one Add accepts.
const MAX_MANUAL_MODELS: usize = 500;

/// Longest model id of a hand-typed model.
const MAX_MODEL_ID_LEN: usize = 256;

/// The fixed chat preference order, used to find a provider's primary mount.
const CHAT_ORDER: [Protocol; 4] = [
    Protocol::OpenaiChat,
    Protocol::AnthropicMessages,
    Protocol::OpenaiResponses,
    Protocol::Gemini,
];

/// What the user chose on the result card.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct SniffSelection {
    /// The provider's name (the card's editable name field).
    pub name: String,
    /// The chat protocol chat traffic prefers; `None` takes the scan's default.
    #[serde(default)]
    pub primary_protocol: Option<Protocol>,
    /// The checked rows of the Protocols table.
    pub protocols: Vec<Protocol>,
    /// The model ids that start enabled; every other listed model is saved disabled (an empty
    /// list saves all of them disabled). The scan's `default_enabled` flags are the
    /// wizard's initial selection.
    #[serde(default)]
    pub enabled_models: Vec<String>,
    /// Model ids typed by hand because the host lists none; saved enabled.
    #[serde(default)]
    pub manual_models: Vec<String>,
    /// Name of the new key row; "Default" when absent.
    #[serde(default)]
    pub key_name: Option<String>,
}

/// A stored provider named in a duplicate notice.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateRef {
    /// Provider id ("Open it").
    pub id: String,
    /// Provider name ("already added as 'relay.acme.dev'").
    pub name: String,
}

/// The duplicates a scan has among the stored providers.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct DuplicateInfo {
    /// The provider at the same address (scheme, host, port, primary mount).
    pub provider: Option<DuplicateRef>,
    /// A provider that already holds the typed key (compared by SHA-256, a warning only).
    pub key_used_by: Option<DuplicateRef>,
}

/// Message prefix of the `Conflict` that carries a [`DuplicateInfo`].
const DUPLICATE_PREFIX: &str = "duplicate_provider: ";

impl DuplicateInfo {
    /// The `Conflict` an Add returns for an address that exists already: the typed info follows
    /// the stable prefix as JSON, [`DuplicateInfo::from_conflict`] reads it back.
    #[must_use]
    pub fn into_conflict(self) -> AppError {
        let json = serde_json::to_string(&self).unwrap_or_default();
        AppError::conflict(format!("{DUPLICATE_PREFIX}{json}"))
    }

    /// The info a duplicate `Conflict` carries; `None` for any other error.
    #[must_use]
    pub fn from_conflict(error: &AppError) -> Option<Self> {
        match error {
            AppError::Conflict { message, .. } => {
                serde_json::from_str(message.strip_prefix(DUPLICATE_PREFIX)?).ok()
            }
            _ => None,
        }
    }
}

/// Runs `work` in one immediate transaction: the store functions inside nest as savepoints, so
/// the whole sequence lands or none of it does.
pub(super) fn in_transaction<T>(
    connection: &Connection,
    work: impl FnOnce(&Connection) -> Result<T, AppError>,
) -> Result<T, AppError> {
    store::in_write_transaction(connection, work)
}

pub(super) fn cached_report(state: &AppState, report_id: &str) -> Result<SniffReport, AppError> {
    state
        .llm
        .services
        .sniff_cache
        .get(report_id, current_time_ms())
        .ok_or_else(|| AppError::not_found("the scan result expired: scan the address again"))
}

/// Checks `selection` against the report and builds the endpoint records:
/// a protocol the scan did not probe or marked absent is refused, a primary must be a checked
/// chat or decision protocol, and the provider's auth is the one that worked on the primary.
fn plan_endpoints(report: &SniffReport, selection: &SniffSelection) -> Result<Scanned, AppError> {
    for protocol in &selection.protocols {
        let endpoint = report
            .endpoints
            .iter()
            .find(|endpoint| endpoint.protocol == *protocol)
            .ok_or_else(|| {
                AppError::validation(format!("the scan did not probe {}", protocol.as_str()))
            })?;
        if endpoint.route == RouteVerdict::Absent {
            return Err(AppError::validation(format!(
                "the scan found no {} route",
                protocol.as_str()
            )));
        }
    }
    let chosen: Vec<&SniffedEndpoint> = report
        .endpoints
        .iter()
        .filter(|endpoint| selection.protocols.contains(&endpoint.protocol))
        .collect();
    let Some(first) = chosen.first() else {
        return Err(AppError::validation("check at least one protocol"));
    };
    let primary = match selection.primary_protocol {
        Some(primary) => {
            if !(primary.is_chat() || primary.is_decision())
                || !chosen.iter().any(|endpoint| endpoint.protocol == primary)
            {
                return Err(AppError::validation(
                    "the primary protocol must be a checked chat or decision protocol",
                ));
            }
            Some(primary)
        }
        None => chosen
            .iter()
            .find(|endpoint| endpoint.is_default && endpoint.protocol.is_chat())
            .or_else(|| chosen.iter().find(|endpoint| endpoint.protocol.is_chat()))
            .map(|endpoint| endpoint.protocol),
    };
    let auth = primary
        .and_then(|primary| chosen.iter().find(|endpoint| endpoint.protocol == primary))
        .unwrap_or(first)
        .auth;
    let endpoints = chosen
        .iter()
        .map(|endpoint| ProviderEndpoint {
            protocol: endpoint.protocol,
            path_prefix: endpoint.path_prefix.clone(),
            auth: (endpoint.auth != auth).then_some(endpoint.auth),
            url_mode: endpoint.url_mode,
            enabled: true,
        })
        .collect();
    Ok(Scanned {
        auth,
        endpoints,
        primary,
    })
}

/// The mount of the provider's primary protocol (duplicate identity).
fn primary_mount(provider: &Provider) -> &str {
    let by_protocol = |protocol: Protocol| {
        provider
            .endpoints
            .iter()
            .find(|endpoint| endpoint.protocol == protocol)
    };
    provider
        .primary_protocol
        .and_then(by_protocol)
        .or_else(|| CHAT_ORDER.into_iter().find_map(by_protocol))
        .or_else(|| provider.endpoints.first())
        .map_or("", |endpoint| endpoint.path_prefix.as_str())
}

/// The mount a report would be stored with when nothing was chosen yet: its default endpoint.
fn default_mount(report: &SniffReport) -> &str {
    report
        .endpoints
        .iter()
        .find(|endpoint| endpoint.is_default)
        .or_else(|| report.endpoints.iter().find(|endpoint| endpoint.supported))
        .map_or("", |endpoint| endpoint.path_prefix.as_str())
}

/// The stored providers at `root_url` + `mount` and the provider that holds `api_key`, compared
/// by address identity and by SHA-256 of the secret (never by plaintext value).
fn find_duplicates(
    connection: &Connection,
    root_url: &str,
    mount: &str,
    api_key: Option<&str>,
) -> Result<DuplicateInfo, AppError> {
    let wanted = duplicate_key(root_url, mount);
    let provider = store::list_providers(connection)?
        .into_iter()
        .find(|stored| {
            wanted.is_some() && duplicate_key(&stored.base_url, primary_mount(stored)) == wanted
        })
        .map(|stored| DuplicateRef {
            id: stored.id,
            name: stored.name,
        });
    let key_used_by = match api_key.map(str::trim).filter(|key| !key.is_empty()) {
        Some(key) => key_holder(connection, &sha256_hex(key.as_bytes()), None)?,
        None => None,
    };
    Ok(DuplicateInfo {
        provider,
        key_used_by,
    })
}

/// The provider (other than `except`) that stores a key whose SHA-256 is `digest`.
fn key_holder(
    connection: &Connection,
    digest: &str,
    except: Option<&str>,
) -> Result<Option<DuplicateRef>, AppError> {
    let providers = store::list_providers(connection)?;
    for resolved in store::list_resolved_keys(connection)? {
        let secret = resolved.secret.trim();
        if secret.is_empty()
            || except == Some(resolved.key.provider_id.as_str())
            || sha256_hex(secret.as_bytes()) != digest
        {
            continue;
        }
        if let Some(provider) = providers
            .iter()
            .find(|provider| provider.id == resolved.key.provider_id)
        {
            return Ok(Some(DuplicateRef {
                id: provider.id.clone(),
                name: provider.name.clone(),
            }));
        }
    }
    Ok(None)
}

/// Whether the family is a relay or gateway product (category `relay`).
const fn is_relay(family: GatewayFamily) -> bool {
    matches!(
        family,
        GatewayFamily::NewApi
            | GatewayFamily::OneApi
            | GatewayFamily::Veloera
            | GatewayFamily::DoneHub
            | GatewayFamily::RixApi
            | GatewayFamily::Sub2Api
            | GatewayFamily::VoApi
            | GatewayFamily::LiteLlm
            | GatewayFamily::OpenRouter
            | GatewayFamily::CliProxyApi
            | GatewayFamily::VercelAiGateway
    )
}

/// Seeds a key's balance with the scan's first finding.
fn seeded_balance(report: &SniffReport) -> Option<StoredBalance> {
    report.balance.clone().map(|balance| StoredBalance {
        last_good: Some(balance),
        error: None,
        stale: false,
    })
}

/// The scan's Connectivity and Auth results for the new key; `None` when the key
/// was never tried.
fn key_checks(report: &SniffReport) -> Option<SniffChecks> {
    if report.key_status == KeyCheckStatus::Untested {
        return None;
    }
    let observed = &report.key_check;
    Some(SniffChecks {
        report_id: report.report_id.clone(),
        connectivity: KeyCheck {
            ok: observed.connectivity_ok,
            status: observed.http_status,
            latency_ms: observed.latency_ms,
        },
        auth: KeyCheck {
            ok: matches!(
                report.key_status,
                KeyCheckStatus::Accepted | KeyCheckStatus::ModelsBlockedButChatWorks
            ),
            status: observed.http_status,
            latency_ms: observed.latency_ms,
        },
    })
}

fn sniff_persist(report: &SniffReport, now_ms: i64) -> SniffPersist {
    SniffPersist {
        family: report.family,
        quota_per_unit: report.quota_per_unit,
        display_in_currency: report.display_in_currency,
        evidence: report
            .evidence
            .iter()
            .take(MAX_SNIFF_EVIDENCE)
            .cloned()
            .collect(),
        summary: SniffSummary {
            family: report.family,
            brand_name: report.brand_name.clone(),
            version: report.version.clone(),
            confidence_score: report.confidence_score,
            confirmed: report.confirmed,
            runner_up: report.runner_up,
            requests_used: report.requests_used,
            elapsed_ms: report.elapsed_ms,
            tokens_spent: report.tokens_spent,
            quota_is_remaining: report.quota_is_remaining,
            scanned_at_ms: now_ms,
        },
    }
}

/// The listed models with their enabled flag: enabled exactly when the selection
/// names them. An id the scan did not list is refused.
fn listed_models(
    report: &SniffReport,
    selection: &SniffSelection,
) -> Result<Vec<(ListedModel, bool)>, AppError> {
    let listed: HashSet<&str> = report
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    if let Some(unknown) = selection
        .enabled_models
        .iter()
        .find(|id| !listed.contains(id.as_str()))
    {
        return Err(AppError::validation(format!(
            "the scan did not list the model {unknown}"
        )));
    }
    let enabled: HashSet<&str> = selection
        .enabled_models
        .iter()
        .map(String::as_str)
        .collect();
    Ok(report
        .models
        .iter()
        .map(|model| {
            (
                ListedModel {
                    id: model.id.clone(),
                    context: model.context_window,
                    max_output: model.max_output,
                    capabilities: model.capabilities,
                },
                enabled.contains(model.id.as_str()),
            )
        })
        .collect())
}

/// The prices the host's own model list declared (both input and output, USD per million tokens,
/// within the price limits), as `custom` pricing rows without a provider. A price the scan took
/// from the catalog is not the host's and is left out.
fn declared_prices(report: &SniffReport) -> Vec<ModelPricing> {
    report
        .models
        .iter()
        .filter(|model| model.price_declared)
        .filter_map(|model| {
            ModelPricing {
                mode: PricingMode::Custom,
                input_per_mtok: model.input_cost_per_mtok,
                output_per_mtok: model.output_cost_per_mtok,
                ..ModelPricing::dynamic("", &model.id)
            }
            .validated()
            .ok()
        })
        .collect()
}

/// The hand-typed models, trimmed and deduplicated; one the scan listed
/// stays the listed row.
fn manual_models(
    report: &SniffReport,
    selection: &SniffSelection,
    now_ms: i64,
) -> Result<Vec<ProviderModel>, AppError> {
    if selection.manual_models.len() > MAX_MANUAL_MODELS {
        return Err(AppError::validation(format!(
            "at most {MAX_MANUAL_MODELS} models can be typed by hand"
        )));
    }
    let mut seen: HashSet<&str> = report
        .models
        .iter()
        .map(|model| model.id.as_str())
        .collect();
    let mut models = Vec::new();
    for id in &selection.manual_models {
        let id = id.trim();
        if id.is_empty() || id.len() > MAX_MODEL_ID_LEN || id.chars().any(char::is_control) {
            return Err(AppError::validation(
                "a typed model id must be 1 to 256 characters without control characters",
            ));
        }
        if seen.insert(id) {
            models.push(ProviderModel {
                model_id: id.to_owned(),
                source: ModelSource::Manual,
                enabled: true,
                first_seen_ms: Some(now_ms),
                ..ProviderModel::default()
            });
        }
    }
    Ok(models)
}

fn emit_created(state: &AppState, provider: &Provider, with_models: bool) {
    // The first balance reading of the new keys; a provider without keys or a balance
    // adapter is skipped by the refresh itself.
    super::providers::queue_balance_refresh(state, &provider.id);
    state.llm.touch();
    state.sync.notify_changed();
    state.llm.emit(LlmEvent::ProvidersChanged);
    state.llm.emit(LlmEvent::KeysChanged {
        provider_id: provider.id.clone(),
    });
    if with_models {
        state.llm.emit(LlmEvent::ModelsChanged {
            provider_id: provider.id.clone(),
        });
    }
    state.llm.emit(LlmEvent::ProviderStatusChanged {
        provider_id: provider.id.clone(),
    });
}

/// The duplicates of a finished scan among the stored providers: runs after the scan
/// and again on Add. `api_key` is the typed key, compared by SHA-256 against the stored ones.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown or expired `report_id`.
pub fn llm_provider_duplicate_check_impl(
    state: &AppState,
    report_id: &str,
    api_key: Option<&str>,
) -> Result<DuplicateInfo, AppError> {
    let report = cached_report(state, report_id)?;
    with_connection(state, |connection| {
        find_duplicates(
            connection,
            &report.root_url,
            default_mount(&report),
            api_key,
        )
    })
}

/// "Add provider": creates the provider, its key, its models and the scan's measured results from
/// a cached report in ONE transaction. A key typed for a scan that stopped at a
/// look-alike or foreign-key warning is refused until the scan was repeated with "Send key
/// anyway". The key is dropped after the transaction.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown or expired report, [`AppError::Validation`] for a
/// selection that names a protocol, primary or model the scan does not support, or a refused
/// key, [`AppError::Conflict`] carrying a [`DuplicateInfo`] (see
/// [`DuplicateInfo::from_conflict`]) when the address exists and `allow_duplicate` is false.
pub fn llm_provider_create_from_sniff_impl(
    state: &AppState,
    report_id: &str,
    selection: SniffSelection,
    api_key: Option<String>,
    extra: ExtraInput,
    allow_duplicate: bool,
) -> Result<Provider, AppError> {
    let report = cached_report(state, report_id)?;
    let api_key = api_key
        .map(|key| key.trim().to_owned())
        .filter(|key| !key.is_empty());
    if api_key.is_some() && report.needs_key_confirmation {
        return Err(AppError::validation(
            "the key was not sent to this host: confirm \"Send key anyway\" and scan again",
        ));
    }
    let planned = plan_endpoints(&report, &selection)?;
    let now = current_time_ms();
    let models = listed_models(&report, &selection)?;
    let manual = manual_models(&report, &selection, now)?;
    let preset = report
        .suggested_preset_id
        .as_deref()
        .and_then(|id| presets::presets().ok()?.provider(id));
    // The host is the name the app suggests: a second provider on it becomes `host 2`; a name
    // the user typed stays as typed (a clash is a conflict).
    let host = normalize_base_url(&report.root_url).map(|url| url.host);
    let suggested = host.is_ok_and(|host| host.eq_ignore_ascii_case(selection.name.trim()));
    let mut draft = provider_draft::compose(DraftInput {
        preset,
        origin: report.root_url.trim_end_matches('/'),
        mounts: Mounts::Scanned(planned),
        name: Some(selection.name.as_str()),
        category: if is_relay(report.family) {
            "relay"
        } else {
            "custom"
        },
        models_endpoint: report.models_endpoint.clone(),
        secret: api_key.clone(),
        key_name: selection.key_name.as_deref(),
        now_ms: now,
    })?;
    draft.name_typed = !suggested;
    if let Some(proxy) = extra.proxy_url.as_deref().map(str::trim) {
        crate::llm::store::validate_proxy_url(proxy)?;
        proxy.clone_into(&mut draft.provider.proxy_url);
    }
    draft
        .provider
        .quirks
        .extra_headers
        .extend(extra.into_quirk_headers()?);
    let primary_mount = primary_mount(&draft.provider).to_owned();
    let provider = with_connection(state, |connection| {
        if !allow_duplicate {
            let duplicates = find_duplicates(
                connection,
                &report.root_url,
                &primary_mount,
                api_key.as_deref(),
            )?;
            if duplicates.provider.is_some() {
                return Err(duplicates.into_conflict());
            }
        }
        provider_draft::place(connection, &mut draft)?;
        let new = NewProviderFromSniff {
            provider: draft.provider.clone(),
            sniff: sniff_persist(&report, now),
            keys: draft
                .key
                .take()
                .map(|(key, secret)| NewKeyFromSniff {
                    key,
                    secret: Some(secret),
                    balance: seeded_balance(&report),
                    checks: key_checks(&report),
                })
                .into_iter()
                .collect(),
            models,
            manual_models: manual,
            declared_prices: declared_prices(&report),
        };
        store::create_provider_from_sniff(connection, &new, now)
    })?;
    state.llm.services.sniff_cache.remove(report_id);
    emit_created(state, &provider, true);
    Ok(provider)
}

/// What "Add anyway as Unverified" brings besides the address.
#[derive(Debug, Default)]
pub struct UnverifiedExtras<'a> {
    /// The preset the wizard matched or the user picked: its auth, mounts and quirks apply.
    pub preset_id: Option<&'a str>,
    /// The key typed before the scan; saved as the provider's first key.
    pub api_key: Option<String>,
}

/// "Add anyway as Unverified": saves the typed base URL with the chosen protocol (Chat
/// Completions by default), or the preset's endpoints when one is given, plus the typed key; no
/// models and no probe results, so its status reads Unverified.
///
/// # Errors
/// [`AppError::Validation`] for an unusable or always-refused address, [`AppError::NotFound`]
/// for an unknown preset.
pub fn llm_provider_create_unverified_impl(
    state: &AppState,
    typed_url: &str,
    protocol: Option<Protocol>,
    name: Option<&str>,
    extras: UnverifiedExtras<'_>,
) -> Result<Provider, AppError> {
    let url = normalize_base_url(typed_url).map_err(sniff_error)?;
    if url.host_class == UrlHostClass::LinkLocal {
        return Err(sniff_error(SniffError::BlockedAddress {
            host: url.host.clone(),
        }));
    }
    let catalog = presets::presets()?;
    let preset = match extras.preset_id {
        Some(id) => Some(
            catalog
                .provider(id)
                .ok_or_else(|| AppError::not_found(format!("preset {id} not found")))?,
        ),
        None => None,
    };
    let now = current_time_ms();
    let mut draft = provider_draft::compose(DraftInput {
        preset,
        origin: &url.origin,
        mounts: Mounts::Typed {
            protocol: protocol.unwrap_or(Protocol::OpenaiChat),
            mount: url.typed_mount.clone(),
            url_mode: if url.as_typed {
                EndpointUrlMode::AsTyped
            } else {
                EndpointUrlMode::Standard
            },
        },
        name: name.or(preset.is_none().then_some(url.host.as_str())),
        category: "custom",
        models_endpoint: None,
        secret: extras.api_key,
        key_name: None,
        now_ms: now,
    })?;
    // Without a typed name the host (or the preset's name) is one the app picked.
    draft.name_typed = name.map(str::trim).is_some_and(|name| !name.is_empty());
    let provider = with_connection(state, |connection| {
        in_transaction(connection, |connection| {
            provider_draft::place(connection, &mut draft)?;
            provider_draft::save(connection, &draft)
        })
    })?;
    emit_created(state, &provider, false);
    Ok(provider)
}

/// "Add this key to it": adds a key row to an existing provider and nothing else.
///
/// # Errors
/// [`AppError::Validation`] for a blank key, [`AppError::NotFound`] for an unknown provider,
/// [`AppError::Conflict`] when the provider holds this key already.
pub fn llm_provider_add_key_from_sniff_impl(
    state: &AppState,
    provider_id: &str,
    api_key: &str,
    name: Option<&str>,
) -> Result<ProviderKey, AppError> {
    let secret = api_key.trim();
    if secret.is_empty() {
        return Err(AppError::validation("type the key to add"));
    }
    let now = current_time_ms();
    let name = name
        .map(str::trim)
        .filter(|name| !name.is_empty())
        .unwrap_or("Default");
    let key = with_connection(state, |connection| {
        let provider = store::get_provider(connection, provider_id)?
            .ok_or_else(|| crate::models::provider_not_found(provider_id))?;
        let digest = sha256_hex(secret.as_bytes());
        let holds_it = store::list_resolved_keys(connection)?
            .iter()
            .any(|resolved| {
                resolved.key.provider_id == provider.id
                    && sha256_hex(resolved.secret.trim().as_bytes()) == digest
            });
        if holds_it {
            return Err(AppError::conflict(format!(
                "{} already holds this key",
                provider.name
            )));
        }
        // A second "Default" becomes "Default 2"; the store puts the key last.
        let key_name = store::unique_key_name(connection, &provider.id, name)?;
        store::save_key(
            connection,
            &ProviderKey {
                id: new_entity_id("key"),
                provider_id: provider.id,
                name: key_name,
                created_at_ms: now,
                updated_at_ms: now,
                ..ProviderKey::default()
            },
            Some(secret),
            now,
        )
    })?;
    state.llm.touch();
    state.sync.notify_changed();
    super::providers::queue_balance_refresh(state, &key.provider_id);
    state.llm.emit(LlmEvent::KeysChanged {
        provider_id: key.provider_id.clone(),
    });
    state.llm.emit(LlmEvent::ProviderStatusChanged {
        provider_id: key.provider_id.clone(),
    });
    Ok(key)
}

/// "Apply changes" of a re-sniff: writes only the endpoints, the identity (family,
/// quota, evidence, summary; `needs_resniff` is cleared) and the model flags: listed models the
/// provider lacks are saved DISABLED with their first-seen time, stored models the host no longer
/// lists are flagged `not_in_provider`, none is ever deleted. One transaction.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown report or provider, [`AppError::Validation`] for a
/// report that is not a re-sniff of this provider's address or a selection the scan does not
/// support.
pub fn llm_provider_apply_resniff_impl(
    state: &AppState,
    report_id: &str,
    provider_id: &str,
    selection: SniffSelection,
) -> Result<Provider, AppError> {
    let report = cached_report(state, report_id)?;
    if report.diff.is_none() {
        return Err(AppError::validation("this scan is not a re-sniff"));
    }
    let now = current_time_ms();
    let provider = with_connection(state, |connection| {
        let stored = store::get_provider(connection, provider_id)?
            .ok_or_else(|| crate::models::provider_not_found(provider_id))?;
        let own_origin = normalize_base_url(&stored.base_url)
            .map_err(sniff_error)?
            .origin;
        if own_origin != report.root_url {
            return Err(AppError::validation(
                "this scan belongs to another address than the provider's",
            ));
        }
        let selection = SniffSelection {
            primary_protocol: selection.primary_protocol.or(stored.primary_protocol),
            ..selection
        };
        let planned = plan_endpoints(&report, &selection)?;
        // Per-endpoint auth is relative to the provider's own default, which stays as saved. A
        // switched-off endpoint stays off: the scan never re-enables it, and one the
        // selection leaves out keeps its record.
        let was_off = |protocol: Protocol| {
            stored
                .endpoints
                .iter()
                .any(|endpoint| endpoint.protocol == protocol && !endpoint.enabled)
        };
        let mut endpoints: Vec<ProviderEndpoint> = planned
            .endpoints
            .iter()
            .map(|endpoint| {
                let worked = endpoint.auth.unwrap_or(planned.auth);
                ProviderEndpoint {
                    auth: (worked != stored.auth).then_some(worked),
                    enabled: !was_off(endpoint.protocol),
                    ..endpoint.clone()
                }
            })
            .collect();
        let kept_off: Vec<ProviderEndpoint> = stored
            .endpoints
            .iter()
            .filter(|kept| {
                !kept.enabled
                    && !endpoints
                        .iter()
                        .any(|endpoint| endpoint.protocol == kept.protocol)
            })
            .cloned()
            .collect();
        endpoints.extend(kept_off);
        let listed: Vec<ListedModel> = listed_models(&report, &selection)
            .map(|rows| rows.into_iter().map(|(model, _)| model).collect())?;
        in_transaction(connection, |connection| {
            let known: HashSet<String> = store::list_models(connection, Some(provider_id))?
                .into_iter()
                .map(|model| model.model_id)
                .collect();
            store::save_provider(
                connection,
                &Provider {
                    endpoints,
                    primary_protocol: planned.primary,
                    models_endpoint: report
                        .models_endpoint
                        .clone()
                        .or_else(|| stored.models_endpoint.clone()),
                    ..stored.clone()
                },
                now,
            )?;
            let mut persist = sniff_persist(&report, now);
            persist.quota_per_unit = persist.quota_per_unit.or(stored.quota_per_unit);
            persist.display_in_currency =
                persist.display_in_currency.or(stored.display_in_currency);
            store::set_provider_sniff(connection, provider_id, &persist)?;
            if !listed.is_empty() {
                let added: Vec<(ListedModel, bool)> = listed
                    .iter()
                    .filter(|model| !known.contains(&model.id))
                    .map(|model| (model.clone(), false))
                    .collect();
                store::insert_models_from_sniff(connection, provider_id, &added, now)?;
                let complete = !report.warnings.contains(&SniffWarning::BodyTruncated);
                store::apply_model_listing(connection, provider_id, &listed, complete, now)?;
            }
            // A declared price fills in a model the provider has no price row for, and replaces
            // the row the previous scan saved when nobody edited it since: that row is `custom`
            // and still carries the scan's time (the create and every re-scan save their rows
            // with the time they store as `scanned_at_ms`; a user save stamps its own time). Any
            // other row the user saved stays theirs. Rewriting an unchanged declared price keeps
            // it marked as the scan's for the next re-scan.
            let scanned_before = stored
                .sniff_summary
                .as_ref()
                .map(|summary| summary.scanned_at_ms);
            let priced: HashSet<String> = store::list_pricing(connection, None)?
                .into_iter()
                .filter(|row| row.provider_id == provider_id)
                .filter(|row| {
                    !(row.mode == PricingMode::Custom && Some(row.updated_at_ms) == scanned_before)
                })
                .map(|row| row.model_id)
                .collect();
            for declared in declared_prices(&report)
                .into_iter()
                .filter(|declared| !priced.contains(&declared.model_id))
            {
                store::save_pricing(
                    connection,
                    ModelPricing {
                        provider_id: provider_id.to_owned(),
                        ..declared
                    },
                    now,
                )?;
            }
            store::get_provider(connection, provider_id)?
                .ok_or_else(|| AppError::internal("provider vanished after re-sniff"))
        })
    })?;
    state.llm.services.sniff_cache.remove(report_id);
    state.llm.touch();
    state.sync.notify_changed();
    state.llm.emit(LlmEvent::ProvidersChanged);
    state.llm.emit(LlmEvent::ModelsChanged {
        provider_id: provider.id.clone(),
    });
    state.llm.emit(LlmEvent::ProviderStatusChanged {
        provider_id: provider.id.clone(),
    });
    Ok(provider)
}
