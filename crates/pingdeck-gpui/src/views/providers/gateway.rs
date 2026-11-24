//! Local AI Gateway Cockpit (`Overview` / `Clients` / `Access` / `Rules`) inside `ProvidersView`.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::services::clipboard;

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_ghost, btn_icon, btn_primary,
    btn_secondary, card, h_row, icon_xs, palette, status_indicator, tab_bar_underline, tab_item,
    to_hsla, v_col,
};

use super::format::{format_tokens, format_uptime};
use super::{
    GatewayTab, ProvidersView, gateway_access, gateway_clients, gateway_overview, gateway_rules,
};

pub fn render_gateway_cockpit(
    view: &ProvidersView,
    zh: bool,
    cx: &mut Context<ProvidersView>,
) -> impl IntoElement {
    let p = palette(cx);
    let gw_running = view.overview.as_ref().is_some_and(|o| o.status.running);
    let active_clients = view
        .gateway_clients
        .iter()
        .filter(|c| c.is_active())
        .count();
    let rules_count = view.gateway_rules.len();
    let ent = cx.entity().clone();

    let clients_lbl = if zh {
        format!("客户端 ({active_clients})")
    } else {
        format!("Clients ({active_clients})")
    };
    let rules_lbl = if zh {
        format!("备用路由 ({rules_count})")
    } else {
        format!("Fallback Rules ({rules_count})")
    };

    let tabs = tab_bar_underline("gw-cockpit-tabs")
        .selected_index(view.gateway_tab.index())
        .child(tab_item(if zh { "概览" } else { "Overview" }))
        .child(tab_item(clients_lbl))
        .child(tab_item(if zh { "访问控制" } else { "Access" }))
        .child(tab_item(rules_lbl))
        .on_click(move |idx, _, cx| {
            let next = GatewayTab::from_index(*idx);
            ent.update(cx, |this, cx| {
                this.gateway_tab = next;
                cx.notify();
            });
        });

    let header = h_row()
        .justify_between()
        .gap(px(12.0))
        .px(px(20.0))
        .py(px(10.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            h_row()
                .gap(px(14.0))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(if zh { "本地网关" } else { "Local Gateway" }),
                )
                .child(tabs),
        )
        .child(
            h_row()
                .gap(px(6.0))
                .children(view.status_msg.clone().map(|(tone, msg)| {
                    h_row().gap(px(4.0)).child(badge(msg, tone, cx)).child(
                        btn_icon("gw-clear-flash", UiIcon::Close).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.status_msg = None;
                                cx.notify();
                            },
                        )),
                    )
                }))
                .child(
                    btn_secondary("gw-restart", if zh { "重启" } else { "Restart" })
                        .on_click(cx.listener(|this, _, _, cx| this.gateway_action("restart", cx))),
                )
                .child(if gw_running {
                    btn_secondary("gw-stop", if zh { "停止网关" } else { "Stop" })
                        .on_click(cx.listener(|this, _, _, cx| this.gateway_action("stop", cx)))
                } else {
                    btn_primary("gw-start", if zh { "启动网关" } else { "Start Gateway" })
                        .on_click(cx.listener(|this, _, _, cx| this.gateway_action("start", cx)))
                }),
        );

    let body = match view.gateway_tab {
        GatewayTab::Overview => {
            render_overview_tab(view, active_clients, zh, cx).into_any_element()
        }
        GatewayTab::Clients => gateway_clients::render_clients_tab(view, zh, cx).into_any_element(),
        GatewayTab::Access => gateway_access::render_access_tab(view, zh, cx).into_any_element(),
        GatewayTab::Rules => gateway_rules::render_rules_tab(view, zh, cx).into_any_element(),
    };

    v_col().size_full().child(header).child(
        div()
            .flex_1()
            .min_h_0()
            .w_full()
            .p(px(20.0))
            .overflow_y_scrollbar()
            .child(div().w_full().child(body)),
    )
}

