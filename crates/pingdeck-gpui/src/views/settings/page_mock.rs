//! Settings › Mock page: default start port (with gateway collision notice), response latency,
//! default CORS, ring-buffer log capacity, and extra Host-guard allowlist.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::model::settings::SettingsSection as CoreSection;

use crate::ui::{
    Tone, UiIcon, alert_banner, btn_ghost, btn_icon, btn_secondary, h_row, icon_xs, number_field,
    palette, setting_row, settings_section, settings_section_padded, text_input, to_hsla,
    toggle_switch, v_col,
};

use super::SettingsView;

pub fn render_mock(view: &SettingsView, zh: bool, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let p = palette(cx);
    let mock = &view.settings.mock;
    let gw_port = view.settings.ai.gateway.port;

    let defaults_rows = v_col()
        .w_full()
        .child(setting_row(
            if zh { "默认起始端口" } else { "Default starting port" },
            Some(SharedString::from(if zh {
                format!("AI 网关端口：{gw_port}")
            } else {
                format!("AI Gateway port: {gw_port}")
            })),
            Some(SharedString::from(if zh {
                "新建 Mock 服务器优先尝试此端口（1024–65535，默认 3001，不能与 AI 网关端口相同）。"
            } else {
                "First port tried for new mock servers (1024–65535, default 3001; must differ from AI gateway port)."
            })),
            h_row()
                .gap(px(6.0))
                .child(number_field(&view.inputs.mock_first_port, None, 88.0, cx))
                .child(
                    btn_secondary("mock-save-port", if zh { "应用" } else { "Apply" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let v = this.inputs.mock_first_port.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                1024,
                                65_535,
                                CoreSection::Mock,
                                |n| serde_json::json!({ "first_port": n }),
                                cx,
                            );
                        }),
                    ),
                ),
            cx,
        ))
        .child(setting_row(
            if zh { "默认模拟延迟" } else { "Default latency" },
            None,
            Some(SharedString::from(if zh {
                "新建 Mock 服务响应前附加的人工延迟（0–60,000 ms，默认 0 ms）。"
            } else {
                "Artificial delay added before responses on new mock servers (0–60,000 ms, default 0 ms)."
            })),
            h_row()
                .gap(px(6.0))
                .child(number_field(&view.inputs.mock_latency_ms, Some("ms"), 96.0, cx))
                .child(
                    btn_secondary("mock-save-latency", if zh { "应用" } else { "Apply" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let v = this.inputs.mock_latency_ms.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                0,
                                60_000,
                                CoreSection::Mock,
                                |n| serde_json::json!({ "default_latency_ms": n }),
                                cx,
                            );
                        }),
                    ),
                ),
            cx,
        ))
        .child(setting_row(
            if zh { "默认启用 CORS 响应头" } else { "Enable CORS headers by default" },
            None,
            Some(SharedString::from(if zh {
                "新建 Mock 服务自动允许跨域预检（OPTIONS）与常见响应头。"
            } else {
                "New mock servers respond to CORS preflight (OPTIONS) and return permissive headers."
            })),
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
            if zh { "内存请求日志容量" } else { "In-memory log capacity" },
            None,
            Some(SharedString::from(if zh {
                "每个运行中的 Mock 实例保留的最近请求记录条数（10–5,000，默认 200）。"
            } else {
                "Transactions kept in each running mock server's ring buffer (10–5,000, default 200)."
            })),
            h_row()
                .gap(px(6.0))
                .child(number_field(
                    &view.inputs.mock_log_capacity,
                    Some(if zh { "条" } else { "rows" }),
                    92.0,
                    cx,
                ))
                .child(
                    btn_secondary("mock-save-logcap", if zh { "应用" } else { "Apply" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let v = this.inputs.mock_log_capacity.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                10,
                                5_000,
                                CoreSection::Mock,
                                |n| serde_json::json!({ "log_capacity": n }),
                                cx,
                            );
                        }),
                    ),
                ),
            cx,
        ));

    let mut defaults_sec = v_col().w_full().gap(px(10.0)).child(settings_section(
        if zh {
            "Mock 服务默认值"
        } else {
            "Mock Server Defaults"
        },
        Some(
            btn_ghost("mock-reset-all", if zh { "恢复默认" } else { "Reset" })
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
            if zh {
                format!("Mock 起始端口与 AI 网关端口（{gw_port}）相同，请修改其中一项。")
            } else {
                format!("Mock starting port matches the AI gateway port ({gw_port}); change one of them.")
            },
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
        if zh { "额外允许的 Host 头" } else { "Extra Allowed Host Headers" },
        None,
        v_col()
            .w_full()
            .gap(px(10.0))
            .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                "Mock 监听器始终仅绑定本机回环地址（127.0.0.1），此列表用于放行 Docker 容器（host.docker.internal）或 Android 模拟器（10.0.2.2）转发来的 Host 头（格式 host 或 host:port，最多 16 项）。"
            } else {
                "Mock listeners always bind loopback only (127.0.0.1). Add host or host:port entries such as host.docker.internal or 10.0.2.2 for containers/emulators (up to 16 entries)."
            }))
            .children((!mock.extra_allowed_hosts.is_empty()).then_some(chips))
            .child(
                h_row()
                    .w_full()
                    .gap(px(8.0))
                    .child(div().flex_1().child(text_input(&view.inputs.mock_host_add)))
                    .child(
                        btn_secondary("mock-host-add-btn", if zh { "添加主机" } else { "Add host" })
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
