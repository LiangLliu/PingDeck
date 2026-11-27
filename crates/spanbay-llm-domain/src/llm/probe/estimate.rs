//! The cost estimate shown before a run and the rule for its confirmation dialog.
//!
//! Single responsibility: sum the token budget of the planned checks, price it with the
//! caller's lookup (the effective price, [`crate::llm::usage::decide`]) and decide
//! whether the user must confirm. Free checks add nothing; the context test has its own typed
//! warning in `spanbay-llm` and is never estimated here.

use std::borrow::Cow;
use std::collections::{BTreeMap, HashSet};
use std::fmt::Write as _;

use serde::{Deserialize, Serialize};

use super::ProbeTest;
use super::plan::{PlanItem, ProbePlan, RowPlan};
use crate::llm::usage::{CostUsage, Price, cost};

/// A confirmation is needed above this many estimated tokens.
const CONFIRM_ABOVE_TOKENS: u64 = 100_000;
/// ... or above this many selected models.
const CONFIRM_ABOVE_MODELS: u32 = 50;
/// ... or, when any selected model has no price, above this many tokens.
const CONFIRM_UNPRICED_ABOVE_TOKENS: u64 = 20_000;
/// The cost threshold in millionths of a unit of USD and of every currency without its own rule.
const CONFIRM_ABOVE_MICRO: i64 = 500_000;
/// The cost threshold in millionths of CNY.
const CONFIRM_ABOVE_MICRO_CNY: i64 = 3_500_000;

/// The tokens one check spends on one model.
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
struct CheckTokens {
    /// Prompt tokens, cached ones included.
    input: u64,
    /// Generated tokens.
    output: u64,
    /// The part of `input` that a provider with prompt caching reads from its cache.
    cached: u64,
}

/// The token table for `test`: Chat 30 in + 20 out, Stream 40 + 450, Tools two requests of
/// about 460 tokens in total (190 in, 270 out), JSON schema 60 + 40, Vision 150 + 20, Reasoning
/// 60 + 900, Cache two requests of 1,500 + 10 out, Embeddings 10 in; Search and PDF are
/// estimates of the same kind. Free checks and the context test are zero.
const fn check_tokens(test: ProbeTest) -> CheckTokens {
    let (input, output, cached) = match test {
        ProbeTest::Chat => (30, 20, 0),
        ProbeTest::Stream => (40, 450, 0),
        ProbeTest::Tools => (190, 270, 0),
        ProbeTest::JsonSchema => (60, 40, 0),
        ProbeTest::Vision => (150, 20, 0),
        ProbeTest::Reasoning => (60, 900, 0),
        ProbeTest::Cache => (3_000, 10, 1_500),
        ProbeTest::Embeddings => (10, 0, 0),
        ProbeTest::Search => (1_200, 300, 0),
        ProbeTest::Pdf => (600, 20, 0),
        ProbeTest::Decision => (50, 20, 0),
        ProbeTest::Connectivity | ProbeTest::Auth | ProbeTest::Models | ProbeTest::Context => {
            (0, 0, 0)
        }
    };
    CheckTokens {
        input,
        output,
        cached,
    }
}

/// An amount of money in one currency.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct CurrencyAmount {
    /// ISO 4217 code of the price used.
    pub currency: String,
    /// Millionths of the currency.
    pub micro: i64,
}

/// The estimate of a run.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProbeEstimate {
    /// Input plus output tokens of every planned check.
    pub tokens: u64,
    /// Models that spend tokens (one per provider and model id, however many keys).
    pub models: u32,
    /// Estimated cost per currency of the price used; models without a price add nothing.
    pub cost: Vec<CurrencyAmount>,
    /// Models that spend tokens and have no price: the line says "price unknown for N models".
    pub unknown_price_models: u32,
    /// Whether a confirmation dialog must come first.
    pub needs_confirmation: bool,
}

impl ProbeEstimate {
    /// The one cost string every surface shows (bench header, Models selection bar, cost dialog,
    /// Add-provider footer): "about 22k tokens, ≈ $0.07 for 10 models". Models without a price
    /// add "price unknown for N models" (the whole line is "about 85k tokens, price unknown for
    /// 38 models" when no model has a price). A run that spends nothing reads "no tokens, free
    /// checks only".
    #[must_use]
    pub fn summary(&self) -> String {
        if self.tokens == 0 {
            return "no tokens, free checks only".to_owned();
        }
        let mut text = format!("about {}", token_text(self.tokens));
        let priced = self.models.saturating_sub(self.unknown_price_models);
        // Writing to a `String` cannot fail.
        if !self.cost.is_empty() {
            let money: Vec<String> = self.cost.iter().map(money_text).collect();
            let _ = write!(text, ", {} for {}", money.join(" · "), models_text(priced));
        }
        if self.unknown_price_models > 0 {
            let _ = write!(
                text,
                ", price unknown for {}",
                models_text(self.unknown_price_models)
            );
        }
        text
    }
}

