//! Usage persistence: the daily and minute rollups and the exact failure counters.
//!
//! Single responsibility: SQL over `llm_usage_daily`, `llm_usage_minute` and
//! `llm_usage_failures` for the pure types of `llm::usage`. `insert_log` writes both rollups in
//! the same transaction as the log row; the gateway log writer's overflow path
//! (`roll_up_overflow`) calls [`upsert_rollup`] on its own, outside any transaction. The rollups
//! are never pruned by the log retention: the daily one keeps `usage_retention_days`, the minute
//! one at most [`MINUTE_RETENTION_DAYS`].

use std::collections::BTreeMap;

use rusqlite::types::Value;
use rusqlite::{Connection, params, params_from_iter};
use serde::{Deserialize, Serialize};

use super::{db, to_i64, to_u64};
use crate::llm::failure::FailureKind;
pub use crate::llm::usage::FailureCount;
use crate::llm::usage::{
    MAX_TZ_OFFSET_MINUTES, RollupDelta, RollupRow, UsageGroup, UsageLatencyMode, UsageQuery,
    UsageSource, add_days, local_day, local_midnight_ms,
};
use pingdeck_base::error::AppError;

const MS_PER_DAY: i64 = 86_400_000;
const MS_PER_MINUTE: i64 = 60_000;
/// Most days the minute rollup keeps: 90 days plus two, so the 90-day window is exact in any
/// time zone. A shorter usage retention cuts it too, so "All" is never shorter than 90d. Older
/// minutes are only in the daily rollup.
pub const MINUTE_RETENTION_DAYS: i64 = 92;

/// Adds the request of `delta`, made at `ts_ms`, to its `llm_usage_minute` row: the minute
/// (UTC, floored) plus the daily row's key and the failure kind (`''` for a success), so the
/// exact failure counts of any window are sums of `requests`. A success adds its latency.
/// Runs inside the caller's transaction.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub(crate) fn upsert_minute(
    connection: &Connection,
    delta: &RollupDelta,
    ts_ms: i64,
    latency_ms: u64,
) -> Result<(), AppError> {
    let timed = delta.ok_requests > 0;
    connection
        .prepare_cached(
            "INSERT INTO llm_usage_minute (minute_ms, source, provider_id, key_id, model, \
                 client_id, alias, currency, failure_kind, requests, ok_requests, input_tokens, \
                 output_tokens, cached_tokens, cache_write_tokens, reasoning_tokens, \
                 cost_micro_usd, latency_ms, timed_requests) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17, \
                 ?18, ?19) \
             ON CONFLICT (minute_ms, source, provider_id, key_id, model, client_id, alias, \
                 currency, failure_kind) \
             DO UPDATE SET \
                 requests = requests + excluded.requests, \
                 ok_requests = ok_requests + excluded.ok_requests, \
                 input_tokens = input_tokens + excluded.input_tokens, \
                 output_tokens = output_tokens + excluded.output_tokens, \
                 cached_tokens = cached_tokens + excluded.cached_tokens, \
                 cache_write_tokens = cache_write_tokens + excluded.cache_write_tokens, \
                 reasoning_tokens = reasoning_tokens + excluded.reasoning_tokens, \
                 cost_micro_usd = cost_micro_usd + excluded.cost_micro_usd, \
                 latency_ms = latency_ms + excluded.latency_ms, \
                 timed_requests = timed_requests + excluded.timed_requests",
        )
        .map_err(db)?
        .execute(params![
            ts_ms.div_euclid(MS_PER_MINUTE) * MS_PER_MINUTE,
            delta.source,
            delta.provider_id,
            delta.key_id,
            delta.model,
            delta.client_id,
            delta.alias,
            delta.currency,
            delta.failure_kind.as_deref().unwrap_or(""),
            to_i64(delta.requests),
            to_i64(delta.ok_requests),
            to_i64(delta.input_tokens),
            to_i64(delta.output_tokens),
            to_i64(delta.cached_tokens),
            to_i64(delta.cache_write_tokens),
            to_i64(delta.reasoning_tokens),
            delta.cost_micro,
            if timed { to_i64(latency_ms) } else { 0 },
            i64::from(timed),
        ])
        .map_err(db)?;
    Ok(())
}

