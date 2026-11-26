//! Tab 2 of Models & Pricing: Unlisted provider models governance with search,
//! provider filter, pricing-status filter, and full bounded pagination.

use std::collections::BTreeMap;

use gpui_kit::{
    Context, Div, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::app::UnlistedModel;

use crate::ui::{
    Anchor, DEFAULT_PAGE_SIZES, PaginationBar, ScrollableElement as _, StatusShape, Tone, UiIcon,
    badge, btn_ghost, btn_secondary, filter_chip, h_row, icon_xs, list_row, page_slice,
    pagination_bar, palette, popover, search_input, select_pill, status_indicator, to_hsla, v_col,
};

use super::format::{format_basis, format_effective_price};
use super::tab_catalog::{th_bar, th_col};
use super::{CatalogSettingsView, UnlistedPriceFilter};

pub fn render_unlisted_tab(
    view: &CatalogSettingsView,
    cx: &mut Context<CatalogSettingsView>,
) -> Div {
    let p = palette(cx);
    let lang = crate::ui::active_language(cx);
    let s = crate::ui::tr(cx).settings();

    let total_all = view.unlisted.len();
    let total_unpriced = view
        .unlisted
        .iter()
        .filter(|u| u.effective.is_none())
        .count();
    let total_priced = total_all.saturating_sub(total_unpriced);

    // Distinct providers present in the unlisted set, sorted by display name.
    let mut provider_counts: BTreeMap<(String, String), usize> = BTreeMap::new();
    for item in &view.unlisted {
        *provider_counts
            .entry((item.provider_name.clone(), item.provider_id.clone()))
            .or_insert(0) += 1;
    }
    let provider_opts: Vec<(String, String, usize)> = provider_counts
        .into_iter()
        .map(|((name, id), cnt)| (id, name, cnt))
        .collect();

    let q = view
        .unlisted_search_input
        .read(cx)
        .value()
        .trim()
        .to_lowercase();
    let filtered: Vec<&UnlistedModel> = view
        .unlisted
        .iter()
        .filter(|item| {
            if let Some(pid) = &view.unlisted_provider
                && &item.provider_id != pid
            {
                return false;
            }
            match view.unlisted_price_filter {
                UnlistedPriceFilter::All => {}
                UnlistedPriceFilter::Unpriced if item.effective.is_some() => return false,
                UnlistedPriceFilter::Priced if item.effective.is_none() => return false,
                _ => {}
            }
            if !q.is_empty()
                && !item.model_id.to_lowercase().contains(&q)
                && !item.provider_name.to_lowercase().contains(&q)
            {
                return false;
            }
            true
        })
        .collect();

    let (page_items, clamped_page, _) =
        page_slice(&filtered, view.unlisted_page, view.unlisted_page_size);
    let has_filters = view.unlisted_has_active_filters(cx);

    let active_prov_label = view
        .unlisted_provider
        .as_deref()
        .and_then(|pid| {
            provider_opts
                .iter()
                .find(|(id, _, _)| id == pid)
                .map(|(_, name, _)| name.clone())
        })
        .unwrap_or_else(|| s.catalog_unlisted_all_providers().into());
    let ent = cx.entity().clone();
    let active_pid = view.unlisted_provider.clone();
    let provider_filter = popover("cat-unlisted-prov-pop")
        .anchor(Anchor::TopLeft)
        .trigger(select_pill(
            "cat-unlisted-prov-btn",
            SharedString::from(active_prov_label),
            active_pid.is_some(),
            cx,
        ))
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut list = v_col()
                .w(px(220.0))
                .max_h(px(280.0))
                .gap(px(2.0))
                .overflow_y_scrollbar();
            let sel_all = active_pid.is_none();
            let e0 = ent.clone();
            let ph0 = pop_cx.entity().clone();
            list = list.child(
                list_row("cat-unlisted-prov-all", sel_all, pop_cx, move |_, w, cx| {
                    e0.update(cx, |this, cx| {
                        this.unlisted_provider = None;
                        this.unlisted_page = 0;
                        cx.notify();
                    });
                    ph0.update(cx, |st, cx| st.dismiss(w, cx));
                })
                .justify_between()
                .child(
                    div().text_xs().child(
                        crate::ui::tr(pop_cx)
                            .settings()
                            .catalog_unlisted_all_providers(),
                    ),
                )
                .children(sel_all.then(|| {
                    div()
                        .text_color(to_hsla(pop_p.accent))
                        .child(icon_xs(UiIcon::Check))
                })),
            );
            for (i, (pid, pname, cnt)) in provider_opts.iter().enumerate() {
                let sel = active_pid.as_deref() == Some(pid.as_str());
                let e = ent.clone();
                let pid_v = pid.clone();
                let ph = pop_cx.entity().clone();
                list = list.child(
                    list_row(
                        SharedString::from(format!("cat-unlisted-prov-{i}")),
                        sel,
                        pop_cx,
                        move |_, w, cx| {
                            e.update(cx, |this, cx| {
                                this.unlisted_provider = Some(pid_v.clone());
                                this.unlisted_page = 0;
                                cx.notify();
                            });
                            ph.update(cx, |st, cx| st.dismiss(w, cx));
                        },
                    )
                    .justify_between()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .truncate()
                            .child(SharedString::from(format!("{pname} ({cnt})"))),
                    )
                    .children(sel.then(|| {
                        div()
                            .text_color(to_hsla(pop_p.accent))
                            .child(icon_xs(UiIcon::Check))
                    })),
                );
            }
            list
        });

    let status_chips = h_row()
        .gap(px(6.0))
        .flex_wrap()
        .child(
            filter_chip(
                "cat-unlisted-flt-all",
                SharedString::from(s.catalog_unlisted_filter_all(total_all)),
                None,
                view.unlisted_price_filter == UnlistedPriceFilter::All,
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.unlisted_price_filter = UnlistedPriceFilter::All;
                this.unlisted_page = 0;
                cx.notify();
            })),
        )
        .child(
            filter_chip(
                "cat-unlisted-flt-unpriced",
                SharedString::from(s.catalog_unlisted_filter_unpriced(total_unpriced)),
                None,
                view.unlisted_price_filter == UnlistedPriceFilter::Unpriced,
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.unlisted_price_filter = UnlistedPriceFilter::Unpriced;
                this.unlisted_page = 0;
                cx.notify();
            })),
        )
        .child(
            filter_chip(
                "cat-unlisted-flt-priced",
                SharedString::from(s.catalog_unlisted_filter_priced(total_priced)),
                None,
                view.unlisted_price_filter == UnlistedPriceFilter::Priced,
                cx,
            )
            .on_click(cx.listener(|this, _, _, cx| {
                this.unlisted_price_filter = UnlistedPriceFilter::Priced;
                this.unlisted_page = 0;
                cx.notify();
            })),
        );

    let toolbar = h_row()
        .justify_between()
        .gap(px(10.0))
        .flex_wrap()
        .p(px(14.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            h_row()
                .gap(px(8.0))
                .flex_wrap()
                .child(
                    div()
                        .w(px(280.0))
                        .child(search_input(&view.unlisted_search_input, cx)),
                )
                .child(provider_filter),
        )
        .child(
            h_row()
                .gap(px(6.0))
                .flex_wrap()
                .child(status_chips)
                .children(has_filters.then(|| {
                    btn_ghost("cat-unlisted-clear-flt", s.catalog_clear_filters()).on_click(
                        cx.listener(|this, _, window, cx| this.clear_unlisted_filters(window, cx)),
                    )
                })),
        );

    let thead = th_bar(cx)
        .child(div().flex_1().min_w(px(200.0)).child(s.catalog_user_id()))
        .child(th_col(170.0, false, s.catalog_th_provider()))
        .child(th_col(175.0, false, s.catalog_th_price()))
        .child(th_col(115.0, true, s.catalog_th_action()));

    let mut tbody = v_col();
    if view.unlisted.is_empty() {
        tbody = tbody.child(
            v_col()
                .items_center()
                .py(px(34.0))
                .gap(px(8.0))
                .child(status_indicator(StatusShape::Ok, cx))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .child(s.catalog_unlisted_empty_all_title()),
                )
                .child(
                    div()
                        .max_w(px(480.0))
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(s.catalog_unlisted_empty_all_desc()),
                ),
        );
    } else if filtered.is_empty() {
        tbody = tbody.child(
            v_col()
                .items_center()
                .py(px(30.0))
                .gap(px(8.0))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink))
                        .child(s.catalog_unlisted_empty_filtered()),
                )
                .child(
                    btn_ghost("cat-unlisted-empty-clear", s.catalog_clear_filters()).on_click(
                        cx.listener(|this, _, window, cx| this.clear_unlisted_filters(window, cx)),
                    ),
                ),
        );
    } else {
        for (idx, item) in page_items.iter().enumerate() {
            let item_click = (*item).clone();
            let item_btn = (*item).clone();
            let price_cell = match &item.effective {
                Some(eff) => {
                    let price_str = format_effective_price(eff, lang);
                    let basis_str = format_basis(&eff.basis, lang);
                    v_col()
                        .w(px(175.0))
                        .gap(px(1.0))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(SharedString::from(price_str)),
                        )
                        .child(
                            div()
                                .text_size(px(10.5))
                                .text_color(to_hsla(p.faint))
                                .truncate()
                                .child(SharedString::from(basis_str)),
                        )
                }
                None => div().w(px(175.0)).flex().items_center().child(badge(
                    s.catalog_unlisted_status_unpriced(),
                    Tone::Warning,
                    cx,
                )),
            };

            let mut row = div()
                .id(SharedString::from(format!(
                    "cat-unlisted-{clamped_page}-{idx}"
                )))
                .flex()
                .flex_row()
                .items_center()
                .gap(px(10.0))
                .px(px(14.0))
                .py(px(9.0))
                .cursor_pointer()
                .hover(|st| st.bg(to_hsla(p.hover.composite_over(p.surface))))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.open_unlisted_drawer(item_click.clone(), window, cx);
                }))
                .child(
                    div()
                        .flex_1()
                        .min_w(px(200.0))
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.accent_ink))
                        .truncate()
                        .child(SharedString::from(item.model_id.clone())),
                )
                .child(
                    div()
                        .w(px(170.0))
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .truncate()
                        .child(SharedString::from(item.provider_name.clone())),
                )
                .child(price_cell)
                .child(
                    div()
                        .id(SharedString::from(format!(
                            "cat-unlisted-act-{clamped_page}-{idx}"
                        )))
                        .w(px(115.0))
                        .flex()
                        .justify_end()
                        .on_click(|_, _, cx| cx.stop_propagation())
                        .child(
                            btn_secondary(
                                SharedString::from(format!(
                                    "cat-unlisted-btn-{clamped_page}-{idx}"
                                )),
                                s.models_table_price_match(),
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open_unlisted_drawer(item_btn.clone(), window, cx);
                                },
                            )),
                        ),
                );
            if idx + 1 < page_items.len() {
                row = row
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.surface)));
            }
            tbody = tbody.child(row);
        }
    }

    let mut body = v_col().w_full().child(toolbar).child(thead).child(tbody);
    if !filtered.is_empty() {
        body = body.child(
            div()
                .px(px(14.0))
                .py(px(9.0))
                .bg(to_hsla(p.pane))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(pagination_bar(
                    PaginationBar {
                        id_prefix: "cat-unlisted-pg",
                        page: clamped_page,
                        page_size: view.unlisted_page_size,
                        total: filtered.len(),
                        page_sizes: DEFAULT_PAGE_SIZES,
                        loading: false,
                        compact: false,
                    },
                    |this: &mut CatalogSettingsView, next_page, cx| {
                        this.unlisted_page = next_page;
                        cx.notify();
                    },
                    Some(|this: &mut CatalogSettingsView, next_size, cx| {
                        this.unlisted_page_size = next_size;
                        this.unlisted_page = 0;
                        cx.notify();
                    }),
                    cx,
                )),
        );
    }

    body
}
