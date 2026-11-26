//! 6-dimension clickable usage breakdown card (Model, Provider, Source, Client, Key, Failures).

use std::collections::BTreeMap;

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::state::SettingsSection;

use crate::ui::{
    PaginationBar, Tone, UiIcon, badge, btn_ghost, card, data_row, empty_inline, h_row, icon_xs,
    page_slice, pagination_bar, palette, tab_bar_underline, tab_item, table_header, to_hsla, v_col,
};

use super::state::{
    BreakdownDim, UsageHandoff, UsageView, fmt_compact, fmt_cost_map, fmt_latency, source_label,
};

const BREAKDOWN_PAGE_SIZE: usize = 20;

fn breakdown_pager(
    id_prefix: &'static str,
    page: usize,
    total: usize,
    cx: &mut Context<UsageView>,
) -> Option<gpui_kit::Div> {
    (total > BREAKDOWN_PAGE_SIZE).then(|| {
        div().pt(px(6.0)).child(pagination_bar(
            PaginationBar {
                id_prefix,
                page,
                page_size: BREAKDOWN_PAGE_SIZE,
                total,
                page_sizes: &[],
                loading: false,
                compact: false,
            },
            |this, next_page, cx| {
                this.breakdown_page = next_page;
                cx.notify();
            },
            None::<fn(&mut UsageView, usize, &mut Context<UsageView>)>,
            cx,
        ))
    })
}

pub fn render_breakdown_card(view: &UsageView, cx: &mut Context<UsageView>) -> AnyElement {
    let p = palette(cx);
    let fail_total: u64 = view.failures.iter().map(|f| f.count).sum();

    let mut tab_bar = tab_bar_underline("usage-dim-tabs")
        .selected_index(view.breakdown_dim.index())
        .on_click(cx.listener(|this, idx: &usize, _, cx| {
            this.breakdown_dim = BreakdownDim::from_index(*idx);
            this.breakdown_page = 0;
            this.reload_analytics_only(cx);
        }));
    for dim in BreakdownDim::ALL {
        let lbl = if dim == BreakdownDim::Failure && fail_total > 0 {
            SharedString::from(format!("{} ({fail_total})", dim.label(view.lang)))
        } else {
            SharedString::from(dim.label(view.lang))
        };
        tab_bar = tab_bar.child(tab_item(lbl));
    }

    let pricing_link = btn_ghost(
        "usage-go-pricing",
        crate::ui::Tr(view.lang).usage().usage_breakdown_pricing(),
    )
    .icon(icon_xs(UiIcon::ExternalLink))
    .on_click(cx.listener(|this, _, _, cx| {
        this.pending_handoff
            .set(Some(UsageHandoff::OpenSettings(SettingsSection::Catalog)));
        cx.notify();
    }));

    let mut box_el = card(cx).w_full().p(px(12.0)).gap(px(8.0)).child(
        h_row()
            .w_full()
            .justify_between()
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)))
            .child(tab_bar)
            .child(pricing_link),
    );

    if view.breakdown_dim == BreakdownDim::Failure {
        box_el = box_el.child(render_failure_table(view, cx));
    } else if view.breakdown_dim == BreakdownDim::Model {
        box_el = box_el.child(render_model_table(view, cx));
    } else {
        box_el = box_el.child(render_group_table(view, cx));
    }

    box_el.into_any_element()
}

