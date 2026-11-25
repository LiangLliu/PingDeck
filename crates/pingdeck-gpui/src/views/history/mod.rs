//! Complete Execution History module (`NavModule::History`).
//!
//! Server-side filtered, keyset-paginated audit & replay browser backed by
//! `pingdeck_core::app::misc_logic_service` and `history_cookies_service`:
//! - Search + source / status-class / failed / environment filters
//! - Delete with 60s undo banner, confirmed Clear Filtered, and redacted HAR 1.2 export
//! - 6-tab inspector (Body, Headers, Sent Request, Request Snapshot, Timing & Call-Chain Trace,
//!   Checks & Run Children) plus 1-click Open in Requests and Ask Agent handoffs.

pub mod actions;
pub mod detail;
pub mod detail_tabs;
pub mod detail_trace;
pub mod sidebar;
pub mod state;

use std::cell::Cell;

use gpui_kit::{
    App, AppContext as _, ClickEvent, Context, IntoElement, ParentElement as _, Render,
    Styled as _, Window, div, px,
};
use pingdeck_desktop::core::app::settings_get_impl;
use pingdeck_desktop::core::model::HistoryFilter;

use crate::bridge::runtime;
use crate::ui::{
    InputEvent, InputState, btn_danger, btn_secondary, h_resizable, h_row, modal_dialog, palette,
    resizable_panel, to_hsla,
};

use state::{HistoryDetailTab, HistoryModal};
pub use state::{HistoryHandoff, HistoryView};

impl HistoryView {
    pub fn new(workspace_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let lang = settings_get_impl(runtime(cx).state()).general.language;
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("搜索 URL / 名称 / 状态码…"));
        let sub = cx.subscribe(&search_input, |this, _, ev: &InputEvent, cx| {
            if matches!(ev, InputEvent::Change | InputEvent::PressEnter { .. }) {
                this.reload_page(true, cx);
            }
        });

        let mut view = Self {
            workspace_id: workspace_id.clone(),
            lang,
            entries: Vec::new(),
            next_cursor: None,
            total: None,
            workspace_total: None,
            mock_dropped: None,
            environments: Vec::new(),
            filter: HistoryFilter::default(),
            search_input,
            loading: false,
            selected_id: None,
            selected_entry: None,
            run_children: Vec::new(),
            detail_tab: HistoryDetailTab::Body,
            body_pretty: true,
            last_undo: None,
            active_modal: None,
            status_msg: None,
            pending_handoff: Cell::new(None),
            _subscriptions: vec![sub],
            _tasks: Vec::new(),
        };
        if workspace_id.is_some() {
            view.reload_page(true, cx);
        }
        view
    }

    pub fn set_workspace(&mut self, workspace_id: Option<String>, cx: &mut Context<Self>) {
        if self.workspace_id == workspace_id {
            return;
        }
        self.workspace_id = workspace_id.clone();
        self.entries.clear();
        self.next_cursor = None;
        self.total = None;
        self.workspace_total = None;
        self.mock_dropped = None;
        self.environments.clear();
        self.filter = HistoryFilter::default();
        self.selected_id = None;
        self.selected_entry = None;
        self.run_children.clear();
        self.last_undo = None;
        self.active_modal = None;
        if workspace_id.is_some() {
            self.reload_page(true, cx);
        }
        cx.notify();
    }

    pub fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        self.reload_page(true, cx);
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
    pub fn take_pending_handoff(&self) -> Option<HistoryHandoff> {
        self.pending_handoff.take()
    }
}

impl Render for HistoryView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        let p = palette(cx);
        let left = sidebar::render_sidebar(self, cx);
        let right = detail::render_detail(self, cx);

        let mut root = div().relative().size_full().bg(to_hsla(p.surface)).child(
            h_resizable("history-split")
                .child(
                    resizable_panel()
                        .size(px(330.0))
                        .size_range(px(260.0)..px(480.0))
                        .child(left),
                )
                .child(resizable_panel().child(right)),
        );

        if let Some(HistoryModal::ClearFiltered { count }) = &self.active_modal {
            let count = *count;
            let entity = cx.entity().clone();
            let on_close = move |_: &ClickEvent, _: &mut Window, cx: &mut App| {
                entity.update(cx, |this, cx| {
                    this.active_modal = None;
                    cx.notify();
                });
            };
            let body = div()
                .text_sm()
                .text_color(to_hsla(p.ink))
                .child(if self.is_zh() {
                    format!("确定要永久清空当前筛选匹配的 {count} 条执行历史吗？此操作无法撤销。")
                } else {
                    format!(
                        "Permanently clear {count} matching history entries? This cannot be undone."
                    )
                })
                .into_any_element();
            let footer = h_row()
                .gap(px(8.0))
                .child(
                    btn_secondary("hist-clr-cancel", self.t("取消", "Cancel")).on_click(
                        cx.listener(|this, _, _, cx| {
                            this.active_modal = None;
                            cx.notify();
                        }),
                    ),
                )
                .child(
                    btn_danger("hist-clr-confirm", self.t("确认清空", "Clear History"))
                        .on_click(cx.listener(|this, _, _, cx| this.clear_filtered(cx))),
                )
                .into_any_element();
            root = root.child(modal_dialog(
                "hist-clear-modal",
                self.t("清空历史记录", "Clear Filtered History"),
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
