//! `/api/status` facts shared by the new-api and one-api adapters: the divisor (`quota_per_unit`)
//! and `display_in_currency`, taken from what the sniffer or the provider stored and read from the
//! public `/api/status` only when a needed fact is still missing. Never assumed.

use serde_json::Value;

use super::BalanceTarget;
use super::error::BalanceError;
use super::extract::{dig, dig_decimal};
use super::fetch::Session;

/// The public status path.
const STATUS_PATH: &str = "/api/status";

/// The facts an adapter needs; only a missing needed fact makes the status request.
#[derive(Debug, Clone, Copy)]
pub(super) struct Wanted {
    /// The divisor is needed.
    pub quota_per_unit: bool,
    /// `display_in_currency` is needed.
    pub display_in_currency: bool,
}

/// What is known about a gateway's quota display.
#[derive(Debug, Clone, Copy, Default, PartialEq)]
pub(super) struct GatewayFacts {
    /// Quota units per USD, positive, as read.
    pub quota_per_unit: Option<f64>,
    /// Whether the gateway displays currency instead of quota.
    pub display_in_currency: Option<bool>,
}

fn usable(divisor: f64) -> bool {
    divisor.is_finite() && divisor > 0.0
}

/// The facts of `target`, completed from `GET /api/status` on the session's origin (no
/// credential) when a wanted fact is missing and the budget allows the request.
///
/// A failing status request is not an error of the balance check: the missing fact stays `None`
/// and the adapter shows the raw number. Only a cancel stops the check.
///
/// # Errors
/// [`super::BalanceErrorKind::Cancelled`] only.
pub(super) async fn read_facts(
    session: &mut Session<'_>,
    target: &BalanceTarget<'_>,
    wanted: Wanted,
) -> Result<GatewayFacts, BalanceError> {
    let mut facts = GatewayFacts {
        quota_per_unit: target.quota_per_unit.filter(|divisor| usable(*divisor)),
        display_in_currency: target.display_in_currency,
    };
    let missing = (wanted.quota_per_unit && facts.quota_per_unit.is_none())
        || (wanted.display_in_currency && facts.display_in_currency.is_none());
    if !missing || !session.has_budget() {
        return Ok(facts);
    }
    match session.get(&session.url(STATUS_PATH), Vec::new()).await {
        Ok(status) => {
            facts.quota_per_unit = facts.quota_per_unit.or_else(|| {
                dig_decimal(&status.json, &["data", "quota_per_unit"]).filter(|d| usable(*d))
            });
            facts.display_in_currency = facts.display_in_currency.or_else(|| {
                dig(&status.json, &["data", "display_in_currency"]).and_then(Value::as_bool)
            });
            Ok(facts)
        }
        Err(error) if error.is_cancelled() => Err(error),
        Err(_) => Ok(facts),
    }
}
