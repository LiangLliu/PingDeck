//! Sub2API `GET /v1/usage` in both modes: `quota_limited` keys report their quota and rate
//! windows; `unrestricted` keys report a wallet or a subscription. `remaining = -1` is an unlimited
//! subscription, decided from the `*_limit_usd` fields being null, not from the value.

use serde_json::Value;

use super::error::BalanceError;
use super::extract::{dig, dig_decimal, dig_str, parse_rfc3339_ms};
use super::fetch::{BalanceAuth, Session, bearer};
use super::report::{seal, usd_of};
use super::target::require_key;
use super::{BalanceReport, BalanceScope, BalanceTarget, BalanceUnit, BalanceWindow};

const PATH: &str = "/v1/usage";
/// The subscription windows in report order: name, limit field, usage field.
const WINDOWS: [(&str, &str, &str); 3] = [
    ("daily", "daily_limit_usd", "daily_usage_usd"),
    ("weekly", "weekly_limit_usd", "weekly_usage_usd"),
    ("monthly", "monthly_limit_usd", "monthly_usage_usd"),
];

/// USD when the unit says so in any ASCII case (no trimming); anything else is unsupported.
fn usd_or_unsupported(value: Option<&Value>) -> BalanceUnit {
    match value.and_then(Value::as_str) {
        Some(unit) if unit.eq_ignore_ascii_case("USD") => BalanceUnit::Usd,
        _ => BalanceUnit::Unsupported,
    }
}

/// The rate windows (`5h`, `1d`, `7d`) of a quota-limited key.
fn rate_windows(body: &Value) -> Vec<BalanceWindow> {
    dig(body, &["rate_limits"])
        .and_then(Value::as_array)
        .map(|windows| {
            windows
                .iter()
                .filter_map(|window| {
                    Some(BalanceWindow {
                        window: dig_str(window, &["window"])?.to_owned(),
                        limit: dig_decimal(window, &["limit"]),
                        used: dig_decimal(window, &["used"]),
                        remaining: dig_decimal(window, &["remaining"]),
                        resets_at_ms: dig_str(window, &["reset_at"]).and_then(parse_rfc3339_ms),
                    })
                })
                .collect()
        })
        .unwrap_or_default()
}

/// `quota_limited`: the key's quota (when it has one) and its rate windows.
fn quota_limited(body: &Value, report: &mut BalanceReport) {
    let quota = dig(body, &["quota"]);
    let unit = quota
        .and_then(|quota| dig(quota, &["unit"]))
        .or_else(|| dig(body, &["unit"]));
    report.unit = usd_or_unsupported(unit);
    report.remaining = quota
        .and_then(|quota| dig_decimal(quota, &["remaining"]))
        .or_else(|| dig_decimal(body, &["remaining"]));
    report.total = quota.and_then(|quota| dig_decimal(quota, &["limit"]));
    report.used = quota.and_then(|quota| dig_decimal(quota, &["used"]));
    report.rate_windows = rate_windows(body);
    if report.remaining.is_some() {
        report.labels.push("Key remaining".to_owned());
    } else {
        report.unit = BalanceUnit::Unsupported;
        report
            .labels
            .push("Sub2API: the key has rate limits only".to_owned());
    }
    match dig_str(body, &["status"]) {
        Some("quota_exhausted") => report
            .labels
            .push("Sub2API: key quota exhausted".to_owned()),
        Some("expired") => report.labels.push("Sub2API: key expired".to_owned()),
        _ => {}
    }
    report.expires_at_ms = dig_str(body, &["expires_at"]).and_then(parse_rfc3339_ms);
}

/// `unrestricted` with a subscription: usage against the daily, weekly and monthly limits.
fn subscription(body: &Value, sub: &Value, report: &mut BalanceReport) {
    let windows: Vec<BalanceWindow> = WINDOWS
        .into_iter()
        .filter_map(|(name, limit_field, usage_field)| {
            let limit = dig_decimal(sub, &[limit_field])?;
            let used = dig_decimal(sub, &[usage_field]);
            Some(BalanceWindow {
                window: name.to_owned(),
                limit: Some(limit),
                used,
                remaining: Some((limit - used.unwrap_or(0.0)).max(0.0)),
                resets_at_ms: None,
            })
        })
        .collect();
    report.expires_at_ms = dig_str(sub, &["expires_at"]).and_then(parse_rfc3339_ms);
    if windows.is_empty() {
        report.unit = BalanceUnit::Unlimited;
        report
            .labels
            .push("Sub2API: subscription without limits".to_owned());
        return;
    }
    report.unit = usd_or_unsupported(dig(body, &["unit"]));
    report.remaining = dig_decimal(body, &["remaining"])
        .filter(|remaining| *remaining >= 0.0)
        .or_else(|| {
            windows
                .iter()
                .filter_map(|window| window.remaining)
                .reduce(f64::min)
        });
    report.rate_windows = windows;
}

/// Reads the key's balance, quota or subscription.
///
/// # Errors
/// The failure classes of [`Session::get`]; a body that is neither mode is a mismatch.
pub(super) async fn query(
    session: &mut Session<'_>,
    target: &BalanceTarget<'_>,
    now_ms: i64,
) -> Result<BalanceReport, BalanceError> {
    let key = require_key(target, BalanceAuth::Bearer)?;
    let url = session.url(PATH);
    let fetched = session.get(&url, vec![bearer(key)]).await?;
    let body = &fetched.json;
    let mut report = BalanceReport {
        source_endpoint: Some(session.endpoint_label(&url)),
        plan_name: dig_str(body, &["planName"]).map(str::to_owned),
        ..BalanceReport::default()
    };
    match (dig_str(body, &["mode"]), dig(body, &["subscription"])) {
        (Some("quota_limited"), _) => {
            report.scope = BalanceScope::Key;
            quota_limited(body, &mut report);
        }
        (_, Some(sub)) if sub.is_object() => subscription(body, sub, &mut report),
        (_, _) if dig(body, &["remaining"]).is_some() || dig(body, &["balance"]).is_some() => {
            // Wallet: remaining equals the balance.
            report.unit = usd_or_unsupported(dig(body, &["unit"]));
            report.remaining =
                dig_decimal(body, &["remaining"]).or_else(|| dig_decimal(body, &["balance"]));
        }
        (Some("unrestricted"), None) => {
            report.unit = BalanceUnit::Unsupported;
            report
                .labels
                .push("Sub2API: subscription details unavailable".to_owned());
        }
        _ => return Err(BalanceError::schema("The answer has neither mode", PATH)),
    }
    report.remaining_usd = report
        .remaining
        .and_then(|remaining| usd_of(report.unit, remaining, None));
    let mut report = seal(report, body, session.secrets(), now_ms);
    report.exhausted |= dig_str(body, &["status"]) == Some("quota_exhausted");
    Ok(report)
}
