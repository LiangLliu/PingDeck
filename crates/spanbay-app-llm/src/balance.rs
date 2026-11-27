//! Balance use-cases: refresh the balance of a provider's keys, test, save and delete the custom
//! balance query, and the automatic refresh task.
//!
//! Single responsibility: wire `llm::balance` (adapters, throttle, keep-last-good policy) to the
//! store, the secret store, the safe transport and the event hub. Credentials are resolved here,
//! passed to the adapters by reference and never enter a DTO, an event or an error message.

use std::collections::BTreeMap;
use std::sync::PoisonError;
use std::time::Duration;

use futures_util::future::join_all;
use serde::{Deserialize, Serialize};

use super::status::llm_status_recompute_impl;
use super::{SAVED_PROVIDER_ALLOW_PRIVATE, safe_http_for, with_connection, with_db_blocking};
use crate::app::{AppState, current_time_ms};
use crate::cancel::{CancelToken, until_cancelled};
use crate::error::AppError;
use crate::llm::balance::{
    BalanceError, BalanceOutcome, BalanceTarget, RefreshInput, RefreshTrigger, StoredBalanceMerge,
    ThrottleVerdict, query_balance, refresh_due, select_adapter, test_custom_query,
};
use crate::llm::balance_state::{
    BalanceErrorKind, BalanceErrorRecord, BalanceReport, CustomBalanceQuery, ResolvedHeaders,
    StoredBalance,
};
use crate::llm::events::LlmEvent;
use crate::llm::safe_http::SafeHttp;
use crate::llm::store::{self, Provider, ResolvedKey};

/// Most HTTP requests one balance check may send (an adapter needs one to three).
const REQUEST_BUDGET: u32 = 4;
/// Keys of one provider that are checked at the same time.
const MAX_CONCURRENT_KEYS: usize = 2;
/// How often the automatic refresh task re-reads the setting and looks for keys that are due.
const SCHEDULER_TICK: Duration = Duration::from_secs(60);

/// What a balance refresh found for one key (`llm_provider_balance`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct KeyBalanceResultDto {
    /// The provider.
    pub provider_id: String,
    /// The key.
    pub key_id: String,
    /// The newest good report (kept when the last refresh failed).
    pub report: Option<BalanceReport>,
    /// Why the last refresh failed; `None` after a success.
    pub error: Option<BalanceErrorRecord>,
    /// Older than one hour, or the last refresh failed.
    pub stale: bool,
    /// The key was checked less than 30 s ago: `report` is the stored one and nothing was sent.
    pub throttled: bool,
    /// Milliseconds until a manual refresh is allowed again; set when `throttled`.
    pub retry_after_ms: Option<u64>,
    /// A request was sent by this call.
    pub refreshed: bool,
    /// A custom query or an adapter applies to the provider; `false` is "No balance API".
    pub has_adapter: bool,
    /// When the balance was last attempted, Unix ms.
    pub attempted_at_ms: Option<i64>,
}

/// The answer of "Test query".
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BalanceQueryTestDto {
    /// HTTP status of the answer; `None` when nothing was sent or no answer arrived.
    pub status: Option<u16>,
    /// The body, redacted, at most 256 KiB.
    pub body: String,
    /// The reading the query would produce.
    pub preview: Option<BalanceReport>,
    /// Why there is no preview, or why nothing was sent.
    pub error: Option<BalanceError>,
}

/// A provider with the credentials a balance check needs.
struct Loaded {
    provider: Provider,
    keys: Vec<ResolvedKey>,
    custom: Option<CustomBalanceQuery>,
}

/// `scheme://host[:port]` of a provider's base URL.
fn origin_of(base_url: &str) -> Result<String, AppError> {
    url::Url::parse(base_url.trim())
        .ok()
        .filter(|url| url.host_str().is_some())
        .map(|url| url.origin().ascii_serialization())
        .ok_or_else(|| AppError::validation("the provider's base URL is not valid"))
}

fn load_in(
    connection: &spanbay_store::DbConnection,
    provider_id: &str,
) -> Result<Option<Loaded>, AppError> {
    let Some(provider) = store::get_provider(connection, provider_id)? else {
        return Ok(None);
    };
    let mut keys = Vec::new();
    for key in store::list_keys(connection, Some(provider_id))? {
        keys.extend(store::get_resolved_key(connection, &key.id)?);
    }
    // A header whose secret went missing still makes the query fail with its own message, so the
    // stored names are kept without values rather than failing the whole refresh.
    let custom = store::resolved_custom_balance(connection, provider_id)
        .unwrap_or_else(|_| provider.custom_balance.clone());
    Ok(Some(Loaded {
        provider,
        keys,
        custom,
    }))
}

