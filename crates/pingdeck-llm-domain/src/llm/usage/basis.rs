//! Which price applies to one model at one provider, and why.
//!
//! The precedence is a pure function over what the store read (D-259): the provider's row of the
//! model, the provider's catalog basis, the model's every-provider row, the official offer, the
//! reference offer, else no price. The store (`llm::store::pricing`) reads the inputs one model at
//! a time for a request or all at once for a table; both end here.

use super::price::Price;
use pingdeck_model::model_catalog::{
    ModelPricing, PriceBasis, PriceBasisKind, PricingMode, RelayPricing,
};

/// One models.dev row of a model: who offers it, under which id, at what price.
#[derive(Debug, Clone, PartialEq)]
pub struct Offer {
    /// The catalog family (models.dev provider id).
    pub family: String,
    /// Its display name.
    pub family_name: String,
    /// The id that family lists the model under.
    pub model_id: String,
    /// The catalog price; `None` without both an input and an output price.
    pub price: Option<Price>,
}

/// A price and the rule that gave it.
#[derive(Debug, Clone, PartialEq)]
pub struct Priced {
    /// The price.
    pub price: Price,
    /// Why it applies.
    pub basis: PriceBasis,
}

/// The stored rows that can price one model at one provider.
#[derive(Debug, Clone, Copy, Default)]
pub struct PricingRows<'a> {
    /// The provider's row of the model.
    pub provider: Option<&'a ModelPricing>,
    /// The provider-level basis: a catalog family whose offers price the provider.
    pub provider_family: Option<&'a str>,
    /// The model's every-provider row.
    pub global: Option<&'a ModelPricing>,
}

fn price_basis(kind: PriceBasisKind, offer: Option<&Offer>) -> PriceBasis {
    PriceBasis {
        kind,
        family: offer.map(|offer| offer.family.clone()),
        family_name: offer.map(|offer| offer.family_name.clone()),
        reference_from: Vec::new(),
        fallback: false,
        relay: None,
    }
}

/// `family`'s priced offer, the one listed under `model_id` first.
fn priced_offer<'a>(offers: &'a [Offer], family: &str, model_id: &str) -> Option<&'a Offer> {
    let of_family = offers
        .iter()
        .filter(|offer| offer.family == family && offer.price.is_some());
    of_family
        .clone()
        .find(|offer| offer.model_id == model_id)
        .or_else(|| of_family.clone().next())
}

fn from_offer(kind: PriceBasisKind, offer: &Offer) -> Option<Priced> {
    Some(Priced {
        price: offer.price.clone()?,
        basis: price_basis(kind, Some(offer)),
    })
}

fn per_token_total(price: &Price) -> f64 {
    price.input_per_mtok + price.output_per_mtok
}

/// The id without a vendor prefix (`moonshotai/kimi-k2` → `kimi-k2`).
fn bare_id(id: &str) -> &str {
    id.rsplit_once('/').map_or(id, |(_, tail)| tail)
}

/// The offers a reference price is taken from: the priced ones, narrowed to those listed under
/// `model_id` (ignoring a vendor prefix) when there are any, then to those above zero unless none
/// is.
fn reference_candidates<'a>(model_id: &str, offers: &'a [Offer]) -> Vec<&'a Offer> {
    let mut priced: Vec<&Offer> = offers
        .iter()
        .filter(|offer| offer.price.is_some())
        .collect();
    let wanted = bare_id(model_id);
    if priced
        .iter()
        .any(|offer| bare_id(&offer.model_id) == wanted)
    {
        priced.retain(|offer| bare_id(&offer.model_id) == wanted);
    }
    let above_zero: Vec<&Offer> = priced
        .iter()
        .copied()
        .filter(|offer| {
            offer
                .price
                .as_ref()
                .is_some_and(|p| per_token_total(p) > 0.0)
        })
        .collect();
    if above_zero.is_empty() {
        priced
    } else {
        above_zero
    }
}

/// Sorts `candidates` by input + output price (ties by family, then model id) and returns the
/// lower median.
fn lower_median<'a>(candidates: &mut [&'a Offer]) -> Option<&'a Offer> {
    candidates.sort_by(|a, b| {
        let total = |offer: &Offer| offer.price.as_ref().map_or(0.0, per_token_total);
        total(a)
            .total_cmp(&total(b))
            .then_with(|| a.family.cmp(&b.family))
            .then_with(|| a.model_id.cmp(&b.model_id))
    });
    candidates
        .get(candidates.len().checked_sub(1)? / 2)
        .copied()
}

/// The display names of the families behind a reference price, sorted case-insensitively; names
/// that differ only in case are listed once, as the first candidate spells it.
fn reference_names(candidates: &[&Offer]) -> Vec<String> {
    let mut names: Vec<(String, &str)> = candidates
        .iter()
        .map(|offer| (offer.family_name.to_lowercase(), offer.family_name.as_str()))
        .collect();
    // Stable, so the first spelling of each lower-case key stays first.
    names.sort_by(|a, b| a.0.cmp(&b.0));
    names.dedup_by(|later, earlier| later.0 == earlier.0);
    names.into_iter().map(|(_, name)| name.to_owned()).collect()
}

