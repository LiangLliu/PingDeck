//! Model facts, default pricing editor, provider overrides, and catalog offers inside the
//! "Models & Pricing" slide-over Sheet drawer.

use gpui_kit::{
    App, Context, Entity, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};
use spanbay_desktop::core::app::{ModelPriceDetail, ProviderModelPrice};
use spanbay_desktop::core::model::model_catalog::{
    CatalogSource, ModelCatalogRow, PricingMode, SUPPORTED_CURRENCIES,
};

use crate::ui::{
    Anchor, Tone, UiIcon, badge, btn_ghost, card, h_row, icon_xs, list_row, palette, popover,
    section_heading, select_pill, tab_bar_segmented, tab_item, text_input, to_hsla, v_col,
};

use super::drawer::CatalogDrawer;
use super::format::{format_basis, format_effective_price, format_tokens};

pub fn labeled_input(
    label: &'static str,
    input: &Entity<crate::ui::InputState>,
    cx: &App,
) -> gpui_kit::Div {
    let p = palette(cx);
    v_col()
        .flex_1()
        .min_w(px(120.0))
        .gap(px(4.0))
        .child(div().text_xs().text_color(to_hsla(p.muted)).child(label))
        .child(text_input(input))
}

pub fn render_currency_popover(
    id: &'static str,
    current: &str,
    on_pick: impl Fn(String, &mut gpui_kit::Window, &mut App) + Clone + 'static,
    cx: &App,
) -> impl IntoElement {
    let cur = current.to_owned();
    popover(SharedString::from(format!("{id}-pop")))
        .anchor(Anchor::TopLeft)
        .trigger(select_pill(
            SharedString::from(format!("{id}-btn")),
            SharedString::from(cur.clone()),
            false,
            cx,
        ))
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut list = v_col().w(px(120.0)).gap(px(2.0));
            for code in SUPPORTED_CURRENCIES {
                let sel = cur == code;
                let cb = on_pick.clone();
                let ph = pop_cx.entity().clone();
                list = list.child(
                    list_row(
                        SharedString::from(format!("{id}-{code}")),
                        sel,
                        pop_cx,
                        move |_, window, cx| {
                            cb(code.to_owned(), window, cx);
                            ph.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(div().text_xs().child(code))
                    .children(sel.then(|| {
                        div()
                            .text_color(to_hsla(pop_p.accent))
                            .child(icon_xs(UiIcon::Check))
                    })),
                );
            }
            list
        })
}

pub fn render_model_facts(row: &ModelCatalogRow, offer_count: usize, cx: &App) -> gpui_kit::Div {
    let p = palette(cx);
    let mut caps = h_row().gap(px(5.0)).flex_wrap();
    caps = caps.child(badge(
        if row.source == CatalogSource::User {
            crate::ui::tr(cx).settings().drawer_forms_custom_model()
        } else {
            "models.dev"
        },
        if row.source == CatalogSource::User {
            Tone::Accent
        } else {
            Tone::Neutral
        },
        cx,
    ));
    if row.reasoning {
        caps = caps.child(badge(
            crate::ui::tr(cx).common().cap_reasoning(),
            Tone::Accent,
            cx,
        ));
    }
    if row.vision {
        caps = caps.child(badge(
            crate::ui::tr(cx).common().cap_vision(),
            Tone::Neutral,
            cx,
        ));
    }
    if row.tools {
        caps = caps.child(badge(
            crate::ui::tr(cx).settings().drawer_forms_tools(),
            Tone::Neutral,
            cx,
        ));
    }

    card(cx).p(px(12.0)).child(
        v_col()
            .gap(px(8.0))
            .child(
                h_row()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        v_col()
                            .flex_1()
                            .min_w_0()
                            .gap(px(2.0))
                            .child(
                                div()
                                    .text_sm()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(to_hsla(p.ink))
                                    .truncate()
                                    .child(SharedString::from(row.name.clone())),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(to_hsla(p.muted))
                                    .truncate()
                                    .child(SharedString::from(row.model_key.clone())),
                            ),
                    )
                    .child(badge(
                        crate::ui::tr(cx)
                            .settings()
                            .drawer_forms_offer_s(offer_count),
                        Tone::Neutral,
                        cx,
                    )),
            )
            .child(
                h_row()
                    .gap(px(16.0))
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(SharedString::from(format!(
                        "{}: {}",
                        crate::ui::tr(cx).settings().sec_agent_context(),
                        format_tokens(row.context_tokens)
                    )))
                    .child(SharedString::from(format!(
                        "{}: {}",
                        crate::ui::tr(cx).settings().catalog_col_max_output(),
                        format_tokens(row.max_output_tokens)
                    ))),
            )
            .child(caps),
    )
}

