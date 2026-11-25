//! View model state and query/filter helpers for `UsageView`.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use gpui_kit::{Entity, Subscription, Task};
use pingdeck_desktop::core::app::current_time_ms;
use pingdeck_desktop::core::llm::capture::ApiClientDraft;
use pingdeck_desktop::core::llm::status::ProviderStatusDto;
use pingdeck_desktop::core::llm::store::{
    CaptureStats, GatewayClient, Provider, ProviderKey, ProviderModel, RequestLogEntry,
    UsageFailure,
};
use pingdeck_desktop::core::llm::usage::{
    UsageGroup, UsageLatencyMode, UsageQuery, UsageRow, UsageSource, UsageSummary,
};
use pingdeck_desktop::core::model::settings::Language;
use pingdeck_desktop::state::SettingsSection;
use pingdeck_desktop::time::local_tz_offset_minutes;

use crate::ui::{InputState, Tone};

pub use super::types::{
    BreakdownDim, CallsQuickFilter, ChartMetric, RangePreset, UsageViewTab, fmt_compact,
    fmt_cost_map, fmt_cost_micro, fmt_latency, is_failed_over, latency_totals, source_label,
};

#[derive(Debug, Clone)]
pub enum UsageModal {
    ClearLogs { count: u64 },
    ClearCaptures { stats: CaptureStats },
}

#[derive(Debug, Clone)]
pub enum UsageHandoff {
    OpenSettings(SettingsSection),
    OpenAiRequest(Box<ApiClientDraft>),
    AskAgent {
        title: String,
        context_markdown: String,
    },
}

pub struct UsageView {
    pub lang: Language,
    pub active_tab: UsageViewTab,
    pub range: RangePreset,
    pub scope_provider_id: Option<String>,
    pub filter_source: Option<UsageSource>,
    pub filter_model: Option<String>,
    pub filter_key_id: Option<String>,
    pub filter_client_id: Option<String>,
    pub chart_metric: ChartMetric,
    pub selected_currency: Option<String>,
    pub breakdown_dim: BreakdownDim,
    pub calls_quick: CallsQuickFilter,
    pub calls_search_input: Entity<InputState>,
    pub body_match_ids: Option<HashSet<i64>>,
    pub body_searching: bool,

    // Loaded catalogs & analytics.
    pub providers: Vec<Provider>,
    pub provider_statuses: HashMap<String, ProviderStatusDto>,
    pub provider_totals: HashMap<String, UsageRow>,
    pub keys: Vec<ProviderKey>,
    pub models: Vec<ProviderModel>,
    pub clients: Vec<GatewayClient>,
    pub summary: Option<UsageSummary>,
    pub latency_modes: Vec<UsageLatencyMode>,
    pub latency_by_day: HashMap<String, (u64, u64)>,
    pub latency_by_model: HashMap<(String, String), (u64, u64)>,
    pub failures: Vec<UsageFailure>,
    pub logs: Vec<RequestLogEntry>,
    pub logs_has_more: bool,
    pub capture_stats: Option<CaptureStats>,
    pub retention_days: u16,
    pub updated_at_ms: i64,

    pub active_modal: Option<UsageModal>,
    pub status_msg: Option<(Tone, String)>,
    pub pending_open_call_id: Option<i64>,
    pub pending_handoff: Cell<Option<UsageHandoff>>,
    pub llm_sub_id: Option<u64>,
    pub llm_stream_task: Option<Task<()>>,
    pub _subscriptions: Vec<Subscription>,
    pub _tasks: Vec<Task<()>>,
}

impl UsageView {
    #[must_use]
    pub fn is_zh(&self) -> bool {
        self.lang == Language::ZhCn
    }

    #[must_use]
    pub fn t<'a>(&self, zh_s: &'a str, en_s: &'a str) -> &'a str {
        if self.is_zh() { zh_s } else { en_s }
    }

    #[must_use]
    pub fn build_usage_query(&self, now_ms: i64, group_by: Vec<UsageGroup>) -> UsageQuery {
        let (from_ms, to_ms, bucket_minutes) = self.range.bounds(now_ms);
        let sources = self.filter_source.map_or_else(Vec::new, |s| vec![s]);
        UsageQuery {
            provider_id: self.scope_provider_id.clone(),
            key_id: self.filter_key_id.clone(),
            client_id: self.filter_client_id.clone(),
            from_ms,
            to_ms,
            group_by,
            sources,
            tz_offset_minutes: local_tz_offset_minutes(),
            bucket_minutes,
        }
    }

