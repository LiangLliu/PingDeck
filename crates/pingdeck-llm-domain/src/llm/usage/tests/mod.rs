//! Tests of the usage, cost and price model, one file per concern, sharing the fixtures below.

mod basis;
mod cost;
mod day;
mod price;
mod rollup;
mod summary;

use crate::llm::usage::{CostUsage, Price, PriceSource, local_midnight_ms};

/// Milliseconds in one day.
pub(super) const DAY_MS: i64 = 86_400_000;

/// Unix ms of the local midnight that starts `day`.
pub(super) fn ms(day: &str, tz: i16) -> i64 {
    local_midnight_ms(day, tz).unwrap()
}

/// A per-token USD override price.
pub(super) fn price(input: f64, output: f64) -> Price {
    Price {
        input_per_mtok: input,
        output_per_mtok: output,
        cache_read_per_mtok: None,
        cache_write_per_mtok: None,
        reasoning_per_mtok: None,
        currency: "USD".to_owned(),
        source: PriceSource::Override,
        per_call: None,
    }
}

/// Reported input and output tokens, nothing cached.
pub(super) fn usage(input: u64, output: u64) -> CostUsage {
    CostUsage {
        input_tokens: input,
        output_tokens: output,
        ..CostUsage::default()
    }
}
