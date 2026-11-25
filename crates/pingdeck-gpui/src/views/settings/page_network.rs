//! Settings › Network page: system/custom/direct proxy with live connectivity testing,
//! bypass host list (`no_proxy`), custom PEM CA bundle inspection, and mTLS Environments jump.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::ProxyTestOutcome;
use pingdeck_desktop::core::model::settings::SettingsSection as CoreSection;
use pingdeck_desktop::state::NavModule;

use crate::ui::{
    Disableable as _, Tone, UiIcon, alert_banner, btn_ghost, btn_icon, btn_primary, btn_secondary,
    h_row, icon_xs, palette, setting_row, settings_section, settings_section_padded,
    settings_sub_well, spinner_xs, tab_bar_segmented, tab_item, text_input, to_hsla, v_col,
};

use super::SettingsView;
use super::state_inputs::ProxyModeChoice;

pub fn render_network(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let p = palette(cx);
    let net = &view.settings.network;
    let mode_idx = match view.proxy_mode {
        ProxyModeChoice::System => 0,
        ProxyModeChoice::Custom => 1,
        ProxyModeChoice::Direct => 2,
    };
    let ent_mode = cx.entity().clone();

    let mut proxy_rows = v_col().w_full().child(setting_row(
        crate::ui::tr(cx).settings().page_network_proxy_mode(),
        None,
        Some(SharedString::from(
            crate::ui::tr(cx)
                .settings()
                .page_network_applies_to_http_requests_ai(),
        )),
        h_row()
            .gap(px(8.0))
            .child(
                tab_bar_segmented("net-proxy-mode")
                    .selected_index(mode_idx)
                    .child(tab_item(crate::ui::tr(cx).settings().appearance_system()))
                    .child(tab_item(crate::ui::tr(cx).settings().network_mode_custom()))
                    .child(tab_item(crate::ui::tr(cx).settings().page_network_direct()))
                    .on_click(move |idx, _, cx| {
                        let mode = match *idx {
                            1 => ProxyModeChoice::Custom,
                            2 => ProxyModeChoice::Direct,
                            _ => ProxyModeChoice::System,
                        };
                        ent_mode.update(cx, |this, cx| this.select_proxy_mode(mode, cx));
                    }),
            )
            .child(
                btn_secondary(
                    "net-test-proxy",
                    crate::ui::tr(cx).settings().page_network_test(),
                )
                .disabled(view.proxy_testing)
                .on_click(cx.listener(|this, _, _, cx| this.test_proxy_connection(cx))),
            ),
        cx,
    ));

    if view.proxy_mode == ProxyModeChoice::Custom {
        proxy_rows = proxy_rows.child(
            div()
                .w_full()
                .py(px(12.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(
                    settings_sub_well(cx)
                        .child(
                            h_row()
                                .w_full()
                                .gap(px(8.0))
                                .child(div().flex_1().child(text_input(&view.inputs.proxy_url)))
                                .child(
                                    btn_primary(
                                        "net-save-proxy",
                                        crate::ui::tr(cx).settings().page_network_save_proxy(),
                                    )
                                    .on_click(
                                        cx.listener(|this, _, _, cx| this.save_custom_proxy(cx)),
                                    ),
                                ),
                        )
                        .child(
                            div().text_xs().text_color(to_hsla(p.muted)).child(
                                crate::ui::tr(cx)
                                    .settings()
                                    .page_network_supports_http_https_socks5_socks5h(),
                            ),
                        ),
                ),
        );
    }

    if view.proxy_testing {
        proxy_rows = proxy_rows.child(
            h_row()
                .gap(px(8.0))
                .py(px(10.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(spinner_xs())
                .child(
                    div().text_xs().text_color(to_hsla(p.muted)).child(
                        crate::ui::tr(cx)
                            .settings()
                            .page_network_testing_connection_to_api_openai(),
                    ),
                ),
        );
    } else if let Some(res) = &view.proxy_test_result {
        let (tone, text) = match res.outcome {
            ProxyTestOutcome::Connected => (
                Tone::Success,
                crate::ui::tr(cx)
                    .settings()
                    .page_network_connected_to_via_route_in(
                        &res.target,
                        &res.route,
                        res.elapsed_ms,
                    ),
            ),
            ProxyTestOutcome::ProxyUnreachable => (
                Tone::Danger,
                crate::ui::tr(cx)
                    .settings()
                    .page_network_proxy_server_unreachable_ms(res.elapsed_ms),
            ),
            ProxyTestOutcome::ProxyAuth => (
                Tone::Warning,
                crate::ui::tr(cx)
                    .settings()
                    .page_network_proxy_requires_authentication_http_407()
                    .to_owned(),
            ),
            ProxyTestOutcome::ProxyRefused => (
                Tone::Danger,
                crate::ui::tr(cx)
                    .settings()
                    .page_network_proxy_refused_tunnel_connection()
                    .to_owned(),
            ),
            ProxyTestOutcome::TargetNoResponse => (
                Tone::Warning,
                crate::ui::tr(cx)
                    .settings()
                    .page_network_route_ok_but_did_not(
                        &res.route,
                        &res.target,
                        res.detail.as_deref().unwrap_or("timeout"),
                        res.elapsed_ms,
                    ),
            ),
        };
        proxy_rows = proxy_rows.child(
            div()
                .py(px(10.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(alert_banner(tone, text, None, cx)),
        );
    }

    // no_proxy chips + add bar
    let mut chips = h_row().flex_wrap().gap(px(6.0));
    for (idx, host) in net.no_proxy.iter().enumerate() {
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
                        SharedString::from(format!("noproxy-rm-{idx}")),
                        UiIcon::Close,
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.remove_no_proxy_entry(idx, cx);
                    })),
                ),
        );
    }

    let no_proxy_row = v_col()
        .w_full()
        .gap(px(8.0))
        .py(px(12.5))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            v_col()
                .gap(px(2.5))
                .child(
                    div()
                        .text_size(px(13.5))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .child(
                            crate::ui::tr(cx)
                                .settings()
                                .page_network_bypass_proxy_for_hosts_no(),
                        ),
                )
                .child(
                    div().text_xs().text_color(to_hsla(p.muted)).child(
                        crate::ui::tr(cx)
                            .settings()
                            .page_network_matching_hosts_connect_directly_supports(),
                    ),
                ),
        )
        .children((!net.no_proxy.is_empty()).then_some(chips))
        .child(
            h_row()
                .w_full()
                .gap(px(8.0))
                .child(div().flex_1().child(text_input(&view.inputs.no_proxy_add)))
                .child(
                    btn_secondary(
                        "noproxy-add-btn",
                        crate::ui::tr(cx).settings().page_network_add(),
                    )
                    .icon(icon_xs(UiIcon::Plus))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.add_no_proxy_entry(window, cx);
                    })),
                ),
        );

    let proxy_sec = settings_section(
        crate::ui::tr(cx).settings().page_network_outbound_proxy(),
        Some(
            btn_ghost(
                "net-reset-proxy",
                crate::ui::tr(cx).settings().page_defaults_reset(),
            )
            .icon(icon_xs(UiIcon::RotateCcw))
            .on_click(cx.listener(|this, _, _, cx| {
                this.proxy_test_result = None;
                this.reset_keys(
                    CoreSection::Network,
                    Some(vec!["proxy_url".into(), "no_proxy".into()]),
                    cx,
                );
            }))
            .into_any_element(),
        ),
        proxy_rows.child(no_proxy_row),
        cx,
    );

    // Custom CA bundle
    let mut ca_body = v_col()
        .w_full()
        .gap(px(10.0))
        .child(
            div().text_xs().text_color(to_hsla(p.muted)).child(
                crate::ui::tr(cx)
                    .settings()
                    .page_network_trust_extra_corporate_or_self(),
            ),
        )
        .child(
            h_row()
                .w_full()
                .gap(px(8.0))
                .child(
                    div()
                        .flex_1()
                        .child(text_input(&view.inputs.ca_bundle_path)),
                )
                .child(
                    btn_secondary(
                        "net-pick-ca",
                        crate::ui::tr(cx).settings().page_network_browse(),
                    )
                    .icon(icon_xs(UiIcon::FolderOpen))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.pick_ca_bundle(window, cx);
                    })),
                )
                .child(
                    btn_secondary("net-save-ca", crate::ui::tr(cx).common().save())
                        .on_click(cx.listener(|this, _, _, cx| this.save_ca_bundle(cx))),
                )
                .child(
                    btn_secondary(
                        "net-test-ca",
                        crate::ui::tr(cx).settings().page_network_verify(),
                    )
                    .disabled(view.ca_testing)
                    .on_click(cx.listener(|this, _, _, cx| this.test_ca_bundle(cx))),
                ),
        );

    if let Some(info) = &view.ca_test_result {
        let (tone, msg) = if info.ok {
            (
                Tone::Success,
                crate::ui::tr(cx)
                    .settings()
                    .page_network_valid_ca_bundle_usable_root(info.certificates),
            )
        } else {
            (
                Tone::Danger,
                info.error.clone().unwrap_or_else(|| {
                    crate::ui::tr(cx)
                        .settings()
                        .page_network_invalid_ca_bundle()
                        .to_owned()
                }),
            )
        };
        ca_body = ca_body.child(alert_banner(tone, msg, None, cx));
    }

    let ca_sec = settings_section_padded(
        crate::ui::tr(cx)
            .settings()
            .page_network_extra_trusted_ca_bundle(),
        Some(
            btn_ghost(
                "net-reset-ca",
                crate::ui::tr(cx).settings().page_defaults_clear(),
            )
            .disabled(net.ca_bundle_path.is_empty())
            .on_click(cx.listener(|this, _, window, cx| this.clear_ca_bundle(window, cx)))
            .into_any_element(),
        ),
        ca_body,
        cx,
    );

    // mTLS client certificates cross-link card
    let mtls_card = settings_section(
        crate::ui::tr(cx).settings().page_network_mutual_tls_mtls(),
        None,
        setting_row(
            crate::ui::tr(cx)
                .settings()
                .page_network_client_certificates_mtls_are_workspace(),
            None,
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .settings()
                    .page_network_configure_per_host_client_certificates(),
            )),
            btn_secondary(
                "net-jump-envs",
                crate::ui::tr(cx)
                    .settings()
                    .page_network_open_environments(),
            )
            .icon(icon_xs(UiIcon::ExternalLink))
            .on_click(cx.listener(|this, _, _, cx| {
                this.jump_to_module(NavModule::Environments, cx);
            })),
            cx,
        ),
        cx,
    );

    v_col()
        .w_full()
        .gap(px(20.0))
        .child(proxy_sec)
        .child(ca_sec)
        .child(mtls_card)
}
