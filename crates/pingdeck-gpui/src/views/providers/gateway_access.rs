//! Gateway Access Control tab: autostart, port, token auth, LAN bind, extra allowed Host names,
//! request body capture mode, Host/Origin boundary card, and circuit reset.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::model::CaptureMode;

use crate::ui::{
    Tone, badge, btn_danger, btn_ghost, btn_secondary, card, field_row, h_row, palette,
    section_heading, tab_bar_segmented, tab_item, text_input, to_hsla, toggle_switch, v_col,
};

use super::ProvidersView;

pub fn render_access_tab(view: &ProvidersView, cx: &mut Context<ProvidersView>) -> gpui_kit::Div {
    let p = palette(cx);
    let gw = &view.gateway_settings;
    let ent = cx.entity().clone();

    let cap_idx = match gw.capture_mode {
        CaptureMode::Off => 0,
        CaptureMode::ErrorsOnly => 1,
        CaptureMode::All => 2,
    };
    let capture_seg = tab_bar_segmented("gw-capture-mode")
        .selected_index(cap_idx)
        .child(tab_item(crate::ui::tr(cx).providers().bench_compare_off()))
        .child(tab_item(
            crate::ui::tr(cx)
                .providers()
                .gwa_opt_ai_gateway_capture_mode_errors_only(),
        ))
        .child(tab_item(crate::ui::tr(cx).providers().models_filter_all()))
        .on_click(move |idx, _, cx| {
            let mode = match *idx {
                1 => "errors_only",
                2 => "all",
                _ => "off",
            };
            ent.update(cx, |this, cx| {
                this.patch_gateway_setting(serde_json::json!({ "capture_mode": mode }), false, cx);
            });
        });

    let pr = crate::ui::tr(cx).providers();
    let desc = |s: &'static str| Some(SharedString::from(s));

    let server_card = card(cx).p(px(16.0)).child(
        v_col()
            .gap(px(10.0))
            .child(
                h_row()
                    .justify_between()
                    .child(section_heading(
                        pr.prov_gateway_access_server_security_boundary(),
                        cx,
                    ))
                    .child(badge(
                        if gw.serve_lan {
                            pr.prov_gateway_access_lan_mode()
                        } else {
                            pr.prov_gateway_access_loopback_only()
                        },
                        if gw.serve_lan {
                            Tone::Warning
                        } else {
                            Tone::Success
                        },
                        cx,
                    )),
            )
            .child(field_row(
                pr.prov_gateway_access_start_on_app_launch(),
                desc(pr.prov_gwa_desc_autostart()),
                toggle_switch("gw-opt-autostart", gw.enabled_on_start).on_click(cx.listener(
                    |this, _, _, cx| {
                        let next = !this.gateway_settings.enabled_on_start;
                        this.patch_gateway_setting(
                            serde_json::json!({ "enabled_on_start": next }),
                            false,
                            cx,
                        );
                    },
                )),
                cx,
            ))
            .child(field_row(
                pr.prov_gateway_access_listen_port(),
                desc(pr.prov_gwa_desc_port()),
                h_row()
                    .gap(px(6.0))
                    .child(div().w(px(90.0)).child(text_input(&view.gw_port_input)))
                    .child(
                        btn_secondary("gw-save-port", pr.prov_gateway_access_save_restart())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                let raw = this.gw_port_input.read(cx).value().trim().to_owned();
                                match raw.parse::<u16>() {
                                    Ok(port) if port >= 1024 => {
                                        this.patch_gateway_setting(
                                            serde_json::json!({ "port": port }),
                                            true,
                                            cx,
                                        );
                                    }
                                    _ => {
                                        this.status_msg = Some((
                                            Tone::Danger,
                                            crate::ui::tr(cx)
                                                .providers()
                                                .prov_gateway_access_enter_a_port_between_1024()
                                                .into(),
                                        ));
                                        cx.notify();
                                    }
                                }
                            })),
                    )
                    .child(
                        btn_ghost("gw-rewrite-addrs", pr.prov_gateway_access_rewrite_urls())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.rewrite_all_client_addresses(cx);
                            })),
                    ),
                cx,
            ))
            .child(field_row(
                pr.prov_gateway_access_require_client_token(),
                desc(pr.prov_gwa_desc_require_token()),
                toggle_switch("gw-opt-req-token", gw.require_token).on_click(cx.listener(
                    |this, _, _, cx| {
                        let next = !this.gateway_settings.require_token;
                        this.patch_gateway_setting(
                            serde_json::json!({ "require_token": next }),
                            true,
                            cx,
                        );
                    },
                )),
                cx,
            ))
            .child(field_row(
                pr.prov_gateway_access_serve_on_local_network_lan(),
                desc(pr.prov_gwa_desc_serve_lan()),
                toggle_switch("gw-opt-lan", gw.serve_lan).on_click(cx.listener(
                    |this, _, _, cx| {
                        if !this.gateway_settings.serve_lan {
                            this.confirm_lan_open = true;
                            cx.notify();
                        } else {
                            this.confirm_lan_open = false;
                            this.patch_gateway_setting(
                                serde_json::json!({ "serve_lan": false }),
                                true,
                                cx,
                            );
                        }
                    },
                )),
                cx,
            ))
            .children(view.confirm_lan_open.then(|| {
                let cands = view
                    .overview
                    .as_ref()
                    .map(|o| {
                        o.hosts
                            .lan_candidates
                            .iter()
                            .map(|a| a.host.as_str())
                            .collect::<Vec<_>>()
                            .join(", ")
                    })
                    .filter(|s| !s.is_empty())
                    .unwrap_or_else(|| "0.0.0.0".to_owned());
                crate::ui::alert_banner(
                    Tone::Warning,
                    crate::ui::tr(cx)
                        .providers()
                        .prov_gateway_access_expose_gateway_over_plaintext_http(cands),
                    Some(
                        h_row()
                            .gap(px(6.0))
                            .child(
                                btn_ghost("gw-lan-cancel", crate::ui::tr(cx).common().cancel())
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm_lan_open = false;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                btn_danger(
                                    "gw-lan-confirm",
                                    crate::ui::tr(cx)
                                        .providers()
                                        .prov_gateway_access_enable_lan(),
                                )
                                .on_click(cx.listener(
                                    |this, _, _, cx| {
                                        this.confirm_lan_open = false;
                                        this.patch_gateway_setting(
                                            serde_json::json!({ "serve_lan": true }),
                                            true,
                                            cx,
                                        );
                                    },
                                )),
                            )
                            .into_any_element(),
                    ),
                    cx,
                )
            }))
            .child(field_row(
                pr.prov_gateway_access_extra_allowed_host_names(),
                desc(pr.prov_gwa_desc_extra_hosts()),
                h_row()
                    .gap(px(6.0))
                    .child(
                        div()
                            .w(px(230.0))
                            .child(text_input(&view.gw_extra_hosts_input)),
                    )
                    .child(
                        btn_secondary("gw-save-hosts", crate::ui::tr(cx).common().save()).on_click(
                            cx.listener(|this, _, _, cx| {
                                let raw = this.gw_extra_hosts_input.read(cx).value().to_string();
                                let hosts: Vec<String> = raw
                                    .split([',', '\n', ' '])
                                    .map(str::trim)
                                    .filter(|s| !s.is_empty())
                                    .map(str::to_owned)
                                    .collect();
                                this.patch_gateway_setting(
                                    serde_json::json!({ "extra_allowed_hosts": hosts }),
                                    true,
                                    cx,
                                );
                            }),
                        ),
                    ),
                cx,
            ))
            .child(field_row(
                pr.prov_gateway_access_request_body_capture(),
                desc(pr.prov_gwa_desc_capture_mode()),
                capture_seg,
                cx,
            )),
    );

    let mut hosts_col = v_col().gap(px(8.0)).child(section_heading(
        crate::ui::tr(cx)
            .providers()
            .prov_gateway_access_accepted_host_names_runtime(),
        cx,
    ));
    if let Some(ov) = &view.overview {
        let lan_hosts: Vec<&str> = ov.hosts.lan.iter().map(|a| a.host.as_str()).collect();
        hosts_col = hosts_col
            .child(field_row(
                crate::ui::tr(cx)
                    .providers()
                    .prov_gateway_access_loopback_always_accepted(),
                None,
                div()
                    .text_xs()
                    .text_color(to_hsla(p.ink_2))
                    .child(SharedString::from(ov.hosts.loopback.join(", "))),
                cx,
            ))
            .child(field_row(
                crate::ui::tr(cx)
                    .providers()
                    .prov_gateway_access_lan_addresses(),
                None,
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(SharedString::from(if lan_hosts.is_empty() {
                        crate::ui::tr(cx)
                            .providers()
                            .prov_gateway_access_disabled()
                            .into()
                    } else {
                        lan_hosts.join(", ")
                    })),
                cx,
            ))
            .child(
                div().text_xs().text_color(to_hsla(p.faint)).child(
                    crate::ui::tr(cx)
                        .providers()
                        .prov_gateway_access_anti_rebinding_guard_no_cors(),
                ),
            );
    }
    hosts_col = hosts_col.child(
        h_row()
            .pt(px(6.0))
            .justify_between()
            .child(
                div().text_xs().text_color(to_hsla(p.muted)).child(
                    crate::ui::tr(cx)
                        .providers()
                        .prov_gateway_access_manually_clear_all_open_circuit(),
                ),
            )
            .child(
                btn_danger(
                    "gw-reset-circuits",
                    crate::ui::tr(cx)
                        .providers()
                        .prov_gateway_access_reset_all_circuits(),
                )
                .on_click(cx.listener(move |this, _, _, cx| this.reset_all_circuits(cx))),
            ),
    );

    v_col()
        .w_full()
        .gap(px(14.0))
        .child(server_card)
        .child(card(cx).p(px(16.0)).child(hosts_col))
}
