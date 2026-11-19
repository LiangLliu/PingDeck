//! OpenRouter `/api/v1/credits` with the `/api/v1/key` fallback. The account balance is
//! `total_credits - total_usage`. When `/credits` answers 401 or 403 (an ordinary key may not read
//! it) the adapter falls back to `/key` and reports `limit_remaining` only as
//! `key_limit_remaining_usd` ("Key limit"), never as the account balance. When both answer, both
//! are reported.

use serde_json::Value;

use super::error::{BalanceError, BalanceErrorKind};
use super::extract::{dig, dig_decimal};
use super::fetch::{BalanceAuth, Fetched, Session, bearer};
use super::report::{raw_of, seal};
use super::target::require_key;
use super::{BalanceReport, BalanceScope, BalanceTarget, BalanceUnit};

const CREDITS_PATH: &str = "/api/v1/credits";
const KEY_PATH: &str = "/api/v1/key";

/// The per-key cap of a `/key` body: `Ok(Some(left))` with a cap, `Ok(None)` without one.
fn key_limit(body: &Value) -> Result<Option<f64>, String> {
    let data = dig(body, &["data"]).ok_or("data is missing")?;
    Ok(dig_decimal(data, &["limit_remaining"]))
}

/// `(total_credits, total_usage)` of a `/credits` body.
fn read_credits(body: &Value) -> Result<(f64, f64), BalanceError> {
    let data = dig(body, &["data"]);
    match (
        data.and_then(|data| dig_decimal(data, &["total_credits"])),
        data.and_then(|data| dig_decimal(data, &["total_usage"])),
    ) {
        (Some(total), Some(usage)) => Ok((total, usage)),
        _ => Err(BalanceError::schema(
            "data.total_credits or data.total_usage is missing",
            CREDITS_PATH,
        )),
    }
}

/// Adds the key's own cap when the budget has room for `/key`; only a cancel is an error.
async fn optional_key_cap(
    session: &mut Session<'_>,
    key: &str,
    report: &mut BalanceReport,
    parts: &mut Vec<(&'static str, Value)>,
) -> Result<(), BalanceError> {
    if !session.has_budget() {
        return Ok(());
    }
    match session.get(&session.url(KEY_PATH), vec![bearer(key)]).await {
        Ok(key_body) => {
            if let Ok(Some(left)) = key_limit(&key_body.json) {
                report.key_limit_remaining_usd = Some(left);
                report.labels.push("Key limit".to_owned());
            }
            parts.push((KEY_PATH, key_body.json));
            Ok(())
        }
        Err(error) if error.is_cancelled() => Err(error),
        Err(_) => Ok(()),
    }
}

/// `/key` after `/credits` was `rejected`. A fallback that does not exist leaves the rejection as
/// the answer.
async fn key_fallback(
    session: &mut Session<'_>,
    key: &str,
    rejected: BalanceError,
) -> Result<Fetched, BalanceError> {
    match session.get(&session.url(KEY_PATH), vec![bearer(key)]).await {
        Ok(body) => Ok(body),
        Err(error) if error.kind == BalanceErrorKind::NotFound => Err(rejected),
        Err(error) => Err(error),
    }
}

/// Reads the account credits and, when allowed, the key's own limit.
///
/// # Errors
/// The failure classes of [`Session::get`]; with a rejected `/credits` the error of `/key` (or
/// the original rejection when the budget has no room for the fallback).
pub(super) async fn query(
    session: &mut Session<'_>,
    target: &BalanceTarget<'_>,
    now_ms: i64,
) -> Result<BalanceReport, BalanceError> {
    let key = require_key(target, BalanceAuth::Bearer)?;
    let credits_url = session.url(CREDITS_PATH);
    let mut report = BalanceReport {
        unit: BalanceUnit::Usd,
        ..BalanceReport::default()
    };
    let mut parts: Vec<(&str, Value)> = Vec::new();

    match session.get(&credits_url, vec![bearer(key)]).await {
        Ok(credits) => {
            let (total, usage) = read_credits(&credits.json)?;
            report.source_endpoint = Some(session.endpoint_label(&credits_url));
            report.remaining = Some(total - usage);
            report.remaining_usd = report.remaining;
            report.total = Some(total);
            report.used = Some(usage);
            parts.push((CREDITS_PATH, credits.json));
            optional_key_cap(session, key, &mut report, &mut parts).await?;
        }
        Err(rejected) if rejected.kind == BalanceErrorKind::Auth && session.has_budget() => {
            let key_body = key_fallback(session, key, rejected).await?;
            let left = key_limit(&key_body.json)
                .map_err(|message| BalanceError::schema(message, KEY_PATH))?;
            report.source_endpoint = Some(session.endpoint_label(&session.url(KEY_PATH)));
            report.scope = BalanceScope::Key;
            match left {
                Some(left) => {
                    report.key_limit_remaining_usd = Some(left);
                    report.labels.push("Key limit".to_owned());
                }
                None => report.labels.push("No key limit set".to_owned()),
            }
            report
                .labels
                .push("OpenRouter: the key cannot read account credits".to_owned());
            parts.push((KEY_PATH, key_body.json));
        }
        Err(error) => return Err(error),
    }
    Ok(seal(report, &raw_of(parts), session.secrets(), now_ms))
}