async fn load(state: &AppState, provider_id: &str) -> Result<Loaded, AppError> {
    let id = provider_id.to_owned();
    with_db_blocking(state, move |connection| load_in(connection, &id))
        .await?
        .ok_or_else(|| AppError::not_found(format!("provider {provider_id} not found")))
}

/// What one refresh call works with. Holds no transport: whether anything is due is decided
/// before one is built, so a refresh with nothing to send never touches the network setup.
struct Run<'a> {
    state: &'a AppState,
    loaded: &'a Loaded,
    origin: String,
    trigger: RefreshTrigger,
    interval_min: u16,
    cancel: &'a CancelToken,
    now_ms: i64,
}

/// What one key needs after the checks that need no transport.
enum Step {
    /// Answered without a request: the result and whether the stored balance changed.
    Done(Box<KeyBalanceResultDto>, bool),
    /// A request is due.
    Send,
}

impl Run<'_> {
    fn target<'a>(&'a self, key: &'a ResolvedKey, origins: &'a [String]) -> BalanceTarget<'a> {
        let provider = &self.loaded.provider;
        BalanceTarget {
            origin: &self.origin,
            endpoint_origins: origins,
            family: provider.gateway_family,
            preset_id: provider.preset_id.as_deref(),
            api_key: &key.secret,
            management_token: key.management_token.as_deref(),
            api_user_id: key
                .key
                .api_user_id
                .as_deref()
                .filter(|id| !id.trim().is_empty()),
            quota_per_unit: provider.quota_per_unit,
            display_in_currency: provider.display_in_currency,
            allow_private: SAVED_PROVIDER_ALLOW_PRIVATE,
            custom: self.loaded.custom.as_ref(),
            request_budget: REQUEST_BUDGET,
        }
    }

    fn stored(&self, key: &ResolvedKey, has_adapter: bool) -> KeyBalanceResultDto {
        let stored = key.key.balance.clone().unwrap_or_default();
        KeyBalanceResultDto {
            provider_id: self.loaded.provider.id.clone(),
            key_id: key.key.id.clone(),
            stale: stored.is_stale_at(self.now_ms),
            report: stored.last_good,
            error: stored.error,
            throttled: false,
            retry_after_ms: None,
            refreshed: false,
            has_adapter,
            attempted_at_ms: key.key.balance_attempt_ms,
        }
    }

    /// Stores `balance`, tells the webview and returns the key's result.
    async fn finish(
        &self,
        key: &ResolvedKey,
        balance: StoredBalance,
    ) -> Result<KeyBalanceResultDto, AppError> {
        let (key_id, now_ms) = (key.key.id.clone(), self.now_ms);
        let stored = balance.clone();
        with_db_blocking(self.state, move |connection| {
            store::set_key_balance(connection, &key_id, &stored, now_ms)
        })
        .await?;
        self.state.llm.emit(LlmEvent::BalanceUpdated {
            provider_id: self.loaded.provider.id.clone(),
            key_id: key.key.id.clone(),
            report: balance.last_good.clone().unwrap_or_default(),
        });
        Ok(KeyBalanceResultDto {
            provider_id: self.loaded.provider.id.clone(),
            key_id: key.key.id.clone(),
            stale: balance.is_stale_at(self.now_ms),
            report: balance.last_good,
            error: balance.error,
            throttled: false,
            retry_after_ms: None,
            refreshed: true,
            has_adapter: true,
            attempted_at_ms: Some(self.now_ms),
        })
    }

    /// Stores `outcome` for `key` the keep-last-good way and returns the key's result.
    async fn fold(
        &self,
        key: &ResolvedKey,
        outcome: &BalanceOutcome,
    ) -> Result<KeyBalanceResultDto, AppError> {
        let previous = key.key.balance.clone().unwrap_or_default();
        let merged = StoredBalance::merge(&previous, outcome, self.now_ms);
        self.finish(key, merged).await
    }

    /// Everything one key needs that sends nothing: no adapter, not due, throttled.
    async fn prepare(&self, key: &ResolvedKey) -> Result<Step, AppError> {
        let origins = [self.origin.clone()];
        let target = self.target(key, &origins);
        if select_adapter(&target).is_none() {
            // The query or adapter that produced a stored value is gone: drop it.
            return if key.key.balance.is_some() {
                let mut cleared = self.finish(key, StoredBalance::default()).await?;
                cleared.refreshed = false;
                cleared.has_adapter = false;
                Ok(Step::Done(Box::new(cleared), true))
            } else {
                Ok(Step::Done(Box::new(self.stored(key, false)), false))
            };
        }
        let last_error = key.key.balance.as_ref().and_then(|b| b.error.as_ref());
        let due = refresh_due(&RefreshInput {
            trigger: self.trigger,
            now_ms: self.now_ms,
            last_attempt_ms: key.key.balance_attempt_ms,
            interval_min: self.interval_min,
            has_adapter: true,
            enabled: self.loaded.provider.enabled && key.key.enabled,
            last_error: last_error.map(|error| error.kind),
            retry_at_ms: last_error.and_then(|error| error.retry_at_ms),
        });
        if !due {
            return Ok(Step::Done(Box::new(self.stored(key, true)), false));
        }
        let verdict = self
            .state
            .llm
            .services
            .balance_throttle
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .try_acquire(&key.key.id, self.now_ms);
        if let ThrottleVerdict::Throttled { retry_after_ms, .. } = verdict {
            let throttled = KeyBalanceResultDto {
                throttled: true,
                retry_after_ms: Some(u64::try_from(retry_after_ms).unwrap_or(0)),
                ..self.stored(key, true)
            };
            return Ok(Step::Done(Box::new(throttled), false));
        }
        Ok(Step::Send)
    }

    /// Sends the due check of one key. The flag says whether the stored balance changed.
    async fn send(
        &self,
        http: &dyn SafeHttp,
        key: &ResolvedKey,
    ) -> Result<(KeyBalanceResultDto, bool), AppError> {
        let origins = [self.origin.clone()];
        let target = self.target(key, &origins);
        let outcome = query_balance(http, &target, self.cancel, self.now_ms).await;
        if matches!(&outcome, BalanceOutcome::Failed { error, .. } if error.kind == BalanceErrorKind::Cancelled)
        {
            return Ok((self.stored(key, true), false));
        }
        Ok((self.fold(key, &outcome).await?, true))
    }
}

