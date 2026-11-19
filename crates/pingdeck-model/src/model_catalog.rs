//! The model catalog and its pricing: the stored row, the pricing modes and every DTO that crosses
//! the IPC boundary for the "Models & pricing" settings page.
//!
//! Single responsibility: data plus the validation of what a user can type (prices, currency,
//! ids, token limits). Reads, writes and the models.dev sync live in `pingdeck-llm`.

use serde::{Deserialize, Serialize};

use pingdeck_base::error::AppError;

use crate::catalog_entry::CatalogEntry;

/// Where the synced rows come from.
pub const MODELS_DEV_URL: &str = "https://models.dev/api.json";
/// Currency of models.dev prices.
pub const DEFAULT_CURRENCY: &str = "USD";
/// Largest accepted price per million tokens (and per call); keeps `tokens * price` far inside
/// `i128`.
pub const MAX_PRICE_PER_MTOK: f64 = 1_000_000.0;
/// Largest accepted context window or output limit.
pub const MAX_TOKEN_LIMIT: u64 = 1 << 40;
/// Longest user-model family or model id, in characters.
pub const MAX_ID_CHARS: usize = 200;
/// The automatic sync intervals the user may pick, in days.
pub const SYNC_INTERVAL_DAYS: [u16; 3] = [1, 7, 30];
/// The interval before the user picks one.
pub const DEFAULT_SYNC_INTERVAL_DAYS: u16 = 7;

/// What a model is for; models.dev types other than chat and embedding are not imported.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogKind {
    /// A chat or completion model.
    #[default]
    Chat,
    /// An embedding model.
    Embedding,
}

impl CatalogKind {
    /// The identifier stored in `model_catalog.kind`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Chat => "chat",
            Self::Embedding => "embedding",
        }
    }

    /// Parses [`CatalogKind::as_str`] output.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "chat" => Some(Self::Chat),
            "embedding" => Some(Self::Embedding),
            _ => None,
        }
    }
}

/// Who owns a catalog row.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
pub enum CatalogSource {
    /// Written by the bundled snapshot or a sync; a sync may rewrite it.
    #[default]
    #[serde(rename = "models.dev")]
    ModelsDev,
    /// Added by the user; a sync never touches it.
    #[serde(rename = "user")]
    User,
}

impl CatalogSource {
    /// The identifier stored in `model_catalog.source`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::ModelsDev => "models.dev",
            Self::User => "user",
        }
    }

    /// Parses [`CatalogSource::as_str`] output.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "models.dev" => Some(Self::ModelsDev),
            "user" => Some(Self::User),
            _ => None,
        }
    }
}

/// Where the models.dev rows came from.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum CatalogOrigin {
    /// The snapshot bundled in the binary; no sync has succeeded yet.
    #[serde(rename = "builtin")]
    Builtin,
    /// A successful online sync.
    #[serde(rename = "models.dev")]
    ModelsDev,
}

impl CatalogOrigin {
    /// The identifier stored in `model_catalog_sync.origin`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Builtin => "builtin",
            Self::ModelsDev => "models.dev",
        }
    }

    /// Parses [`CatalogOrigin::as_str`] output.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "builtin" => Some(Self::Builtin),
            "models.dev" => Some(Self::ModelsDev),
            _ => None,
        }
    }
}

/// How a model is billed.
#[derive(Debug, Clone, Copy, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingMode {
    /// Follow the catalog's synced per-token prices (the default).
    #[default]
    Dynamic,
    /// A fixed price per successful request (a relay that bills per call).
    Fixed,
    /// Per-token prices the user typed.
    Custom,
}

impl PricingMode {
    /// The identifier stored in `model_pricing.mode`.
    #[must_use]
    pub const fn as_str(self) -> &'static str {
        match self {
            Self::Dynamic => "dynamic",
            Self::Fixed => "fixed",
            Self::Custom => "custom",
        }
    }

    /// Parses [`PricingMode::as_str`] output.
    #[must_use]
    pub fn parse(text: &str) -> Option<Self> {
        match text {
            "dynamic" => Some(Self::Dynamic),
            "fixed" => Some(Self::Fixed),
            "custom" => Some(Self::Custom),
            _ => None,
        }
    }
}

/// The pricing-mode filter of a catalog query by the model's global pricing: a mode, or `none`
/// for "dynamic with no catalog price" (the cost is unknown), a subset of `dynamic`.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PricingFilter {
    /// No global pricing row: the catalog price applies (priced or not).
    Dynamic,
    /// Global mode fixed.
    Fixed,
    /// Global mode custom.
    Custom,
    /// Dynamic and no catalog price.
    None,
}

