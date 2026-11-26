//! Main Models & Pricing table, pagination footer, and Unlisted Models section.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::model::model_catalog::{CatalogSource, ModelCatalogRow};

use crate::ui::{
    DEFAULT_PAGE_SIZES, PaginationBar, StatusShape, Tone, UiIcon, badge, btn_ghost, btn_primary,
    btn_secondary, filter_chip, h_row, page_slice, pagination_bar, palette, search_input,
    settings_section_flush, status_indicator, to_hsla, v_col,
};

use super::CatalogSettingsView;
use super::filters::{
    render_kind_filter, render_mode_filter, render_publisher_filter, render_source_filter,
};
use super::format::{format_effective_price, format_row_prices, format_tokens};

fn th_bar(cx: &gpui_kit::App) -> gpui_kit::Div {
    let p = palette(cx);
    h_row()
        .px(px(14.0))
        .py(px(8.0))
        .gap(px(10.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .text_xs()
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(to_hsla(p.muted))
}

fn th_col(width: f32, right: bool, label: &'static str) -> gpui_kit::Div {
    if right {
        div().w(px(width)).flex().justify_end().child(label)
    } else {
        div().w(px(width)).child(label)
    }
}

pub fn render_models_section(
    view: &CatalogSettingsView,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let shown = view.page.as_ref().map_or(0, |pg| pg.rows.len());
    let total = view.page.as_ref().map_or(0, |pg| pg.total as usize);
    let has_filters = view.has_active_filters(cx);

    let toolbar_top = h_row()
        .justify_between()
        .gap(px(10.0))
        .flex_wrap()
        .child(
            div()
                .w(px(320.0))
                .child(search_input(&view.search_input, cx)),
        )
        .child(
            h_row()
                .gap(px(6.0))
                .flex_wrap()
                .child(
                    filter_chip(
                        "cat-filter-inuse",
                        crate::ui::tr(cx).settings().models_table_in_use_only(),
                        Some(UiIcon::Zap),
                        view.in_use,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.in_use = !this.in_use;
                        this.load_page(false, cx);
                    })),
                )
                .child(
                    filter_chip(
                        "cat-filter-dist",
                        crate::ui::tr(cx)
                            .settings()
                            .models_table_distributor_entries(),
                        Some(UiIcon::Globe),
                        view.distributor_entries,
                        cx,
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.distributor_entries = !this.distributor_entries;
                        this.load_page(false, cx);
                    })),
                )
                .child(
                    btn_primary(
                        "cat-add-user-model",
                        crate::ui::tr(cx).settings().models_table_add_custom_model(),
                    )
                    .h(px(30.0))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_add_user_model_drawer(window, cx);
                    })),
                ),
        );

    let count_text = crate::ui::tr(cx)
        .settings()
        .models_table_showing_of(shown, total);

    let toolbar_filters = h_row()
        .justify_between()
        .gap(px(8.0))
        .flex_wrap()
        .child(
            h_row()
                .gap(px(6.0))
                .flex_wrap()
                .child(render_publisher_filter(view, cx))
                .child(render_kind_filter(view, cx))
                .child(render_source_filter(view, cx))
                .child(render_mode_filter(view, cx))
                .children(has_filters.then(|| {
                    btn_ghost(
                        "cat-clear-filters",
                        crate::ui::tr(cx).settings().catalog_clear_filters(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.clear_filters(window, cx)))
                })),
        )
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.faint))
                .child(SharedString::from(count_text)),
        );

    let s = crate::ui::tr(cx).settings();
    let thead = th_bar(cx)
        .child(div().flex_1().min_w(px(210.0)).child(s.catalog_col_model()))
        .child(th_col(108.0, false, s.catalog_th_ctx_out()))
        .child(th_col(82.0, true, s.catalog_th_input_1m()))
        .child(th_col(82.0, true, s.catalog_th_output_1m()))
        .child(th_col(110.0, true, s.catalog_th_cache_rw()))
        .child(th_col(88.0, true, s.catalog_th_pricing()));

    let mut tbody = v_col();
    if view.loading_page && shown == 0 {
        tbody = tbody.child(
            h_row()
                .justify_center()
                .py(px(28.0))
                .gap(px(8.0))
                .child(status_indicator(StatusShape::Run, cx))
                .child(
                    div().text_xs().text_color(to_hsla(p.muted)).child(
                        crate::ui::tr(cx)
                            .settings()
                            .models_table_loading_model_catalog(),
                    ),
                ),
        );
    } else if shown == 0 {
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
                        .child(if has_filters {
                            crate::ui::tr(cx)
                                .settings()
                                .models_table_no_models_match_these_filters()
                        } else {
                            crate::ui::tr(cx).settings().catalog_empty_title()
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(if has_filters {
                            crate::ui::tr(cx)
                                .settings()
                                .models_table_try_clearing_filters_or_broadening()
                        } else {
                            crate::ui::tr(cx)
                                .settings()
                                .models_table_click_sync_now_to_download()
                        }),
                ),
        );
    } else if let Some(pg) = &view.page {
        for (idx, row) in pg.rows.iter().enumerate() {
            tbody = tbody.child(render_catalog_row(idx, row, idx + 1 < pg.rows.len(), cx));
        }
    }

    let mut table_body = v_col()
        .w_full()
        .child(
            v_col()
                .w_full()
                .p(px(14.0))
                .gap(px(10.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(toolbar_top)
                .child(toolbar_filters),
        )
        .child(thead)
        .child(tbody);

    if total > 0 {
        table_body = table_body.child(
            div()
                .px(px(14.0))
                .py(px(9.0))
                .bg(to_hsla(p.pane))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(pagination_bar(
                    PaginationBar {
                        id_prefix: "cat-pg",
                        page: view.page_index,
                        page_size: view.page_size as usize,
                        total,
                        page_sizes: DEFAULT_PAGE_SIZES,
                        loading: view.loading_page,
                        compact: false,
                    },
                    |this: &mut CatalogSettingsView, next_page, cx| {
                        this.set_page(next_page, cx);
                    },
                    Some(|this: &mut CatalogSettingsView, next_size, cx| {
                        this.set_page_size(next_size, cx);
                    }),
                    cx,
                )),
        );
    }

    v_col()
        .w_full()
        .gap(px(8.0))
        .child(settings_section_flush(
            crate::ui::tr(cx).settings().models_table_models(),
            None,
            table_body,
            cx,
        ))
        .child(
            div()
                .px(px(4.0))
                .text_xs()
                .text_color(to_hsla(p.faint))
                .child(
                    crate::ui::tr(cx)
                        .settings()
                        .models_table_prices_are_per_1m_tokens(),
                ),
        )
}

fn render_catalog_row(
    idx: usize,
    row: &ModelCatalogRow,
    has_border: bool,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let lang = crate::ui::active_language(cx);
    let (inp, out, cache, mode_label, mode_tone) = format_row_prices(row, lang);
    let ctx_str = format!(
        "{} / {}",
        format_tokens(row.context_tokens),
        format_tokens(row.max_output_tokens)
    );
    let pub_label = if !row.publisher_name.is_empty() {
        row.publisher_name.clone()
    } else {
        row.family_name.clone()
    };
    let row_clone = row.clone();

    let mut caps = h_row().gap(px(4.0)).flex_wrap();
    if row.source == CatalogSource::User {
        caps = caps.child(badge(
            crate::ui::tr(cx).settings().catalog_source_user(),
            Tone::Accent,
            cx,
        ));
    }
    if row.in_use.unwrap_or(0) > 0 {
        caps = caps.child(badge(
            crate::ui::tr(cx).settings().models_table_in_use(),
            Tone::Success,
            cx,
        ));
    }
    if row.provider_overrides > 0 {
        caps = caps.child(badge(
            crate::ui::tr(cx)
                .settings()
                .models_table_ovr(row.provider_overrides),
            Tone::Warning,
            cx,
        ));
    }
    if row.reasoning {
        caps = caps.child(cap_chip(
            crate::ui::tr(cx).settings().models_table_reason(),
            cx,
        ));
    }
    if row.vision {
        caps = caps.child(cap_chip(crate::ui::tr(cx).common().cap_vision(), cx));
    }
    if row.tools {
        caps = caps.child(cap_chip(crate::ui::tr(cx).common().cap_tools(), cx));
    }

    let mut el = div()
        .id(SharedString::from(format!("cat-row-{idx}")))
        .flex()
        .flex_row()
        .items_center()
        .gap(px(10.0))
        .px(px(14.0))
        .py(px(9.0))
        .cursor_pointer()
        .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.surface))))
        .on_click(cx.listener(move |this, _, window, cx| {
            this.open_model_drawer(row_clone.clone(), window, cx);
        }))
        .child(
            v_col()
                .flex_1()
                .min_w(px(210.0))
                .gap(px(3.0))
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.accent_ink))
                                .truncate()
                                .child(SharedString::from(row.name.clone())),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.faint))
                                .truncate()
                                .child(SharedString::from(pub_label)),
                        ),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .flex_wrap()
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(SharedString::from(row.model_id.clone())),
                        )
                        .child(caps),
                ),
        )
        .child(
            div()
                .w(px(108.0))
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .child(SharedString::from(ctx_str)),
        )
        .child(
            div()
                .w(px(82.0))
                .flex()
                .justify_end()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(to_hsla(p.ink))
                .child(SharedString::from(inp)),
        )
        .child(
            div()
                .w(px(82.0))
                .flex()
                .justify_end()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(to_hsla(p.ink))
                .child(SharedString::from(out)),
        )
        .child(
            div()
                .w(px(110.0))
                .flex()
                .justify_end()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(cache)),
        )
        .child(
            div()
                .w(px(88.0))
                .flex()
                .justify_end()
                .child(badge(mode_label, mode_tone, cx)),
        );

    if has_border {
        el = el
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)));
    }
    el
}

