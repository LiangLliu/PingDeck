//! Summary DTOs and the pure aggregation behind the Usage tab and "Today".
//!
//! [`summarize`] folds [`RollupRow`]s (rollup rows plus the newest unrolled log rows, already
//! converted by the use-case) and the exact [`FailureCount`]s into per-day, per-model and
//! per-group figures. Costs stay per currency (one entry per currency in every `cost_micro` map,
//! so a two-currency range yields one series per currency); nothing is converted or summed across
//! currencies. All displayed costs are estimates (the UI prefixes "~").

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::day::local_day;
use super::query::{UsageGroup, UsageQuery, UsageSource};
use super::rollup::{RollupRow, UNPRICED_CURRENCY};
use crate::llm::failure::FailureKind;

/// How many requests of one kind failed, per day, source, provider and model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct FailureCount {
    /// Local day.
    pub day: String,
    /// Source identifier.
    pub source: String,
    /// Provider id (empty when the request failed before routing).
    pub provider_id: String,
    /// Model.
    pub model: String,
    /// A `FailureKind::as_str` value.
    pub kind: String,
    /// Failed requests of that kind.
    pub count: u64,
}

/// Context [`summarize`] needs besides the rows.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct SummaryContext {
    /// "Now", Unix ms: picks the day of the Today figures.
    pub now_ms: i64,
    /// Providers whose models cost nothing (local runtimes); their rows are never "unpriced".
    pub free_providers: BTreeSet<String>,
}

/// The numbers of one bucket (a day, a model, a group, the total).
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRow {
    /// Requests finished.
    pub requests: u64,
    /// Requests that succeeded.
    pub ok_requests: u64,
    /// Requests that failed; cancellations are not failures and are in `canceled`.
    pub failed: u64,
    /// Requests the client or the app cancelled: neither succeeded nor failed, and left out of
    /// the failure rate's denominator. Absent when zero.
    #[serde(default, skip_serializing_if = "is_zero")]
    pub canceled: u64,
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
    /// Estimated cost per currency in millionths; never summed across currencies. Unpriced
    /// and free rows add nothing.
    pub cost_micro: BTreeMap<String, i64>,
    /// Requests of models with no price (excluded from `cost_micro`).
    pub unpriced_requests: u64,
}

impl UsageRow {
    fn add(&mut self, row: &RollupRow, free: bool) {
        self.requests = self.requests.saturating_add(row.requests);
        self.ok_requests = self.ok_requests.saturating_add(row.ok_requests);
        let canceled = row.canceled.min(row.failed());
        self.failed = self.failed.saturating_add(row.failed() - canceled);
        self.canceled = self.canceled.saturating_add(canceled);
        self.input_tokens = self.input_tokens.saturating_add(row.input_tokens);
        self.output_tokens = self.output_tokens.saturating_add(row.output_tokens);
        self.cached_tokens = self.cached_tokens.saturating_add(row.cached_tokens);
        self.cache_write_tokens = self
            .cache_write_tokens
            .saturating_add(row.cache_write_tokens);
        self.reasoning_tokens = self.reasoning_tokens.saturating_add(row.reasoning_tokens);
        if free {
            return;
        }
        if row.currency == UNPRICED_CURRENCY {
            self.unpriced_requests = self.unpriced_requests.saturating_add(row.requests);
        } else {
            let slot = self.cost_micro.entry(row.currency.clone()).or_insert(0);
            *slot = slot.saturating_add(row.cost_micro);
        }
    }

    /// Prompt plus generated tokens.
    #[must_use]
    pub const fn total_tokens(&self) -> u64 {
        self.input_tokens.saturating_add(self.output_tokens)
    }
}

// `&u64` because serde's `skip_serializing_if` passes a reference.
const fn is_zero(value: &u64) -> bool {
    *value == 0
}

/// One slot of the series: a local day, or a bucket in minute-rollup mode.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageDay {
    /// `YYYY-MM-DD`, or the bucket start `YYYY-MM-DDTHH:MM` with `bucket_minutes`.
    pub day: String,
    /// The day's numbers (zeros when nothing happened).
    #[serde(flatten)]
    pub usage: UsageRow,
}

/// One row of the per-model table.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageModelRow {
    /// Provider the model was used through.
    pub provider_id: String,
    /// Upstream model id.
    pub model: String,
    /// The model's numbers.
    #[serde(flatten)]
    pub usage: UsageRow,
    /// The provider is a free local runtime: show "free", not "–".
    pub free: bool,
    /// Some requests had no price: show "–" for them.
    pub price_missing: bool,
    /// Share of all requests in the range, 0..=1.
    pub share_requests: f64,
    /// Share of all prompt plus generated tokens, 0..=1.
    pub share_tokens: f64,
    /// Share of the cost within each currency, 0..=1.
    pub share_cost: BTreeMap<String, f64>,
}

