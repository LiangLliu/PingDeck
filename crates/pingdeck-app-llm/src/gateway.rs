//! Gateway use-cases: status, start/stop/restart, settings hooks, the page's overview, circuit
//! reset, the event subscription and the launch-time autostart.
//!
//! Single responsibility: the shell-facing calls over [`crate::llm::gateway::GatewayManager`].
//! Lifecycle calls block (a stop drains for up to 10 s), so the shell runs them on its blocking
//! pool. The overview is a read of the stores plus the in-memory circuit snapshot, cheap enough
//! to poll every 5 s while the page is visible.

use std::sync::Arc;

use super::overview::{GatewayOverviewDto, OverviewInputs, build_overview};
use super::{current_time_ms, with_connection};
use crate::app::AppState;
use crate::error::AppError;
use crate::events::{EventSink, Sequenced};
use crate::llm::events::LlmEvent;
use crate::llm::gateway::{GatewayError, GatewayManager, GatewayStatus, RestartReport};
use crate::llm::health::HealthSnapshot;
use crate::llm::store;
use crate::llm::usage::{SummaryContext, UsageQuery, UsageSource, summarize};
use crate::settings::Settings;

// ------------------------------------------------------------------ lifecycle

/// The gateway's status.
#[must_use]
pub fn llm_gateway_status_impl(state: &AppState) -> GatewayStatus {
    state.llm.gateway.status(&state.llm)
}

/// Starts the gateway on the configured port.
///
/// # Errors
/// See [`GatewayManager::start`].
pub fn llm_gateway_start_impl(state: &AppState) -> Result<GatewayStatus, AppError> {
    state.llm.gateway.start(&state.llm)
}

/// Stops the gateway, letting in-flight requests finish for up to 10 s. Blocks.
#[must_use]
pub fn llm_gateway_stop_impl(state: &AppState) -> GatewayStatus {
    state.llm.gateway.stop(&state.llm)
}

/// Stops the gateway at once ("Stop now"); called while another thread is draining, it ends that
/// drain. Blocks briefly.
#[must_use]
pub fn llm_gateway_stop_now_impl(state: &AppState) -> GatewayStatus {
    state.llm.gateway.stop_now(&state.llm)
}

/// Stops (draining) and starts again, which also applies the Failover settings a running gateway
/// kept until now. The report names the open connections that were
/// dropped, for the confirm text. Blocks.
///
/// # Errors
/// See [`GatewayManager::start`]; a failed start is also in the status (`last_error`).
pub fn llm_gateway_restart_impl(state: &AppState) -> Result<RestartReport, AppError> {
    state.llm.gateway.restart(&state.llm)
}

/// Starts the gateway when `gateway.enabled_on_start` is set. A gateway that cannot start must
/// not keep the app from starting: the failure stays in the manager (`last_error` of the status)
/// and is returned here so the shell can toast "Gateway did not start: <reason>. Open
/// Providers." once. `None`: autostart is off, or the gateway is running.
pub fn llm_gateway_autostart_impl(state: &AppState) -> Option<GatewayError> {
    if !state.settings().ai.gateway.enabled_on_start {
        return None;
    }
    match state.llm.gateway.start(&state.llm) {
        Ok(_) => None,
        Err(error) => Some(
            state
                .llm
                .gateway
                .status(&state.llm)
                .last_error
                .unwrap_or_else(|| GatewayError {
                    code: "start_failed".to_owned(),
                    message: error.message().to_owned(),
                    port: state.llm.settings().ai.gateway.port,
                }),
        ),
    }
}

// ------------------------------------------------------------------ settings hooks

/// Refuses an `ai` settings write the gateway cannot honour: serving the local network needs
/// "Require a token" and at least one active client, which settings validation cannot
/// see. Call before storing `next`.
///
/// # Errors
/// [`AppError::Validation`] naming what to do first; [`AppError::Database`] when the clients
/// cannot be read.
pub fn llm_gateway_check_settings_impl(state: &AppState, next: &Settings) -> Result<(), AppError> {
    GatewayManager::validate_lan(&state.llm, &next.ai.gateway)
}

// ------------------------------------------------------------------ overview

