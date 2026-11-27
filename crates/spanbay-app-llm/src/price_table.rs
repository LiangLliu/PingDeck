//! The bulk price table and the usage-retention preview.
//!
//! Single responsibility: [`llm_price_table_impl`] answers, in one read, the effective price of
//! every provider model (the resolver's precedence through [`PriceBook`]) with where it
//! came from and whether the model was used in the last 30 days, filtered by
//! [`PriceTableQuery`];
//! [`llm_usage_retention_preview_impl`] says how many daily rows a shorter retention deletes.

use std::collections::{BTreeMap, BTreeSet};

use serde::{Deserialize, Serialize};

use super::price_book::PriceBook;
use super::usage::free_providers;
use super::{ensure_catalog, with_connection};
use crate::app::{AppState, current_time_ms};
use crate::error::AppError;
use crate::llm::store;
use crate::llm::usage::{PriceSource, UsageQuery};

const DAY_MS: i64 = 24 * 60 * 60 * 1000;

/// Which rows of the price table to return; every set field must match.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceTableQuery {
    /// Only this provider's models.
    #[serde(default)]
    pub provider_id: Option<String>,
    /// Only models with requests in the last 30 days.
    #[serde(default)]
    pub used_30d: bool,
    /// Only models priced by a pricing row (custom or fixed) rather than the catalog.
    #[serde(default)]
    pub overridden: bool,
    /// Only models with no price at all.
    #[serde(default)]
    pub no_price: bool,
    /// Only models whose id or provider name contains this text (case-insensitive).
    #[serde(default)]
    pub search: Option<String>,
}

/// One model's price.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceRow {
    /// The provider.
    pub provider_id: String,
    /// The model.
    pub model_id: String,
    /// Per million input tokens; `None` with no price and for a fixed (per-call) price, which has
    /// no per-token figure to show (the row says `source: fixed` and the currency).
    pub input_per_mtok: Option<f64>,
    /// Per million output tokens; `None` as `input_per_mtok`.
    pub output_per_mtok: Option<f64>,
    /// Per million cache-read tokens.
    pub cache_read_per_mtok: Option<f64>,
    /// Per million cache-write tokens.
    pub cache_write_per_mtok: Option<f64>,
    /// ISO 4217 code of the price; `None` with no price.
    pub currency: Option<String>,
    /// Where the price came from; `None` with no price.
    pub source: Option<PriceSource>,
    /// The model costs nothing: both prices (or the per-call price) are zero, or the provider is
    /// a local runtime that needs no key.
    pub free: bool,
    /// The model had requests in the last 30 days.
    pub used_30d: bool,
}

