//! Live gateway-only traffic log loading, filtering, search, pagination, and shared Call Detail
//! Drawer launch for `GatewayView`.

use std::collections::HashSet;

use gpui_kit::{AppContext as _, Context, ParentElement as _, Styled as _, Window, px};
use spanbay_desktop::core::app::{llm_log_get_impl, llm_log_page_impl, llm_log_search_bodies_impl};
use spanbay_desktop::core::llm::store::{LogFilter, StatusClass};
use spanbay_desktop::core::llm::usage::UsageSource;

use crate::bridge::{runtime, spawn_async_into, spawn_blocking_into};
use crate::ui::{Tone, WindowExt as _};
use crate::views::usage::call_drawer::{CallDetailHost, CallDetailSheet};

use super::GatewayView;

#[derive(Debug, Clone, Copy, PartialEq, Eq, Default)]
pub enum GatewayTrafficFilter {
    #[default]
    All,
    Failed,
    FailedOver,
    Rejected,
}

impl GatewayTrafficFilter {
    pub const ALL: [Self; 4] = [Self::All, Self::Failed, Self::FailedOver, Self::Rejected];

    #[must_use]
    pub fn label(self, cx: &gpui_kit::App) -> &'static str {
        let pr = crate::ui::tr(cx).providers();
        match self {
            Self::All => pr.prov_gateway_traffic_filter_all(),
            Self::Failed => pr.prov_gateway_traffic_filter_failed(),
            Self::FailedOver => pr.prov_gateway_traffic_filter_failover(),
            Self::Rejected => pr.prov_gateway_traffic_filter_rejected(),
        }
    }
}

impl GatewayView {
    #[must_use]
    pub fn build_traffic_log_filter(&self, search_raw: &str) -> LogFilter {
        let mut filter = LogFilter {
            source: Some(UsageSource::Gateway.as_str().to_owned()),
            ..LogFilter::default()
        };

        match self.traffic_filter {
            GatewayTrafficFilter::All => {}
            GatewayTrafficFilter::Failed => filter.failed_only = true,
            GatewayTrafficFilter::FailedOver => filter.status_class = Some(StatusClass::FailedOver),
            GatewayTrafficFilter::Rejected => filter.status_class = Some(StatusClass::ClientError),
        }

        if let Some(ids) = &self.traffic_body_match_ids {
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
            } else if let Some(v) = tok.strip_prefix("model:") {
                if !v.is_empty() {
                    filter.model_contains = Some(v.to_owned());
                }
            } else if let Some(v) = tok.strip_prefix("client:") {
                if !v.is_empty() {
                    filter.search_terms.push(v.to_owned());
                }
            } else if tok.starts_with("body:") {
                // Handled asynchronously via `traffic_body_match_ids`.
            } else {
                filter.search_terms.push(tok);
            }
        }

        filter
    }

    pub fn reload_traffic_page(&mut self, reset_page: bool, cx: &mut Context<Self>) {
        if reset_page {
            self.traffic_page = 0;
        }
        if self.traffic_page == 0 {
            self.traffic_pending_live = 0;
        }
        let search_raw = self.traffic_search_input.read(cx).value().to_string();
        let filter = self.build_traffic_log_filter(&search_raw);
        let page_size = self.traffic_page_size.clamp(10, 500);
        let req_page = self.traffic_page;
        let offset = (req_page as u64).saturating_mul(page_size as u64);
        self.traffic_loading = true;
        cx.notify();

        let task = spawn_blocking_into(
            cx,
            move |state| llm_log_page_impl(state, Some(filter), offset, Some(page_size as u32)),
            move |this, res, cx| {
                this.traffic_loading = false;
                match res {
                    Ok(page) => {
                        this.traffic_total = page.total;
                        let max_page = (page.total as usize)
                            .div_ceil(this.traffic_page_size.max(1))
                            .saturating_sub(1);
                        if page.entries.is_empty() && page.total > 0 && req_page > max_page {
                            this.traffic_page = max_page;
                            this.reload_traffic_page(false, cx);
                            return;
                        }
                        this.traffic_logs = page.entries;
                    }
                    Err(e) => this.status_msg = Some((Tone::Danger, e.message().to_owned())),
                }
                cx.notify();
            },
        );
        self._tasks.push(task);
    }

    pub fn set_traffic_page(&mut self, page: usize, cx: &mut Context<Self>) {
        let total_pages = (self.traffic_total as usize)
            .div_ceil(self.traffic_page_size.max(1))
            .max(1);
        let next = page.min(total_pages - 1);
        if next == self.traffic_page && self.traffic_pending_live == 0 {
            return;
        }
        self.traffic_page = next;
        if next == 0 {
            self.traffic_pending_live = 0;
        }
        self.reload_traffic_page(false, cx);
    }

    pub fn set_traffic_page_size(&mut self, size: usize, cx: &mut Context<Self>) {
        let clamped = size.clamp(10, 500);
        if clamped == self.traffic_page_size {
            return;
        }
        self.traffic_page_size = clamped;
        self.reload_traffic_page(true, cx);
    }

    pub fn handle_traffic_search_changed(&mut self, cx: &mut Context<Self>) {
        let raw = self.traffic_search_input.read(cx).value().to_string();
        let body_term = raw
            .split_whitespace()
            .find_map(|tok| tok.strip_prefix("body:"))
            .map(str::to_owned);
        match body_term {
            Some(term) if !term.is_empty() => {
                self.traffic_body_searching = true;
                self.traffic_page = 0;
                cx.notify();
                let task = spawn_async_into(
                    cx,
                    move |state| async move { llm_log_search_bodies_impl(&state, &term).await },
                    |this, res, cx| {
                        this.traffic_body_searching = false;
                        if let Ok(ids) = res {
                            this.traffic_body_match_ids =
                                Some(ids.into_iter().collect::<HashSet<_>>());
                        }
                        this.reload_traffic_page(true, cx);
                    },
                );
                self._tasks.push(task);
            }
            _ => {
                self.traffic_body_match_ids = None;
                self.traffic_body_searching = false;
                self.reload_traffic_page(true, cx);
            }
        }
    }

    pub fn open_traffic_call_drawer(
        &mut self,
        row_id: i64,
        window: &mut Window,
        cx: &mut Context<Self>,
    ) {
        match llm_log_get_impl(runtime(cx).state(), row_id) {
            Ok(detail) => {
                let host = CallDetailHost::Gateway(cx.entity().clone());
                let sheet_entity = cx.new(|cx| CallDetailSheet::new(host, detail, window, cx));
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
}
