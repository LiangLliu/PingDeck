//! Built-in Gateway Traffic Log tab (`GatewayTab::Traffic`): live proxy traffic scoped strictly
//! to `source = "gateway"` (including 401/403/429 pre-routing rejections, passthrough vs transcode
//! indicators, failover hops, and click-to-inspect shared Call Detail Drawer).

use gpui_kit::{
    Context, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::time::format_local_time;

use crate::ui::{
    DEFAULT_PAGE_SIZES, PaginationBar, Tone, UiIcon, badge, btn_ghost, btn_secondary, card,
    choice_chip, data_row, empty_state, h_row, icon_xs, pagination_bar, palette, search_input,
    table_header, to_hsla, v_col,
};
use crate::views::usage::state::{fmt_latency, is_failed_over};

use super::traffic_actions::GatewayTrafficFilter;
use super::{GatewayHandoff, GatewayView};

pub fn render_traffic_tab(view: &GatewayView, cx: &mut Context<GatewayView>) -> gpui_kit::Div {
    let p = palette(cx);
    let pr = crate::ui::tr(cx).providers();
    let cm = crate::ui::tr(cx).common();

    let mut chips = h_row().gap(px(4.0)).flex_shrink_0();
    for f in GatewayTrafficFilter::ALL {
        chips = chips.child(
            choice_chip(
                SharedString::from(format!("gw-tf-{f:?}")),
                f.label(cx),
                view.traffic_filter == f,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.traffic_filter = f;
                this.reload_traffic_page(true, cx);
            })),
        );
    }

    let filter_bar = h_row()
        .w_full()
        .gap(px(8.0))
        .flex_wrap()
        .child(
            div()
                .flex_1()
                .min_w(px(220.0))
                .child(search_input(&view.traffic_search_input, cx)),
        )
        .child(chips)
        .child(
            btn_ghost("gw-tf-refresh", cm.refresh())
                .icon(icon_xs(UiIcon::Refresh))
                .on_click(cx.listener(|this, _, _, cx| this.reload_traffic_page(false, cx))),
        );

    let mut table = v_col().w_full().gap(px(4.0)).child(table_header(
        &[
            (pr.prov_gateway_traffic_col_time(), Some(74.0)),
            (pr.prov_gateway_traffic_col_client(), Some(144.0)),
            (pr.prov_gateway_traffic_col_model(), None),
            (pr.prov_gateway_traffic_col_mode(), Some(94.0)),
            (pr.prov_gateway_traffic_col_status(), Some(112.0)),
            (pr.prov_gateway_traffic_col_latency(), Some(112.0)),
        ],
        cx,
    ));

    if view.traffic_logs.is_empty() {
        table = table.child(empty_state(
            UiIcon::Zap,
            pr.prov_gateway_traffic_empty_title(),
            pr.prov_gateway_traffic_empty_desc(),
            cx,
        ));
    } else {
        let us = crate::ui::tr(cx).usage();
        for row in &view.traffic_logs {
            let rid = row.id;
            let client_text = match (&row.client_name, &row.peer) {
                (Some(name), _) if !name.is_empty() => name.clone(),
                (_, Some(peer)) if !peer.is_empty() => {
                    format!("{} · {peer}", pr.prov_gateway_traffic_anonymous())
                }
                _ => pr.prov_gateway_traffic_anonymous().to_owned(),
            };
            let route_text = match &row.upstream_model {
                Some(up) if !up.is_empty() && up != &row.requested_model => {
                    format!("{} → {up}", row.requested_model)
                }
                _ if !row.requested_model.is_empty() => row.requested_model.clone(),
                _ => "—".to_owned(),
            };
            let (mode_lbl, mode_tone) = if row.provider_id.is_none() {
                (pr.prov_gateway_traffic_mode_blocked(), Tone::Danger)
            } else if row.translated {
                (pr.prov_gateway_traffic_mode_transcode(), Tone::Accent)
            } else {
                (pr.prov_gateway_traffic_mode_passthrough(), Tone::Neutral)
            };
            let failover = is_failed_over(row);
            let (st_label, st_tone) = if row.outcome == "canceled" {
                (us.log_flag_canceled().to_owned(), Tone::Neutral)
            } else if failover && row.status >= 200 && row.status < 300 {
                (
                    format!("{} · {}", row.status, us.usage_calls_failover()),
                    Tone::Warning,
                )
            } else if row.status >= 200 && row.status < 300 {
                (row.status.to_string(), Tone::Success)
            } else if row.status >= 400 {
                (row.status.to_string(), Tone::Danger)
            } else {
                (cm.banner_tone_bad().to_owned(), Tone::Danger)
            };
            let lat_text = match row.ttft_ms {
                Some(ttft) => format!("{} / {}", fmt_latency(ttft), fmt_latency(row.latency_ms)),
                None => format!("— / {}", fmt_latency(row.latency_ms)),
            };

            table = table.child(
                div()
                    .id(SharedString::from(format!("gw-tf-row-{rid}")))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_traffic_call_drawer(rid, window, cx);
                    }))
                    .child(
                        data_row(cx)
                            .child(
                                div()
                                    .w(px(74.0))
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(to_hsla(p.muted))
                                    .child(format_local_time(row.ts_ms)),
                            )
                            .child(
                                div()
                                    .w(px(144.0))
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(to_hsla(p.ink_2))
                                    .truncate()
                                    .child(SharedString::from(client_text)),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(SharedString::from(route_text)),
                            )
                            .child(
                                div()
                                    .w(px(94.0))
                                    .flex_shrink_0()
                                    .child(badge(mode_lbl, mode_tone, cx)),
                            )
                            .child(
                                div()
                                    .w(px(112.0))
                                    .flex_shrink_0()
                                    .child(badge(st_label, st_tone, cx)),
                            )
                            .child(
                                div()
                                    .w(px(112.0))
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(to_hsla(p.ink_2))
                                    .truncate()
                                    .child(SharedString::from(lat_text)),
                            ),
                    ),
            );
        }
    }

    let new_banner = (view.traffic_pending_live > 0).then(|| {
        h_row().w_full().justify_center().child(
            btn_secondary(
                "gw-tf-new-jump",
                pr.prov_gateway_traffic_new_logs(view.traffic_pending_live),
            )
            .icon(icon_xs(UiIcon::Refresh))
            .on_click(cx.listener(|this, _, _, cx| this.set_traffic_page(0, cx))),
        )
    });

    let pager = (view.traffic_total > 0).then(|| {
        div()
            .w_full()
            .pt(px(8.0))
            .border_t_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)))
            .child(pagination_bar(
                PaginationBar {
                    id_prefix: "gw-tf-pg",
                    page: view.traffic_page,
                    page_size: view.traffic_page_size,
                    total: view.traffic_total as usize,
                    page_sizes: DEFAULT_PAGE_SIZES,
                    loading: view.traffic_loading || view.traffic_body_searching,
                    compact: false,
                },
                |this, next_page, cx| this.set_traffic_page(next_page, cx),
                Some(
                    |this: &mut GatewayView, next_size, cx: &mut Context<GatewayView>| {
                        this.set_traffic_page_size(next_size, cx);
                    },
                ),
                cx,
            ))
    });

    let footer = h_row()
        .w_full()
        .justify_between()
        .gap(px(8.0))
        .pt(px(2.0))
        .child(
            div()
                .text_size(px(11.0))
                .text_color(to_hsla(p.muted))
                .child(pr.prov_gateway_traffic_footer_note()),
        )
        .child(
            btn_ghost("gw-tf-analytics", pr.prov_gateway_traffic_open_analytics()).on_click(
                cx.listener(|this, _, _, cx| {
                    this.pending_handoff
                        .set(Some(GatewayHandoff::OpenUsageGateway));
                    cx.notify();
                }),
            ),
        );

    v_col()
        .w_full()
        .gap(px(12.0))
        .child(filter_bar)
        .children(new_banner)
        .child(card(cx).p(px(12.0)).child(table))
        .children(pager)
        .child(footer)
}
