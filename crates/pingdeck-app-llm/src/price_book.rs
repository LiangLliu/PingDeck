//! Prices of many models at once, for the tables and the "Models & pricing" page.
//!
//! Loads the catalog offers, pricing rows, provider bases, aliases and enabled models in one pass
//! of separate reads (`store::load_price_book_snapshot`; not a transactional snapshot), then
//! resolves any `(provider, model id)` using `pingdeck_model::model_catalog::resolve_with` +
//! `llm::usage::decide` and assembles the page's views.

use std::collections::{BTreeMap, BTreeSet, HashMap};
use std::convert::Infallible;

use serde::{Deserialize, Serialize};

use crate::error::AppError;
use crate::llm::catalog::{CatalogIndex, ModelOffers};
use crate::llm::store::{
    KeyedPricing, PriceBookSnapshot, load_price_book_snapshot, model_detail_catalog, pick_rows,
};
use crate::llm::usage::{
    Price, Priced, PricingRows, apply_relay, decide, is_free_provider, official_family,
    preset_family,
};
use pingdeck_model::model_catalog::{
    ModelCatalogRow, ModelPricing, ModelResolution, PriceBasis, UNLISTED_PREFIX, resolve_with,
};
use pingdeck_store::DbConnection as Connection;

/// A price and why it applies.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct EffectivePriceView {
    pub price: Price,
    /// The rule that gave it.
    pub basis: PriceBasis,
}

impl From<Priced> for EffectivePriceView {
    fn from(priced: Priced) -> Self {
        Self {
            price: priced.price,
            basis: priced.basis,
        }
    }
}

/// What a model costs at one provider, in the three states the model table shows: the effective
/// price and why, free (a local runtime without a key), or unknown. Unknown is never free.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(tag = "state", rename_all = "snake_case")]
pub enum EffectivePriceState {
    /// A price applies; a catalogue price is an estimate (`price.source = catalog`).
    Known(EffectivePriceView),
    /// A no-key provider on a local origin.
    Free,
    /// No price: the user can set one.
    Unknown,
}

/// One model at one provider of the user's.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ProviderModelPrice {
    pub provider_id: String,
    pub provider_name: String,
    /// The id that provider serves the model under.
    pub model_id: String,
    /// `false` for a stored row whose provider no longer serves this id.
    pub listed: bool,
    /// The provider's row of the model.
    pub pricing: Option<ModelPricing>,
    pub effective: Option<EffectivePriceView>,
    /// The provider's catalog basis, if it has one.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub provider_family_name: Option<String>,
}

/// Everything the model editor shows about one model's prices.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPriceDetail {
    pub model_key: String,
    /// The listed row (`None` for an unlisted model).
    pub row: Option<ModelCatalogRow>,
    /// The price without a provider: every-provider row, official offer, reference offer.
    pub default: Option<EffectivePriceView>,
    /// The every-provider row.
    pub global: Option<ModelPricing>,
    /// Every catalog row of the model, the publisher's own first, then by provider name.
    pub offers: Vec<ModelCatalogRow>,
    /// The user's providers serving it, and stored rows of providers that no longer do.
    pub providers: Vec<ProviderModelPrice>,
}

/// A model of the user's providers that no catalog row matches.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UnlistedModel {
    pub provider_id: String,
    pub provider_name: String,
    pub model_id: String,
    /// `id:<model id>`.
    pub model_key: String,
    pub effective: Option<EffectivePriceView>,
}

/// One provider of the user's and its catalog basis.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderPricingView {
    pub provider_id: String,
    pub provider_name: String,
    /// The models.dev provider whose prices it follows (none: official prices).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_family_name: Option<String>,
    /// A models.dev provider that looks like this one (name, preset or host) and lists at least
    /// one of its models.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_family: Option<String>,
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub suggested_family_name: Option<String>,
    /// The provider's enabled models.
    pub models: u32,
    /// Of them, the ones `catalog_family` prices (0 without a basis).
    pub matched: u32,
    /// The provider's pricing rows.
    pub overrides: u32,
}

struct ProviderInfo {
    id: String,
    name: String,
    preset_id: Option<String>,
    base_url: String,
    /// A local runtime that needs no key: its models are free, never "price unknown".
    free: bool,
    /// The catalog family its preset declares for its host.
    preset_family: Option<String>,
    /// How the relay prices against the official prices.
    relay: Option<pingdeck_model::model_catalog::RelayPricing>,
}

