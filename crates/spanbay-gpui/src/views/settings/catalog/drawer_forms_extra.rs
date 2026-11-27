//! Add-custom-model form, Unlisted model catalog matcher & pricing form, and shared Sheet footer.

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::model::model_catalog::{CatalogKind, ModelCatalogRow, PricingMode};

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, btn_danger, btn_ghost, btn_primary, btn_secondary, card,
    checkbox, h_row, icon_xs, list_row, palette, search_input, section_heading, status_indicator,
    tab_bar_segmented, tab_item, to_hsla, v_col,
};

use super::drawer::CatalogDrawer;
use super::drawer_forms::{labeled_input, render_currency_popover, render_pricing_editor_fields};
use super::format::format_money;

pub fn render_offers_table(
    offers: &[ModelCatalogRow],
    active_family: Option<&str>,
    cx: &mut Context<CatalogDrawer>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let box_el = v_col().gap(px(6.0)).child(section_heading(
        crate::ui::tr(cx)
            .settings()
            .drawer_forms_extra_catalog_offers(offers.len()),
        cx,
    ));
    if offers.is_empty() {
        return box_el.child(
            div().text_xs().text_color(to_hsla(p.muted)).child(
                crate::ui::tr(cx)
                    .settings()
                    .drawer_forms_extra_no_catalog_distributors_list_this(),
            ),
        );
    }

    let mut rows = v_col();
    for (i, o) in offers.iter().enumerate() {
        let inp = o
            .input_per_mtok
            .map_or_else(|| "—".into(), |v| format_money(v, &o.currency));
        let out = o
            .output_per_mtok
            .map_or_else(|| "—".into(), |v| format_money(v, &o.currency));
        let cr = o
            .cache_read_per_mtok
            .map_or_else(|| "—".into(), |v| format_money(v, &o.currency));
        let is_selected = active_family == Some(o.family.as_str());
        let fam_pick = o.family.clone();

        let mut row = h_row()
            .justify_between()
            .gap(px(8.0))
            .px(px(12.0))
            .py(px(7.0))
            .child(
                h_row()
                    .flex_1()
                    .min_w_0()
                    .gap(px(6.0))
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(SharedString::from(o.family_name.clone())),
                    )
                    .children((i == 0).then(|| {
                        badge(
                            crate::ui::tr(cx).settings().catalog_offers_official(),
                            Tone::Accent,
                            cx,
                        )
                    })),
            )
            .child(
                div()
                    .text_xs()
                    .text_color(to_hsla(p.ink_2))
                    .child(SharedString::from(format!("{inp} / {out} · R:{cr}"))),
            )
            .child(
                btn_ghost(
                    SharedString::from(format!("cat-offer-use-{i}")),
                    if is_selected {
                        crate::ui::tr(cx).settings().drawer_forms_extra_active()
                    } else {
                        crate::ui::tr(cx).settings().drawer_forms_extra_use()
                    },
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    this.pricing_mode = PricingMode::Dynamic;
                    this.selected_catalog_family = Some(fam_pick.clone());
                    cx.notify();
                })),
            );
        if i + 1 < offers.len() {
            row = row
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)));
        }
        rows = rows.child(row);
    }

    box_el.child(card(cx).child(rows))
}

