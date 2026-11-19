//! The provider sniff use cases of the Add-provider wizard: address normaliser, endpoint preview
//! and the scan with its report cache.
//!
//! Single responsibility: glue `llm::sniff` to the app: pick the transport, resolve what a
//! re-sniff of a saved provider needs (its stored key stays inside core), register the cancel
//! token, map sniff errors, and keep the finished report in the cache. A scan writes nothing to
//! the database: a cancelled or discarded scan leaves no trace.

use std::collections::BTreeMap;

use pingdeck_store::DbConnection as Connection;
use serde::{Deserialize, Serialize};

use super::{AppState, ensure_catalog, safe_http_for, with_db_blocking};
pub(super) use crate::app::sniff_runner::{SniffScanParams, run_sniff_scan, sniff_error};
use crate::error::AppError;
use crate::events::EventSink;
use crate::llm::balance::{BalanceTarget, select_adapter};
use crate::llm::catalog::CatalogIndex;
use crate::llm::safe_http::SafeHttp;
use crate::llm::sniff::{
    SniffEvent, SniffInput, SniffReport, SniffWarning, StoredProviderView, UrlHostClass,
    normalize_base_url,
};
use crate::llm::store::{self, Provider};
use crate::llm::wire::{EndpointUrlMode, Protocol, endpoint_url};

/// The address the user typed, normalised by core, for the wizard's live
/// hints. Pure: nothing is requested.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct NormalizedUrlDto {
    /// `origin + mount` (+ `#` when typed as is); normalising it again returns it.
    pub typed: String,
    /// `scheme://host[:port]`.
    pub origin: String,
    /// Lower-case punycode host.
    pub host: String,
    /// The explicit non-default port.
    pub port: Option<u16>,
    /// The path after a pasted operation tail was stripped.
    pub mount: String,
    /// The protocol of a pasted operation tail (`/chat/completions`, `/messages`, ...).
    pub tail_protocol: Option<Protocol>,
    /// The model id of a pasted Gemini `models/{id}:action` tail.
    pub model_hint: Option<String>,
    /// A trailing `#`: the typed path is used as is.
    pub as_typed: bool,
    /// Loopback, private, CGNAT, link-local or public.
    pub host_class: UrlHostClass,
    /// The preset that owns the host, when it is a known provider host.
    pub preset_id: Option<String>,
    /// The preset region that owns the host.
    pub region_id: Option<String>,
    /// Dropped userinfo or key, plain http, a cut dashboard path.
    pub warnings: Vec<SniffWarning>,
}

/// Normalises a typed address (pure).
///
/// # Errors
/// [`AppError::Validation`] with the code `invalid_input: <reason>` for an unusable address.
pub fn llm_url_normalize_impl(input: &str) -> Result<NormalizedUrlDto, AppError> {
    let url = normalize_base_url(input).map_err(sniff_error)?;
    Ok(NormalizedUrlDto {
        typed: url.typed,
        origin: url.origin,
        host: url.host,
        port: url.port,
        mount: url.typed_mount,
        tail_protocol: url.tail_hint,
        model_hint: url.model_hint,
        as_typed: url.as_typed,
        host_class: url.host_class,
        preset_id: url.known_host.as_ref().map(|known| known.preset_id.clone()),
        region_id: url.known_host.map(|known| known.region_id),
        warnings: url.warnings,
    })
}

/// The request URL a protocol row of the wizard will use ("Base URL we will store" column):
/// Gemini shows the literal `{model}`.
///
/// # Errors
/// [`AppError::Validation`] never in practice (Gemini always gets its placeholder); kept for the
/// contract's `Result`.
pub fn llm_endpoint_preview_impl(
    base_url: &str,
    path_prefix: &str,
    url_mode: EndpointUrlMode,
    protocol: Protocol,
) -> Result<String, AppError> {
    let model = (protocol == Protocol::Gemini).then_some("{model}");
    endpoint_url(base_url, path_prefix, url_mode, protocol, model, false)
}