/// One row of `model_catalog`: a model of a family with the facts and prices models.dev (or the
/// user) gives for it.
#[derive(Debug, Clone, Default, PartialEq, Serialize, Deserialize)]
pub struct ModelRecord {
    /// The models.dev provider id (`openai`, `anthropic`, …) or the user's own family.
    pub family: String,
    /// Display name of the family.
    pub family_name: String,
    /// The model id as providers name it.
    pub model_id: String,
    /// Display name of the model.
    pub name: String,
    /// models.dev's `release_date` (`YYYY-MM` or `YYYY-MM-DD`); `None` when unknown, always for
    /// user rows.
    #[serde(default)]
    pub release_date: Option<String>,
    /// Chat or embedding.
    pub kind: CatalogKind,
    /// Context window in tokens.
    pub context_tokens: Option<u64>,
    /// Maximum generated tokens.
    pub max_output_tokens: Option<u64>,
    /// Supports reasoning.
    pub reasoning: bool,
    /// Accepts images.
    pub vision: bool,
    /// models.dev's known input modalities (`text`, `image`, `audio`, `video`, `pdf`, in that
    /// order); empty when unknown, always for user rows.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub input_modalities: Vec<String>,
    /// Supports tool calls.
    pub tools: bool,
    /// Has a cache-read or cache-write price.
    pub prompt_caching: bool,
    /// models.dev's `canonical_model_id` (`<family>/<id>`); empty for an original.
    pub canonical_id: String,
    /// Per million input tokens.
    pub input_per_mtok: Option<f64>,
    /// Per million output tokens.
    pub output_per_mtok: Option<f64>,
    /// Per million cache-read input tokens.
    pub cache_read_per_mtok: Option<f64>,
    /// Per million cache-write input tokens.
    pub cache_write_per_mtok: Option<f64>,
    /// Per million reasoning tokens.
    pub reasoning_per_mtok: Option<f64>,
    /// ISO 4217 code of the prices.
    pub currency: String,
    /// Who owns the row.
    pub source: CatalogSource,
    /// When the row was written: the snapshot date for bundled rows, the sync time for synced
    /// rows, the save time for user rows (Unix ms).
    pub synced_at_ms: i64,
}

impl ModelRecord {
    /// Whether the row prices both input and output (the minimum for a per-token cost).
    #[must_use]
    pub fn has_price(&self) -> bool {
        self.input_per_mtok.is_some() && self.output_per_mtok.is_some()
    }

    /// Whether the row's own family published it: no canonical id, or one under this family.
    #[must_use]
    pub fn is_first_party(&self) -> bool {
        self.canonical_id.is_empty()
            || self
                .canonical_id
                .strip_prefix(self.family.as_str())
                .is_some_and(|rest| rest.starts_with('/'))
    }
}

impl From<&ModelRecord> for CatalogEntry {
    fn from(record: &ModelRecord) -> Self {
        Self {
            model_id: record.model_id.clone(),
            provider: record.family.clone(),
            mode: record.kind.as_str().to_owned(),
            max_input_tokens: record.context_tokens,
            max_output_tokens: record.max_output_tokens,
            vision: record.vision,
            tools: record.tools,
            reasoning: record.reasoning,
            prompt_caching: record.prompt_caching,
            input_cost_per_mtok: record.input_per_mtok,
            output_cost_per_mtok: record.output_per_mtok,
            cache_read_cost_per_mtok: record.cache_read_per_mtok,
            input_modalities: record.input_modalities.clone(),
        }
    }
}

/// Pricing of one model for one provider (`provider_id` empty: every provider).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelPricing {
    /// The provider the row applies to; empty for every provider.
    pub provider_id: String,
    /// The model id as the provider names it.
    pub model_id: String,
    /// How the model is billed.
    pub mode: PricingMode,
    /// Fixed: currency units per successful request; `None` otherwise.
    pub per_call: Option<f64>,
    /// Custom: per million uncached input tokens; `None` otherwise.
    pub input_per_mtok: Option<f64>,
    /// Custom: per million output tokens.
    pub output_per_mtok: Option<f64>,
    /// Custom: per million cache-read input tokens.
    pub cache_read_per_mtok: Option<f64>,
    /// Custom: per million cache-write input tokens.
    pub cache_write_per_mtok: Option<f64>,
    /// Custom: per million reasoning tokens.
    pub reasoning_per_mtok: Option<f64>,
    /// ISO 4217 code, three upper-case letters.
    pub currency: String,
    /// When the row was saved, Unix ms.
    pub updated_at_ms: i64,
    /// Dynamic only: bill at this models.dev provider's catalog offer of the model instead of
    /// the official price; `None` follows the default.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub catalog_family: Option<String>,
}

/// Trims and upper-cases a currency code.
///
/// # Errors
/// [`AppError::validation`] unless the result is one of [`SUPPORTED_CURRENCIES`].
pub fn normalize_currency(text: &str) -> Result<String, AppError> {
    let code = text.trim().to_ascii_uppercase();
    if SUPPORTED_CURRENCIES.contains(&code.as_str()) {
        Ok(code)
    } else {
        Err(AppError::validation(format!(
            "Currency must be one of {}",
            SUPPORTED_CURRENCIES.join(", ")
        )))
    }
}

/// The currencies a price, a balance or a cost may be in (C-$2): every save goes through
/// [`normalize_currency`], so costs in different currencies are never summed or compared by a
/// code the app does not know.
pub const SUPPORTED_CURRENCIES: [&str; 6] = ["USD", "CNY", "EUR", "GBP", "JPY", "HKD"];

/// Checks that `value` is finite and within `0..=`[`MAX_PRICE_PER_MTOK`].
///
/// # Errors
/// [`AppError::validation`] naming `label`.
pub fn check_price(label: &str, value: f64) -> Result<(), AppError> {
    if value.is_finite() && (0.0..=MAX_PRICE_PER_MTOK).contains(&value) {
        Ok(())
    } else {
        Err(AppError::validation(format!(
            "{label} price must be a number between 0 and {MAX_PRICE_PER_MTOK}"
        )))
    }
}

