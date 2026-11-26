//! Gateway Access Control tab matching `docs/design/prototype/js/data-subtabs.js`:
//! inset-grouped `settings_section` + `setting_row` boundary card (port, autostart, token auth,
//! LAN bind, extra allowed Hosts, capture mode) and runtime Host / circuit controls card.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::model::CaptureMode;

use crate::ui::{
    Tone, alert_banner, badge, btn_danger, btn_ghost, btn_secondary, h_row, palette, setting_row,
    settings_section, tab_bar_segmented, tab_item, text_input, to_hsla, toggle_switch, v_col,
};

use super::GatewayView;

pub fn render_access_tab(view: &GatewayView, cx: &mut Context<GatewayView>) -> gpui_kit::Div {
    let p = palette(cx);
    let gw = &view.gateway_settings;
    let ent = cx.entity().clone();
    let pr = crate::ui::tr(cx).providers();
    let cm = crate::ui::tr(cx).common();

    let cap_idx = match gw.capture_mode {
        CaptureMode::Off => 0,
        CaptureMode::ErrorsOnly => 1,
        CaptureMode::All => 2,
    };
    let capture_seg = tab_bar_segmented("gw-capture-mode")
        .selected_index(cap_idx)
        .child(tab_item(pr.bench_compare_off()))
        .child(tab_item(pr.gwa_opt_ai_gateway_capture_mode_errors_only()))
        .child(tab_item(pr.models_filter_all()))
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

    let desc = |s: &'static str| Some(SharedString::from(s));
    let mode_badge = badge(
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
    )
    .into_any_element();

    let lan_confirm = view.confirm_lan_open.then(|| {
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
        div().py(px(8.0)).child(alert_banner(
            Tone::Warning,
            pr.prov_gateway_access_expose_gateway_over_plaintext_http(cands),
            Some(
                h_row()
                    .gap(px(6.0))
                    .child(
                        btn_ghost("gw-lan-cancel", cm.cancel()).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.confirm_lan_open = false;
                                cx.notify();
                            },
                        )),
                    )
                    .child(
                        btn_danger("gw-lan-confirm", pr.prov_gateway_access_enable_lan()).on_click(
                            cx.listener(|this, _, _, cx| {
                                this.confirm_lan_open = false;
                                this.patch_gateway_setting(
                                    serde_json::json!({ "serve_lan": true }),
                                    true,
                                    cx,
                                );
                            }),
                        ),
                    )
                    .into_any_element(),
            ),
            cx,
        ))
    });

    let boundary_rows = v_col()
        .w_full()
        .child(setting_row(
            pr.prov_gateway_access_listen_port(),
            Some(SharedString::from(format!(
                "{}:{}",
                if gw.serve_lan { "0.0.0.0" } else { "127.0.0.1" },
                gw.port
            ))),
            desc(pr.prov_gwa_desc_port()),
            h_row()
                .gap(px(6.0))
                .child(div().w(px(84.0)).child(text_input(&view.gw_port_input)))
                .child(
                    btn_secondary("gw-save-port", pr.prov_gateway_access_save_restart()).on_click(
                        cx.listener(|this, _, _, cx| {
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
                        }),
                    ),
                )
                .child(
                    btn_ghost("gw-rewrite-addrs", pr.prov_gateway_access_rewrite_urls()).on_click(
                        cx.listener(|this, _, _, cx| this.rewrite_all_client_addresses(cx)),
                    ),
                ),
            cx,
        ))
        .child(setting_row(
            pr.prov_gateway_access_require_client_token(),
            None,
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
        .child(setting_row(
            pr.prov_gateway_access_serve_on_local_network_lan(),
            None,
            desc(pr.prov_gwa_desc_serve_lan()),
            toggle_switch("gw-opt-lan", gw.serve_lan).on_click(cx.listener(|this, _, _, cx| {
                if !this.gateway_settings.serve_lan {
                    this.confirm_lan_open = true;
                    cx.notify();
                } else {
                    this.confirm_lan_open = false;
                    this.patch_gateway_setting(serde_json::json!({ "serve_lan": false }), true, cx);
                }
            })),
            cx,
        ))
        .children(lan_confirm)
        .child(setting_row(
            pr.prov_gateway_access_extra_allowed_host_names(),
            None,
            desc(pr.prov_gwa_desc_extra_hosts()),
            h_row()
                .gap(px(6.0))
                .child(
                    div()
                        .w(px(220.0))
                        .child(text_input(&view.gw_extra_hosts_input)),
                )
                .child(
                    btn_secondary("gw-save-hosts", cm.save()).on_click(cx.listener(
                        |this, _, _, cx| {
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
                        },
                    )),
                ),
            cx,
        ))
        .child(setting_row(
            pr.prov_gateway_access_start_on_app_launch(),
            None,
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
        .child(setting_row(
            pr.prov_gateway_access_request_body_capture(),
            None,
            desc(pr.prov_gwa_desc_capture_mode()),
            capture_seg,
            cx,
        ));

    let mut runtime_rows = v_col().w_full();
    if let Some(ov) = &view.overview {
        let lan_hosts: Vec<&str> = ov.hosts.lan.iter().map(|a| a.host.as_str()).collect();
        runtime_rows = runtime_rows
            .child(setting_row(
                pr.prov_gateway_access_loopback_always_accepted(),
                None,
                desc(pr.prov_gateway_access_anti_rebinding_guard_no_cors()),
                div()
                    .text_xs()
                    .text_color(to_hsla(p.ink_2))
                    .child(SharedString::from(ov.hosts.loopback.join(", "))),
                cx,
            ))
            .child(setting_row(
                pr.prov_gateway_access_lan_addresses(),
                None,
                None,
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(SharedString::from(if lan_hosts.is_empty() {
                        pr.prov_gateway_access_disabled().into()
                    } else {
                        lan_hosts.join(", ")
                    })),
                cx,
            ));
    }
    runtime_rows = runtime_rows.child(setting_row(
        pr.prov_gateway_access_reset_all_circuits(),
        None,
        desc(pr.prov_gateway_access_manually_clear_all_open_circuit()),
        btn_danger(
            "gw-reset-circuits",
            pr.prov_gateway_access_reset_all_circuits(),
        )
        .on_click(cx.listener(|this, _, _, cx| this.reset_all_circuits(cx))),
        cx,
    ));

    v_col()
        .w_full()
        .gap(px(16.0))
        .child(settings_section(
            pr.prov_gateway_access_server_security_boundary(),
            Some(mode_badge),
            boundary_rows,
            cx,
        ))
        .child(settings_section(
            pr.prov_gateway_access_accepted_host_names_runtime(),
            None,
            runtime_rows,
            cx,
        ))
}
