//! Callbacks, Proxy, and Server Settings sub-tabs for `MockView`.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use pingdeck_desktop::core::model::HttpMethod;

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, alert_banner, btn_ghost, btn_icon, btn_secondary, card,
    choice_chip, code_editor, empty_inline, empty_state, h_row, icon_xs, list_row, number_field,
    palette, section_header, setting_row, text_input, to_hsla, toggle_switch, v_col,
};

use super::state::{MockModal, MockView};
use super::tab_extra::{KvListTarget, render_kv_card};

pub fn render_callbacks_tab(view: &MockView, cx: &mut Context<MockView>) -> AnyElement {
    let p = palette(cx);
    let Some(srv) = &view.draft else {
        return div().into_any_element();
    };

    let mut list = v_col().w_full().p(px(8.0)).gap(px(4.0));
    if srv.callbacks.is_empty() {
        list = list.child(empty_inline(
            crate::ui::Tr(view.lang).mock().tab_ops_no_callbacks_yet(),
            Some(SharedString::from(
                crate::ui::Tr(view.lang)
                    .mock()
                    .tab_ops_attach_callbacks_to_response_branches(),
            )),
            None,
            cx,
        ));
    } else {
        for cb in &srv.callbacks {
            let cid = cb.id.clone();
            let cid_del = cb.id.clone();
            let active = view.selected_callback_id.as_deref() == Some(cb.id.as_str());
            list = list.child(
                list_row(
                    SharedString::from(format!("mock-cb-{}", cb.id)),
                    active,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.snapshot_inputs(cx);
                        this.selected_callback_id = Some(cid.clone());
                        this.pending_sync = true;
                        cx.notify();
                    }),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(cb.name.clone()),
                )
                .child(
                    btn_icon(
                        SharedString::from(format!("mock-cb-del-{}", cb.id)),
                        UiIcon::Trash,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.snapshot_inputs(cx);
                        if let Some(s) = this.draft.as_mut() {
                            s.callbacks.retain(|x| x.id != cid_del);
                            for r in &mut s.routes {
                                for rp in &mut r.responses {
                                    rp.callback_ids.retain(|x| x != &cid_del);
                                }
                            }
                            this.dirty = true;
                            this.pending_sync = true;
                            cx.notify();
                        }
                    })),
                ),
            );
        }
    }

    let left = v_col()
        .w(px(240.0))
        .flex_shrink_0()
        .h_full()
        .bg(to_hsla(p.pane))
        .border_r_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .px(px(10.0))
                .py(px(8.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(crate::ui::Tr(view.lang).mock().tab_ops_callbacks()),
                )
                .child(
                    btn_secondary(
                        "mock-add-cb",
                        crate::ui::Tr(view.lang).mock().tab_extra_add(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| this.add_callback(cx))),
                ),
        )
        .child(
            div()
                .id("mock-cb-list-scroll")
                .flex_1()
                .min_h_0()
                .overflow_y_scrollbar()
                .child(list),
        );

    let right = if let Some(cb) = view.selected_callback() {
        let methods = [
            HttpMethod::Post,
            HttpMethod::Put,
            HttpMethod::Patch,
            HttpMethod::Get,
            HttpMethod::Delete,
        ];
        let mut m_row = h_row().gap(px(4.0)).flex_wrap();
        for m in methods {
            let label = format!("{m:?}").to_uppercase();
            m_row = m_row.child(
                choice_chip(
                    SharedString::from(format!("mock-cbm-{label}")),
                    label,
                    cb.method == m,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(c) = this.selected_callback_mut() {
                        c.method = m;
                        this.dirty = true;
                        cx.notify();
                    }
                })),
            );
        }
        let body = v_col()
            .w_full()
            .p(px(16.0))
            .gap(px(14.0))
            .child(
                card(cx)
                    .p(px(14.0))
                    .gap(px(10.0))
                    .child(section_header(
                        crate::ui::Tr(view.lang)
                            .mock()
                            .tab_ops_callback_configuration(),
                        Some(
                            btn_secondary(
                                "mock-cb-test-now",
                                crate::ui::Tr(view.lang).mock().tab_ops_test_now(),
                            )
                            .icon(icon_xs(UiIcon::Send))
                            .on_click(cx.listener(|this, _, _, cx| this.test_callback_now(cx)))
                            .into_any_element(),
                        ),
                        cx,
                    ))
                    .child(m_row)
                    .child(
                        h_row()
                            .w_full()
                            .gap(px(10.0))
                            .child(div().w(px(180.0)).child(text_input(&view.cb_name_input)))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(text_input(&view.cb_url_input)),
                            )
                            .child(number_field(&view.cb_delay_input, Some("ms"), 86.0, cx)),
                    )
                    .child(
                        div()
                            .w_full()
                            .h(px(160.0))
                            .child(code_editor(&view.cb_body_editor)),
                    ),
            )
            .child(render_kv_card(
                view,
                crate::ui::Tr(view.lang).mock().tab_ops_callback_headers(),
                &cb.headers,
                KvListTarget::Callback,
                cx,
            ));
        div()
            .id("mock-cb-detail-scroll")
            .size_full()
            .overflow_y_scrollbar()
            .child(body)
            .into_any_element()
    } else {
        empty_state(
            UiIcon::Zap,
            crate::ui::Tr(view.lang)
                .mock()
                .tab_ops_select_or_create_a_callback(),
            crate::ui::Tr(view.lang)
                .mock()
                .tab_ops_configure_outbound_webhook_requests_trigge(),
            cx,
        )
        .into_any_element()
    };

    h_row()
        .size_full()
        .min_h_0()
        .child(left)
        .child(div().flex_1().min_w_0().h_full().child(right))
        .into_any_element()
}

