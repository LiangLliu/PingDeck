//! The request-log figures of the Overview "Health" card.
//!
//! Single responsibility: four indexed reads over `llm_request_log` for one provider and the last
//! 24 hours (counts, streamed TTFTs, 429 buckets, the timings of the last successful requests).
//! Medians and percentiles are the caller's: this file only reads.

use rusqlite::{Connection, params};

use super::{db, to_u64};
use crate::llm::capture_record::TimingJson;
use crate::sqlite::i64_from_usize;
use spanbay_base::error::AppError;

const HOUR_MS: i64 = 60 * 60 * 1000;
/// The window every figure covers: the 24 hours before now.
pub const HEALTH_WINDOW_MS: i64 = 24 * HOUR_MS;
/// Hourly buckets of the 429 timeline.
pub const HEALTH_HOURS: usize = 24;
/// Successful requests the "Typical request" strip is the median of.
pub const TYPICAL_REQUESTS: usize = 20;

/// What the log says about one provider over the last 24 hours.
#[derive(Debug, Clone, PartialEq, Eq, Default)]
pub struct ProviderLogStats {
    /// Forwarded requests (sources gateway, agent and api) of the window; cancellations are left
    /// out, as in the usage failure rate.
    pub requests: u64,
    /// Of those, the ones that succeeded by the rule of `llm::usage::request_succeeded`: outcome
    /// `success`, status below 400, not truncated.
    pub succeeded: u64,
    /// TTFT of the successful streamed requests, unordered.
    pub ttft_ms: Vec<u64>,
    /// Requests answered 429 per hour, index 0 being the last hour (`now - 1 h .. now`).
    pub rate_limited: [u32; HEALTH_HOURS],
    /// Timings of the last [`TYPICAL_REQUESTS`] successful requests that stored one, newest
    /// first.
    pub timings: Vec<TimingJson>,
}

// Served by `idx_llm_request_log_provider` (provider, time); `source` is only a filter.
// `succeeded` mirrors `request_succeeded`; keep the two in step.
const COUNTS_SQL: &str = "SELECT COUNT(*), COALESCE(SUM(outcome = 'success' AND status < 400 \
         AND COALESCE(failure_kind, '') <> 'truncated'), 0) \
     FROM llm_request_log WHERE provider_id = ?1 AND ts_ms >= ?2 \
       AND +source IN ('gateway', 'agent', 'api') AND outcome <> 'canceled'";
const TTFT_SQL: &str = "SELECT ttft_ms FROM llm_request_log \
     WHERE provider_id = ?1 AND ts_ms >= ?2 AND +source IN ('gateway', 'agent', 'api') \
       AND stream = 1 AND outcome = 'success' AND ttft_ms IS NOT NULL";
// `3600000` is HOUR_MS: SQL text is a const, so the literal is spelled out.
const RATE_LIMITED_SQL: &str = "SELECT (?3 - ts_ms) / 3600000, COUNT(*) FROM llm_request_log \
     WHERE provider_id = ?1 AND ts_ms >= ?2 AND status = 429 \
       AND +source IN ('gateway', 'agent', 'api') GROUP BY 1";
const TIMING_SQL: &str = "SELECT timing_json FROM llm_request_log \
     WHERE provider_id = ?1 AND ts_ms >= ?2 AND +source IN ('gateway', 'agent', 'api') \
       AND outcome = 'success' AND timing_json IS NOT NULL ORDER BY ts_ms DESC LIMIT ?3";

/// The 429 answers of `provider_id` since `since`, per hour before `now_ms` (index 0 is the last
/// hour); a request exactly 24 hours old falls past the last bucket and is left out.
fn rate_limited_by_hour(
    connection: &Connection,
    provider_id: &str,
    since: i64,
    now_ms: i64,
) -> Result<[u32; HEALTH_HOURS], AppError> {
    let mut rate_limited = [0_u32; HEALTH_HOURS];
    let buckets = connection
        .prepare_cached(RATE_LIMITED_SQL)
        .map_err(db)?
        .query_map(params![provider_id, since, now_ms], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(db)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db)?;
    for (hours_ago, count) in buckets {
        if let Some(slot) = usize::try_from(hours_ago)
            .ok()
            .and_then(|index| rate_limited.get_mut(index))
        {
            *slot = u32::try_from(count).unwrap_or(u32::MAX);
        }
    }
    Ok(rate_limited)
}

/// The Health-card figures of `provider_id` over the 24 hours before `now_ms`.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn provider_log_stats(
    connection: &Connection,
    provider_id: &str,
    now_ms: i64,
) -> Result<ProviderLogStats, AppError> {
    let since = now_ms.saturating_sub(HEALTH_WINDOW_MS);
    let (requests, succeeded) = connection
        .prepare_cached(COUNTS_SQL)
        .map_err(db)?
        .query_row(params![provider_id, since], |row| {
            Ok((row.get::<_, i64>(0)?, row.get::<_, i64>(1)?))
        })
        .map_err(db)?;
    let ttft_ms = connection
        .prepare_cached(TTFT_SQL)
        .map_err(db)?
        .query_map(params![provider_id, since], |row| {
            row.get::<_, i64>(0).map(to_u64)
        })
        .map_err(db)?
        .collect::<Result<_, _>>()
        .map_err(db)?;
    let rate_limited = rate_limited_by_hour(connection, provider_id, since, now_ms)?;
    let texts = connection
        .prepare_cached(TIMING_SQL)
        .map_err(db)?
        .query_map(
            params![provider_id, since, i64_from_usize(TYPICAL_REQUESTS)],
            |row| row.get::<_, String>(0),
        )
        .map_err(db)?
        .collect::<Result<Vec<_>, _>>()
        .map_err(db)?;
    Ok(ProviderLogStats {
        requests: to_u64(requests),
        succeeded: to_u64(succeeded),
        ttft_ms,
        rate_limited,
        // A timing that no longer parses only leaves the strip with fewer samples.
        timings: texts
            .iter()
            .filter_map(|text| serde_json::from_str(text).ok())
            .collect(),
    })
}

#[cfg(test)]
mod tests;