/// The one refresh engine: checks the provider's keys that `trigger` makes due (or only
/// `key_id`), at most two at a time, stores and announces each result. The transport is built
/// only when a key is due; when it cannot be built (an unusable proxy, an unreadable CA file)
/// every due key records that failure instead of losing it. `http` replaces the real transport
/// in tests.
async fn refresh_provider(
    state: &AppState,
    provider_id: &str,
    key_id: Option<&str>,
    trigger: RefreshTrigger,
    cancel: &CancelToken,
    now_ms: i64,
    http: Option<&dyn SafeHttp>,
) -> Result<Vec<KeyBalanceResultDto>, AppError> {
    let mut loaded = load(state, provider_id).await?;
    // "Off": nothing is queried, stored or announced, and it is no error.
    if loaded.provider.balance_off {
        return Ok(Vec::new());
    }
    match key_id {
        Some(id) => {
            loaded.keys.retain(|key| key.key.id == id);
            if loaded.keys.is_empty() {
                return Err(AppError::not_found(format!(
                    "key {id} of provider {provider_id} not found"
                )));
            }
        }
        None => loaded.keys.retain(|key| key.key.enabled),
    }
    let origin = origin_of(&loaded.provider.base_url)?;
    let run = Run {
        state,
        loaded: &loaded,
        origin: origin.clone(),
        trigger,
        interval_min: state.settings().ai.providers.balance_refresh_min,
        cancel,
        now_ms,
    };
    let mut results: Vec<Option<(KeyBalanceResultDto, bool)>> = Vec::new();
    let mut due: Vec<(usize, &ResolvedKey)> = Vec::new();
    for key in &loaded.keys {
        match run.prepare(key).await? {
            Step::Done(dto, changed) => results.push(Some((*dto, changed))),
            Step::Send => {
                due.push((results.len(), key));
                results.push(None);
            }
        }
    }
    if !due.is_empty() {
        let real;
        let built: Result<&dyn SafeHttp, AppError> = match http {
            Some(http) => Ok(http),
            None => match safe_http_for(state, &origin, &loaded.provider.proxy_url) {
                Ok(client) => {
                    real = client;
                    Ok(&real)
                }
                Err(error) => Err(error),
            },
        };
        match built {
            Ok(http) => {
                for batch in due.chunks(MAX_CONCURRENT_KEYS) {
                    let checks = batch.iter().map(|(_, key)| run.send(http, key));
                    for ((index, _), result) in batch.iter().zip(join_all(checks).await) {
                        results[*index] = Some(result?);
                    }
                }
            }
            Err(error) => {
                let outcome = BalanceOutcome::Failed {
                    error: BalanceError::new(
                        BalanceErrorKind::Blocked,
                        format!(
                            "The balance request could not be prepared: {}",
                            error.message()
                        ),
                    ),
                    requests_used: 0,
                };
                for (index, key) in &due {
                    results[*index] = Some((run.fold(key, &outcome).await?, true));
                }
            }
        }
    }
    let results: Vec<(KeyBalanceResultDto, bool)> = results.into_iter().flatten().collect();
    if results.iter().any(|(_, stored)| *stored) {
        llm_status_recompute_impl(state, provider_id).await?;
    }
    Ok(results.into_iter().map(|(dto, _)| dto).collect())
}