/// Adds `delta` to its `llm_usage_daily` row (one upsert; counters add) and bumps the exact
/// failure counter of `llm_usage_failures`.
///
/// Does not open a transaction: the two upserts are atomic only when the caller holds one
/// (`insert_log` does; the gateway overflow path does not, so a crash between the two statements
/// can leave the daily and failure counters out of step).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn upsert_rollup(connection: &Connection, delta: &RollupDelta) -> Result<(), AppError> {
    connection
        .prepare_cached(
            "INSERT INTO llm_usage_daily (day, source, provider_id, key_id, model, client_id, alias, \
                 currency, requests, ok_requests, input_tokens, output_tokens, cached_tokens, \
                 cache_write_tokens, reasoning_tokens, cost_micro_usd, \
                 canceled_requests) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13, ?14, ?15, ?16, ?17) \
             ON CONFLICT (day, source, provider_id, key_id, model, client_id, alias, currency) \
             DO UPDATE SET \
                 requests = requests + excluded.requests, \
                 ok_requests = ok_requests + excluded.ok_requests, \
                 input_tokens = input_tokens + excluded.input_tokens, \
                 output_tokens = output_tokens + excluded.output_tokens, \
                 cached_tokens = cached_tokens + excluded.cached_tokens, \
                 cache_write_tokens = cache_write_tokens + excluded.cache_write_tokens, \
                 reasoning_tokens = reasoning_tokens + excluded.reasoning_tokens, \
                 cost_micro_usd = cost_micro_usd + excluded.cost_micro_usd, \
                 canceled_requests = canceled_requests + excluded.canceled_requests",
        )
        .map_err(db)?
        .execute(params![
                delta.day,
                delta.source,
                delta.provider_id,
                delta.key_id,
                delta.model,
                delta.client_id,
                delta.alias,
                delta.currency,
                to_i64(delta.requests),
                to_i64(delta.ok_requests),
                to_i64(delta.input_tokens),
                to_i64(delta.output_tokens),
                to_i64(delta.cached_tokens),
                to_i64(delta.cache_write_tokens),
                to_i64(delta.reasoning_tokens),
                delta.cost_micro,
                i64::from(delta.failure_kind.as_deref() == Some(FailureKind::Cancelled.as_str())),
            ])
        .map_err(db)?;
    if let Some(kind) = &delta.failure_kind {
        connection
            .prepare_cached(
                "INSERT INTO llm_usage_failures (day, source, provider_id, model, kind, count) \
                 VALUES (?1, ?2, ?3, ?4, ?5, 1) \
                 ON CONFLICT (day, source, provider_id, model, kind) \
                 DO UPDATE SET count = count + 1",
            )
            .map_err(db)?
            .execute(params![
                delta.day,
                delta.source,
                delta.provider_id,
                delta.model,
                kind
            ])
            .map_err(db)?;
    }
    Ok(())
}

/// Adds the latency of a request that succeeded to its `llm_usage_daily` row (the one
/// [`upsert_rollup`] just wrote): `latency_ms` sums it, `timed_requests` counts it, so the
/// average is their ratio. Failed and cancelled requests are not timed: their latency says how
/// fast the failure came, not how fast the model answers. Runs inside the caller's transaction.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub(super) fn add_rollup_latency(
    connection: &Connection,
    delta: &RollupDelta,
    latency_ms: u64,
) -> Result<(), AppError> {
    if delta.ok_requests == 0 {
        return Ok(());
    }
    connection
        .prepare_cached(
            "UPDATE llm_usage_daily SET latency_ms = latency_ms + ?9, \
                 timed_requests = timed_requests + 1 \
             WHERE day = ?1 AND source = ?2 AND provider_id = ?3 AND key_id = ?4 AND model = ?5 \
                 AND client_id = ?6 AND alias = ?7 AND currency = ?8",
        )
        .map_err(db)?
        .execute(params![
            delta.day,
            delta.source,
            delta.provider_id,
            delta.key_id,
            delta.model,
            delta.client_id,
            delta.alias,
            delta.currency,
            to_i64(latency_ms),
        ])
        .map_err(db)?;
    Ok(())
}