/// One bucket of a `group_by` split.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageBucket {
    /// Values of the grouped dimensions, in the order of `group_by`.
    pub key: Vec<String>,
    /// The bucket's numbers.
    #[serde(flatten)]
    pub usage: UsageRow,
}

/// The most frequent failure reason.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct TopFailure {
    /// A [`FailureKind::as_str`] value.
    pub kind: String,
    /// Failed requests carrying that kind.
    pub count: u64,
    /// Provider with the most such failures.
    pub provider_id: String,
    /// Model with the most such failures on that provider.
    pub model: String,
}

/// The "Today" figures: gateway rows of the local day of `now_ms`.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageToday {
    /// Local day `YYYY-MM-DD`.
    pub day: String,
    /// Today's numbers.
    #[serde(flatten)]
    pub usage: UsageRow,
    /// Most frequent failure reason today. Today's cost is `usage.cost_micro`: one amount per
    /// currency, never converted or added.
    pub top_failure: Option<TopFailure>,
}

/// Everything the Usage tab and the Today strip render.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UsageSummary {
    /// One entry per slot of the range ([`UsageQuery::slots`]), oldest first, zero-filled.
    pub series: Vec<UsageDay>,
    /// Per provider and model, requests descending.
    pub by_model: Vec<UsageModelRow>,
    /// The `group_by` split, ordered by key; empty when `group_by` is empty.
    pub groups: Vec<UsageBucket>,
    /// Totals of the range. `cost_micro` has one entry per currency.
    pub totals: UsageRow,
    /// Currencies with a cost in the range, sorted: the Cost metric shows a selector when
    /// there is more than one and no total.
    pub currencies: Vec<String>,
    /// Models used without a price, sorted; the UI says "N models have no price".
    pub models_without_price: Vec<String>,
    /// Most frequent failure reason of the range.
    pub top_failure: Option<TopFailure>,
    /// The Today figures.
    pub today: UsageToday,
}

/// Failed requests of one kind: the total, and the total per `(provider, model)`.
type KindTally<'a> = (u64, BTreeMap<(&'a str, &'a str), u64>);

/// Most frequent failure kind among `failures` (cancellations excluded), with the provider and
/// model carrying most of it. Counts are exact (`llm_usage_failures`).
fn top_failure<'a>(failures: impl Iterator<Item = &'a FailureCount>) -> Option<TopFailure> {
    let cancelled = FailureKind::Cancelled.as_str();
    let mut by_kind: BTreeMap<&str, KindTally<'_>> = BTreeMap::new();
    for failure in failures.filter(|failure| failure.count > 0 && failure.kind != cancelled) {
        let entry = by_kind.entry(&failure.kind).or_default();
        entry.0 = entry.0.saturating_add(failure.count);
        let place = entry
            .1
            .entry((&failure.provider_id, &failure.model))
            .or_insert(0);
        *place = place.saturating_add(failure.count);
    }
    // Most failures first; ties fall to the alphabetically first kind / provider / model.
    let (kind, (count, places)) = by_kind
        .into_iter()
        .reduce(|best, next| if next.1.0 > best.1.0 { next } else { best })?;
    let ((provider_id, model), _) = places
        .into_iter()
        .reduce(|best, next| if next.1 > best.1 { next } else { best })?;
    Some(TopFailure {
        kind: kind.to_owned(),
        count,
        provider_id: provider_id.to_owned(),
        model: model.to_owned(),
    })
}

fn share(part: u64, whole: u64) -> f64 {
    if whole == 0 {
        0.0
    } else {
        part as f64 / whole as f64
    }
}

/// The per-model table: one row per provider and model with its shares of `totals`, requests
/// descending, then tokens descending, then by provider and model.
fn model_table(
    models: BTreeMap<(&str, &str), UsageRow>,
    totals: &UsageRow,
    free_providers: &BTreeSet<String>,
) -> Vec<UsageModelRow> {
    let mut by_model: Vec<UsageModelRow> = models
        .into_iter()
        .map(|((provider_id, model), usage)| UsageModelRow {
            provider_id: provider_id.to_owned(),
            model: model.to_owned(),
            free: free_providers.contains(provider_id),
            price_missing: usage.unpriced_requests > 0,
            share_requests: share(usage.requests, totals.requests),
            share_tokens: share(usage.total_tokens(), totals.total_tokens()),
            share_cost: usage
                .cost_micro
                .iter()
                .map(|(currency, micro)| {
                    let whole = totals.cost_micro.get(currency).copied().unwrap_or(0);
                    (
                        currency.clone(),
                        share(micro.unsigned_abs(), whole.unsigned_abs()),
                    )
                })
                .collect(),
            usage,
        })
        .collect();
    by_model.sort_by(|a, b| {
        b.usage
            .requests
            .cmp(&a.usage.requests)
            .then_with(|| b.usage.total_tokens().cmp(&a.usage.total_tokens()))
            .then_with(|| (&a.provider_id, &a.model).cmp(&(&b.provider_id, &b.model)))
    });
    by_model
}