fn check_optional_price(label: &str, value: Option<f64>) -> Result<(), AppError> {
    value.map_or(Ok(()), |value| check_price(label, value))
}

fn check_limit(label: &str, value: Option<u64>) -> Result<(), AppError> {
    match value {
        Some(0) => Err(AppError::validation(format!("{label} must be at least 1"))),
        Some(tokens) if tokens > MAX_TOKEN_LIMIT => Err(AppError::validation(format!(
            "{label} must be at most {MAX_TOKEN_LIMIT}"
        ))),
        _ => Ok(()),
    }
}

fn check_id(label: &str, value: &str, max_chars: usize) -> Result<(), AppError> {
    if value.is_empty() || value.trim() != value {
        return Err(AppError::validation(format!(
            "{label} must not be empty or padded with spaces"
        )));
    }
    if value.chars().count() > max_chars {
        return Err(AppError::validation(format!(
            "{label} must be at most {max_chars} characters"
        )));
    }
    Ok(())
}

impl ModelPricing {
    /// The default pricing of a model that has no stored row.
    #[must_use]
    pub fn dynamic(provider_id: &str, model_id: &str) -> Self {
        Self {
            provider_id: provider_id.to_owned(),
            model_id: model_id.to_owned(),
            mode: PricingMode::Dynamic,
            per_call: None,
            input_per_mtok: None,
            output_per_mtok: None,
            cache_read_per_mtok: None,
            cache_write_per_mtok: None,
            reasoning_per_mtok: None,
            currency: DEFAULT_CURRENCY.to_owned(),
            updated_at_ms: 0,
            catalog_family: None,
        }
    }

    /// Trims the ids, normalises the currency, drops the fields the mode does not use and checks
    /// the rest: prices finite within `0..=`[`MAX_PRICE_PER_MTOK`], `custom` has input and
    /// output, `fixed` has `per_call`.
    ///
    /// # Errors
    /// [`AppError::validation`] naming the first problem.
    pub fn validated(mut self) -> Result<Self, AppError> {
        self.provider_id = self.provider_id.trim().to_owned();
        self.model_id = self.model_id.trim().to_owned();
        if self.model_id.is_empty() {
            return Err(AppError::validation("A price needs a model"));
        }
        self.currency = normalize_currency(&self.currency)?;
        self.catalog_family = self
            .catalog_family
            .map(|family| family.trim().to_owned())
            .filter(|family| !family.is_empty());
        if let Some(family) = &self.catalog_family {
            if self.mode != PricingMode::Dynamic {
                return Err(AppError::validation(
                    "Only a dynamic price can follow a catalog provider",
                ));
            }
            check_id("Catalog provider", family, MAX_ID_CHARS)?;
        }
        match self.mode {
            PricingMode::Dynamic => {
                self.per_call = None;
                self.clear_token_prices();
            }
            PricingMode::Fixed => {
                self.clear_token_prices();
                check_price(
                    "Per-call",
                    self.per_call.ok_or_else(|| {
                        AppError::validation("A fixed price needs a per-call price")
                    })?,
                )?;
            }
            PricingMode::Custom => {
                self.per_call = None;
                check_price(
                    "Input",
                    self.input_per_mtok.ok_or_else(|| {
                        AppError::validation("A custom price needs an input price")
                    })?,
                )?;
                check_price(
                    "Output",
                    self.output_per_mtok.ok_or_else(|| {
                        AppError::validation("A custom price needs an output price")
                    })?,
                )?;
                check_optional_price("Cache read", self.cache_read_per_mtok)?;
                check_optional_price("Cache write", self.cache_write_per_mtok)?;
                check_optional_price("Reasoning", self.reasoning_per_mtok)?;
            }
        }
        Ok(self)
    }

    fn clear_token_prices(&mut self) {
        self.input_per_mtok = None;
        self.output_per_mtok = None;
        self.cache_read_per_mtok = None;
        self.cache_write_per_mtok = None;
        self.reasoning_per_mtok = None;
    }
}

/// A user-added model (`source = 'user'`); a sync never touches it.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct UserModelInput {
    /// The family the model is listed under.
    pub family: String,
    /// The model id as providers name it.
    pub model_id: String,
    /// Display name.
    pub name: String,
    /// Chat or embedding.
    pub kind: CatalogKind,
    /// Context window in tokens.
    pub context_tokens: Option<u64>,
    /// Maximum generated tokens.
    pub max_output_tokens: Option<u64>,
    /// Supports reasoning.
    pub reasoning: bool,
    /// Accepts images.
    pub vision: bool,
    /// Supports tool calls.
    pub tools: bool,
    /// Per million input tokens.
    pub input_per_mtok: Option<f64>,
    /// Per million output tokens.
    pub output_per_mtok: Option<f64>,
    /// Per million cache-read input tokens.
    pub cache_read_per_mtok: Option<f64>,
    /// Per million cache-write input tokens.
    pub cache_write_per_mtok: Option<f64>,
    /// Per million reasoning tokens.
    pub reasoning_per_mtok: Option<f64>,
    /// ISO 4217 code of the prices.
    pub currency: String,
}