    #[must_use]
    pub fn provider_name(&self, id: &str) -> String {
        self.providers
            .iter()
            .find(|p| p.id == id)
            .map(|p| p.name.clone())
            .unwrap_or_else(|| id.to_owned())
    }

    #[must_use]
    pub fn client_name(&self, id: &str) -> String {
        self.clients
            .iter()
            .find(|c| c.id == id)
            .map(|c| c.name.clone())
            .unwrap_or_else(|| {
                if id.is_empty() {
                    self.t("非网关调用", "Direct").to_owned()
                } else {
                    id.to_owned()
                }
            })
    }

    #[must_use]
    pub fn key_name(&self, id: &str) -> String {
        self.keys
            .iter()
            .find(|k| k.id == id)
            .map(|k| k.name.clone())
            .unwrap_or_else(|| id.to_owned())
    }

    #[must_use]
    pub fn active_currency(&self) -> String {
        if let Some(c) = &self.selected_currency {
            return c.clone();
        }
        self.summary
            .as_ref()
            .and_then(|s| s.currencies.first().cloned())
            .unwrap_or_else(|| "USD".to_owned())
    }

    #[must_use]
    pub fn filtered_logs(&self, search_raw: &str) -> Vec<&RequestLogEntry> {
        let tokens: Vec<String> = search_raw
            .split_whitespace()
            .map(|t| t.to_ascii_lowercase())
            .collect();
        let (from_ms, to_ms, _) = self.range.bounds(current_time_ms());

        self.logs
            .iter()
            .filter(|row| {
                if self.range != RangePreset::All && (row.ts_ms < from_ms || row.ts_ms > to_ms) {
                    return false;
                }
                if let Some(pid) = &self.scope_provider_id
                    && row.provider_id.as_deref() != Some(pid.as_str())
                {
                    return false;
                }
                if let Some(src) = self.filter_source
                    && row.source != src.as_str()
                {
                    return false;
                }
                if let Some(model) = &self.filter_model
                    && row.requested_model != *model
                    && row.upstream_model.as_deref() != Some(model.as_str())
                {
                    return false;
                }
                if let Some(kid) = &self.filter_key_id
                    && row.key_id.as_deref() != Some(kid.as_str())
                {
                    return false;
                }
                if let Some(cid) = &self.filter_client_id
                    && row.client_id.as_deref() != Some(cid.as_str())
                {
                    return false;
                }
                match self.calls_quick {
                    CallsQuickFilter::All => {}
                    CallsQuickFilter::FailedOnly => {
                        if row.outcome != "failure" && row.status < 400 {
                            return false;
                        }
                    }
                    CallsQuickFilter::FailedOver => {
                        if !is_failed_over(row) {
                            return false;
                        }
                    }
                    CallsQuickFilter::WithTools => {
                        if row.finish_reason.as_deref() != Some("tool_calls")
                            && row.purpose.as_deref() != Some("tool")
                        {
                            return false;
                        }
                    }
                }
                if let Some(ids) = &self.body_match_ids
                    && !ids.contains(&row.id)
                {
                    return false;
                }
                for tok in &tokens {
                    if tok == "is:failed" {
                        if row.outcome != "failure" && row.status < 400 {
                            return false;
                        }
                    } else if tok == "is:failover" {
                        if !is_failed_over(row) {
                            return false;
                        }
                    } else if let Some(v) = tok.strip_prefix("source:") {
                        if !row.source.eq_ignore_ascii_case(v) {
                            return false;
                        }
                    } else if let Some(v) = tok.strip_prefix("model:") {
                        let m1 = row.requested_model.to_ascii_lowercase();
                        let m2 = row
                            .upstream_model
                            .as_deref()
                            .unwrap_or("")
                            .to_ascii_lowercase();
                        if !m1.contains(v) && !m2.contains(v) {
                            return false;
                        }
                    } else if let Some(v) = tok.strip_prefix("run:") {
                        if !row
                            .run_id
                            .as_deref()
                            .unwrap_or("")
                            .to_ascii_lowercase()
                            .contains(v)
                        {
                            return false;
                        }
                    } else if tok.starts_with("body:") {
                        // Handled asynchronously via `body_match_ids`.
                    } else {
                        let hay = format!(
                            "{} {} {} {} {} {} {}",
                            row.id,
                            row.requested_model,
                            row.upstream_model.as_deref().unwrap_or(""),
                            row.provider_name.as_deref().unwrap_or(""),
                            row.client_name.as_deref().unwrap_or(""),
                            row.source,
                            row.error.as_deref().unwrap_or(""),
                        )
                        .to_ascii_lowercase();
                        if !hay.contains(tok) {
                            return false;
                        }
                    }
                }
                true
            })
            .collect()
    }
}