/// The model's default catalog price for `model_id`: the offer of its publisher (the one listed
/// under `model_id` first), else the reference offer — the lower median by input + output price
/// of the priced offers above zero (all-zero offers count only when no other has a price), taken
/// among the offers listed under `model_id` (ignoring a vendor prefix) when there are any (so a
/// `-highspeed` variant keyed with its base model keeps its own price) — else none.
#[must_use]
pub fn default_price(publisher: &str, model_id: &str, offers: &[Offer]) -> Option<Priced> {
    if let Some(official) = priced_offer(offers, publisher, model_id) {
        return from_offer(PriceBasisKind::Official, official);
    }
    let mut candidates = reference_candidates(model_id, offers);
    let chosen = lower_median(&mut candidates)?;
    let mut priced = from_offer(PriceBasisKind::Reference, chosen)?;
    priced.basis.reference_from = reference_names(&candidates);
    Some(priced)
}

/// The row's own price (`custom` or `fixed`), or the offer of its `catalog_family` (`dynamic`);
/// `Err(true)` when that family no longer prices the model, `Err(false)` for a plain `dynamic`
/// row or a corrupt one.
fn row_price(
    row: &ModelPricing,
    kind: PriceBasisKind,
    offers: &[Offer],
    model_id: &str,
) -> Result<Priced, bool> {
    if row.mode != PricingMode::Dynamic {
        return Price::from_pricing(row)
            .map(|price| Priced {
                price,
                basis: price_basis(kind, None),
            })
            .ok_or(false);
    }
    let Some(family) = row.catalog_family.as_deref() else {
        return Err(false);
    };
    priced_offer(offers, family, model_id)
        .and_then(|offer| from_offer(kind, offer))
        .ok_or(true)
}

/// The price of `model_id` (whose model has `publisher` and `offers`) given the stored `rows`:
/// the provider's row; the provider-level basis; the every-provider row; the official offer; the
/// reference offer; else `None`. A `dynamic` provider row that gives no price (plain, or following
/// a family that no longer prices the model) skips the provider-level basis and the every-provider
/// row; a corrupt row is skipped. A basis family that does not price the model marks the price
/// that applies instead as a `fallback`.
#[must_use]
pub fn decide(
    model_id: &str,
    publisher: &str,
    offers: &[Offer],
    rows: PricingRows<'_>,
) -> Option<Priced> {
    let mut fallback = false;
    let mut follow_default_only = false;
    if let Some(row) = rows.provider {
        match row_price(row, PriceBasisKind::ProviderModel, offers, model_id) {
            Ok(priced) => return Some(priced),
            Err(missing) => {
                fallback |= missing;
                follow_default_only = row.mode == PricingMode::Dynamic;
            }
        }
    }
    if !follow_default_only {
        if let Some(family) = rows.provider_family {
            match priced_offer(offers, family, model_id) {
                Some(offer) => return from_offer(PriceBasisKind::Provider, offer),
                None => fallback = true,
            }
        }
        if let Some(row) = rows.global {
            match row_price(row, PriceBasisKind::Model, offers, model_id) {
                Ok(mut priced) => {
                    priced.basis.fallback = fallback;
                    return Some(priced);
                }
                Err(missing) => fallback |= missing,
            }
        }
    }
    default_price(publisher, model_id, offers).map(|mut priced| {
        priced.basis.fallback = fallback;
        priced
    })
}

/// `priced` as the relay bills it: every price that is not the provider's own row of the model
/// (a provider basis family, the every-provider row, the official or the reference offer) is the
/// official price, so the relay's multiplier applies and its billing currency replaces the
/// price's. The provider's own row is the relay's price already and stays as it is.
#[must_use]
pub fn apply_relay(mut priced: Priced, relay: Option<&RelayPricing>) -> Priced {
    let Some(relay) = relay else {
        return priced;
    };
    if priced.basis.kind == PriceBasisKind::ProviderModel {
        return priced;
    }
    let factor = relay.multiplier;
    let price = &mut priced.price;
    price.input_per_mtok *= factor;
    price.output_per_mtok *= factor;
    for slot in [
        &mut price.cache_read_per_mtok,
        &mut price.cache_write_per_mtok,
        &mut price.reasoning_per_mtok,
        &mut price.per_call,
    ] {
        if let Some(value) = slot.as_mut() {
            *value *= factor;
        }
    }
    if let Some(currency) = &relay.currency {
        price.currency.clone_from(currency);
    }
    priced.basis.relay = Some(Box::new(relay.clone()));
    priced
}

/// The catalog family a provider's preset declares for its host: the matching region's
/// `catalog_family`, else the preset's. `None` without a preset, or when the base URL left the
/// preset's hosts (the user then priced it themselves or it is someone else's relay).
#[must_use]
pub fn preset_family(preset_id: Option<&str>, base_url: &str) -> Option<String> {
    let preset_id = preset_id?;
    let url = url::Url::parse(base_url).ok()?;
    let host = url.host_str()?;
    let known = crate::llm::presets::preset_region_for_host(preset_id, None, host, url.port())?;
    let preset = crate::llm::presets::presets()
        .ok()?
        .provider(&known.preset_id)?;
    preset
        .regions
        .iter()
        .find(|region| region.id == known.region_id)
        .and_then(|region| region.catalog_family.clone())
        .or_else(|| preset.catalog_family.clone())
}

/// The family whose offer is the "official" price: the preset's when it prices the model (a
/// `-cn` distributor finds its own price list), else the inferred publisher.
#[must_use]
pub fn official_family<'a>(
    publisher: &'a str,
    preset: Option<&'a str>,
    offers: &[Offer],
) -> &'a str {
    preset
        .filter(|family| {
            offers
                .iter()
                .any(|offer| offer.family == *family && offer.price.is_some())
        })
        .unwrap_or(publisher)
}
