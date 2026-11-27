//! Native anchored Popover filter pickers (publisher, kind, source, pricing mode) for the
//! "Models & Pricing" catalog table.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::model::model_catalog::{
    CatalogKind, CatalogSource, ModelCatalogFamily, PricingFilter,
};

use crate::ui::{
    Anchor, ScrollableElement as _, UiIcon, icon_xs, list_row, palette, popover, select_pill,
    to_hsla, v_col,
};

use super::CatalogSettingsView;

pub fn render_publisher_filter(
    view: &CatalogSettingsView,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement {
    let pubs: Vec<ModelCatalogFamily> = view
        .page
        .as_ref()
        .map(|pg| pg.publishers.clone())
        .unwrap_or_default();
    let active = view.publisher.clone();
    let label = active
        .as_deref()
        .and_then(|id| pubs.iter().find(|f| f.id == id).map(|f| f.name.clone()))
        .unwrap_or_else(|| crate::ui::tr(cx).settings().catalog_all_publishers().into());
    let ent = cx.entity().clone();

    popover("cat-flt-pub-pop")
        .anchor(Anchor::TopLeft)
        .trigger(select_pill(
            "cat-flt-pub-btn",
            SharedString::from(label),
            active.is_some(),
            cx,
        ))
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut list = v_col()
                .w(px(230.0))
                .max_h(px(300.0))
                .gap(px(2.0))
                .overflow_y_scrollbar();
            let sel_all = active.is_none();
            let e0 = ent.clone();
            let ph0 = pop_cx.entity().clone();
            list = list.child(
                list_row("cat-pub-all", sel_all, pop_cx, move |_, window, cx| {
                    e0.update(cx, |this, cx| {
                        this.publisher = None;
                        this.load_page(false, cx);
                    });
                    ph0.update(cx, |st, cx| st.dismiss(window, cx));
                })
                .justify_between()
                .child(
                    div()
                        .text_xs()
                        .child(crate::ui::tr(pop_cx).settings().catalog_all_publishers()),
                )
                .children(sel_all.then(|| {
                    div()
                        .text_color(to_hsla(pop_p.accent))
                        .child(icon_xs(UiIcon::Check))
                })),
            );
            for (i, f) in pubs.iter().enumerate() {
                let sel = active.as_deref() == Some(f.id.as_str());
                let e = ent.clone();
                let fid = f.id.clone();
                let ph = pop_cx.entity().clone();
                list = list.child(
                    list_row(
                        SharedString::from(format!("cat-pub-{i}")),
                        sel,
                        pop_cx,
                        move |_, window, cx| {
                            e.update(cx, |this, cx| {
                                this.publisher = Some(fid.clone());
                                this.load_page(false, cx);
                            });
                            ph.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .truncate()
                            .child(SharedString::from(format!("{} ({})", f.name, f.models))),
                    )
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

pub fn render_kind_filter(
    view: &CatalogSettingsView,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement {
    let active = view.kind;
    let s = crate::ui::tr(cx).settings();
    let label = match active {
        None => s.catalog_filter_all_kinds(),
        Some(CatalogKind::Chat) => s.catalog_filter_kind_chat(),
        Some(CatalogKind::Embedding) => s.catalog_filter_kind_embedding(),
    };
    let ent = cx.entity().clone();
    let opts: [(Option<CatalogKind>, &str); 3] = [
        (None, crate::ui::tr(cx).settings().filters_all_kinds()),
        (
            Some(CatalogKind::Chat),
            crate::ui::tr(cx).settings().catalog_kind_chat(),
        ),
        (
            Some(CatalogKind::Embedding),
            crate::ui::tr(cx).common().cap_embedding(),
        ),
    ];

    popover("cat-flt-kind-pop")
        .anchor(Anchor::TopLeft)
        .trigger(select_pill("cat-flt-kind-btn", label, active.is_some(), cx))
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut list = v_col().w(px(150.0)).gap(px(2.0));
            for (val, text) in opts {
                let sel = val == active;
                let e = ent.clone();
                let ph = pop_cx.entity().clone();
                list = list.child(
                    list_row(
                        SharedString::from(format!("cat-kind-{val:?}")),
                        sel,
                        pop_cx,
                        move |_, window, cx| {
                            e.update(cx, |this, cx| {
                                this.kind = val;
                                this.load_page(false, cx);
                            });
                            ph.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(div().text_xs().child(text))
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

pub fn render_source_filter(
    view: &CatalogSettingsView,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement {
    let active = view.source;
    let s = crate::ui::tr(cx).settings();
    let label = match active {
        None => s.catalog_filter_all_sources(),
        Some(CatalogSource::ModelsDev) => "models.dev",
        Some(CatalogSource::User) => s.catalog_filter_source_user(),
    };
    let ent = cx.entity().clone();
    let opts: [(Option<CatalogSource>, &str); 3] = [
        (None, crate::ui::tr(cx).settings().catalog_all_sources()),
        (Some(CatalogSource::ModelsDev), "models.dev"),
        (
            Some(CatalogSource::User),
            crate::ui::tr(cx).settings().catalog_source_user(),
        ),
    ];

    popover("cat-flt-src-pop")
        .anchor(Anchor::TopLeft)
        .trigger(select_pill("cat-flt-src-btn", label, active.is_some(), cx))
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut list = v_col().w(px(150.0)).gap(px(2.0));
            for (val, text) in opts {
                let sel = val == active;
                let e = ent.clone();
                let ph = pop_cx.entity().clone();
                list = list.child(
                    list_row(
                        SharedString::from(format!("cat-src-{val:?}")),
                        sel,
                        pop_cx,
                        move |_, window, cx| {
                            e.update(cx, |this, cx| {
                                this.source = val;
                                this.load_page(false, cx);
                            });
                            ph.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(div().text_xs().child(text))
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

pub fn render_mode_filter(
    view: &CatalogSettingsView,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement {
    let active = view.mode;
    let s = crate::ui::tr(cx).settings();
    let label = match active {
        None => s.catalog_filter_all_pricing(),
        Some(PricingFilter::Dynamic) => s.catalog_filter_pricing_dynamic(),
        Some(PricingFilter::Custom) => s.catalog_filter_pricing_custom(),
        Some(PricingFilter::Fixed) => s.catalog_filter_pricing_fixed(),
        Some(PricingFilter::None) => s.catalog_filter_pricing_none(),
    };
    let ent = cx.entity().clone();
    let opts: [(Option<PricingFilter>, &str); 5] = [
        (None, crate::ui::tr(cx).settings().catalog_all_modes()),
        (
            Some(PricingFilter::Dynamic),
            crate::ui::tr(cx).settings().drawer_forms_catalog(),
        ),
        (
            Some(PricingFilter::Custom),
            crate::ui::tr(cx).settings().network_mode_custom(),
        ),
        (
            Some(PricingFilter::Fixed),
            crate::ui::tr(cx).settings().catalog_mode_fixed(),
        ),
        (
            Some(PricingFilter::None),
            crate::ui::tr(cx).settings().catalog_mode_none(),
        ),
    ];

    popover("cat-flt-mode-pop")
        .anchor(Anchor::TopLeft)
        .trigger(select_pill("cat-flt-mode-btn", label, active.is_some(), cx))
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut list = v_col().w(px(150.0)).gap(px(2.0));
            for (val, text) in opts {
                let sel = val == active;
                let e = ent.clone();
                let ph = pop_cx.entity().clone();
                list = list.child(
                    list_row(
                        SharedString::from(format!("cat-mode-{val:?}")),
                        sel,
                        pop_cx,
                        move |_, window, cx| {
                            e.update(cx, |this, cx| {
                                this.mode = val;
                                this.load_page(false, cx);
                            });
                            ph.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(div().text_xs().child(text))
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
