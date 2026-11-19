//! Extraction rules shared by the declarative adapters and the custom query: where the numbers are
//! in a body (JSONPath, read with the response-viewer reader), what unit they have, and the draft
//! [`BalanceReport`]. A missing `remaining` path is a mismatch; a string number is a decimal; a
//! host-dependent unit without a mapping for the host is `Unsupported` with the raw number.

use std::collections::BTreeMap;

use serde::{Deserialize, Serialize};
use serde_json::Value;

use super::extract::{decimal, path_decimal, path_value};
use super::report::usd_of;
use super::{BalanceReport, BalanceScope, BalanceUnit};
pub(super) use pingdeck_llm_domain::llm::balance_state::UnitName;

/// A host-dependent unit: the longest `by_host` suffix of the request host wins.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ByHost {
    /// Host suffix (`moonshot.cn`) to unit.
    pub by_host: BTreeMap<String, UnitName>,
}

/// The unit of a reading.
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum UnitSpec {
    /// The same unit on every host.
    Fixed(UnitName),
    /// The unit depends on the host.
    ByHost(ByHost),
}

/// The divisor of a quota unit.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(untagged)]
pub enum DivisorSpec {
    /// A fixed divisor.
    Number(f64),
    /// The provider's harvested `quota_per_unit` (nothing is converted when it is unknown).
    Provider(ProviderDivisor),
}

/// The only named divisor.
#[derive(Debug, Clone, Copy, PartialEq, Eq, Serialize, Deserialize)]
pub enum ProviderDivisor {
    /// `provider.quota_per_unit`.
    #[serde(rename = "provider.quota_per_unit")]
    QuotaPerUnit,
}

/// A path that must hold a value.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Condition {
    /// JSONPath of the value.
    pub path: String,
    /// The value that makes the condition true.
    pub equals: Value,
}

/// A vendor envelope (`code: 20000`, `success: true`) that must say OK.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct Envelope {
    /// JSONPath of the status field.
    pub ok_path: String,
    /// The status that means success.
    pub ok_equals: Value,
    /// JSONPath of the vendor's message, shown when the status says failure.
    #[serde(default)]
    pub message_path: Option<String>,
}

/// An extra amount that goes to the report labels (voucher balance, frozen balance).
#[derive(Debug, Clone, PartialEq, Eq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct LabelPath {
    /// Name shown before the number.
    pub name: String,
    /// JSONPath of the amount; a missing path is skipped.
    pub path: String,
}

/// Where the numbers are and what they mean.
#[derive(Debug, Clone, PartialEq, Serialize, Deserialize)]
#[serde(deny_unknown_fields)]
pub struct ExtractRules {
    /// JSONPath of the remaining amount.
    pub remaining: String,
    /// JSONPath of the spent amount.
    #[serde(default)]
    pub used: Option<String>,
    /// JSONPath of the granted total.
    #[serde(default)]
    pub total: Option<String>,
    /// Whether the numbers are about the key or the account.
    pub scope: BalanceScope,
    /// Unit of the numbers.
    pub unit: UnitSpec,
    /// Divisor of a quota unit.
    #[serde(default)]
    pub divisor: Option<DivisorSpec>,
    /// When this holds the account is unlimited.
    #[serde(default)]
    pub unlimited_when: Option<Condition>,
    /// Envelope check.
    #[serde(default)]
    pub envelope: Option<Envelope>,
    /// Extra amounts for the labels.
    #[serde(default)]
    pub labels: Vec<LabelPath>,
}

/// Whether `host` is `suffix` or a subdomain of it.
pub(super) fn host_matches(host: &str, suffix: &str) -> bool {
    let host = host.trim_end_matches('.').as_bytes();
    let suffix = suffix.trim_matches('.').as_bytes();
    // Byte slices: ASCII case folding only, and no slice can split a UTF-8 character into a panic.
    host.eq_ignore_ascii_case(suffix)
        || (host.len() > suffix.len()
            && host[host.len() - suffix.len()..].eq_ignore_ascii_case(suffix)
            && host[host.len() - suffix.len() - 1] == b'.')
}