pub fn render_proxy_tab(view: &MockView, cx: &mut Context<MockView>) -> AnyElement {
    let Some(srv) = &view.draft else {
        return div().into_any_element();
    };
    let running = view.is_running(&srv.id);
    let recording = view.status_of(&srv.id).is_some_and(|s| s.recording);
    let rec_count = view.status_of(&srv.id).map_or(0, |s| s.recorded_count);

    let mut body = v_col().w_full().p(px(18.0)).gap(px(14.0)).child(
        card(cx)
            .p(px(14.0))
            .gap(px(8.0))
            .child(section_header(
                crate::ui::Tr(view.lang).mock().tab_ops_transparent_proxy(),
                None,
                cx,
            ))
            .child(setting_row(
                crate::ui::Tr(view.lang)
                    .mock()
                    .tab_ops_proxy_unmatched_requests_upstream(),
                None,
                Some(SharedString::from(
                    crate::ui::Tr(view.lang)
                        .mock()
                        .tab_ops_forwards_unmatched_requests_to_the(),
                )),
                toggle_switch("mock-proxy-en", srv.proxy_enabled).on_click(cx.listener(
                    |this, _, _, cx| {
                        if let Some(s) = this.draft.as_mut() {
                            s.proxy_enabled = !s.proxy_enabled;
                            this.dirty = true;
                            cx.notify();
                        }
                    },
                )),
                cx,
            ))
            .child(setting_row(
                crate::ui::Tr(view.lang).mock().tab_ops_upstream_base_url(),
                None,
                None,
                div().w(px(340.0)).child(text_input(&view.proxy_url_input)),
                cx,
            ))
            .child(setting_row(
                crate::ui::Tr(view.lang)
                    .mock()
                    .tab_ops_strip_endpoint_prefix_when_forwarding(),
                None,
                None,
                toggle_switch("mock-proxy-strip", srv.proxy_remove_prefix).on_click(cx.listener(
                    |this, _, _, cx| {
                        if let Some(s) = this.draft.as_mut() {
                            s.proxy_remove_prefix = !s.proxy_remove_prefix;
                            this.dirty = true;
                            cx.notify();
                        }
                    },
                )),
                cx,
            )),
    );

    if running && srv.proxy_enabled {
        body = body.child(alert_banner(
            if recording {
                Tone::Accent
            } else {
                Tone::Neutral
            },
            if recording {
                crate::ui::Tr(view.lang)
                    .mock()
                    .proxy_recording_active(rec_count)
            } else {
                crate::ui::Tr(view.lang)
                    .mock()
                    .proxy_recording_idle(rec_count)
            },
            Some(
                h_row()
                    .gap(px(6.0))
                    .child(
                        btn_secondary(
                            "mock-proxy-rec-toggle",
                            if recording {
                                crate::ui::Tr(view.lang).mock().tab_ops_stop_recording()
                            } else {
                                crate::ui::Tr(view.lang).mock().tab_ops_start_recording()
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_recording(cx))),
                    )
                    .children((rec_count > 0).then(|| {
                        btn_secondary(
                            "mock-proxy-rec-save",
                            crate::ui::Tr(view.lang).mock().tab_ops_save_as_routes(),
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.save_recorded_routes(cx)))
                    }))
                    .into_any_element(),
            ),
            cx,
        ));
    }

    body = body
        .child(render_kv_card(
            view,
            crate::ui::Tr(view.lang)
                .mock()
                .tab_ops_proxy_request_headers(),
            &srv.proxy_req_headers,
            KvListTarget::ProxyReq,
            cx,
        ))
        .child(render_kv_card(
            view,
            crate::ui::Tr(view.lang)
                .mock()
                .tab_ops_proxy_response_headers(),
            &srv.proxy_res_headers,
            KvListTarget::ProxyRes,
            cx,
        ));

    div()
        .id("mock-proxy-scroll")
        .size_full()
        .overflow_y_scrollbar()
        .child(body)
        .into_any_element()
}

