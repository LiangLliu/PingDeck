//! Usage Statistics (`统计`) view: 9-preset filter bar, 4 KPI cards, 5-metric interactive chart,
//! and 6-dimension clickable breakdown table with drill-down into Calls.

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    Styled as _, div, px,
};
use pingdeck_desktop::core::app::current_time_ms;
use pingdeck_desktop::time::format_local_date;

use crate::ui::{
    Anchor, ScrollableElement as _, UiIcon, btn_ghost, h_row, icon_xs, kpi_card, list_row, palette,
    popover, popover_header, select_pill, to_hsla, v_col,
};

use super::breakdown::render_breakdown_card;
use super::chart::render_trend_chart;
use super::state::{RangePreset, UsageView, fmt_compact, fmt_cost_map, fmt_latency};

pub fn render_overview(view: &UsageView, cx: &mut Context<UsageView>) -> AnyElement {
    let body = v_col()
        .w_full()
        .p(px(18.0))
        .gap(px(14.0))
        .child(render_filter_bar(view, cx))
        .child(render_kpi_grid(view, cx))
        .child(render_trend_chart(view, cx))
        .child(render_breakdown_card(view, cx));

    div()
        .id("usage-overview-scroll")
        .size_full()
        .overflow_y_scrollbar()
        .child(body)
        .into_any_element()
}

