//! Gateway Overview tab matching `docs/design/prototype/index.html`:
//! - 4 operational KPI cards (Requests Today + failover recovery, Success Rate + clickable failed
//!   drill-down into Traffic, Passthrough vs Transcode, Median Gateway TTFT)
//! - Clean 4-column Protocol Base URLs table (no floating endpoint chip cloud)
//! - Two-column structured tables for Connected Clients and Fallback Rules with today's hits.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement as _, ParentElement as _,
    SharedString, StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::services::clipboard;

use crate::ui::{
    Tone, badge, btn_ghost, btn_secondary, card, h_row, palette, section_heading, table_header,
    to_hsla, v_col,
};
use crate::views::providers::format::format_relative_time;

use super::traffic_actions::GatewayTrafficFilter;
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

    let (today_reqs, today_ok, today_fail) = view
        .overview
        .as_ref()
        .and_then(|o| o.today.as_ref())
        .map(|t| (t.usage.requests, t.usage.ok_requests, t.usage.failed))
        .unwrap_or((0, 0, 0));

    let ops = view.overview.as_ref().map(|o| &o.ops);
    let passthrough = ops.map_or(0, |o| o.passthrough);
    let translated = ops.map_or(0, |o| o.translated);
    let failover_rec = ops.map_or(0, |o| o.failover_recovered);
    let median_ttft = ops.and_then(|o| o.median_ttft_ms);

    let ok_rate = if today_reqs > 0 {
        format!("{:.1}%", (today_ok as f64 / today_reqs as f64) * 100.0)
    } else {
        "—".into()
    };

    let req_sub: AnyElement = div()
        .text_size(px(11.0))
        .text_color(to_hsla(p.muted))
        .truncate()
        .child(SharedString::from(if failover_rec > 0 {
            pr.prov_gateway_overview_failover_recovered(failover_rec)
        } else {
            pr.prov_gateway_overview_no_failover_today().into()
        }))
        .into_any_element();

    let rate_sub: AnyElement = if today_fail > 0 {
        div()
            .id("gw-kpi-failed-jump")
            .text_size(px(11.0))
            .font_weight(gpui_kit::FontWeight::MEDIUM)
            .text_color(to_hsla(p.warn))
            .cursor_pointer()
            .truncate()
            .on_click(cx.listener(|this, _, _, cx| {
                this.traffic_filter = GatewayTrafficFilter::Failed;
                this.gateway_tab = GatewayTab::Traffic;
                this.reload_traffic_page(true, cx);
            }))
            .child(SharedString::from(
                pr.prov_gateway_overview_failures_view_traffic(today_fail),
            ))
            .into_any_element()
    } else {
        div()
            .text_size(px(11.0))
            .text_color(to_hsla(p.muted))
            .truncate()
            .child(pr.prov_gateway_overview_all_routed_ok())
            .into_any_element()
    };

    let mode_sub: AnyElement = div()
        .text_size(px(11.0))
        .text_color(to_hsla(p.muted))
        .truncate()
        .child(pr.prov_gateway_overview_passthrough_sub())
        .into_any_element();

    let ttft_val = median_ttft.map_or_else(|| "—".into(), |ms| format!("{ms} ms"));
    let ttft_sub: AnyElement = div()
        .text_size(px(11.0))
        .text_color(to_hsla(p.muted))
        .truncate()
        .child(if median_ttft.is_some() {
            pr.prov_gateway_overview_median_ttft_sub()
        } else {
            pr.prov_gateway_overview_no_ttft_today()
        })
        .into_any_element();

    let kpis = h_row()
        .gap(px(10.0))
        .child(kpi_card_el(
            pr.prov_gateway_requests_today(),
            today_reqs.to_string(),
            req_sub,
            cx,
        ))
        .child(kpi_card_el(
            pr.prov_gateway_success_rate(),
            ok_rate,
            rate_sub,
            cx,
        ))
        .child(kpi_card_el(
            pr.prov_gateway_overview_passthrough_vs_transcode(),
            format!("{passthrough} / {translated}"),
            mode_sub,
            cx,
        ))
        .child(kpi_card_el(
            pr.prov_gateway_overview_median_ttft(),
            ttft_val,
            ttft_sub,
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
        .child(render_protocol_table(view, cx))
        .child(render_bottom_grid(view, active_clients, cx))
}

fn render_protocol_table(view: &GatewayView, cx: &mut Context<GatewayView>) -> gpui_kit::Div {
    let p = palette(cx);
    let pr = crate::ui::tr(cx).providers();
    let cm = crate::ui::tr(cx).common();

    let mut box_el = v_col().gap(px(6.0)).child(
        h_row()
            .justify_between()
            .pb(px(4.0))
            .child(section_heading(
                pr.prov_gateway_overview_protocol_base_urls_passthrough_or(),
                cx,
            ))
            .child(
                btn_ghost(
                    "gw-ov-view-traffic",
                    pr.prov_gateway_overview_view_traffic(),
                )
                .on_click(cx.listener(|this, _, _, cx| {
                    this.gateway_tab = GatewayTab::Traffic;
                    this.reload_traffic_page(false, cx);
                })),
            ),
    );

    box_el = box_el.child(table_header(
        &[
            (pr.prov_gateway_overview_col_family(), Some(165.0)),
            (pr.prov_gateway_overview_col_base_url(), Some(240.0)),
            (pr.prov_gateway_overview_col_paths(), None),
            (pr.prov_gateway_overview_col_action(), Some(74.0)),
        ],
        cx,
    ));

    if let Some(ov) = &view.overview {
        for (i, b) in ov.base_urls.iter().enumerate() {
            let copy_url = b.url.clone();
            let (fam_lbl, paths_text) = match b.family.as_str() {
                "openai" => (
                    pr.prov_gateway_overview_openai_style(),
                    "/chat/completions, /responses, /embeddings, /models",
                ),
                "anthropic" => (
                    "Anthropic Messages",
                    "/v1/messages, /v1/messages/count_tokens",
                ),
                "gemini" => ("Google Gemini", "/v1beta/models/*"),
                other => (other, "—"),
            };
            box_el = box_el.child(
                h_row()
                    .w_full()
                    .gap(px(8.0))
                    .px(px(10.0))
                    .py(px(7.5))
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.surface)))
                    .child(
                        div()
                            .w(px(165.0))
                            .flex_shrink_0()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(SharedString::from(fam_lbl.to_owned())),
                    )
                    .child(
                        div()
                            .w(px(240.0))
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(SharedString::from(b.url.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .truncate()
                            .child(paths_text),
                    )
                    .child(h_row().w(px(74.0)).flex_shrink_0().justify_end().child(
                        btn_secondary(("copy-gw-url", i), cm.copy()).on_click(cx.listener(
                            move |this, _, _, cx| {
                                let _ = clipboard::set_text(&copy_url);
                                this.status_msg = Some((
                                    Tone::Success,
                                    crate::ui::tr(cx)
                                        .providers()
                                        .prov_gateway_overview_url_copied()
                                        .into(),
                                ));
                                cx.notify();
                            },
                        )),
                    )),
            );
        }
    }

    card(cx).p(px(14.0)).child(box_el)
}

