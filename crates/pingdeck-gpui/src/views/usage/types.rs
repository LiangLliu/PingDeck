//! Query presets, enums, and formatting helpers for `UsageView`.

use std::collections::{BTreeMap, HashMap};

use pingdeck_desktop::core::llm::store::{RequestLogEntry, UsageLatency};
use pingdeck_desktop::core::llm::usage::UsageGroup;
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::time::{
    local_day_start_ms, local_month_bounds_ms, local_prev_month_bounds_ms,
};

const MS_HOUR: i64 = 3_600_000;
const MS_DAY: i64 = 86_400_000;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum UsageViewTab {
    #[default]
    Overview,
    Calls,
}

impl UsageViewTab {
    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Overview => 0,
            Self::Calls => 1,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Calls,
            _ => Self::Overview,
        }
    }
}

/// All 9 prototype time-range presets (`data-overlays.js:55-57`).
#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum RangePreset {
    Today,
    ThisMonth,
    LastMonth,
    Hour1,
    Hour24,
    #[default]
    Day7,
    Day30,
    Day90,
    All,
}

impl RangePreset {
    pub const ALL: [Self; 9] = [
        Self::Today,
        Self::ThisMonth,
        Self::LastMonth,
        Self::Hour1,
        Self::Hour24,
        Self::Day7,
        Self::Day30,
        Self::Day90,
        Self::All,
    ];

