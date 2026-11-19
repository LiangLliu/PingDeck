//! Main Models & Pricing table, pagination footer, and Unlisted Models section.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::model::model_catalog::{CatalogSource, ModelCatalogRow};

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, btn_ghost, btn_primary, btn_secondary, card, filter_chip,
    h_row, palette, search_input, section_heading, status_indicator, to_hsla, v_col,
};

use super::CatalogSettingsView;
use super::filters::{
    render_kind_filter, render_mode_filter, render_publisher_filter, render_source_filter,
};
use super::format::{format_effective_price, format_row_prices, format_tokens};

pub fn render_models_section(
    view: &CatalogSettingsView,
    zh: bool,
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
                        if zh { "只看在用的" } else { "In use only" },
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
                        if zh {
                            "分发商自有条目"
                        } else {
                            "Distributor entries"
                        },
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
                        if zh {
                            "+ 添加自建模型"
                        } else {
                            "+ Add custom model"
                        },
                    )
                    .h(px(30.0))
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_add_user_model_drawer(window, cx);
                    })),
                ),
        );

    let count_text = if zh {
        format!("已显示 {shown} 个，共 {total} 个")
    } else {
        format!("Showing {shown} of {total}")
    };

    let toolbar_filters = h_row()
        .justify_between()
        .gap(px(8.0))
        .flex_wrap()
        .child(
            h_row()
                .gap(px(6.0))
                .flex_wrap()
                .child(render_publisher_filter(view, zh, cx))
                .child(render_kind_filter(view, zh, cx))
                .child(render_source_filter(view, zh, cx))
                .child(render_mode_filter(view, zh, cx))
                .children(has_filters.then(|| {
                    btn_ghost(
                        "cat-clear-filters",
                        if zh { "清除筛选" } else { "Clear filters" },
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

    let thead =
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
            .child(
                div()
                    .flex_1()
                    .min_w(px(210.0))
                    .child(if zh { "模型" } else { "Model" }),
            )
            .child(div().w(px(108.0)).child(if zh {
                "上下文 / 输出"
            } else {
                "Context / Out"
            }))
            .child(div().w(px(82.0)).flex().justify_end().child(if zh {
                "输入 / 1M"
            } else {
                "Input / 1M"
            }))
            .child(div().w(px(82.0)).flex().justify_end().child(if zh {
                "输出 / 1M"
            } else {
                "Output / 1M"
            }))
            .child(div().w(px(110.0)).flex().justify_end().child(if zh {
                "缓存 读 / 写"
            } else {
                "Cache R / W"
            }))
            .child(div().w(px(88.0)).flex().justify_end().child(if zh {
                "计价"
            } else {
                "Pricing"
            }));

    let mut tbody = v_col();
    if view.loading_page && shown == 0 {
        tbody = tbody.child(
            h_row()
                .justify_center()
                .py(px(28.0))
                .gap(px(8.0))
                .child(status_indicator(StatusShape::Run, cx))
                .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                    "正在加载模型目录…"
                } else {
                    "Loading model catalog…"
                })),
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
                            if zh {
                                "没有符合筛选条件的模型"
                            } else {
                                "No models match these filters"
                            }
                        } else if zh {
                            "模型目录是空的"
                        } else {
                            "The model catalog is empty"
                        }),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(if has_filters {
                            if zh {
                                "尝试放宽搜索关键词或清除筛选条件。"
                            } else {
                                "Try clearing filters or broadening your search."
                            }
                        } else if zh {
                            "点击「立即同步」下载 models.dev 目录，或添加自建模型。"
                        } else {
                            "Click Sync now to download models.dev, or add a custom model."
                        }),
                ),
        );
    } else if let Some(pg) = &view.page {
        for (idx, row) in pg.rows.iter().enumerate() {
            tbody = tbody.child(render_catalog_row(
                idx,
                row,
                idx + 1 < pg.rows.len(),
                zh,
                cx,
            ));
        }
    }

    let mut table_card = card(cx).overflow_hidden().child(thead).child(tbody);

    if total > shown {
        let remaining = total - shown;
        let step = remaining.min(80);
        table_card = table_card.child(
            h_row()
                .justify_center()
                .py(px(10.0))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(
                    btn_secondary(
                        "cat-load-more",
                        if view.loading_more {
                            if zh {
                                "加载中…".into()
                            } else {
                                "Loading…".into()
                            }
                        } else if zh {
                            format!("再加载 {step} 个（剩余 {remaining} 个）")
                        } else {
                            format!("Load {step} more ({remaining} left)")
                        },
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        if !this.loading_more {
                            this.load_page(true, cx);
                        }
                    })),
                ),
        );
    }

    v_col()
        .gap(px(8.0))
        .child(section_heading(if zh { "模型" } else { "Models" }, cx))
        .child(toolbar_top)
        .child(toolbar_filters)
        .child(table_card)
        .child(div().text_xs().text_color(to_hsla(p.faint)).child(if zh {
            "价格按每百万 token 计，币种见各行；固定价格按每次成功请求计。不同币种的金额不会相加或换算。点击任意行可编辑价格、查看各分发商报价与配置服务商覆盖。"
        } else {
            "Prices are per 1M tokens in each row's currency; fixed prices apply per successful request. Currencies are never mixed or converted. Click any row to edit pricing or provider overrides."
        }))
}