/// Summed latency of one provider and model over a range (`llm_usage_latency`), optionally
/// split further by the query's `group_by` dimensions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageLatency {
    pub provider_id: String,
    pub model: String,
    /// Successful requests whose latency is in `latency_ms` (rows written without a latency have
    /// none, so this can be lower than the request count).
    pub timed_requests: u64,
    /// Sum of their end-to-end latencies, ms; the average is `latency_ms / timed_requests`.
    pub latency_ms: u64,
    /// Values of the query's `group_by` dimensions, in its order; empty (and left off the
    /// wire) when the query has no `group_by`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key: Vec<String>,
}

/// The latency sums of `query` (window, sources, provider, key, client) per provider, model and
/// the values of `query.group_by`; only groups with at least one timed request, ordered by
/// provider, model, then the grouped values.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn usage_latency(
    connection: &Connection,
    query: &UsageQuery,
) -> Result<Vec<UsageLatency>, AppError> {
    let Some((filter, values)) = scope(query, true) else {
        return Ok(Vec::new());
    };
    let extra = group_columns(query);
    let mut statement = connection
        .prepare(&format!(
            "SELECT provider_id, model, SUM(timed_requests), SUM(latency_ms){extra} \
             FROM {table} WHERE {filter} GROUP BY provider_id, model{extra} \
             HAVING SUM(timed_requests) > 0 ORDER BY provider_id, model{extra}",
            table = table_of(query)
        ))
        .map_err(db)?;
    let width = query.group_by.len();
    let rows = statement
        .query_map(params_from_iter(values), |row| {
            Ok(UsageLatency {
                provider_id: row.get(0)?,
                model: row.get(1)?,
                timed_requests: to_u64(row.get(2)?),
                latency_ms: to_u64(row.get(3)?),
                key: (4..4 + width)
                    .map(|index| row.get(index))
                    .collect::<Result<_, _>>()?,
            })
        })
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// The successful calls of `query` in the request log (request time in `[from_ms, to_ms)`, the
/// query's sources, provider, key and client), per source, provider, key, model, client and
/// whether the call streamed, with their end-to-end and first-token sums. Success is the
/// rollups' rule (`request_succeeded`): outcome `success`, status below 400, not truncated. The
/// log keeps fewer days than the rollups, so an older window has fewer rows or none.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn usage_latency_modes(
    connection: &Connection,
    query: &UsageQuery,
) -> Result<Vec<UsageLatencyMode>, AppError> {
    let sources: Vec<Value> = query
        .effective_sources()
        .into_iter()
        .map(|source| Value::Text(source.as_str().to_owned()))
        .collect();
    let placeholders: Vec<String> = (0..sources.len()).map(|n| format!("?{}", n + 6)).collect();
    let mut values = vec![
        Value::Integer(query.from_ms),
        Value::Integer(query.to_ms),
        query.provider_id.clone().map_or(Value::Null, Value::Text),
        query.key_id.clone().map_or(Value::Null, Value::Text),
        query.client_id.clone().map_or(Value::Null, Value::Text),
    ];
    values.extend(sources);
    // The source count varies the SQL text, so it is prepared, not cached.
    let mut statement = connection
        .prepare(&format!(
            "SELECT source, COALESCE(provider_id, ''), COALESCE(key_id, ''), \
             COALESCE(NULLIF(upstream_model, ''), requested_model), COALESCE(client_id, ''), \
             stream, COUNT(*), SUM(latency_ms), COUNT(ttft_ms), COALESCE(SUM(ttft_ms), 0) \
             FROM llm_request_log WHERE ts_ms >= ?1 AND ts_ms < ?2 \
             AND COALESCE(provider_id, '') <> '' \
             AND (?3 IS NULL OR provider_id = ?3) AND (?4 IS NULL OR key_id = ?4) \
             AND (?5 IS NULL OR client_id = ?5) AND source IN ({}) \
             AND outcome = 'success' AND status < 400 \
             AND COALESCE(failure_kind, '') <> '{truncated}' \
             GROUP BY 1, 2, 3, 4, 5, 6 ORDER BY 1, 2, 3, 4, 5, 6",
            placeholders.join(", "),
            truncated = FailureKind::Truncated.as_str(),
        ))
        .map_err(db)?;
    let rows = statement
        .query_map(params_from_iter(values), |row| {
            Ok(UsageLatencyMode {
                source: row.get(0)?,
                provider_id: row.get(1)?,
                key_id: row.get(2)?,
                model: row.get(3)?,
                client_id: row.get(4)?,
                stream: row.get::<_, i64>(5)? != 0,
                timed_requests: to_u64(row.get(6)?),
                latency_ms: to_u64(row.get(7)?),
                ttft_requests: to_u64(row.get(8)?),
                ttft_ms: to_u64(row.get(9)?),
            })
        })
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// Failed requests of one failure kind over a range (`llm_usage_failures`), split by the
/// query's `group_by` dimensions.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageFailure {
    /// Failure kind (`FailureKind` wire name).
    pub kind: String,
    pub count: u64,
    /// Values of the query's `group_by` dimensions, in its order; empty (and left off the
    /// wire) when the query has no `group_by`.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub key: Vec<String>,
}

