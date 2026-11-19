//! Usage and price use-cases: the Usage tab summary, the gateway "Today" strip, the effective
//! price of a model and the legacy per-provider price overrides, which are the provider-scoped
//! `custom` rows of `model_pricing` (D-34).
//!
//! Single responsibility: read the daily rollup (`llm_usage_daily`, written by the log writer in
//! the same transaction as the log row) and fold it with [`summarize`]. Days are local calendar
//! days under the UTC offset the shell reports; costs stay per currency and are never added.

use std::collections::BTreeSet;

use super::{with_connection, with_db_blocking};
use crate::app::{AppState, current_time_ms};
use crate::error::AppError;
use crate::llm::events::LlmEvent;
use crate::llm::store::{self, UsageFailure, UsageLatency};
use crate::llm::usage::{
    MAX_BUCKETS, MAX_TZ_OFFSET_MINUTES, PriceOverride, SummaryContext, UsageLatencyMode,
    UsageQuery, UsageSource, UsageSummary, UsageToday, add_days, bucket_start_ms, local_day,
    local_midnight_ms, summarize,
};

/// Providers whose models cost nothing: a local runtime (loopback or private address) that needs
/// no key. Their rows are "free", never "unpriced".
pub(super) fn free_providers(
    connection: &pingdeck_store::DbConnection,
) -> Result<BTreeSet<String>, AppError> {
    Ok(store::list_providers(connection)?
        .into_iter()
        .filter(crate::llm::usage::is_free_provider)
        .map(|provider| provider.id)
        .collect())
}

/// The usage of a range: one entry per local day (zero-filled), or per bucket of
/// the minute rollup when the query sets `bucket_minutes`, the per-model table, the optional
/// `group_by` split, the totals and the Today figures (always from the daily rollup). Cost is
/// one amount per currency, so a range with two currencies has one series per currency and no
/// sum. A daily range starts no earlier than the usage retention.
///
/// # Errors
/// [`AppError::Validation`] for an offset beyond 14 hours, an empty range or a bad bucket,
/// [`AppError::Database`] on SQLite failure.
pub async fn llm_usage_summary_impl(
    state: &AppState,
    mut query: UsageQuery,
) -> Result<UsageSummary, AppError> {
    let now_ms = current_time_ms();
    prepare_query(state, &mut query, now_ms)?;
    with_db_blocking(state, move |connection| {
        let mut rows = store::usage_rows(connection, &query)?;
        let mut failures = store::failure_counts(connection, &query)?;
        if query.bucket_minutes.is_some() {
            let tz = query.tz_offset_minutes;
            rows.extend(store::today_gateway_rows(connection, now_ms, tz)?);
            failures.extend(store::failure_counts(connection, &today_query(now_ms, tz))?);
        }
        let context = SummaryContext {
            now_ms,
            free_providers: free_providers(connection)?,
        };
        Ok(summarize(&rows, &failures, &query, &context))
    })
    .await
}

/// Checks the query, then starts a daily range no earlier than the oldest day the usage
/// retention keeps (the pruning cut plus a day for the time zone), so "All" spans what is kept.
fn prepare_query(state: &AppState, query: &mut UsageQuery, now_ms: i64) -> Result<(), AppError> {
    if query.tz_offset_minutes.unsigned_abs() > MAX_TZ_OFFSET_MINUTES.unsigned_abs() {
        return Err(AppError::validation("the UTC offset is out of range"));
    }
    if query.to_ms <= query.from_ms {
        return Err(AppError::validation("the range of days is empty"));
    }
    let tz = query.tz_offset_minutes;
    if let Some(minutes) = query.bucket_minutes {
        if !(1..=1440).contains(&minutes) {
            return Err(AppError::validation("a bucket is 1 to 1440 minutes"));
        }
        let width = i64::from(minutes) * 60_000;
        let span = query
            .to_ms
            .saturating_sub(bucket_start_ms(query.from_ms, tz, minutes));
        if span / width >= i64::try_from(MAX_BUCKETS).unwrap_or(i64::MAX) {
            return Err(AppError::validation("the range has too many buckets"));
        }
        return Ok(());
    }
    let retention_days = state.settings().ai.providers.usage_retention_days;
    let oldest = add_days(&local_day(now_ms, tz), -(i64::from(retention_days) + 2))
        .and_then(|day| local_midnight_ms(&day, tz));
    if let Some(oldest) = oldest {
        query.from_ms = query.from_ms.max(oldest);
    }
    Ok(())
}

/// The latency of a range per provider, model and the query's `group_by` values (the Usage
/// module's "avg latency"): the sums of `latency_ms` / `timed_requests` of the daily rollup, or
/// of the minute rollup with `bucket_minutes`, over the query's window, sources and scope. Only
/// successful requests are timed.
///
/// # Errors
/// [`AppError::Validation`] for an offset beyond 14 hours, an empty range or a bad bucket,
/// [`AppError::Database`] on SQLite failure.
pub async fn llm_usage_latency_impl(
    state: &AppState,
    mut query: UsageQuery,
) -> Result<Vec<UsageLatency>, AppError> {
    prepare_query(state, &mut query, current_time_ms())?;
    with_db_blocking(state, move |connection| {
        store::usage_latency(connection, &query)
    })
    .await
}