fn render_catalog_row(
    idx: usize,
    row: &ModelCatalogRow,
    has_border: bool,
    zh: bool,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let (inp, out, cache, mode_label, mode_tone) = format_row_prices(row, zh);
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
        caps = caps.child(badge(if zh { "自建" } else { "Custom" }, Tone::Accent, cx));
    }
    if row.in_use.unwrap_or(0) > 0 {
        caps = caps.child(badge(if zh { "在用" } else { "In use" }, Tone::Success, cx));
    }
    if row.provider_overrides > 0 {
        caps = caps.child(badge(
            if zh {
                format!("{}家另计", row.provider_overrides)
            } else {
                format!("{} ovr", row.provider_overrides)
            },
            Tone::Warning,
            cx,
        ));
    }
    if row.reasoning {
        caps = caps.child(cap_chip(if zh { "推理" } else { "Reason" }, cx));
    }
    if row.vision {
        caps = caps.child(cap_chip(if zh { "视觉" } else { "Vision" }, cx));
    }
    if row.tools {
        caps = caps.child(cap_chip(if zh { "工具" } else { "Tools" }, cx));
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
    zh: bool,
    cx: &mut Context<CatalogSettingsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let count = view.unlisted.len();

    let thead =
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
            .child(
                div()
                    .w(px(180.0))
                    .child(if zh { "你的服务商" } else { "Provider" }),
            )
            .child(
                div()
                    .flex_1()
                    .child(if zh { "模型 ID" } else { "Model ID" }),
            )
            .child(
                div()
                    .w(px(150.0))
                    .child(if zh { "当前价格" } else { "Price" }),
            )
            .child(div().w(px(120.0)).flex().justify_end().child(if zh {
                "操作"
            } else {
                "Action"
            }));

    let mut tbody = v_col();
    for (idx, item) in view.unlisted.iter().enumerate() {
        let price_str = item
            .effective
            .as_ref()
            .map(|e| format_effective_price(e, zh))
            .unwrap_or_else(|| {
                if zh {
                    "无价格".into()
                } else {
                    "No price".into()
                }
            });
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
                div().w(px(120.0)).flex().justify_end().child(
                    btn_secondary(
                        SharedString::from(format!("cat-unlisted-btn-{idx}")),
                        if zh {
                            "设置价格 / 指认"
                        } else {
                            "Price / Match"
                        },
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_unlisted_drawer(item_btn.clone(), window, cx);
                    })),
                ),
            );
        if idx + 1 < count {
            row = row
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)));
        }
        tbody = tbody.child(row);
    }

    v_col()
        .gap(px(6.0))
        .child(section_heading(
            if zh {
                format!("未收录的模型（{count}）")
            } else {
                format!("Unlisted models ({count})")
            },
            cx,
        ))
        .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
            "你的服务商提供、目录里还没有的模型；点击行可关联到目录模型或单独定价。"
        } else {
            "Models your providers serve that are not in the catalog; click a row to match to a catalog model or set a price."
        }))
        .child(card(cx).overflow_hidden().child(thead).child(tbody))
}