/// The failed requests of `query` (window, sources, provider, key, client) per failure kind and
/// the values of `query.group_by`, from the minute rollup; ordered by kind, then the grouped
/// values. Only bucket queries have failure kinds per key, alias and client: a daily query
/// returns nothing.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn usage_failures(
    connection: &Connection,
    query: &UsageQuery,
) -> Result<Vec<UsageFailure>, AppError> {
    if query.bucket_minutes.is_none() {
        return Ok(Vec::new());
    }
    let Some((filter, values)) = scope(query, true) else {
        return Ok(Vec::new());
    };
    let extra = group_columns(query);
    let mut statement = connection
        .prepare(&format!(
            "SELECT failure_kind, SUM(requests){extra} FROM llm_usage_minute \
             WHERE {filter} AND failure_kind <> '' GROUP BY failure_kind{extra} \
             ORDER BY failure_kind{extra}"
        ))
        .map_err(db)?;
    let width = query.group_by.len();
    let rows = statement
        .query_map(params_from_iter(values), |row| {
            Ok(UsageFailure {
                kind: row.get(0)?,
                count: to_u64(row.get(1)?),
                key: (2..2 + width)
                    .map(|index| row.get(index))
                    .collect::<Result<_, _>>()?,
            })
        })
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

const fn table_of(query: &UsageQuery) -> &'static str {
    if query.bucket_minutes.is_some() {
        "llm_usage_minute"
    } else {
        "llm_usage_daily"
    }
}

/// What the `day` of a row is: the column of the daily rollup, or in bucket mode the local
/// bucket label `YYYY-MM-DDTHH:MM` of `minute_ms` (the same as `llm::usage::bucket_range`). The
/// offset and width are integers, so splicing them is safe.
fn day_column(query: &UsageQuery) -> String {
    let Some(minutes) = query.bucket_minutes else {
        return "day".to_owned();
    };
    let offset = i64::from(
        query
            .tz_offset_minutes
            .clamp(-MAX_TZ_OFFSET_MINUTES, MAX_TZ_OFFSET_MINUTES),
    ) * MS_PER_MINUTE;
    let width = i64::from(minutes.max(1)) * MS_PER_MINUTE;
    format!(
        "strftime('%Y-%m-%dT%H:%M', ((minute_ms + {offset}) / {width}) * {width} / 1000, \
         'unixepoch')"
    )
}