impl UserModelInput {
    /// Trims the text fields (an empty name becomes the model id), normalises the currency and
    /// checks ids, limits and prices.
    ///
    /// # Errors
    /// [`AppError::validation`] naming the first problem.
    pub fn validated(mut self) -> Result<Self, AppError> {
        self.family = self.family.trim().to_owned();
        self.model_id = self.model_id.trim().to_owned();
        self.name = self.name.trim().to_owned();
        check_id("Family", &self.family, MAX_ID_CHARS)?;
        check_id("Model id", &self.model_id, MAX_ID_CHARS)?;
        if self.name.is_empty() {
            self.name.clone_from(&self.model_id);
        }
        check_limit("Context window", self.context_tokens)?;
        check_limit("Max output", self.max_output_tokens)?;
        self.currency = normalize_currency(&self.currency)?;
        for (label, value) in [
            ("Input", self.input_per_mtok),
            ("Output", self.output_per_mtok),
            ("Cache read", self.cache_read_per_mtok),
            ("Cache write", self.cache_write_per_mtok),
            ("Reasoning", self.reasoning_per_mtok),
        ] {
            check_optional_price(label, value)?;
        }
        Ok(self)
    }
}

/// Filters and paging of the catalog list; every field optional.
#[derive(Debug, Clone, Default, PartialEq, Eq, Serialize, Deserialize)]
#[serde(default)]
pub struct ModelCatalogQuery {
    /// Case-insensitive substring of the model id, name or family name.
    pub search: Option<String>,
    /// Exact family id.
    pub family: Option<String>,
    /// Only this kind.
    pub kind: Option<CatalogKind>,
    /// Only rows of this owner.
    pub source: Option<CatalogSource>,
    /// Only models whose global pricing is this.
    pub mode: Option<PricingFilter>,
    /// Only models released by this publisher.
    pub publisher: Option<String>,
    /// Lists the offers of this one model (one row per provider) instead; the other filters are
    /// ignored.
    pub model_key: Option<String>,
    /// Only models one of the user's providers serves.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub in_use: Option<bool>,
    /// Browse the distributor-own entries (models.dev rows no lab released) instead of the lab
    /// models.
    #[serde(skip_serializing_if = "Option::is_none")]
    pub distributor_entries: Option<bool>,
    /// Rows to skip (default 0).
    pub offset: Option<u32>,
    /// Rows to return (default 100, clamped to 1..=500).
    pub limit: Option<u32>,
}

/// One family with its model count.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalogFamily {
    /// Family id.
    pub id: String,
    /// Display name.
    pub name: String,
    /// Models listed under it.
    pub models: u32,
    /// A lab (some models.dev canonical id names it); `false` for a distributor's own entries.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub lab: bool,
}

/// A catalog row as the page lists it: the facts, the catalog prices and the model's pricing.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCatalogRow {
    /// Family id.
    pub family: String,
    /// Display name of the family.
    pub family_name: String,
    /// The model id.
    pub model_id: String,
    /// Display name.
    pub name: String,
    /// Release date, `YYYY-MM` or `YYYY-MM-DD`; `None` when unknown (always for user rows).
    #[serde(default)]
    pub release_date: Option<String>,
    /// The model this row stands for (see `docs/decisions.md`, model grouping): models.dev's
    /// canonical id, a matched one, `<family>/<model_id>`, or `user:<family>/<model_id>`.
    #[serde(default)]
    pub model_key: String,
    /// Who released the model (`anthropic`, `openai`, …); the row's own family when unknown.
    #[serde(default)]
    pub publisher: String,
    /// Display name of the publisher.
    #[serde(default)]
    pub publisher_name: String,
    /// Catalog rows (providers) that share `model_key`, this one included.
    #[serde(default)]
    pub provider_count: u32,
    /// Chat or embedding.
    pub kind: CatalogKind,
    /// Context window in tokens.
    pub context_tokens: Option<u64>,
    /// Maximum generated tokens.
    pub max_output_tokens: Option<u64>,
    /// Supports reasoning.
    pub reasoning: bool,
    /// Accepts images.
    pub vision: bool,
    /// Supports tool calls.
    pub tools: bool,
    /// Catalog price per million input tokens.
    pub input_per_mtok: Option<f64>,
    /// Catalog price per million output tokens.
    pub output_per_mtok: Option<f64>,
    /// Catalog price per million cache-read tokens.
    pub cache_read_per_mtok: Option<f64>,
    /// Catalog price per million cache-write tokens.
    pub cache_write_per_mtok: Option<f64>,
    /// Catalog price per million reasoning tokens.
    pub reasoning_per_mtok: Option<f64>,
    /// ISO 4217 code of the catalog prices.
    pub currency: String,
    /// Who owns the row.
    pub source: CatalogSource,
    /// When the row was written, Unix ms.
    pub synced_at_ms: i64,
    /// The global (every provider) pricing of this model id; dynamic when none is stored.
    pub pricing: ModelPricing,
    /// How many of the user's providers have a provider-scoped pricing row for this model.
    pub provider_overrides: u32,
    /// Where the row's catalog prices come from: the official offer, the reference (lower
    /// median) offer, or none.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub price_basis: Option<DefaultBasis>,
    /// The family whose offer gives the row's catalog prices.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis_family: Option<String>,
    /// Display name of `basis_family`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub basis_family_name: Option<String>,
    /// Reference basis only: the display names of the offers the median was taken from.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference_from: Vec<String>,
    /// How many of the user's providers serve this model (omitted when none).
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub in_use: Option<u32>,
}

