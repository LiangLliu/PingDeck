//! Usage Call Log (`调用`) view matching `docs/design/prototype/index.html:1493-1521`.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::state::SettingsSection;
use pingdeck_desktop::time::format_local_time;

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_ghost, btn_secondary, choice_chip, data_row,
    empty_state, h_row, icon_xs, palette, search_input, table_header, to_hsla, v_col,
};

use super::state::{
    CallsQuickFilter, UsageHandoff, UsageModal, UsageView, fmt_compact, fmt_cost_micro,
    fmt_latency, is_failed_over, source_label,
};

pub fn render_calls(view: &UsageView, cx: &mut Context<UsageView>) -> AnyElement {
    let p = palette(cx);
    let search_raw = view.calls_search_input.read(cx).value().to_string();
    let rows = view.filtered_logs(&search_raw);

    let mut pills = h_row().gap(px(4.0)).flex_shrink_0();
    for q in CallsQuickFilter::ALL {
        pills = pills.child(
            choice_chip(
                SharedString::from(format!("u-cq-{q:?}")),
                q.label(view.lang),
                view.calls_quick == q,
                cx,
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.calls_quick = q;
                cx.notify();
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
                .child(search_input(&view.calls_search_input, cx)),
        )
        .child(pills)
        .children(view.capture_stats.filter(|s| s.count > 0).map(|st| {
            btn_ghost(
                "usage-clr-cap",
                crate::ui::tr(cx)
                    .usage()
                    .usage_calls_clear_captures_kb(st.bytes / 1024),
            )
            .on_click(cx.listener(move |this, _, _, cx| {
                this.active_modal = Some(UsageModal::ClearCaptures { stats: st });
                cx.notify();
            }))
        }))
        .child(
            btn_ghost(
                "usage-clr-logs",
                crate::ui::Tr(view.lang).usage().usage_calls_clear_logs(),
            )
            .icon(icon_xs(UiIcon::Trash))
            .on_click(cx.listener(|this, _, _, cx| this.open_clear_logs_modal(cx))),
        );

    let mut table = v_col().w_full().gap(px(4.0)).child(table_header(
        &[
            (crate::ui::Tr(view.lang).usage().log_col_time(), Some(74.0)),
            (
                crate::ui::Tr(view.lang).usage().usage_calls_source_client(),
                Some(150.0),
            ),
            (
                crate::ui::Tr(view.lang).usage().usage_calls_model_route(),
                None,
            ),
            (
                crate::ui::Tr(view.lang).usage().log_status_group(),
                Some(110.0),
            ),
            (
                crate::ui::Tr(view.lang).usage().usage_calls_ttft_total(),
                Some(110.0),
            ),
            (
                crate::ui::Tr(view.lang).usage().usage_calls_in_out(),
                Some(108.0),
            ),
            (
                crate::ui::Tr(view.lang).usage().usage_kpi_cost(),
                Some(78.0),
            ),
        ],
        cx,
    ));

    if rows.is_empty() {
        table = table.child(empty_state(
            UiIcon::Usage,
            crate::ui::Tr(view.lang)
                .usage()
                .usage_calls_no_matching_call_logs(),
            crate::ui::Tr(view.lang)
                .usage()
                .usage_calls_adjust_filters_above_or_send(),
            cx,
        ));
    } else {
        for row in &rows {
            let rid = row.id;
            let src_text = match &row.client_name {
                Some(c) if !c.is_empty() => {
                    format!("{} · {c}", source_label(&row.source, view.lang))
                }
                _ => source_label(&row.source, view.lang).to_owned(),
            };
            let route_text = match &row.upstream_model {
                Some(up) if up != &row.requested_model => {
                    format!("{} → {up}", row.requested_model)
                }
                _ => row.requested_model.clone(),
            };
            let failover = is_failed_over(row);
            let (st_label, st_tone) = if row.outcome == "canceled" {
                (
                    crate::ui::Tr(view.lang)
                        .usage()
                        .log_flag_canceled()
                        .to_owned(),
                    Tone::Neutral,
                )
            } else if failover && row.status >= 200 && row.status < 300 {
                (
                    format!(
                        "{} · {}",
                        row.status,
                        crate::ui::Tr(view.lang).usage().usage_calls_failover()
                    ),
                    Tone::Warning,
                )
            } else if row.status >= 200 && row.status < 300 {
                let extra = if row.finish_reason.as_deref() == Some("tool_calls") {
                    format!(
                        "{} · {}",
                        row.status,
                        crate::ui::Tr(view.lang).common().cap_tools()
                    )
                } else {
                    row.status.to_string()
                };
                (extra, Tone::Success)
            } else if row.status >= 400 {
                (row.status.to_string(), Tone::Danger)
            } else {
                (
                    crate::ui::Tr(view.lang)
                        .common()
                        .banner_tone_bad()
                        .to_owned(),
                    Tone::Danger,
                )
            };
            let lat_text = match row.ttft_ms {
                Some(ttft) => format!("{} / {}", fmt_latency(ttft), fmt_latency(row.latency_ms)),
                None => format!("— / {}", fmt_latency(row.latency_ms)),
            };
            let tok_text = format!(
                "{} / {}",
                fmt_compact(row.input_tokens.unwrap_or(0)),
                fmt_compact(row.output_tokens.unwrap_or(0))
            );
            let cost_text = row
                .cost_micro_usd
                .map(|m| fmt_cost_micro(m, &row.cost_currency))
                .unwrap_or_else(|| "—".to_owned());

            table = table.child(
                div()
                    .id(SharedString::from(format!("u-call-row-{rid}")))
                    .cursor_pointer()
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_call_drawer(rid, window, cx);
                    }))
                    .child(
                        data_row(cx)
                            .child(
                                div()
                                    .w(px(74.0))
                                    .text_xs()
                                    .text_color(to_hsla(p.muted))
                                    .child(format_local_time(row.ts_ms)),
                            )
                            .child(
                                div()
                                    .w(px(150.0))
                                    .text_xs()
                                    .text_color(to_hsla(p.ink_2))
                                    .truncate()
                                    .child(src_text),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(route_text),
                            )
                            .child(div().w(px(110.0)).child(badge(st_label, st_tone, cx)))
                            .child(
                                div()
                                    .w(px(110.0))
                                    .text_xs()
                                    .text_color(to_hsla(p.ink_2))
                                    .truncate()
                                    .child(lat_text),
                            )
                            .child(
                                div()
                                    .w(px(108.0))
                                    .text_xs()
                                    .text_color(to_hsla(p.ink_2))
                                    .truncate()
                                    .child(tok_text),
                            )
                            .child(
                                div()
                                    .w(px(78.0))
                                    .text_xs()
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(cost_text),
                            ),
                    ),
            );
        }
    }

    let load_more = view.logs_has_more.then(|| {
        h_row().w_full().justify_center().pt(px(6.0)).child(
            btn_secondary(
                "usage-load-more",
                crate::ui::Tr(view.lang).usage().usage_calls_load_more(),
            )
            .on_click(cx.listener(|this, _, _, cx| this.load_more_logs(cx))),
        )
    });

    let footer_bar = h_row()
        .w_full()
        .justify_between()
        .pt(px(8.0))
        .child(
            div()
                .text_size(px(11.0))
                .text_color(to_hsla(p.muted))
                .child(
                    crate::ui::Tr(view.lang)
                        .usage()
                        .usage_calls_call_logs_keep_every_request(),
                ),
        )
        .child(
            btn_ghost(
                "usage-go-retention",
                crate::ui::tr(cx)
                    .usage()
                    .usage_calls_retention_d(view.retention_days),
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.pending_handoff
                    .set(Some(UsageHandoff::OpenSettings(SettingsSection::Usage)));
                cx.notify();
            })),
        );

    let body = v_col()
        .w_full()
        .p(px(18.0))
        .gap(px(12.0))
        .child(filter_bar)
        .child(table)
        .children(load_more)
        .child(footer_bar);

    div()
        .id("usage-calls-scroll")
        .size_full()
        .overflow_y_scrollbar()
        .child(body)
        .into_any_element()
}