/// `, <column>` for every `group_by` dimension, in order (the day in bucket mode is the bucket
/// label of `day_column`).
fn group_columns(query: &UsageQuery) -> String {
    let day = day_column(query);
    query
        .group_by
        .iter()
        .map(|group| match group {
            UsageGroup::Day => format!(", {day}"),
            other => format!(", {}", other.column()),
        })
        .collect()
}

/// The part of a [`UsageQuery`] the rollup tables are filtered by, as SQL and bound values:
/// local days of the daily rollup, or in bucket mode the minutes starting in
/// `[from_ms, to_ms)`. `None` when the query covers no day.
fn scope(query: &UsageQuery, with_key_and_client: bool) -> Option<(String, Vec<Value>)> {
    let (window, first, last) = if query.bucket_minutes.is_some() {
        (
            "minute_ms >= ?1 AND minute_ms < ?2",
            Value::Integer(query.from_ms),
            Value::Integer(query.to_ms),
        )
    } else {
        let (first, last) = query.day_window()?;
        (
            "day BETWEEN ?1 AND ?2",
            Value::Text(first),
            Value::Text(last),
        )
    };
    let sources: Vec<Value> = query
        .effective_sources()
        .into_iter()
        .map(|source| Value::Text(source.as_str().to_owned()))
        .collect();
    let placeholders: Vec<String> = (0..sources.len()).map(|n| format!("?{}", n + 4)).collect();
    let mut sql = format!(
        "{window} AND provider_id <> '' AND (?3 IS NULL OR provider_id = ?3) AND source IN ({})",
        placeholders.join(", ")
    );
    let mut values = vec![
        first,
        last,
        query.provider_id.clone().map_or(Value::Null, Value::Text),
    ];
    values.extend(sources);
    if with_key_and_client {
        let base = values.len();
        sql.push_str(&format!(
            " AND (?{} IS NULL OR key_id = ?{}) AND (?{} IS NULL OR client_id = ?{})",
            base + 1,
            base + 1,
            base + 2,
            base + 2
        ));
        values.push(query.key_id.clone().map_or(Value::Null, Value::Text));
        values.push(query.client_id.clone().map_or(Value::Null, Value::Text));
    }
    Some((sql, values))
}

const ROLLUP_COLUMNS: &str = "day, source, provider_id, key_id, model, client_id, alias, currency, \
     requests, ok_requests, input_tokens, output_tokens, cached_tokens, cache_write_tokens, \
     reasoning_tokens, cost_micro_usd, canceled_requests";
/// The key of a rollup row after its day.
const ROW_KEY: &str = "source, provider_id, key_id, model, client_id, alias, currency";

fn rollup_from_row(row: &rusqlite::Row<'_>) -> rusqlite::Result<RollupRow> {
    let count = |index: usize| row.get::<_, i64>(index).map(to_u64);
    Ok(RollupRow {
        day: row.get(0)?,
        source: row.get(1)?,
        provider_id: row.get(2)?,
        key_id: row.get(3)?,
        model: row.get(4)?,
        client_id: row.get(5)?,
        alias: row.get(6)?,
        currency: row.get(7)?,
        requests: count(8)?,
        ok_requests: count(9)?,
        input_tokens: count(10)?,
        output_tokens: count(11)?,
        cached_tokens: count(12)?,
        cache_write_tokens: count(13)?,
        reasoning_tokens: count(14)?,
        cost_micro: row.get(15)?,
        canceled: count(16)?,
    })
}