/// Refreshes the balance of a provider's enabled keys, or of `key_id` alone.
///
/// `force` is the "Refresh balance" button: every key is checked unless it was checked less than
/// 30 s ago (then its stored report returns with `throttled: true`). Without `force` the
/// Overview-open rule applies: a key is checked only when its last attempt is older than five
/// minutes. Every finished refresh pushes [`LlmEvent::BalanceUpdated`] and recomputes
/// the provider's status. A provider without an adapter returns results with
/// `has_adapter: false` and sends nothing; a provider with `balance_off` returns no results at all.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown provider or key, [`AppError::Validation`] for an unusable
/// proxy URL, [`AppError::Database`] when a result cannot be stored. A failing balance request is
/// not an error: it is the key's `error` record.
pub async fn llm_provider_balance_impl(
    state: &AppState,
    provider_id: &str,
    key_id: Option<String>,
    force: bool,
    cancel_token: Option<String>,
) -> Result<Vec<KeyBalanceResultDto>, AppError> {
    let guard = state.cancellations.register(cancel_token.as_deref())?;
    let trigger = if force {
        RefreshTrigger::Manual
    } else {
        RefreshTrigger::OverviewOpened
    };
    refresh_provider(
        state,
        provider_id,
        key_id.as_deref(),
        trigger,
        guard.token(),
        current_time_ms(),
        None,
    )
    .await
}

/// Refreshes the balance of a provider for a trigger the use-case layer knows: the provider was
/// just added ([`RefreshTrigger::AfterAdd`]), its Overview opened, or the interval
/// elapsed. [`refresh_due`] decides per key.
///
/// # Errors
/// As [`llm_provider_balance_impl`].
pub async fn llm_balance_refresh_if_due_impl(
    state: &AppState,
    provider_id: &str,
    trigger: RefreshTrigger,
    cancel: &CancelToken,
) -> Result<Vec<KeyBalanceResultDto>, AppError> {
    refresh_provider(
        state,
        provider_id,
        None,
        trigger,
        cancel,
        current_time_ms(),
        None,
    )
    .await
}

// ------------------------------------------------------------- custom query

fn find_header<'a>(headers: &'a ResolvedHeaders, name: &str) -> Option<&'a str> {
    headers
        .0
        .iter()
        .find(|(own, _)| own.eq_ignore_ascii_case(name))
        .map(|(_, value)| value.as_str())
}

/// Performs a draft custom balance query once and returns the redacted body, the status and the
/// reading it would produce. `header_values` holds the header values typed in the form;
/// a header without one uses the value stored with the saved query. Nothing is saved.
///
/// The key of `key_id` (else the provider's first enabled key) is injected as the query's auth
/// style. A refused or failed request is `error`, not an `Err`.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown provider or key, [`AppError::Validation`] for an unusable
/// proxy URL.
pub async fn llm_balance_query_test_impl(
    state: &AppState,
    provider_id: &str,
    key_id: Option<String>,
    query: CustomBalanceQuery,
    header_values: BTreeMap<String, String>,
) -> Result<BalanceQueryTestDto, AppError> {
    query_test(state, provider_id, key_id, query, header_values, None).await
}