/// Which catalog offer prices a model by default.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum DefaultBasis {
    /// The offer of the model's own publisher.
    Official,
    /// No official offer: the lower median of the other priced offers.
    Reference,
    /// No offer has a price.
    None,
}

/// Where an effective price came from, in precedence order.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum PriceBasisKind {
    /// The provider-scoped row of the model.
    ProviderModel,
    /// The provider-level basis (a models.dev provider's catalog prices).
    Provider,
    /// The every-provider row of the model.
    Model,
    /// The official offer.
    Official,
    /// The reference offer.
    Reference,
}

/// Why a price applies.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct PriceBasis {
    /// Which rule gave the price.
    pub kind: PriceBasisKind,
    /// The catalog family whose offer gave the price, when one did.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family: Option<String>,
    /// Display name of `family`.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub family_name: Option<String>,
    /// Reference only: the display names of the offers compared.
    #[serde(default, skip_serializing_if = "Vec::is_empty")]
    pub reference_from: Vec<String>,
    /// The provider-level basis does not list the model, so a later rule priced it.
    #[serde(default, skip_serializing_if = "std::ops::Not::not")]
    pub fallback: bool,
    /// The relay's multiplier (and billing currency) applied on top of the price that rule gave.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub relay: Option<Box<RelayPricing>>,
}

/// Largest relay multiplier a provider may declare.
pub const MAX_RELAY_MULTIPLIER: f64 = 1_000.0;

/// How a relay prices its models against the official prices ("倍率"): every price that is not
/// the relay's own (a provider row of the model) is multiplied by `multiplier` and, when
/// `currency` is set, read in that currency (a relay that bills ¥1 per $1 of official price).
/// Stored with the provider (`quirks.relay_pricing`), so it needs no table of its own.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct RelayPricing {
    /// Factor on the official price, above 0 and at most [`MAX_RELAY_MULTIPLIER`].
    pub multiplier: f64,
    /// ISO 4217 code the relay bills in; `None` keeps the price's own currency.
    #[serde(default, skip_serializing_if = "Option::is_none")]
    pub currency: Option<String>,
}

// `check` admits finite multipliers only, so the `f64` never holds NaN and equality is total.
impl Eq for RelayPricing {}

impl RelayPricing {
    /// Checks the stored form: a finite multiplier above 0 and at most
    /// [`MAX_RELAY_MULTIPLIER`], a currency already in [`SUPPORTED_CURRENCIES`] upper case.
    ///
    /// # Errors
    /// [`AppError::validation`] naming the problem.
    pub fn check(&self) -> Result<(), AppError> {
        if !(self.multiplier.is_finite()
            && self.multiplier > 0.0
            && self.multiplier <= MAX_RELAY_MULTIPLIER)
        {
            return Err(AppError::validation(format!(
                "The relay multiplier must be above 0 and at most {MAX_RELAY_MULTIPLIER}"
            )));
        }
        match self.currency.as_deref() {
            Some(code) if normalize_currency(code)? != code => {
                Err(AppError::validation("Currency must be upper case"))
            }
            _ => Ok(()),
        }
    }
}

/// One provider of the user's following a models.dev provider's catalog prices.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ProviderPricing {
    /// The user's provider.
    pub provider_id: String,
    /// The models.dev provider id (catalog family) whose offers price it.
    pub catalog_family: String,
    /// When it was saved, Unix ms.
    pub updated_at_ms: i64,
}

/// The user's statement that a provider's model id is a catalog model.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelAlias {
    /// The user's provider.
    pub provider_id: String,
    /// The id as that provider names it.
    pub model_id: String,
    /// The catalog model it is.
    pub model_key: String,
    /// When it was saved, Unix ms (ignored on save).
    #[serde(default)]
    pub updated_at_ms: i64,
}

/// How a model id was matched to a model.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum ResolveMatch {
    /// The user's alias.
    Alias,
    /// The id is a model key.
    Key,
    /// A catalog row has the id.
    Catalog,
    /// Nothing matched: the key is `id:<model id>`.
    None,
}

/// The model a provider's model id stands for.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelResolution {
    /// The model key.
    pub model_key: String,
    /// How it was found.
    pub matched: ResolveMatch,
}

impl ModelResolution {
    /// The key of an id nothing matched.
    #[must_use]
    pub fn unlisted(model_id: &str) -> Self {
        Self {
            model_key: format!("{UNLISTED_PREFIX}{model_id}"),
            matched: ResolveMatch::None,
        }
    }
}

/// Prefix of the key of a model id no catalog row matches.
pub const UNLISTED_PREFIX: &str = "id:";