fn render_model_table(view: &UsageView, cx: &mut Context<UsageView>) -> AnyElement {
    let p = palette(cx);
    let rows = view
        .summary
        .as_ref()
        .map_or(&[][..], |s| s.by_model.as_slice());
    if rows.is_empty() {
        return empty_inline(
            crate::ui::Tr(view.lang)
                .usage()
                .usage_breakdown_no_model_usage_in_range(),
            None,
            None,
            cx,
        )
        .into_any_element();
    }
    let mut list = v_col().w_full().gap(px(4.0)).child(table_header(
        &[
            (
                crate::ui::Tr(view.lang)
                    .usage()
                    .usage_breakdown_model_click_to_inspect(),
                None,
            ),
            (
                crate::ui::Tr(view.lang).usage().usage_dim_provider(),
                Some(120.0),
            ),
            (
                crate::ui::Tr(view.lang).usage().usage_kpi_calls(),
                Some(70.0),
            ),
            (
                crate::ui::Tr(view.lang).usage().usage_breakdown_fail(),
                Some(60.0),
            ),
            (
                crate::ui::Tr(view.lang).usage().usage_breakdown_avg_lat(),
                Some(80.0),
            ),
            (
                crate::ui::Tr(view.lang).usage().usage_breakdown_tokens(),
                Some(100.0),
            ),
            (
                crate::ui::Tr(view.lang).usage().usage_breakdown_cost(),
                Some(96.0),
            ),
        ],
        cx,
    ));

    let (page_rows, clamped_page, _) = page_slice(rows, view.breakdown_page, BREAKDOWN_PAGE_SIZE);
    for (idx, row) in page_rows.iter().enumerate() {
        let model_click = row.model.clone();
        let avg_lat = view
            .latency_by_model
            .get(&(row.provider_id.clone(), row.model.clone()))
            .filter(|(_, c)| *c > 0)
            .map_or_else(|| "—".to_owned(), |(s, c)| fmt_latency(s / c));
        let cost = if row.free {
            crate::ui::Tr(view.lang).common().free().to_owned()
        } else {
            fmt_cost_map(&row.usage.cost_micro)
        };
        list = list.child(
            div()
                .id(SharedString::from(format!("u-dim-m-{clamped_page}-{idx}")))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.drill_down(BreakdownDim::Model, model_click.clone(), window, cx);
                }))
                .child(
                    data_row(cx)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.accent_ink))
                                .truncate()
                                .child(row.model.clone()),
                        )
                        .child(
                            div()
                                .w(px(120.0))
                                .text_xs()
                                .text_color(to_hsla(p.ink_2))
                                .truncate()
                                .child(view.provider_name(&row.provider_id)),
                        )
                        .child(
                            div()
                                .w(px(70.0))
                                .text_xs()
                                .text_color(to_hsla(p.ink))
                                .child(fmt_compact(row.usage.requests)),
                        )
                        .child(
                            div()
                                .w(px(60.0))
                                .text_xs()
                                .text_color(to_hsla(if row.usage.failed > 0 {
                                    p.warn
                                } else {
                                    p.muted
                                }))
                                .child(row.usage.failed.to_string()),
                        )
                        .child(
                            div()
                                .w(px(80.0))
                                .text_xs()
                                .text_color(to_hsla(p.ink_2))
                                .child(avg_lat),
                        )
                        .child(
                            div()
                                .w(px(100.0))
                                .text_xs()
                                .text_color(to_hsla(p.ink_2))
                                .child(fmt_compact(row.usage.total_tokens())),
                        )
                        .child(
                            div()
                                .w(px(96.0))
                                .text_xs()
                                .text_color(to_hsla(p.ink))
                                .child(cost),
                        ),
                ),
        );
    }
    list.children(breakdown_pager("u-dim-m-pg", clamped_page, rows.len(), cx))
        .into_any_element()
}