/// Fills the credentials a re-sniff of `input.provider_id` takes from the secret store (the
/// stored key, console token, user id, extra headers) where the input has none, and returns the
/// view to diff against. The origin must be the provider's own: a stored key is never sent to a
/// host the user merely typed. Nothing here leaves core.
fn resolve_stored(
    connection: &Connection,
    input: &mut SniffInput,
    provider_id: &str,
) -> Result<StoredProviderView, AppError> {
    let provider = store::get_provider(connection, provider_id)?
        .ok_or_else(|| AppError::not_found(format!("provider {provider_id} not found")))?;
    if input.base_url.trim().is_empty() {
        input.base_url.clone_from(&provider.base_url);
    }
    let typed = normalize_base_url(&input.base_url).map_err(sniff_error)?;
    let own = normalize_base_url(&provider.base_url).map_err(sniff_error)?;
    if typed.origin != own.origin {
        return Err(AppError::validation(
            "a re-sniff must scan the provider's own address",
        ));
    }
    if input.preset_id.is_none() {
        input.preset_id.clone_from(&provider.preset_id);
    }
    let keys = store::list_resolved_keys(connection)?;
    let key = keys.iter().find(|resolved| {
        resolved.key.provider_id == provider_id
            && resolved.key.enabled
            && !resolved.secret.trim().is_empty()
    });
    if let Some(resolved) = key {
        if input.key().is_none() {
            input.api_key = Some(resolved.secret.clone());
        }
        if input.management_token.is_none() {
            input
                .management_token
                .clone_from(&resolved.management_token);
        }
        if input.api_user_id.is_none() {
            input.api_user_id.clone_from(&resolved.key.api_user_id);
        }
    }
    if input.extra_headers.is_empty() {
        for (name, value) in store::resolve_provider_headers(connection, &provider)? {
            if name.eq_ignore_ascii_case("user-agent") {
                input.user_agent.get_or_insert(value);
            } else {
                input.extra_headers.push((name, value));
            }
        }
        input.extra_headers.truncate(8);
    }
    let models = store::list_models(connection, Some(provider_id))?;
    let balance_adapter = stored_balance_adapter(&provider, &typed.origin);
    Ok(StoredProviderView {
        family: provider.gateway_family,
        brand_name: provider
            .sniff_summary
            .as_ref()
            .and_then(|summary| summary.brand_name.clone()),
        balance_adapter,
        protocols: provider
            .endpoints
            .iter()
            .map(|endpoint| endpoint.protocol)
            .collect(),
        model_ids: models.into_iter().map(|model| model.model_id).collect(),
    })
}

/// The balance adapter the stored provider would use (the sniffer's `balance_adapter` of the
/// same inputs), so a re-sniff reports a change only when the choice really changed.
fn stored_balance_adapter(provider: &Provider, origin: &str) -> Option<String> {
    let origins = [origin.to_owned()];
    let target = BalanceTarget {
        origin,
        endpoint_origins: &origins,
        family: provider.gateway_family,
        preset_id: provider.preset_id.as_deref(),
        api_key: "",
        management_token: None,
        api_user_id: None,
        quota_per_unit: provider.quota_per_unit,
        display_in_currency: provider.display_in_currency,
        allow_private: false,
        custom: provider.custom_balance.as_ref(),
        request_budget: 0,
    };
    select_adapter(&target).map(|adapter| adapter.as_str().to_owned())
}

/// Scans the typed address and caches the report (8 reports, 10 minutes) for the Add step.
///
/// The transport is the provider's proxy, else the global proxy unless the host is on
/// `network.no_proxy` or is a loopback/private address, else direct. With `input.provider_id`
/// the stored key, console token and headers of that provider are used and the report carries
/// the changes since the last scan; the secrets never leave core. Progress goes to
/// `sink`. Nothing is written to the database.
///
/// # Errors
/// [`AppError::Validation`] for an unusable or always-refused address (`invalid_input`,
/// `blocked_address` in the message), [`AppError::NotFound`] for an unknown `provider_id`,
/// [`AppError::ConfirmationRequired`] for a public name that resolves privately,
/// [`AppError::Network`] when nothing answered, [`AppError::Cancelled`].
pub async fn llm_provider_sniff_impl(
    state: &AppState,
    input: SniffInput,
    cancel_token: Option<String>,
    sink: &dyn EventSink<SniffEvent>,
) -> Result<SniffReport, AppError> {
    let proxy_url = match input.provider_id.clone() {
        Some(id) => {
            with_db_blocking(state, move |connection| {
                store::get_provider(connection, &id)?
                    .map(|provider| provider.proxy_url)
                    .ok_or_else(|| AppError::not_found(format!("provider {id} not found")))
            })
            .await?
        }
        None => input.proxy_url.clone().unwrap_or_default(),
    };
    let origin = match input.base_url.trim() {
        "" => String::new(),
        typed => normalize_base_url(typed).map_err(sniff_error)?.origin,
    };
    let http = safe_http_for(state, &origin, &proxy_url)?;
    llm_provider_sniff_in(state, &http, http.uses_proxy(), input, cancel_token, sink).await
}

/// [`llm_provider_sniff_impl`] over an explicit transport (tests script it).
pub(crate) async fn llm_provider_sniff_in(
    state: &AppState,
    http: &dyn SafeHttp,
    uses_proxy: bool,
    mut input: SniffInput,
    cancel_token: Option<String>,
    sink: &dyn EventSink<SniffEvent>,
) -> Result<SniffReport, AppError> {
    let provider_id = input.provider_id.clone();
    let (input, stored, index) = with_db_blocking(state, move |connection| {
        ensure_catalog(connection)?;
        let stored = provider_id
            .map(|id| resolve_stored(connection, &mut input, &id))
            .transpose()?;
        Ok((input, stored, CatalogIndex::load(connection)?))
    })
    .await?;
    run_sniff_scan(
        state,
        http,
        SniffScanParams {
            input,
            stored,
            index,
            uses_proxy,
            cancel_token,
        },
        sink,
    )
    .await
}

