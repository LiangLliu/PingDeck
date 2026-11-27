//! View model state and query/filter helpers for `UsageView`.

use std::cell::Cell;
use std::collections::{HashMap, HashSet};

use gpui_kit::{Entity, Subscription, Task};
use spanbay_desktop::core::app::current_time_ms;
use spanbay_desktop::core::llm::capture::ApiClientDraft;
use spanbay_desktop::core::llm::status::ProviderStatusDto;
use spanbay_desktop::core::llm::store::{
    CaptureStats, GatewayClient, LogFilter, Provider, ProviderKey, ProviderModel, RequestLogEntry,
    StatusClass, UsageFailure,
};
use spanbay_desktop::core::llm::usage::{
    UsageGroup, UsageLatencyMode, UsageQuery, UsageRow, UsageSource, UsageSummary,
};
use spanbay_desktop::core::model::settings::Language;
use spanbay_desktop::state::SettingsSection;
use spanbay_desktop::time::local_tz_offset_minutes;

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
    pub calls_page: usize,
    pub calls_page_size: usize,
    pub calls_total: u64,
    pub calls_loading: bool,
    pub pending_live_logs: u64,
    pub breakdown_page: usize,
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
                    crate::ui::Tr(self.lang)
                        .usage()
                        .usage_state_direct()
                        .to_owned()
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

    /// Builds a server-side [`LogFilter`] matching every active Calls filter, time range,
    /// quick-filter chip, search token, and asynchronous `body:` match set.
    #[must_use]
    pub fn build_calls_log_filter(&self, search_raw: &str) -> LogFilter {
        let mut filter = LogFilter {
            source: self.filter_source.map(|s| s.as_str().to_owned()),
            provider_id: self.scope_provider_id.clone(),
            client_id: self.filter_client_id.clone(),
            key_id: self.filter_key_id.clone(),
            model: self.filter_model.clone(),
            ..LogFilter::default()
        };

        if self.range != RangePreset::All {
            let (from_ms, to_ms, _) = self.range.bounds(current_time_ms());
            filter.since_ms = Some(from_ms);
            filter.until_ms = Some(to_ms);
        }

        match self.calls_quick {
            CallsQuickFilter::All => {}
            CallsQuickFilter::FailedOnly => filter.failed_only = true,
            CallsQuickFilter::FailedOver => filter.status_class = Some(StatusClass::FailedOver),
            CallsQuickFilter::WithTools => filter.with_tools = true,
        }

        if let Some(ids) = &self.body_match_ids {
            let mut list: Vec<i64> = ids.iter().copied().collect();
            list.sort_unstable();
            filter.ids = Some(list);
        }

        for raw_tok in search_raw.split_whitespace() {
            let tok = raw_tok.to_ascii_lowercase();
            if tok == "is:failed" {
                filter.failed_only = true;
            } else if tok == "is:failover" {
                filter.status_class = Some(StatusClass::FailedOver);
            } else if let Some(v) = tok.strip_prefix("source:") {
                if !v.is_empty() {
                    if let Some(existing) = &filter.source
                        && !existing.eq_ignore_ascii_case(v)
                    {
                        filter.ids = Some(Vec::new());
                    } else {
                        filter.source = Some(v.to_owned());
                    }
                }
            } else if let Some(v) = tok.strip_prefix("model:") {
                if !v.is_empty() {
                    filter.model_contains = Some(v.to_owned());
                }
            } else if let Some(v) = tok.strip_prefix("run:") {
                if !v.is_empty() {
                    filter.run_id_contains = Some(v.to_owned());
                }
            } else if tok.starts_with("body:") {
                // Handled asynchronously via `body_match_ids` -> `filter.ids`.
            } else {
                filter.search_terms.push(tok);
            }
        }

        filter
    }
}
