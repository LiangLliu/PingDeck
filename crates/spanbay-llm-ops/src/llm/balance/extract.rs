//! Value readers every adapter shares: decimals that may be JSON numbers or strings, JSONPath
//! through the existing response-viewer reader, an object-key path lookup for code adapters, and
//! RFC 3339 timestamps (Sub2API windows).

use serde_json::Value;

use spanbay_response::response_viewer::{JsonPathFilterOutcomeDto, filter_response_jsonpath};

use super::{BalanceUnit, MAX_BODY_BYTES};

/// A finite number from a JSON number or a numeric string (`"86.40"`); anything else is `None`.
pub(super) fn decimal(value: &Value) -> Option<f64> {
    let number = match value {
        Value::Number(number) => number.as_f64()?,
        Value::String(text) => text.trim().parse::<f64>().ok()?,
        _ => return None,
    };
    number.is_finite().then_some(number)
}

/// The value at `path` (object keys only), `None` when a step is missing or `null`.
pub(super) fn dig<'a>(value: &'a Value, path: &[&str]) -> Option<&'a Value> {
    let found = path
        .iter()
        .try_fold(value, |current, key| current.get(*key))?;
    (!found.is_null()).then_some(found)
}

/// [`dig`] read as a string; `None` when the value is missing, `null` or not a string.
pub(super) fn dig_str<'a>(value: &'a Value, path: &[&str]) -> Option<&'a str> {
    dig(value, path).and_then(Value::as_str)
}

/// The unit of a currency code: `CNY` or `USD` in any ASCII case, surrounding whitespace ignored;
/// anything else is unsupported.
pub(super) fn currency_unit(code: &str) -> BalanceUnit {
    let code = code.trim();
    if code.eq_ignore_ascii_case("CNY") {
        BalanceUnit::Cny
    } else if code.eq_ignore_ascii_case("USD") {
        BalanceUnit::Usd
    } else {
        BalanceUnit::Unsupported
    }
}

/// [`dig`] followed by [`decimal`].
pub(super) fn dig_decimal(value: &Value, path: &[&str]) -> Option<f64> {
    dig(value, path).and_then(decimal)
}

/// The single value a JSONPath expression selects in `body_text`.
///
/// # Errors
/// A short reason when the expression is invalid, the body is not JSON or too large, or nothing
/// (or more than one thing) matched.
pub(super) fn path_value(body_text: &str, expression: &str) -> Result<Value, String> {
    match filter_response_jsonpath(body_text, expression, MAX_BODY_BYTES) {
        JsonPathFilterOutcomeDto::Error { message } => Err(message),
        JsonPathFilterOutcomeDto::Filtered { text, mode } => {
            if mode.ends_with("no matches") {
                return Err(format!("{expression} matched nothing"));
            }
            serde_json::from_str(&text).map_err(|_| format!("{expression} did not select a value"))
        }
    }
}

/// [`path_value`] read as a decimal.
///
/// # Errors
/// A short reason as for [`path_value`], or that the value is not a number.
pub(super) fn path_decimal(body_text: &str, expression: &str) -> Result<f64, String> {
    let value = path_value(body_text, expression)?;
    decimal(&value).ok_or_else(|| format!("{expression} is not a number"))
}

/// Unix milliseconds of an RFC 3339 timestamp (`2026-11-07T12:00:00Z`, fractions and numeric
/// offsets allowed).
pub(super) fn parse_rfc3339_ms(text: &str) -> Option<i64> {
    chrono::DateTime::parse_from_rfc3339(text.trim())
        .ok()
        .map(|instant| instant.timestamp_millis())
}

#[cfg(test)]
mod tests {
    use serde_json::json;

    use super::*;

    #[test]
    fn decimals_read_numbers_and_numeric_strings_only() {
        assert_eq!(decimal(&json!(12.5)), Some(12.5));
        assert_eq!(decimal(&json!(" 86.40 ")), Some(86.4));
        assert_eq!(decimal(&json!("1e2")), Some(100.0));
        for bad in [
            json!("inf"),
            json!("NaN"),
            json!("abc"),
            json!(null),
            json!(true),
            json!([1]),
        ] {
            assert_eq!(decimal(&bad), None, "{bad}");
        }
    }

    #[test]
    fn dig_treats_null_as_missing() {
        let body = json!({"data": {"a": 1, "b": null, "c": "2.5"}});
        assert_eq!(dig_decimal(&body, &["data", "a"]), Some(1.0));
        assert_eq!(dig_decimal(&body, &["data", "b"]), None);
        assert_eq!(dig_decimal(&body, &["data", "c"]), Some(2.5));
        assert_eq!(dig_decimal(&body, &["data", "d", "e"]), None);
    }

    #[test]
    fn currency_codes_fold_ascii_case_and_whitespace_only() {
        assert_eq!(currency_unit(" cny "), BalanceUnit::Cny);
        assert_eq!(currency_unit("Usd"), BalanceUnit::Usd);
        for other in ["", "EUR", "CNYX", "C NY", "ＣＮＹ"] {
            assert_eq!(currency_unit(other), BalanceUnit::Unsupported, "{other}");
        }
        let body = json!({"a": {"s": "x", "n": 1, "z": null}});
        assert_eq!(dig_str(&body, &["a", "s"]), Some("x"));
        assert_eq!(dig_str(&body, &["a", "n"]), None);
        assert_eq!(dig_str(&body, &["a", "z"]), None);
    }

    #[test]
    fn jsonpath_values_parse_numbers_strings_and_report_misses() {
        let body = r#"{"data":{"n":49.59,"s":"12.50","list":[{"v":"3"},{"v":4}]}}"#;
        assert_eq!(path_decimal(body, "$.data.n"), Ok(49.59));
        assert_eq!(path_decimal(body, "$.data.s"), Ok(12.5));
        assert_eq!(path_decimal(body, "$.data.list[0].v"), Ok(3.0));
        assert!(path_decimal(body, "$.data.nope").is_err());
        assert!(
            path_decimal(body, "$.data.list[*].v").is_err(),
            "two matches are not one number"
        );
        assert!(path_decimal("<html>", "$.a").is_err());
        assert!(path_decimal(body, "$..[").is_err());
    }

    #[test]
    fn rfc3339_handles_zulu_fractions_and_offsets() {
        assert_eq!(parse_rfc3339_ms("1970-01-01T00:00:00Z"), Some(0));
        assert_eq!(
            parse_rfc3339_ms("2026-11-07T12:00:00Z"),
            Some(1_794_052_800_000)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-11-07T12:00:00.250Z"),
            Some(1_794_052_800_250)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-11-07T20:00:00+08:00"),
            Some(1_794_052_800_000)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-11-07T07:00:00-05:00"),
            Some(1_794_052_800_000)
        );
        assert_eq!(
            parse_rfc3339_ms("2026-11-07T12:00:00.123456789Z"),
            Some(1_794_052_800_123)
        );
        for bad in [
            "",
            "2026-11-07",
            "2026-13-07T00:00:00Z",
            "2026-11-07T25:00:00Z",
            "nope",
        ] {
            assert_eq!(parse_rfc3339_ms(bad), None, "{bad}");
        }
    }
}