pub fn render_pricing_editor_fields(
    drawer: &CatalogDrawer,
    offers: &[ModelCatalogRow],
    cx: &mut Context<CatalogDrawer>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ent = cx.entity().clone();
    let mode_idx = match drawer.pricing_mode {
        PricingMode::Dynamic => 0,
        PricingMode::Custom => 1,
        PricingMode::Fixed => 2,
    };

    let seg = tab_bar_segmented("cat-drawer-mode-seg")
        .selected_index(mode_idx)
        .child(tab_item(
            crate::ui::tr(cx).settings().drawer_forms_catalog(),
        ))
        .child(tab_item(crate::ui::tr(cx).settings().network_mode_custom()))
        .child(tab_item(crate::ui::tr(cx).settings().catalog_mode_fixed()))
        .on_click(move |idx, _, cx| {
            let next = match *idx {
                1 => PricingMode::Custom,
                2 => PricingMode::Fixed,
                _ => PricingMode::Dynamic,
            };
            ent.update(cx, |this, cx| {
                this.pricing_mode = next;
                cx.notify();
            });
        });

    let mut box_el = v_col().gap(px(10.0)).child(h_row().child(seg));

    match drawer.pricing_mode {
        PricingMode::Dynamic => {
            if offers.len() > 1 {
                box_el = box_el.child(render_offer_family_row(drawer, offers, cx));
            } else {
                box_el = box_el.child(
                    div().text_xs().text_color(to_hsla(p.muted)).child(
                        crate::ui::tr(cx)
                            .settings()
                            .drawer_forms_follows_synced_models_dev_catalog(),
                    ),
                );
            }
        }
        PricingMode::Custom => {
            let ent_c = cx.entity().clone();
            box_el = box_el
                .child(
                    h_row()
                        .justify_between()
                        .child(
                            div().text_xs().text_color(to_hsla(p.muted)).child(
                                crate::ui::tr(cx)
                                    .settings()
                                    .drawer_forms_currency_per_1m_tokens(),
                            ),
                        )
                        .child(render_currency_popover(
                            "cat-dr-cur",
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
                            crate::ui::tr(cx).settings().drawer_forms_input(),
                            &drawer.in_price,
                            cx,
                        ))
                        .child(labeled_input(
                            crate::ui::tr(cx).settings().drawer_forms_output(),
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
                        ))
                        .child(labeled_input(
                            crate::ui::tr(cx).settings().drawer_forms_reasoning(),
                            &drawer.reasoning_price,
                            cx,
                        )),
                );
        }
        PricingMode::Fixed => {
            let ent_c = cx.entity().clone();
            box_el = box_el
                .child(
                    h_row()
                        .gap(px(10.0))
                        .items_end()
                        .child(labeled_input(
                            crate::ui::tr(cx)
                                .settings()
                                .drawer_forms_price_per_successful_call(),
                            &drawer.per_call_price,
                            cx,
                        ))
                        .child(render_currency_popover(
                            "cat-dr-fix-cur",
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
                    div().text_xs().text_color(to_hsla(p.muted)).child(
                        crate::ui::tr(cx)
                            .settings()
                            .drawer_forms_flat_fee_per_successful_request(),
                    ),
                );
        }
    }

    box_el
}

fn render_offer_family_row(
    drawer: &CatalogDrawer,
    offers: &[ModelCatalogRow],
    cx: &mut Context<CatalogDrawer>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let active_fam = drawer.selected_catalog_family.clone();
    let label = active_fam
        .as_deref()
        .and_then(|f| offers.iter().find(|o| o.family == f))
        .map(|o| o.family_name.clone())
        .unwrap_or_else(|| {
            crate::ui::tr(cx)
                .settings()
                .drawer_forms_official_reference_default()
                .into()
        });
    let ent_f = cx.entity().clone();
    let offers_owned: Vec<(String, String)> = offers
        .iter()
        .map(|o| (o.family.clone(), o.family_name.clone()))
        .collect();

    let pick = popover("cat-drawer-offer-pop")
        .anchor(Anchor::TopLeft)
        .trigger(select_pill(
            "cat-drawer-offer-btn",
            SharedString::from(label),
            active_fam.is_some(),
            cx,
        ))
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut list = v_col().w(px(230.0)).gap(px(2.0));
            let sel0 = active_fam.is_none();
            let e0 = ent_f.clone();
            let ph0 = pop_cx.entity().clone();
            list = list.child(
                list_row("dr-off-def", sel0, pop_cx, move |_, window, cx| {
                    e0.update(cx, |this, cx| {
                        this.selected_catalog_family = None;
                        cx.notify();
                    });
                    ph0.update(cx, |st, cx| st.dismiss(window, cx));
                })
                .justify_between()
                .child(
                    div().text_xs().child(
                        crate::ui::tr(pop_cx)
                            .settings()
                            .drawer_forms_official_reference_default(),
                    ),
                )
                .children(sel0.then(|| {
                    div()
                        .text_color(to_hsla(pop_p.accent))
                        .child(icon_xs(UiIcon::Check))
                })),
            );
            for (i, (fid, fname)) in offers_owned.iter().enumerate() {
                let sel = active_fam.as_deref() == Some(fid.as_str());
                let ef = ent_f.clone();
                let fv = fid.clone();
                let ph = pop_cx.entity().clone();
                list = list.child(
                    list_row(
                        SharedString::from(format!("dr-off-{i}")),
                        sel,
                        pop_cx,
                        move |_, window, cx| {
                            ef.update(cx, |this, cx| {
                                this.selected_catalog_family = Some(fv.clone());
                                cx.notify();
                            });
                            ph.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(div().text_xs().child(SharedString::from(fname.clone())))
                    .children(sel.then(|| {
                        div()
                            .text_color(to_hsla(pop_p.accent))
                            .child(icon_xs(UiIcon::Check))
                    })),
                );
            }
            list
        });

    h_row()
        .justify_between()
        .gap(px(8.0))
        .child(
            div().text_xs().text_color(to_hsla(p.muted)).child(
                crate::ui::tr(cx)
                    .settings()
                    .drawer_forms_follow_catalog_offer(),
            ),
        )
        .child(pick)
}

pub fn render_provider_overrides_list(
    detail: &ModelPriceDetail,
    cx: &mut Context<CatalogDrawer>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let box_el = v_col().gap(px(6.0)).child(section_heading(
        crate::ui::tr(cx)
            .settings()
            .catalog_drawer_yours(detail.providers.len()),
        cx,
    ));

    if detail.providers.is_empty() {
        return box_el.child(
            div().text_xs().text_color(to_hsla(p.muted)).child(
                crate::ui::tr(cx)
                    .settings()
                    .drawer_forms_none_of_your_configured_providers(),
            ),
        );
    }

    let mut list = v_col();
    for (i, pm) in detail.providers.iter().enumerate() {
        list = list.child(render_provider_model_row(
            i,
            pm,
            i + 1 < detail.providers.len(),
            cx,
        ));
    }
    box_el.child(card(cx).child(list))
}

fn render_provider_model_row(
    idx: usize,
    pm: &ProviderModelPrice,
    border: bool,
    cx: &mut Context<CatalogDrawer>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let lang = crate::ui::active_language(cx);
    let eff = pm
        .effective
        .as_ref()
        .map(|e| {
            format!(
                "{} · {}",
                format_effective_price(e, lang),
                format_basis(&e.basis, lang)
            )
        })
        .unwrap_or_else(|| crate::ui::tr(cx).settings().catalog_mode_none().into());
    let has_override = pm.pricing.is_some();
    let pid_del = pm.provider_id.clone();
    let mid_del = pm.model_id.clone();

    let pm_edit = pm.clone();
    let mut row = h_row()
        .justify_between()
        .gap(px(8.0))
        .px(px(12.0))
        .py(px(8.0))
        .child(
            v_col()
                .flex_1()
                .min_w_0()
                .gap(px(2.0))
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .child(SharedString::from(pm.provider_name.clone())),
                        )
                        .children((!pm.listed).then(|| {
                            badge(
                                crate::ui::tr(cx).settings().drawer_forms_unlisted(),
                                Tone::Warning,
                                cx,
                            )
                        })),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .truncate()
                        .child(SharedString::from(eff)),
                ),
        )
        .child(
            h_row()
                .gap(px(4.0))
                .child(
                    btn_ghost(
                        SharedString::from(format!("cat-pm-ovr-{idx}")),
                        if has_override {
                            crate::ui::tr(cx).settings().drawer_forms_edit()
                        } else {
                            crate::ui::tr(cx).settings().drawer_forms_override()
                        },
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.start_edit_provider_override(&pm_edit, window, cx);
                    })),
                )
                .children(has_override.then(|| {
                    btn_ghost(
                        SharedString::from(format!("cat-pm-del-{idx}")),
                        crate::ui::tr(cx).settings().drawer_forms_reset(),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.delete_provider_override(pid_del.clone(), mid_del.clone(), window, cx);
                    }))
                })),
        );

    if border {
        row = row
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)));
    }
    row
}
