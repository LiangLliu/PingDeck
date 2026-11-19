//! The `llm_usage_daily` row, and what one finished request adds to it.
//!
//! Plain data for the daily rollup and the pure [`delta_for`] the log writer applies in the same
//! transaction as the log insert. No SQL here.
//!
//! Storage conventions the store layer must follow:
//! - `cost_micro` is the `cost_micro_usd` column: millionths of the row's `currency`.
//! - `currency` is [`UNPRICED_CURRENCY`] (`""`) when the model had no price. Such a row still
//!   counts requests and tokens; its cost stays 0 and the summary lists the model as unpriced.
//! - The exact failure counts live in `llm_usage_failures` ([`RollupDelta::failure_kind`] is what
//!   the store counts there).
//! - `day` is the local calendar day under the UTC offset in force when the request was made
//!   (`tz_offset_minutes` of [`RollupInput`]). There is no timezone database: the writer passes
//!   the offset it knows for that instant, so a day that spans a DST change is 23 or 25 hours
//!   long and its boundaries follow the offset used for each request.

use serde::{Deserialize, Serialize};

use super::cost::{Cost, CostUsage};
use super::day::local_day;
use super::query::UsageSource;
use crate::llm::failure::FailureKind;

/// `currency` of a row whose model had no price.
pub const UNPRICED_CURRENCY: &str = "";

/// One `llm_usage_daily` row.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollupRow {
    /// Local calendar day `YYYY-MM-DD`.
    pub day: String,
    /// A [`UsageSource::as_str`] value.
    pub source: String,
    /// Provider id (empty when the request failed before routing).
    pub provider_id: String,
    /// Key id (empty when none was chosen).
    pub key_id: String,
    /// Upstream model id.
    pub model: String,
    /// Gateway client id (empty for non-gateway sources).
    pub client_id: String,
    /// Alias the client asked for (empty when it used a provider/model id).
    pub alias: String,
    /// Currency of `cost_micro`, `UNPRICED_CURRENCY` (`""`) when unpriced.
    pub currency: String,
    /// Requests finished.
    pub requests: u64,
    /// Requests that succeeded.
    pub ok_requests: u64,
    /// Prompt tokens (cached ones included).
    pub input_tokens: u64,
    /// Generated tokens (reasoning included).
    pub output_tokens: u64,
    /// Prompt tokens read from the provider's cache.
    pub cached_tokens: u64,
    /// Prompt tokens written to the provider's cache.
    pub cache_write_tokens: u64,
    /// Reasoning tokens.
    pub reasoning_tokens: u64,
    /// Millionths of `currency` (column `cost_micro_usd`).
    pub cost_micro: i64,
    /// Requests the client or the app cancelled (counted in `requests`, not in `ok_requests`).
    #[serde(default)]
    pub canceled: u64,
}

impl RollupRow {
    /// Requests that did not succeed.
    #[must_use]
    pub const fn failed(&self) -> u64 {
        self.requests.saturating_sub(self.ok_requests)
    }

    /// Adds `delta` to this row (same key assumed); counters saturate.
    pub fn apply(&mut self, delta: &RollupDelta) {
        self.requests = self.requests.saturating_add(delta.requests);
        self.ok_requests = self.ok_requests.saturating_add(delta.ok_requests);
        self.input_tokens = self.input_tokens.saturating_add(delta.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(delta.output_tokens);
        self.cached_tokens = self.cached_tokens.saturating_add(delta.cached_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(delta.cache_write_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(delta.reasoning_tokens);
        self.cost_micro = self.cost_micro.saturating_add(delta.cost_micro);
        if delta.failure_kind.as_deref() == Some(FailureKind::Cancelled.as_str()) {
            self.canceled = self.canceled.saturating_add(delta.requests);
        }
    }
}

/// How a request ended, as the log's `outcome` column says.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum RequestOutcome {
    /// A response was delivered.
    Success,
    /// No usable response was delivered.
    Failure,
    /// The client or the app cancelled the request.
    Canceled,
}

impl RequestOutcome {
    /// The log's `outcome` text (the serde name).
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Success => "success",
            Self::Failure => "failure",
            Self::Canceled => "canceled",
        }
    }

    /// Reads the log's `outcome` text: `success` and `canceled` as such, anything else is a
    /// failure (an unknown outcome is never counted as a success).
    #[must_use]
    pub fn from_log(text: &str) -> Self {
        match text {
            "success" => Self::Success,
            "canceled" => Self::Canceled,
            _ => Self::Failure,
        }
    }
}

