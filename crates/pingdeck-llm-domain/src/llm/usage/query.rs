//! What the Usage tab asks for: the query, the sources and dimensions it names, and the slot
//! and scope rules [`summarize`](super::summarize) applies.

use serde::{Deserialize, Serialize};

use super::day::{bucket_range, day_range, day_window_bounds};
use super::rollup::RollupRow;
#[cfg(doc)]
use super::summary::UsageSummary;

/// Who made a request (`llm_usage_daily.source`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, PartialOrd, Ord, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageSource {
    /// A client of the local gateway.
    Gateway,
    /// The AI chat agent.
    Agent,
    /// The API client (including its AI mode).
    Api,
    /// Provider probes and benchmarks.
    Probe,
}

/// The sources shown when a query names none: everything except probes.
pub const DEFAULT_SOURCES: [UsageSource; 3] =
    [UsageSource::Gateway, UsageSource::Agent, UsageSource::Api];

impl UsageSource {
    /// Every source, in the order of the stored identifiers.
    pub const ALL: [Self; 4] = [Self::Gateway, Self::Agent, Self::Api, Self::Probe];

    /// The value stored in `source` columns.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Gateway => "gateway",
            Self::Agent => "agent",
            Self::Api => "api",
            Self::Probe => "probe",
        }
    }

    /// Parses [`UsageSource::as_str`] output.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        Self::ALL.into_iter().find(|source| source.as_str() == text)
    }
}

/// A dimension [`UsageSummary::groups`] can be split by.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum UsageGroup {
    /// Local calendar day.
    Day,
    /// Provider id.
    Provider,
    /// Key id.
    Key,
    /// Upstream model.
    Model,
    /// Request source.
    Source,
    /// Gateway client id.
    Client,
    /// Alias the client used.
    Alias,
    /// Price currency.
    Currency,
}

impl UsageGroup {
    /// The row's value of this dimension.
    pub(super) fn value(self, row: &RollupRow) -> &str {
        match self {
            Self::Day => &row.day,
            Self::Provider => &row.provider_id,
            Self::Key => &row.key_id,
            Self::Model => &row.model,
            Self::Source => &row.source,
            Self::Client => &row.client_id,
            Self::Alias => &row.alias,
            Self::Currency => &row.currency,
        }
    }

    /// The `llm_usage_daily` column of the dimension (a fixed identifier, safe to splice into SQL).
    #[must_use]
    pub const fn column(self) -> &'static str {
        match self {
            Self::Day => "day",
            Self::Provider => "provider_id",
            Self::Key => "key_id",
            Self::Model => "model",
            Self::Source => "source",
            Self::Client => "client_id",
            Self::Alias => "alias",
            Self::Currency => "currency",
        }
    }
}

/// The successful calls of one place (source, provider, key, model, client) and one transport in
/// the request log, with their end-to-end and first-token sums (`llm_usage_latency_modes_get`): the
/// Usage latency card splits streamed from unary calls and summarises time to first token, which
/// the rollups do not keep.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageLatencyMode {
    /// Who made the calls (`UsageSource` wire name).
    pub source: String,
    /// Provider that answered; empty when none.
    pub provider_id: String,
    /// Key used; empty when none.
    pub key_id: String,
    /// Upstream model, else the requested one (the rollups' `model`).
    pub model: String,
    /// Gateway client id; empty for other sources.
    pub client_id: String,
    /// Whether the calls streamed their answer.
    pub stream: bool,
    /// Successful calls in `latency_ms`.
    pub timed_requests: u64,
    /// Sum of their end-to-end latencies, ms.
    pub latency_ms: u64,
    /// Calls among them that recorded a time to first token (streamed calls only).
    pub ttft_requests: u64,
    /// Sum of those times to first token, ms.
    pub ttft_ms: u64,
}

/// What the Usage tab asks for (`llm_usage_summary`).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageQuery {
    /// Only this provider.
    #[serde(default)]
    pub provider_id: Option<String>,
    /// Only this key.
    #[serde(default)]
    pub key_id: Option<String>,
    /// Only this gateway client.
    #[serde(default)]
    pub client_id: Option<String>,
    /// Start of the range, Unix ms (inclusive).
    pub from_ms: i64,
    /// End of the range, Unix ms (exclusive).
    pub to_ms: i64,
    /// Extra splits for [`UsageSummary::groups`]; the series and the model table always exist.
    #[serde(default)]
    pub group_by: Vec<UsageGroup>,
    /// Sources to count; empty means [`DEFAULT_SOURCES`].
    #[serde(default)]
    pub sources: Vec<UsageSource>,
    /// Webview UTC offset in minutes (east positive).
    #[serde(default)]
    pub tz_offset_minutes: i16,
    /// Minute-rollup mode: `Some(n)` reads `llm_usage_minute` for exactly `[from_ms, to_ms)` (by
    /// the start of each stored minute) and cuts the series, and the `day` group, into local
    /// buckets of `n` minutes (1..=1440) labelled `YYYY-MM-DDTHH:MM`. `None` reads the daily
    /// rollup by local day.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub bucket_minutes: Option<u32>,
}

impl UsageQuery {
    /// The sources that count: the named ones, or [`DEFAULT_SOURCES`].
    #[must_use]
    pub fn effective_sources(&self) -> Vec<UsageSource> {
        if self.sources.is_empty() {
            DEFAULT_SOURCES.to_vec()
        } else {
            self.sources.clone()
        }
    }

    /// First and last local day (inclusive) the range covers, capped at `MAX_RANGE_DAYS` (the
    /// newest days kept); `None` for an empty range. Store SQL filters `day BETWEEN` these.
    #[must_use]
    pub fn day_window(&self) -> Option<(String, String)> {
        day_window_bounds(self.from_ms, self.to_ms, self.tz_offset_minutes)
    }

    /// The series slots, oldest first: local days, or bucket labels in minute-rollup mode. A row
    /// counts when its `day` is one of them.
    #[must_use]
    pub fn slots(&self) -> Vec<String> {
        match self.bucket_minutes {
            Some(minutes) => {
                bucket_range(self.from_ms, self.to_ms, self.tz_offset_minutes, minutes)
            }
            None => day_range(self.from_ms, self.to_ms, self.tz_offset_minutes),
        }
    }

    /// Whether `row` lies within the provider, key and client the query names.
    pub(super) fn scope_matches(&self, row: &RollupRow) -> bool {
        let is =
            |wanted: &Option<String>, actual: &str| wanted.as_deref().is_none_or(|w| w == actual);
        is(&self.provider_id, &row.provider_id)
            && is(&self.key_id, &row.key_id)
            && is(&self.client_id, &row.client_id)
    }

    /// Whether `provider_id` lies within the query's provider: the only scope failure counters
    /// know (they have no key or client).
    pub(super) fn provider_in_scope(&self, provider_id: &str) -> bool {
        self.provider_id
            .as_deref()
            .is_none_or(|provider| provider == provider_id)
    }
}