fn cap_chip(label: &'static str, cx: &gpui_kit::App) -> gpui_kit::Div {
    let p = palette(cx);
    div()
        .px(px(5.0))
        .py(px(1.0))
        .rounded(px(4.0))
        .bg(to_hsla(p.sunken))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.sunken)))
        .text_size(px(10.5))
        .text_color(to_hsla(p.muted))
        .child(label)
}

pub fn render_unlisted_section(
    view: &CatalogSettingsView,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let lang = crate::ui::active_language(cx);
    let s = crate::ui::tr(cx).settings();
    const UNLISTED_PAGE_SIZE: usize = 20;
    let count = view.unlisted.len();
    let (page_items, clamped_page, _) =
        page_slice(&view.unlisted, view.unlisted_page, UNLISTED_PAGE_SIZE);

    let thead = th_bar(cx)
        .child(th_col(180.0, false, s.catalog_th_provider()))
        .child(div().flex_1().child(s.catalog_user_id()))
        .child(th_col(150.0, false, s.catalog_th_price()))
        .child(th_col(120.0, true, s.catalog_th_action()));

    let mut tbody = v_col();
    for (idx, item) in page_items.iter().enumerate() {
        let price_str = item
            .effective
            .as_ref()
            .map(|e| format_effective_price(e, lang))
            .unwrap_or_else(|| s.catalog_mode_none().into());
        let has_price = item.effective.is_some();
        let item_click = item.clone();
        let item_btn = item.clone();

        let mut row = div()
            .id(SharedString::from(format!("cat-unlisted-{idx}")))
            .flex()
            .flex_row()
            .items_center()
            .gap(px(10.0))
            .px(px(14.0))
            .py(px(9.0))
            .cursor_pointer()
            .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.surface))))
            .on_click(cx.listener(move |this, _, window, cx| {
                this.open_unlisted_drawer(item_click.clone(), window, cx);
            }))
            .child(
                div()
                    .w(px(180.0))
                    .text_xs()
                    .text_color(to_hsla(p.ink))
                    .truncate()
                    .child(SharedString::from(item.provider_name.clone())),
            )
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .text_color(to_hsla(p.accent_ink))
                    .truncate()
                    .child(SharedString::from(item.model_id.clone())),
            )
            .child(
                div()
                    .w(px(150.0))
                    .text_xs()
                    .text_color(if has_price {
                        to_hsla(p.ink)
                    } else {
                        to_hsla(p.warn)
                    })
                    .child(SharedString::from(price_str)),
            )
            .child(
                div()
                    .id(SharedString::from(format!("cat-unlisted-act-{idx}")))
                    .w(px(120.0))
                    .flex()
                    .justify_end()
                    .on_click(|_, _, cx| cx.stop_propagation())
                    .child(
                        btn_secondary(
                            SharedString::from(format!("cat-unlisted-btn-{idx}")),
                            crate::ui::tr(cx).settings().models_table_price_match(),
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

    let mut section_body = v_col().w_full().child(thead).child(tbody);
    if count > UNLISTED_PAGE_SIZE {
        section_body = section_body.child(
            div()
                .px(px(14.0))
                .py(px(8.0))
                .bg(to_hsla(p.pane))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(pagination_bar(
                    PaginationBar {
                        id_prefix: "cat-unlisted-pg",
                        page: clamped_page,
                        page_size: UNLISTED_PAGE_SIZE,
                        total: count,
                        page_sizes: &[],
                        loading: false,
                        compact: false,
                    },
                    |this: &mut CatalogSettingsView, next_page, cx| {
                        this.unlisted_page = next_page;
                        cx.notify();
                    },
                    None::<fn(&mut CatalogSettingsView, usize, &mut Context<CatalogSettingsView>)>,
                    cx,
                )),
        );
    }

    v_col()
        .w_full()
        .gap(px(8.0))
        .child(settings_section_flush(
            crate::ui::tr(cx)
                .settings()
                .models_table_unlisted_models(count),
            None,
            section_body,
            cx,
        ))
        .child(
            div()
                .px(px(4.0))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(
                    crate::ui::tr(cx)
                        .settings()
                        .models_table_models_your_providers_serve_that(),
                ),
        )
}