/// The successful calls of a range per source, provider, key, model, client and transport from
/// the request log (the Usage latency card: streamed against unary, and time to first token),
/// over the query's window, sources and scope. `group_by` and `bucket_minutes` do not apply; the
/// log keeps fewer days than the rollups.
///
/// # Errors
/// [`AppError::Validation`] for an offset beyond 14 hours, an empty range or a bad bucket,
/// [`AppError::Database`] on SQLite failure.
pub async fn llm_usage_latency_modes_impl(
    state: &AppState,
    mut query: UsageQuery,
) -> Result<Vec<UsageLatencyMode>, AppError> {
    prepare_query(state, &mut query, current_time_ms())?;
    with_db_blocking(state, move |connection| {
        store::usage_latency_modes(connection, &query)
    })
    .await
}

/// The failed requests of a range per failure kind and the query's `group_by` values (the Usage
/// module's "Failure reasons"), from the minute rollup over the query's window, sources and
/// scope; a query without `bucket_minutes` has none.
///
/// # Errors
/// [`AppError::Validation`] for an offset beyond 14 hours, an empty range or a bad bucket,
/// [`AppError::Database`] on SQLite failure.
pub async fn llm_usage_failures_impl(
    state: &AppState,
    mut query: UsageQuery,
) -> Result<Vec<UsageFailure>, AppError> {
    prepare_query(state, &mut query, current_time_ms())?;
    with_db_blocking(state, move |connection| {
        store::usage_failures(connection, &query)
    })
    .await
}

/// The gateway rows of the local day of `now_ms` (the gateway "Today" window).
fn today_query(now_ms: i64, tz: i16) -> UsageQuery {
    let midnight = local_midnight_ms(&local_day(now_ms, tz), tz).unwrap_or(now_ms);
    UsageQuery {
        provider_id: None,
        key_id: None,
        client_id: None,
        from_ms: midnight,
        to_ms: now_ms.saturating_add(1),
        group_by: Vec::new(),
        sources: vec![UsageSource::Gateway],
        tz_offset_minutes: tz,
        bucket_minutes: None,
    }
}

/// The gateway "Today" strip: requests, failures, tokens and one cost amount per
/// currency of the local day, read from the rollup (so it equals the row count of `since:today`
/// whatever the log retention did). "Failed" is a final status of 400 or more or a failure
/// outcome (a stream that broke after a 200 counts, so the log's `is:failed` matches it, not a
/// status filter), never a served failover; a cancellation is in `canceled`, not in `failed`.
/// The Usage module's Today window uses the same rule. The day is cut with the offset the shell set through
/// [`llm_usage_set_tz_offset_impl`].
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub async fn llm_today_summary_impl(state: &AppState) -> Result<UsageToday, AppError> {
    let now_ms = current_time_ms();
    let tz = state.llm.tz_offset_minutes();
    with_db_blocking(state, move |connection| {
        let query = today_query(now_ms, tz);
        let rows = store::today_gateway_rows(connection, now_ms, tz)?;
        let failures = store::failure_counts(connection, &query)?;
        let context = SummaryContext {
            now_ms,
            free_providers: free_providers(connection)?,
        };
        Ok(summarize(&rows, &failures, &query, &context).today)
    })
    .await
}

/// Pins the UTC offset, in minutes east, that cuts local days for the usage rollup and "Today".
/// The shell calls it at startup and when the zone changes (daylight saving).
///
/// # Errors
/// [`AppError::Validation`] for an offset beyond 14 hours.
pub fn llm_usage_set_tz_offset_impl(state: &AppState, minutes: i16) -> Result<(), AppError> {
    if minutes.unsigned_abs() > MAX_TZ_OFFSET_MINUTES.unsigned_abs() {
        return Err(AppError::validation("the UTC offset is out of range"));
    }
    state.llm.set_tz_offset_minutes(minutes);
    Ok(())
}

// ------------------------------------------------------------------- prices

/// Every price override (provider-scoped `custom` pricing row), ordered by provider and model.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn llm_price_override_list_impl(state: &AppState) -> Result<Vec<PriceOverride>, AppError> {
    with_connection(state, |connection| {
        Ok(store::price_overrides(connection)?.into_values().collect())
    })
}

/// Saves the override of one model as a `custom` pricing row of that provider: prices between 0
/// and 1,000,000 per million tokens, a
/// three-letter currency (normalised to upper case), an existing provider. The change applies to
/// requests made from now on; rows already logged keep their cost.
///
/// # Errors
/// [`AppError::Validation`] for a bad price, currency or id, [`AppError::NotFound`] for an unknown
/// provider.
pub fn llm_price_override_save_impl(
    state: &AppState,
    price: PriceOverride,
) -> Result<PriceOverride, AppError> {
    let saved = with_connection(state, |connection| {
        let price = price.validated()?;
        if store::get_provider(connection, &price.provider_id)?.is_none() {
            return Err(AppError::not_found(format!(
                "provider {} not found",
                price.provider_id
            )));
        }
        store::save_price_override(connection, price, current_time_ms())
    })?;
    state.llm.touch();
    state.llm.emit(LlmEvent::ModelsChanged {
        provider_id: saved.provider_id.clone(),
    });
    Ok(saved)
}

/// Removes the override of one model ("Reset to the catalog price"); `true` when one existed.
/// A `fixed` or `dynamic` row of the provider is left to `llm_model_pricing_delete`.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn llm_price_override_delete_impl(
    state: &AppState,
    provider_id: &str,
    model_id: &str,
) -> Result<bool, AppError> {
    let deleted = with_connection(state, |connection| {
        store::delete_price_override(connection, provider_id, model_id)
    })?;
    if deleted {
        state.llm.touch();
        state.llm.emit(LlmEvent::ModelsChanged {
            provider_id: provider_id.to_owned(),
        });
    }
    Ok(deleted)
}

#[cfg(test)]
mod tests;
