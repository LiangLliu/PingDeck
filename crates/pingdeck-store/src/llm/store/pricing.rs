//! The user's pricing and the effective price of a model.
//!
//! CRUD over `model_pricing` rows (`provider_id` empty = every provider; `model_key` is the model
//! the row's id resolves to) and `provider_pricing` (a provider following a models.dev provider's
//! catalog prices); the legacy per-provider override view of the `custom` rows
//! (`llm_price_override_*`, `AiConfig.price_overrides`); and [`effective_price`]/[`resolve_price`].
//! `model_alias` rows are in `aliases`; the bulk reads for price views assembled outside the store
//! ([`load_price_book_snapshot`], [`model_detail_catalog`]) are in `snapshot`.

use std::collections::BTreeMap;

use rusqlite::{Connection, OptionalExtension, params};

use super::{db, get_provider};
use crate::llm::catalog::{self, PRICING_COLUMNS, pricing_from_row};
use crate::llm::usage::{
    Price, PriceOverride, PriceResolution, Priced, PricingRows, UnknownReason, apply_relay, decide,
    is_free_provider, official_family, preset_family,
};
use pingdeck_base::error::AppError;
use pingdeck_model::model_catalog::{ModelPricing, PricingMode, ProviderPricing};

mod aliases;
mod snapshot;

pub(crate) use aliases::list_aliases;
pub use aliases::{delete_alias, save_alias};
pub use snapshot::{
    ModelDetailCatalog, PriceBookSnapshot, load_price_book_snapshot, model_detail_catalog,
};

/// `read_keyed` filters. Only these shapes are spliced into its statement, so the statement
/// cache holds one entry per shape.
const KEYED_ALL: &str = "";
const KEYED_BY_MODEL: &str = "WHERE model_id = ?1 OR model_key = ?1";
const KEYED_FOR_PRICE: &str =
    "WHERE (model_key = ?1 OR (model_key = '' AND model_id = ?2)) AND provider_id IN (?3, '')";

/// The price of `model_id` at `provider_id` in three states: the effective price, free for a
/// local runtime without a key, or unknown (with whether the catalogue knows the model).
///
/// # Errors
/// [`AppError::Database`] on `SQLite` failure.
pub fn resolve_price(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
) -> Result<PriceResolution, AppError> {
    if let Some(price) = effective_price(connection, provider_id, model_id)? {
        return Ok(PriceResolution::Known(price));
    }
    let free =
        get_provider(connection, provider_id)?.is_some_and(|provider| is_free_provider(&provider));
    Ok(if free {
        PriceResolution::Free
    } else if catalog::lookup(connection, model_id)?.is_some() {
        PriceResolution::Unknown(UnknownReason::CatalogWithoutPrice)
    } else {
        PriceResolution::Unknown(UnknownReason::NotInCatalog)
    })
}

/// A stored pricing row with the model it prices.
#[derive(Debug, Clone, PartialEq)]
pub struct KeyedPricing {
    pub pricing: ModelPricing,
    /// Empty when the row was never keyed; callers key it as a request would.
    pub model_key: String,
}

/// The `model_pricing` rows matching `filter` (one of the `KEYED_*` consts) with `args` bound,
/// ordered by provider then model.
fn read_keyed(
    connection: &Connection,
    filter: &str,
    args: &[&dyn rusqlite::ToSql],
) -> Result<Vec<KeyedPricing>, AppError> {
    let mut statement = connection
        .prepare_cached(&format!(
            "SELECT {PRICING_COLUMNS}, model_key FROM model_pricing {filter} \
             ORDER BY provider_id, model_id"
        ))
        .map_err(db)?;
    let width = PRICING_COLUMNS.split(", ").count();
    let rows = statement
        .query_map(args, |row| {
            Ok(pricing_from_row(row, 0)?.map(|pricing| KeyedPricing {
                pricing,
                model_key: row.get::<_, String>(width).unwrap_or_default(),
            }))
        })
        .map_err(db)?;
    // A corrupt row maps to None and is skipped (a_corrupt_row_is_skipped_and_the_walk_continues).
    let mut all = Vec::new();
    for row in rows {
        all.extend(row.map_err(db)?);
    }
    Ok(all)
}