/// The rollup rows of `query`: days (or, in bucket mode, buckets summed over their minutes) of
/// its window, its sources (all but probes by default), provider, key and client; ordered by
/// day, then the row key.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn usage_rows(connection: &Connection, query: &UsageQuery) -> Result<Vec<RollupRow>, AppError> {
    let Some((filter, values)) = scope(query, true) else {
        return Ok(Vec::new());
    };
    // The failure kind is spelled `cancelled` (FailureKind::Cancelled); the log outcome is
    // `canceled`.
    let sql = if query.bucket_minutes.is_some() {
        format!(
            "SELECT {day}, {ROW_KEY}, SUM(requests), SUM(ok_requests), SUM(input_tokens), \
                 SUM(output_tokens), SUM(cached_tokens), SUM(cache_write_tokens), \
                 SUM(reasoning_tokens), SUM(cost_micro_usd), \
                 SUM(CASE WHEN failure_kind = 'cancelled' THEN requests ELSE 0 END) \
             FROM llm_usage_minute WHERE {filter} GROUP BY 1, {ROW_KEY} ORDER BY 1, {ROW_KEY}",
            day = day_column(query)
        )
    } else {
        format!(
            "SELECT {ROLLUP_COLUMNS} FROM llm_usage_daily WHERE {filter} ORDER BY day, {ROW_KEY}"
        )
    };
    let mut statement = connection.prepare(&sql).map_err(db)?;
    let rows = statement
        .query_map(params_from_iter(values), rollup_from_row)
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// The gateway rows of the local day of `now_ms` (all providers, keys and clients): the exact
/// "Today" figures, read from the rollup and not from the capped log.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn today_gateway_rows(
    connection: &Connection,
    now_ms: i64,
    tz_offset_minutes: i16,
) -> Result<Vec<RollupRow>, AppError> {
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {ROLLUP_COLUMNS} FROM llm_usage_daily WHERE day = ?1 AND source = ?2 \
             AND provider_id <> '' \
             ORDER BY provider_id, key_id, model, client_id, alias, currency"
        ))
        .map_err(db)?;
    let rows = statement
        .query_map(
            params![
                local_day(now_ms, tz_offset_minutes),
                UsageSource::Gateway.as_str()
            ],
            rollup_from_row,
        )
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// Real-time operational figures for today's local gateway traffic, read from `llm_request_log`.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct GatewayTodayOps {
    /// All gateway log rows recorded during the local day (including pre-routing rejections).
    pub total_logged: u64,
    /// Routed gateway calls today answered via same-protocol passthrough (`translated = 0`).
    pub passthrough: u64,
    /// Routed gateway calls today answered via cross-protocol transcoding (`translated = 1`).
    pub translated: u64,
    /// Successful gateway calls today that recovered via a fallback attempt (`attempts > 1`).
    pub failover_recovered: u64,
    /// Gateway calls rejected before routing to any provider (401/403/429/unknown model).
    pub rejected: u64,
    /// Median streaming time-to-first-token (ms) among successful gateway calls today.
    pub median_ttft_ms: Option<u64>,
    /// Today's gateway request count keyed by `requested_model`.
    #[serde(default, skip_serializing_if = "BTreeMap::is_empty")]
    pub model_hits: BTreeMap<String, u64>,
}