fn render_group_table(view: &UsageView, cx: &mut Context<UsageView>) -> AnyElement {
    let p = palette(cx);
    let groups = view
        .summary
        .as_ref()
        .map_or(&[][..], |s| s.groups.as_slice());
    if groups.is_empty() {
        return empty_inline(
            crate::ui::Tr(view.lang)
                .usage()
                .usage_breakdown_no_rows_for_this_breakdown(),
            None,
            None,
            cx,
        )
        .into_any_element();
    }
    let dim = view.breakdown_dim;
    let mut list = v_col().w_full().gap(px(4.0)).child(table_header(
        &[
            (dim.label(view.lang), None),
            (
                crate::ui::Tr(view.lang).usage().usage_kpi_calls(),
                Some(80.0),
            ),
            (
                crate::ui::Tr(view.lang).usage().usage_kpi_failed(),
                Some(70.0),
            ),
            (
                crate::ui::Tr(view.lang).usage().usage_breakdown_input(),
                Some(100.0),
            ),
            (
                crate::ui::Tr(view.lang).usage().usage_breakdown_output(),
                Some(100.0),
            ),
            (
                crate::ui::Tr(view.lang).usage().usage_breakdown_cost(),
                Some(100.0),
            ),
        ],
        cx,
    ));

    let (page_groups, clamped_page, _) =
        page_slice(groups, view.breakdown_page, BREAKDOWN_PAGE_SIZE);
    for (idx, b) in page_groups.iter().enumerate() {
        let raw_key = b.key.first().cloned().unwrap_or_default();
        let display = match dim {
            BreakdownDim::Provider => view.provider_name(&raw_key),
            BreakdownDim::Source => source_label(&raw_key, view.lang).to_owned(),
            BreakdownDim::Client => view.client_name(&raw_key),
            BreakdownDim::Key => view.key_name(&raw_key),
            _ => raw_key.clone(),
        };
        list = list.child(
            div()
                .id(SharedString::from(format!("u-dim-g-{clamped_page}-{idx}")))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.drill_down(dim, raw_key.clone(), window, cx);
                }))
                .child(
                    data_row(cx)
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.accent_ink))
                                .truncate()
                                .child(display),
                        )
                        .child(
                            div()
                                .w(px(80.0))
                                .text_xs()
                                .text_color(to_hsla(p.ink))
                                .child(fmt_compact(b.usage.requests)),
                        )
                        .child(
                            div()
                                .w(px(70.0))
                                .text_xs()
                                .text_color(to_hsla(if b.usage.failed > 0 {
                                    p.warn
                                } else {
                                    p.muted
                                }))
                                .child(b.usage.failed.to_string()),
                        )
                        .child(
                            div()
                                .w(px(100.0))
                                .text_xs()
                                .text_color(to_hsla(p.ink_2))
                                .child(fmt_compact(b.usage.input_tokens)),
                        )
                        .child(
                            div()
                                .w(px(100.0))
                                .text_xs()
                                .text_color(to_hsla(p.ink_2))
                                .child(fmt_compact(b.usage.output_tokens)),
                        )
                        .child(
                            div()
                                .w(px(100.0))
                                .text_xs()
                                .text_color(to_hsla(p.ink))
                                .child(fmt_cost_map(&b.usage.cost_micro)),
                        ),
                ),
        );
    }
    list.children(breakdown_pager(
        "u-dim-g-pg",
        clamped_page,
        groups.len(),
        cx,
    ))
    .into_any_element()
}

fn render_failure_table(view: &UsageView, cx: &mut Context<UsageView>) -> AnyElement {
    let p = palette(cx);
    if view.failures.is_empty() {
        return empty_inline(
            crate::ui::Tr(view.lang)
                .usage()
                .usage_breakdown_no_failures_in_range(),
            None,
            None,
            cx,
        )
        .into_any_element();
    }
    let mut by_kind: BTreeMap<String, u64> = BTreeMap::new();
    for f in &view.failures {
        *by_kind.entry(f.kind.clone()).or_insert(0) += f.count;
    }
    let mut list = v_col().w_full().gap(px(4.0)).child(table_header(
        &[
            (
                crate::ui::Tr(view.lang)
                    .usage()
                    .usage_breakdown_failure_kind(),
                None,
            ),
            (
                crate::ui::Tr(view.lang).usage().usage_breakdown_count(),
                Some(90.0),
            ),
        ],
        cx,
    ));
    for (idx, (kind, count)) in by_kind.into_iter().enumerate() {
        let k_click = kind.clone();
        list = list.child(
            div()
                .id(SharedString::from(format!("u-dim-f-{idx}")))
                .cursor_pointer()
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.drill_down(BreakdownDim::Failure, k_click.clone(), window, cx);
                }))
                .child(
                    data_row(cx)
                        .child(badge(kind, Tone::Warning, cx))
                        .child(div().flex_1())
                        .child(
                            div()
                                .w(px(90.0))
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.ink))
                                .child(count.to_string()),
                        ),
                ),
        );
    }
    list.into_any_element()
}
