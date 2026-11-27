//! Provider status and health use-cases: the one word on every card, the Overview health card and
//! the push event when either changes.
//!
//! Single responsibility: gather the stored evidence ([`store::provider_status_facts`]), merge the
//! in-memory circuit book into it and hand it to the pure [`provider_status`]; remember the last
//! status per provider so a recompute can tell a change and push
//! [`LlmEvent::ProviderStatusChanged`]. The webview never re-derives a status.

use std::collections::{HashMap, HashSet};
use std::sync::PoisonError;

use serde::{Deserialize, Serialize};

use super::with_db_blocking;
use crate::app::{AppState, current_time_ms};
use crate::error::AppError;
use crate::llm::capture_record::TimingJson;
use crate::llm::events::LlmEvent;
use crate::llm::health::{CircuitState, HealthSnapshot};
use crate::llm::status::{PairHealth, ProviderStatusDto, provider_status};
use crate::llm::store::{self, ProviderLogStats};

/// Requests the 24 h figures need before they are shown (fewer reads "Not enough
/// requests yet").
pub const MIN_HEALTH_REQUESTS: u64 = 10;

/// The Overview health card of one provider, "Last 24 h" (`llm_provider_health`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderHealthDto {
    /// The provider.
    pub provider_id: String,
    /// Forwarded requests of the window (gateway, agent and api sources).
    pub requests: u64,
    /// `false` below [`MIN_HEALTH_REQUESTS`]: the card reads "Not enough requests yet" and
    /// `success_rate`, `ttft_p50_ms` and `ttft_p95_ms` are `None`.
    pub enough_requests: bool,
    /// 2xx requests over all forwarded requests, 0 to 1.
    pub success_rate: Option<f64>,
    /// TTFT p50 of the successful streamed requests.
    pub ttft_p50_ms: Option<u64>,
    /// TTFT p95 of the successful streamed requests.
    pub ttft_p95_ms: Option<u64>,
    /// Requests answered 429 per hour, oldest first: the last entry is the most recent hour.
    pub rate_limited_by_hour: Vec<u32>,
    /// The median of the last 20 successful requests per phase.
    pub typical_request: TypicalRequestDto,
    /// Circuits of this provider's pairs that are open or half-open.
    pub open_circuits: Vec<OpenCircuitDto>,
}

/// The median wire strip of the last successful requests. A phase no sample
/// measured (reused connection, not measured) is `None` and is not drawn.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct TypicalRequestDto {
    /// Requests the medians are taken over.
    pub samples: u32,
    /// Queue: routing plus the attempts that failed before the serving one.
    pub queue_ms: Option<u64>,
    /// Connect and TLS, for requests that opened their own connection.
    pub connect_ms: Option<u64>,
    /// Wait: connection ready to the first response content.
    pub wait_ms: Option<u64>,
    /// Receive: the rest of the response.
    pub receive_ms: Option<u64>,
}

/// A circuit that is not closed.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct OpenCircuitDto {
    /// Key of the pair.
    pub key_id: String,
    /// The key's display name.
    pub key_name: String,
    /// Model of the pair.
    pub model_id: String,
    /// `open` or `half_open`.
    pub state: CircuitState,
    /// Whole seconds until a probe request is let through (at least 1); `None` when half-open.
    pub retry_in_s: Option<u64>,
}

/// The statuses of every provider, in display order.
fn compute_all(
    connection: &spanbay_store::DbConnection,
    health: &[HealthSnapshot],
    now_ms: i64,
) -> Result<Vec<ProviderStatusDto>, AppError> {
    let mut inputs = store::provider_status_facts(connection, now_ms)?;
    let keys = store::list_keys(connection, None)?;
    let enabled_keys: HashMap<&str, &str> = keys
        .iter()
        .filter(|key| key.enabled)
        .map(|key| (key.id.as_str(), key.provider_id.as_str()))
        .collect();
    let models = store::list_models(connection, None)?;
    let enabled_models: HashSet<(&str, &str)> = models
        .iter()
        .filter(|model| model.enabled)
        .map(|model| (model.provider_id.as_str(), model.model_id.as_str()))
        .collect();
    for snapshot in health {
        let Some(provider_id) = enabled_keys.get(snapshot.key_id.as_str()) else {
            continue;
        };
        if !enabled_models.contains(&(*provider_id, snapshot.model_id.as_str())) {
            continue;
        }
        let Some(input) = inputs
            .iter_mut()
            .find(|input| input.provider_id == *provider_id)
        else {
            continue;
        };
        if snapshot.state != CircuitState::Closed {
            input.open_circuits += 1;
        }
        input.pairs.push(PairHealth {
            requests: u32::try_from(snapshot.total).unwrap_or(u32::MAX),
            failure_rate: snapshot.failure_rate,
        });
    }
    Ok(inputs
        .iter()
        .map(|input| provider_status(input, now_ms))
        .collect())
}

async fn statuses(state: &AppState, now_ms: i64) -> Result<Vec<ProviderStatusDto>, AppError> {
    let health = state.llm.health.snapshot();
    with_db_blocking(state, move |connection| {
        compute_all(connection, &health, now_ms)
    })
    .await
}

/// The status of every provider, in display order: stored probe, sniff, balance and
/// gateway evidence plus the live circuits. The result becomes the baseline
/// [`llm_status_recompute_impl`] compares with.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub async fn llm_provider_status_list_impl(
    state: &AppState,
) -> Result<Vec<ProviderStatusDto>, AppError> {
    let list = statuses(state, current_time_ms()).await?;
    *state
        .llm
        .services
        .provider_status
        .lock()
        .unwrap_or_else(PoisonError::into_inner) = list
        .iter()
        .map(|status| (status.provider_id.clone(), status.clone()))
        .collect();
    Ok(list)
}