/// [`llm_balance_query_test_impl`] over `http` (tests) or the provider's real transport.
async fn query_test(
    state: &AppState,
    provider_id: &str,
    key_id: Option<String>,
    mut query: CustomBalanceQuery,
    header_values: BTreeMap<String, String>,
    http: Option<&dyn SafeHttp>,
) -> Result<BalanceQueryTestDto, AppError> {
    let loaded = load(state, provider_id).await?;
    let key = match &key_id {
        Some(id) => Some(
            loaded
                .keys
                .iter()
                .find(|key| key.key.id == *id)
                .ok_or_else(|| AppError::not_found(format!("key {id} not found")))?,
        ),
        None => loaded.keys.iter().find(|key| key.key.enabled),
    };
    let saved = loaded
        .custom
        .as_ref()
        .map(|saved| &saved.resolved_headers)
        .cloned()
        .unwrap_or_default();
    query.resolved_headers = ResolvedHeaders(
        query
            .headers
            .iter()
            .filter_map(|header| {
                let value = header_values
                    .get(&header.name)
                    .map(String::as_str)
                    .or_else(|| find_header(&saved, &header.name))?;
                Some((header.name.clone(), value.to_owned()))
            })
            .collect(),
    );
    let origin = origin_of(&loaded.provider.base_url)?;
    let real;
    let http = match http {
        Some(http) => http,
        None => {
            real = safe_http_for(state, &origin, &loaded.provider.proxy_url)?;
            &real
        }
    };
    let origins = [origin.clone()];
    let provider = &loaded.provider;
    let target = BalanceTarget {
        origin: &origin,
        endpoint_origins: &origins,
        family: provider.gateway_family,
        preset_id: provider.preset_id.as_deref(),
        api_key: key.map_or("", |key| key.secret.as_str()),
        management_token: None,
        api_user_id: None,
        quota_per_unit: provider.quota_per_unit,
        display_in_currency: provider.display_in_currency,
        allow_private: SAVED_PROVIDER_ALLOW_PRIVATE,
        custom: None,
        request_budget: 1,
    };
    let cancel = CancelToken::new();
    Ok(
        match test_custom_query(http, &target, &query, &cancel, current_time_ms()).await {
            Ok(test) => BalanceQueryTestDto {
                status: Some(test.status),
                body: test.body,
                preview: test.preview,
                error: test.preview_error,
            },
            Err(error) => BalanceQueryTestDto {
                status: None,
                body: String::new(),
                preview: None,
                error: Some(error),
            },
        },
    )
}

/// Saves the custom balance query of a provider: it wins over the adapter from
/// now on. The scope is required; a URL on another host than the provider's needs
/// `confirm_cross_host`, and the confirmation is stored with the query. `header_values` go to the
/// secret store; a header without a value keeps the stored one.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown provider, [`AppError::Validation`] for an invalid query
/// (no scope, bad URL or path, forbidden header, header without a value) and
/// [`AppError::ConfirmationRequired`] for an unconfirmed other host.
pub fn llm_balance_custom_save_impl(
    state: &AppState,
    provider_id: &str,
    mut query: CustomBalanceQuery,
    header_values: &BTreeMap<String, String>,
    confirm_cross_host: bool,
) -> Result<Provider, AppError> {
    query.cross_host = confirm_cross_host;
    query.resolved_headers = ResolvedHeaders::default();
    store_custom(state, provider_id, Some(&query), header_values)
}

/// Removes the custom balance query and its header secrets; the adapter of the provider (or "No
/// balance API") applies again.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown provider.
pub fn llm_balance_custom_delete_impl(
    state: &AppState,
    provider_id: &str,
) -> Result<Provider, AppError> {
    store_custom(state, provider_id, None, &BTreeMap::new())
}

fn store_custom(
    state: &AppState,
    provider_id: &str,
    query: Option<&CustomBalanceQuery>,
    header_values: &BTreeMap<String, String>,
) -> Result<Provider, AppError> {
    let provider = with_connection(state, |connection| {
        store::set_custom_balance(
            connection,
            provider_id,
            query,
            header_values,
            current_time_ms(),
        )?;
        store::get_provider(connection, provider_id)?
            .ok_or_else(|| AppError::not_found(format!("provider {provider_id} not found")))
    })?;
    state.llm.touch();
    state.llm.emit(LlmEvent::ProvidersChanged);
    Ok(provider)
}

