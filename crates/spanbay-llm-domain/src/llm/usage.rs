//! Usage, cost and price model of the LLM layer.
//!
//! Pure logic for what requests cost and how usage is bucketed and summarised: prices and
//! overrides, the price precedence, the cost function, token estimation, local-day arithmetic,
//! the `llm_usage_daily` row/delta types and the summary fold. No SQL, no I/O; the store reads
//! and writes the structs defined here.

mod basis;
mod cost;
mod day;
mod price;
mod query;
mod rollup;
mod summary;

#[cfg(test)]
mod tests;

// Named by the tests through the facade.
#[cfg(test)]
use rollup::UNPRICED_CURRENCY;

pub use basis::{
    Offer, Priced, PricingRows, apply_relay, decide, default_price, official_family, preset_family,
};
pub use cost::{
    BookedCost, Cost, CostUsage, PriceResolution, UnknownReason, book_cost, cost, is_free_provider,
    request_cost,
};
pub use day::{
    MAX_BUCKETS, MAX_TZ_OFFSET_MINUTES, add_days, bucket_range, bucket_start_ms, local_day,
    local_midnight_ms,
};
pub(crate) use price::default_currency;
pub use price::{Price, PriceOverride, PriceSource};
pub use query::{DEFAULT_SOURCES, UsageGroup, UsageLatencyMode, UsageQuery, UsageSource};
pub use rollup::{
    RequestOutcome, RollupDelta, RollupInput, RollupRow, delta_for, request_succeeded,
};
pub use summary::{
    FailureCount, SummaryContext, TopFailure, UsageBucket, UsageDay, UsageModelRow, UsageRow,
    UsageSummary, UsageToday, summarize,
};

/// Tokens of `text` by the one estimate (`spanbay_model::tokens`), used wherever the provider
/// reports none.
#[must_use]
pub fn estimate_tokens(text: &str) -> u64 {
    spanbay_model::tokens::text_tokens(text)
}

/// [`estimate_tokens`] for raw bytes (invalid UTF-8 counts like any non-ASCII byte).
#[must_use]
pub fn estimate_tokens_bytes(bytes: &[u8]) -> u64 {
    spanbay_model::tokens::bytes_tokens(bytes)
}
