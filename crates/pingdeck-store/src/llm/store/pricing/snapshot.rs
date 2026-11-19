//! Bulk reads for price views assembled outside the store.

use std::collections::HashMap;

use rusqlite::Connection;

use super::{KEYED_ALL, KeyedPricing, list_aliases, list_provider_pricing, read_keyed};
use crate::llm::catalog::{self, CatalogIndex, ModelOffers};
use crate::llm::store::{Provider, list_models, list_providers};
use pingdeck_base::error::AppError;
use pingdeck_model::model_catalog::{ModelCatalogQuery, ModelCatalogRow, UNLISTED_PREFIX};

/// Most offer rows a model detail view lists.
const DETAIL_OFFER_LIMIT: u32 = 500;

/// Everything a price view needs: catalog offers, pricing rows, provider bases, aliases,
/// providers and enabled models. Separate reads in one pass, not a transactional snapshot.
#[derive(Debug)]
pub struct PriceBookSnapshot {
    /// The whole catalog index.
    pub index: CatalogIndex,
    /// Every catalog model's offers: `model_key -> offers`.
    pub offers: HashMap<String, ModelOffers>,
    /// The model of every catalog row: `(family, model_id) -> model_key`.
    pub key_of: HashMap<(String, String), String>,
    /// Every pricing row, ordered by provider then model, those never keyed included.
    pub pricing: Vec<KeyedPricing>,
    /// The catalog family each provider follows: `provider_id -> family`.
    pub bases: HashMap<String, String>,
    /// Every alias: `(provider_id, model_id) -> model_key`.
    pub aliases: HashMap<(String, String), String>,
    /// Every provider.
    pub providers: Vec<Provider>,
    /// Every enabled model: `(provider_id, model_id)`.
    pub models: Vec<(String, String)>,
}

/// Reads everything [`PriceBookSnapshot`] holds in one pass.
///
/// # Errors
/// [`AppError::Database`] on `SQLite` failure.
pub fn load_price_book_snapshot(connection: &Connection) -> Result<PriceBookSnapshot, AppError> {
    let (offers, key_of) = catalog::all_offers(connection)?;
    Ok(PriceBookSnapshot {
        index: CatalogIndex::load(connection)?,
        offers,
        key_of,
        pricing: read_keyed(connection, KEYED_ALL, &[])?,
        bases: list_provider_pricing(connection)?
            .into_iter()
            .map(|basis| (basis.provider_id, basis.catalog_family))
            .collect(),
        aliases: list_aliases(connection)?
            .into_iter()
            .map(|alias| ((alias.provider_id, alias.model_id), alias.model_key))
            .collect(),
        providers: list_providers(connection)?,
        models: list_models(connection, None)?
            .into_iter()
            .filter(|model| model.enabled)
            .map(|model| (model.provider_id, model.model_id))
            .collect(),
    })
}

/// Catalog row and offers for one model detail view.
#[derive(Debug)]
pub struct ModelDetailCatalog {
    pub row: Option<ModelCatalogRow>,
    pub offers: Vec<ModelCatalogRow>,
}

/// Reads the catalog row and offer rows for `model_key` with precomputed `in_use` provider count.
///
/// # Errors
/// [`AppError::Database`] on `SQLite` failure.
pub fn model_detail_catalog(
    connection: &Connection,
    model_key: &str,
    in_use: u32,
) -> Result<ModelDetailCatalog, AppError> {
    let offers = if model_key.starts_with(UNLISTED_PREFIX) {
        Vec::new()
    } else {
        catalog::list_page(
            connection,
            &ModelCatalogQuery {
                model_key: Some(model_key.to_owned()),
                limit: Some(DETAIL_OFFER_LIMIT),
                ..Default::default()
            },
            &HashMap::new(),
        )?
        .rows
    };
    Ok(ModelDetailCatalog {
        row: catalog::model_row(connection, model_key, in_use)?,
        offers,
    })
}