/// Canonical D-259 model-id resolution over alias, key and catalog lookup callbacks.
///
/// Precedence: `id:` prefix passthrough, then per-provider alias, then exact `model_key`, then
/// catalog `(family, model_id)` lookup, else `id:<model_id>`.
///
/// # Errors
/// Propagates any error returned by the lookup callbacks.
pub fn resolve_with<E>(
    provider_id: Option<&str>,
    model_id: &str,
    mut find_alias: impl FnMut(&str, &str) -> Result<Option<String>, E>,
    mut is_known_key: impl FnMut(&str) -> Result<bool, E>,
    mut find_catalog_key: impl FnMut(&str) -> Result<Option<String>, E>,
) -> Result<ModelResolution, E> {
    let model_id = model_id.trim();
    let found = |model_key: String, matched| ModelResolution { model_key, matched };
    if model_id.starts_with(UNLISTED_PREFIX) {
        return Ok(found(model_id.to_owned(), ResolveMatch::Key));
    }
    if let Some(provider_id) = provider_id.filter(|id| !id.is_empty())
        && let Some(key) = find_alias(provider_id, model_id)?
    {
        return Ok(found(key, ResolveMatch::Alias));
    }
    if is_known_key(model_id)? {
        return Ok(found(model_id.to_owned(), ResolveMatch::Key));
    }
    if let Some(key) = find_catalog_key(model_id)?
        && !key.is_empty()
    {
        return Ok(found(key, ResolveMatch::Catalog));
    }
    Ok(ModelResolution::unlisted(model_id))
}

impl ModelCatalogRow {
    /// The listed form of `record`, as a model of its own (key `<family>/<model_id>`, publisher
    /// its family, one provider) until the list says otherwise.
    #[must_use]
    pub fn new(record: ModelRecord, pricing: ModelPricing, provider_overrides: u32) -> Self {
        Self {
            model_key: format!("{}/{}", record.family, record.model_id),
            publisher: record.family.clone(),
            publisher_name: record.family_name.clone(),
            provider_count: 1,
            family: record.family,
            family_name: record.family_name,
            model_id: record.model_id,
            name: record.name,
            release_date: record.release_date,
            kind: record.kind,
            context_tokens: record.context_tokens,
            max_output_tokens: record.max_output_tokens,
            reasoning: record.reasoning,
            vision: record.vision,
            tools: record.tools,
            input_per_mtok: record.input_per_mtok,
            output_per_mtok: record.output_per_mtok,
            cache_read_per_mtok: record.cache_read_per_mtok,
            cache_write_per_mtok: record.cache_write_per_mtok,
            reasoning_per_mtok: record.reasoning_per_mtok,
            currency: record.currency,
            source: record.source,
            synced_at_ms: record.synced_at_ms,
            pricing,
            provider_overrides,
            price_basis: None,
            basis_family: None,
            basis_family_name: None,
            reference_from: Vec::new(),
            in_use: None,
        }
    }
}

/// One page of the catalog list.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCatalogPage {
    /// The rows of this page.
    pub rows: Vec<ModelCatalogRow>,
    /// Rows matching the query, all pages.
    pub total: u32,
    /// Every family with its count, independent of the query.
    pub families: Vec<ModelCatalogFamily>,
    /// Every publisher with its model count (models, not offers), independent of the query.
    #[serde(default)]
    pub publishers: Vec<ModelCatalogFamily>,
}

/// What one successful sync changed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub struct ModelCatalogSyncReport {
    /// Rows new to the catalog.
    pub added: u32,
    /// Existing synced rows whose content changed.
    pub updated: u32,
    /// Existing synced rows that did not change.
    pub unchanged: u32,
    /// Models the download contained.
    pub total_remote: u32,
    /// When the sync finished, Unix ms.
    pub finished_at_ms: i64,
    /// How long the download and the write took.
    pub duration_ms: u64,
}

/// Where the catalog stands: its origin, size, last sync and the automatic-sync schedule.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct ModelCatalogStatus {
    /// Bundled snapshot until a sync succeeded.
    pub origin: CatalogOrigin,
    /// Where syncs download from.
    pub source_url: String,
    /// Date of the bundled snapshot, Unix ms.
    pub snapshot_at_ms: i64,
    /// All rows.
    pub entry_count: u32,
    /// Rows owned by models.dev.
    pub models_dev_count: u32,
    /// Rows the user added.
    pub user_count: u32,
    /// The last successful sync, Unix ms.
    pub last_success_ms: Option<i64>,
    /// The last sync attempt, successful or not, Unix ms.
    pub last_attempt_ms: Option<i64>,
    /// Why the last attempt failed, in English; `None` after a success.
    pub last_error: Option<String>,
    /// What kind of failure `last_error` is, for a localised sentence; `None` after a success.
    pub last_error_kind: Option<CatalogSyncErrorKind>,
    /// The HTTP status models.dev answered with, for [`CatalogSyncErrorKind::Http`].
    pub last_error_http_status: Option<u16>,
    /// The last successful sync's report.
    pub last_report: Option<ModelCatalogSyncReport>,
    /// The setting `ai.providers.catalog_auto_refresh`.
    pub auto_sync: bool,
    /// Days between automatic syncs: 1, 7 or 30.
    pub auto_interval_days: u16,
    /// When the next automatic sync is due; `None` with auto-sync off.
    pub next_due_ms: Option<i64>,
}