pub fn render_add_user_model_body(
    drawer: &CatalogDrawer,
    cx: &mut Context<CatalogDrawer>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ent_k = cx.entity().clone();
    let ent_c = cx.entity().clone();
    let kind_idx = usize::from(drawer.user_kind == CatalogKind::Embedding);

    v_col()
        .gap(px(12.0))
        .child(
            div().text_xs().text_color(to_hsla(p.muted)).child(
                crate::ui::tr(cx)
                    .settings()
                    .drawer_forms_extra_for_models_not_listed_on(),
            ),
        )
        .child(
            h_row()
                .gap(px(10.0))
                .child(labeled_input(
                    crate::ui::tr(cx).settings().drawer_forms_extra_family(),
                    &drawer.user_family,
                    cx,
                ))
                .child(labeled_input(
                    crate::ui::tr(cx).settings().drawer_forms_extra_model_id(),
                    &drawer.user_model_id,
                    cx,
                )),
        )
        .child(
            h_row()
                .gap(px(10.0))
                .items_end()
                .child(labeled_input(
                    crate::ui::tr(cx)
                        .settings()
                        .drawer_forms_extra_display_name(),
                    &drawer.user_name,
                    cx,
                ))
                .child(
                    tab_bar_segmented("cat-user-kind-seg")
                        .selected_index(kind_idx)
                        .child(tab_item(crate::ui::tr(cx).settings().catalog_kind_chat()))
                        .child(tab_item(crate::ui::tr(cx).common().cap_embedding()))
                        .on_click(move |idx, _, cx| {
                            let next = if *idx == 1 {
                                CatalogKind::Embedding
                            } else {
                                CatalogKind::Chat
                            };
                            ent_k.update(cx, |this, cx| {
                                this.user_kind = next;
                                cx.notify();
                            });
                        }),
                ),
        )
        .child(
            h_row()
                .gap(px(10.0))
                .child(labeled_input(
                    crate::ui::tr(cx).settings().catalog_user_context(),
                    &drawer.user_context,
                    cx,
                ))
                .child(labeled_input(
                    crate::ui::tr(cx).settings().catalog_col_max_output(),
                    &drawer.user_max_output,
                    cx,
                )),
        )
        .child(
            h_row()
                .gap(px(14.0))
                .child(
                    checkbox("cat-u-reason", drawer.user_reasoning)
                        .label(crate::ui::tr(cx).common().cap_reasoning())
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.user_reasoning = *checked;
                            cx.notify();
                        })),
                )
                .child(
                    checkbox("cat-u-vision", drawer.user_vision)
                        .label(crate::ui::tr(cx).common().cap_vision())
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.user_vision = *checked;
                            cx.notify();
                        })),
                )
                .child(
                    checkbox("cat-u-tools", drawer.user_tools)
                        .label(crate::ui::tr(cx).settings().drawer_forms_tools())
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.user_tools = *checked;
                            cx.notify();
                        })),
                ),
        )
        .child(
            h_row()
                .justify_between()
                .pt(px(4.0))
                .child(section_heading(
                    crate::ui::tr(cx)
                        .settings()
                        .drawer_forms_extra_catalog_prices_per_1m_tokens(),
                    cx,
                ))
                .child(render_currency_popover(
                    "cat-u-cur",
                    &drawer.currency,
                    move |code, _, cx| {
                        ent_c.update(cx, |this, cx| {
                            this.currency = code;
                            cx.notify();
                        });
                    },
                    cx,
                )),
        )
        .child(
            h_row()
                .gap(px(10.0))
                .child(labeled_input(
                    crate::ui::tr(cx).settings().catalog_col_input(),
                    &drawer.in_price,
                    cx,
                ))
                .child(labeled_input(
                    crate::ui::tr(cx).settings().catalog_col_output(),
                    &drawer.out_price,
                    cx,
                )),
        )
        .child(
            h_row()
                .gap(px(10.0))
                .child(labeled_input(
                    crate::ui::tr(cx).settings().catalog_offers_cache_read(),
                    &drawer.cache_read_price,
                    cx,
                ))
                .child(labeled_input(
                    crate::ui::tr(cx).settings().catalog_field_cache_write(),
                    &drawer.cache_write_price,
                    cx,
                )),
        )
}