/// Operational breakdown for today's local gateway traffic (`source = 'gateway'`).
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn today_gateway_ops(
    connection: &Connection,
    now_ms: i64,
    tz_offset_minutes: i16,
) -> Result<GatewayTodayOps, AppError> {
    let day = local_day(now_ms, tz_offset_minutes);
    let Some(start_ms) = local_midnight_ms(&day, tz_offset_minutes) else {
        return Ok(GatewayTodayOps::default());
    };
    let end_ms = start_ms.saturating_add(MS_PER_DAY);
    let source = UsageSource::Gateway.as_str();

    let (total_logged, passthrough, translated, failover_recovered, rejected) = connection
        .prepare_cached(
            "SELECT \
                 COUNT(*), \
                 COALESCE(SUM(CASE WHEN COALESCE(provider_id, '') <> '' AND translated = 0 THEN 1 ELSE 0 END), 0), \
                 COALESCE(SUM(CASE WHEN COALESCE(provider_id, '') <> '' AND translated = 1 THEN 1 ELSE 0 END), 0), \
                 COALESCE(SUM(CASE WHEN outcome = 'success' AND status < 400 \
                     AND (CASE WHEN json_valid(attempts_json) THEN json_array_length(attempts_json) ELSE 0 END) > 1 \
                     THEN 1 ELSE 0 END), 0), \
                 COALESCE(SUM(CASE WHEN COALESCE(provider_id, '') = '' THEN 1 ELSE 0 END), 0) \
             FROM llm_request_log \
             WHERE source = ?1 AND ts_ms >= ?2 AND ts_ms < ?3",
        )
        .map_err(db)?
        .query_row(params![source, start_ms, end_ms], |row| {
            Ok((
                to_u64(row.get(0)?),
                to_u64(row.get(1)?),
                to_u64(row.get(2)?),
                to_u64(row.get(3)?),
                to_u64(row.get(4)?),
            ))
        })
        .map_err(db)?;

    let mut ttft_stmt = connection
        .prepare_cached(
            "SELECT ttft_ms FROM llm_request_log \
             WHERE source = ?1 AND ts_ms >= ?2 AND ts_ms < ?3 \
               AND outcome = 'success' AND status < 400 AND ttft_ms IS NOT NULL \
             ORDER BY ttft_ms ASC",
        )
        .map_err(db)?;
    let ttfts: Vec<u64> = ttft_stmt
        .query_map(params![source, start_ms, end_ms], |row| {
            Ok(to_u64(row.get(0)?))
        })
        .map_err(db)?
        .collect::<Result<_, _>>()
        .map_err(db)?;
    let median_ttft_ms = if ttfts.is_empty() {
        None
    } else {
        let mid = ttfts.len() / 2;
        if ttfts.len() % 2 == 1 {
            ttfts.get(mid).copied()
        } else {
            match (ttfts.get(mid - 1), ttfts.get(mid)) {
                (Some(&a), Some(&b)) => Some(a.saturating_add(b) / 2),
                _ => None,
            }
        }
    };

    let mut hits_stmt = connection
        .prepare_cached(
            "SELECT requested_model, COUNT(*) FROM llm_request_log \
             WHERE source = ?1 AND ts_ms >= ?2 AND ts_ms < ?3 AND requested_model <> '' \
             GROUP BY requested_model",
        )
        .map_err(db)?;
    let mut model_hits = BTreeMap::new();
    let hit_rows = hits_stmt
        .query_map(params![source, start_ms, end_ms], |row| {
            Ok((row.get::<_, String>(0)?, to_u64(row.get(1)?)))
        })
        .map_err(db)?;
    for row in hit_rows {
        let (model, count) = row.map_err(db)?;
        model_hits.insert(model, count);
    }

    Ok(GatewayTodayOps {
        total_logged,
        passthrough,
        translated,
        failover_recovered,
        rejected,
        median_ttft_ms,
        model_hits,
    })
}

/// The exact failure counters of `query` (window, sources and provider; keys and clients are not
/// tracked here), for the "most frequent failure" figure. In bucket mode they are the failed
/// requests of the minute rollup per bucket.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn failure_counts(
    connection: &Connection,
    query: &UsageQuery,
) -> Result<Vec<FailureCount>, AppError> {
    let Some((filter, values)) = scope(query, false) else {
        return Ok(Vec::new());
    };
    let sql = if query.bucket_minutes.is_some() {
        format!(
            "SELECT {day}, source, provider_id, model, failure_kind, SUM(requests) \
             FROM llm_usage_minute WHERE {filter} AND failure_kind <> '' \
             GROUP BY 1, source, provider_id, model, failure_kind \
             ORDER BY 1, source, provider_id, model, failure_kind",
            day = day_column(query)
        )
    } else {
        format!(
            "SELECT day, source, provider_id, model, kind, count FROM llm_usage_failures \
             WHERE {filter} ORDER BY day, source, provider_id, model, kind"
        )
    };
    let mut statement = connection.prepare(&sql).map_err(db)?;
    let rows = statement
        .query_map(params_from_iter(values), |row| {
            Ok(FailureCount {
                day: row.get(0)?,
                source: row.get(1)?,
                provider_id: row.get(2)?,
                model: row.get(3)?,
                kind: row.get(4)?,
                count: to_u64(row.get(5)?),
            })
        })
        .map_err(db)?;
    rows.collect::<Result<_, _>>().map_err(db)
}

