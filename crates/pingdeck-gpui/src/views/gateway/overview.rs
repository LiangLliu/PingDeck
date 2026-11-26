//! Gateway Overview tab: today's KPI strip, start error banner, Protocol Base URLs + Endpoints,
//! Connected Clients summary, and Fallback Rules summary.

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::services::clipboard;

use crate::ui::{
    Tone, UiIcon, badge, btn_ghost, btn_icon, btn_secondary, card, h_row, kv_chip, method_badge,
    palette, section_heading, to_hsla, v_col,
};

use crate::views::providers::format::{format_relative_time, format_rule_status, format_tokens};

use super::{GatewayTab, GatewayView};

pub fn render_overview_tab(
    view: &GatewayView,
    active_clients: usize,
    cx: &mut Context<GatewayView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let pr = crate::ui::tr(cx).providers();
    let last_err = view
        .overview
        .as_ref()
        .and_then(|o| o.status.last_error.as_ref().map(|e| e.message.clone()));

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

    let today_cost = view
        .overview
        .as_ref()
        .and_then(|o| o.today.as_ref())
        .map(|t| {
            if t.usage.cost_micro.is_empty() {
                "—".to_owned()
            } else {
                t.usage
                    .cost_micro
                    .iter()
                    .map(|(cur, &m)| format!("{cur} {:.4}", m as f64 / 1_000_000.0))
                    .collect::<Vec<_>>()
                    .join(" + ")
            }
        })
        .unwrap_or_else(|| "—".to_owned());

    let kpis = h_row()
        .gap(px(10.0))
        .child(kpi_card(
            pr.prov_gateway_requests_today(),
            today_reqs.to_string(),
            pr.prov_gateway_ok_failed_cost(today_ok, today_fail, today_cost),
            cx,
        ))
        .child(kpi_card(
            pr.prov_gateway_success_rate(),
            ok_rate,
            pr.prov_gateway_routed_calls_today().into(),
            cx,
        ))
        .child(kpi_card(
            pr.prov_gateway_tokens_today(),
            format_tokens(today_in + today_out),
            pr.prov_gateway_in_out_cache(
                format_tokens(today_in),
                format_tokens(today_out),
                format_tokens(today_cached),
            ),
            cx,
        ))
        .child(kpi_card(
            pr.prov_gateway_clients_models(),
            format!("{active_clients} / {enabled_models}"),
            pr.prov_gateway_providers_rules(view.providers.len(), view.gateway_rules.len()),
            cx,
        ));

    let mut out = v_col().gap(px(14.0));
    if let Some(err) = last_err {
        out =
            out.child(
                card(cx)
                    .p(px(12.0))
                    .bg(to_hsla(p.bad_soft))
                    .border_color(to_hsla(p.bad_line.composite_over(p.bad_soft)))
                    .child(
                        h_row()
                            .justify_between()
                            .gap(px(8.0))
                            .child(div().text_xs().text_color(to_hsla(p.bad)).child(
                                SharedString::from(pr.prov_gateway_gateway_start_error(err)),
                            ))
                            .child(
                                btn_secondary("gw-err-access", pr.gw_err_change_port()).on_click(
                                    cx.listener(|this, _, _, cx| {
                                        this.gateway_tab = GatewayTab::Access;
                                        cx.notify();
                                    }),
                                ),
                            ),
                    ),
            );
    }

    out.child(kpis)
        .child(render_overview_panels(view, active_clients, cx))
}

fn render_overview_panels(
    view: &GatewayView,
    active_clients: usize,
    cx: &mut Context<GatewayView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let pr = crate::ui::tr(cx).providers();

    let mut urls_box = v_col().gap(px(6.0)).child(
        h_row()
            .justify_between()
            .child(section_heading(
                pr.prov_gateway_overview_protocol_base_urls_passthrough_or(),
                cx,
            ))
            .child(
                div()
                    .text_xs()
                    .text_color(to_hsla(p.faint))
                    .child(pr.prov_gateway_overview_each_client_needs_its_own()),
            ),
    );
    if let Some(ov) = &view.overview {
        for (i, b) in ov.base_urls.iter().enumerate() {
            let copy_url = b.url.clone();
            let fam_lbl = match b.family.as_str() {
                "openai" => pr.prov_gateway_overview_openai_style(),
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
                                pr.prov_gateway_overview_models(),
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
                pr.prov_gateway_overview_connected_clients_active(active_clients),
                cx,
            ))
            .child(
                btn_ghost("gw-jump-clients", pr.prov_gateway_overview_manage()).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.gateway_tab = GatewayTab::Clients;
                        cx.notify();
                    }),
                ),
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
                .child(pr.prov_gateway_overview_no_clients_yet_click_connect()),
        );
    } else {
        for c in client_rows.iter().take(6) {
            let scope = if c.allow_all {
                pr.gw_clients_all().into()
            } else {
                c.allowed_models
                    .first()
                    .map(|m| view.format_model_ref(m))
                    .unwrap_or_else(|| pr.prov_gateway_overview_none().into())
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
                                        || pr.gw_clients_never().into(),
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
                pr.prov_gateway_overview_fallback_rules_locked_after_first(),
                cx,
            ))
            .child(
                btn_ghost("gw-jump-rules", pr.prov_gateway_overview_configure()).on_click(
                    cx.listener(|this, _, _, cx| {
                        this.gateway_tab = GatewayTab::Rules;
                        cx.notify();
                    }),
                ),
            ),
    );
    if view.gateway_rules.is_empty() {
        rules_summary = rules_summary.child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .py(px(8.0))
                .child(pr.prov_gateway_overview_no_fallback_rules_failures_return()),
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