fn render_bottom_grid(
    view: &GatewayView,
    active_clients: usize,
    cx: &mut Context<GatewayView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let pr = crate::ui::tr(cx).providers();

    let mut clients_col = v_col().gap(px(4.0)).child(
        h_row()
            .justify_between()
            .pb(px(4.0))
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
        clients_col = clients_col.child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .py(px(10.0))
                .child(pr.prov_gateway_overview_no_clients_yet_click_connect()),
        );
    } else {
        clients_col = clients_col.child(table_header(
            &[
                (pr.prov_gateway_overview_col_client(), Some(108.0)),
                (pr.prov_gateway_overview_col_token(), Some(112.0)),
                (pr.prov_gateway_overview_col_default_model(), None),
                (pr.prov_gateway_overview_col_last_active(), Some(74.0)),
            ],
            cx,
        ));
        for c in client_rows.iter().filter(|c| !c.revoked).take(5) {
            let scope = if c.allow_all {
                pr.gw_clients_all().into()
            } else {
                c.allowed_models
                    .first()
                    .map(|m| view.format_model_ref(m))
                    .unwrap_or_else(|| pr.prov_gateway_overview_none().into())
            };
            let active_txt = c.last_used_ms.map_or_else(
                || pr.gw_clients_never().into(),
                |ms| format_relative_time(ms, crate::ui::active_language(cx)),
            );
            clients_col = clients_col.child(
                h_row()
                    .w_full()
                    .gap(px(8.0))
                    .px(px(10.0))
                    .py(px(6.5))
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.surface)))
                    .child(
                        div()
                            .w(px(108.0))
                            .flex_shrink_0()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(SharedString::from(c.name.clone())),
                    )
                    .child(
                        div()
                            .w(px(112.0))
                            .flex_shrink_0()
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.muted))
                            .truncate()
                            .child(SharedString::from(c.token_hint.clone())),
                    )
                    .child(div().flex_1().min_w_0().child(badge(
                        SharedString::from(scope),
                        Tone::Accent,
                        cx,
                    )))
                    .child(
                        div()
                            .w(px(74.0))
                            .flex_shrink_0()
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.muted))
                            .truncate()
                            .child(SharedString::from(active_txt)),
                    ),
            );
        }
    }

    let mut rules_col = v_col().gap(px(4.0)).child(
        h_row()
            .justify_between()
            .pb(px(4.0))
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
        rules_col = rules_col.child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .py(px(10.0))
                .child(pr.prov_gateway_overview_no_fallback_rules_failures_return()),
        );
    } else {
        rules_col = rules_col.child(table_header(
            &[
                (pr.prov_gateway_overview_col_primary(), Some(148.0)),
                (pr.prov_gateway_overview_col_chain(), None),
                (pr.prov_gateway_overview_col_hits_today(), Some(58.0)),
            ],
            cx,
        ));
        for rule in view.gateway_rules.iter().take(5) {
            let primary_disp = format!(
                "{}/{}",
                view.provider_name(&rule.primary.provider_id),
                rule.primary.model
            );
            let chain_txt = rule
                .fallbacks
                .iter()
                .map(|m| format!("{}/{}", view.provider_name(&m.provider_id), m.model))
                .collect::<Vec<_>>()
                .join(" → ");
            let hits = view.today_hits_for_rule(rule);
            rules_col = rules_col.child(
                h_row()
                    .w_full()
                    .gap(px(8.0))
                    .px(px(10.0))
                    .py(px(6.5))
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.surface)))
                    .child(
                        div()
                            .w(px(148.0))
                            .flex_shrink_0()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(SharedString::from(primary_disp)),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.muted))
                            .truncate()
                            .child(SharedString::from(chain_txt)),
                    )
                    .child(
                        div()
                            .w(px(58.0))
                            .flex_shrink_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink_2))
                            .child(hits.to_string()),
                    ),
            );
        }
    }

    h_row()
        .gap(px(14.0))
        .items_start()
        .child(card(cx).flex_1().min_w_0().p(px(14.0)).child(clients_col))
        .child(card(cx).flex_1().min_w_0().p(px(14.0)).child(rules_col))
}

fn kpi_card_el(
    label: &'static str,
    value: String,
    sub: AnyElement,
    cx: &gpui_kit::App,
) -> gpui_kit::Div {
    let p = palette(cx);
    card(cx).flex_1().min_w_0().p(px(13.0)).child(
        v_col()
            .gap(px(4.0))
            .child(div().text_xs().text_color(to_hsla(p.muted)).child(label))
            .child(
                div()
                    .text_xl()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(to_hsla(p.ink))
                    .truncate()
                    .child(SharedString::from(value)),
            )
            .child(sub),
    )
}
