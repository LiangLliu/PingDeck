//! Gateway Overview bottom panels: Protocol Base URLs + Endpoints, Clients Summary, and Rules Summary.

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::services::clipboard;

use crate::ui::{
    Tone, UiIcon, badge, btn_ghost, btn_icon, card, h_row, kv_chip, method_badge, palette,
    section_heading, to_hsla, v_col,
};

use super::format::{format_relative_time, format_rule_status};
use super::{GatewayTab, ProvidersView};

pub fn render_overview_panels(
    view: &ProvidersView,
    active_clients: usize,
    zh: bool,
    cx: &mut Context<ProvidersView>,
) -> gpui_kit::Div {
    let p = palette(cx);

    let mut urls_box = v_col().gap(px(6.0)).child(
        h_row()
            .justify_between()
            .child(section_heading(
                if zh {
                    "协议入口地址（同协议直通或自动转码）"
                } else {
                    "Protocol Base URLs (passthrough or auto-transcode)"
                },
                cx,
            ))
            .child(div().text_xs().text_color(to_hsla(p.faint)).child(if zh {
                "每个客户端需携带独立令牌"
            } else {
                "Each client needs its own token"
            })),
    );
    if let Some(ov) = &view.overview {
        for (i, b) in ov.base_urls.iter().enumerate() {
            let copy_url = b.url.clone();
            let fam_lbl = match b.family.as_str() {
                "openai" => {
                    if zh {
                        "OpenAI 兼容"
                    } else {
                        "OpenAI-style"
                    }
                }
                "anthropic" => "Anthropic Messages",
                "gemini" => "Google Gemini",
                other => other,
            };
            urls_box = urls_box.child(
                h_row()
                    .justify_between()
                    .gap(px(10.0))
                    .px(px(10.0))
                    .py(px(7.0))
                    .rounded(px(7.0))
                    .bg(to_hsla(p.pane))
                    .child(
                        h_row()
                            .gap(px(8.0))
                            .flex_1()
                            .min_w_0()
                            .child(badge(fam_lbl, Tone::Accent, cx))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(SharedString::from(b.url.clone())),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(to_hsla(p.faint))
                                    .truncate()
                                    .child(SharedString::from(b.tools.join(" · "))),
                            ),
                    )
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(kv_chip(
                                if zh { "模型" } else { "models" },
                                b.model_count.to_string(),
                                cx,
                            ))
                            .child(btn_icon(("copy-gw-url", i), UiIcon::Copy).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    let _ = clipboard::set_text(&copy_url);
                                    this.status_msg = Some((
                                        Tone::Success,
                                        if zh {
                                            "地址已复制".into()
                                        } else {
                                            "URL copied".into()
                                        },
                                    ));
                                    cx.notify();
                                }),
                            )),
                    ),
            );
        }
        let mut ep_row = h_row().flex_wrap().gap(px(6.0)).pt(px(4.0));
        for ep in &ov.endpoints {
            ep_row = ep_row.child(
                h_row()
                    .gap(px(5.0))
                    .px(px(7.0))
                    .py(px(3.0))
                    .rounded(px(6.0))
                    .bg(to_hsla(p.sunken))
                    .child(method_badge(&ep.method, cx))
                    .child(
                        div()
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.ink_2))
                            .child(SharedString::from(ep.path.clone())),
                    ),
            );
        }
        urls_box = urls_box.child(ep_row);
    }

    let mut clients_summary = v_col().gap(px(6.0)).child(
        h_row()
            .justify_between()
            .child(section_heading(
                if zh {
                    format!("已接入客户端（{active_clients} 个有效令牌）")
                } else {
                    format!("Connected Clients ({active_clients} active)")
                },
                cx,
            ))
            .child(
                btn_ghost(
                    "gw-jump-clients",
                    if zh { "管理令牌 →" } else { "Manage →" },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.gateway_tab = GatewayTab::Clients;
                    cx.notify();
                })),
            ),
    );
    let client_rows = view
        .overview
        .as_ref()
        .map(|o| &o.clients[..])
        .unwrap_or(&[]);
    if client_rows.is_empty() {
        clients_summary = clients_summary.child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .py(px(8.0))
                .child(if zh {
                    "还没有客户端。点击右上角「接入编码工具…」一键写入配置或创建令牌。"
                } else {
                    "No clients yet. Click Connect client… to configure a tool or mint a token."
                }),
        );
    } else {
        for c in client_rows.iter().take(6) {
            let scope = if c.allow_all {
                if zh {
                    "全部模型".into()
                } else {
                    "All models".into()
                }
            } else {
                c.allowed_models.first().cloned().unwrap_or_else(|| {
                    if zh {
                        "无模型".into()
                    } else {
                        "None".into()
                    }
                })
            };
            clients_summary = clients_summary.child(
                h_row()
                    .justify_between()
                    .gap(px(8.0))
                    .px(px(8.0))
                    .py(px(5.0))
                    .rounded(px(6.0))
                    .bg(to_hsla(p.pane))
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .min_w_0()
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(SharedString::from(c.name.clone())),
                            )
                            .child(badge(
                                SharedString::from(c.token_hint.clone()),
                                Tone::Neutral,
                                cx,
                            )),
                    )
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(badge(SharedString::from(scope), Tone::Accent, cx))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(to_hsla(p.faint))
                                    .child(SharedString::from(c.last_used_ms.map_or_else(
                                        || if zh { "从未".into() } else { "Never".into() },
                                        |ms| format_relative_time(ms, zh),
                                    ))),
                            ),
                    ),
            );
        }
    }

    let mut rules_summary = v_col().gap(px(6.0)).child(
        h_row()
            .justify_between()
            .child(section_heading(
                if zh {
                    "网关备用规则（首个内容事件后不切换）"
                } else {
                    "Fallback Rules (locked after first content)"
                },
                cx,
            ))
            .child(
                btn_ghost(
                    "gw-jump-rules",
                    if zh {
                        "配置规则 →"
                    } else {
                        "Configure →"
                    },
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.gateway_tab = GatewayTab::Rules;
                    cx.notify();
                })),
            ),
    );
    if view.gateway_rules.is_empty() {
        rules_summary = rules_summary.child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .py(px(8.0))
                .child(if zh {
                    "暂无备用规则：主模型失败时直接向客户端返回错误。"
                } else {
                    "No fallback rules: failures return directly to the caller."
                }),
        );
    } else {
        for r in view.gateway_rules.iter().take(6) {
            let (st_lbl, st_tone) = format_rule_status(r.status, zh);
            let chain: Vec<String> = r.fallbacks.iter().map(|m| m.id_form()).collect();
            rules_summary = rules_summary.child(
                h_row()
                    .justify_between()
                    .gap(px(8.0))
                    .px(px(8.0))
                    .py(px(5.0))
                    .rounded(px(6.0))
                    .bg(to_hsla(p.pane))
                    .child(
                        v_col()
                            .min_w_0()
                            .flex_1()
                            .gap(px(1.0))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(SharedString::from(r.primary.id_form())),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(to_hsla(p.muted))
                                    .truncate()
                                    .child(SharedString::from(format!("→ {}", chain.join(" → ")))),
                            ),
                    )
                    .child(badge(st_lbl, st_tone, cx)),
            );
        }
    }

    v_col()
        .gap(px(14.0))
        .child(card(cx).p(px(14.0)).child(urls_box))
        .child(
            h_row()
                .items_start()
                .gap(px(14.0))
                .child(
                    card(cx)
                        .flex_1()
                        .min_w_0()
                        .p(px(14.0))
                        .child(clients_summary),
                )
                .child(card(cx).flex_1().min_w_0().p(px(14.0)).child(rules_summary)),
        )
}