/// Why a catalog sync failed.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum CatalogSyncErrorKind {
    /// The request could not be made or was cut off (offline, DNS, TLS, reset).
    Network,
    /// models.dev did not answer within the time limit.
    Timeout,
    /// models.dev answered with a non-success HTTP status.
    Http,
    /// The download is larger than the size limit.
    TooLarge,
    /// The body is not a models.dev catalog.
    Invalid,
    /// The catalog holds too few models to be trusted.
    TooFew,
    /// Anything else (a database error, say).
    Other,
}

#[cfg(test)]
mod tests {
    use super::*;

    fn custom() -> ModelPricing {
        ModelPricing {
            mode: PricingMode::Custom,
            input_per_mtok: Some(1.0),
            output_per_mtok: Some(2.0),
            currency: " cny ".into(),
            ..ModelPricing::dynamic("p1", " m1 ")
        }
    }

    #[test]
    fn pricing_is_normalised_and_checked_per_mode() {
        let ok = custom().validated().unwrap();
        assert_eq!(
            (ok.currency.as_str(), ok.model_id.as_str(), ok.per_call),
            ("CNY", "m1", None)
        );
        let fixed = ModelPricing {
            mode: PricingMode::Fixed,
            per_call: Some(0.02),
            input_per_mtok: Some(9.0),
            ..custom()
        }
        .validated()
        .unwrap();
        assert_eq!((fixed.per_call, fixed.input_per_mtok), (Some(0.02), None));
        let dynamic = ModelPricing {
            mode: PricingMode::Dynamic,
            per_call: Some(1.0),
            ..custom()
        }
        .validated()
        .unwrap();
        assert_eq!(
            (
                dynamic.per_call,
                dynamic.input_per_mtok,
                dynamic.output_per_mtok
            ),
            (None, None, None)
        );
        for bad in [
            ModelPricing {
                input_per_mtok: None,
                ..custom()
            },
            ModelPricing {
                output_per_mtok: Some(f64::NAN),
                ..custom()
            },
            ModelPricing {
                cache_read_per_mtok: Some(-1.0),
                ..custom()
            },
            ModelPricing {
                reasoning_per_mtok: Some(2_000_000.0),
                ..custom()
            },
            ModelPricing {
                currency: "dollars".into(),
                ..custom()
            },
            ModelPricing {
                model_id: "  ".into(),
                ..custom()
            },
            ModelPricing {
                mode: PricingMode::Fixed,
                per_call: None,
                ..custom()
            },
            ModelPricing {
                mode: PricingMode::Fixed,
                per_call: Some(f64::INFINITY),
                ..custom()
            },
        ] {
            assert!(
                matches!(bad.validated(), Err(AppError::Validation { .. })),
                "accepted"
            );
        }
    }

    fn user() -> UserModelInput {
        UserModelInput {
            family: " mine ".into(),
            model_id: "m-1".into(),
            name: String::new(),
            kind: CatalogKind::Chat,
            context_tokens: Some(8_192),
            max_output_tokens: None,
            reasoning: false,
            vision: false,
            tools: true,
            input_per_mtok: Some(1.0),
            output_per_mtok: None,
            cache_read_per_mtok: None,
            cache_write_per_mtok: None,
            reasoning_per_mtok: None,
            currency: "usd".into(),
        }
    }

    #[test]
    fn a_user_model_is_trimmed_named_and_limited() {
        let ok = user().validated().unwrap();
        assert_eq!(
            (ok.family.as_str(), ok.name.as_str(), ok.currency.as_str()),
            ("mine", "m-1", "USD")
        );
        for bad in [
            UserModelInput {
                family: String::new(),
                ..user()
            },
            UserModelInput {
                model_id: "x".repeat(201),
                ..user()
            },
            UserModelInput {
                context_tokens: Some(0),
                ..user()
            },
            UserModelInput {
                max_output_tokens: Some(MAX_TOKEN_LIMIT + 1),
                ..user()
            },
            UserModelInput {
                input_per_mtok: Some(-0.1),
                ..user()
            },
            UserModelInput {
                currency: "US".into(),
                ..user()
            },
        ] {
            assert!(
                matches!(bad.validated(), Err(AppError::Validation { .. })),
                "accepted"
            );
        }
    }

    #[test]
    fn first_party_rows_have_no_canonical_id_or_one_under_their_family() {
        let row = |family: &str, canonical: &str| ModelRecord {
            family: family.into(),
            canonical_id: canonical.into(),
            ..ModelRecord::default()
        };
        assert!(row("openai", "").is_first_party());
        assert!(row("openai", "openai/gpt-4o").is_first_party());
        assert!(!row("openai", "openai-compat/gpt-4o").is_first_party());
        assert!(!row("azure", "openai/gpt-4o").is_first_party());
    }

    fn value<T: Serialize>(v: &T) -> serde_json::Value {
        serde_json::to_value(v).unwrap()
    }

