//! Complete Mock Servers module (`NavModule::Mock`).
//!
//! Full-featured local HTTP/CRUD mock workbench backed by `pingdeck_core::app::mock_service`:
//! - Master sidebar with live status indicators, port pills, hit counters, search, and Import
//! - 7 dedicated sub-tabs: Routes (HTTP branches, matching rules, faults, SSE streams, CRUD
//!   buckets), Data (seed JSON + live runtime state reset), Headers (global headers + CORS),
//!   Callbacks (outbound webhooks), Traffic (live coalesced log stream + route generation),
//!   Proxy (transparent upstream forwarding + recording), and Settings (port, prefix, environment).

pub mod actions;
pub mod actions_edit;
pub mod actions_try;
pub mod modals;
pub mod sidebar;
pub mod state;
pub mod tab_extra;
pub mod tab_ops;
pub mod tab_route_detail;
pub mod tab_route_rules;
pub mod tab_route_strip;
pub mod tab_routes;
pub mod tab_traffic;

use std::cell::Cell;
use std::collections::HashMap;

use gpui_kit::{
    AppContext as _, Context, IntoElement, ParentElement as _, Render, SharedString, Styled as _,
    Window, div, px,
};
use pingdeck_desktop::core::app::settings_get_impl;
use pingdeck_desktop::core::model::items::{MockRuleOperator, MockRuleTarget};
use pingdeck_desktop::services::clipboard;

use crate::bridge::runtime;
use crate::ui::{
    EditorState, InputEvent, InputState, StatusShape, Tone, UiIcon, badge, btn_danger, btn_icon,
    btn_primary, btn_secondary, dismissible_banner, empty_state_with_actions, h_resizable, h_row,
    icon_xs, meta_pill, palette, resizable_panel, status_indicator, subtab_strip,
    tab_bar_underline, tab_item, to_hsla, toolbar_strip, v_col,
};

pub use state::{MockHandoff, MockView};
use state::{MockModal, MockSubTab, TrafficFilter};

