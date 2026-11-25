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
            view.t("暂无 Webhook 回调", "No callbacks yet"),
            Some(SharedString::from(view.t(
                "可在响应分支上绑定回调，在返回响应后异步发送出站请求。",
                "Attach callbacks to response branches to trigger async webhooks.",
            ))),
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
                        .child(view.t("回调列表", "Callbacks")),
                )
                .child(
                    btn_secondary("mock-add-cb", view.t("+ 新建", "+ Add"))
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
                        view.t("回调配置", "Callback Configuration"),
                        Some(
                            btn_secondary("mock-cb-test-now", view.t("立即测试", "Test Now"))
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
                view.t("回调请求头", "Callback Headers"),
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
            view.t("选择或新建回调", "Select or create a callback"),
            view.t(
                "配置出站 Webhook 请求，供响应分支异步触发。",
                "Configure outbound webhook requests triggered by mock responses.",
            ),
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
            .child(section_header(view.t("透明代理回源", "Transparent Proxy"), None, cx))
            .child(setting_row(
                view.t("未命中本地路由时转发到上游", "Proxy unmatched requests upstream"),
                None,
                Some(SharedString::from(view.t(
                    "开启后未匹配路由的请求会透传至目标服务器，并可录制为本地路由。",
                    "Forwards unmatched requests to the upstream URL and supports recording them into routes.",
                ))),
                toggle_switch("mock-proxy-en", srv.proxy_enabled).on_click(cx.listener(|this, _, _, cx| {
                    if let Some(s) = this.draft.as_mut() {
                        s.proxy_enabled = !s.proxy_enabled;
                        this.dirty = true;
                        cx.notify();
                    }
                })),
                cx,
            ))
            .child(setting_row(
                view.t("上游基础 URL", "Upstream Base URL"),
                None,
                None,
                div().w(px(340.0)).child(text_input(&view.proxy_url_input)),
                cx,
            ))
            .child(setting_row(
                view.t("转发时剥离服务前缀", "Strip endpoint prefix when forwarding"),
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
            if view.is_zh() {
                format!(
                    "代理录制{} · 当前已捕获 {rec_count} 条回源交换",
                    if recording { "进行中" } else { "未开启" }
                )
            } else {
                format!(
                    "Proxy recording {} · {rec_count} exchange(s) captured",
                    if recording { "active" } else { "idle" }
                )
            },
            Some(
                h_row()
                    .gap(px(6.0))
                    .child(
                        btn_secondary(
                            "mock-proxy-rec-toggle",
                            if recording {
                                view.t("停止录制", "Stop Recording")
                            } else {
                                view.t("开始录制", "Start Recording")
                            },
                        )
                        .on_click(cx.listener(|this, _, _, cx| this.toggle_recording(cx))),
                    )
                    .children((rec_count > 0).then(|| {
                        btn_secondary("mock-proxy-rec-save", view.t("生成路由", "Save as Routes"))
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
            view.t("代理请求附加头", "Proxy Request Headers"),
            &srv.proxy_req_headers,
            KvListTarget::ProxyReq,
            cx,
        ))
        .child(render_kv_card(
            view,
            view.t("代理响应附加头", "Proxy Response Headers"),
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
            view.t("无绑定环境", "None"),
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
                view.t(
                    "服务运行期间修改端口需先停止服务；其余路由、响应与代理改动点击保存后立即热生效。",
                    "Stop the server before changing its port; other route/response edits hot-reload on save.",
                ),
                None,
                cx,
            )
        }))
        .child(
            card(cx)
                .p(px(14.0))
                .child(section_header(view.t("基础配置", "Server Settings"), None, cx))
                .child(setting_row(
                    view.t("服务名称", "Server Name"),
                    None,
                    None,
                    div().w(px(260.0)).child(text_input(&view.srv_name_input)),
                    cx,
                ))
                .child(setting_row(
                    view.t("监听端口（仅绑定 127.0.0.1）", "Listen Port (127.0.0.1 only)"),
                    running.then(|| SharedString::from(view.t("运行中锁定", "Locked while running"))),
                    None,
                    h_row()
                        .gap(px(6.0))
                        .child(number_field(&view.srv_port_input, None, 100.0, cx))
                        .child(
                            btn_ghost("mock-suggest-port", view.t("推荐端口", "Find Free")).on_click(
                                cx.listener(|this, _, _, cx| this.suggest_free_port(cx)),
                            ),
                        ),
                    cx,
                ))
                .child(setting_row(
                    view.t("端点前缀", "Endpoint Prefix"),
                    None,
                    Some(SharedString::from(view.t("例如 /api/v1，留空表示根路径。", "e.g. /api/v1; blank for root."))),
                    div().w(px(220.0)).child(text_input(&view.srv_prefix_input)),
                    cx,
                ))
                .child(setting_row(
                    view.t("全局默认延迟", "Default Latency"),
                    None,
                    None,
                    number_field(&view.srv_latency_input, Some("ms"), 100.0, cx),
                    cx,
                ))
                .child(setting_row(
                    view.t("模板绑定环境", "Bound Environment"),
                    None,
                    Some(SharedString::from(view.t(
                        "在响应模板中解析 {{var}} 时使用的工作区环境。",
                        "Workspace environment used when resolving {{var}} in response templates.",
                    ))),
                    env_chips,
                    cx,
                )),
        )
        .child(
            card(cx)
                .p(px(14.0))
                .child(section_header(view.t("导入与导出", "Import & Export"), None, cx))
                .child(setting_row(
                    view.t("导出为 PingDeck JSON", "Export PingDeck JSON"),
                    None,
                    Some(SharedString::from(view.t(
                        "默认剥离代理和回调中的明文凭据头，可在弹窗中选择是否保留。",
                        "Strips literal credential headers by default unless opted in.",
                    ))),
                    btn_secondary("mock-settings-export", view.t("导出…", "Export…"))
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