pub struct PriceBook {
    index: CatalogIndex,
    offers: HashMap<String, ModelOffers>,
    key_of: HashMap<(String, String), String>,
    family_names: BTreeMap<String, String>,
    pricing: Vec<KeyedPricing>,
    bases: HashMap<String, String>,
    aliases: HashMap<(String, String), String>,
    providers: Vec<ProviderInfo>,
    /// The enabled models of the user's providers, `(provider, model id)`.
    models: Vec<(String, String)>,
}

fn count(n: usize) -> u32 {
    u32::try_from(n).unwrap_or(u32::MAX)
}

/// Lower-case ASCII letters and digits of `text`.
fn squash(text: &str) -> String {
    text.chars()
        .filter(char::is_ascii_alphanumeric)
        .map(|c| c.to_ascii_lowercase())
        .collect()
}

impl PriceBook {
    /// Reads everything once.
    ///
    /// # Errors
    /// [`AppError::Database`] on `SQLite` failure.
    pub fn load(conn: &Connection) -> Result<Self, AppError> {
        Ok(Self::from_snapshot(load_price_book_snapshot(conn)?))
    }

    #[must_use]
    pub fn from_snapshot(snapshot: PriceBookSnapshot) -> Self {
        let family_names = snapshot
            .offers
            .values()
            .flat_map(|model| &model.offers)
            .map(|offer| (offer.family.clone(), offer.family_name.clone()))
            .collect();
        let providers = snapshot
            .providers
            .into_iter()
            .map(|provider| ProviderInfo {
                free: is_free_provider(&provider),
                preset_family: preset_family(provider.preset_id.as_deref(), &provider.base_url),
                relay: provider.quirks.relay_pricing.clone(),
                id: provider.id,
                name: provider.name,
                preset_id: provider.preset_id,
                base_url: provider.base_url,
            })
            .collect();
        let mut book = Self {
            index: snapshot.index,
            offers: snapshot.offers,
            key_of: snapshot.key_of,
            family_names,
            pricing: snapshot.pricing,
            bases: snapshot.bases,
            aliases: snapshot.aliases,
            providers,
            models: snapshot.models,
        };
        // Rows without a model key: key them as a request would.
        let unkeyed: Vec<(usize, String)> = book
            .pricing
            .iter()
            .enumerate()
            .filter(|(_, row)| row.model_key.is_empty())
            .map(|(i, row)| {
                let key = book
                    .resolve(&row.pricing.provider_id, &row.pricing.model_id)
                    .model_key;
                (i, key)
            })
            .collect();
        for (i, key) in unkeyed {
            if let Some(row) = book.pricing.get_mut(i) {
                row.model_key = key;
            }
        }
        book
    }

    /// Reuses the preloaded catalog index so callers need not reload `model_catalog`.
    #[must_use]
    pub fn catalog_index(&self) -> &CatalogIndex {
        &self.index
    }

    fn provider_name(&self, provider_id: &str) -> String {
        self.providers
            .iter()
            .find(|provider| provider.id == provider_id)
            .map_or_else(|| provider_id.to_owned(), |provider| provider.name.clone())
    }

    fn family_name(&self, family: &str) -> Option<String> {
        self.family_names.get(family).cloned()
    }

    /// The same answer as `catalog::resolve` on the loaded state.
    #[must_use]
    pub fn resolve(&self, provider_id: &str, model_id: &str) -> ModelResolution {
        let Ok(resolved) = resolve_with::<Infallible>(
            Some(provider_id),
            model_id,
            |provider, model| {
                Ok(self
                    .aliases
                    .get(&(provider.to_owned(), model.to_owned()))
                    .cloned())
            },
            |key| Ok(self.offers.contains_key(key)),
            |model| {
                Ok(self
                    .index
                    .lookup_record(model)
                    .and_then(|record| {
                        self.key_of
                            .get(&(record.family.clone(), record.model_id.clone()))
                    })
                    .cloned())
            },
        );
        resolved
    }