impl UnitSpec {
    /// The unit for requests to `host`; `Unsupported` when a host-dependent unit has no mapping.
    pub(super) fn resolve(&self, host: &str) -> BalanceUnit {
        match self {
            Self::Fixed(name) => (*name).into(),
            Self::ByHost(by_host) => by_host
                .by_host
                .iter()
                .filter(|(suffix, _)| host_matches(host, suffix))
                .max_by_key(|(suffix, _)| suffix.len())
                .map_or(BalanceUnit::Unsupported, |(_, name)| (*name).into()),
        }
    }
}

/// Whether the value at `path` equals `equals` (numbers compare as numbers).
fn holds(body: &str, path: &str, equals: &Value) -> bool {
    path_value(body, path).is_ok_and(|found| match (&found, equals) {
        (Value::Number(left), Value::Number(right)) => left.as_f64() == right.as_f64(),
        (left, right) => left == right,
    })
}

impl ExtractRules {
    /// Whether the envelope (when there is one) says OK.
    ///
    /// # Errors
    /// The vendor message, or a fixed text when the envelope has none, when it says failure.
    fn check_envelope(&self, body: &str) -> Result<(), String> {
        let Some(envelope) = &self.envelope else {
            return Ok(());
        };
        if holds(body, &envelope.ok_path, &envelope.ok_equals) {
            return Ok(());
        }
        let message = envelope
            .message_path
            .as_deref()
            .and_then(|path| path_value(body, path).ok())
            .and_then(|value| value.as_str().map(str::to_owned));
        Err(message.unwrap_or_else(|| "The vendor reported a failure".to_owned()))
    }

    /// The quota divisor: the fixed one or the provider's, positive and finite only.
    fn divisor(&self, provider_quota_per_unit: Option<f64>) -> Option<f64> {
        match &self.divisor {
            Some(DivisorSpec::Number(divisor)) => Some(*divisor),
            Some(DivisorSpec::Provider(ProviderDivisor::QuotaPerUnit)) => provider_quota_per_unit,
            None => None,
        }
        .filter(|divisor| *divisor > 0.0 && divisor.is_finite())
    }

    /// Appends `name amount` for every label path that holds a number, in rule order.
    fn push_label_amounts(&self, body: &str, report: &mut BalanceReport) {
        for label in &self.labels {
            if let Some(value) = path_value(body, &label.path)
                .ok()
                .as_ref()
                .and_then(decimal)
            {
                report.labels.push(format!("{} {value}", label.name));
            }
        }
    }

    /// Reads `body` (text of a 2xx answer) into a report draft for a request to `host`.
    ///
    /// # Errors
    /// The reason the body does not match: the envelope says failure (with the vendor message),
    /// or the `remaining` path is missing or not a number.
    pub(super) fn evaluate(
        &self,
        body: &str,
        host: &str,
        provider_quota_per_unit: Option<f64>,
    ) -> Result<BalanceReport, String> {
        self.check_envelope(body)?;
        let mut report = BalanceReport {
            scope: self.scope,
            unit: self.unit.resolve(host),
            ..BalanceReport::default()
        };
        if self
            .unlimited_when
            .as_ref()
            .is_some_and(|condition| holds(body, &condition.path, &condition.equals))
        {
            report.unit = BalanceUnit::Unlimited;
            report.labels.push("Unlimited".to_owned());
            return Ok(report);
        }
        report.remaining = Some(path_decimal(body, &self.remaining)?);
        report.used = self
            .used
            .as_deref()
            .and_then(|path| path_decimal(body, path).ok());
        report.total = self
            .total
            .as_deref()
            .and_then(|path| path_decimal(body, path).ok());
        if report.unit == BalanceUnit::Quota {
            report.quota_per_unit = self.divisor(provider_quota_per_unit);
        }
        report.remaining_usd = report
            .remaining
            .and_then(|remaining| usd_of(report.unit, remaining, report.quota_per_unit));
        if report.unit == BalanceUnit::Unsupported {
            report.labels.push(format!("Unit unknown for {host}"));
        }
        self.push_label_amounts(body, &mut report);
        Ok(report)
    }
}

