//! Settings › Network page: system/custom/direct proxy with live connectivity testing,
//! bypass host list (`no_proxy`), custom PEM CA bundle inspection, and mTLS Environments jump.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::ProxyTestOutcome;
use pingdeck_desktop::core::model::settings::SettingsSection as CoreSection;
use pingdeck_desktop::state::NavModule;

use crate::ui::{
    Disableable as _, Tone, UiIcon, alert_banner, btn_ghost, btn_icon, btn_primary, btn_secondary,
    card, h_row, icon_xs, palette, section_header, setting_row, spinner_xs, tab_bar_segmented,
    tab_item, text_input, to_hsla, v_col,
};

use super::SettingsView;
use super::state_inputs::ProxyModeChoice;

pub fn render_network(
    view: &SettingsView,
    zh: bool,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let net = &view.settings.network;
    let mode_idx = match view.proxy_mode {
        ProxyModeChoice::System => 0,
        ProxyModeChoice::Custom => 1,
        ProxyModeChoice::Direct => 2,
    };
    let ent_mode = cx.entity().clone();

    let mut proxy_sec = v_col()
        .w_full()
        .gap(px(8.0))
        .child(section_header(
            if zh { "出站代理" } else { "Outbound Proxy" },
            Some(
                btn_ghost("net-reset-proxy", if zh { "恢复默认" } else { "Reset" })
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
            cx,
        ))
        .child(setting_row(
            if zh { "代理模式" } else { "Proxy mode" },
            None,
            Some(SharedString::from(if zh {
                "应用于 HTTP 请求、AI 服务商调用、模型目录与内置搜索（单条请求或服务商可单独覆写）。"
            } else {
                "Applies to HTTP requests, AI providers, catalog sync, and web search (per-request/provider overrides win)."
            })),
            h_row()
                .gap(px(8.0))
                .child(
                    tab_bar_segmented("net-proxy-mode")
                        .selected_index(mode_idx)
                        .child(tab_item(if zh { "跟随系统" } else { "System" }))
                        .child(tab_item(if zh { "自定义" } else { "Custom" }))
                        .child(tab_item(if zh { "直连" } else { "Direct" }))
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
                        if zh { "测试连接" } else { "Test" },
                    )
                    .disabled(view.proxy_testing)
                    .on_click(cx.listener(|this, _, _, cx| this.test_proxy_connection(cx))),
                ),
            cx,
        ));

    if view.proxy_mode == ProxyModeChoice::Custom {
        proxy_sec = proxy_sec.child(
            v_col()
                .w_full()
                .gap(px(6.0))
                .py(px(6.0))
                .child(
                    h_row()
                        .w_full()
                        .gap(px(8.0))
                        .child(div().flex_1().child(text_input(&view.inputs.proxy_url)))
                        .child(
                            btn_primary("net-save-proxy", if zh { "保存代理" } else { "Save proxy" })
                                .on_click(cx.listener(|this, _, _, cx| this.save_custom_proxy(cx))),
                        ),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(if zh {
                            "支持 http://、https://、socks5://、socks5h://。若含认证密码，密码单独保存在本地 0600 明文保险库中，界面与导出文件均以 **** 掩码显示。"
                        } else {
                            "Supports http://, https://, socks5://, socks5h://. Passwords are stored in the local 0600 plaintext vault and masked as **** in UI & exports."
                        }),
                ),
        );
    }

    if view.proxy_testing {
        proxy_sec = proxy_sec.child(h_row().gap(px(8.0)).py(px(4.0)).child(spinner_xs()).child(
            div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                "正在通过当前代理设置探测 api.openai.com…"
            } else {
                "Testing connection to api.openai.com via current proxy route…"
            }),
        ));
    } else if let Some(res) = &view.proxy_test_result {
        let (tone, text) = match res.outcome {
            ProxyTestOutcome::Connected => (
                Tone::Success,
                if zh {
                    format!(
                        "连接成功：经由 {} 路由访问 {}，耗时 {} ms",
                        res.route, res.target, res.elapsed_ms
                    )
                } else {
                    format!(
                        "Connected to {} via {} route in {} ms",
                        res.target, res.route, res.elapsed_ms
                    )
                },
            ),
            ProxyTestOutcome::ProxyUnreachable => (
                Tone::Danger,
                if zh {
                    format!("无法连接到代理服务器（耗时 {} ms）", res.elapsed_ms)
                } else {
                    format!("Proxy server unreachable ({} ms)", res.elapsed_ms)
                },
            ),
            ProxyTestOutcome::ProxyAuth => (
                Tone::Warning,
                if zh {
                    "代理要求身份验证（HTTP 407），请在 URL 中补充 user:password@".to_owned()
                } else {
                    "Proxy requires authentication (HTTP 407); include user:password@ in URL"
                        .to_owned()
                },
            ),
            ProxyTestOutcome::ProxyRefused => (
                Tone::Danger,
                if zh {
                    "代理拒绝建立隧道连接".to_owned()
                } else {
                    "Proxy refused tunnel connection".to_owned()
                },
            ),
            ProxyTestOutcome::TargetNoResponse => (
                Tone::Warning,
                if zh {
                    format!(
                        "路由（{}）可达，但目标 {} 未响应（{}，{} ms）",
                        res.route,
                        res.target,
                        res.detail.as_deref().unwrap_or("timeout"),
                        res.elapsed_ms
                    )
                } else {
                    format!(
                        "Route ({}) ok, but {} did not answer ({}, {} ms)",
                        res.route,
                        res.target,
                        res.detail.as_deref().unwrap_or("timeout"),
                        res.elapsed_ms
                    )
                },
            ),
        };
        proxy_sec = proxy_sec.child(alert_banner(tone, text, None, cx));
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

    let no_proxy_sec = v_col()
        .w_full()
        .gap(px(8.0))
        .pt(px(6.0))
        .child(
            v_col()
                .gap(px(2.0))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .child(if zh { "绕过代理的主机（No Proxy）" } else { "Bypass proxy for hosts (No Proxy)" }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(if zh {
                            "匹配的主机直接连接而不走全局代理；支持 *.suffix 通配（最多 64 项，逗号分隔可批量添加）。"
                        } else {
                            "Matching hosts connect directly; supports *.suffix wildcards (up to 64 entries, comma-separated)."
                        }),
                ),
        )
        .children((!net.no_proxy.is_empty()).then_some(chips))
        .child(
            h_row()
                .w_full()
                .gap(px(8.0))
                .child(div().flex_1().child(text_input(&view.inputs.no_proxy_add)))
                .child(
                    btn_secondary("noproxy-add-btn", if zh { "添加" } else { "Add" })
                        .icon(icon_xs(UiIcon::Plus))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.add_no_proxy_entry(window, cx);
                        })),
                ),
        );

    // Custom CA bundle
    let mut ca_sec = v_col()
        .w_full()
        .gap(px(8.0))
        .child(section_header(
            if zh {
                "自定义受信任 CA 证书包"
            } else {
                "Extra Trusted CA Bundle"
            },
            Some(
                btn_ghost("net-reset-ca", if zh { "清除" } else { "Clear" })
                    .disabled(net.ca_bundle_path.is_empty())
                    .on_click(cx.listener(|this, _, window, cx| this.clear_ca_bundle(window, cx)))
                    .into_any_element(),
            ),
            cx,
        ))
        .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
            "用于企业自签名根证书或公司内网 HTTPS 代理（PEM 格式，最大 8 MiB，需填写绝对路径）。"
        } else {
            "Trust extra corporate or self-signed root CAs (PEM bundle up to 8 MiB, absolute path)."
        }))
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
                    btn_secondary("net-pick-ca", if zh { "选择文件…" } else { "Browse…" })
                        .icon(icon_xs(UiIcon::FolderOpen))
                        .on_click(cx.listener(|this, _, window, cx| {
                            this.pick_ca_bundle(window, cx);
                        })),
                )
                .child(
                    btn_secondary("net-save-ca", if zh { "保存" } else { "Save" })
                        .on_click(cx.listener(|this, _, _, cx| this.save_ca_bundle(cx))),
                )
                .child(
                    btn_secondary("net-test-ca", if zh { "校验证书" } else { "Verify" })
                        .disabled(view.ca_testing)
                        .on_click(cx.listener(|this, _, _, cx| this.test_ca_bundle(cx))),
                ),
        );

    if let Some(info) = &view.ca_test_result {
        let (tone, msg) = if info.ok {
            (
                Tone::Success,
                if zh {
                    format!("证书包有效：已识别 {} 个可用根证书", info.certificates)
                } else {
                    format!(
                        "Valid CA bundle: {} usable root certificate(s)",
                        info.certificates
                    )
                },
            )
        } else {
            (
                Tone::Danger,
                info.error.clone().unwrap_or_else(|| {
                    if zh {
                        "证书包无效".to_owned()
                    } else {
                        "Invalid CA bundle".to_owned()
                    }
                }),
            )
        };
        ca_sec = ca_sec.child(alert_banner(tone, msg, None, cx));
    }

    // mTLS client certificates cross-link card
    let mtls_card = card(cx).p(px(14.0)).child(
        h_row()
            .w_full()
            .justify_between()
            .gap(px(12.0))
            .child(
                v_col()
                    .flex_1()
                    .min_w_0()
                    .gap(px(3.0))
                    .child(
                        div()
                            .text_sm()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(to_hsla(p.ink))
                            .child(if zh {
                                "客户端证书（mTLS）按工作区管理"
                            } else {
                                "Client certificates (mTLS) are workspace-scoped"
                            }),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .child(if zh {
                                "如需为特定域名配置双向 TLS 客户端证书与私钥，请前往「环境」模块管理。"
                            } else {
                                "Configure per-host client certificates and private keys inside the Environments module."
                            }),
                    ),
            )
            .child(
                btn_secondary("net-jump-envs", if zh { "前往环境" } else { "Open Environments" })
                    .icon(icon_xs(UiIcon::ExternalLink))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.jump_to_module(NavModule::Environments, cx);
                    })),
            ),
    );

    v_col()
        .w_full()
        .gap(px(22.0))
        .child(proxy_sec.child(no_proxy_sec))
        .child(ca_sec)
        .child(mtls_card)
}
