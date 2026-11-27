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
pub mod chart;
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
use spanbay_desktop::core::app::{current_time_ms, settings_get_impl};
use spanbay_desktop::core::llm::usage::UsageSource;
use spanbay_desktop::time::format_local_time;

use crate::bridge::runtime;
use crate::ui::{
    InputEvent, InputState, UiIcon, btn_danger, btn_ghost, btn_secondary, dismissible_banner,
    h_resizable, h_row, icon_xs, modal_dialog, palette, resizable_panel, tab_bar_segmented,
    tab_bar_underline, tab_item, to_hsla, toolbar_strip, v_col,
};

use state::{BreakdownDim, CallsQuickFilter, ChartMetric, RangePreset, UsageModal, UsageViewTab};
pub use state::{UsageHandoff, UsageView};

impl UsageView {
    pub fn new(window: &mut Window, cx: &mut Context<Self>) -> Self {
        let settings = settings_get_impl(runtime(cx).state());
        let search_ph = crate::ui::tr(cx).usage().usage_calls_search_placeholder();
        let calls_search_input = cx.new(|cx| InputState::new(window, cx).placeholder(search_ph));
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
            calls_page: 0,
            calls_page_size: 50,
            calls_total: 0,
            calls_loading: false,
            pending_live_logs: 0,
            breakdown_page: 0,
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

    /// Pre-filters Usage Analytics strictly to Local Gateway traffic (`source = Gateway`, `Today`).
    pub fn select_gateway_scope(&mut self, open_calls: bool, cx: &mut Context<Self>) {
        self.scope_provider_id = None;
        self.filter_source = Some(UsageSource::Gateway);
        self.range = RangePreset::Today;
        self.active_tab = if open_calls {
            UsageViewTab::Calls
        } else {
            UsageViewTab::Overview
        };
        self.reload_all(cx);
    }

    pub fn open_call_by_id(&mut self, call_id: i64, cx: &mut Context<Self>) {
        self.active_tab = UsageViewTab::Calls;
        self.pending_open_call_id = Some(call_id);
        cx.notify();
    }

    pub fn open_calls_for_run(
        &mut self,
        run_id: String,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        self.active_tab = UsageViewTab::Calls;
        self.scope_provider_id = None;
        self.calls_quick = CallsQuickFilter::All;
        self.calls_search_input
            .update(cx, |s, cx| s.set_value(format!("run:{run_id}"), window, cx));
        self.reload_all(cx);
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
        if let Ok(Some(id)) = spanbay_desktop::core::app::llm_log_id_for_request_id_impl(
            runtime(cx).state(),
            &request_id,
        ) {
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
        self.lang = crate::ui::active_language(cx);
        if let Some(call_id) = self.pending_open_call_id.take() {
            self.open_call_drawer(call_id, window, cx);
        }
        let p = palette(cx);
        let left = sidebar::render_sidebar(self, cx);

        let scope_title = self
            .scope_provider_id
            .as_deref()
            .map(|id| self.provider_name(id))
            .unwrap_or_else(|| {
                crate::ui::Tr(self.lang)
                    .usage()
                    .usage_usage_all_providers()
                    .to_owned()
            });

        let tabs = tab_bar_underline("usage-view-tabs")
            .selected_index(self.active_tab.index())
            .on_click(cx.listener(|this, idx: &usize, _, cx| {
                this.active_tab = UsageViewTab::from_index(*idx);
                cx.notify();
            }))
            .child(tab_item(
                crate::ui::Tr(self.lang).usage().usage_usage_overview(),
            ))
            .child(tab_item(
                crate::ui::tr(cx)
                    .usage()
                    .usage_usage_calls(self.calls_total as usize),
            ));

        let updated_lbl = crate::ui::tr(cx)
            .usage()
            .usage_usage_updated(format_local_time(self.updated_at_ms));

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
                        btn_ghost("usage-refresh", crate::ui::Tr(self.lang).common().refresh())
                            .icon(icon_xs(UiIcon::Refresh))
                            .on_click(cx.listener(|this, _, _, cx| this.reload_all(cx))),
                    )
                    .child(
                        btn_secondary(
                            "usage-export-csv",
                            crate::ui::Tr(self.lang).usage().usage_usage_export_csv(),
                        )
                        .icon(icon_xs(UiIcon::Download))
                        .on_click(cx.listener(|this, _, _, cx| this.export_csv(cx))),
                    ),
            );

        let src_idx = match self.filter_source {
            None => 0,
            Some(UsageSource::Gateway) => 1,
            Some(UsageSource::Agent) => 2,
            Some(UsageSource::Api) => 3,
            Some(UsageSource::Probe) => 4,
        };
        let u_tr = crate::ui::Tr(self.lang).usage();
        let src_seg = tab_bar_segmented("usage-source-seg")
            .selected_index(src_idx)
            .child(tab_item(u_tr.usage_overview_source_seg_all()))
            .child(tab_item(u_tr.usage_overview_source_seg_gateway()))
            .child(tab_item(u_tr.usage_overview_source_seg_agent()))
            .child(tab_item(u_tr.usage_overview_source_seg_api()))
            .child(tab_item(u_tr.usage_overview_source_seg_probe()))
            .on_click(cx.listener(|this, idx: &usize, _, cx| {
                this.filter_source = match *idx {
                    1 => Some(UsageSource::Gateway),
                    2 => Some(UsageSource::Agent),
                    3 => Some(UsageSource::Api),
                    4 => Some(UsageSource::Probe),
                    _ => None,
                };
                this.reload_all(cx);
            }));

        let source_bar = h_row()
            .w_full()
            .flex_shrink_0()
            .justify_between()
            .gap(px(10.0))
            .px(px(18.0))
            .py(px(7.0))
            .bg(to_hsla(p.pane))
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .child(
                h_row()
                    .gap(px(8.0))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.muted))
                            .child(u_tr.usage_dim_source()),
                    )
                    .child(src_seg),
            );

        let content = match self.active_tab {
            UsageViewTab::Overview => overview::render_overview(self, cx),
            UsageViewTab::Calls => calls::render_calls(self, cx),
        };

        let mut right = v_col()
            .size_full()
            .min_h_0()
            .child(header)
            .child(source_bar);
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
                    crate::ui::Tr(self.lang)
                        .usage()
                        .usage_usage_clear_call_logs(),
                    crate::ui::tr(cx)
                        .usage()
                        .usage_usage_permanently_delete_matching_call_log(count),
                    "usage-clr-logs-ok",
                    true,
                ),
                UsageModal::ClearCaptures { stats } => (
                    crate::ui::Tr(self.lang)
                        .usage()
                        .usage_usage_delete_stored_captures(),
                    crate::ui::tr(cx)
                        .usage()
                        .usage_usage_delete_captured_exchange_s_kb(stats.count, stats.bytes / 1024),
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
                    btn_secondary(
                        "usage-modal-cancel",
                        crate::ui::Tr(self.lang).common().cancel(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = None;
                        cx.notify();
                    })),
                )
                .child(
                    btn_danger(
                        confirm_id,
                        crate::ui::Tr(self.lang).usage().usage_usage_delete(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if is_logs {
                            this.confirm_clear_logs(cx);
                        } else {
                            this.confirm_clear_captures(cx);
                        }
                    })),
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