    fn priced_key(&self, provider_id: &str, model_id: &str, key: &str) -> Option<Priced> {
        let empty = ModelOffers::default();
        let offers = self.offers.get(key).unwrap_or(&empty);
        let rows = self.pricing.iter().filter(|row| row.model_key == key);
        let (provider, global) = pick_rows(rows, provider_id, model_id);
        let info = self.providers.iter().find(|info| info.id == provider_id);
        let preset = info.and_then(|info| info.preset_family.as_deref());
        let relay = info.and_then(|info| info.relay.as_ref());
        decide(
            model_id,
            official_family(&offers.publisher, preset, &offers.offers),
            &offers.offers,
            PricingRows {
                provider,
                provider_family: if provider_id.is_empty() {
                    None
                } else {
                    self.bases.get(provider_id).map(String::as_str)
                },
                global,
            },
        )
        .map(|priced| apply_relay(priced, relay))
    }

    /// The price of `model_id` at `provider_id` and why, as `store::effective_priced` says.
    #[must_use]
    pub fn priced(&self, provider_id: &str, model_id: &str) -> Option<Priced> {
        let key = self.resolve(provider_id, model_id).model_key;
        self.priced_key(provider_id, model_id, &key)
    }

    /// The three-state price of `model_id` at `provider_id` (the order of `resolve_price`: a
    /// price wins over free).
    #[must_use]
    pub fn price_state(&self, provider_id: &str, model_id: &str) -> EffectivePriceState {
        if let Some(priced) = self.priced(provider_id, model_id) {
            return EffectivePriceState::Known(priced.into());
        }
        if self
            .providers
            .iter()
            .any(|provider| provider.id == provider_id && provider.free)
        {
            EffectivePriceState::Free
        } else {
            EffectivePriceState::Unknown
        }
    }

    /// How many of the user's providers serve each model.
    #[must_use]
    pub fn in_use(&self) -> HashMap<String, u32> {
        let mut providers: HashMap<String, Vec<&str>> = HashMap::new();
        for (provider_id, model_id) in &self.models {
            let key = self.resolve(provider_id, model_id).model_key;
            let seen = providers.entry(key).or_default();
            if !seen.contains(&provider_id.as_str()) {
                seen.push(provider_id);
            }
        }
        providers
            .into_iter()
            .map(|(key, seen)| (key, count(seen.len())))
            .collect()
    }

    /// One model's prices at every provider of the user's that serves it, its offers and rows.
    ///
    /// # Errors
    /// [`AppError::Database`] on `SQLite` failure.
    pub fn detail(&self, conn: &Connection, model_key: &str) -> Result<ModelPriceDetail, AppError> {
        let mut providers: Vec<ProviderModelPrice> = Vec::new();
        let mut push = |provider_id: &str, model_id: &str, listed: bool| {
            if providers
                .iter()
                .any(|p| p.provider_id == provider_id && p.model_id == model_id)
            {
                return;
            }
            let rows = self.pricing.iter().filter(|row| row.model_key == model_key);
            let family = self.bases.get(provider_id).cloned();
            providers.push(ProviderModelPrice {
                provider_id: provider_id.to_owned(),
                provider_name: self.provider_name(provider_id),
                model_id: model_id.to_owned(),
                listed,
                pricing: rows
                    .filter(|row| {
                        row.pricing.provider_id == provider_id && row.pricing.model_id == model_id
                    })
                    .map(|row| row.pricing.clone())
                    .next(),
                effective: self
                    .priced_key(provider_id, model_id, model_key)
                    .map(EffectivePriceView::from),
                provider_family_name: family.as_deref().and_then(|f| self.family_name(f)),
                provider_family: family,
            });
        };
        for (provider_id, model_id) in &self.models {
            if self.resolve(provider_id, model_id).model_key == model_key {
                push(provider_id, model_id, true);
            }
        }
        let known: Vec<&str> = self.providers.iter().map(|p| p.id.as_str()).collect();
        for row in &self.pricing {
            let p = &row.pricing;
            if row.model_key == model_key
                && !p.provider_id.is_empty()
                && known.contains(&p.provider_id.as_str())
            {
                push(&p.provider_id, &p.model_id, false);
            }
        }
        providers.sort_by(|a, b| {
            (!a.listed, a.provider_name.to_lowercase(), &a.model_id).cmp(&(
                !b.listed,
                b.provider_name.to_lowercase(),
                &b.model_id,
            ))
        });
        let (_, global) = pick_rows(
            self.pricing.iter().filter(|row| row.model_key == model_key),
            "",
            model_key,
        );
        let in_use = providers
            .iter()
            .filter(|p| p.listed)
            .map(|p| p.provider_id.as_str())
            .collect::<BTreeSet<_>>()
            .len();
        let catalog_detail = model_detail_catalog(conn, model_key, count(in_use))?;
        Ok(ModelPriceDetail {
            model_key: model_key.to_owned(),
            row: catalog_detail.row,
            default: self
                .priced_key("", model_key, model_key)
                .map(EffectivePriceView::from),
            global: global.cloned(),
            offers: catalog_detail.offers,
            providers,
        })
    }