pub fn render_unlisted_body(
    drawer: &CatalogDrawer,
    provider_name: &str,
    cx: &mut Context<CatalogDrawer>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let mut alias_list = v_col()
        .gap(px(2.0))
        .p(px(4.0))
        .rounded(px(8.0))
        .bg(to_hsla(p.pane))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)));

    if drawer.alias_candidates.is_empty() {
        alias_list = alias_list.child(
            div()
                .px(px(10.0))
                .py(px(12.0))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(
                    crate::ui::tr(cx)
                        .settings()
                        .drawer_forms_extra_no_matching_catalog_models(),
                ),
        );
    } else {
        for (i, cand) in drawer.alias_candidates.iter().take(12).enumerate() {
            let sel = drawer.selected_alias_key.as_deref() == Some(cand.model_key.as_str());
            let key = cand.model_key.clone();
            alias_list = alias_list.child(
                list_row(
                    SharedString::from(format!("cat-which-{i}")),
                    sel,
                    cx,
                    cx.listener(move |this, _, _, cx| {
                        this.selected_alias_key = Some(key.clone());
                        cx.notify();
                    }),
                )
                .justify_between()
                .child(
                    v_col()
                        .flex_1()
                        .min_w_0()
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .truncate()
                                .child(SharedString::from(cand.name.clone())),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(SharedString::from(cand.model_key.clone())),
                        ),
                )
                .children(sel.then(|| {
                    div()
                        .text_color(to_hsla(p.accent))
                        .child(icon_xs(UiIcon::Check))
                })),
            );
        }
    }

    let mut col = v_col()
        .gap(px(14.0))
        .child(
            v_col()
                .gap(px(6.0))
                .child(section_heading(
                    crate::ui::tr(cx)
                        .settings()
                        .drawer_forms_extra_which_catalog_model_is_this(),
                    cx,
                ))
                .child(
                    div().text_xs().text_color(to_hsla(p.muted)).child(
                        crate::ui::tr(cx)
                            .settings()
                            .drawer_forms_extra_this_id_will_follow_the(provider_name),
                    ),
                )
                .child(search_input(&drawer.alias_search, cx))
                .child(alias_list),
        )
        .child(
            v_col()
                .gap(px(8.0))
                .child(section_heading(
                    crate::ui::tr(cx)
                        .settings()
                        .drawer_forms_extra_or_set_a_direct_price(),
                    cx,
                ))
                .child(render_pricing_editor_fields(drawer, &[], cx)),
        );

    if drawer.pricing_mode == PricingMode::Dynamic && drawer.selected_alias_key.is_none() {
        col = col.child(
            h_row()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(8.0))
                .rounded(px(8.0))
                .bg(to_hsla(p.warn_soft))
                .child(status_indicator(StatusShape::Warn, cx))
                .child(
                    div().text_xs().text_color(to_hsla(p.warn)).child(
                        crate::ui::tr(cx)
                            .settings()
                            .drawer_forms_extra_no_catalog_match_or_custom(),
                    ),
                ),
        );
    }
    col
}

pub fn render_drawer_footer(
    show_reset: bool,
    show_delete_user: bool,
    busy: bool,
    cx: &mut Context<CatalogDrawer>,
) -> gpui_kit::Div {
    let p = palette(cx);
    h_row()
        .justify_between()
        .gap(px(8.0))
        .pt(px(12.0))
        .border_t_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            h_row()
                .gap(px(6.0))
                .children(show_reset.then(|| {
                    btn_ghost(
                        "cat-dr-reset",
                        crate::ui::tr(cx).settings().catalog_reset_pricing(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.reset_pricing(window, cx)))
                }))
                .children(show_delete_user.then(|| {
                    btn_danger(
                        "cat-dr-del-user",
                        crate::ui::tr(cx).settings().catalog_delete_button(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.delete_user_model(window, cx)))
                })),
        )
        .child(
            h_row()
                .gap(px(6.0))
                .child(
                    btn_secondary("cat-dr-cancel", crate::ui::tr(cx).common().cancel())
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                )
                .child(
                    btn_primary(
                        "cat-dr-save",
                        if busy {
                            crate::ui::tr(cx).settings().drawer_forms_extra_saving()
                        } else {
                            crate::ui::tr(cx)
                                .settings()
                                .drawer_forms_extra_save_changes()
                        },
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                ),
        )
}

pub fn render_user_model_meta_editor(
    drawer: &CatalogDrawer,
    cx: &mut Context<CatalogDrawer>,
) -> gpui_kit::Div {
    v_col()
        .gap(px(8.0))
        .child(section_heading(
            crate::ui::tr(cx)
                .settings()
                .drawer_forms_extra_custom_model_parameters(),
            cx,
        ))
        .child(
            h_row()
                .gap(px(8.0))
                .child(labeled_input(
                    crate::ui::tr(cx).settings().drawer_forms_extra_name(),
                    &drawer.user_name,
                    cx,
                ))
                .child(labeled_input(
                    crate::ui::tr(cx).settings().sec_agent_context(),
                    &drawer.user_context,
                    cx,
                ))
                .child(labeled_input(
                    crate::ui::tr(cx).settings().drawer_forms_extra_max_out(),
                    &drawer.user_max_output,
                    cx,
                )),
        )
        .child(
            h_row()
                .gap(px(14.0))
                .child(
                    checkbox("cat-ed-reason", drawer.user_reasoning)
                        .label(crate::ui::tr(cx).common().cap_reasoning())
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.user_reasoning = *checked;
                            cx.notify();
                        })),
                )
                .child(
                    checkbox("cat-ed-vision", drawer.user_vision)
                        .label(crate::ui::tr(cx).common().cap_vision())
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.user_vision = *checked;
                            cx.notify();
                        })),
                )
                .child(
                    checkbox("cat-ed-tools", drawer.user_tools)
                        .label(crate::ui::tr(cx).common().cap_tools())
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.user_tools = *checked;
                            cx.notify();
                        })),
                ),
        )
}
