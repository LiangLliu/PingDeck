//! Provider Overview supporting cards: Protocol Endpoints & Sniff Evidence, Balance,
//! 24h Health & Circuits, and Recent Calls.

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::llm::store::{Provider, ProviderKey};

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, btn_ghost, card, collapsible_box, h_row, icon_xs, kv_chip,
    palette, progress_bar, section_heading, status_indicator, to_hsla, v_col,
};

use super::ProvidersView;
use super::format::{format_auth, format_balance_report, format_protocol, format_relative_time};

pub fn render_endpoints_card(
    view: &ProvidersView,
    prov: &Provider,
    zh: bool,
    cx: &mut Context<ProvidersView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let enabled_eps = prov.endpoints.iter().filter(|e| e.enabled).count();
    let mut ep_rows = v_col().gap(px(5.0));

    for ep in &prov.endpoints {
        let is_primary = prov.primary_protocol == Some(ep.protocol);
        let prefix = if ep.path_prefix.is_empty() {
            "/"
        } else {
            &ep.path_prefix
        };
        let auth_str = ep
            .auth
            .map_or_else(|| format_auth(prov.auth, zh), |a| format_auth(a, zh));
        ep_rows = ep_rows.child(
            h_row()
                .justify_between()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.pane))
                .child(
                    h_row()
                        .gap(px(7.0))
                        .min_w_0()
                        .child(status_indicator(
                            if ep.enabled {
                                StatusShape::Ok
                            } else {
                                StatusShape::Idle
                            },
                            cx,
                        ))
                        .child(badge(
                            format_protocol(ep.protocol, zh),
                            if ep.enabled {
                                Tone::Accent
                            } else {
                                Tone::Neutral
                            },
                            cx,
                        ))
                        .children(is_primary.then(|| {
                            badge(if zh { "主协议" } else { "Primary" }, Tone::Success, cx)
                        }))
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.ink_2))
                                .truncate()
                                .child(SharedString::from(format!(
                                    "{prefix}{}",
                                    ep.protocol.suffix()
                                ))),
                        ),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(badge(auth_str, Tone::Neutral, cx))
                        .child(badge(
                            if ep.enabled {
                                if zh { "启用" } else { "Enabled" }
                            } else if zh {
                                "停用"
                            } else {
                                "Disabled"
                            },
                            if ep.enabled {
                                Tone::Success
                            } else {
                                Tone::Neutral
                            },
                            cx,
                        )),
                ),
        );
    }

    let mut ev_rows = v_col().gap(px(4.0)).pt(px(6.0));
    for ev in prov.sniff_evidence.iter().take(16) {
        ev_rows = ev_rows.child(
            h_row()
                .justify_between()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(5.0))
                .bg(to_hsla(p.sunken))
                .child(
                    h_row()
                        .gap(px(6.0))
                        .min_w_0()
                        .flex_1()
                        .child(badge(
                            SharedString::from(ev.phase.clone()),
                            Tone::Neutral,
                            cx,
                        ))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.ink_2))
                                .truncate()
                                .child(SharedString::from(format!("{} {}", ev.method, ev.url))),
                        ),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(badge(
                            ev.status.map_or_else(|| "—".into(), |s| s.to_string()),
                            if ev.status.is_some_and(|s| s < 400) {
                                Tone::Success
                            } else {
                                Tone::Neutral
                            },
                            cx,
                        ))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.faint))
                                .child(SharedString::from(format!("{}ms", ev.latency_ms))),
                        ),
                ),
        );
    }

    let ev_open = view.evidence_open;
    let ev_block = (!prov.sniff_evidence.is_empty()).then(|| {
        v_col()
            .gap(px(4.0))
            .pt(px(4.0))
            .child(
                btn_ghost(
                    "prov-toggle-ev",
                    if zh {
                        format!(
                            "{} 嗅探依据（{} 条请求）",
                            if ev_open { "▾" } else { "▸" },
                            prov.sniff_evidence.len()
                        )
                    } else {
                        format!(
                            "{} Sniff evidence ({} requests)",
                            if ev_open { "▾" } else { "▸" },
                            prov.sniff_evidence.len()
                        )
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.evidence_open = !this.evidence_open;
                    cx.notify();
                })),
            )
            .child(collapsible_box(ev_open, "prov-ev-col").content(ev_rows))
    });

    card(cx).p(px(14.0)).child(
        v_col()
            .gap(px(8.0))
            .child(
                h_row()
                    .justify_between()
                    .child(section_heading(
                        if zh {
                            "协议端点与探测矩阵"
                        } else {
                            "Protocol Endpoints & Evidence"
                        },
                        cx,
                    ))
                    .child(badge(
                        if zh {
                            format!("{enabled_eps} / {} 协议已启用", prov.endpoints.len())
                        } else {
                            format!("{enabled_eps} / {} enabled", prov.endpoints.len())
                        },
                        Tone::Accent,
                        cx,
                    )),
            )
            .child(ep_rows)
            .children(ev_block),
    )
}

