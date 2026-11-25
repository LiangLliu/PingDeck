//! Gateway Access Control tab: autostart, port, token auth, LAN bind, extra allowed Host names,
//! request body capture mode, Host/Origin boundary card, and circuit reset.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::model::CaptureMode;

use crate::ui::{
    Tone, badge, btn_danger, btn_ghost, btn_secondary, card, field_row, h_row, palette,
    section_heading, tab_bar_segmented, tab_item, text_input, to_hsla, toggle_switch, v_col,
};

use super::ProvidersView;

pub fn render_access_tab(
    view: &ProvidersView,
    zh: bool,
    cx: &mut Context<ProvidersView>,
) -> gpui_kit::Div {
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
        .child(tab_item(if zh { "关闭" } else { "Off" }))
        .child(tab_item(if zh { "仅出错时" } else { "Errors only" }))
        .child(tab_item(if zh { "全部" } else { "All" }))
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

    let desc = |z: &'static str, e: &'static str| Some(SharedString::from(if zh { z } else { e }));

    let server_card = card(cx).p(px(16.0)).child(
        v_col()
            .gap(px(10.0))
            .child(
                h_row()
                    .justify_between()
                    .child(section_heading(
                        if zh { "服务与安全边界" } else { "Server & Security Boundary" },
                        cx,
                    ))
                    .child(badge(
                        if gw.serve_lan {
                            if zh { "局域网模式" } else { "LAN mode" }
                        } else if zh {
                            "严格本机模式"
                        } else {
                            "Loopback only"
                        },
                        if gw.serve_lan { Tone::Warning } else { Tone::Success },
                        cx,
                    )),
            )
            .child(field_row(
                if zh { "启动时自动开启网关" } else { "Start on app launch" },
                desc(
                    "PingDeck 打开时自动启动网关，客户端无需先切到服务商页即可使用。",
                    "Start the gateway automatically when PingDeck launches.",
                ),
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
                if zh { "监听端口" } else { "Listen port" },
                desc(
                    "修改端口后会重启网关；可用右侧按钮同步更新已接入客户端的配置文件。",
                    "Changing port restarts the gateway; rewrite client configs afterwards.",
                ),
                h_row()
                    .gap(px(6.0))
                    .child(div().w(px(90.0)).child(text_input(&view.gw_port_input)))
                    .child(
                        btn_secondary("gw-save-port", if zh { "保存并重启" } else { "Save & restart" })
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
                                            if zh {
                                                "请输入 1024–65535 之间的端口".into()
                                            } else {
                                                "Enter a port between 1024 and 65535".into()
                                            },
                                        ));
                                        cx.notify();
                                    }
                                }
                            })),
                    )
                    .child(
                        btn_ghost(
                            "gw-rewrite-addrs",
                            if zh { "同步客户端地址" } else { "Rewrite URLs" },
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.rewrite_all_client_addresses(zh, cx);
                        })),
                    ),
                cx,
            ))
            .child(field_row(
                if zh { "要求客户端令牌鉴权" } else { "Require client token" },
                desc(
                    "每次请求都必须携带有效客户端令牌；未知或已吊销令牌一律返回 401。",
                    "Every call requires a valid client token; invalid tokens always receive 401.",
                ),
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
                if zh { "在局域网提供服务 (0.0.0.0)" } else { "Serve on local network (LAN)" },
                desc(
                    "允许同局域网设备以明文 HTTP 访问网关（需要至少一个有效客户端令牌）。",
                    "Bind 0.0.0.0 for LAN devices (requires token auth and an active client).",
                ),
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
                    if zh {
                        format!("确认在局域网 ({cands}) 以明文 HTTP 暴露网关？仅限受信任局域网。")
                    } else {
                        format!("Expose gateway over plaintext HTTP on LAN ({cands})?")
                    },
                    Some(
                        h_row()
                            .gap(px(6.0))
                            .child(
                                btn_ghost("gw-lan-cancel", if zh { "取消" } else { "Cancel" })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm_lan_open = false;
                                        cx.notify();
                                    })),
                            )
                            .child(
                                btn_danger("gw-lan-confirm", if zh { "确认开启" } else { "Enable LAN" })
                                    .on_click(cx.listener(|this, _, _, cx| {
                                        this.confirm_lan_open = false;
                                        this.patch_gateway_setting(
                                            serde_json::json!({ "serve_lan": true }),
                                            true,
                                            cx,
                                        );
                                    })),
                            )
                            .into_any_element(),
                    ),
                    cx,
                )
            }))
            .child(field_row(
                if zh { "额外允许的 Host 名" } else { "Extra allowed Host names" },
                desc(
                    "逗号分隔，用于 Docker / devcontainer 或隧道（如 host.docker.internal），仍需令牌。",
                    "Comma-separated hostnames for Docker/tunnels (e.g. host.docker.internal).",
                ),
                h_row()
                    .gap(px(6.0))
                    .child(div().w(px(230.0)).child(text_input(&view.gw_extra_hosts_input)))
                    .child(
                        btn_secondary("gw-save-hosts", if zh { "保存" } else { "Save" }).on_click(
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
                if zh { "请求体调试捕获" } else { "Request body capture" },
                desc(
                    "决定哪些网关交互保留请求与响应体供日志排障。",
                    "Which gateway calls retain request/response bodies for debugging.",
                ),
                capture_seg,
                cx,
            )),
    );

    let mut hosts_col = v_col().gap(px(8.0)).child(section_heading(
        if zh {
            "当前接受的 Host 名称与运行控制"
        } else {
            "Accepted Host Names & Runtime"
        },
        cx,
    ));
    if let Some(ov) = &view.overview {
        let lan_hosts: Vec<&str> = ov.hosts.lan.iter().map(|a| a.host.as_str()).collect();
        hosts_col = hosts_col
            .child(field_row(
                if zh { "本机回环（始终接受）" } else { "Loopback (always accepted)" },
                None,
                div()
                    .text_xs()
                    .text_color(to_hsla(p.ink_2))
                    .child(SharedString::from(ov.hosts.loopback.join(", "))),
                cx,
            ))
            .child(field_row(
                if zh { "局域网地址" } else { "LAN addresses" },
                None,
                div().text_xs().text_color(to_hsla(p.muted)).child(
                    SharedString::from(if lan_hosts.is_empty() {
                        if zh { "未开启".into() } else { "Disabled".into() }
                    } else {
                        lan_hosts.join(", ")
                    }),
                ),
                cx,
            ))
            .child(div().text_xs().text_color(to_hsla(p.faint)).child(if zh {
                "防重绑定策略：不发送 CORS 头，所有携带 Origin 头的浏览器请求或未知 Host 一律返回 403。"
            } else {
                "Anti-rebinding guard: no CORS headers; requests with Origin or unknown Host always receive 403."
            }));
    }
    hosts_col = hosts_col.child(
        h_row()
            .pt(px(6.0))
            .justify_between()
            .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                "当上游故障恢复后，可手动清空所有服务商与模型的熔断器状态。"
            } else {
                "Manually clear all open circuit breakers after upstream recovery."
            }))
            .child(
                btn_danger(
                    "gw-reset-circuits",
                    if zh {
                        "重置全部熔断器"
                    } else {
                        "Reset all circuits"
                    },
                )
                .on_click(cx.listener(move |this, _, _, cx| this.reset_all_circuits(zh, cx))),
            ),
    );

    v_col()
        .w_full()
        .gap(px(14.0))
        .child(server_card)
        .child(card(cx).p(px(16.0)).child(hosts_col))
}