fn render_overview_tab(
    view: &ProvidersView,
    active_clients: usize,
    zh: bool,
    cx: &mut Context<ProvidersView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let gw_running = view.overview.as_ref().is_some_and(|o| o.status.running);
    let gw_port = view
        .overview
        .as_ref()
        .map_or(view.gateway_settings.port, |o| o.status.port);
    let base_url = view
        .overview
        .as_ref()
        .map(|o| o.status.base_url.clone())
        .filter(|s| !s.is_empty())
        .unwrap_or_else(|| format!("http://127.0.0.1:{gw_port}"));
    let uptime_s = view.overview.as_ref().map_or(0, |o| o.status.uptime_s);
    let last_err = view
        .overview
        .as_ref()
        .and_then(|o| o.status.last_error.as_ref().map(|e| e.message.clone()));
    let require_tok = view.gateway_settings.require_token;
    let serve_lan = view.gateway_settings.serve_lan;
    let copy_base = base_url.clone();

    let hero = card(cx).p(px(16.0)).bg(to_hsla(p.pane)).child(
        h_row()
            .justify_between()
            .gap(px(12.0))
            .flex_wrap()
            .child(
                v_col()
                    .gap(px(6.0))
                    .child(
                        h_row()
                            .gap(px(8.0))
                            .flex_wrap()
                            .child(status_indicator(
                                if gw_running { StatusShape::Live } else { StatusShape::Idle },
                                cx,
                            ))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(to_hsla(p.ink))
                                    .child(if gw_running {
                                        if zh { "正在监听" } else { "Listening" }
                                    } else if zh {
                                        "网关已停止"
                                    } else {
                                        "Gateway stopped"
                                    }),
                            )
                            .child(
                                btn_ghost("gw-copy-root", base_url)
                                    .icon(icon_xs(UiIcon::Copy))
                                    .border_1()
                                    .border_color(to_hsla(p.line_strong.composite_over(p.surface)))
                                    .bg(to_hsla(p.surface))
                                    .on_click(cx.listener(move |this, _, _, cx| {
                                        let _ = clipboard::set_text(&copy_base);
                                        this.status_msg = Some((
                                            Tone::Success,
                                            if zh { "网关地址已复制".into() } else { "Gateway URL copied".into() },
                                        ));
                                        cx.notify();
                                    })),
                            )
                            .child(badge(
                                if require_tok {
                                    if zh { "令牌鉴权已开启" } else { "Token auth on" }
                                } else if zh {
                                    "允许匿名调用"
                                } else {
                                    "Anonymous allowed"
                                },
                                if require_tok { Tone::Success } else { Tone::Warning },
                                cx,
                            )),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .child(SharedString::from(if gw_running {
                                if zh {
                                    format!(
                                        "已运行 {} · {} · Host / Origin 防重绑定保护已启用",
                                        format_uptime(uptime_s, true),
                                        if serve_lan { "局域网监听 (0.0.0.0)" } else { "仅本机回环 (127.0.0.1)" }
                                    )
                                } else {
                                    format!(
                                        "Up {} · {} · Host/Origin guard active",
                                        format_uptime(uptime_s, false),
                                        if serve_lan { "LAN enabled" } else { "Loopback only" }
                                    )
                                }
                            } else if zh {
                                "启动网关后，本地编码助手即可通过统一协议地址和备用链路由请求。".into()
                            } else {
                                "Start the gateway to route local coding tools with unified failover.".into()
                            })),
                    ),
            )
            .child(
                btn_primary("gw-connect-btn", if zh { "接入编码工具…" } else { "Connect client…" })
                    .icon(icon_xs(UiIcon::Terminal))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_connect_sheet(None, window, cx);
                    })),
            ),
    );

    let (today_reqs, today_ok, today_fail, today_in, today_out, today_cached) = view
        .overview
        .as_ref()
        .and_then(|o| o.today.as_ref())
        .map(|t| {
            (
                t.usage.requests,
                t.usage.ok_requests,
                t.usage.failed,
                t.usage.input_tokens,
                t.usage.output_tokens,
                t.usage.cached_tokens,
            )
        })
        .unwrap_or((0, 0, 0, 0, 0, 0));

    let ok_rate = if today_reqs > 0 {
        format!("{:.1}%", (today_ok as f64 / today_reqs as f64) * 100.0)
    } else {
        "—".into()
    };
    let enabled_models = view.models.iter().filter(|m| m.model.enabled).count();

    let kpis = h_row()
        .gap(px(10.0))
        .child(kpi_card(
            if zh {
                "今日网关转发"
            } else {
                "Requests Today"
            },
            today_reqs.to_string(),
            if zh {
                format!("{today_ok} 次成功 · {today_fail} 次失败")
            } else {
                format!("{today_ok} ok · {today_fail} failed")
            },
            cx,
        ))
        .child(kpi_card(
            if zh {
                "转发成功率"
            } else {
                "Success Rate"
            },
            ok_rate,
            if zh {
                "仅统计本地今日已路由请求".into()
            } else {
                "Routed calls today".into()
            },
            cx,
        ))
        .child(kpi_card(
            if zh { "今日 Tokens" } else { "Tokens Today" },
            format_tokens(today_in + today_out),
            if zh {
                format!(
                    "输入 {} · 输出 {} · 缓存 {}",
                    format_tokens(today_in),
                    format_tokens(today_out),
                    format_tokens(today_cached)
                )
            } else {
                format!(
                    "In {} · Out {} · Cache {}",
                    format_tokens(today_in),
                    format_tokens(today_out),
                    format_tokens(today_cached)
                )
            },
            cx,
        ))
        .child(kpi_card(
            if zh {
                "客户端 / 可用模型"
            } else {
                "Clients / Models"
            },
            format!("{active_clients} / {enabled_models}"),
            if zh {
                format!(
                    "{} 个服务商 · {} 条备用规则",
                    view.providers.len(),
                    view.gateway_rules.len()
                )
            } else {
                format!(
                    "{} providers · {} rules",
                    view.providers.len(),
                    view.gateway_rules.len()
                )
            },
            cx,
        ));

    let mut out = v_col().gap(px(14.0)).child(hero);
    if let Some(err) = last_err {
        out = out.child(
            card(cx)
                .p(px(12.0))
                .bg(to_hsla(p.bad_soft))
                .border_color(to_hsla(p.bad_line.composite_over(p.bad_soft)))
                .child(
                    h_row()
                        .justify_between()
                        .gap(px(8.0))
                        .child(div().text_xs().text_color(to_hsla(p.bad)).child(
                            SharedString::from(if zh {
                                format!("网关启动失败：{err}")
                            } else {
                                format!("Gateway start error: {err}")
                            }),
                        ))
                        .child(
                            btn_secondary(
                                "gw-err-access",
                                if zh { "换端口" } else { "Change port" },
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.gateway_tab = GatewayTab::Access;
                                cx.notify();
                            })),
                        ),
                ),
        );
    }
    out.child(kpis)
        .child(gateway_overview::render_overview_panels(
            view,
            active_clients,
            zh,
            cx,
        ))
}

fn kpi_card(label: &'static str, value: String, sub: String, cx: &gpui_kit::App) -> gpui_kit::Div {
    let p = palette(cx);
    card(cx).flex_1().min_w(px(150.0)).p(px(12.0)).child(
        v_col()
            .gap(px(3.0))
            .child(div().text_xs().text_color(to_hsla(p.muted)).child(label))
            .child(
                div()
                    .text_lg()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(to_hsla(p.ink))
                    .child(SharedString::from(value)),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(to_hsla(p.faint))
                    .truncate()
                    .child(SharedString::from(sub)),
            ),
    )
}