/// "850 tokens", "3.1k tokens", "22k tokens".
fn token_text(tokens: u64) -> String {
    match tokens {
        0..=999 => format!("{tokens} tokens"),
        1_000..=9_999 => format!("{:.1}k tokens", tokens as f64 / 1_000.0),
        _ => format!("{}k tokens", (tokens + 500) / 1_000),
    }
}

fn models_text(count: u32) -> String {
    format!("{count} model{}", if count == 1 { "" } else { "s" })
}

/// "≈ $0.07", "≈ ¥3.50", "≈ EUR 0.40", "< $0.01" for a sub-cent amount.
fn money_text(amount: &CurrencyAmount) -> String {
    let symbol: Cow<'static, str> = match amount.currency.as_str() {
        "USD" => Cow::Borrowed("$"),
        "CNY" => Cow::Borrowed("¥"),
        other => Cow::Owned(format!("{other} ")),
    };
    let cents = (amount.micro + 5_000) / 10_000;
    if cents == 0 && amount.micro > 0 {
        return format!("< {symbol}0.01");
    }
    format!("≈ {symbol}{}.{:02}", cents / 100, cents % 100)
}

/// Whether the confirmation dialog is due: more than 100,000 tokens; a cost above 0.50 (3.50 for
/// CNY); more than 50 models; or an unpriced model with more than 20,000 tokens. Never for a run
/// that spends no tokens.
fn needs_confirmation(
    tokens: u64,
    models: u32,
    cost: &[CurrencyAmount],
    unknown_price_models: u32,
) -> bool {
    if tokens == 0 {
        return false;
    }
    tokens > CONFIRM_ABOVE_TOKENS
        || models > CONFIRM_ABOVE_MODELS
        || (unknown_price_models > 0 && tokens > CONFIRM_UNPRICED_ABOVE_TOKENS)
        || cost.iter().any(|amount| {
            amount.micro
                > match amount.currency.as_str() {
                    "CNY" => CONFIRM_ABOVE_MICRO_CNY,
                    _ => CONFIRM_ABOVE_MICRO,
                }
        })
}

/// The tokens of the row's runnable cells and how many of them send a model request.
fn row_usage(row: &RowPlan) -> (CostUsage, i64) {
    let mut usage = CostUsage::default();
    let mut requests = 0_i64;
    for cell in row.cells.iter().filter(|cell| cell.skip.is_none()) {
        let spent = check_tokens(cell.test);
        // A free check sends no model request.
        requests += i64::from(spent.input + spent.output > 0);
        usage.input_tokens += spent.input;
        usage.output_tokens += spent.output;
        usage.cached_tokens += spent.cached;
    }
    (usage, requests)
}

/// The currency and cost in millionths of a row priced at `price`.
fn row_cost(usage: &CostUsage, price: &Price, requests: i64) -> (String, i64) {
    let amount = cost(usage, price);
    // A per-call price is paid by every request of the row, whatever its tokens.
    let micro = if price.per_call.is_some() {
        amount.micro.saturating_mul(requests)
    } else {
        amount.micro
    };
    (amount.currency, micro)
}

/// Estimates the planned run. `price_of(provider_id, model_id)` returns the catalogue or
/// override price of a model, `None` when it has none.
#[must_use]
pub fn estimate_run(
    plan: &ProbePlan,
    items: &[PlanItem],
    price_of: &dyn Fn(&str, &str) -> Option<Price>,
) -> ProbeEstimate {
    // A model tested with two keys is priced once per row but counted once as a model.
    let mut models: HashSet<(&str, &str)> = HashSet::new();
    let mut unknown: HashSet<(&str, &str)> = HashSet::new();
    let mut tokens = 0u64;
    let mut cost_by_currency: BTreeMap<String, i64> = BTreeMap::new();
    for row in &plan.rows {
        let Some(item) = items.get(row.item) else {
            continue;
        };
        let (usage, requests) = row_usage(row);
        let spent = usage.input_tokens + usage.output_tokens;
        if spent == 0 {
            continue;
        }
        tokens += spent;
        let id = (item.provider_id.as_str(), item.model_id.as_str());
        models.insert(id);
        match price_of(id.0, id.1) {
            Some(price) => {
                let (currency, micro) = row_cost(&usage, &price, requests);
                *cost_by_currency.entry(currency).or_default() += micro;
            }
            None => {
                unknown.insert(id);
            }
        }
    }
    let models = u32::try_from(models.len()).unwrap_or(u32::MAX);
    let unknown_price_models = u32::try_from(unknown.len()).unwrap_or(u32::MAX);
    let cost: Vec<CurrencyAmount> = cost_by_currency
        .into_iter()
        .map(|(currency, micro)| CurrencyAmount { currency, micro })
        .collect();
    ProbeEstimate {
        tokens,
        models,
        needs_confirmation: needs_confirmation(tokens, models, &cost, unknown_price_models),
        cost,
        unknown_price_models,
    }
}

#[cfg(test)]
mod tests;