/// The price of every provider model, name order, filtered by `query`.
///
/// # Errors
/// [`AppError::Database`] on SQLite failure.
pub fn llm_price_table_impl(
    state: &AppState,
    query: &PriceTableQuery,
) -> Result<Vec<PriceRow>, AppError> {
    let now_ms = current_time_ms();
    let tz_offset_minutes = state.llm.tz_offset_minutes();
    with_connection(state, |connection| {
        ensure_catalog(connection)?;
        let providers = store::list_providers(connection)?;
        let names: BTreeMap<&str, &str> = providers
            .iter()
            .map(|provider| (provider.id.as_str(), provider.name.as_str()))
            .collect();
        let free = free_providers(connection)?;
        let book = PriceBook::load(connection)?;
        let used: BTreeSet<(String, String)> = store::usage_rows(
            connection,
            &UsageQuery {
                provider_id: query.provider_id.clone(),
                key_id: None,
                client_id: None,
                from_ms: now_ms - 30 * DAY_MS,
                to_ms: now_ms + 1,
                group_by: Vec::new(),
                sources: Vec::new(),
                tz_offset_minutes,
                bucket_minutes: None,
            },
        )?
        .into_iter()
        .filter(|row| row.requests > 0)
        .map(|row| (row.provider_id, row.model))
        .collect();
        let needle = query
            .search
            .as_deref()
            .map(|text| text.trim().to_lowercase())
            .unwrap_or_default();
        let mut rows = Vec::new();
        for model in store::list_models(connection, query.provider_id.as_deref())? {
            let key = (model.provider_id.clone(), model.model_id.clone());
            let is_used = used.contains(&key);
            if query.used_30d && !is_used {
                continue;
            }
            if !needle.is_empty()
                && !model.model_id.to_lowercase().contains(&needle)
                && !names
                    .get(model.provider_id.as_str())
                    .is_some_and(|name| name.to_lowercase().contains(&needle))
            {
                continue;
            }
            let price = book
                .priced(&model.provider_id, &model.model_id)
                .map(|priced| priced.price);
            let user_priced = price
                .as_ref()
                .is_some_and(|p| p.source != PriceSource::Catalog);
            if (query.overridden && !user_priced) || (query.no_price && price.is_some()) {
                continue;
            }
            let zero = price.as_ref().is_some_and(|p| {
                p.per_call.map_or(
                    p.input_per_mtok == 0.0 && p.output_per_mtok == 0.0,
                    |per_call| per_call == 0.0,
                )
            });
            // A fixed price has no per-token figure: the row reports its source and currency only.
            let per_token = price.as_ref().filter(|p| p.per_call.is_none());
            rows.push(PriceRow {
                free: zero || free.contains(&model.provider_id),
                input_per_mtok: per_token.map(|p| p.input_per_mtok),
                output_per_mtok: per_token.map(|p| p.output_per_mtok),
                cache_read_per_mtok: price.as_ref().and_then(|p| p.cache_read_per_mtok),
                cache_write_per_mtok: price.as_ref().and_then(|p| p.cache_write_per_mtok),
                currency: price.as_ref().map(|p| p.currency.clone()),
                source: price.map(|p| p.source),
                provider_id: model.provider_id,
                model_id: model.model_id,
                used_30d: is_used,
            });
        }
        Ok(rows)
    })
}

/// What a shorter usage retention would do.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct UsageRetentionPreview {
    /// Daily rollup rows older than `days` that the next prune deletes.
    pub rows_to_delete: u64,
    /// Size of the rollup table now, in bytes (an estimate of the stored payload).
    pub table_bytes: u64,
}

/// How many daily rows keeping only `days` days would delete, and the rollup's size now.
///
/// # Errors
/// [`AppError::Validation`] for `days` of 0 or more than a century, [`AppError::Database`] on
/// SQLite failure.
pub fn llm_usage_retention_preview_impl(
    state: &AppState,
    days: u16,
) -> Result<UsageRetentionPreview, AppError> {
    if days == 0 || days > 36_500 {
        return Err(AppError::validation(
            "usage retention must be between 1 and 36500 days",
        ));
    }
    let now_ms = current_time_ms();
    let (rows_to_delete, table_bytes) = with_connection(state, |connection| {
        store::usage_retention_preview(connection, days, now_ms)
    })?;
    Ok(UsageRetentionPreview {
        rows_to_delete,
        table_bytes,
    })
}

#[cfg(test)]
mod tests {
    use super::*;
    use crate::llm::usage::{PriceOverride, RollupDelta, local_day};
    use crate::uc_support::Fx;

    fn delta(day: &str, provider: &str, model: &str) -> RollupDelta {
        RollupDelta {
            day: day.to_owned(),
            source: "gateway".to_owned(),
            provider_id: provider.to_owned(),
            key_id: "k".to_owned(),
            model: model.to_owned(),
            client_id: String::new(),
            alias: String::new(),
            currency: "USD".to_owned(),
            requests: 1,
            ok_requests: 1,
            input_tokens: 10,
            output_tokens: 5,
            cached_tokens: 0,
            cache_write_tokens: 0,
            reasoning_tokens: 0,
            cost_micro: 1,
            failure_kind: None,
        }
    }

    fn rows(fx: &Fx, query: &PriceTableQuery) -> Vec<PriceRow> {
        llm_price_table_impl(&fx.state, query).unwrap()
    }