/// Recomputes one provider's status and pushes [`LlmEvent::ProviderStatusChanged`] when it
/// differs from the last one computed (or when there is none yet). Call it after anything that
/// changes the evidence: probe results, a sniff, a balance refresh, a gateway health change.
///
/// Computing reads the evidence of every provider (the facts are gathered in one pass); it is
/// cheap enough to call after each such event.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub async fn llm_status_recompute_impl(
    state: &AppState,
    provider_id: &str,
) -> Result<(), AppError> {
    let current = statuses(state, current_time_ms())
        .await?
        .into_iter()
        .find(|status| status.provider_id == provider_id);
    let changed = {
        let mut known = state
            .llm
            .services
            .provider_status
            .lock()
            .unwrap_or_else(PoisonError::into_inner);
        match current {
            Some(status) => known.insert(provider_id.to_owned(), status.clone()) != Some(status),
            None => known.remove(provider_id).is_some(),
        }
    };
    if changed {
        state.llm.emit(LlmEvent::ProviderStatusChanged {
            provider_id: provider_id.to_owned(),
        });
    }
    Ok(())
}

/// The median of `values` (the upper middle for an even count); `None` when empty.
fn median(mut values: Vec<u64>) -> Option<u64> {
    values.sort_unstable();
    values.get(values.len() / 2).copied()
}

/// The nearest-rank percentile (`0 < percent <= 100`) of `values`; `None` when empty.
fn percentile(mut values: Vec<u64>, percent: usize) -> Option<u64> {
    values.sort_unstable();
    let rank = (values.len() * percent).div_ceil(100).max(1);
    values.get(rank - 1).copied()
}

fn typical_request(timings: &[TimingJson]) -> TypicalRequestDto {
    let mut queue = Vec::new();
    let mut connect = Vec::new();
    let mut wait = Vec::new();
    let mut receive = Vec::new();
    for timing in timings {
        let Some((serving, failed)) = timing.attempts.split_last() else {
            continue;
        };
        queue.push(
            timing.queue_ms
                + failed
                    .iter()
                    .filter_map(|attempt| attempt.end_ms.ms())
                    .sum::<u64>(),
        );
        if !serving.reused
            && [serving.dns, serving.connect, serving.tls]
                .iter()
                .any(|p| p.ms().is_some())
        {
            connect.push(serving.setup_ms());
        }
        if let Some(headers) = serving.headers_ms.ms() {
            wait.push(headers + serving.first_token_ms.ms().unwrap_or(0));
        }
        receive.extend(serving.receive_ms());
    }
    TypicalRequestDto {
        samples: u32::try_from(timings.len()).unwrap_or(u32::MAX),
        queue_ms: median(queue),
        connect_ms: median(connect),
        wait_ms: median(wait),
        receive_ms: median(receive),
    }
}

#[allow(clippy::cast_precision_loss)]
fn health_dto(
    provider_id: &str,
    stats: ProviderLogStats,
    circuits: Vec<OpenCircuitDto>,
) -> ProviderHealthDto {
    let enough = stats.requests >= MIN_HEALTH_REQUESTS;
    let mut rate_limited = stats.rate_limited.to_vec();
    rate_limited.reverse();
    ProviderHealthDto {
        provider_id: provider_id.to_owned(),
        requests: stats.requests,
        enough_requests: enough,
        success_rate: enough.then(|| stats.succeeded as f64 / stats.requests as f64),
        ttft_p50_ms: enough.then(|| median(stats.ttft_ms.clone())).flatten(),
        ttft_p95_ms: enough.then(|| percentile(stats.ttft_ms, 95)).flatten(),
        rate_limited_by_hour: rate_limited,
        typical_request: typical_request(&stats.timings),
        open_circuits: circuits,
    }
}

/// The Overview health card of a provider: success rate, TTFT p50 and p95, the 429
/// timeline, the typical request and the circuits that are not closed, over the last 24 hours.
///
/// # Errors
/// [`AppError::NotFound`] for an unknown provider, [`AppError::Database`] on SQLite failure.
pub async fn llm_provider_health_impl(
    state: &AppState,
    provider_id: &str,
) -> Result<ProviderHealthDto, AppError> {
    let id = provider_id.to_owned();
    let now_ms = current_time_ms();
    let (stats, keys) = with_db_blocking(state, move |connection| {
        if store::get_provider(connection, &id)?.is_none() {
            return Err(AppError::not_found(format!("provider {id} not found")));
        }
        Ok((
            store::provider_log_stats(connection, &id, now_ms)?,
            store::list_keys(connection, Some(&id))?,
        ))
    })
    .await?;
    let circuits = state
        .llm
        .health
        .snapshot()
        .into_iter()
        .filter(|snapshot| snapshot.state != CircuitState::Closed)
        .filter_map(|snapshot| {
            let key = keys.iter().find(|key| key.id == snapshot.key_id)?;
            Some(OpenCircuitDto {
                key_id: key.id.clone(),
                key_name: key.name.clone(),
                model_id: snapshot.model_id,
                state: snapshot.state,
                retry_in_s: snapshot.retry_in_s,
            })
        })
        .collect();
    Ok(health_dto(provider_id, stats, circuits))
}

#[cfg(test)]
mod tests;