    /// Custom `skip_serializing_if` rules that `ts-rs` type generation does not exercise at runtime.
    #[test]
    fn custom_skip_serializing_if_fields_keep_wire_shape() {
        use serde_json::json;
        let pricing = ModelPricing {
            mode: PricingMode::Dynamic,
            input_per_mtok: None,
            output_per_mtok: None,
            cache_write_per_mtok: None,
            catalog_family: Some("openrouter".into()),
            ..ModelPricing {
                provider_id: "p1".into(),
                model_id: "m1".into(),
                mode: PricingMode::Custom,
                per_call: None,
                input_per_mtok: Some(1.0),
                output_per_mtok: Some(2.0),
                cache_read_per_mtok: None,
                cache_write_per_mtok: Some(0.5),
                reasoning_per_mtok: None,
                currency: "USD".into(),
                updated_at_ms: 9,
                catalog_family: None,
            }
        };
        assert_eq!(value(&pricing)["catalog_family"], json!("openrouter"));
        assert!(
            value(&ModelPricing::dynamic("p1", "m1"))
                .get("catalog_family")
                .is_none()
        );

        assert_eq!(
            value(&PriceBasis {
                kind: PriceBasisKind::ProviderModel,
                family: None,
                family_name: None,
                reference_from: Vec::new(),
                fallback: false,
                relay: None,
            }),
            json!({ "kind": "provider_model" })
        );

        let page = ModelCatalogPage {
            rows: Vec::new(),
            total: 0,
            families: vec![ModelCatalogFamily {
                id: "openai".into(),
                name: "OpenAI".into(),
                models: 46,
                lab: false,
            }],
            publishers: vec![ModelCatalogFamily {
                id: "openai".into(),
                name: "OpenAI".into(),
                models: 40,
                lab: true,
            }],
        };
        assert_eq!(
            value(&page),
            json!({
                "rows": [],
                "total": 0,
                "families": [{ "id": "openai", "name": "OpenAI", "models": 46 }],
                "publishers": [{ "id": "openai", "name": "OpenAI", "models": 40, "lab": true }]
            })
        );
    }

    type Lookup<T> = Result<T, &'static str>;

    /// Runs [`resolve_with`] with fixed lookup answers; also returns which lookups ran, with the
    /// arguments they got.
    fn resolve_logged(
        provider_id: Option<&str>,
        model_id: &str,
        alias: Lookup<Option<&str>>,
        known: Lookup<bool>,
        catalog: Lookup<Option<&str>>,
    ) -> (Lookup<ModelResolution>, Vec<String>) {
        let calls = std::cell::RefCell::new(Vec::new());
        let result = resolve_with(
            provider_id,
            model_id,
            |provider, model| {
                calls.borrow_mut().push(format!("alias {provider} {model}"));
                alias.map(|key| key.map(str::to_owned))
            },
            |model| {
                calls.borrow_mut().push(format!("key {model}"));
                known
            },
            |model| {
                calls.borrow_mut().push(format!("catalog {model}"));
                catalog.map(|key| key.map(str::to_owned))
            },
        );
        (result, calls.into_inner())
    }

    #[test]
    fn resolve_with_follows_d259_precedence_and_skips_needless_lookups() {
        let found = |key: &str, matched| -> Lookup<ModelResolution> {
            Ok(ModelResolution {
                model_key: key.to_owned(),
                matched,
            })
        };
        // An `id:` key passes through without any lookup.
        let (result, calls) = resolve_logged(
            Some("p"),
            " id:x ",
            Ok(Some("f/a")),
            Ok(true),
            Ok(Some("f/c")),
        );
        assert_eq!(result, found("id:x", ResolveMatch::Key));
        assert!(calls.is_empty(), "{calls:?}");

        // No provider, or an empty one, never asks for an alias.
        for provider in [None, Some("")] {
            let (result, calls) =
                resolve_logged(provider, "m", Ok(Some("f/a")), Ok(false), Ok(None));
            assert_eq!(result, found("id:m", ResolveMatch::None), "{provider:?}");
            assert_eq!(calls, ["key m", "catalog m"], "{provider:?}");
        }

        // The alias wins over a known key; the id is trimmed before every lookup.
        let (result, calls) =
            resolve_logged(Some("p"), " m ", Ok(Some("f/a")), Ok(true), Ok(Some("f/c")));
        assert_eq!(result, found("f/a", ResolveMatch::Alias));
        assert_eq!(calls, ["alias p m"]);

        // A known key wins over the catalog.
        let (result, calls) = resolve_logged(Some("p"), "m", Ok(None), Ok(true), Ok(Some("f/c")));
        assert_eq!(result, found("m", ResolveMatch::Key));
        assert_eq!(calls, ["alias p m", "key m"]);

        // The catalog answers last; an empty catalog key counts as no match.
        let (result, _) = resolve_logged(Some("p"), "m", Ok(None), Ok(false), Ok(Some("f/c")));
        assert_eq!(result, found("f/c", ResolveMatch::Catalog));
        let (result, calls) = resolve_logged(Some("p"), " m ", Ok(None), Ok(false), Ok(Some("")));
        assert_eq!(result, found("id:m", ResolveMatch::None));
        assert_eq!(calls, ["alias p m", "key m", "catalog m"]);

        // A failing lookup is returned as is and stops the resolution.
        let (result, calls) = resolve_logged(Some("p"), "m", Err("alias"), Ok(true), Ok(None));
        assert_eq!((result, calls.len()), (Err("alias"), 1));
        let (result, calls) = resolve_logged(Some("p"), "m", Ok(None), Err("key"), Ok(None));
        assert_eq!((result, calls.len()), (Err("key"), 2));
        let (result, _) = resolve_logged(Some("p"), "m", Ok(None), Ok(false), Err("catalog"));
        assert_eq!(result, Err("catalog"));
    }
}