pub fn render_balance_card(
    view: &ProvidersView,
    prov: &Provider,
    keys: &[ProviderKey],
    zh: bool,
    cx: &mut Context<ProvidersView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let pid = prov.id.clone();
    let best = keys
        .iter()
        .filter(|k| k.enabled)
        .filter_map(|k| k.balance.as_ref())
        .find(|b| b.last_good.as_ref().is_some_and(|r| r.supported));

    let mut content = v_col().gap(px(7.0)).child(
        h_row()
            .justify_between()
            .child(section_heading(if zh { "账户余额" } else { "Balance" }, cx))
            .child(
                btn_ghost(
                    "prov-bal-refresh",
                    if view.refreshing_balance {
                        if zh { "刷新中…" } else { "Refreshing…" }
                    } else if zh {
                        "刷新"
                    } else {
                        "Refresh"
                    },
                )
                .icon(icon_xs(UiIcon::Refresh))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if !this.refreshing_balance {
                        this.refresh_provider_balance(pid.clone(), zh, cx);
                    }
                })),
            ),
    );

    if prov.category == "local" {
        content = content.child(
            div()
                .text_base()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(p.ok))
                .child(if zh {
                    "本地，免费"
                } else {
                    "Local · Free"
                }),
        );
    } else if let Some(stored) = best
        && let Some(rep) = &stored.last_good
    {
        content = content.child(
            div()
                .text_lg()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(if rep.exhausted { p.bad } else { p.ink }))
                .child(SharedString::from(format_balance_report(rep, zh))),
        );
        if let (Some(rem), Some(tot)) = (rep.remaining, rep.total)
            && tot > 0.0
        {
            content = content.child(progress_bar(
                "prov-bal-prog",
                ((rem / tot) * 100.0).clamp(0.0, 100.0) as f32,
            ));
        }
        if let Some(src) = &rep.source_endpoint {
            content = content.child(
                div()
                    .text_size(px(11.0))
                    .text_color(to_hsla(p.faint))
                    .truncate()
                    .child(SharedString::from(if zh {
                        format!(
                            "来源 {src} · {}",
                            format_relative_time(rep.checked_at_ms, true)
                        )
                    } else {
                        format!(
                            "From {src} · {}",
                            format_relative_time(rep.checked_at_ms, false)
                        )
                    })),
            );
        }
        if let Some(err) = &stored.error {
            content = content.child(badge(
                SharedString::from(err.message.clone()),
                Tone::Warning,
                cx,
            ));
        }
    } else if let Some(err) = keys
        .iter()
        .filter_map(|k| k.balance.as_ref().and_then(|b| b.error.as_ref()))
        .next()
    {
        content = content.child(
            div()
                .text_xs()
                .text_color(to_hsla(p.warn))
                .child(SharedString::from(err.message.clone())),
        );
    } else {
        content = content.child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
            "尚未查询或该服务商未提供余额接口"
        } else {
            "Not checked yet or no balance API"
        }));
    }

    card(cx).p(px(14.0)).child(content)
}

