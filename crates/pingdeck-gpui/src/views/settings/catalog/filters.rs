//! Native anchored Popover filter pickers (publisher, kind, source, pricing mode) for the
//! "Models & Pricing" catalog table.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::model::model_catalog::{
    CatalogKind, CatalogSource, ModelCatalogFamily, PricingFilter,
};

use crate::ui::{
    Anchor, ScrollableElement as _, UiIcon, icon_xs, list_row, palette, popover, select_pill,
    to_hsla, v_col,
};

use super::CatalogSettingsView;

#[allow(clippy::too_many_arguments)]
pub fn render_provider_family_picker(
    idx: usize,
    pid: String,
    pname: String,
    cur_fam: Option<String>,
    sug_fam: Option<String>,
    sug_name: Option<String>,
    current_label: String,
    fams: Vec<ModelCatalogFamily>,
    zh: bool,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement + use<> {
    let view_ent = cx.entity().clone();
    let active = cur_fam.is_some();

    popover(SharedString::from(format!("cat-pp-pop-{idx}")))
        .anchor(Anchor::TopRight)
        .trigger(select_pill(
            SharedString::from(format!("cat-pp-btn-{idx}")),
            SharedString::from(current_label),
            active,
            cx,
        ))
        .content(move |_, _, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut list = v_col()
                .w(px(250.0))
                .max_h(px(300.0))
                .gap(px(2.0))
                .overflow_y_scrollbar();

            let sel_off = cur_fam.is_none();
            let e1 = view_ent.clone();
            let pid1 = pid.clone();
            let pname1 = pname.clone();
            let ph1 = pop_cx.entity().clone();
            list = list.child(
                list_row(
                    SharedString::from(format!("pp-off-{idx}")),
                    sel_off,
                    pop_cx,
                    move |_, window, cx| {
                        e1.update(cx, |this, cx| {
                            this.save_provider_pricing(pid1.clone(), pname1.clone(), None, zh, cx);
                        });
                        ph1.update(cx, |st, cx| st.dismiss(window, cx));
                    },
                )
                .justify_between()
                .child(div().text_xs().child(if zh {
                    "跟随官方价（默认）"
                } else {
                    "Follow official prices"
                }))
                .children(sel_off.then(|| {
                    div()
                        .text_color(to_hsla(pop_p.accent))
                        .child(icon_xs(UiIcon::Check))
                })),
            );

            if let Some(sf) = &sug_fam {
                let sname = sug_name.as_deref().unwrap_or(sf.as_str());
                let label = if zh {
                    format!("{sname}（建议）")
                } else {
                    format!("{sname} (suggested)")
                };
                let sel = cur_fam.as_deref() == Some(sf.as_str());
                let e2 = view_ent.clone();
                let pid2 = pid.clone();
                let pname2 = pname.clone();
                let sf_val = sf.clone();
                let ph2 = pop_cx.entity().clone();
                list = list.child(
                    list_row(
                        SharedString::from(format!("pp-sug-{idx}")),
                        sel,
                        pop_cx,
                        move |_, window, cx| {
                            e2.update(cx, |this, cx| {
                                this.save_provider_pricing(
                                    pid2.clone(),
                                    pname2.clone(),
                                    Some(sf_val.clone()),
                                    zh,
                                    cx,
                                );
                            });
                            ph2.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(pop_p.accent_ink))
                            .child(SharedString::from(label)),
                    )
                    .children(sel.then(|| {
                        div()
                            .text_color(to_hsla(pop_p.accent))
                            .child(icon_xs(UiIcon::Check))
                    })),
                );
            }

            for (fi, fam) in fams.iter().enumerate() {
                if sug_fam.as_deref() == Some(fam.id.as_str()) {
                    continue;
                }
                let sel = cur_fam.as_deref() == Some(fam.id.as_str());
                let ef = view_ent.clone();
                let pidf = pid.clone();
                let pnamef = pname.clone();
                let fid = fam.id.clone();
                let phf = pop_cx.entity().clone();
                list =
                    list.child(
                        list_row(
                            SharedString::from(format!("pp-f-{idx}-{fi}")),
                            sel,
                            pop_cx,
                            move |_, window, cx| {
                                ef.update(cx, |this, cx| {
                                    this.save_provider_pricing(
                                        pidf.clone(),
                                        pnamef.clone(),
                                        Some(fid.clone()),
                                        zh,
                                        cx,
                                    );
                                });
                                phf.update(cx, |st, cx| st.dismiss(window, cx));
                            },
                        )
                        .justify_between()
                        .child(
                            div().flex_1().min_w_0().text_xs().truncate().child(
                                SharedString::from(format!("{} ({})", fam.name, fam.models)),
                            ),
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

pub fn render_publisher_filter(
    view: &CatalogSettingsView,
    zh: bool,
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
        .unwrap_or_else(|| {
            if zh {
                "全部厂商".into()
            } else {
                "All publishers".into()
            }
        });
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
                        .child(if zh { "全部厂商" } else { "All publishers" }),
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
    zh: bool,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement {
    let active = view.kind;
    let label = match (active, zh) {
        (None, true) => "全部类型",
        (None, false) => "All kinds",
        (Some(CatalogKind::Chat), true) => "对话",
        (Some(CatalogKind::Chat), false) => "Chat",
        (Some(CatalogKind::Embedding), true) => "嵌入",
        (Some(CatalogKind::Embedding), false) => "Embedding",
    };
    let ent = cx.entity().clone();
    let opts: [(Option<CatalogKind>, &str); 3] = [
        (None, if zh { "全部类型" } else { "All kinds" }),
        (Some(CatalogKind::Chat), if zh { "对话" } else { "Chat" }),
        (
            Some(CatalogKind::Embedding),
            if zh { "嵌入" } else { "Embedding" },
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
    zh: bool,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement {
    let active = view.source;
    let label = match (active, zh) {
        (None, true) => "全部来源",
        (None, false) => "All sources",
        (Some(CatalogSource::ModelsDev), _) => "models.dev",
        (Some(CatalogSource::User), true) => "自建",
        (Some(CatalogSource::User), false) => "Custom",
    };
    let ent = cx.entity().clone();
    let opts: [(Option<CatalogSource>, &str); 3] = [
        (None, if zh { "全部来源" } else { "All sources" }),
        (Some(CatalogSource::ModelsDev), "models.dev"),
        (
            Some(CatalogSource::User),
            if zh { "自建" } else { "Custom" },
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
    zh: bool,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement {
    let active = view.mode;
    let label = match (active, zh) {
        (None, true) => "全部计价",
        (None, false) => "All pricing",
        (Some(PricingFilter::Dynamic), true) => "跟随目录",
        (Some(PricingFilter::Dynamic), false) => "Catalog",
        (Some(PricingFilter::Custom), true) => "自定义",
        (Some(PricingFilter::Custom), false) => "Custom",
        (Some(PricingFilter::Fixed), true) => "按次",
        (Some(PricingFilter::Fixed), false) => "Per call",
        (Some(PricingFilter::None), true) => "无价格",
        (Some(PricingFilter::None), false) => "No price",
    };
    let ent = cx.entity().clone();
    let opts: [(Option<PricingFilter>, &str); 5] = [
        (None, if zh { "全部计价" } else { "All pricing" }),
        (
            Some(PricingFilter::Dynamic),
            if zh { "跟随目录" } else { "Catalog" },
        ),
        (
            Some(PricingFilter::Custom),
            if zh { "自定义" } else { "Custom" },
        ),
        (
            Some(PricingFilter::Fixed),
            if zh { "按次" } else { "Per call" },
        ),
        (
            Some(PricingFilter::None),
            if zh { "无价格" } else { "No price" },
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