    #[test]
    fn the_table_lists_override_catalogue_and_unpriced_models_with_their_filters() {
        let fx = Fx::new();
        let wired = fx.provider("Relay", 1, &["gpt-4o", "zz-unpriced", "free-m"]);
        let provider = wired.provider_id;
        crate::llm_price_override_save_impl(
            &fx.state,
            PriceOverride {
                provider_id: provider.clone(),
                model_id: "free-m".to_owned(),
                input_per_mtok: 0.0,
                output_per_mtok: 0.0,
                cache_read_per_mtok: None,
                cache_write_per_mtok: None,
                reasoning_per_mtok: None,
                currency: "usd".to_owned(),
                updated_at_ms: 0,
            },
        )
        .unwrap();
        let all = rows(&fx, &PriceTableQuery::default());
        let by = |id: &str| all.iter().find(|row| row.model_id == id).unwrap();
        assert_eq!(by("gpt-4o").source, Some(PriceSource::Catalog));
        assert!(by("gpt-4o").input_per_mtok.is_some());
        assert_eq!(by("zz-unpriced").source, None);
        assert_eq!(by("zz-unpriced").currency, None);
        assert!(!by("zz-unpriced").free);
        let free = by("free-m");
        assert_eq!(
            (free.source, free.free),
            (Some(PriceSource::Override), true)
        );
        assert_eq!(free.currency.as_deref(), Some("USD"));

        let overridden = rows(
            &fx,
            &PriceTableQuery {
                overridden: true,
                ..Default::default()
            },
        );
        assert_eq!(overridden.len(), 1);
        let unpriced = rows(
            &fx,
            &PriceTableQuery {
                no_price: true,
                ..Default::default()
            },
        );
        assert_eq!(
            unpriced
                .iter()
                .map(|r| r.model_id.as_str())
                .collect::<Vec<_>>(),
            ["zz-unpriced"]
        );
        let searched = rows(
            &fx,
            &PriceTableQuery {
                search: Some("RELAY".to_owned()),
                ..Default::default()
            },
        );
        assert_eq!(searched.len(), 3, "a provider-name match keeps its models");
        let nothing = rows(
            &fx,
            &PriceTableQuery {
                provider_id: Some("other".to_owned()),
                ..Default::default()
            },
        );
        assert!(nothing.is_empty());
    }

    #[test]
    fn used_30d_follows_the_rollup_of_the_last_thirty_days() {
        let fx = Fx::new();
        let provider = fx
            .provider("Relay", 1, &["gpt-4o", "gpt-4o-mini"])
            .provider_id;
        let today = local_day(current_time_ms(), 0);
        let old = local_day(current_time_ms() - 60 * DAY_MS, 0);
        let connection = fx.state.store();
        store::upsert_rollup(connection.connection(), &delta(&today, &provider, "gpt-4o")).unwrap();
        store::upsert_rollup(
            connection.connection(),
            &delta(&old, &provider, "gpt-4o-mini"),
        )
        .unwrap();
        drop(connection);
        let used = rows(
            &fx,
            &PriceTableQuery {
                used_30d: true,
                ..Default::default()
            },
        );
        assert_eq!(
            used.iter().map(|r| r.model_id.as_str()).collect::<Vec<_>>(),
            ["gpt-4o"]
        );
        assert!(used[0].used_30d);
    }

    #[test]
    fn the_retention_preview_counts_the_rows_a_shorter_retention_deletes() {
        let fx = Fx::new();
        let now = current_time_ms();
        {
            let connection = fx.state.store();
            for age in [0, 5, 40, 200] {
                let day = local_day(now - age * DAY_MS, 0);
                store::upsert_rollup(connection.connection(), &delta(&day, "p", "m")).unwrap();
            }
        }
        let thirty = llm_usage_retention_preview_impl(&fx.state, 30).unwrap();
        assert_eq!(thirty.rows_to_delete, 2);
        assert!(thirty.table_bytes > 0);
        assert_eq!(
            llm_usage_retention_preview_impl(&fx.state, 365)
                .unwrap()
                .rows_to_delete,
            0
        );
        assert!(llm_usage_retention_preview_impl(&fx.state, 0).is_err());
    }
}