// ---------------------------------------------------------------- scheduler

/// The token a background job of the LLM use cases runs on: a child of the background token
/// (stopped with [`stop_balance_scheduler`] at shutdown), or a token of its own before the
/// background tasks started.
pub(crate) fn background_token(state: &AppState) -> CancelToken {
    state
        .llm
        .services
        .balance_scheduler
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .as_ref()
        .filter(|token| !token.is_cancelled())
        .map_or_else(CancelToken::new, CancelToken::child)
}

/// A running background job, by name; the name is free again when this drops.
pub(crate) struct Flight {
    state: AppState,
    name: String,
}

impl Drop for Flight {
    fn drop(&mut self) {
        self.state
            .llm
            .services
            .background_flights
            .lock()
            .unwrap_or_else(PoisonError::into_inner)
            .remove(&self.name);
    }
}

/// Claims the job `name`; `None` while another copy of it runs. (Balance refreshes need no name:
/// the per-key throttle already keeps two of them from calling the same key.)
pub(crate) fn begin_flight(state: &AppState, name: String) -> Option<Flight> {
    let claimed = state
        .llm
        .services
        .background_flights
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .insert(name.clone());
    claimed.then(|| Flight {
        state: state.clone(),
        name,
    })
}

/// One pass of the automatic refresh: when `ai.providers.balance_refresh_min` is on,
/// every enabled provider without `balance_off` is offered to [`refresh_due`] with the interval
/// trigger, key by key (the policy backs off by the last failure).
/// Returns how many keys were queried. A provider that fails to refresh (deleted meanwhile) does
/// not stop the pass, and a transport that cannot be built is stored as the keys' error record.
async fn tick(
    state: &AppState,
    now_ms: i64,
    cancel: &CancelToken,
    http: Option<&dyn SafeHttp>,
) -> Result<usize, AppError> {
    if state.settings().ai.providers.balance_refresh_min == 0 {
        return Ok(0);
    }
    let providers = with_db_blocking(state, store::list_providers).await?;
    let mut queried = 0;
    for provider in providers
        .iter()
        .filter(|provider| provider.enabled && !provider.balance_off)
    {
        if cancel.is_cancelled() {
            break;
        }
        if let Ok(results) = refresh_provider(
            state,
            &provider.id,
            None,
            RefreshTrigger::Interval,
            cancel,
            now_ms,
            http,
        )
        .await
        {
            queried += results.iter().filter(|dto| dto.refreshed).count();
        }
    }
    Ok(queried)
}

/// Starts the automatic balance refresh: one task that wakes every minute, re-reads
/// `ai.providers.balance_refresh_min` (Off by default, 5 to 1440 minutes, so a settings change
/// applies within a minute) and refreshes what is due. Starting twice keeps the one task. Call it
/// from async context after the state is opened; [`stop_balance_scheduler`] ends it
/// (`AppState::shutdown` must call that).
///
/// # Errors
/// [`AppError::Internal`] when there is no Tokio runtime to spawn on.
pub fn start_balance_scheduler(state: &AppState) -> Result<(), AppError> {
    let runtime = tokio::runtime::Handle::try_current()
        .map_err(|_| AppError::internal("the balance scheduler needs a Tokio runtime"))?;
    let token = {
        let mut slot = state
            .llm
            .services
            .balance_scheduler
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        if slot.as_ref().is_some_and(|token| !token.is_cancelled()) {
            return Ok(());
        }
        let token = CancelToken::new();
        *slot = Some(token.clone());
        token
    };
    let state = state.clone();
    runtime.spawn(async move {
        while until_cancelled(Some(&token), tokio::time::sleep(SCHEDULER_TICK))
            .await
            .is_some()
        {
            // Failures are stored per key; a database error here retries at the next tick.
            let _ = tick(&state, current_time_ms(), &token, None).await;
        }
    });
    Ok(())
}

/// Stops the automatic balance refresh task, if it runs.
pub fn stop_balance_scheduler(state: &AppState) {
    if let Some(token) = state
        .llm
        .services
        .balance_scheduler
        .lock()
        .unwrap_or_else(PoisonError::into_inner)
        .take()
    {
        token.cancel();
    }
}

#[cfg(test)]
mod custom_tests;
#[cfg(test)]
mod tests;
