//! Settings › Mock page: default start port (with gateway collision notice), response latency,
//! default CORS, ring-buffer log capacity, and extra Host-guard allowlist.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::model::settings::SettingsSection as CoreSection;

use crate::ui::{
    Tone, UiIcon, alert_banner, btn_ghost, btn_icon, btn_secondary, h_row, icon_xs, number_field,
    palette, setting_row, settings_section, settings_section_padded, text_input, to_hsla,
    toggle_switch, v_col,
};

use super::SettingsView;

pub fn render_mock(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let p = palette(cx);
    let mock = &view.settings.mock;
    let gw_port = view.settings.ai.gateway.port;

    let defaults_rows = v_col()
        .w_full()
        .child(setting_row(
            crate::ui::tr(cx)
                .settings()
                .page_mock_default_starting_port(),
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_mock_ai_gateway_port(gw_port),
            )),
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_mock_first_port_tried_for_new(),
            )),
            h_row()
                .gap(px(6.0))
                .child(number_field(&view.inputs.mock_first_port, None, 88.0, cx))
                .child(
                    btn_secondary("mock-save-port", crate::ui::tr(cx).common().kv_bulk_apply())
                        .on_click(cx.listener(|this, _, _, cx| {
                            let v = this.inputs.mock_first_port.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                1024,
                                65_535,
                                CoreSection::Mock,
                                |n| serde_json::json!({ "first_port": n }),
                                cx,
                            );
                        })),
                ),
            cx,
        ))
        .child(setting_row(
            crate::ui::tr(cx).settings().page_mock_default_latency(),
            None,
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_mock_artificial_delay_added_before_responses(),
            )),
            h_row()
                .gap(px(6.0))
                .child(number_field(
                    &view.inputs.mock_latency_ms,
                    Some("ms"),
                    96.0,
                    cx,
                ))
                .child(
                    btn_secondary(
                        "mock-save-latency",
                        crate::ui::tr(cx).common().kv_bulk_apply(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        let v = this.inputs.mock_latency_ms.read(cx).value().to_string();
                        this.commit_u64(
                            &v,
                            0,
                            60_000,
                            CoreSection::Mock,
                            |n| serde_json::json!({ "default_latency_ms": n }),
                            cx,
                        );
                    })),
                ),
            cx,
        ))
        .child(setting_row(
            crate::ui::tr(cx)
                .settings()
                .page_mock_enable_cors_headers_by_default(),
            None,
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_mock_new_mock_servers_respond_to(),
            )),
            toggle_switch("mock-cors", mock.cors_default).on_change(cx.listener(
                |this, checked: &bool, _, cx| {
                    this.patch_section(
                        CoreSection::Mock,
                        serde_json::json!({ "cors_default": *checked }),
                        cx,
                    );
                },
            )),
            cx,
        ))
        .child(setting_row(
            crate::ui::tr(cx)
                .settings()
                .page_mock_in_memory_log_capacity(),
            None,
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_mock_transactions_kept_in_each_running(),
            )),
            h_row()
                .gap(px(6.0))
                .child(number_field(
                    &view.inputs.mock_log_capacity,
                    Some(crate::ui::tr(cx).settings().page_advanced_rows()),
                    92.0,
                    cx,
                ))
                .child(
                    btn_secondary(
                        "mock-save-logcap",
                        crate::ui::tr(cx).common().kv_bulk_apply(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        let v = this.inputs.mock_log_capacity.read(cx).value().to_string();
                        this.commit_u64(
                            &v,
                            10,
                            5_000,
                            CoreSection::Mock,
                            |n| serde_json::json!({ "log_capacity": n }),
                            cx,
                        );
                    })),
                ),
            cx,
        ));

    let mut defaults_sec = v_col().w_full().gap(px(10.0)).child(settings_section(
        crate::ui::tr(cx)
            .settings()
            .page_mock_mock_server_defaults(),
        Some(
            btn_ghost(
                "mock-reset-all",
                crate::ui::tr(cx).settings().page_defaults_reset(),
            )
            .icon(icon_xs(UiIcon::RotateCcw))
            .on_click(cx.listener(|this, _, _, cx| {
                this.reset_section(CoreSection::Mock, cx);
            }))
            .into_any_element(),
        ),
        defaults_rows,
        cx,
    ));

    if mock.first_port == gw_port {
        defaults_sec = defaults_sec.child(alert_banner(
            Tone::Warning,
            crate::ui::tr(cx)
                .settings()
                .page_mock_mock_starting_port_matches_the(gw_port),
            None,
            cx,
        ));
    }

    let mut chips = h_row().flex_wrap().gap(px(6.0));
    for (idx, host) in mock.extra_allowed_hosts.iter().enumerate() {
        chips = chips.child(
            h_row()
                .gap(px(4.0))
                .pl(px(8.0))
                .pr(px(3.0))
                .py(px(2.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.sunken))
                .border_1()
                .border_color(to_hsla(p.line_strong.composite_over(p.sunken)))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.ink))
                        .child(host.clone()),
                )
                .child(
                    btn_icon(
                        SharedString::from(format!("mock-host-rm-{idx}")),
                        UiIcon::Close,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.remove_mock_host(idx, cx);
                    })),
                ),
        );
    }

    let hosts_sec = settings_section_padded(
        crate::ui::tr(cx)
            .settings()
            .page_mock_extra_allowed_host_headers(),
        None,
        v_col()
            .w_full()
            .gap(px(10.0))
            .child(
                div().text_xs().text_color(to_hsla(p.muted)).child(
                    crate::ui::tr(cx)
                        .settings()
                        .page_mock_mock_listeners_always_bind_loopback(),
                ),
            )
            .children((!mock.extra_allowed_hosts.is_empty()).then_some(chips))
            .child(
                h_row()
                    .w_full()
                    .gap(px(8.0))
                    .child(div().flex_1().child(text_input(&view.inputs.mock_host_add)))
                    .child(
                        btn_secondary(
                            "mock-host-add-btn",
                            crate::ui::tr(cx).settings().network_no_proxy_add(),
                        )
                        .icon(icon_xs(UiIcon::Plus))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.add_mock_host(window, cx);
                        })),
                    ),
            ),
        cx,
    );

    v_col()
        .w_full()
        .gap(px(20.0))
        .child(defaults_sec)
        .child(hosts_sec)
}