impl MockView {
    pub fn new(workspace_id: Option<String>, window: &mut Window, cx: &mut Context<Self>) -> Self {
        let lang = settings_get_impl(runtime(cx).state()).general.language;
        let m_tr = crate::ui::tr(cx).mock();
        let search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(m_tr.ph_filter_servers()));
        let route_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(m_tr.ph_filter_routes()));
        let new_srv_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Payment Mock API"));
        let new_srv_port_input = cx.new(|cx| InputState::new(window, cx).placeholder("3001"));
        let srv_name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Local mock"));
        let srv_port_input = cx.new(|cx| InputState::new(window, cx).placeholder("3001"));
        let srv_prefix_input = cx.new(|cx| InputState::new(window, cx).placeholder("/api"));
        let srv_latency_input = cx.new(|cx| InputState::new(window, cx).placeholder("0"));
        let route_path_input = cx.new(|cx| InputState::new(window, cx).placeholder("/users/:id"));
        let route_desc_input = cx.new(|cx| InputState::new(window, cx).placeholder("Description…"));
        let route_crud_id_input = cx.new(|cx| InputState::new(window, cx).placeholder("id"));
        let resp_label_input = cx.new(|cx| InputState::new(window, cx).placeholder("200 OK"));
        let resp_status_input = cx.new(|cx| InputState::new(window, cx).placeholder("200"));
        let resp_latency_input = cx.new(|cx| InputState::new(window, cx).placeholder("0"));
        let resp_body_editor = cx.new(|cx| {
            let mut ed = EditorState::new(window, cx)
                .soft_wrap(true)
                .placeholder("{\n  \"ok\": true\n}");
            ed.set_highlighter("json", cx);
            ed
        });
        let resp_file_input = cx.new(|cx| InputState::new(window, cx).placeholder("/path/to/file"));
        let stream_interval_input = cx.new(|cx| InputState::new(window, cx).placeholder("200"));
        let stream_ev_name_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("event (optional)"));
        let stream_ev_data_input = cx.new(|cx| InputState::new(window, cx).placeholder("data…"));
        let kv_key_input = cx.new(|cx| InputState::new(window, cx).placeholder("Header name"));
        let kv_val_input = cx.new(|cx| InputState::new(window, cx).placeholder("Header value"));
        let proxy_res_kv_key =
            cx.new(|cx| InputState::new(window, cx).placeholder("Response header name"));
        let proxy_res_kv_val =
            cx.new(|cx| InputState::new(window, cx).placeholder("Response header value"));
        let rule_mod_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Key / JSON path (e.g. $.role)"));
        let rule_val_input = cx.new(|cx| InputState::new(window, cx).placeholder("Expected value"));
        let bucket_name_input = cx.new(|cx| InputState::new(window, cx).placeholder("items"));
        let bucket_doc_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("Bucket notes…"));
        let bucket_json_editor = cx.new(|cx| {
            let mut ed = EditorState::new(window, cx)
                .soft_wrap(true)
                .placeholder("[]");
            ed.set_highlighter("json", cx);
            ed
        });
        let cb_name_input = cx.new(|cx| InputState::new(window, cx).placeholder("Webhook"));
        let cb_url_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://example.com/hook"));
        let cb_delay_input = cx.new(|cx| InputState::new(window, cx).placeholder("0"));
        let cb_body_editor = cx.new(|cx| {
            let mut ed = EditorState::new(window, cx).soft_wrap(true);
            ed.set_highlighter("json", cx);
            ed
        });
        let proxy_url_input =
            cx.new(|cx| InputState::new(window, cx).placeholder("https://api.example.com"));
        let traffic_search_input =
            cx.new(|cx| InputState::new(window, cx).placeholder(m_tr.ph_filter_traffic()));
        let import_editor = cx.new(|cx| {
            let mut ed = EditorState::new(window, cx)
                .soft_wrap(true)
                .placeholder("Paste PingDeck / Mockoon / OpenAPI document…");
            ed.set_highlighter("json", cx);
            ed
        });

        let mut subs = Vec::new();
        for inp in [
            &srv_name_input,
            &srv_port_input,
            &srv_prefix_input,
            &srv_latency_input,
            &route_path_input,
            &route_desc_input,
            &route_crud_id_input,
            &resp_label_input,
            &resp_status_input,
            &resp_latency_input,
            &resp_file_input,
            &stream_interval_input,
            &bucket_name_input,
            &bucket_doc_input,
            &cb_name_input,
            &cb_url_input,
            &cb_delay_input,
            &proxy_url_input,
        ] {
            subs.push(cx.subscribe(inp, |this, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) && !this.syncing {
                    this.dirty = true;
                    cx.notify();
                }
            }));
        }
        for ed in [&resp_body_editor, &bucket_json_editor, &cb_body_editor] {
            subs.push(cx.subscribe(ed, |this, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) && !this.syncing {
                    this.dirty = true;
                    cx.notify();
                }
            }));
        }
        for filter_inp in [&search_input, &route_search_input] {
            subs.push(cx.subscribe(filter_inp, |_, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    cx.notify();
                }
            }));
        }
        subs.push(
            cx.subscribe(&traffic_search_input, |this, _, ev: &InputEvent, cx| {
                if matches!(ev, InputEvent::Change) {
                    this.traffic_page = 0;
                    cx.notify();
                }
            }),
        );

        let mut view = Self {
            workspace_id: workspace_id.clone(),
            lang,
            servers: Vec::new(),
            statuses: HashMap::new(),
            environments: Vec::new(),
            selected_id: None,
            draft: None,
            dirty: false,
            active_tab: MockSubTab::Routes,
            selected_route_id: None,
            selected_response_id: None,
            selected_bucket_id: None,
            selected_callback_id: None,
            logs: Vec::new(),
            selected_log_id: None,
            traffic_filter: TrafficFilter::All,
            traffic_page: 0,
            log_sub_id: None,
            log_stream_task: None,
            live_data: None,
            new_rule_target: MockRuleTarget::Query,
            new_rule_op: MockRuleOperator::Equals,
            new_rule_invert: false,
            search_input,
            route_search_input,
            new_srv_name_input,
            new_srv_port_input,
            srv_name_input,
            srv_port_input,
            srv_prefix_input,
            srv_latency_input,
            route_path_input,
            route_desc_input,
            route_crud_id_input,
            resp_label_input,
            resp_status_input,
            resp_latency_input,
            resp_body_editor,
            resp_file_input,
            stream_interval_input,
            stream_ev_name_input,
            stream_ev_data_input,
            kv_key_input,
            kv_val_input,
            proxy_res_kv_key,
            proxy_res_kv_val,
            rule_mod_input,
            rule_val_input,
            bucket_name_input,
            bucket_doc_input,
            bucket_json_editor,
            cb_name_input,
            cb_url_input,
            cb_delay_input,
            cb_body_editor,
            proxy_url_input,
            traffic_search_input,
            import_editor,
            try_it_busy: false,
            try_it_result: None,
            active_modal: None,
            status_msg: None,
            pending_sync: false,
            syncing: false,
            pending_handoff: Cell::new(None),
            _subscriptions: subs,
            _tasks: Vec::new(),
        };
        if let Some(ws) = workspace_id {
            view.reload(ws, cx);
        }
        view
    }

    pub fn set_workspace(&mut self, workspace_id: Option<String>, cx: &mut Context<Self>) {
        if self.workspace_id == workspace_id {
            return;
        }
        self.unsubscribe_logs(cx);
        self.workspace_id = workspace_id.clone();
        self.servers.clear();
        self.statuses.clear();
        self.environments.clear();
        self.selected_id = None;
        self.draft = None;
        self.dirty = false;
        self.logs.clear();
        self.traffic_page = 0;
        self.live_data = None;
        self.try_it_busy = false;
        self.try_it_result = None;
        self.active_modal = None;
        if let Some(ws) = workspace_id {
            self.reload(ws, cx);
        }
        cx.notify();
    }

    pub fn refresh_catalog(&mut self, cx: &mut Context<Self>) {
        self.lang = settings_get_impl(runtime(cx).state()).general.language;
        if let Some(ws) = self.workspace_id.clone() {
            self.reload(ws, cx);
        }
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
    pub fn take_pending_handoff(&self) -> Option<MockHandoff> {
        self.pending_handoff.take()
    }
}

