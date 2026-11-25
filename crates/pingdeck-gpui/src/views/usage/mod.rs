//! Complete Usage Analytics & Gateway Request Logs module (`NavModule::Usage`).
//!
//! Aligned with `docs/design/prototype/index.html:1376-1523`:
//! - Two-pane `h_resizable` workspace: left provider-scope sidebar + right `统计` / `调用` workspace
//! - Live `llm_subscribe_impl` stream + local timezone sync (`llm_usage_set_tz_offset_impl`)
//! - 9 time-range presets, 4 prototype KPI cards, 5-metric interactive trend chart, 6-dimension
//!   clickable breakdown table, searchable/filterable 7-column Call Log, CSV export, and slide-over
//!   Call Detail Sheet with Replay / cURL / Diagnostics / Open in Requests / Ask Agent handoffs.

pub mod actions;
pub mod breakdown;
pub mod call_drawer;
pub mod call_drawer_tabs;
pub mod calls;
pub mod overview;
pub mod sidebar;
pub mod state;
pub mod types;

use std::cell::Cell;
use std::collections::HashMap;

use gpui_kit::{
    App, AppContext as _, ClickEvent, Context, IntoElement, ParentElement as _, Render,
    Styled as _, Window, div, px,
};
use pingdeck_desktop::core::app::{current_time_ms, settings_get_impl};
use pingdeck_desktop::time::format_local_time;

use crate::bridge::runtime;
use crate::ui::{
    InputEvent, InputState, UiIcon, btn_danger, btn_ghost, btn_secondary, dismissible_banner,
    h_resizable, h_row, icon_xs, modal_dialog, palette, resizable_panel, tab_bar_underline,
    tab_item, to_hsla, toolbar_strip, v_col,
};

use state::{BreakdownDim, CallsQuickFilter, ChartMetric, RangePreset, UsageModal, UsageViewTab};
pub use state::{UsageHandoff, UsageView};

impl UsageView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = settings_get_impl(runtime(cx).state());
        let calls_search_input = cx.new(|cx| {
            InputState::new(window, cx)
                .placeholder("筛选：is:failed source:gateway model:deepseek body:<关键词>…")
        });
        let search_sub = cx.subscribe(&calls_search_input, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change | InputEvent::PressEnter { .. }) {
                this.handle_calls_search_changed(cx);
            }
        });

        let mut view = Self {
            lang: settings.general.language,
            active_tab: UsageViewTab::Overview,
            range: RangePreset::Day7,
            scope_provider_id: None,
            filter_source: None,
            filter_model: None,
            filter_key_id: None,
            filter_client_id: None,
            chart_metric: ChartMetric::Calls,
            selected_currency: None,
            breakdown_dim: BreakdownDim::Model,
            calls_quick: CallsQuickFilter::All,
            calls_search_input,
            body_match_ids: None,
            body_searching: false,
            providers: Vec::new(),
            provider_statuses: HashMap::new(),
            provider_totals: HashMap::new(),
            keys: Vec::new(),
            models: Vec::new(),
            clients: Vec::new(),
            summary: None,
            latency_modes: Vec::new(),
            latency_by_day: HashMap::new(),
            latency_by_model: HashMap::new(),
            failures: Vec::new(),
            logs: Vec::new(),
            logs_has_more: false,
            capture_stats: None,
            retention_days: settings.ai.providers.usage_retention_days,
            updated_at_ms: current_time_ms(),
            active_modal: None,
            status_msg: None,
            pending_open_call_id: None,
            pending_handoff: Cell::new(None),
            llm_sub_id: None,
            llm_stream_task: None,
            _subscriptions: vec![search_sub],
            _tasks: Vec::new(),
        };
        view.init_tz_and_subscribe(cx);
        view.reload_all(cx);
        view
    }

    pub fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        self.init_tz_and_subscribe(cx);
        self.reload_all(cx);
    }

    pub fn select_provider_scope(
        &mut self,
        provider_id: Option<String>,
        open_calls: bool,
        cx: &mut Context<Self>,
    ) {
        self.scope_provider_id = provider_id;
        if open_calls {
            self.active_tab = UsageViewTab::Calls;
        }
        self.reload_all(cx);
    }

    pub fn open_call_by_id(&mut self, call_id: i64, cx: &mut Context<Self>) {
        self.active_tab = UsageViewTab::Calls;
        self.pending_open_call_id = Some(call_id);
        cx.notify();
    }

    pub fn open_call_by_request_id(&mut self, request_id: String, cx: &mut Context<Self>) {
        self.active_tab = UsageViewTab::Calls;
        if let Some(id) = self
            .logs
            .iter()
            .find(|r| r.request_id.as_deref() == Some(request_id.as_str()))
            .map(|r| r.id)
        {
            self.pending_open_call_id = Some(id);
            cx.notify();
            return;
        }
        if let Ok(rows) = pingdeck_desktop::core::app::llm_log_list_impl(
            runtime(cx).state(),
            None,
            Some(200),
            None,
        ) && let Some(id) = rows
            .iter()
            .find(|r| r.request_id.as_deref() == Some(request_id.as_str()))
            .map(|r| r.id)
        {
            self.pending_open_call_id = Some(id);
        }
        cx.notify();
    }

    pub fn dismiss_modal_if_open(&mut self, cx: &mut Context<Self>) -> bool {
        if self.active_modal.take().is_some() {
            cx.notify();
            true
        } else {
            false
        }
    }

    #[must_use]
    pub fn take_pending_handoff(&self) -> Option<UsageHandoff> {
        self.pending_handoff.take()
    }
}