fn render_filter_bar(view: &UsageView, cx: &mut Context<UsageView>) -> AnyElement {
    let p = palette(cx);
    let lang = view.lang;
    let entity = cx.entity().clone();
    let now = current_time_ms();
    let (from_ms, to_ms, _) = view.range.bounds(now);
    let date_span = if view.range == RangePreset::All {
        crate::ui::Tr(lang)
            .usage()
            .usage_overview_all_recorded_time()
            .to_owned()
    } else {
        crate::ui::Tr(lang)
            .usage()
            .usage_overview_date_range(format_local_date(from_ms), format_local_date(to_ms))
    };

    let range_ent = entity.clone();
    let active_range = view.range;
    let range_pop = popover("usage-range-pop")
        .anchor(Anchor::TopLeft)
        .trigger(select_pill(
            "usage-range-trigger",
            format!(
                "{} {}",
                crate::ui::Tr(lang).usage().usage_overview_range(),
                view.range.short_label(lang)
            ),
            view.range != RangePreset::Day7,
            cx,
        ))
        .content(move |_, _w, pop_cx| {
            let mut m = v_col().w(px(176.0)).gap(px(2.0)).child(popover_header(
                crate::ui::tr(pop_cx).usage().usage_overview_time_range(),
                None,
                pop_cx,
            ));
            for preset in RangePreset::ALL {
                let ent = range_ent.clone();
                let pop_h = pop_cx.entity().clone();
                m = m.child(pick_row(
                    SharedString::from(format!("u-rng-{preset:?}")),
                    preset.short_label(lang),
                    active_range == preset,
                    pop_cx,
                    move |_, w, cx| {
                        ent.update(cx, |this, cx| {
                            this.range = preset;
                            this.reload_all(cx);
                        });
                        pop_h.update(cx, |s, cx| s.dismiss(w, cx));
                    },
                ));
            }
            m
        });

    let client_ent = entity.clone();
    let clients_snap: Vec<(String, String)> = view
        .clients
        .iter()
        .map(|c| (c.id.clone(), c.name.clone()))
        .collect();
    let active_cid = view.filter_client_id.clone();
    let client_lbl = active_cid
        .as_deref()
        .map(|id| {
            format!(
                "{} {}",
                crate::ui::Tr(view.lang).usage().usage_dim_client(),
                view.client_name(id)
            )
        })
        .unwrap_or_else(|| {
            crate::ui::Tr(view.lang)
                .usage()
                .usage_overview_client_all()
                .to_owned()
        });
    let client_pop = popover("usage-client-pop")
        .anchor(Anchor::TopLeft)
        .trigger(select_pill(
            "usage-client-trigger",
            client_lbl,
            view.filter_client_id.is_some(),
            cx,
        ))
        .content(move |_, _w, pop_cx| {
            let mut m = v_col().w(px(196.0)).gap(px(2.0)).child(popover_header(
                crate::ui::tr(pop_cx)
                    .usage()
                    .usage_overview_gateway_client(),
                None,
                pop_cx,
            ));
            let ent0 = client_ent.clone();
            let pop0 = pop_cx.entity().clone();
            m = m.child(pick_row(
                "u-cli-all",
                crate::ui::tr(pop_cx).usage().usage_overview_all_clients(),
                active_cid.is_none(),
                pop_cx,
                move |_, w, cx| {
                    ent0.update(cx, |this, cx| {
                        this.filter_client_id = None;
                        this.reload_all(cx);
                    });
                    pop0.update(cx, |st, cx| st.dismiss(w, cx));
                },
            ));
            for (cid, name) in &clients_snap {
                let ent = client_ent.clone();
                let pop_h = pop_cx.entity().clone();
                let cid_c = cid.clone();
                m = m.child(pick_row(
                    SharedString::from(format!("u-cli-{cid}")),
                    name.clone(),
                    active_cid.as_deref() == Some(cid.as_str()),
                    pop_cx,
                    move |_, w, cx| {
                        ent.update(cx, |this, cx| {
                            this.filter_client_id = Some(cid_c.clone());
                            this.reload_all(cx);
                        });
                        pop_h.update(cx, |st, cx| st.dismiss(w, cx));
                    },
                ));
            }
            m
        });

    let key_ent = entity.clone();
    let keys_snap: Vec<(String, String)> = view
        .keys
        .iter()
        .filter(|k| {
            view.scope_provider_id
                .as_deref()
                .is_none_or(|pid| k.provider_id == pid)
        })
        .map(|k| (k.id.clone(), k.name.clone()))
        .collect();
    let active_kid = view.filter_key_id.clone();
    let key_lbl = active_kid
        .as_deref()
        .map(|id| {
            format!(
                "{} {}",
                crate::ui::Tr(view.lang).usage().usage_dim_key(),
                view.key_name(id)
            )
        })
        .unwrap_or_else(|| {
            crate::ui::Tr(view.lang)
                .usage()
                .usage_overview_key_all()
                .to_owned()
        });
    let key_pop = popover("usage-key-pop")
        .anchor(Anchor::TopLeft)
        .trigger(select_pill(
            "usage-key-trigger",
            key_lbl,
            view.filter_key_id.is_some(),
            cx,
        ))
        .content(move |_, _w, pop_cx| {
            let mut m = v_col().w(px(196.0)).gap(px(2.0)).child(popover_header(
                crate::ui::tr(pop_cx).usage().usage_overview_provider_key(),
                None,
                pop_cx,
            ));
            let ent0 = key_ent.clone();
            let pop0 = pop_cx.entity().clone();
            m = m.child(pick_row(
                "u-key-all",
                crate::ui::tr(pop_cx).usage().usage_overview_all_keys(),
                active_kid.is_none(),
                pop_cx,
                move |_, w, cx| {
                    ent0.update(cx, |this, cx| {
                        this.filter_key_id = None;
                        this.reload_all(cx);
                    });
                    pop0.update(cx, |st, cx| st.dismiss(w, cx));
                },
            ));
            for (kid, name) in &keys_snap {
                let ent = key_ent.clone();
                let pop_h = pop_cx.entity().clone();
                let kid_c = kid.clone();
                m = m.child(pick_row(
                    SharedString::from(format!("u-key-{kid}")),
                    name.clone(),
                    active_kid.as_deref() == Some(kid.as_str()),
                    pop_cx,
                    move |_, w, cx| {
                        ent.update(cx, |this, cx| {
                            this.filter_key_id = Some(kid_c.clone());
                            this.reload_all(cx);
                        });
                        pop_h.update(cx, |st, cx| st.dismiss(w, cx));
                    },
                ));
            }
            m
        });

    let has_filter = view.filter_source.is_some()
        || view.filter_model.is_some()
        || view.filter_key_id.is_some()
        || view.filter_client_id.is_some();

    h_row()
        .w_full()
        .gap(px(6.0))
        .flex_wrap()
        .child(range_pop)
        .child(key_pop)
        .child(client_pop)
        .children(has_filter.then(|| {
            btn_ghost(
                "usage-reset-flt",
                crate::ui::Tr(view.lang).usage().usage_overview_reset(),
            )
            .on_click(cx.listener(|this, _, window, cx| {
                this.clear_filters(window, cx);
            }))
        }))
        .child(div().flex_1())
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(date_span),
        )
        .into_any_element()
}

