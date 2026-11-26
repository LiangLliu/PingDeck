//! Core data loaders, live LlmEvent subscription, body search, CSV export, and drill-down actions for `UsageView`.

use std::collections::{HashMap, HashSet};

use gpui_kit::{AppContext as _, Context, ParentElement as _, Styled as _, Window, px};
use pingdeck_desktop::core::app::{
    current_time_ms, llm_gateway_client_list_impl, llm_key_list_impl, llm_log_capture_stats_impl,
    llm_log_clear_impl, llm_log_count_impl, llm_log_delete_captures_impl, llm_log_get_impl,
    llm_log_page_impl, llm_log_search_bodies_impl, llm_model_list_impl, llm_provider_list_impl,
    llm_provider_status_list_impl, llm_subscribe_impl, llm_unsubscribe_impl,
    llm_usage_failures_impl, llm_usage_latency_impl, llm_usage_latency_modes_impl,
    llm_usage_set_tz_offset_impl, llm_usage_summary_impl, settings_get_impl,
};
use pingdeck_desktop::core::error::AppError;
use pingdeck_desktop::core::llm::events::LlmEvent;
use pingdeck_desktop::core::llm::store::LogFilter;
use pingdeck_desktop::core::llm::usage::{UsageGroup, UsageRow, UsageSource};
use pingdeck_desktop::core::model::events::Sequenced;
use pingdeck_desktop::services::{FileFilter, HostDialog as _, RfdDialog, SaveFileSpec};
use pingdeck_desktop::streams::channel_sink;
use pingdeck_desktop::time::{format_local_datetime, local_tz_offset_minutes};

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into, subscribe_stream_into};
use crate::ui::{Tone, WindowExt as _};

use super::call_drawer::{CallDetailHost, CallDetailSheet};
use super::state::{
    BreakdownDim, UsageModal, UsageView, UsageViewTab, fmt_cost_micro, latency_totals,
};

impl UsageView {
    pub fn init_tz_and_subscribe(&mut self, cx: &mut Context<Self>) {
        let tz = local_tz_offset_minutes();
        let _ = llm_usage_set_tz_offset_impl(runtime(cx).state(), tz);

        if self.llm_stream_task.is_some() {
            return;
        }
        let (sink, rx) = channel_sink::<Sequenced<LlmEvent>>();
        if let Ok(sub_id) = llm_subscribe_impl(runtime(cx).state(), sink, None) {
            self.llm_sub_id = Some(sub_id);
            self.llm_stream_task = Some(subscribe_stream_into(cx, rx, 24, |this, batch, cx| {
                let mut need_summary_refresh = false;
                let mut incoming_logs = 0u64;
                for seq in batch {
                    match seq.event {
                        LlmEvent::GatewayLogBatch { entries } => {
                            incoming_logs = incoming_logs.saturating_add(entries.len() as u64);
                            need_summary_refresh = true;
                        }
                        LlmEvent::ProvidersChanged
                        | LlmEvent::KeysChanged { .. }
                        | LlmEvent::ModelsChanged { .. }
                        | LlmEvent::ClientsChanged
                        | LlmEvent::CatalogChanged
                        | LlmEvent::ProviderStatusChanged { .. }
                        | LlmEvent::Gap { .. } => {
                            need_summary_refresh = true;
                        }
                        _ => {}
                    }
                }
                if incoming_logs > 0 {
                    this.updated_at_ms = current_time_ms();
                    if this.calls_page == 0 && !this.body_searching {
                        this.reload_logs_page(false, cx);
                    } else {
                        this.pending_live_logs =
                            this.pending_live_logs.saturating_add(incoming_logs);
                    }
                }
                if need_summary_refresh {
                    this.reload_analytics_only(cx);
                }
                cx.notify();
            }));
        }
    }

    pub fn shutdown_sub(&mut self, cx: &Context<Self>) {
        if let Some(sub) = self.llm_sub_id.take() {
            llm_unsubscribe_impl(runtime(cx).state(), sub);
        }
        self.llm_stream_task = None;
    }

    pub fn reload_all(&mut self, cx: &mut Context<Self>) {
        let settings = settings_get_impl(runtime(cx).state());
        self.lang = settings.general.language;
        self.retention_days = settings.ai.providers.usage_retention_days;
        self.breakdown_page = 0;
        let _ = llm_usage_set_tz_offset_impl(runtime(cx).state(), local_tz_offset_minutes());

        let t_cat = spawn_blocking_into(
            cx,
            move |state| {
                let providers = llm_provider_list_impl(state).unwrap_or_default();
                let keys = llm_key_list_impl(state, None).unwrap_or_default();
                let models = llm_model_list_impl(state, None).unwrap_or_default();
                let clients = llm_gateway_client_list_impl(state).unwrap_or_default();
                Ok::<_, AppError>((providers, keys, models, clients))
            },
            |this, res, cx| {
                if let Ok((providers, keys, models, clients)) = res {
                    this.providers = providers;
                    this.keys = keys;
                    this.models = models;
                    this.clients = clients;
                    this.updated_at_ms = current_time_ms();
                }
                cx.notify();
            },
        );
        self._tasks.push(t_cat);
        self.reload_logs_page(true, cx);
        self.reload_analytics_only(cx);
    }