    #[must_use]
    pub const fn short_label(self, lang: Language) -> &'static str {
        let u = crate::ui::Tr(lang).usage();
        match self {
            Self::Today => u.usage_range_today(),
            Self::ThisMonth => u.usage_range_month(),
            Self::LastMonth => u.usage_range_last_month(),
            Self::Hour1 => "1h",
            Self::Hour24 => "24h",
            Self::Day7 => "7d",
            Self::Day30 => "30d",
            Self::Day90 => "90d",
            Self::All => u.usage_types_all_time(),
        }
    }

    /// Returns `(from_ms, to_ms, bucket_minutes)` in local time.
    #[must_use]
    pub fn bounds(self, now_ms: i64) -> (i64, i64, Option<u32>) {
        match self {
            Self::Today => (local_day_start_ms(now_ms), now_ms, Some(60)),
            Self::ThisMonth => {
                let (s, e) = local_month_bounds_ms(now_ms);
                (s, e, None)
            }
            Self::LastMonth => {
                let (s, e) = local_prev_month_bounds_ms(now_ms);
                (s, e, None)
            }
            Self::Hour1 => (now_ms - MS_HOUR, now_ms, Some(5)),
            Self::Hour24 => (now_ms - MS_DAY, now_ms, Some(60)),
            Self::Day7 => (now_ms - 7 * MS_DAY, now_ms, None),
            Self::Day30 => (now_ms - 30 * MS_DAY, now_ms, None),
            Self::Day90 => (now_ms - 90 * MS_DAY, now_ms, None),
            Self::All => (0, now_ms, None),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum ChartMetric {
    #[default]
    Calls,
    Tokens,
    Cost,
    FailRate,
    Latency,
}

impl ChartMetric {
    pub const ALL: [Self; 5] = [
        Self::Calls,
        Self::Tokens,
        Self::Cost,
        Self::FailRate,
        Self::Latency,
    ];

    #[must_use]
    pub const fn label(self, lang: Language) -> &'static str {
        let u = crate::ui::Tr(lang).usage();
        match self {
            Self::Calls => u.usage_kpi_calls(),
            Self::Tokens => "Token",
            Self::Cost => u.usage_kpi_cost(),
            Self::FailRate => u.usage_types_fail(),
            Self::Latency => u.usage_kpi_latency(),
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum BreakdownDim {
    #[default]
    Model,
    Provider,
    Source,
    Client,
    Key,
    Failure,
}

impl BreakdownDim {
    pub const ALL: [Self; 6] = [
        Self::Model,
        Self::Provider,
        Self::Source,
        Self::Client,
        Self::Key,
        Self::Failure,
    ];

    #[must_use]
    pub const fn index(self) -> usize {
        match self {
            Self::Model => 0,
            Self::Provider => 1,
            Self::Source => 2,
            Self::Client => 3,
            Self::Key => 4,
            Self::Failure => 5,
        }
    }

    #[must_use]
    pub const fn from_index(idx: usize) -> Self {
        match idx {
            1 => Self::Provider,
            2 => Self::Source,
            3 => Self::Client,
            4 => Self::Key,
            5 => Self::Failure,
            _ => Self::Model,
        }
    }

    #[must_use]
    pub const fn label(self, lang: Language) -> &'static str {
        let u = crate::ui::Tr(lang).usage();
        match self {
            Self::Model => u.usage_dim_model(),
            Self::Provider => u.usage_dim_provider(),
            Self::Source => u.usage_dim_source(),
            Self::Client => u.usage_dim_client(),
            Self::Key => u.usage_dim_key(),
            Self::Failure => u.usage_types_failures(),
        }
    }

    #[must_use]
    pub const fn usage_group(self) -> Option<UsageGroup> {
        match self {
            Self::Model => Some(UsageGroup::Model),
            Self::Provider => Some(UsageGroup::Provider),
            Self::Source => Some(UsageGroup::Source),
            Self::Client => Some(UsageGroup::Client),
            Self::Key => Some(UsageGroup::Key),
            Self::Failure => None,
        }
    }
}

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum CallsQuickFilter {
    #[default]
    All,
    FailedOnly,
    FailedOver,
    WithTools,
}

impl CallsQuickFilter {
    pub const ALL: [Self; 4] = [
        Self::All,
        Self::FailedOnly,
        Self::FailedOver,
        Self::WithTools,
    ];

    #[must_use]
    pub const fn label(self, lang: Language) -> &'static str {
        let u = crate::ui::Tr(lang).usage();
        match self {
            Self::All => u.usage_filter_all(),
            Self::FailedOnly => u.usage_types_failed(),
            Self::FailedOver => u.usage_calls_failover(),
            Self::WithTools => u.usage_types_tool_calls(),
        }
    }
}

#[must_use]
pub fn is_failed_over(row: &RequestLogEntry) -> bool {
    row.attempts.as_array().is_some_and(|a| a.len() > 1)
}

#[must_use]
pub fn fmt_compact(n: u64) -> String {
    if n >= 1_000_000 {
        format!("{:.2}M", n as f64 / 1_000_000.0)
    } else if n >= 10_000 {
        format!("{:.1}K", n as f64 / 1_000.0)
    } else if n >= 1_000 {
        let thousands = n / 1_000;
        let rem = n % 1_000;
        format!("{thousands},{rem:03}")
    } else {
        n.to_string()
    }
}

#[must_use]
pub fn currency_symbol(currency: &str) -> &str {
    match currency.to_ascii_uppercase().as_str() {
        "CNY" | "RMB" | "JPY" => "¥",
        "EUR" => "€",
        "GBP" => "£",
        _ => "$",
    }
}

#[must_use]
pub fn fmt_cost_micro(micro: i64, currency: &str) -> String {
    let val = micro as f64 / 1_000_000.0;
    let sym = currency_symbol(currency);
    if val.abs() < 0.01 && micro != 0 {
        format!("{sym}{val:.4}")
    } else {
        format!("{sym}{val:.2}")
    }
}

#[must_use]
pub fn fmt_cost_map(costs: &BTreeMap<String, i64>) -> String {
    if costs.is_empty() {
        return "—".to_owned();
    }
    costs
        .iter()
        .map(|(cur, &m)| format!("~{}", fmt_cost_micro(m, cur)))
        .collect::<Vec<_>>()
        .join(" + ")
}

#[must_use]
pub fn fmt_latency(ms: u64) -> String {
    if ms >= 1000 {
        format!("{:.2} s", ms as f64 / 1000.0)
    } else {
        format!("{ms} ms")
    }
}

#[must_use]
pub fn source_label(src: &str, lang: Language) -> &'static str {
    let u = crate::ui::Tr(lang).usage();
    match src {
        "gateway" => u.usage_vocab_source_gateway(),
        "agent" => u.usage_vocab_source_agent(),
        "api" => u.usage_types_ai_request(),
        "probe" => u.usage_types_probe(),
        _ => u.usage_types_other(),
    }
}

pub fn latency_totals(latencies: &[UsageLatency]) -> HashMap<String, (u64, u64)> {
    let mut map: HashMap<String, (u64, u64)> = HashMap::new();
    for row in latencies {
        if let Some(k) = row.key.first() {
            let slot = map.entry(k.clone()).or_insert((0, 0));
            slot.0 = slot.0.saturating_add(row.latency_ms);
            slot.1 = slot.1.saturating_add(row.timed_requests);
        }
    }
    map
}
