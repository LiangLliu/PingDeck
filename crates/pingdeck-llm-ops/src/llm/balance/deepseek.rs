//! DeepSeek `/user/balance`: `balance_infos[]` (decimal strings, one entry per currency) becomes
//! one report whose first entry is `remaining` and the rest `alt`. `is_available = false` is an
//! exhausted account, not an invalid key.

use serde_json::Value;

use super::error::BalanceError;
use super::extract::{currency_unit, decimal, dig, dig_str};
use super::fetch::{BalanceAuth, Session, bearer};
use super::report::{seal, usd_of};
use super::target::require_key;
use super::{BalanceAlt, BalanceReport, BalanceTarget, BalanceUnit};

const PATH: &str = "/user/balance";

/// The report of a `/user/balance` body and its `is_available` flag.
///
/// # Errors
/// A short reason when `balance_infos` is missing or none of its entries holds a number while the
/// account claims to be available.
fn parse(body: &Value) -> Result<(BalanceReport, Option<bool>), String> {
    let infos = dig(body, &["balance_infos"])
        .and_then(Value::as_array)
        .ok_or("balance_infos is missing")?;
    let available = dig(body, &["is_available"]).and_then(Value::as_bool);
    let entries: Vec<(BalanceUnit, f64, &str)> = infos
        .iter()
        .filter_map(|info| {
            let amount = dig(info, &["total_balance"]).and_then(decimal)?;
            let currency = dig_str(info, &["currency"]).unwrap_or("");
            Some((currency_unit(currency), amount, currency))
        })
        .collect();
    if entries.is_empty() && available != Some(false) {
        return Err("balance_infos holds no total_balance".to_owned());
    }
    let mut report = BalanceReport::default();
    if let Some(&(unit, amount, _)) = entries.first() {
        report.unit = unit;
        report.remaining = Some(amount);
        report.remaining_usd = usd_of(unit, amount, None);
    }
    for &(unit, amount, currency) in entries.iter().skip(1) {
        report.alt.push(BalanceAlt {
            unit,
            remaining: amount,
        });
        if unit == BalanceUnit::Unsupported {
            report
                .labels
                .push(format!("{} {amount}", clean_currency(currency)));
        }
    }
    if let Some(&(BalanceUnit::Unsupported, amount, currency)) = entries.first() {
        report
            .labels
            .push(format!("{} {amount}", clean_currency(currency)));
    }
    if available == Some(false) {
        report
            .labels
            .push("DeepSeek: is_available is false".to_owned());
    }
    Ok((report, available))
}

/// A currency code that is safe to print: letters and digits, at most 8 characters.
fn clean_currency(currency: &str) -> String {
    let clean: String = currency
        .chars()
        .filter(char::is_ascii_alphanumeric)
        .take(8)
        .collect();
    if clean.is_empty() {
        "?".to_owned()
    } else {
        clean
    }
}

/// Reads the account balance.
///
/// # Errors
/// The failure classes of [`Session::get`] and a schema mismatch for an unexpected body.
pub(super) async fn query(
    session: &mut Session<'_>,
    target: &BalanceTarget<'_>,
    now_ms: i64,
) -> Result<BalanceReport, BalanceError> {
    let key = require_key(target, BalanceAuth::Bearer)?;
    let url = session.url(PATH);
    let fetched = session.get(&url, vec![bearer(key)]).await?;
    let (mut report, available) =
        parse(&fetched.json).map_err(|message| BalanceError::schema(message, PATH))?;
    report.source_endpoint = Some(session.endpoint_label(&url));
    let mut report = seal(report, &fetched.json, session.secrets(), now_ms);
    // The vendor flag, or every currency being empty, decides; not the first entry alone.
    report.exhausted = available == Some(false)
        || report
            .remaining
            .is_some_and(|first| first <= 0.0 && report.alt.iter().all(|alt| alt.remaining <= 0.0));
    Ok(report)
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn one_currency_reads_as_remaining_with_its_own_unit() {
        let body = json!({"is_available": true, "balance_infos": [
            {"currency": "CNY", "total_balance": "86.40", "granted_balance": "10.00", "topped_up_balance": "76.40"}
        ]});
        let (report, _) = parse(&body).unwrap_or_default();
        assert_eq!(report.unit, BalanceUnit::Cny);
        assert_eq!(report.remaining, Some(86.4));
        assert_eq!(report.remaining_usd, None);
        assert!(report.alt.is_empty());
    }

    #[test]
    fn the_first_currency_is_remaining_and_the_rest_are_alt() {
        let body = json!({"is_available": true, "balance_infos": [
            {"currency": "USD", "total_balance": "5.25"},
            {"currency": "CNY", "total_balance": "0.00"},
            {"currency": "EUR", "total_balance": "1.5"}
        ]});
        let (report, _) = parse(&body).unwrap_or_default();
        assert_eq!(
            (report.unit, report.remaining, report.remaining_usd),
            (BalanceUnit::Usd, Some(5.25), Some(5.25))
        );
        assert_eq!(
            report.alt,
            vec![
                BalanceAlt {
                    unit: BalanceUnit::Cny,
                    remaining: 0.0
                },
                BalanceAlt {
                    unit: BalanceUnit::Unsupported,
                    remaining: 1.5
                },
            ]
        );
        assert_eq!(report.labels, vec!["EUR 1.5".to_owned()]);
    }

    #[test]
    fn unavailable_without_entries_is_exhausted_not_a_mismatch() {
        let (report, available) =
            parse(&json!({"is_available": false, "balance_infos": []})).unwrap_or_default();
        assert_eq!(available, Some(false));
        assert_eq!(report.remaining, None);
        assert!(
            report
                .labels
                .iter()
                .any(|label| label.contains("is_available"))
        );
    }

    #[test]
    fn broken_bodies_are_mismatches() {
        for body in [
            json!({}),
            json!({"balance_infos": "x"}),
            json!({"is_available": true, "balance_infos": []}),
            json!({"is_available": true, "balance_infos": [{"currency": "CNY", "total_balance": "abc"}]}),
        ] {
            assert!(parse(&body).is_err(), "{body}");
        }
    }
}
