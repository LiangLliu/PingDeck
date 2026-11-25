//! Core data loaders, live LlmEvent subscription, body search, CSV export, and drill-down actions for `UsageView`.

use std::collections::{HashMap, HashSet};

use gpui_kit::{AppContext as _, Context, ParentElement as _, Styled as _, Window, px};
use pingdeck_desktop::core::app::{
    current_time_ms, llm_gateway_client_list_impl, llm_key_list_impl, llm_log_capture_stats_impl,
    llm_log_clear_impl, llm_log_count_impl, llm_log_delete_captures_impl, llm_log_get_impl,
    llm_log_list_impl, llm_log_search_bodies_impl, llm_model_list_impl, llm_provider_list_impl,
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

use super::call_drawer::CallDetailSheet;
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
                let mut added_rows = false;
                for seq in batch {
                    match seq.event {
                        LlmEvent::GatewayLogBatch { entries } => {
                            for entry in entries {
                                if !this.logs.iter().any(|r| r.id == entry.id) {
                                    this.logs.insert(0, entry);
                                    added_rows = true;
                                }
                            }
                            this.logs.truncate(500);
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
                if added_rows {
                    this.updated_at_ms = current_time_ms();
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
        let _ = llm_usage_set_tz_offset_impl(runtime(cx).state(), local_tz_offset_minutes());

        let scope_pid = self.scope_provider_id.clone();
        let filter_src = self.filter_source.map(|s| s.as_str().to_owned());
        let t_cat = spawn_blocking_into(
            cx,
            move |state| {
                let providers = llm_provider_list_impl(state).unwrap_or_default();
                let keys = llm_key_list_impl(state, None).unwrap_or_default();
                let models = llm_model_list_impl(state, None).unwrap_or_default();
                let clients = llm_gateway_client_list_impl(state).unwrap_or_default();
                let logs = llm_log_list_impl(
                    state,
                    Some(LogFilter {
                        source: filter_src,
                        provider_id: scope_pid,
                        ..LogFilter::default()
                    }),
                    Some(200),
                    None,
                )
                .unwrap_or_default();
                Ok::<_, AppError>((providers, keys, models, clients, logs))
            },
            |this, res, cx| {
                if let Ok((providers, keys, models, clients, logs)) = res {
                    this.providers = providers;
                    this.keys = keys;
                    this.models = models;
                    this.clients = clients;
                    this.logs_has_more = logs.len() >= 200;
                    this.logs = logs;
                    this.updated_at_ms = current_time_ms();
                }
                cx.notify();
            },
        );
        self._tasks.push(t_cat);
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

    pub fn load_more_logs(&mut self, cx: &mut Context<Self>) {
        let Some(last_id) = self.logs.last().map(|r| r.id) else {
            return;
        };
        let filter = LogFilter {
            source: self.filter_source.map(|s| s.as_str().to_owned()),
            provider_id: self.scope_provider_id.clone(),
            ..LogFilter::default()
        };
        let task = spawn_blocking_into(
            cx,
            move |state| llm_log_list_impl(state, Some(filter), Some(200), Some(last_id)),
            |this, res, cx| {
                if let Ok(more) = res {
                    this.logs_has_more = more.len() >= 200;
                    for row in more {
                        if !this.logs.iter().any(|existing| existing.id == row.id) {
                            this.logs.push(row);
                        }
                    }
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
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
                let task = spawn_async_into(
                    cx,
                    move |state| async move { llm_log_search_bodies_impl(&state, &term).await },
                    |this, res, cx| {
                        this.body_searching = false;
                        if let Ok(ids) = res {
                            this.body_match_ids = Some(ids.into_iter().collect::<HashSet<_>>());
                        }
                        cx.notify();
                    },
                );
                self._tasks.push(task);
            }
            _ => {
                self.body_match_ids = None;
                self.body_searching = false;
                cx.notify();
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
        let filter = self.active_log_filter();
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
        let filter = self.active_log_filter();
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
                let sheet_entity = cx.new(|cx| CallDetailSheet::new(parent, detail, window, cx));
                window.open_sheet(cx, move |sheet, _w, _cx| {
                    sheet.size(px(640.0)).p_0().child(sheet_entity.clone())
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

        let mut csv = String::from(
            "id,time,source,client,provider,requested_model,upstream_model,status,outcome,ttft_ms,latency_ms,input_tokens,output_tokens,cached_tokens,cost,currency\n",
        );
        let search = self.calls_search_input.read(cx).value().to_string();
        let rows = self.filtered_logs(&search);
        for r in rows {
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

        match std::fs::write(&path, csv) {
            Ok(()) => {
                self.status_msg = Some((
                    Tone::Success,
                    crate::ui::Tr(self.lang)
                        .usage()
                        .usage_actions_exported_csv_to(path.display()),
                ));
            }
            Err(e) => {
                self.status_msg = Some((Tone::Danger, e.to_string()));
            }
        }
        cx.notify();
    }

    fn active_log_filter(&self) -> LogFilter {
        LogFilter {
            source: self.filter_source.map(|s| s.as_str().to_owned()),
            provider_id: self.scope_provider_id.clone(),
            client_id: self.filter_client_id.clone(),
            status_class: None,
            since_ms: None,
        }
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