/// Everything the gateway page renders: the
/// aliases panel with a state word per target and the target that served last, today's numbers
/// from the daily rollup (gateway rows only, one amount per currency), the accepted Host names,
/// the clients table and the endpoint rows.
///
/// A rollup that cannot be read leaves `today` empty with `today_error` and the rest of
/// the page intact.
///
/// # Errors
/// [`AppError::Database`] when the routing data or clients cannot be read.
pub fn llm_gateway_overview_impl(state: &AppState) -> Result<GatewayOverviewDto, AppError> {
    let now_ms = current_time_ms();
    let settings = state.llm.settings();
    let status = state.llm.gateway.status(&state.llm);
    let snapshot = state.llm.snapshot_blocking()?;
    let tz_offset_minutes = state.llm.tz_offset_minutes();
    let (clients, today, ops) = with_connection(state, |connection| {
        let clients = store::list_clients(connection)?;
        let today = today_figures(connection, now_ms, tz_offset_minutes).map_err(|e| e.to_string());
        let ops =
            store::today_gateway_ops(connection, now_ms, tz_offset_minutes).unwrap_or_default();
        Ok((clients, today, ops))
    })?;
    let lan = state.llm.gateway.lan_addresses();
    Ok(build_overview(OverviewInputs {
        status,
        settings: &settings.ai.gateway,
        lan,
        clients: &clients,
        snapshot: &snapshot,
        today,
        ops,
    }))
}

/// Today's gateway rollup rows and their summary.
fn today_figures(
    connection: &pingdeck_store::DbConnection,
    now_ms: i64,
    tz_offset_minutes: i16,
) -> Result<
    (
        Vec<crate::llm::usage::RollupRow>,
        crate::llm::usage::UsageToday,
    ),
    AppError,
> {
    let rows = store::today_gateway_rows(connection, now_ms, tz_offset_minutes)?;
    // A one-millisecond window selects exactly the local day of `now_ms`.
    let query = UsageQuery {
        provider_id: None,
        key_id: None,
        client_id: None,
        from_ms: now_ms,
        to_ms: now_ms + 1,
        group_by: Vec::new(),
        sources: vec![UsageSource::Gateway],
        tz_offset_minutes,
        bucket_minutes: None,
    };
    let failures = store::failure_counts(connection, &query)?;
    let summary = summarize(
        &rows,
        &failures,
        &query,
        &SummaryContext {
            now_ms,
            ..SummaryContext::default()
        },
    );
    Ok((rows, summary.today))
}

// ------------------------------------------------------------------ health

/// Health (circuit state, failure rate, latency) of every `(key, model)` the gateway used.
#[must_use]
pub fn llm_health_list_impl(state: &AppState) -> Vec<HealthSnapshot> {
    state.llm.health.snapshot()
}

/// Resets one key's health, or all when `key_id` is `None`. A key's reset also ends its cooldown
/// ("Reset circuit breaker" makes the key usable now); the UI hears `HealthChanged` and,
/// when a cooldown ended, `KeysChanged`.
pub fn llm_health_reset_impl(state: &AppState, key_id: Option<&str>) {
    state.llm.health.reset(key_id);
    if let Some(key_id) = key_id
        && state.llm.keys.clear_cooldown(key_id)
    {
        super::providers::announce_key_runtime(state, key_id);
    }
    state.llm.emit(LlmEvent::HealthChanged {
        snapshots: state.llm.health.snapshot(),
    });
}

/// Closes every circuit and clears the failure counts (Settings › AI › Failover "Reset
/// circuits").
pub fn llm_gateway_reset_circuits_impl(state: &AppState) {
    state.llm.health.reset(None);
}

// ------------------------------------------------------------------ events

/// Pushes every [`LlmEvent`] (providers, keys, models, aliases, balances, health, gateway status
/// and log batches) to `sink`, numbered. With `from_seq` the events after it that the hub still
/// remembers are replayed first, without gaps or duplicates; without it only future events
/// arrive. Any number of subscribers may listen at once; returns the subscription id. A `from_seq`
/// older than what the hub remembers first yields [`LlmEvent::Gap`], then the whole remembered
/// ring (the caller reloads the persisted state).
///
/// # Errors
/// [`AppError::Conflict`] when too many subscriptions are live.
pub fn llm_subscribe_impl(
    state: &AppState,
    sink: Arc<dyn EventSink<Sequenced<LlmEvent>>>,
    from_seq: Option<u64>,
) -> Result<u64, AppError> {
    match from_seq {
        Some(_) => state
            .llm
            .events()
            .subscribe_announcing_gap(from_seq, sink, |first_seq| LlmEvent::Gap { first_seq }),
        None => state.llm.events().subscribe_live(sink),
    }
}

/// Ends the subscription `subscription_id` (other subscribers keep listening); returns whether
/// it was still active.
pub fn llm_unsubscribe_impl(state: &AppState, subscription_id: u64) -> bool {
    state.llm.events().unsubscribe(subscription_id)
}