/// Deletes the daily rows (and failure counters) older than `retention_days` before the local
/// day of `now_ms`, and the minute rows older than `retention_days` plus two (at most
/// [`MINUTE_RETENTION_DAYS`]); returns the number of daily rows deleted. The daily cut is made at
/// UTC and one day is added, so a day inside the retention is never deleted whatever the user's
/// time zone. The three deletes run atomically: in their own immediate transaction, or as a
/// savepoint inside the caller's.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn prune_usage(
    connection: &Connection,
    retention_days: u16,
    now_ms: i64,
) -> Result<u64, AppError> {
    let Some(cutoff) = daily_cutoff(retention_days, now_ms) else {
        return Ok(0);
    };
    super::atomic(connection, |transaction| {
        transaction
            .execute(
                "DELETE FROM llm_usage_failures WHERE day < ?1",
                params![cutoff],
            )
            .map_err(db)?;
        transaction
            .execute(
                "DELETE FROM llm_usage_minute WHERE minute_ms < ?1",
                params![now_ms.saturating_sub(minute_retention_days(retention_days) * MS_PER_DAY)],
            )
            .map_err(db)?;
        let deleted = transaction
            .execute(
                "DELETE FROM llm_usage_daily WHERE day < ?1",
                params![cutoff],
            )
            .map_err(db)?;
        Ok(deleted as u64)
    })
}

/// The day before which [`prune_usage`] deletes daily rows; [`usage_retention_preview`] counts
/// the same rows. `None` when that day falls outside years 0000..=9999.
fn daily_cutoff(retention_days: u16, now_ms: i64) -> Option<String> {
    let oldest_kept = local_day(
        now_ms.saturating_sub(i64::from(retention_days).saturating_mul(MS_PER_DAY)),
        0,
    );
    add_days(&oldest_kept, -1)
}

/// Days the minute rollup keeps under a usage retention of `retention_days`: the
/// retention plus two days of time-zone slack, at most [`MINUTE_RETENTION_DAYS`].
#[must_use]
fn minute_retention_days(retention_days: u16) -> i64 {
    MINUTE_RETENTION_DAYS.min(i64::from(retention_days) + 2)
}

/// What lowering the usage retention to `retention_days` would delete, and how big the rollup is
/// now: the daily rows [`prune_usage`] would remove at `now_ms`, and the table's size
/// in bytes. SQLite's page statistics are not compiled in, so the size is the stored payload of
/// every row (text lengths plus 8 bytes per number) and 16 bytes of row overhead.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn usage_retention_preview(
    connection: &Connection,
    retention_days: u16,
    now_ms: i64,
) -> Result<(u64, u64), AppError> {
    let rows_to_delete = match daily_cutoff(retention_days, now_ms) {
        Some(cutoff) => connection
            .query_row(
                "SELECT COUNT(*) FROM llm_usage_daily WHERE day < ?1",
                params![cutoff],
                |row| row.get::<_, i64>(0),
            )
            .map_err(db)?,
        None => 0,
    };
    // `11 * 8`: the 11 INTEGER columns of llm_usage_daily, 8 bytes each.
    let table_bytes: i64 = connection
        .query_row(
            "SELECT COALESCE(SUM(16 + length(day) + length(source) + length(provider_id) \
                 + length(key_id) + length(model) + length(client_id) + length(alias) \
                 + length(currency) + 11 * 8), 0) \
             FROM llm_usage_daily",
            [],
            |row| row.get(0),
        )
        .map_err(db)?;
    Ok((to_u64(rows_to_delete), to_u64(table_bytes)))
}

#[cfg(test)]
mod tests;
