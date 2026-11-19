//! What a price costs: the pure cost function over token counts or a per-call price, and how a
//! finished call is booked when its price is known, free or unknown.
//!
//! Cost is integer millionths of the price's currency. Costs are never converted or summed across
//! currencies.

use serde::{Deserialize, Serialize};

use super::price::{Price, PriceSource};
use crate::llm::hub::HubUsage;
use crate::llm::providers::Provider;
use crate::llm::wire::AuthStyle;
use pingdeck_model::model_catalog::MAX_PRICE_PER_MTOK;

/// Price resolution: prices are rounded to millionths of a currency unit per million tokens.
const PRICE_SCALE: f64 = 1_000_000.0;
/// Cost is `tokens * scaled price / COST_DIVISOR` millionths (`1e6` tokens per Mtok).
const COST_DIVISOR: i128 = 1_000_000;
/// A one-hour cache write costs this many times the base input price (Anthropic pricing).
const LONG_WRITE_INPUT_MULTIPLE: i128 = 2;

/// Token counts of one request, as billed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct CostUsage {
    /// Prompt tokens, cached and cache-write tokens included.
    pub input_tokens: u64,
    /// Generated tokens, reasoning included.
    pub output_tokens: u64,
    /// Prompt tokens read from the provider's cache.
    pub cached_tokens: u64,
    /// Prompt tokens written to the provider's cache.
    pub cache_write_tokens: u64,
    /// The part of `cache_write_tokens` written at the one-hour tier.
    #[serde(default)]
    pub cache_write_long_tokens: u64,
    /// Reasoning tokens (a subset of `output_tokens`).
    pub reasoning_tokens: u64,
    /// The counts were estimated locally rather than reported by the provider.
    pub tokens_estimated: bool,
}

impl CostUsage {
    /// The reported usage, `None` when the provider reported neither input nor output tokens
    /// (the cost is then unknown, not zero).
    #[must_use]
    pub fn from_hub(usage: &HubUsage) -> Option<Self> {
        if usage.input_tokens.is_none() && usage.output_tokens.is_none() {
            return None;
        }
        Some(Self {
            input_tokens: usage.input_tokens.unwrap_or(0),
            output_tokens: usage.output_tokens.unwrap_or(0),
            cached_tokens: usage.cached_input_tokens.unwrap_or(0),
            cache_write_tokens: usage.cache_write_tokens.unwrap_or(0),
            cache_write_long_tokens: usage.cache_write_long_tokens.unwrap_or(0),
            reasoning_tokens: usage.reasoning_tokens.unwrap_or(0),
            tokens_estimated: false,
        })
    }
}

/// The cost of one request.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct Cost {
    /// Millionths of `currency` (stored in `cost_micro_usd` columns whatever the currency).
    pub micro: i64,
    /// ISO 4217 code of the price used.
    pub currency: String,
    /// Where the price came from (`llm_request_log.price_source`).
    pub source: PriceSource,
    /// The figure is an estimate: a catalog price or locally estimated token counts.
    pub estimated: bool,
}

/// Price scaled to an integer; garbage (negative, NaN, infinite) counts as free so a corrupt
/// price can never panic or overflow.
fn scaled(price: f64) -> i128 {
    if price.is_finite() && price > 0.0 {
        // Saturating float-to-int cast; prices are capped far below `i128::MAX`.
        (price.min(MAX_PRICE_PER_MTOK) * PRICE_SCALE).round() as i128
    } else {
        0
    }
}

/// Cost of one **successful** request with `usage` at `price`, in millionths of the price's
/// currency, rounded half up once at the end (components are summed exactly in `i128` first).
/// The result saturates at `i64::MAX`.
///
/// A fixed price (`per_call`) is the whole cost whatever the token counts. Otherwise, chosen so
/// spend is never under-reported:
/// - fresh input = `input - cached - cache_write` at the input price; `cached` is clamped to
///   `input` and `cache_write` to what is left, so a provider that reports more cache tokens
///   than prompt tokens never yields a negative or double-counted figure;
/// - cache-read and cache-write tokens use their own price, or the input price when none is
///   set (a provider without a cache discount bills them in full); the one-hour part of the
///   cache writes costs `LONG_WRITE_INPUT_MULTIPLE` (2) times the input price (Anthropic);
/// - reasoning tokens (clamped to `output`) use the reasoning price, or the output price when
///   none is set; the rest of `output` uses the output price.
///
/// Unknown token counts are not this function's concern: callers pass `None` upstream and store
/// a NULL cost (see [`CostUsage::from_hub`]); [`request_cost`] adds that and the failure rule.
#[must_use]
pub fn cost(usage: &CostUsage, price: &Price) -> Cost {
    if let Some(per_call) = price.per_call {
        return Cost {
            micro: i64::try_from(scaled(per_call)).unwrap_or(i64::MAX),
            currency: price.currency.clone(),
            source: price.source,
            estimated: false,
        };
    }
    let input = scaled(price.input_per_mtok);
    let output = scaled(price.output_per_mtok);
    let cached = usage.cached_tokens.min(usage.input_tokens);
    let cache_write = usage.cache_write_tokens.min(usage.input_tokens - cached);
    let fresh = usage.input_tokens - cached - cache_write;
    let long_write = usage.cache_write_long_tokens.min(cache_write);
    let reasoning = usage.reasoning_tokens.min(usage.output_tokens);
    let plain_output = usage.output_tokens - reasoning;
    let component = |tokens: u64, scaled_price: i128| i128::from(tokens) * scaled_price;
    let total = component(fresh, input)
        + component(cached, price.cache_read_per_mtok.map_or(input, scaled))
        + component(
            cache_write - long_write,
            price.cache_write_per_mtok.map_or(input, scaled),
        )
        + component(long_write, input * LONG_WRITE_INPUT_MULTIPLE)
        + component(plain_output, output)
        + component(reasoning, price.reasoning_per_mtok.map_or(output, scaled));
    let micro = (total + COST_DIVISOR / 2) / COST_DIVISOR;
    Cost {
        micro: i64::try_from(micro).unwrap_or(i64::MAX),
        currency: price.currency.clone(),
        source: price.source,
        estimated: usage.tokens_estimated || price.source == PriceSource::Catalog,
    }
}