/// The Today figures: gateway rows and failures of the local day of `ctx.now_ms`, within the
/// query's scope whatever its range.
fn today_figures(
    rows: &[RollupRow],
    failures: &[FailureCount],
    query: &UsageQuery,
    ctx: &SummaryContext,
) -> UsageToday {
    let day = local_day(ctx.now_ms, query.tz_offset_minutes);
    let gateway = UsageSource::Gateway.as_str();
    let mut usage = UsageRow::default();
    for row in rows.iter().filter(|row| {
        !row.provider_id.is_empty()
            && query.scope_matches(row)
            && row.source == gateway
            && row.day == day
    }) {
        usage.add(row, ctx.free_providers.contains(&row.provider_id));
    }
    let failures = failures.iter().filter(|failure| {
        !failure.provider_id.is_empty()
            && query.provider_in_scope(&failure.provider_id)
            && failure.source == gateway
            && failure.day == day
    });
    UsageToday {
        top_failure: top_failure(failures),
        day,
        usage,
    }
}

/// The figures of a range, added up row by row.
struct RangeFold<'a> {
    /// One entry per slot, zero-filled; a row whose day is not a slot is out of range.
    series: BTreeMap<String, UsageRow>,
    totals: UsageRow,
    models: BTreeMap<(&'a str, &'a str), UsageRow>,
    groups: BTreeMap<Vec<&'a str>, UsageRow>,
    unpriced_models: BTreeSet<&'a str>,
}

impl<'a> RangeFold<'a> {
    fn new(slots: Vec<String>) -> Self {
        Self {
            series: slots
                .into_iter()
                .map(|slot| (slot, UsageRow::default()))
                .collect(),
            totals: UsageRow::default(),
            models: BTreeMap::new(),
            groups: BTreeMap::new(),
            unpriced_models: BTreeSet::new(),
        }
    }

    /// Adds `row` to every figure it belongs to; a row outside the slots is skipped.
    fn add(&mut self, row: &'a RollupRow, group_by: &[UsageGroup], free: bool) {
        let Some(slot) = self.series.get_mut(&row.day) else {
            return;
        };
        slot.add(row, free);
        self.totals.add(row, free);
        self.models
            .entry((&row.provider_id, &row.model))
            .or_default()
            .add(row, free);
        if !group_by.is_empty() {
            let key = group_by.iter().map(|group| group.value(row)).collect();
            self.groups.entry(key).or_default().add(row, free);
        }
        if !free && row.currency == UNPRICED_CURRENCY {
            self.unpriced_models.insert(&row.model);
        }
    }
}

/// Folds `rows` into the summary of `query`; the failure figures come from the exact
/// `failures` (`llm_usage_failures`), not from the rows.
/// Rows and failure counts outside the query's days, sources or scope are ignored, so the
/// caller may pass a superset (the Today figures use today's gateway rows whatever the range
/// is, within the provider/key/client scope; failure counters know provider but not key or
/// client).
#[must_use]
pub fn summarize(
    rows: &[RollupRow],
    failures: &[FailureCount],
    query: &UsageQuery,
    ctx: &SummaryContext,
) -> UsageSummary {
    let sources: Vec<&str> = query
        .effective_sources()
        .into_iter()
        .map(UsageSource::as_str)
        .collect();
    let mut range = RangeFold::new(query.slots());
    for row in rows.iter().filter(|row| {
        !row.provider_id.is_empty()
            && query.scope_matches(row)
            && sources.contains(&row.source.as_str())
    }) {
        let free = ctx.free_providers.contains(&row.provider_id);
        range.add(row, &query.group_by, free);
    }
    let top_range_failure = top_failure(failures.iter().filter(|failure| {
        !failure.provider_id.is_empty()
            && query.provider_in_scope(&failure.provider_id)
            && sources.contains(&failure.source.as_str())
            && range.series.contains_key(&failure.day)
    }));
    let RangeFold {
        series,
        totals,
        models,
        groups,
        unpriced_models,
    } = range;
    UsageSummary {
        series: series
            .into_iter()
            .map(|(day, usage)| UsageDay { day, usage })
            .collect(),
        by_model: model_table(models, &totals, &ctx.free_providers),
        groups: groups
            .into_iter()
            .map(|(key, usage)| UsageBucket {
                key: key.into_iter().map(str::to_owned).collect(),
                usage,
            })
            .collect(),
        top_failure: top_range_failure,
        currencies: totals.cost_micro.keys().cloned().collect(),
        totals,
        models_without_price: unpriced_models.into_iter().map(str::to_owned).collect(),
        today: today_figures(rows, failures, query, ctx),
    }
}