impl Render for UsageView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        if let Some(call_id) = self.pending_open_call_id.take() {
            self.open_call_drawer(call_id, window, cx);
        }
        let p = palette(cx);
        let zh = self.is_zh();
        let left = sidebar::render_sidebar(self, cx);

        let scope_title = self
            .scope_provider_id
            .as_deref()
            .map(|id| self.provider_name(id))
            .unwrap_or_else(|| self.t("全部服务商", "All Providers").to_owned());

        let tabs = tab_bar_underline("usage-view-tabs")
            .selected_index(self.active_tab.index())
            .on_click(cx.listener(|this, idx: &usize, _, cx| {
                this.active_tab = UsageViewTab::from_index(*idx);
                cx.notify();
            }))
            .child(tab_item(self.t("统计", "Overview")))
            .child(tab_item(if zh {
                format!("调用 ({})", self.logs.len())
            } else {
                format!("Calls ({})", self.logs.len())
            }));

        let updated_lbl = if zh {
            format!("更新于 {}", format_local_time(self.updated_at_ms))
        } else {
            format!("Updated {}", format_local_time(self.updated_at_ms))
        };

        let header = toolbar_strip(cx)
            .justify_between()
            .child(
                h_row()
                    .gap(px(12.0))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(to_hsla(p.ink))
                            .child(scope_title),
                    )
                    .child(tabs),
            )
            .child(
                h_row()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .child(updated_lbl),
                    )
                    .child(
                        btn_ghost("usage-refresh", self.t("刷新", "Refresh"))
                            .icon(icon_xs(UiIcon::Refresh))
                            .on_click(cx.listener(|this, _, _, cx| this.reload_all(cx))),
                    )
                    .child(
                        btn_secondary("usage-export-csv", self.t("导出 CSV", "Export CSV"))
                            .icon(icon_xs(UiIcon::Download))
                            .on_click(cx.listener(|this, _, _, cx| this.export_csv(cx))),
                    ),
            );

        let content = match self.active_tab {
            UsageViewTab::Overview => overview::render_overview(self, cx),
            UsageViewTab::Calls => calls::render_calls(self, cx),
        };

        let mut right = v_col().size_full().min_h_0().child(header);
        if let Some((tone, msg)) = &self.status_msg {
            right = right.child(div().px(px(16.0)).pt(px(8.0)).child(dismissible_banner(
                "usage-banner-close",
                *tone,
                msg.clone(),
                cx.listener(|this, _, _, cx| {
                    this.status_msg = None;
                    cx.notify();
                }),
                cx,
            )));
        }
        right = right.child(div().flex_1().min_h_0().w_full().child(content));

        let mut root = div().relative().size_full().bg(to_hsla(p.surface)).child(
            h_resizable("usage-split")
                .child(
                    resizable_panel()
                        .size(px(236.0))
                        .size_range(px(196.0)..px(340.0))
                        .child(left),
                )
                .child(resizable_panel().child(right)),
        );

        if let Some(modal) = self.active_modal.clone() {
            let entity = cx.entity().clone();
            let on_close = move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |this, cx| {
                    this.active_modal = None;
                    cx.notify();
                });
            };
            let (title, body_txt, confirm_id, is_logs) = match modal {
                UsageModal::ClearLogs { count } => (
                    self.t("清空调用日志", "Clear Call Logs"),
                    if zh {
                        format!(
                            "确定永久删除当前筛选范围内的 {count} 条调用日志及其捕获正文吗？此操作无法撤销。"
                        )
                    } else {
                        format!(
                            "Permanently delete {count} matching call log(s) and captures? This cannot be undone."
                        )
                    },
                    "usage-clr-logs-ok",
                    true,
                ),
                UsageModal::ClearCaptures { stats } => (
                    self.t("清理捕获正文", "Delete Stored Captures"),
                    if zh {
                        format!(
                            "确定删除 {} 条捕获正文（约 {} KB）？调用日志元信息与统计汇总将保留。",
                            stats.count,
                            stats.bytes / 1024
                        )
                    } else {
                        format!(
                            "Delete {} captured exchange(s) (~{} KB)? Log metadata and usage totals remain intact.",
                            stats.count,
                            stats.bytes / 1024
                        )
                    },
                    "usage-clr-caps-ok",
                    false,
                ),
            };
            let body = div()
                .text_sm()
                .text_color(to_hsla(p.ink))
                .child(body_txt)
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("usage-modal-cancel", self.t("取消", "Cancel")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_danger(confirm_id, self.t("确认删除", "Delete")).on_click(cx.listener(
                        move |this, _, _, cx| {
                            if is_logs {
                                this.confirm_clear_logs(cx);
                            } else {
                                this.confirm_clear_captures(cx);
                            }
                        },
                    )),
                )
                .into_any_element();
            root = root.child(modal_dialog(
                "usage-confirm-modal",
                title,
                440.0,
                body,
                footer,
                window,
                cx,
                on_close,
            ));
        }

        root
    }
}
