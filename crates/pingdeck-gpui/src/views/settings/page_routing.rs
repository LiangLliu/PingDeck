//! Settings › Routing & Failover page: AI gateway retries, timeouts, circuit breaker,
//! provider probing settings, read-only failover status codes, and local gateway runtime controls.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::model::settings::SettingsSection as CoreSection;
use pingdeck_desktop::core::model::settings::registry::RegistryConstant;
use pingdeck_desktop::state::NavModule;

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, btn_ghost, btn_secondary, card, h_row, icon_xs, number_field,
    palette, section_header, setting_row, status_indicator, text_input, to_hsla, toggle_switch,
    v_col,
};

use super::SettingsView;

pub fn render_routing(
    view: &SettingsView,
    zh: bool,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let gw = &view.settings.ai.gateway;
    let gw_running = view.gateway_status.as_ref().is_some_and(|s| s.running);
    let gw_base = view
        .gateway_status
        .as_ref()
        .filter(|s| s.running && !s.base_url.is_empty())
        .map(|s| s.base_url.clone())
        .unwrap_or_else(|| format!("http://127.0.0.1:{}", gw.port));

    let failover_codes = match view.registry.constants.get("failover.status_codes") {
        Some(RegistryConstant::List(codes)) => codes.join(" / "),
        _ => "401 / 402 / 403 / 429".to_owned(),
    };

    let runtime_card = card(cx).p(px(14.0)).child(
        h_row()
            .w_full()
            .justify_between()
            .gap(px(12.0))
            .child(
                h_row()
                    .gap(px(10.0))
                    .flex_1()
                    .min_w_0()
                    .child(status_indicator(
                        if gw_running {
                            StatusShape::Live
                        } else {
                            StatusShape::Idle
                        },
                        cx,
                    ))
                    .child(
                        v_col()
                            .min_w_0()
                            .gap(px(2.0))
                            .child(
                                h_row()
                                    .gap(px(8.0))
                                    .child(
                                        div()
                                            .text_sm()
                                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                            .text_color(to_hsla(p.ink))
                                            .child(if zh { "本地 AI 网关" } else { "Local AI Gateway" }),
                                    )
                                    .child(badge(
                                        if gw_running {
                                            if zh { "运行中" } else { "Running" }
                                        } else if zh {
                                            "已停止"
                                        } else {
                                            "Stopped"
                                        },
                                        if gw_running {
                                            Tone::Success
                                        } else {
                                            Tone::Neutral
                                        },
                                        cx,
                                    )),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(to_hsla(p.muted))
                                    .truncate()
                                    .child(SharedString::from(if zh {
                                        format!("监听地址 {gw_base} · 启停、局域网绑定与客户端令牌请在「服务商」页管理")
                                    } else {
                                        format!("Endpoint {gw_base} · manage start/stop, LAN binding, and client tokens in Providers")
                                    })),
                            ),
                    ),
            )
            .child(
                btn_secondary(
                    "route-jump-providers",
                    if zh { "前往服务商与网关" } else { "Open Providers" },
                )
                .icon(icon_xs(UiIcon::ExternalLink))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.jump_to_module(NavModule::Providers, cx);
                })),
            ),
    );

    let retry_sec = v_col()
        .w_full()
        .child(section_header(
            if zh { "网关重试与超时" } else { "Gateway Retries & Timeouts" },
            Some(
                btn_ghost("route-reset-gw", if zh { "恢复默认" } else { "Reset" })
                    .icon(icon_xs(UiIcon::RotateCcw))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.reset_keys(
                            CoreSection::Ai,
                            Some(
                                [
                                    "gateway.enabled_on_start",
                                    "gateway.port",
                                    "gateway.max_retries",
                                    "gateway.first_byte_timeout_s",
                                    "gateway.idle_timeout_s",
                                ]
                                .into_iter()
                                .map(str::to_owned)
                                .collect(),
                            ),
                            cx,
                        );
                    }))
                    .into_any_element(),
            ),
            cx,
        ))
        .child(setting_row(
            if zh { "启动时自动运行本地网关" } else { "Autostart local gateway on launch" },
            None,
            Some(SharedString::from(if zh {
                "打开 PingDeck 时自动在配置端口启动网关服务。"
            } else {
                "Automatically start the local AI gateway when PingDeck opens."
            })),
            toggle_switch("gw-autostart", gw.enabled_on_start).on_change(cx.listener(
                |this, checked: &bool, _, cx| {
                    this.patch_section(
                        CoreSection::Ai,
                        serde_json::json!({ "gateway": { "enabled_on_start": *checked } }),
                        cx,
                    );
                },
            )),
            cx,
        ))
        .child(setting_row(
            if zh { "网关监听端口" } else { "Gateway port" },
            Some(SharedString::from(if zh { "需重启网关" } else { "Restart gateway" })),
            Some(SharedString::from(if zh {
                "本地网关监听端口（1024–65535，默认 4141，不能与 Mock 起始端口重复）。"
            } else {
                "Port the local gateway binds to (1024–65535, default 4141; must differ from Mock start port)."
            })),
            h_row()
                .gap(px(6.0))
                .child(number_field(&view.inputs.gw_port, None, 88.0, cx))
                .child(
                    btn_secondary("gw-save-port", if zh { "应用" } else { "Apply" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let v = this.inputs.gw_port.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                1024,
                                65_535,
                                CoreSection::Ai,
                                |n| serde_json::json!({ "gateway": { "port": n } }),
                                cx,
                            );
                        }),
                    ),
                ),
            cx,
        ))
        .child(setting_row(
            if zh { "单次调用最大重试与故障转移次数" } else { "Max upstream retries / failover hops" },
            None,
            Some(SharedString::from(if zh {
                "首个内容事件发出前允许的上游重试与备用密钥切换次数（0–10，默认 4）。"
            } else {
                "Retries and backup-key failovers allowed before the first content event (0–10, default 4)."
            })),
            h_row()
                .gap(px(6.0))
                .child(number_field(
                    &view.inputs.gw_max_retries,
                    Some(if zh { "次" } else { "x" }),
                    72.0,
                    cx,
                ))
                .child(
                    btn_secondary("gw-save-retries", if zh { "应用" } else { "Apply" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let v = this.inputs.gw_max_retries.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                0,
                                10,
                                CoreSection::Ai,
                                |n| serde_json::json!({ "gateway": { "max_retries": n } }),
                                cx,
                            );
                        }),
                    ),
                ),
            cx,
        ))
        .child(setting_row(
            if zh { "首字节等待超时" } else { "First-byte timeout" },
            None,
            Some(SharedString::from(if zh {
                "等待上游返回首个响应字节的最长时间（1–600 秒，默认 30 秒）。"
            } else {
                "Seconds to wait for the first upstream byte (1–600 s, default 30 s)."
            })),
            h_row()
                .gap(px(6.0))
                .child(number_field(&view.inputs.gw_first_byte_s, Some("s"), 80.0, cx))
                .child(
                    btn_secondary("gw-save-fb", if zh { "应用" } else { "Apply" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let v = this.inputs.gw_first_byte_s.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                1,
                                600,
                                CoreSection::Ai,
                                |n| serde_json::json!({ "gateway": { "first_byte_timeout_s": n } }),
                                cx,
                            );
                        }),
                    ),
                ),
            cx,
        ))
        .child(setting_row(
            if zh { "流式静默超时" } else { "Stream idle timeout" },
            None,
            Some(SharedString::from(if zh {
                "流式输出中途无新数据到达的断开超时（1–3,600 秒，默认 60 秒）。"
            } else {
                "Seconds an open stream may remain silent before aborting (1–3,600 s, default 60 s)."
            })),
            h_row()
                .gap(px(6.0))
                .child(number_field(&view.inputs.gw_idle_s, Some("s"), 80.0, cx))
                .child(
                    btn_secondary("gw-save-idle", if zh { "应用" } else { "Apply" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let v = this.inputs.gw_idle_s.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                1,
                                3_600,
                                CoreSection::Ai,
                                |n| serde_json::json!({ "gateway": { "idle_timeout_s": n } }),
                                cx,
                            );
                        }),
                    ),
                ),
            cx,
        ))
        .child(setting_row(
            if zh { "触发故障转移的固定状态码" } else { "Failover trigger status codes" },
            Some(SharedString::from(if zh { "只读规则" } else { "Read-only" })),
            Some(SharedString::from(if zh {
                "遇到认证失败、余额不足、限流或 5xx/网络中断时自动切换到下一个可用密钥或路由。"
            } else {
                "Auth, quota, rate-limit, 5xx, and transport failures automatically roll over to the next healthy key."
            })),
            badge(format!("{failover_codes} · 5xx"), Tone::Neutral, cx),
            cx,
        ));

    let circuit_sec = v_col()
        .w_full()
        .child(section_header(
            if zh { "路由熔断保护" } else { "Circuit Breaker" },
            Some(
                btn_ghost("route-reset-cb", if zh { "恢复默认" } else { "Reset" })
                    .icon(icon_xs(UiIcon::RotateCcw))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.reset_keys(
                            CoreSection::Ai,
                            Some(
                                [
                                    "gateway.circuit_failure_threshold",
                                    "gateway.circuit_window_s",
                                    "gateway.circuit_open_s",
                                    "gateway.circuit_open_max_s",
                                ]
                                .into_iter()
                                .map(str::to_owned)
                                .collect(),
                            ),
                            cx,
                        );
                    }))
                    .into_any_element(),
            ),
            cx,
        ))
        .child(setting_row(
            if zh { "触发熔断的连续失败次数" } else { "Failure threshold" },
            None,
            Some(SharedString::from(if zh {
                "统计窗口内连续失败达到此次数后暂时摘除该路由（1–100，默认 3）。"
            } else {
                "Consecutive failures inside the window that open a route's circuit (1–100, default 3)."
            })),
            h_row()
                .gap(px(6.0))
                .child(number_field(
                    &view.inputs.gw_cb_threshold,
                    Some(if zh { "次" } else { "x" }),
                    72.0,
                    cx,
                ))
                .child(
                    btn_secondary("gw-save-cbt", if zh { "应用" } else { "Apply" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let v = this.inputs.gw_cb_threshold.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                1,
                                100,
                                CoreSection::Ai,
                                |n| {
                                    serde_json::json!({
                                        "gateway": { "circuit_failure_threshold": n }
                                    })
                                },
                                cx,
                            );
                        }),
                    ),
                ),
            cx,
        ))
        .child(setting_row(
            if zh { "失败统计时间窗口" } else { "Failure counting window" },
            None,
            Some(SharedString::from(if zh {
                "统计连续失败的滑动时间窗（1–3,600 秒，默认 30 秒）。"
            } else {
                "Rolling window in which consecutive failures are counted (1–3,600 s, default 30 s)."
            })),
            h_row()
                .gap(px(6.0))
                .child(number_field(&view.inputs.gw_cb_window_s, Some("s"), 80.0, cx))
                .child(
                    btn_secondary("gw-save-cbw", if zh { "应用" } else { "Apply" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let v = this.inputs.gw_cb_window_s.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                1,
                                3_600,
                                CoreSection::Ai,
                                |n| serde_json::json!({ "gateway": { "circuit_window_s": n } }),
                                cx,
                            );
                        }),
                    ),
                ),
            cx,
        ))
        .child(setting_row(
            if zh { "初始熔断冷却时长" } else { "Initial open cooldown" },
            None,
            Some(SharedString::from(if zh {
                "熔断打开后首次放行探测请求前的冷却时间（5–3,600 秒，默认 30 秒）。"
            } else {
                "Cooldown before a half-open probe call is attempted (5–3,600 s, default 30 s)."
            })),
            h_row()
                .gap(px(6.0))
                .child(number_field(&view.inputs.gw_cb_open_s, Some("s"), 80.0, cx))
                .child(
                    btn_secondary("gw-save-cbo", if zh { "应用" } else { "Apply" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let v = this.inputs.gw_cb_open_s.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                5,
                                3_600,
                                CoreSection::Ai,
                                |n| serde_json::json!({ "gateway": { "circuit_open_s": n } }),
                                cx,
                            );
                        }),
                    ),
                ),
            cx,
        ))
        .child(setting_row(
            if zh { "最大退避冷却上限" } else { "Max backoff cooldown" },
            None,
            Some(SharedString::from(if zh {
                "持续失败时指数退避熔断冷却的封顶时间（5–86,400 秒，默认 300 秒）。"
            } else {
                "Upper cap for exponential circuit backoff (5–86,400 s, default 300 s)."
            })),
            h_row()
                .gap(px(6.0))
                .child(number_field(&view.inputs.gw_cb_open_max_s, Some("s"), 88.0, cx))
                .child(
                    btn_secondary("gw-save-cbm", if zh { "应用" } else { "Apply" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let v = this.inputs.gw_cb_open_max_s.read(cx).value().to_string();
                            this.commit_u64(
                                &v,
                                5,
                                86_400,
                                CoreSection::Ai,
                                |n| serde_json::json!({ "gateway": { "circuit_open_max_s": n } }),
                                cx,
                            );
                        }),
                    ),
                ),
            cx,
        ));

    let probe_sec = v_col()
        .w_full()
        .child(section_header(
            if zh {
                "模型连通性探测"
            } else {
                "Provider Connectivity Probing"
            },
            Some(
                btn_ghost("route-reset-probe", if zh { "恢复默认" } else { "Reset" })
                    .icon(icon_xs(UiIcon::RotateCcw))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.reset_keys(
                            CoreSection::Ai,
                            Some(
                                [
                                    "providers.probe_concurrency",
                                    "providers.probe_timeout_s",
                                    "providers.default_test_prompt",
                                ]
                                .into_iter()
                                .map(str::to_owned)
                                .collect(),
                            ),
                            cx,
                        );
                    }))
                    .into_any_element(),
            ),
            cx,
        ))
        .child(setting_row(
            if zh {
                "批量探测并发数"
            } else {
                "Probe concurrency"
            },
            None,
            Some(SharedString::from(if zh {
                "在服务商页一键测试多个模型时的并行请求数（1–16，默认 3）。"
            } else {
                "Models tested in parallel during batch connectivity checks (1–16, default 3)."
            })),
            h_row()
                .gap(px(6.0))
                .child(number_field(
                    &view.inputs.prov_probe_concurrency,
                    None,
                    72.0,
                    cx,
                ))
                .child(
                    btn_secondary("prov-save-conc", if zh { "应用" } else { "Apply" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let v = this
                                .inputs
                                .prov_probe_concurrency
                                .read(cx)
                                .value()
                                .to_string();
                            this.commit_u64(
                                &v,
                                1,
                                16,
                                CoreSection::Ai,
                                |n| serde_json::json!({ "providers": { "probe_concurrency": n } }),
                                cx,
                            );
                        }),
                    ),
                ),
            cx,
        ))
        .child(setting_row(
            if zh {
                "单次探测超时"
            } else {
                "Probe timeout"
            },
            None,
            Some(SharedString::from(if zh {
                "每个模型测试请求的超时限制（5–300 秒，默认 30 秒）。"
            } else {
                "Timeout for a single model test call (5–300 s, default 30 s)."
            })),
            h_row()
                .gap(px(6.0))
                .child(number_field(
                    &view.inputs.prov_probe_timeout_s,
                    Some("s"),
                    80.0,
                    cx,
                ))
                .child(
                    btn_secondary("prov-save-pt", if zh { "应用" } else { "Apply" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let v = this
                                .inputs
                                .prov_probe_timeout_s
                                .read(cx)
                                .value()
                                .to_string();
                            this.commit_u64(
                                &v,
                                5,
                                300,
                                CoreSection::Ai,
                                |n| serde_json::json!({ "providers": { "probe_timeout_s": n } }),
                                cx,
                            );
                        }),
                    ),
                ),
            cx,
        ))
        .child(setting_row(
            if zh {
                "默认模型测试提示词"
            } else {
                "Default model test prompt"
            },
            None,
            Some(SharedString::from(if zh {
                "点击模型测试时发送的极简提示词（1–2,000 字节非空文本）。"
            } else {
                "Short prompt sent when probing a model (1–2,000 non-blank bytes)."
            })),
            h_row()
                .gap(px(6.0))
                .child(
                    div()
                        .w(px(250.0))
                        .child(text_input(&view.inputs.prov_test_prompt)),
                )
                .child(
                    btn_secondary("prov-save-prompt", if zh { "保存" } else { "Save" }).on_click(
                        cx.listener(|this, _, _, cx| {
                            let prompt = this.inputs.prov_test_prompt.read(cx).value().to_string();
                            this.patch_section(
                            CoreSection::Ai,
                            serde_json::json!({ "providers": { "default_test_prompt": prompt } }),
                            cx,
                        );
                        }),
                    ),
                ),
            cx,
        ));

    v_col()
        .w_full()
        .gap(px(22.0))
        .child(runtime_card)
        .child(retry_sec)
        .child(circuit_sec)
        .child(probe_sec)
}