/// Everything the rollup needs to know about one finished request.
#[derive(Debug, Clone, Copy)]
pub struct RollupInput<'a> {
    /// When the request arrived, Unix ms (picks the local day).
    pub ts_ms: i64,
    /// Webview UTC offset in minutes (east positive).
    pub tz_offset_minutes: i16,
    /// Who made the request.
    pub source: UsageSource,
    /// Provider that answered or was last tried; empty when none.
    pub provider_id: &'a str,
    /// Key used; empty when none.
    pub key_id: &'a str,
    /// Upstream model; the requested model when routing failed.
    pub model: &'a str,
    /// Gateway client id; empty for other sources.
    pub client_id: &'a str,
    /// Alias the client used; empty when none.
    pub alias: &'a str,
    /// HTTP status finally returned to the caller; `None` when no response was produced.
    pub final_status: Option<u16>,
    /// How the request ended.
    pub outcome: RequestOutcome,
    /// Why it failed; ignored when the request succeeded. A cancelled request always counts
    /// as [`FailureKind::Cancelled`].
    pub failure_kind: Option<FailureKind>,
    /// Token counts, `None` when the provider reported none.
    pub usage: Option<&'a CostUsage>,
    /// Cost, `None` when unknown (no price or no usage).
    pub cost: Option<&'a Cost>,
    /// Currency of the resolved price, `None` when the model has no price. Used when `cost`
    /// is `None` so a priced model without usage is not listed as unpriced.
    pub price_currency: Option<&'a str>,
}

impl RollupInput<'_> {
    /// Whether the request counts as succeeded ([`request_succeeded`]).
    #[must_use]
    pub fn succeeded(&self) -> bool {
        request_succeeded(self.outcome, self.final_status, self.failure_kind)
    }
}

/// The one definition of a failed request: the caller did not get a complete answer. A request
/// succeeded when its outcome is [`RequestOutcome::Success`], the final status (if any) is below
/// 400 and the answer was not cut short ([`FailureKind::Truncated`]).
/// A served failover writes `success`; a stream that broke after a failover writes `failure` and
/// counts as failed. The health card and the summaries read the same rule.
#[must_use]
pub fn request_succeeded(
    outcome: RequestOutcome,
    final_status: Option<u16>,
    failure_kind: Option<FailureKind>,
) -> bool {
    outcome == RequestOutcome::Success
        && final_status.is_none_or(|status| status < 400)
        && failure_kind != Some(FailureKind::Truncated)
}

/// What one request adds to its day row: the row key plus the counter increments.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct RollupDelta {
    /// Local day of the request.
    pub day: String,
    /// Source identifier.
    pub source: String,
    /// Provider id.
    pub provider_id: String,
    /// Key id.
    pub key_id: String,
    /// Upstream model.
    pub model: String,
    /// Gateway client id.
    pub client_id: String,
    /// Alias used.
    pub alias: String,
    /// Currency of `cost_micro`, `UNPRICED_CURRENCY` (`""`) when unpriced.
    pub currency: String,
    /// Always 1.
    pub requests: u64,
    /// 1 when the request succeeded.
    pub ok_requests: u64,
    /// Prompt tokens.
    pub input_tokens: u64,
    /// Generated tokens.
    pub output_tokens: u64,
    /// Cache-read tokens.
    pub cached_tokens: u64,
    /// Cache-write tokens.
    pub cache_write_tokens: u64,
    /// Reasoning tokens.
    pub reasoning_tokens: u64,
    /// Cost increment in millionths of `currency` (0 when unknown).
    pub cost_micro: i64,
    /// Failure kind of a request that did not succeed. The store counts it exactly in
    /// `llm_usage_failures`.
    pub failure_kind: Option<String>,
}

/// What `input` adds to its `llm_usage_daily` row.
#[must_use]
pub fn delta_for(input: &RollupInput<'_>) -> RollupDelta {
    let usage = input.usage.copied().unwrap_or_default();
    let ok = input.succeeded();
    let failure_kind = if input.outcome == RequestOutcome::Canceled {
        FailureKind::Cancelled
    } else {
        input.failure_kind.unwrap_or(FailureKind::Unknown)
    };
    let currency = input
        .cost
        .map(|cost| cost.currency.as_str())
        .or(input.price_currency)
        .unwrap_or(UNPRICED_CURRENCY);
    RollupDelta {
        day: local_day(input.ts_ms, input.tz_offset_minutes),
        source: input.source.as_str().to_owned(),
        provider_id: input.provider_id.to_owned(),
        key_id: input.key_id.to_owned(),
        model: input.model.to_owned(),
        client_id: input.client_id.to_owned(),
        alias: input.alias.to_owned(),
        currency: currency.to_owned(),
        requests: 1,
        ok_requests: u64::from(ok),
        input_tokens: usage.input_tokens,
        output_tokens: usage.output_tokens,
        cached_tokens: usage.cached_tokens,
        cache_write_tokens: usage.cache_write_tokens,
        reasoning_tokens: usage.reasoning_tokens,
        cost_micro: input.cost.map_or(0, |cost| cost.micro),
        failure_kind: (!ok).then(|| failure_kind.as_str().to_owned()),
    }
}

impl From<&RollupDelta> for RollupRow {
    /// The row a first request creates.
    fn from(delta: &RollupDelta) -> Self {
        let mut row = Self {
            day: delta.day.clone(),
            source: delta.source.clone(),
            provider_id: delta.provider_id.clone(),
            key_id: delta.key_id.clone(),
            model: delta.model.clone(),
            client_id: delta.client_id.clone(),
            alias: delta.alias.clone(),
            currency: delta.currency.clone(),
            ..Self::default()
        };
        row.apply(delta);
        row
    }
}
