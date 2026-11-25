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
    cx: &mut Context<ProvidersView>,
) -> gpui_kit::Div {
    let p = palette(cx);

    let mut urls_box = v_col().gap(px(6.0)).child(
        h_row()
            .justify_between()
            .child(section_heading(
                crate::ui::tr(cx)
                    .providers()
                    .prov_gateway_overview_protocol_base_urls_passthrough_or(),
                cx,
            ))
            .child(
                div().text_xs().text_color(to_hsla(p.faint)).child(
                    crate::ui::tr(cx)
                        .providers()
                        .prov_gateway_overview_each_client_needs_its_own(),
                ),
            ),
    );
    if let Some(ov) = &view.overview {
        for (i, b) in ov.base_urls.iter().enumerate() {
            let copy_url = b.url.clone();
            let fam_lbl = match b.family.as_str() {
                "openai" => crate::ui::tr(cx)
                    .providers()
                    .prov_gateway_overview_openai_style(),
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
                                crate::ui::tr(cx).providers().prov_gateway_overview_models(),
                                b.model_count.to_string(),
                                cx,
                            ))
                            .child(btn_icon(("copy-gw-url", i), UiIcon::Copy).on_click(
                                cx.listener(move |this, _, _, cx| {
                                    let _ = clipboard::set_text(&copy_url);
                                    this.status_msg = Some((
                                        Tone::Success,
                                        crate::ui::tr(cx)
                                            .providers()
                                            .prov_gateway_overview_url_copied()
                                            .into(),
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
                crate::ui::tr(cx)
                    .providers()
                    .prov_gateway_overview_connected_clients_active(active_clients),
                cx,
            ))
            .child(
                btn_ghost(
                    "gw-jump-clients",
                    crate::ui::tr(cx).providers().prov_gateway_overview_manage(),
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
                .child(
                    crate::ui::tr(cx)
                        .providers()
                        .prov_gateway_overview_no_clients_yet_click_connect(),
                ),
        );
    } else {
        for c in client_rows.iter().take(6) {
            let scope = if c.allow_all {
                crate::ui::tr(cx).providers().gw_clients_all().into()
            } else {
                c.allowed_models
                    .first()
                    .map(|m| view.format_model_ref(m))
                    .unwrap_or_else(|| {
                        crate::ui::tr(cx)
                            .providers()
                            .prov_gateway_overview_none()
                            .into()
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
                                        || crate::ui::tr(cx).providers().gw_clients_never().into(),
                                        |ms| {
                                            format_relative_time(ms, crate::ui::active_language(cx))
                                        },
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
                crate::ui::tr(cx)
                    .providers()
                    .prov_gateway_overview_fallback_rules_locked_after_first(),
                cx,
            ))
            .child(
                btn_ghost(
                    "gw-jump-rules",
                    crate::ui::tr(cx)
                        .providers()
                        .prov_gateway_overview_configure(),
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
                .child(
                    crate::ui::tr(cx)
                        .providers()
                        .prov_gateway_overview_no_fallback_rules_failures_return(),
                ),
        );
    } else {
        for r in view.gateway_rules.iter().take(6) {
            let (st_lbl, st_tone) = format_rule_status(r.status, crate::ui::active_language(cx));
            let primary_disp = format!(
                "{}/{}",
                view.provider_name(&r.primary.provider_id),
                r.primary.model
            );
            let chain: Vec<String> = r
                .fallbacks
                .iter()
                .map(|m| format!("{}/{}", view.provider_name(&m.provider_id), m.model))
                .collect();
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
                                    .child(SharedString::from(primary_disp)),
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