pub fn render_health_card(
    view: &ProvidersView,
    prov: &Provider,
    zh: bool,
    cx: &mut Context<ProvidersView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let mut col = v_col().gap(px(7.0)).child(
        h_row()
            .justify_between()
            .child(section_heading(
                if zh {
                    "健康与熔断（24h）"
                } else {
                    "Health & Circuits (24h)"
                },
                cx,
            ))
            .children(view.provider_health.as_ref().map(|h| {
                badge(
                    if zh {
                        format!("{} 次请求", h.requests)
                    } else {
                        format!("{} reqs", h.requests)
                    },
                    Tone::Neutral,
                    cx,
                )
            })),
    );

    if let Some(h) = &view.provider_health {
        if h.enough_requests {
            let sr = h
                .success_rate
                .map_or_else(|| "—".into(), |r| format!("{:.1}%", r * 100.0));
            let p50 = h
                .ttft_p50_ms
                .map_or_else(|| "—".into(), |m| format!("{m} ms"));
            let p95 = h
                .ttft_p95_ms
                .map_or_else(|| "—".into(), |m| format!("{m} ms"));
            col = col.child(
                h_row()
                    .flex_wrap()
                    .gap(px(5.0))
                    .child(kv_chip(if zh { "成功率" } else { "OK" }, sr, cx))
                    .child(kv_chip("p50", p50, cx))
                    .child(kv_chip("p95", p95, cx)),
            );
        } else {
            col = col.child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                "最近 24 小时请求不足 10 次，暂未生成延迟分位统计。"
            } else {
                "Not enough requests (<10 in 24h) for latency percentiles."
            }));
        }

        if !h.open_circuits.is_empty() {
            let pid_reset = prov.id.clone();
            col = col.child(
                h_row()
                    .justify_between()
                    .pt(px(4.0))
                    .child(badge(
                        if zh {
                            format!("{} 个熔断已打开", h.open_circuits.len())
                        } else {
                            format!("{} open circuit(s)", h.open_circuits.len())
                        },
                        Tone::Danger,
                        cx,
                    ))
                    .child(
                        btn_ghost("prov-reset-cb", if zh { "重置熔断" } else { "Reset" }).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.reset_provider_circuits(pid_reset.clone(), zh, cx);
                            }),
                        ),
                    ),
            );
            for oc in h.open_circuits.iter().take(4) {
                col = col.child(
                    h_row()
                        .justify_between()
                        .px(px(7.0))
                        .py(px(4.0))
                        .rounded(px(5.0))
                        .bg(to_hsla(p.bad_soft))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.bad))
                                .truncate()
                                .child(SharedString::from(oc.model_id.clone())),
                        )
                        .child(
                            div().text_size(px(10.5)).text_color(to_hsla(p.bad)).child(
                                SharedString::from(
                                    oc.retry_in_s
                                        .map_or_else(|| "half-open".into(), |s| format!("{s}s")),
                                ),
                            ),
                        ),
                );
            }
        }
    } else {
        col = col.child(div().text_xs().text_color(to_hsla(p.faint)).child(if zh {
            "加载健康数据中…"
        } else {
            "Loading health…"
        }));
    }

    card(cx).p(px(14.0)).child(col)
}

pub fn render_recent_calls_card(
    view: &ProvidersView,
    zh: bool,
    cx: &gpui_kit::App,
) -> gpui_kit::Div {
    let p = palette(cx);
    let mut col = v_col().gap(px(6.0)).child(section_heading(
        if zh { "最近调用" } else { "Recent Calls" },
        cx,
    ));

    if view.recent_logs.is_empty() {
        col = col.child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
            "还没有调用记录。"
        } else {
            "No recent calls yet."
        }));
    } else {
        for log in view.recent_logs.iter().take(6) {
            let tone = if (200..300).contains(&log.status) {
                Tone::Success
            } else if log.status == 429 {
                Tone::Warning
            } else {
                Tone::Danger
            };
            let ms = log.ttft_ms.unwrap_or(log.latency_ms);
            col = col.child(
                h_row()
                    .justify_between()
                    .gap(px(6.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.ink_2))
                            .truncate()
                            .child(SharedString::from(
                                log.upstream_model
                                    .clone()
                                    .unwrap_or_else(|| log.requested_model.clone()),
                            )),
                    )
                    .child(badge(format!("{} · {ms}ms", log.status), tone, cx)),
            );
        }
    }

    card(cx).p(px(14.0)).child(col)
}
