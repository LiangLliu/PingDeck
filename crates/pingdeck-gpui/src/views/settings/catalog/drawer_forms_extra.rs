//! Add-custom-model form, Unlisted model catalog matcher & pricing form, and shared Sheet footer.

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::model::model_catalog::{CatalogKind, ModelCatalogRow, PricingMode};

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
    zh: bool,
    cx: &mut Context<CatalogDrawer>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let box_el = v_col().gap(px(6.0)).child(section_heading(
        if zh {
            format!("目录报价（{} 家）", offers.len())
        } else {
            format!("Catalog offers ({})", offers.len())
        },
        cx,
    ));
    if offers.is_empty() {
        return box_el.child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
            "目录中没有分发商提供这个模型。"
        } else {
            "No catalog distributors list this model."
        }));
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
                    .children(
                        (i == 0)
                            .then(|| badge(if zh { "官方" } else { "Official" }, Tone::Accent, cx)),
                    ),
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
                        if zh { "已选" } else { "Active" }
                    } else if zh {
                        "选用"
                    } else {
                        "Use"
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
    zh: bool,
    cx: &mut Context<CatalogDrawer>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ent_k = cx.entity().clone();
    let ent_c = cx.entity().clone();
    let kind_idx = usize::from(drawer.user_kind == CatalogKind::Embedding);

    v_col()
        .gap(px(12.0))
        .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
            "用于目录里没有的模型（如中转站自有模型或本地模型）。models.dev 同步不会改动它。"
        } else {
            "For models not listed on models.dev (e.g. relay or local models). Sync never touches it."
        }))
        .child(
            h_row()
                .gap(px(10.0))
                .child(labeled_input(
                    if zh { "厂商 *" } else { "Family *" },
                    &drawer.user_family,
                    cx,
                ))
                .child(labeled_input(
                    if zh { "模型 ID *" } else { "Model ID *" },
                    &drawer.user_model_id,
                    cx,
                )),
        )
        .child(
            h_row()
                .gap(px(10.0))
                .items_end()
                .child(labeled_input(
                    if zh { "显示名称" } else { "Display name" },
                    &drawer.user_name,
                    cx,
                ))
                .child(
                    tab_bar_segmented("cat-user-kind-seg")
                        .selected_index(kind_idx)
                        .child(tab_item(if zh { "对话" } else { "Chat" }))
                        .child(tab_item(if zh { "嵌入" } else { "Embedding" }))
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
                    if zh { "上下文长度" } else { "Context window" },
                    &drawer.user_context,
                    cx,
                ))
                .child(labeled_input(
                    if zh { "最大输出" } else { "Max output" },
                    &drawer.user_max_output,
                    cx,
                )),
        )
        .child(
            h_row()
                .gap(px(14.0))
                .child(
                    checkbox("cat-u-reason", drawer.user_reasoning)
                        .label(if zh { "推理" } else { "Reasoning" })
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.user_reasoning = *checked;
                            cx.notify();
                        })),
                )
                .child(
                    checkbox("cat-u-vision", drawer.user_vision)
                        .label(if zh { "视觉" } else { "Vision" })
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.user_vision = *checked;
                            cx.notify();
                        })),
                )
                .child(
                    checkbox("cat-u-tools", drawer.user_tools)
                        .label(if zh { "工具调用" } else { "Tools" })
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
                    if zh {
                        "目录价格（每百万 token，留空为无价格）"
                    } else {
                        "Catalog prices per 1M tokens (optional)"
                    },
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
                    if zh { "输入" } else { "Input" },
                    &drawer.in_price,
                    cx,
                ))
                .child(labeled_input(
                    if zh { "输出" } else { "Output" },
                    &drawer.out_price,
                    cx,
                )),
        )
        .child(
            h_row()
                .gap(px(10.0))
                .child(labeled_input(
                    if zh { "缓存读取" } else { "Cache read" },
                    &drawer.cache_read_price,
                    cx,
                ))
                .child(labeled_input(
                    if zh { "缓存写入" } else { "Cache write" },
                    &drawer.cache_write_price,
                    cx,
                )),
        )
}