pub fn render_settings_tab(view: &MockView, cx: &mut Context<MockView>) -> AnyElement {
    let Some(srv) = &view.draft else {
        return div().into_any_element();
    };
    let running = view.is_running(&srv.id);
    let mut env_chips = h_row().gap(px(4.0)).flex_wrap().child(
        choice_chip(
            "mock-env-none",
            crate::ui::Tr(view.lang).mock().tab_ops_none(),
            srv.environment_id.is_none(),
            cx,
        )
        .on_click(cx.listener(|this, _, _, cx| {
            if let Some(s) = this.draft.as_mut() {
                s.environment_id = None;
                this.dirty = true;
                cx.notify();
            }
        })),
    );
    for env in &view.environments {
        let eid = env.id.clone();
        let active = srv.environment_id.as_deref() == Some(env.id.as_str());
        env_chips = env_chips.child(
            choice_chip(
                SharedString::from(format!("mock-env-{}", env.id)),
                env.name.clone(),
                active,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                if let Some(s) = this.draft.as_mut() {
                    s.environment_id = Some(eid.clone());
                    this.dirty = true;
                    cx.notify();
                }
            })),
        );
    }

    let body = v_col()
        .w_full()
        .p(px(18.0))
        .gap(px(14.0))
        .children(running.then(|| {
            alert_banner(
                Tone::Warning,
                crate::ui::Tr(view.lang)
                    .mock()
                    .tab_ops_stop_the_server_before_changing(),
                None,
                cx,
            )
        }))
        .child(
            card(cx)
                .p(px(14.0))
                .child(section_header(
                    crate::ui::Tr(view.lang).mock().tab_ops_server_settings(),
                    None,
                    cx,
                ))
                .child(setting_row(
                    crate::ui::Tr(view.lang).mock().modals_server_name(),
                    None,
                    None,
                    div().w(px(260.0)).child(text_input(&view.srv_name_input)),
                    cx,
                ))
                .child(setting_row(
                    crate::ui::Tr(view.lang).mock().modals_listen_port_127_0_0(),
                    running.then(|| {
                        SharedString::from(
                            crate::ui::Tr(view.lang)
                                .mock()
                                .tab_ops_locked_while_running(),
                        )
                    }),
                    None,
                    h_row()
                        .gap(px(6.0))
                        .child(number_field(&view.srv_port_input, None, 100.0, cx))
                        .child(
                            btn_ghost(
                                "mock-suggest-port",
                                crate::ui::Tr(view.lang).mock().tab_ops_find_free(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.suggest_free_port(cx))),
                        ),
                    cx,
                ))
                .child(setting_row(
                    crate::ui::Tr(view.lang).mock().tab_ops_endpoint_prefix(),
                    None,
                    Some(SharedString::from(
                        crate::ui::Tr(view.lang).mock().tab_ops_e_g_api_v1_blank(),
                    )),
                    div().w(px(220.0)).child(text_input(&view.srv_prefix_input)),
                    cx,
                ))
                .child(setting_row(
                    crate::ui::Tr(view.lang).mock().tab_ops_default_latency(),
                    None,
                    None,
                    number_field(&view.srv_latency_input, Some("ms"), 100.0, cx),
                    cx,
                ))
                .child(setting_row(
                    crate::ui::Tr(view.lang).mock().tab_ops_bound_environment(),
                    None,
                    Some(SharedString::from(
                        crate::ui::Tr(view.lang)
                            .mock()
                            .tab_ops_workspace_environment_used_when_resolving(),
                    )),
                    env_chips,
                    cx,
                )),
        )
        .child(
            card(cx)
                .p(px(14.0))
                .child(section_header(
                    crate::ui::Tr(view.lang).mock().tab_ops_import_export(),
                    None,
                    cx,
                ))
                .child(setting_row(
                    crate::ui::Tr(view.lang)
                        .mock()
                        .tab_ops_export_pingdeck_json(),
                    None,
                    Some(SharedString::from(
                        crate::ui::Tr(view.lang)
                            .mock()
                            .tab_ops_strips_literal_credential_headers_by(),
                    )),
                    btn_secondary(
                        "mock-settings-export",
                        crate::ui::Tr(view.lang).mock().tab_ops_export(),
                    )
                    .icon(icon_xs(UiIcon::Download))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.active_modal = Some(MockModal::ExportOptions {
                            include_credentials: false,
                        });
                        cx.notify();
                    })),
                    cx,
                )),
        );

    div()
        .id("mock-settings-scroll")
        .size_full()
        .overflow_y_scrollbar()
        .child(body)
        .into_any_element()
}
