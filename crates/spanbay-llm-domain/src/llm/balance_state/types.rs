//! Wire types of an account-balance reading: the [`BalanceReport`] every adapter produces and the
//! small value types inside it. A number is never reported without its unit and its source
//! endpoint; USD is set only when the unit is USD or a verified divisor was read.

use serde::{Deserialize, Serialize};
use serde_json::Value;

/// What a balance reading is about.
///
/// Exhaustion follows the scope: a [`BalanceScope::Key`] reading that is exhausted is a per-key
/// condition (the provider needs attention only when every enabled key is exhausted); an exhausted
/// [`BalanceScope::Account`] reading makes the provider need attention by itself. The rule lives in
/// [`crate::llm::records::balance_signal`] and [`crate::llm::status`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BalanceScope {
    /// A quota or budget of the calling key only ("Key remaining").
    Key,
    /// The account behind the key.
    #[default]
    Account,
}

/// The unit of [`BalanceReport::remaining`].
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default, Serialize, Deserialize)]
#[serde(rename_all = "snake_case")]
pub enum BalanceUnit {
    /// US dollars.
    Usd,
    /// Chinese yuan.
    Cny,
    /// Gateway quota units (`remaining / quota_per_unit` is USD when the divisor was read).
    Quota,
    /// A user-labelled unit of a custom query ([`BalanceReport::unit_label`]).
    Custom,
    /// No limit.
    Unlimited,
    /// The unit could not be determined; the raw number is kept.
    #[default]
    Unsupported,
}

/// A rolling-window quota (Sub2API `rate_limits`: `5h`, `1d`, `7d`).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BalanceWindow {
    /// Window name as the service spells it.
    pub window: String,
    /// Window limit.
    #[serde(default)]
    pub limit: Option<f64>,
    /// Used in the window.
    #[serde(default)]
    pub used: Option<f64>,
    /// Left in the window.
    #[serde(default)]
    pub remaining: Option<f64>,
    /// When the window resets, Unix milliseconds.
    #[serde(default)]
    pub resets_at_ms: Option<i64>,
}

/// A second amount of a multi-currency account (DeepSeek reports one entry per currency).
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
pub struct BalanceAlt {
    /// Unit of `remaining`.
    pub unit: BalanceUnit,
    /// The amount.
    pub remaining: f64,
}

/// What a balance endpoint answered, normalised.
#[derive(Debug, Clone, PartialEq, Default, Serialize, Deserialize)]
pub struct BalanceReport {
    /// Whether an adapter produced a reading (false: "No balance API").
    pub supported: bool,
    /// Whether the reading is about the key or the account.
    #[serde(default)]
    pub scope: BalanceScope,
    /// The evidence grade of the adapter's endpoint (drives the provenance tooltip);
    /// `None` for a custom query.
    #[serde(default)]
    pub evidence: Option<super::Evidence>,
    /// Path (and host for vendor endpoints) of the request that produced the number.
    #[serde(default)]
    pub source_endpoint: Option<String>,
    /// Unit of `remaining`, `used` and `total`.
    pub unit: BalanceUnit,
    /// Label of a [`BalanceUnit::Custom`] unit.
    #[serde(default)]
    pub unit_label: Option<String>,
    /// What is left, in `unit`.
    #[serde(default)]
    pub remaining: Option<f64>,
    /// What was spent, in `unit`.
    #[serde(default)]
    pub used: Option<f64>,
    /// Total granted, in `unit`.
    #[serde(default)]
    pub total: Option<f64>,
    /// `remaining` in USD: set only for a USD unit or a verified `quota_per_unit`.
    #[serde(default)]
    pub remaining_usd: Option<f64>,
    /// A per-key spending cap that is *not* the account balance (OpenRouter `limit_remaining`,
    /// LiteLLM `max_budget - spend`).
    #[serde(default)]
    pub key_limit_remaining_usd: Option<f64>,
    /// The quota divisor that was read (never assumed).
    #[serde(default)]
    pub quota_per_unit: Option<f64>,
    /// Subscription plan name.
    #[serde(default)]
    pub plan_name: Option<String>,
    /// Plan or key expiry, Unix milliseconds.
    #[serde(default)]
    pub expires_at_ms: Option<i64>,
    /// Rolling quota windows.
    #[serde(default)]
    pub rate_windows: Vec<BalanceWindow>,
    /// Other currencies of the same account.
    #[serde(default)]
    pub alt: Vec<BalanceAlt>,
    /// Adapter notes ("done-hub: quota is remaining", "Key limit").
    #[serde(default)]
    pub labels: Vec<String>,
    /// Whether what [`Self::scope`] names is exhausted (`remaining <= 0` with a known unit, or
    /// the vendor's own flag). Unlimited is never exhausted. Key-scope exhaustion is per key;
    /// Account-scope exhaustion needs the provider's attention (see [`BalanceScope`]).
    #[serde(default)]
    pub exhausted: bool,
    /// The response body, redacted (account ids and emails removed), for the debug popover.
    #[serde(default)]
    pub raw_json: Value,
    /// When the reading was taken, Unix milliseconds.
    pub checked_at_ms: i64,
}
