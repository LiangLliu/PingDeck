//! The one-api billing facade: `hard_limit_usd` (granted total) and `total_usage` (spent, times
//! 100) from the OpenAI-style dashboard endpoints, in their real unit: USD only when the gateway
//! says `display_in_currency`, quota with a read divisor otherwise, and a raw number when neither
//! is known. A hard limit of 100,000,000 is an unlimited token.

use serde_json::Value;

use super::error::BalanceError;
use super::extract::{dig_decimal, dig_str};
use super::fetch::{BalanceAuth, Fetched, Session, bearer};
use super::gateway::{GatewayFacts, Wanted, read_facts};
use super::report::{raw_of, seal, usd_of};
use super::target::require_key;
use super::{BalanceReport, BalanceScope, BalanceTarget, BalanceUnit};

const SUBSCRIPTION_PATH: &str = "/v1/dashboard/billing/subscription";
const USAGE_PATH: &str = "/v1/dashboard/billing/usage";
/// `amount` one-api reports for a token without a quota limit.
const UNLIMITED_AMOUNT: f64 = 100_000_000.0;

/// A 200 answer that carries an `error` object instead of data.
fn error_message(body: &Value) -> Option<String> {
    let error = body.get("error").filter(|error| !error.is_null())?;
    Some(
        dig_str(error, &["message"])
            .unwrap_or("The service reported an error")
            .to_owned(),
    )
}

fn checked(fetched: &Fetched, path: &str, session: &Session<'_>) -> Result<(), BalanceError> {
    match error_message(&fetched.json) {
        Some(message) => Err(BalanceError::schema(session.safe_text(&message), path)),
        None => Ok(()),
    }
}

/// Fills the limited reading: `hard_limit - spent` in the unit `display_in_currency` names, the
/// divisor for quota, the USD amount and the unit label.
fn apply_usage(report: &mut BalanceReport, hard_limit: f64, spent: f64, facts: GatewayFacts) {
    report.unit = match facts.display_in_currency {
        Some(true) => BalanceUnit::Usd,
        Some(false) => BalanceUnit::Quota,
        None => BalanceUnit::Unsupported,
    };
    report.total = Some(hard_limit);
    report.used = Some(spent);
    report.remaining = Some(hard_limit - spent);
    if report.unit == BalanceUnit::Quota {
        report.quota_per_unit = facts.quota_per_unit;
    }
    report.remaining_usd = report
        .remaining
        .and_then(|remaining| usd_of(report.unit, remaining, report.quota_per_unit));
    report.labels.push(
        match report.unit {
            BalanceUnit::Usd => "one-api: hard limit minus usage / 100",
            BalanceUnit::Quota => "one-api: quota units (display_in_currency is false)",
            _ => "one-api: display_in_currency unknown, unit unknown",
        }
        .to_owned(),
    );
}

/// Reads the billing facade of a one-api gateway.
///
/// # Errors
/// The failure classes of [`Session::get`]; a body without the expected fields is a mismatch.
pub(super) async fn query(
    session: &mut Session<'_>,
    target: &BalanceTarget<'_>,
    now_ms: i64,
) -> Result<BalanceReport, BalanceError> {
    let key = require_key(target, BalanceAuth::Bearer)?;
    let subscription_url = session.url(SUBSCRIPTION_PATH);
    let subscription = session.get(&subscription_url, vec![bearer(key)]).await?;
    checked(&subscription, SUBSCRIPTION_PATH, session)?;
    let hard_limit = dig_decimal(&subscription.json, &["hard_limit_usd"])
        .ok_or_else(|| BalanceError::schema("hard_limit_usd is missing", SUBSCRIPTION_PATH))?;
    // The billing facade answers for the calling key (or, when the gateway shows user stats, for the
    // account; the two cannot be told apart, so the conservative per-key scope is reported).
    let mut report = BalanceReport {
        labels: vec!["Key remaining".to_owned()],
        scope: BalanceScope::Key,
        source_endpoint: Some(session.endpoint_label(&subscription_url)),
        expires_at_ms: dig_decimal(&subscription.json, &["access_until"])
            .filter(|seconds| *seconds > 0.0)
            .map(|seconds| (seconds * 1000.0) as i64),
        ..BalanceReport::default()
    };
    if hard_limit >= UNLIMITED_AMOUNT {
        report.unit = BalanceUnit::Unlimited;
        report.labels.push("one-api: unlimited token".to_owned());
        return Ok(seal(report, &subscription.json, session.secrets(), now_ms));
    }

    let usage_url = session.url(USAGE_PATH);
    let usage = session.get(&usage_url, vec![bearer(key)]).await?;
    checked(&usage, USAGE_PATH, session)?;
    let total_usage = dig_decimal(&usage.json, &["total_usage"])
        .ok_or_else(|| BalanceError::schema("total_usage is missing", USAGE_PATH))?;
    // total_usage is amount x 100 (cents of the same unit as hard_limit_usd).
    let spent = total_usage / 100.0;
    let wanted = Wanted {
        quota_per_unit: target.display_in_currency != Some(true),
        display_in_currency: true,
    };
    let facts = read_facts(session, target, wanted).await?;
    apply_usage(&mut report, hard_limit, spent, facts);
    let raw = raw_of(vec![
        (SUBSCRIPTION_PATH, subscription.json),
        (USAGE_PATH, usage.json),
    ]);
    Ok(seal(report, &raw, session.secrets(), now_ms))
}