fn render_kpi_grid(view: &UsageView, cx: &mut Context<UsageView>) -> AnyElement {
    let totals = view
        .summary
        .as_ref()
        .map(|s| s.totals.clone())
        .unwrap_or_default();

    let denom = totals.requests.saturating_sub(totals.canceled);
    let fail_pct = if denom > 0 {
        totals.failed as f64 / denom as f64 * 100.0
    } else {
        0.0
    };
    let calls_sub = crate::ui::tr(cx).usage().usage_overview_failed_canceled(
        totals.failed,
        format!("{:.1}", fail_pct),
        totals.canceled,
    );

    let cost_val = fmt_cost_map(&totals.cost_micro);
    let unpriced = view
        .summary
        .as_ref()
        .map_or(0, |s| s.models_without_price.len());
    let cost_sub = if unpriced > 0 {
        crate::ui::tr(cx)
            .usage()
            .usage_overview_model_s_unpriced(unpriced)
    } else {
        crate::ui::Tr(view.lang)
            .usage()
            .usage_overview_estimated_from_local_catalog()
            .to_owned()
    };

    let tok_sub = crate::ui::tr(cx).usage().usage_overview_in_out_cache(
        fmt_compact(totals.input_tokens),
        fmt_compact(totals.output_tokens),
        fmt_compact(totals.cached_tokens),
    );

    let (mut ttft_sum, mut ttft_cnt, mut lat_sum, mut lat_cnt) = (0u64, 0u64, 0u64, 0u64);
    for lm in &view.latency_modes {
        ttft_sum = ttft_sum.saturating_add(lm.ttft_ms);
        ttft_cnt = ttft_cnt.saturating_add(lm.ttft_requests);
        lat_sum = lat_sum.saturating_add(lm.latency_ms);
        lat_cnt = lat_cnt.saturating_add(lm.timed_requests);
    }
    let avg_ttft = (ttft_cnt > 0).then(|| ttft_sum / ttft_cnt);
    let avg_e2e = (lat_cnt > 0).then(|| lat_sum / lat_cnt);
    let lat_val = avg_ttft
        .or(avg_e2e)
        .map_or_else(|| "—".to_owned(), fmt_latency);
    let lat_sub = match (avg_ttft, avg_e2e) {
        (Some(_), Some(e2e)) => crate::ui::tr(cx)
            .usage()
            .usage_overview_avg_ttft_e2e(fmt_latency(e2e)),
        (None, Some(_)) => crate::ui::Tr(view.lang)
            .usage()
            .usage_overview_avg_end_to_end()
            .to_owned(),
        _ => crate::ui::Tr(view.lang)
            .usage()
            .usage_overview_no_latency_samples()
            .to_owned(),
    };

    h_row()
        .w_full()
        .gap(px(10.0))
        .child(kpi_card(
            crate::ui::Tr(view.lang).usage().usage_kpi_calls(),
            fmt_compact(totals.requests),
            Some(SharedString::from(calls_sub)),
            cx,
        ))
        .child(kpi_card(
            crate::ui::Tr(view.lang)
                .usage()
                .usage_overview_estimated_cost(),
            cost_val,
            Some(SharedString::from(cost_sub)),
            cx,
        ))
        .child(kpi_card(
            "Token",
            fmt_compact(totals.total_tokens()),
            Some(SharedString::from(tok_sub)),
            cx,
        ))
        .child(kpi_card(
            crate::ui::Tr(view.lang).usage().usage_kpi_latency(),
            lat_val,
            Some(SharedString::from(lat_sub)),
            cx,
        ))
        .into_any_element()
}

fn pick_row(
    id: impl Into<gpui_kit::ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    cx: &gpui_kit::App,
    on_click: impl Fn(&gpui_kit::ClickEvent, &mut gpui_kit::Window, &mut gpui_kit::App) + 'static,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let p = palette(cx);
    list_row(id, selected, cx, on_click)
        .justify_between()
        .child(div().text_xs().truncate().child(label.into()))
        .children(selected.then(|| {
            div()
                .text_color(to_hsla(p.accent))
                .child(icon_xs(UiIcon::Check))
        }))
}
