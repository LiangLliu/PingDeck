//! The rules every adapter result goes through: [`seal`] (`supported`, the check time, the
//! exhausted flag, credential scrubbing and the redacted `raw_json`), the USD conversion helper
//! (only for USD or a read divisor) and the multi-request raw body.

use serde_json::Value;

use pingdeck_base::secrets::{redact_json, redact_secrets};

use super::{BalanceReport, BalanceUnit};

/// `remaining` in USD: the amount itself for USD, `remaining / quota_per_unit` for quota with a
/// positive divisor that was read, nothing otherwise.
pub(super) fn usd_of(
    unit: BalanceUnit,
    remaining: f64,
    quota_per_unit: Option<f64>,
) -> Option<f64> {
    match unit {
        BalanceUnit::Usd => Some(remaining),
        BalanceUnit::Quota => quota_per_unit
            .filter(|divisor| *divisor > 0.0)
            .map(|divisor| remaining / divisor),
        _ => None,
    }
}

/// Whether `unit` is a known currency or quota (an unknown unit is never "exhausted").
const fn is_known_unit(unit: BalanceUnit) -> bool {
    matches!(
        unit,
        BalanceUnit::Usd | BalanceUnit::Cny | BalanceUnit::Quota | BalanceUnit::Custom
    )
}

/// Finishes a report: marks it supported, stamps the time, redacts and attaches the raw body and
/// sets `exhausted` to `remaining <= 0` with a known unit (an adapter that has a better rule
/// overrides the flag afterwards). Unlimited is never exhausted.
pub(super) fn seal(
    mut report: BalanceReport,
    raw: &Value,
    secrets: &[String],
    now_ms: i64,
) -> BalanceReport {
    report.supported = true;
    report.checked_at_ms = now_ms;
    report.raw_json = redact_json(raw, secrets);
    // Vendor text that reaches the report outside raw_json never carries a credential either.
    let scrub = |text: &str| redact_secrets(text, secrets);
    report.plan_name = report.plan_name.as_deref().map(scrub);
    for label in &mut report.labels {
        *label = scrub(label);
    }
    for window in &mut report.rate_windows {
        window.window = scrub(&window.window);
    }
    report.exhausted = report.unit != BalanceUnit::Unlimited
        && is_known_unit(report.unit)
        && report.remaining.is_some_and(|remaining| remaining <= 0.0);
    report
}

/// The raw body of a check that made several requests: the single body as it is, or an object
/// keyed by endpoint path.
pub(super) fn raw_of(mut parts: Vec<(&str, Value)>) -> Value {
    if parts.len() == 1
        && let Some((_, only)) = parts.pop()
    {
        return only;
    }
    Value::Object(
        parts
            .into_iter()
            .map(|(path, body)| (path.to_owned(), body))
            .collect(),
    )
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;
    use crate::llm::balance::BalanceWindow;

    #[test]
    fn usd_needs_a_usd_unit_or_a_read_divisor() {
        assert_eq!(usd_of(BalanceUnit::Usd, 12.4, None), Some(12.4));
        assert_eq!(
            usd_of(BalanceUnit::Quota, 6_200_000.0, Some(500_000.0)),
            Some(12.4)
        );
        assert_eq!(usd_of(BalanceUnit::Quota, 6_200_000.0, None), None);
        assert_eq!(usd_of(BalanceUnit::Quota, 6_200_000.0, Some(0.0)), None);
        assert_eq!(usd_of(BalanceUnit::Cny, 86.4, Some(500_000.0)), None);
    }

    #[test]
    fn exhausted_needs_a_known_unit_and_unlimited_never_is() {
        let report = |unit, remaining| {
            seal(
                BalanceReport {
                    unit,
                    remaining,
                    ..BalanceReport::default()
                },
                &json!({}),
                &[],
                7,
            )
        };
        assert!(report(BalanceUnit::Cny, Some(0.0)).exhausted);
        assert!(report(BalanceUnit::Quota, Some(0.0)).exhausted);
        assert!(!report(BalanceUnit::Usd, Some(0.01)).exhausted);
        assert!(!report(BalanceUnit::Unsupported, Some(0.0)).exhausted);
        assert!(!report(BalanceUnit::Unlimited, Some(0.0)).exhausted);
        assert!(!report(BalanceUnit::Usd, None).exhausted);
        let sealed = report(BalanceUnit::Usd, Some(1.0));
        assert!(sealed.supported);
        assert_eq!(sealed.checked_at_ms, 7);
    }

    #[test]
    fn seal_scrubs_plan_name_labels_and_window_names() {
        let echoed = "my-secret-value-1234";
        let shaped = "sk-abcdefghijklmnopqrstuvwxyz0123456789";
        let window = |name: String| BalanceWindow {
            window: name,
            limit: None,
            used: None,
            remaining: None,
            resets_at_ms: None,
        };
        let report = seal(
            BalanceReport {
                plan_name: Some(format!("plan {echoed} {shaped}")),
                labels: vec![
                    format!("note {echoed}"),
                    format!("key {shaped}"),
                    "plain".to_owned(),
                ],
                rate_windows: vec![
                    window(format!("5h {echoed}")),
                    window(format!("1d {shaped}")),
                ],
                ..BalanceReport::default()
            },
            &json!({}),
            &[echoed.to_owned()],
            1,
        );
        let mut texts: Vec<&str> = report.labels.iter().map(String::as_str).collect();
        texts.extend(report.plan_name.as_deref());
        texts.extend(report.rate_windows.iter().map(|w| w.window.as_str()));
        assert_eq!(texts.len(), 6);
        for text in texts {
            assert!(!text.contains(echoed) && !text.contains(shaped), "{text}");
        }
        assert_eq!(report.labels[2], "plain");
    }

    #[test]
    fn sealed_raw_json_uses_the_shared_table() {
        let raw = json!({
            "code": 0,
            "data": {
                "email": "a@b.c",
                "user_id": 42,
                "Username": "ann",
                "account_id": "acc-1",
                "nested": [{"display_name": "X", "balance": "12.40"}],
                "note": "key sk-abcdefghijklmnopqrstuvwxyz0123456789 leaked",
                "exact": "my-secret-value-1234",
                "path": "https://x.test/t/AbCdEfGhIjKlMnOpQrStUvWx/v1",
            }
        });
        let report = seal(
            BalanceReport::default(),
            &raw,
            &["my-secret-value-1234".to_owned()],
            1,
        );
        let text = report.raw_json.to_string();
        for leaked in [
            "a@b.c",
            "ann",
            "acc-1",
            "sk-abcdefghijklmnopqrstuvwxyz0123456789",
            "my-secret-value-1234",
            "AbCdEfGhIjKlMnOpQrStUvWx",
        ] {
            assert!(!text.contains(leaked), "{leaked} in {text}");
        }
        assert_eq!(report.raw_json["data"]["email"], json!("…"));
        assert_eq!(report.raw_json["data"]["account_id"], json!("…"));
        assert_eq!(
            report.raw_json["data"]["nested"][0]["balance"],
            json!("12.40")
        );
        assert_eq!(report.raw_json["code"], json!(0));
    }
}