pub fn render_unlisted_body(
    drawer: &CatalogDrawer,
    provider_name: &str,
    zh: bool,
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
                .child(if zh {
                    "没有匹配的目录模型"
                } else {
                    "No matching catalog models"
                }),
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
                    if zh {
                        "这是哪个模型？（关联到目录）"
                    } else {
                        "Which catalog model is this?"
                    },
                    cx,
                ))
                .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                    format!("之后这个 ID 按所选目录模型计价，只对 {provider_name} 生效。")
                } else {
                    format!(
                        "This ID will follow the selected catalog model's pricing for {provider_name}."
                    )
                }))
                .child(search_input(&drawer.alias_search, cx))
                .child(alias_list),
        )
        .child(
            v_col()
                .gap(px(8.0))
                .child(section_heading(
                    if zh {
                        "或者直接为它设定价格"
                    } else {
                        "Or set a direct price"
                    },
                    cx,
                ))
                .child(render_pricing_editor_fields(drawer, &[], zh, cx)),
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
                .child(div().text_xs().text_color(to_hsla(p.warn)).child(if zh {
                    "尚未关联目录模型且未设价格，费用将显示为未知。"
                } else {
                    "No catalog match or custom price set; cost will show as unknown."
                })),
        );
    }
    col
}

pub fn render_drawer_footer(
    show_reset: bool,
    show_delete_user: bool,
    busy: bool,
    zh: bool,
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
                        if zh {
                            "恢复默认"
                        } else {
                            "Reset to default"
                        },
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.reset_pricing(window, cx)))
                }))
                .children(show_delete_user.then(|| {
                    btn_danger(
                        "cat-dr-del-user",
                        if zh { "删除模型" } else { "Delete model" },
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.delete_user_model(window, cx)))
                })),
        )
        .child(
            h_row()
                .gap(px(6.0))
                .child(
                    btn_secondary("cat-dr-cancel", if zh { "取消" } else { "Cancel" })
                        .on_click(cx.listener(|this, _, window, cx| this.close(window, cx))),
                )
                .child(
                    btn_primary(
                        "cat-dr-save",
                        if busy {
                            if zh { "保存中…" } else { "Saving…" }
                        } else if zh {
                            "保存更改"
                        } else {
                            "Save changes"
                        },
                    )
                    .on_click(cx.listener(|this, _, window, cx| this.save(window, cx))),
                ),
        )
}

pub fn render_user_model_meta_editor(
    drawer: &CatalogDrawer,
    zh: bool,
    cx: &mut Context<CatalogDrawer>,
) -> gpui_kit::Div {
    v_col()
        .gap(px(8.0))
        .child(section_heading(
            if zh {
                "自建模型参数"
            } else {
                "Custom model parameters"
            },
            cx,
        ))
        .child(
            h_row()
                .gap(px(8.0))
                .child(labeled_input(
                    if zh { "显示名称" } else { "Name" },
                    &drawer.user_name,
                    cx,
                ))
                .child(labeled_input(
                    if zh { "上下文" } else { "Context" },
                    &drawer.user_context,
                    cx,
                ))
                .child(labeled_input(
                    if zh { "最大输出" } else { "Max out" },
                    &drawer.user_max_output,
                    cx,
                )),
        )
        .child(
            h_row()
                .gap(px(14.0))
                .child(
                    checkbox("cat-ed-reason", drawer.user_reasoning)
                        .label(if zh { "推理" } else { "Reasoning" })
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.user_reasoning = *checked;
                            cx.notify();
                        })),
                )
                .child(
                    checkbox("cat-ed-vision", drawer.user_vision)
                        .label(if zh { "视觉" } else { "Vision" })
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.user_vision = *checked;
                            cx.notify();
                        })),
                )
                .child(
                    checkbox("cat-ed-tools", drawer.user_tools)
                        .label(if zh { "工具" } else { "Tools" })
                        .on_click(cx.listener(|this, checked: &bool, _, cx| {
                            this.user_tools = *checked;
                            cx.notify();
                        })),
                ),
        )
}
