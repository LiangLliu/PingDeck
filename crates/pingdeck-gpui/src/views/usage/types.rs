//! Query presets, enums, and formatting helpers for `UsageView`.

use std::collections::{BTreeMap, HashMap};

use pingdeck_desktop::core::llm::store::{RequestLogEntry, UsageLatency};
use pingdeck_desktop::core::llm::usage::UsageGroup;
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
    pub const fn short_label(self, zh: bool) -> &'static str {
        match self {
            Self::Today => {
                if zh {
                    "今天"
                } else {
                    "Today"
                }
            }
            Self::ThisMonth => {
                if zh {
                    "本月"
                } else {
                    "This month"
                }
            }
            Self::LastMonth => {
                if zh {
                    "上月"
                } else {
                    "Last month"
                }
            }
            Self::Hour1 => "1h",
            Self::Hour24 => "24h",
            Self::Day7 => "7d",
            Self::Day30 => "30d",
            Self::Day90 => "90d",
            Self::All => {
                if zh {
                    "全部"
                } else {
                    "All time"
                }
            }
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
    pub const fn label(self, zh: bool) -> &'static str {
        match self {
            Self::Calls => {
                if zh {
                    "调用"
                } else {
                    "Calls"
                }
            }
            Self::Tokens => "Token",
            Self::Cost => {
                if zh {
                    "费用"
                } else {
                    "Cost"
                }
            }
            Self::FailRate => {
                if zh {
                    "失败率"
                } else {
                    "Fail %"
                }
            }
            Self::Latency => {
                if zh {
                    "延迟"
                } else {
                    "Latency"
                }
            }
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
    pub const fn label(self, zh: bool) -> &'static str {
        match self {
            Self::Model => {
                if zh {
                    "模型"
                } else {
                    "Model"
                }
            }
            Self::Provider => {
                if zh {
                    "服务商"
                } else {
                    "Provider"
                }
            }
            Self::Source => {
                if zh {
                    "来源"
                } else {
                    "Source"
                }
            }
            Self::Client => {
                if zh {
                    "客户端"
                } else {
                    "Client"
                }
            }
            Self::Key => {
                if zh {
                    "密钥"
                } else {
                    "Key"
                }
            }
            Self::Failure => {
                if zh {
                    "失败原因"
                } else {
                    "Failures"
                }
            }
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
    pub const fn label(self, zh: bool) -> &'static str {
        match self {
            Self::All => {
                if zh {
                    "全部"
                } else {
                    "All"
                }
            }
            Self::FailedOnly => {
                if zh {
                    "仅失败"
                } else {
                    "Failed"
                }
            }
            Self::FailedOver => {
                if zh {
                    "故障转移"
                } else {
                    "Failover"
                }
            }
            Self::WithTools => {
                if zh {
                    "含工具调用"
                } else {
                    "Tool calls"
                }
            }
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
pub fn source_label(src: &str, zh: bool) -> &'static str {
    match src {
        "gateway" => {
            if zh {
                "网关"
            } else {
                "Gateway"
            }
        }
        "agent" => {
            if zh {
                "对话"
            } else {
                "Chat"
            }
        }
        "api" => {
            if zh {
                "AI 请求"
            } else {
                "AI Request"
            }
        }
        "probe" => {
            if zh {
                "探针"
            } else {
                "Probe"
            }
        }
        _ => {
            if zh {
                "其他"
            } else {
                "Other"
            }
        }
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