/// The rows that price `model_id` at `provider_id`: the provider's row
/// (the one saved under this very id first, else the newest) and the every-provider row (the
/// newest).
pub fn pick_rows<'a>(
    rows: impl Iterator<Item = &'a KeyedPricing> + Clone,
    provider_id: &str,
    model_id: &str,
) -> (Option<&'a ModelPricing>, Option<&'a ModelPricing>) {
    let best = |scope: &str| {
        rows.clone()
            .map(|row| &row.pricing)
            .filter(|row| row.provider_id == scope)
            .max_by_key(|row| (row.model_id == model_id, row.updated_at_ms))
    };
    let provider = if provider_id.is_empty() {
        None
    } else {
        best(provider_id)
    };
    (provider, best(""))
}

/// Every pricing row, or those saved under or resolving to one model id or key, ordered by
/// provider then model.
///
/// # Errors
/// [`AppError::Database`] on `SQLite` failure.
pub fn list_pricing(
    connection: &Connection,
    model_id: Option<&str>,
) -> Result<Vec<ModelPricing>, AppError> {
    let rows = match model_id {
        Some(model_id) => read_keyed(connection, KEYED_BY_MODEL, &[&model_id])?,
        None => read_keyed(connection, KEYED_ALL, &[])?,
    };
    Ok(rows.into_iter().map(|row| row.pricing).collect())
}

/// The model `model_id` of `provider_id` prices (see `catalog::resolve`).
fn model_key(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
) -> Result<String, AppError> {
    Ok(catalog::resolve(connection, Some(provider_id), model_id)?.model_key)
}

/// Saves the pricing of one model after [`ModelPricing::validated`] and stamps it with `now_ms`.
/// A global (`provider_id` empty) `dynamic` pricing without a catalog family deletes the model's
/// global rows instead — that is what a model without a row already is — and a provider-scoped
/// `dynamic` row is kept: it tells the resolver to ignore the provider basis and the global row
/// for that provider. A global save replaces any other global row of the same model (one saved
/// under another of its ids). Returns the pricing as it now holds.
///
/// # Errors
/// [`AppError::Validation`] for a bad price, currency or id, [`AppError::Database`] on `SQLite`
/// failure.
pub fn save_pricing(
    connection: &Connection,
    pricing: ModelPricing,
    now_ms: i64,
) -> Result<ModelPricing, AppError> {
    let mut pricing = pricing.validated()?;
    pricing.updated_at_ms = now_ms;
    let key = model_key(connection, &pricing.provider_id, &pricing.model_id)?;
    let global = pricing.provider_id.is_empty();
    if global && pricing.mode == PricingMode::Dynamic && pricing.catalog_family.is_none() {
        delete_pricing(connection, "", &pricing.model_id)?;
        return Ok(pricing);
    }
    if global {
        connection
            .execute(
                "DELETE FROM model_pricing WHERE provider_id = '' AND model_key = ?1 \
                 AND model_id <> ?2",
                params![key, pricing.model_id],
            )
            .map_err(db)?;
    }
    connection
        .execute(
            "INSERT INTO model_pricing (provider_id, model_id, mode, per_call, input_per_mtok, \
                 output_per_mtok, cache_read_per_mtok, cache_write_per_mtok, reasoning_per_mtok, \
                 currency, updated_at_ms, catalog_family, model_key) \
             VALUES (?1, ?2, ?3, ?4, ?5, ?6, ?7, ?8, ?9, ?10, ?11, ?12, ?13) \
             ON CONFLICT (provider_id, model_id) DO UPDATE SET mode = excluded.mode, \
                 per_call = excluded.per_call, input_per_mtok = excluded.input_per_mtok, \
                 output_per_mtok = excluded.output_per_mtok, \
                 cache_read_per_mtok = excluded.cache_read_per_mtok, \
                 cache_write_per_mtok = excluded.cache_write_per_mtok, \
                 reasoning_per_mtok = excluded.reasoning_per_mtok, \
                 currency = excluded.currency, updated_at_ms = excluded.updated_at_ms, \
                 catalog_family = excluded.catalog_family, model_key = excluded.model_key",
            params![
                pricing.provider_id,
                pricing.model_id,
                pricing.mode.as_str(),
                pricing.per_call,
                pricing.input_per_mtok,
                pricing.output_per_mtok,
                pricing.cache_read_per_mtok,
                pricing.cache_write_per_mtok,
                pricing.reasoning_per_mtok,
                pricing.currency,
                pricing.updated_at_ms,
                pricing.catalog_family,
                key,
            ],
        )
        .map_err(db)?;
    Ok(pricing)
}