#[cfg(test)]
mod tests {
    use super::*;

    fn rules(unit: UnitSpec) -> ExtractRules {
        ExtractRules {
            remaining: "$.data.remaining".to_owned(),
            used: Some("$.data.used".to_owned()),
            total: None,
            scope: BalanceScope::Account,
            unit,
            divisor: None,
            unlimited_when: None,
            envelope: None,
            labels: Vec::new(),
        }
    }

    fn by_host(pairs: &[(&str, UnitName)]) -> UnitSpec {
        UnitSpec::ByHost(ByHost {
            by_host: pairs
                .iter()
                .map(|(suffix, unit)| ((*suffix).to_owned(), *unit))
                .collect(),
        })
    }

    #[test]
    fn host_suffix_needs_a_label_boundary() {
        assert!(host_matches("api.moonshot.cn", "moonshot.cn"));
        assert!(host_matches("moonshot.cn", "moonshot.cn"));
        assert!(host_matches("API.Moonshot.CN.", "moonshot.cn"));
        assert!(!host_matches("evilmoonshot.cn", "moonshot.cn"));
        assert!(!host_matches("moonshot.cn.evil.test", "moonshot.cn"));
    }

    #[test]
    fn host_suffix_edge_cases() {
        // An empty suffix matches only an empty host.
        assert!(host_matches("", ""));
        assert!(host_matches("", "."));
        assert!(host_matches(".", ""));
        assert!(!host_matches("a.com", ""));
        assert!(!host_matches("a.com.", ""));
        // Dots around the suffix are ignored, case is ASCII-folded on both sides.
        assert!(host_matches("api.moonshot.cn", ".moonshot.cn."));
        assert!(host_matches("API.MOONSHOT.CN", "Moonshot.Cn"));
        assert!(host_matches("moonshot.cn.", "moonshot.cn"));
        assert!(!host_matches("cn", "moonshot.cn"));
        // Non-ASCII hosts compare by bytes: no label boundary inside a character, no case folding.
        assert!(host_matches("例子.moonshot.cn", "moonshot.cn"));
        assert!(!host_matches("api.mönshot.cn", "nshot.cn"));
        assert!(!host_matches("aö.cn", "ö.cn"));
        assert!(host_matches("x.ö.cn", "ö.cn"));
        assert!(!host_matches("Ö.cn", "ö.cn"));
    }

    #[test]
    fn by_host_picks_the_longest_suffix_and_unknown_hosts_are_unsupported() {
        let unit = by_host(&[("z.ai", UnitName::Usd), ("api.z.ai", UnitName::Cny)]);
        assert_eq!(unit.resolve("api.z.ai"), BalanceUnit::Cny);
        assert_eq!(unit.resolve("open.z.ai"), BalanceUnit::Usd);
        assert_eq!(
            unit.resolve("moonshot.example.com"),
            BalanceUnit::Unsupported
        );
        assert_eq!(
            by_host(&[]).resolve("api.stepfun.com"),
            BalanceUnit::Unsupported
        );
    }