/// The extra headers of a sniff or create call, by name (values are secrets: never printed).
#[derive(Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct ExtraInput {
    /// Up to 8 `(name, value)` pairs; values go to the secret store.
    #[serde(default)]
    pub headers: Vec<(String, String)>,
    /// `User-Agent` override.
    #[serde(default)]
    pub user_agent: Option<String>,
    /// The new provider's own proxy (a URL or `direct`); absent follows Settings › Network.
    #[serde(default)]
    pub proxy_url: Option<String>,
}

/// Prints header names only.
impl std::fmt::Debug for ExtraInput {
    fn fmt(&self, f: &mut std::fmt::Formatter<'_>) -> std::fmt::Result {
        f.debug_struct("ExtraInput")
            .field(
                "header_names",
                &self
                    .headers
                    .iter()
                    .map(|(name, _)| name)
                    .collect::<Vec<_>>(),
            )
            .field("user_agent", &self.user_agent.as_ref().map(|_| "***"))
            .finish()
    }
}

impl ExtraInput {
    /// The headers as the quirks of a new provider hold them (plaintext until the store moves
    /// the values to the secret store): the pairs plus the user agent as `user-agent`.
    pub(super) fn into_quirk_headers(self) -> Result<BTreeMap<String, String>, AppError> {
        if self.headers.len() > 8 {
            return Err(AppError::validation("at most 8 extra headers are allowed"));
        }
        let mut headers = BTreeMap::new();
        for (name, value) in self.headers {
            let name = name.trim().to_ascii_lowercase();
            if name.is_empty() || headers.insert(name, value).is_some() {
                return Err(AppError::validation(
                    "extra header names must be unique and not blank",
                ));
            }
        }
        if let Some(agent) = self
            .user_agent
            .map(|agent| agent.trim().to_owned())
            .filter(|agent| !agent.is_empty())
        {
            headers.insert("user-agent".to_owned(), agent);
        }
        Ok(headers)
    }
}

#[cfg(test)]
mod tests {
    //! Transport selection of the sniff use cases.

    use serde_json::json;

    use super::*;
    use crate::app::settings_update_impl;
    use crate::settings::SettingsSection;

    fn state(proxy: &str, no_proxy: &[&str]) -> AppState {
        let state = AppState::open_in_memory().expect("state");
        settings_update_impl(
            &state,
            SettingsSection::Network,
            &json!({"proxy_url": proxy, "no_proxy": no_proxy}),
        )
        .expect("settings");
        state
    }

    fn uses_proxy(state: &AppState, origin: &str, provider_proxy: &str) -> bool {
        safe_http_for(state, origin, provider_proxy)
            .expect("transport")
            .uses_proxy()
    }

    // An empty Settings proxy is the system proxy (Clash, Surge); `direct` is none.
    #[test]
    fn the_scan_follows_the_system_proxy_unless_settings_say_none() {
        assert!(uses_proxy(&state("", &[]), "https://relay.acme.dev", ""));
        assert!(!uses_proxy(
            &state("direct", &[]),
            "https://relay.acme.dev",
            ""
        ));
        assert!(!uses_proxy(
            &state("", &[]),
            "https://relay.acme.dev",
            "direct"
        ));
    }

    #[test]
    fn the_global_proxy_serves_remote_hosts() {
        let state = state("http://proxy.test:3128", &[]);
        assert!(uses_proxy(&state, "https://relay.acme.dev", ""));
    }

    #[test]
    fn a_no_proxy_host_goes_direct() {
        let state = state("http://proxy.test:3128", &["relay.acme.dev"]);
        assert!(!uses_proxy(&state, "https://relay.acme.dev", ""));
        assert!(uses_proxy(&state, "https://other.acme.dev", ""));
    }

    #[test]
    fn loopback_and_private_hosts_skip_the_global_proxy() {
        let state = state("http://proxy.test:3128", &[]);
        for origin in [
            "http://localhost:11434",
            "http://127.0.0.1:8080",
            "http://192.168.1.20:8080",
        ] {
            assert!(!uses_proxy(&state, origin, ""), "{origin}");
        }
    }

    #[test]
    fn the_providers_own_proxy_wins_even_for_local_hosts() {
        let state = state("", &[]);
        assert!(uses_proxy(
            &state,
            "http://localhost:11434",
            "socks5://proxy.test:1080"
        ));
        assert!(uses_proxy(
            &state,
            "https://relay.acme.dev",
            "http://proxy.test:3128"
        ));
    }

    #[test]
    fn an_unusable_provider_proxy_is_a_validation_error() {
        let state = state("", &[]);
        let error =
            safe_http_for(&state, "https://relay.acme.dev", "not a proxy").expect_err("bad");
        assert!(matches!(error, AppError::Validation { .. }));
    }
}