/// Removes one pricing row ("back to inheriting"); `true` when one existed. For every provider
/// (`provider_id` empty) it removes every global row of the model `model_id` stands for.
///
/// # Errors
/// [`AppError::Database`] on `SQLite` failure.
pub fn delete_pricing(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
) -> Result<bool, AppError> {
    let deleted = if provider_id.is_empty() {
        let key = model_key(connection, "", model_id)?;
        connection.execute(
            "DELETE FROM model_pricing WHERE provider_id = '' AND (model_id = ?1 OR model_key = ?2)",
            params![model_id, key],
        )
    } else {
        connection.execute(
            "DELETE FROM model_pricing WHERE provider_id = ?1 AND model_id = ?2",
            params![provider_id, model_id],
        )
    }
    .map_err(db)?;
    Ok(deleted > 0)
}

/// Every provider-scoped `custom` row as a legacy override, keyed by `(provider_id, model_id)`.
///
/// # Errors
/// [`AppError::Database`] on `SQLite` failure.
pub fn price_overrides(
    connection: &Connection,
) -> Result<BTreeMap<(String, String), PriceOverride>, AppError> {
    Ok(list_pricing(connection, None)?
        .iter()
        .filter_map(PriceOverride::from_pricing)
        .map(|price| ((price.provider_id.clone(), price.model_id.clone()), price))
        .collect())
}

/// Saves (inserts or replaces) the `custom` row of one model of one provider after
/// [`PriceOverride::validated`], stamped `now_ms`. Returns the stored value.
///
/// # Errors
/// [`AppError::Validation`] for a bad price, currency or id, [`AppError::Database`] on `SQLite`
/// failure.
pub fn save_price_override(
    connection: &Connection,
    price: PriceOverride,
    now_ms: i64,
) -> Result<PriceOverride, AppError> {
    let mut price = price.validated()?;
    price.updated_at_ms = now_ms;
    save_pricing(connection, price.to_pricing(), now_ms)?;
    Ok(price)
}

/// Removes the provider's `custom` row of one model; `true` when one existed. A `fixed` or
/// `dynamic` row of the same provider and model is left alone (it is not an override).
///
/// # Errors
/// [`AppError::Database`] on `SQLite` failure.
pub fn delete_price_override(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
) -> Result<bool, AppError> {
    let deleted = connection
        .execute(
            "DELETE FROM model_pricing \
             WHERE provider_id = ?1 AND model_id = ?2 AND mode = 'custom' AND provider_id <> ''",
            params![provider_id, model_id],
        )
        .map_err(db)?;
    Ok(deleted > 0)
}

/// Every provider that follows a catalog family's prices.
///
/// # Errors
/// [`AppError::Database`] on `SQLite` failure.
pub(crate) fn list_provider_pricing(
    connection: &Connection,
) -> Result<Vec<ProviderPricing>, AppError> {
    let mut statement = connection
        .prepare_cached(
            "SELECT provider_id, catalog_family, updated_at_ms FROM provider_pricing \
             ORDER BY provider_id",
        )
        .map_err(db)?;
    let rows = statement
        .query_map([], |row| {
            Ok(ProviderPricing {
                provider_id: row.get(0)?,
                catalog_family: row.get(1)?,
                updated_at_ms: row.get(2)?,
            })
        })
        .map_err(db)?;
    rows.collect::<rusqlite::Result<_>>().map_err(db)
}