    /// The enabled models of the user's providers that no catalog row matches; free local
    /// runtimes are left out.
    #[must_use]
    pub fn unlisted(&self) -> Vec<UnlistedModel> {
        let mut out: Vec<UnlistedModel> = self
            .models
            .iter()
            .filter(|(provider_id, _)| {
                !self
                    .providers
                    .iter()
                    .any(|provider| provider.id == *provider_id && provider.free)
            })
            .filter_map(|(provider_id, model_id)| {
                let key = self.resolve(provider_id, model_id).model_key;
                key.starts_with(UNLISTED_PREFIX).then(|| UnlistedModel {
                    provider_id: provider_id.clone(),
                    provider_name: self.provider_name(provider_id),
                    model_id: model_id.clone(),
                    effective: self
                        .priced_key(provider_id, model_id, &key)
                        .map(EffectivePriceView::from),
                    model_key: key,
                })
            })
            .collect();
        out.sort_by(|a, b| {
            (a.provider_name.to_lowercase(), &a.model_id)
                .cmp(&(b.provider_name.to_lowercase(), &b.model_id))
        });
        out
    }

    /// A models.dev provider that looks like `provider` (its preset id, name or base URL host
    /// names one) and lists at least one of `models`.
    fn suggestion(&self, provider: &ProviderInfo, models: &[&str]) -> Option<String> {
        let host = provider
            .base_url
            .split("://")
            .nth(1)
            .unwrap_or(&provider.base_url)
            .split(['/', ':'])
            .next()
            .unwrap_or_default()
            .to_ascii_lowercase();
        let names = [
            provider
                .preset_id
                .as_deref()
                .map(squash)
                .unwrap_or_default(),
            squash(&provider.name),
        ];
        self.family_names
            .iter()
            .filter(|(family, name)| {
                let (id, name) = (squash(family), squash(name));
                !id.is_empty()
                    && (names
                        .iter()
                        .any(|n| !n.is_empty() && (*n == id || *n == name))
                        || host.split('.').any(|part| squash(part) == id))
            })
            .map(|(family, _)| family)
            .find(|family| self.lists_any(family, &provider.id, models))
            .cloned()
    }

    /// How many of `models` (ids at `provider_id`) `family` has a priced offer of.
    fn matched(&self, family: &str, provider_id: &str, models: &[&str]) -> usize {
        models
            .iter()
            .filter(|model_id| {
                let key = self.resolve(provider_id, model_id).model_key;
                self.offers.get(&key).is_some_and(|model| {
                    model
                        .offers
                        .iter()
                        .any(|offer| offer.family == family && offer.price.is_some())
                })
            })
            .count()
    }

    fn lists_any(&self, family: &str, provider_id: &str, models: &[&str]) -> bool {
        self.matched(family, provider_id, models) > 0
    }

    /// Every provider of the user's with its catalog basis, a suggested one and counts.
    #[must_use]
    pub fn provider_views(&self) -> Vec<ProviderPricingView> {
        self.providers
            .iter()
            .map(|provider| {
                let models: Vec<&str> = self
                    .models
                    .iter()
                    .filter(|(p, _)| *p == provider.id)
                    .map(|(_, m)| m.as_str())
                    .collect();
                let family = self.bases.get(&provider.id).cloned();
                let suggested = self.suggestion(provider, &models);
                ProviderPricingView {
                    provider_id: provider.id.clone(),
                    provider_name: provider.name.clone(),
                    catalog_family_name: family.as_deref().and_then(|f| self.family_name(f)),
                    matched: count(
                        family
                            .as_deref()
                            .map_or(0, |f| self.matched(f, &provider.id, &models)),
                    ),
                    catalog_family: family,
                    suggested_family_name: suggested.as_deref().and_then(|f| self.family_name(f)),
                    suggested_family: suggested,
                    models: count(models.len()),
                    overrides: count(
                        self.pricing
                            .iter()
                            .filter(|row| row.pricing.provider_id == provider.id)
                            .count(),
                    ),
                }
            })
            .collect()
    }
}

#[cfg(test)]
mod tests;