    pub fn reload_analytics_only(&mut self, cx: &mut Context<Self>) {
        let now = current_time_ms();
        let group_by = self
            .breakdown_dim
            .usage_group()
            .into_iter()
            .collect::<Vec<_>>();
        let q_main = self.build_usage_query(now, group_by);
        let q_lat_day = self.build_usage_query(now, vec![UsageGroup::Day]);
        let q_lat_mod = self.build_usage_query(now, Vec::new());
        let mut q_scope = self.build_usage_query(now, vec![UsageGroup::Provider]);
        q_scope.provider_id = None;
        let mut q_fail = self.build_usage_query(now, Vec::new());
        if q_fail.bucket_minutes.is_none() {
            q_fail.bucket_minutes = Some(60);
        }

        let t = spawn_async_into(
            cx,
            move |state| async move {
                let summary = llm_usage_summary_impl(&state, q_main).await?;
                let scope_summary = llm_usage_summary_impl(&state, q_scope).await.ok();
                let statuses = llm_provider_status_list_impl(&state)
                    .await
                    .unwrap_or_default();
                let lat_modes = llm_usage_latency_modes_impl(&state, q_lat_mod.clone())
                    .await
                    .unwrap_or_default();
                let lat_days = llm_usage_latency_impl(&state, q_lat_day)
                    .await
                    .unwrap_or_default();
                let lat_models = llm_usage_latency_impl(&state, q_lat_mod)
                    .await
                    .unwrap_or_default();
                let failures = llm_usage_failures_impl(&state, q_fail)
                    .await
                    .unwrap_or_default();
                let capture_stats = llm_log_capture_stats_impl(&state).await.ok();
                Ok::<_, AppError>((
                    summary,
                    scope_summary,
                    statuses,
                    lat_modes,
                    lat_days,
                    lat_models,
                    failures,
                    capture_stats,
                ))
            },
            |this, res, cx| {
                match res {
                    Ok((
                        summary,
                        scope_summary,
                        statuses,
                        lat_modes,
                        lat_days,
                        lat_models,
                        failures,
                        capture_stats,
                    )) => {
                        this.provider_statuses = statuses
                            .into_iter()
                            .map(|s| (s.provider_id.clone(), s))
                            .collect();
                        if let Some(sc) = scope_summary {
                            this.provider_totals = sc
                                .groups
                                .into_iter()
                                .filter_map(|b| b.key.into_iter().next().map(|k| (k, b.usage)))
                                .collect();
                        }
                        if this
                            .selected_currency
                            .as_ref()
                            .is_some_and(|c| !summary.currencies.contains(c))
                        {
                            this.selected_currency = None;
                        }
                        this.summary = Some(summary);
                        this.latency_modes = lat_modes;
                        this.latency_by_day = latency_totals(&lat_days);
                        let mut m_map: HashMap<(String, String), (u64, u64)> = HashMap::new();
                        for row in lat_models {
                            let slot = m_map.entry((row.provider_id, row.model)).or_insert((0, 0));
                            slot.0 = slot.0.saturating_add(row.latency_ms);
                            slot.1 = slot.1.saturating_add(row.timed_requests);
                        }
                        this.latency_by_model = m_map;
                        this.failures = failures;
                        this.capture_stats = capture_stats;
                        this.updated_at_ms = current_time_ms();
                    }
                    Err(e) => {
                        this.status_msg = Some((Tone::Danger, e.message().to_owned()));
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(t);
    }

    pub fn reload_logs_page(&mut self, reset_page: bool, cx: &mut Context<Self>) {
        if reset_page {
            self.calls_page = 0;
        }
        if self.calls_page == 0 {
            self.pending_live_logs = 0;
        }
        let search_raw = self.calls_search_input.read(cx).value().to_string();
        let filter = self.build_calls_log_filter(&search_raw);
        let page_size = self.calls_page_size.clamp(10, 500);
        let req_page = self.calls_page;
        let offset = (req_page as u64).saturating_mul(page_size as u64);
        self.calls_loading = true;
        cx.notify();

        let task = spawn_blocking_into(
            cx,
            move |state| llm_log_page_impl(state, Some(filter), offset, Some(page_size as u32)),
            move |this, res, cx| {
                this.calls_loading = false;
                match res {
                    Ok(page) => {
                        this.calls_total = page.total;
                        let max_page = (page.total as usize)
                            .div_ceil(this.calls_page_size.max(1))
                            .saturating_sub(1);
                        if page.entries.is_empty() && page.total > 0 && req_page > max_page {
                            this.calls_page = max_page;
                            this.reload_logs_page(false, cx);
                            return;
                        }
                        this.logs = page.entries;
                        this.updated_at_ms = current_time_ms();
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn set_calls_page(&mut self, page: usize, cx: &mut Context<Self>) {
        let total_pages = (self.calls_total as usize)
            .div_ceil(self.calls_page_size.max(1))
            .max(1);
        let next = page.min(total_pages - 1);
        if next == self.calls_page && self.pending_live_logs == 0 {
            return;
        }
        self.calls_page = next;
        if next == 0 {
            self.pending_live_logs = 0;
        }
        self.reload_logs_page(false, cx);
    }

    pub fn set_calls_page_size(&mut self, size: usize, cx: &mut Context<Self>) {
        let clamped = size.clamp(10, 500);
        if clamped == self.calls_page_size {
            return;
        }
        self.calls_page_size = clamped;
        self.reload_logs_page(true, cx);
    }

    pub fn handle_calls_search_changed(&mut self, cx: &mut Context<Self>) {
        let raw = self.calls_search_input.read(cx).value().to_string();
        let body_term = raw
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix("body:"))
            .map(str::to_owned);
        match body_term {
            Some(term) if !term.is_empty() => {
                self.body_searching = true;
                self.calls_page = 0;
                cx.notify();
                let task = spawn_async_into(
                    cx,
                    move |state| async move { llm_log_search_bodies_impl(&state, &term).await },
                    |this, res, cx| {
                        this.body_searching = false;
                        if let Ok(ids) = res {
                            this.body_match_ids = Some(ids.into_iter().collect::<HashSet<_>>());
                        }
                        this.reload_logs_page(true, cx);
                    },
                );
                self._tasks.push(task);
            }
            _ => {
                self.body_match_ids = None;
                self.body_searching = false;
                self.reload_logs_page(true, cx);
            }
        }
    }

    pub fn drill_down(
        &mut self,
        dim: BreakdownDim,
        key: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match dim {
            BreakdownDim::Model => {
                self.filter_model = Some(key);
            }
            BreakdownDim::Provider => {
                self.scope_provider_id = Some(key);
            }
            BreakdownDim::Source => {
                self.filter_source = UsageSource::parse(&key);
            }
            BreakdownDim::Client => {
                if !key.is_empty() {
                    self.filter_client_id = Some(key);
                }
            }
            BreakdownDim::Key => {
                if !key.is_empty() {
                    self.filter_key_id = Some(key);
                }
            }
            BreakdownDim::Failure => {
                self.calls_quick = super::state::CallsQuickFilter::FailedOnly;
                self.calls_search_input
                    .update(cx, |s, cx| s.set_value(key, window, cx));
            }
        }
        self.active_tab = UsageViewTab::Calls;
        self.reload_all(cx);
    }

    pub fn clear_filters(&mut self, window: &mut Window, cx: &mut Context<Self>) {
        self.filter_source = None;
        self.filter_model = None;
        self.filter_key_id = None;
        self.filter_client_id = None;
        self.calls_quick = super::state::CallsQuickFilter::All;
        self.body_match_ids = None;
        self.calls_search_input
            .update(cx, |s, cx| s.set_value("", window, cx));
        self.reload_all(cx);
    }

    pub fn open_clear_logs_modal(&mut self, cx: &mut Context<Self>) {
        let filter = self.active_log_filter(cx);
        let task = spawn_blocking_into(
            cx,
            move |state| llm_log_count_impl(state, Some(filter)),
            |this, res, cx| {
                match res {
                    Ok(count) => {
                        this.active_modal = Some(UsageModal::ClearLogs { count });
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn confirm_clear_logs(&mut self, cx: &mut Context<Self>) {
        let filter = self.active_log_filter(cx);
        let task = spawn_async_into(
            cx,
            move |state| async move { llm_log_clear_impl(&state, Some(filter)).await },
            |this, res, cx| {
                this.active_modal = None;
                match res {
                    Ok(rep) => {
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::Tr(this.lang)
                                .usage()
                                .usage_actions_cleared_call_log_s(rep.rows),
                        ));
                        this.reload_all(cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn confirm_clear_captures(&mut self, cx: &mut Context<Self>) {
        let task = spawn_async_into(
            cx,
            |state| async move { llm_log_delete_captures_impl(&state).await },
            |this, res, cx| {
                this.active_modal = None;
                match res {
                    Ok(bytes) => {
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::Tr(this.lang)
                                .usage()
                                .usage_actions_deleted_stored_captures_kb_freed(bytes / 1024),
                        ));
                        this.reload_all(cx);
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn open_call_drawer(&mut self, row_id: i64, window: &mut Window, cx: &mut Context<Self>) {
        let res = llm_log_get_impl(runtime(cx).state(), row_id);
        match res {
            Ok(detail) => {
                let parent = cx.entity().clone();
                let sheet_entity = cx.new(|cx| {
                    CallDetailSheet::new(CallDetailHost::Usage(parent), detail, window, cx)
                });
                window.open_sheet(cx, move |sheet, w, _cx| {
                    let sheet_w = crate::ui::responsive_sheet_width(
                        640.0,
                        f32::from(w.viewport_size().width),
                    );
                    sheet.size(px(sheet_w)).p_0().child(sheet_entity.clone())
                });
            }
            Err(e) => {
                self.status_msg = Some((Tone::Danger, e.message().to_owned()));
                cx.notify();
            }
        }
    }

    pub fn export_csv(&mut self, cx: &mut Context<Self>) {
        let Some(path) = RfdDialog.save_file(&SaveFileSpec {
            title: None,
            file_name: Some("pingdeck-usage.csv".to_owned()),
            filters: vec![FileFilter {
                name: "CSV".to_owned(),
                extensions: vec!["csv".to_owned()],
            }],
            starting_dir: None,
        }) else {
            return;
        };

        let filter = self.active_log_filter(cx);
        let path_for_msg = path.clone();
        let task = spawn_blocking_into(
            cx,
            move |state| {
                const MAX_EXPORT: u64 = 5_000;
                const CHUNK: u32 = 500;
                let mut csv = String::from(
                    "id,time,source,client,provider,requested_model,upstream_model,status,outcome,ttft_ms,latency_ms,input_tokens,output_tokens,cached_tokens,cost,currency\n",
                );
                let mut offset = 0u64;
                while offset < MAX_EXPORT {
                    let page = llm_log_page_impl(state, Some(filter.clone()), offset, Some(CHUNK))?;
                    if page.entries.is_empty() {
                        break;
                    }
                    for r in &page.entries {
                        let cost = r
                            .cost_micro_usd
                            .map(|m| fmt_cost_micro(m, &r.cost_currency))
                            .unwrap_or_default();
                        let line = format!(
                            "{},{},{},{},{},{},{},{},{},{},{},{},{},{},{},{}\n",
                            r.id,
                            csv_cell(&format_local_datetime(r.ts_ms)),
                            csv_cell(&r.source),
                            csv_cell(r.client_name.as_deref().unwrap_or("")),
                            csv_cell(r.provider_name.as_deref().unwrap_or("")),
                            csv_cell(&r.requested_model),
                            csv_cell(r.upstream_model.as_deref().unwrap_or("")),
                            r.status,
                            csv_cell(&r.outcome),
                            r.ttft_ms.map_or(String::new(), |v| v.to_string()),
                            r.latency_ms,
                            r.input_tokens.unwrap_or(0),
                            r.output_tokens.unwrap_or(0),
                            r.cached_tokens.unwrap_or(0),
                            csv_cell(&cost),
                            csv_cell(&r.cost_currency),
                        );
                        csv.push_str(&line);
                    }
                    offset = offset.saturating_add(page.entries.len() as u64);
                    if offset >= page.total {
                        break;
                    }
                }
                std::fs::write(&path, csv)
                    .map_err(|e| AppError::io(format!("CSV write failed: {e}")))?;
                Ok::<_, AppError>(())
            },
            move |this, res, cx| {
                match res {
                    Ok(()) => {
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::Tr(this.lang)
                                .usage()
                                .usage_actions_exported_csv_to(path_for_msg.display()),
                        ));
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    fn active_log_filter(&self, cx: &gpui_kit::App) -> LogFilter {
        let search_raw = self.calls_search_input.read(cx).value().to_string();
        self.build_calls_log_filter(&search_raw)
    }

    #[must_use]
    pub fn total_scope_row(&self) -> UsageRow {
        let mut total = UsageRow::default();
        for row in self.provider_totals.values() {
            total.requests = total.requests.saturating_add(row.requests);
            total.ok_requests = total.ok_requests.saturating_add(row.ok_requests);
            total.failed = total.failed.saturating_add(row.failed);
            total.canceled = total.canceled.saturating_add(row.canceled);
            total.input_tokens = total.input_tokens.saturating_add(row.input_tokens);
            total.output_tokens = total.output_tokens.saturating_add(row.output_tokens);
            total.cached_tokens = total.cached_tokens.saturating_add(row.cached_tokens);
        }
        total
    }
}

fn csv_cell(raw: &str) -> String {
    if raw.contains(',') || raw.contains('"') || raw.contains('\n') {
        format!("\"{}\"", raw.replace('"', "\"\""))
    } else {
        raw.to_owned()
    }
}