fn provider_family(connection: &Connection, provider_id: &str) -> Result<Option<String>, AppError> {
    connection
        .prepare_cached("SELECT catalog_family FROM provider_pricing WHERE provider_id = ?1")
        .map_err(db)?
        .query_row([provider_id], |row| row.get(0))
        .optional()
        .map_err(db)
}

/// Makes `provider_id` follow `catalog_family`'s catalog prices, or the official ones again
/// (`None`, the default: the row is removed).
///
/// # Errors
/// [`AppError::Validation`] for an empty id or a family the catalog does not have,
/// [`AppError::Database`] on `SQLite` failure.
pub fn save_provider_pricing(
    connection: &Connection,
    provider_id: &str,
    catalog_family: Option<&str>,
    now_ms: i64,
) -> Result<(), AppError> {
    let provider_id = provider_id.trim();
    if provider_id.is_empty() {
        return Err(AppError::validation("A provider basis needs a provider"));
    }
    let Some(family) = catalog_family.map(str::trim).filter(|f| !f.is_empty()) else {
        connection
            .execute(
                "DELETE FROM provider_pricing WHERE provider_id = ?1",
                [provider_id],
            )
            .map_err(db)?;
        return Ok(());
    };
    let known: bool = connection
        .query_row(
            "SELECT EXISTS (SELECT 1 FROM model_catalog WHERE family = ?1 AND source = 'models.dev')",
            [family],
            |row| row.get(0),
        )
        .map_err(db)?;
    if !known {
        return Err(AppError::validation(format!(
            "The model catalog has no provider {family}"
        )));
    }
    connection
        .execute(
            "INSERT INTO provider_pricing (provider_id, catalog_family, updated_at_ms) \
             VALUES (?1, ?2, ?3) \
             ON CONFLICT (provider_id) DO UPDATE SET catalog_family = excluded.catalog_family, \
                 updated_at_ms = excluded.updated_at_ms",
            params![provider_id, family, now_ms],
        )
        .map_err(db)?;
    Ok(())
}

/// The price that applies to `model_id` of `provider_id` and why (precedence in
/// `llm::usage::decide`): the provider's row of the model, the provider's catalog basis, the
/// model's every-provider row, the official offer, the reference offer, else `None` (no cost is
/// invented). Rows without a model key still match by their saved id. Every price but the
/// provider's own row is then multiplied by the relay's multiplier (`llm::usage::apply_relay`).
///
/// # Errors
/// [`AppError::Database`] on `SQLite` failure.
pub fn effective_priced(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
) -> Result<Option<Priced>, AppError> {
    let key = model_key(connection, provider_id, model_id)?;
    let offers = catalog::offers_of(connection, &key)?;
    let rows = read_keyed(
        connection,
        KEYED_FOR_PRICE,
        &[&key, &model_id, &provider_id],
    )?;
    let (provider, global) = pick_rows(rows.iter(), provider_id, model_id);
    let (family, preset, relay) = if provider_id.is_empty() {
        (None, None, None)
    } else {
        let stored = get_provider(connection, provider_id)?;
        let preset = stored
            .as_ref()
            .and_then(|p| preset_family(p.preset_id.as_deref(), &p.base_url));
        let relay = stored.and_then(|p| p.quirks.relay_pricing);
        (provider_family(connection, provider_id)?, preset, relay)
    };
    Ok(decide(
        model_id,
        official_family(&offers.publisher, preset.as_deref(), &offers.offers),
        &offers.offers,
        PricingRows {
            provider,
            provider_family: family.as_deref(),
            global,
        },
    )
    .map(|priced| apply_relay(priced, relay.as_ref())))
}

/// [`effective_priced`] without the reason.
///
/// # Errors
/// [`AppError::Database`] on `SQLite` failure.
pub fn effective_price(
    connection: &Connection,
    provider_id: &str,
    model_id: &str,
) -> Result<Option<Price>, AppError> {
    Ok(effective_priced(connection, provider_id, model_id)?.map(|priced| priced.price))
}

#[cfg(test)]
mod tests;