    #[test]
    fn string_numbers_parse_and_a_missing_remaining_path_fails() {
        let rules = rules(UnitSpec::Fixed(UnitName::Cny));
        let ok = rules
            .evaluate(r#"{"data":{"remaining":"86.40","used":3}}"#, "h", None)
            .unwrap_or_default();
        assert_eq!(ok.remaining, Some(86.4));
        assert_eq!(ok.used, Some(3.0));
        assert_eq!(ok.unit, BalanceUnit::Cny);
        assert!(rules.evaluate(r#"{"data":{}}"#, "h", None).is_err());
        assert!(
            rules
                .evaluate(r#"{"data":{"remaining":"x"}}"#, "h", None)
                .is_err()
        );
    }

    #[test]
    fn unknown_unit_keeps_the_raw_number_and_says_so() {
        let draft = rules(by_host(&[("moonshot.cn", UnitName::Cny)]))
            .evaluate(
                r#"{"data":{"remaining":49.59}}"#,
                "moonshot.example.com",
                None,
            )
            .unwrap_or_default();
        assert_eq!(draft.unit, BalanceUnit::Unsupported);
        assert_eq!(draft.remaining, Some(49.59));
        assert_eq!(draft.remaining_usd, None);
    }

    #[test]
    fn quota_divisors_convert_only_when_read() {
        let mut quota = rules(UnitSpec::Fixed(UnitName::Quota));
        let body = r#"{"data":{"remaining":6200000}}"#;
        quota.divisor = Some(DivisorSpec::Provider(ProviderDivisor::QuotaPerUnit));
        let unread = quota.evaluate(body, "h", None).unwrap_or_default();
        assert_eq!((unread.remaining_usd, unread.quota_per_unit), (None, None));
        let read = quota
            .evaluate(body, "h", Some(500_000.0))
            .unwrap_or_default();
        assert_eq!(read.remaining_usd, Some(12.4));
        assert_eq!(read.quota_per_unit, Some(500_000.0));
        quota.divisor = Some(DivisorSpec::Number(1_000_000.0));
        let fixed = quota
            .evaluate(body, "h", Some(500_000.0))
            .unwrap_or_default();
        assert_eq!(fixed.remaining_usd, Some(6.2));
        quota.divisor = Some(DivisorSpec::Number(0.0));
        assert_eq!(
            quota
                .evaluate(body, "h", None)
                .unwrap_or_default()
                .remaining_usd,
            None
        );
    }

    #[test]
    fn envelope_failure_carries_the_vendor_message() {
        let mut rules = rules(UnitSpec::Fixed(UnitName::Usd));
        rules.envelope = Some(Envelope {
            ok_path: "$.success".to_owned(),
            ok_equals: Value::Bool(true),
            message_path: Some("$.msg".to_owned()),
        });
        let failed = rules.evaluate(r#"{"success":false,"msg":"bad key"}"#, "h", None);
        assert_eq!(failed.err().as_deref(), Some("bad key"));
        let silent = rules.evaluate(r#"{"data":{"remaining":1}}"#, "h", None);
        assert!(silent.is_err());
        let ok = rules.evaluate(r#"{"success":true,"data":{"remaining":1}}"#, "h", None);
        assert!(ok.is_ok());
    }

    #[test]
    fn unlimited_when_wins_and_labels_skip_missing_paths() {
        let mut rules = rules(UnitSpec::Fixed(UnitName::Usd));
        rules.unlimited_when = Some(Condition {
            path: "$.data.plan".to_owned(),
            equals: Value::String("unlimited".to_owned()),
        });
        rules.labels = vec![
            LabelPath {
                name: "voucher".to_owned(),
                path: "$.data.voucher".to_owned(),
            },
            LabelPath {
                name: "gone".to_owned(),
                path: "$.data.gone".to_owned(),
            },
        ];
        let unlimited = rules
            .evaluate(r#"{"data":{"plan":"unlimited"}}"#, "h", None)
            .unwrap_or_default();
        assert_eq!(unlimited.unit, BalanceUnit::Unlimited);
        assert_eq!(unlimited.remaining, None);
        let limited = rules
            .evaluate(
                r#"{"data":{"plan":"x","remaining":2,"voucher":"1.5"}}"#,
                "h",
                None,
            )
            .unwrap_or_default();
        assert_eq!(limited.labels, vec!["voucher 1.5".to_owned()]);
    }
}
