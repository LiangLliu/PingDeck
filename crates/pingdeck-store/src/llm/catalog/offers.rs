//! The catalog offers of a model, as the price precedence reads them.
//!
//! Reads the `model_catalog` rows of one model (or of all models) into [`Offer`]s with their
//! publisher, and stamps a listed row with its default catalog price (the official offer, else the
//! reference offer; `llm::usage::default_price`).

use std::collections::HashMap;

use rusqlite::{Connection, Row};

use super::db_error;
use crate::llm::usage::{Offer, Price, PriceSource, default_price};
use pingdeck_base::error::AppError;
use pingdeck_model::model_catalog::{DefaultBasis, ModelCatalogRow, PriceBasisKind};

const OFFER_COLUMNS: &str = "family, family_name, model_id, model_key, publisher, input_per_mtok, \
     output_per_mtok, cache_read_per_mtok, cache_write_per_mtok, reasoning_per_mtok, currency";

/// The offers of one model and who released it.
#[derive(Debug, Clone, Default, PartialEq)]
pub struct ModelOffers {
    /// The publisher (the key's part before the first `/`, or a user row's family).
    pub publisher: String,
    pub offers: Vec<Offer>,
}

fn read_offer(row: &Row<'_>) -> rusqlite::Result<(String, String, Offer)> {
    let input: Option<f64> = row.get(5)?;
    let output: Option<f64> = row.get(6)?;
    let price = match (input, output) {
        (Some(input_per_mtok), Some(output_per_mtok)) => Some(Price {
            input_per_mtok,
            output_per_mtok,
            cache_read_per_mtok: row.get(7)?,
            cache_write_per_mtok: row.get(8)?,
            reasoning_per_mtok: row.get(9)?,
            currency: row.get(10)?,
            source: PriceSource::Catalog,
            per_call: None,
        }),
        _ => None,
    };
    Ok((
        row.get(3)?,
        row.get(4)?,
        Offer {
            family: row.get(0)?,
            family_name: row.get(1)?,
            model_id: row.get(2)?,
            price,
        },
    ))
}

/// The offers of `model_key` (none for an unlisted model), family order.
///
/// # Errors
/// [`AppError::database`] on any `SQLite` failure.
pub(crate) fn offers_of(conn: &Connection, model_key: &str) -> Result<ModelOffers, AppError> {
    let run = || -> rusqlite::Result<ModelOffers> {
        let mut out = ModelOffers::default();
        let mut statement = conn.prepare_cached(&format!(
            "SELECT {OFFER_COLUMNS} FROM model_catalog WHERE model_key = ?1 ORDER BY family, model_id"
        ))?;
        for row in statement.query_map([model_key], read_offer)? {
            let (_, publisher, offer) = row?;
            out.publisher = publisher;
            out.offers.push(offer);
        }
        Ok(out)
    };
    run().map_err(db_error)
}

/// Every model's offers by key, and the key of every `(family, model id)` row.
pub(crate) type AllOffers = (
    HashMap<String, ModelOffers>,
    HashMap<(String, String), String>,
);

/// The whole catalog as [`AllOffers`].
///
/// # Errors
/// [`AppError::database`] on any `SQLite` failure.
pub(crate) fn all_offers(conn: &Connection) -> Result<AllOffers, AppError> {
    let run = || -> rusqlite::Result<AllOffers> {
        let mut by_key: HashMap<String, ModelOffers> = HashMap::new();
        let mut key_of = HashMap::new();
        let mut statement = conn.prepare_cached(&format!(
            "SELECT {OFFER_COLUMNS} FROM model_catalog ORDER BY family, model_id"
        ))?;
        for row in statement.query_map([], read_offer)? {
            let (key, publisher, offer) = row?;
            key_of.insert((offer.family.clone(), offer.model_id.clone()), key.clone());
            let entry = by_key.entry(key).or_default();
            entry.publisher = publisher;
            entry.offers.push(offer);
        }
        Ok((by_key, key_of))
    };
    run().map_err(db_error)
}

/// Sets the catalog prices of a listed `row` to its model's default (official, else reference)
/// and says which it is.
pub(super) fn stamp_default(row: &mut ModelCatalogRow, offers: &ModelOffers) {
    let Some(priced) = default_price(&offers.publisher, &row.model_id, &offers.offers) else {
        row.price_basis = Some(DefaultBasis::None);
        return;
    };
    let price = priced.price;
    row.input_per_mtok = Some(price.input_per_mtok);
    row.output_per_mtok = Some(price.output_per_mtok);
    row.cache_read_per_mtok = price.cache_read_per_mtok;
    row.cache_write_per_mtok = price.cache_write_per_mtok;
    row.reasoning_per_mtok = price.reasoning_per_mtok;
    row.currency = price.currency;
    row.price_basis = Some(if priced.basis.kind == PriceBasisKind::Official {
        DefaultBasis::Official
    } else {
        DefaultBasis::Reference
    });
    row.basis_family = priced.basis.family;
    row.basis_family_name = priced.basis.family_name;
    row.reference_from = priced.basis.reference_from;
}
