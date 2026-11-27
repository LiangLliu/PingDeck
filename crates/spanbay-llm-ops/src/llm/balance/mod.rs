//! Account balance and quota readings of providers and gateways.
//!
//! A check asks a provider's balance endpoint (vendor, gateway family or custom query) through
//! the safe transport and normalises the answer into a [`BalanceReport`]. Code adapters live in
//! one file each, declarative ones in `fixtures/llm-balance-probes.json`. The refresh, throttle
//! and keep-last-good rules ([`refresh_due`], [`BalanceThrottle`], [`StoredBalanceMerge`]) are
//! pure functions for the use-case layer. The stored shapes (report, stored balance, custom
//! query) live in `spanbay_llm_domain::llm::balance_state`.

mod adapter_id;
mod custom;
mod declarative;
mod deepseek;
mod error;
mod extract;
mod fetch;
mod gateway;
mod litellm;
mod newapi;
mod oneapi;
mod openrouter;
mod policy;
mod report;
mod rules;
mod select;
mod sub2api;
mod target;
#[cfg(test)]
mod tests;
mod voapi;

pub use adapter_id::AdapterId;
pub use custom::{CustomQueryTest, test_custom_query};
pub use error::BalanceError;
pub(crate) use spanbay_llm_domain::llm::balance_state::{
    BalanceAlt, BalanceReport, BalanceScope, BalanceUnit, BalanceWindow,
};
// Named by the tests through the module.
pub use policy::{
    BalanceThrottle, RefreshInput, RefreshTrigger, StoredBalanceMerge, ThrottleVerdict, refresh_due,
};
pub use select::{query_balance, select_adapter};
#[cfg(test)]
pub(crate) use spanbay_llm_domain::llm::balance_state::{
    BalanceAuth, BalanceErrorKind, CustomBalanceQuery, CustomHeaderRef, Evidence, ResolvedHeaders,
    UnitName,
};
pub use target::{BalanceOutcome, BalanceTarget};

/// Longest response body an adapter reads.
pub const MAX_BODY_BYTES: usize = 256 * 1024;
