//! Model prices: the effective [`Price`] of a model, where it came from, and the legacy
//! per-provider [`PriceOverride`] view of a `model_pricing` row.
//!
//! Prices are per million tokens in a named currency, or per successful call. What a price costs
//! is in `cost`; which price applies is in `basis`.

use serde::{Deserialize, Serialize};

use pingdeck_base::error::AppError;
use pingdeck_model::model_catalog::{
    DEFAULT_CURRENCY, ModelPricing, ModelRecord, PricingMode, check_price, normalize_currency,
};

/// Where an effective price came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceSource {
    /// A `custom` per-token price of `model_pricing`.
    Override,
    /// The model catalog (models.dev, or a model the user added).
    Catalog,
    /// A `fixed` per-call price of `model_pricing`.
    Fixed,
}

impl PriceSource {
    /// The identifier stored in `llm_request_log.price_source`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Override => "override",
            Self::Catalog => "catalog",
            Self::Fixed => "fixed",
        }
    }
}

/// An effective price in one currency: per million tokens, or per successful call.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct Price {
    /// Per million uncached input tokens; 0 when the price is per call.
    pub input_per_mtok: f64,
    /// Per million output tokens; 0 when the price is per call.
    pub output_per_mtok: f64,
    /// Per million cache-read input tokens; `None` bills them at the input price.
    #[serde(default)]
    pub cache_read_per_mtok: Option<f64>,
    /// Per million cache-write input tokens; `None` bills them at the input price.
    #[serde(default)]
    pub cache_write_per_mtok: Option<f64>,
    /// Per million reasoning tokens; `None` bills them at the output price.
    #[serde(default)]
    pub reasoning_per_mtok: Option<f64>,
    /// ISO 4217 code, three upper-case letters.
    pub currency: String,
    /// Where the price came from.
    pub source: PriceSource,
    /// Currency units per successful request ([`PriceSource::Fixed`]): token counts then do not
    /// matter and the per-token fields are 0.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub per_call: Option<f64>,
}

impl Price {
    /// The catalog price of `record`; `None` unless both input and output are known.
    #[must_use]
    pub fn from_catalog(record: &ModelRecord) -> Option<Self> {
        Some(Self {
            input_per_mtok: record.input_per_mtok?,
            output_per_mtok: record.output_per_mtok?,
            cache_read_per_mtok: record.cache_read_per_mtok,
            cache_write_per_mtok: record.cache_write_per_mtok,
            reasoning_per_mtok: record.reasoning_per_mtok,
            currency: record.currency.clone(),
            source: PriceSource::Catalog,
            per_call: None,
        })
    }

    /// The price a stored `custom` or `fixed` row states; `None` for a `dynamic` row and for a
    /// corrupt one (a missing or out-of-range price), which the resolver then skips.
    #[must_use]
    pub fn from_pricing(pricing: &ModelPricing) -> Option<Self> {
        let valid = pricing.clone().validated().ok()?;
        match valid.mode {
            PricingMode::Dynamic => None,
            PricingMode::Fixed => Some(Self {
                input_per_mtok: 0.0,
                output_per_mtok: 0.0,
                cache_read_per_mtok: None,
                cache_write_per_mtok: None,
                reasoning_per_mtok: None,
                currency: valid.currency,
                source: PriceSource::Fixed,
                per_call: valid.per_call,
            }),
            PricingMode::Custom => Some(Self {
                input_per_mtok: valid.input_per_mtok?,
                output_per_mtok: valid.output_per_mtok?,
                cache_read_per_mtok: valid.cache_read_per_mtok,
                cache_write_per_mtok: valid.cache_write_per_mtok,
                reasoning_per_mtok: valid.reasoning_per_mtok,
                currency: valid.currency,
                source: PriceSource::Override,
                per_call: None,
            }),
        }
    }
}