impl Render for MockView {
    fn render(&mut self, window: &mut Window, cx: &mut Context<Self>) -> impl IntoElement {
        self.lang = crate::ui::active_language(cx);
        if self.pending_sync {
            self.sync_inputs(window, cx);
        }
        let p = palette(cx);

        let left = sidebar::render_sidebar(self, cx);
        let right = if let Some(srv) = self.draft.clone() {
            let st = self.status_of(&srv.id).cloned();
            let running = st.as_ref().is_some_and(|s| s.running);
            let url = st
                .as_ref()
                .map(|s| format!("{}{}", s.bound_url, s.endpoint_prefix))
                .unwrap_or_else(|| format!("http://127.0.0.1:{}{}", srv.port, srv.endpoint_prefix));
            let url_copy = url.clone();
            let srv_id_del = srv.id.clone();
            let srv_name_del = srv.name.clone();

            let mut header = toolbar_strip(cx)
                .justify_between()
                .child(
                    h_row()
                        .flex_1()
                        .min_w_0()
                        .gap(px(8.0))
                        .child(status_indicator(
                            if running {
                                StatusShape::Live
                            } else {
                                StatusShape::Idle
                            },
                            cx,
                        ))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(srv.name.clone()),
                        )
                        .child(meta_pill(url, cx))
                        .children(srv.cors_enabled.then(|| badge("CORS", Tone::Success, cx)))
                        .children(
                            (srv.latency_ms > 0)
                                .then(|| meta_pill(format!("+{}ms", srv.latency_ms), cx)),
                        )
                        .child(
                            btn_icon("mock-copy-url", UiIcon::Copy)
                                .tooltip(crate::ui::Tr(self.lang).mock().mock_copy_local_url())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    let _ = clipboard::set_text(&url_copy);
                                    this.status_msg = Some((
                                        Tone::Success,
                                        crate::ui::Tr(this.lang)
                                            .mock()
                                            .mock_copied_local_url()
                                            .to_owned(),
                                    ));
                                    cx.notify();
                                })),
                        ),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .flex_shrink_0()
                        .child(if self.dirty {
                            btn_primary("mock-save-btn", crate::ui::Tr(self.lang).common().save())
                                .icon(icon_xs(UiIcon::Check))
                                .on_click(cx.listener(|this, _, _, cx| this.save_draft(cx)))
                        } else {
                            btn_secondary(
                                "mock-save-btn",
                                crate::ui::Tr(self.lang).common().save_saved(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.save_draft(cx)))
                        })
                        .child(if running {
                            btn_danger("mock-run-btn", crate::ui::Tr(self.lang).mock().mock_stop())
                                .icon(icon_xs(UiIcon::Stop))
                                .on_click(cx.listener(|this, _, _, cx| this.toggle_start_stop(cx)))
                        } else {
                            btn_primary(
                                "mock-run-btn",
                                crate::ui::Tr(self.lang).mock().mock_start(),
                            )
                            .icon(icon_xs(UiIcon::Play))
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_start_stop(cx)))
                        })
                        .children(running.then(|| {
                            btn_icon("mock-reset-state", UiIcon::RotateCcw)
                                .tooltip(
                                    crate::ui::Tr(self.lang)
                                        .mock()
                                        .mock_reset_counters_cursors(),
                                )
                                .on_click(cx.listener(|this, _, _, cx| {
                                    this.reset_runtime_state(cx);
                                }))
                        }))
                        .child(
                            btn_icon("mock-dup-srv", UiIcon::Copy)
                                .tooltip(crate::ui::Tr(self.lang).mock().mock_duplicate_server())
                                .on_click(
                                    cx.listener(|this, _, _, cx| this.duplicate_selected(cx)),
                                ),
                        )
                        .child(
                            btn_icon("mock-del-srv", UiIcon::Trash)
                                .tooltip(crate::ui::Tr(self.lang).mock().settings_delete())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.active_modal = Some(MockModal::DeleteServer {
                                        id: srv_id_del.clone(),
                                        name: srv_name_del.clone(),
                                    });
                                    cx.notify();
                                })),
                        ),
                );
            let _ = &mut header;

            let mut tab_bar = tab_bar_underline("mock-subtabs")
                .selected_index(self.active_tab.index())
                .on_click(cx.listener(|this, idx: &usize, _, cx| {
                    this.snapshot_inputs(cx);
                    this.active_tab = MockSubTab::from_index(*idx);
                    if this.active_tab == MockSubTab::Data {
                        this.refresh_live_data(cx);
                    }
                    cx.notify();
                }));
            for tab in MockSubTab::ALL {
                let count = match tab {
                    MockSubTab::Routes => Some(srv.routes.len()),
                    MockSubTab::Data => Some(srv.data_buckets.len()),
                    MockSubTab::Callbacks => Some(srv.callbacks.len()),
                    MockSubTab::Traffic if !self.logs.is_empty() => Some(self.logs.len()),
                    _ => None,
                };
                let label = match count {
                    Some(n) => SharedString::from(format!("{} ({n})", tab.label(self.lang))),
                    None => SharedString::from(tab.label(self.lang)),
                };
                tab_bar = tab_bar.child(tab_item(label));
            }

            let tab_body = match self.active_tab {
                MockSubTab::Routes => tab_routes::render_routes_tab(self, cx),
                MockSubTab::Data => tab_extra::render_data_tab(self, cx),
                MockSubTab::Headers => tab_extra::render_headers_tab(self, cx),
                MockSubTab::Callbacks => tab_ops::render_callbacks_tab(self, cx),
                MockSubTab::Traffic => tab_traffic::render_traffic_tab(self, cx),
                MockSubTab::Proxy => tab_ops::render_proxy_tab(self, cx),
                MockSubTab::Settings => tab_ops::render_settings_tab(self, cx),
            };

            let mut detail = v_col()
                .size_full()
                .min_h_0()
                .child(header)
                .child(subtab_strip(
                    tab_bar,
                    st.as_ref().filter(|s| s.running).map(|s| {
                        meta_pill(crate::ui::tr(cx).mock().mock_hits(s.hit_count), cx)
                            .into_any_element()
                    }),
                    cx,
                ));

            if let Some((tone, msg)) = &self.status_msg {
                detail = detail.child(div().px(px(12.0)).pt(px(8.0)).child(dismissible_banner(
                    "mock-banner-close",
                    *tone,
                    msg.clone(),
                    cx.listener(|this, _, _, cx| {
                        this.status_msg = None;
                        cx.notify();
                    }),
                    cx,
                )));
            }

            detail.child(div().flex_1().min_h_0().w_full().child(tab_body))
        } else {
            v_col().size_full().child(empty_state_with_actions(
                UiIcon::Server,
                crate::ui::Tr(self.lang)
                    .mock()
                    .mock_no_mock_server_selected(),
                crate::ui::Tr(self.lang)
                    .mock()
                    .mock_run_local_loopback_http_crud(),
                Some(
                    h_row()
                        .gap(px(8.0))
                        .child(
                            btn_primary(
                                "mock-empty-new",
                                crate::ui::Tr(self.lang).mock().mock_new_mock_server(),
                            )
                            .icon(icon_xs(UiIcon::Plus))
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.open_create_server_modal(window, cx);
                                },
                            )),
                        )
                        .child(
                            btn_secondary(
                                "mock-empty-imp",
                                crate::ui::Tr(self.lang).mock().empty_import(),
                            )
                            .icon(icon_xs(UiIcon::Upload))
                            .on_click(cx.listener(
                                |this, _, window, cx| {
                                    this.open_import_modal(window, cx);
                                },
                            )),
                        )
                        .into_any_element(),
                ),
                cx,
            ))
        };

        let mut root = div().relative().size_full().bg(to_hsla(p.surface)).child(
            h_resizable("mock-split")
                .child(
                    resizable_panel()
                        .size(px(260.0))
                        .size_range(px(210.0)..px(380.0))
                        .child(left),
                )
                .child(resizable_panel().child(right)),
        );
        if let Some(modal) = modals::render_modal(self, window, cx) {
            root = root.child(modal);
        }
        root
    }
}