/// The cost a finished request is booked with. A fixed price makes the cost known without token
/// counts: `succeeded` pays the per-call price, a failed request costs 0. A per-token price needs
/// the counts: `None` without them (the cost is unknown, not zero), and the same figure whether
/// or not the request succeeded.
#[must_use]
pub fn request_cost(usage: Option<&CostUsage>, price: &Price, succeeded: bool) -> Option<Cost> {
    if price.per_call.is_some() {
        let mut cost = cost(&CostUsage::default(), price);
        if !succeeded {
            cost.micro = 0;
        }
        return Some(cost);
    }
    usage.map(|usage| cost(usage, price))
}

/// Why the price of a model is unknown.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum UnknownReason {
    /// Neither the provider model nor the catalog knows the model.
    NotInCatalog,
    /// The catalog knows the model but lists no price for it.
    CatalogWithoutPrice,
}

/// What a model costs per call, in three states: a known price (an all-zero official one
/// included), free (a local runtime that needs no key), or unknown. Unknown is never free.
#[derive(Debug, Clone, PartialEq)]
pub enum PriceResolution {
    /// A price exists.
    Known(Price),
    /// A no-key provider on a local origin ([`is_free_provider`]).
    Free,
    /// No price.
    Unknown(UnknownReason),
}

impl Default for PriceResolution {
    fn default() -> Self {
        Self::Unknown(UnknownReason::NotInCatalog)
    }
}

impl PriceResolution {
    /// The price, when known.
    #[must_use]
    pub fn known(&self) -> Option<&Price> {
        match self {
            Self::Known(price) => Some(price),
            _ => None,
        }
    }
}

/// What one finished call is booked at.
#[derive(Debug, Clone, PartialEq, Eq)]
pub enum BookedCost {
    /// Priced; `Cost.estimated` marks catalog prices and estimated tokens.
    Known(Cost),
    /// A local runtime: nothing to pay.
    Free,
    /// No price for the model.
    Unknown(UnknownReason),
    /// Priced per token, but the upstream reported no usage.
    NoUsage,
}

impl BookedCost {
    /// The word stored in `llm_request_log.price_source` for this booking: the price's own
    /// source when known, else why it is free or unknown.
    #[must_use]
    pub fn price_source(&self) -> &'static str {
        match self {
            Self::Known(cost) => cost.source.as_str(),
            Self::Free => "local_free",
            Self::Unknown(UnknownReason::NotInCatalog) => "not_in_catalog",
            Self::Unknown(UnknownReason::CatalogWithoutPrice) => "catalog_without_price",
            Self::NoUsage => "no_usage",
        }
    }
}

/// Books one finished call at `price` (the arithmetic of [`request_cost`]).
#[must_use]
pub fn book_cost(
    price: &PriceResolution,
    usage: Option<&CostUsage>,
    succeeded: bool,
) -> BookedCost {
    match price {
        PriceResolution::Known(price) => {
            request_cost(usage, price, succeeded).map_or(BookedCost::NoUsage, BookedCost::Known)
        }
        PriceResolution::Free => BookedCost::Free,
        PriceResolution::Unknown(reason) => BookedCost::Unknown(*reason),
    }
}

/// Whether `origin` (a provider base URL) names a loopback or private host.
fn origin_is_local(origin: &str) -> bool {
    url::Url::parse(origin)
        .ok()
        .and_then(|url| url.host_str().map(pingdeck_base::net_guard::is_local_host))
        .unwrap_or(false)
}

/// The one definition of a free provider: a local runtime (loopback or private address) that
/// needs no key. Its models cost nothing; they are never "unpriced".
#[must_use]
pub fn is_free_provider(provider: &Provider) -> bool {
    provider.auth == AuthStyle::None && origin_is_local(&provider.base_url)
}