/// A user-set per-token price for one model of one provider: the legacy view
/// (`llm_price_override_*`, `AiConfig.price_overrides`) of a provider-scoped `custom` row of
/// `model_pricing`.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct PriceOverride {
    /// Provider the override applies to.
    pub provider_id: String,
    /// Model id as the provider names it.
    pub model_id: String,
    /// Per million uncached input tokens.
    pub input_per_mtok: f64,
    /// Per million output tokens.
    pub output_per_mtok: f64,
    /// Per million cache-read input tokens.
    #[serde(default)]
    pub cache_read_per_mtok: Option<f64>,
    /// Per million cache-write input tokens.
    #[serde(default)]
    pub cache_write_per_mtok: Option<f64>,
    /// Per million reasoning tokens.
    #[serde(default)]
    pub reasoning_per_mtok: Option<f64>,
    /// ISO 4217 code; defaults to USD.
    #[serde(default = "default_currency")]
    pub currency: String,
    /// When the override was saved, Unix ms.
    #[serde(default)]
    pub updated_at_ms: i64,
}

/// The serde default of a currency field: [`DEFAULT_CURRENCY`].
pub(crate) fn default_currency() -> String {
    DEFAULT_CURRENCY.to_owned()
}

impl PriceOverride {
    /// Checks the override as stored: ids present, every price finite and within
    /// `0..=`[`MAX_PRICE_PER_MTOK`], currency already three upper-case letters.
    ///
    /// [`MAX_PRICE_PER_MTOK`]: pingdeck_model::model_catalog::MAX_PRICE_PER_MTOK
    ///
    /// # Errors
    /// [`AppError::validation`] naming the first problem.
    pub fn validate(&self) -> Result<(), AppError> {
        if self.provider_id.trim().is_empty() || self.model_id.trim().is_empty() {
            return Err(AppError::validation(
                "A price override needs a provider and a model",
            ));
        }
        check_price("Input", self.input_per_mtok)?;
        check_price("Output", self.output_per_mtok)?;
        for (label, value) in [
            ("Cache read", self.cache_read_per_mtok),
            ("Cache write", self.cache_write_per_mtok),
            ("Reasoning", self.reasoning_per_mtok),
        ] {
            if let Some(value) = value {
                check_price(label, value)?;
            }
        }
        if normalize_currency(&self.currency)? == self.currency {
            Ok(())
        } else {
            Err(AppError::validation("Currency must be upper case"))
        }
    }

    /// Normalises the currency (trim, upper case), then [`PriceOverride::validate`]s.
    ///
    /// # Errors
    /// [`AppError::validation`] as [`PriceOverride::validate`] and [`normalize_currency`].
    pub fn validated(mut self) -> Result<Self, AppError> {
        self.currency = normalize_currency(&self.currency)?;
        self.validate()?;
        Ok(self)
    }

    /// The `model_pricing` row this override is.
    #[must_use]
    pub fn to_pricing(&self) -> ModelPricing {
        ModelPricing {
            provider_id: self.provider_id.clone(),
            model_id: self.model_id.clone(),
            mode: PricingMode::Custom,
            per_call: None,
            input_per_mtok: Some(self.input_per_mtok),
            output_per_mtok: Some(self.output_per_mtok),
            cache_read_per_mtok: self.cache_read_per_mtok,
            cache_write_per_mtok: self.cache_write_per_mtok,
            reasoning_per_mtok: self.reasoning_per_mtok,
            currency: self.currency.clone(),
            updated_at_ms: self.updated_at_ms,
            catalog_family: None,
        }
    }

    /// The override a provider-scoped `custom` row states; `None` for any other row.
    #[must_use]
    pub fn from_pricing(pricing: &ModelPricing) -> Option<Self> {
        if pricing.mode != PricingMode::Custom || pricing.provider_id.is_empty() {
            return None;
        }
        Some(Self {
            provider_id: pricing.provider_id.clone(),
            model_id: pricing.model_id.clone(),
            input_per_mtok: pricing.input_per_mtok?,
            output_per_mtok: pricing.output_per_mtok?,
            cache_read_per_mtok: pricing.cache_read_per_mtok,
            cache_write_per_mtok: pricing.cache_write_per_mtok,
            reasoning_per_mtok: pricing.reasoning_per_mtok,
            currency: pricing.currency.clone(),
            updated_at_ms: pricing.updated_at_ms,
        })
    }
}
