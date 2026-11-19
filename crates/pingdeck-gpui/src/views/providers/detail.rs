//! Provider Detail View: Header + 3 Tabs (`Overview` / `Models` / `Config`).

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::ProviderView;
use pingdeck_desktop::core::llm::status::ProviderStatus;
use pingdeck_desktop::core::llm::wire::AuthStyle;
use pingdeck_desktop::services::clipboard;

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_ghost, btn_icon, btn_primary, btn_secondary,
    card, h_row, icon_xs, palette, status_indicator, tab_bar_underline, tab_item, to_hsla, v_col,
};

use super::format::{
    format_family, format_reason, provider_status_label, provider_status_shape,
    provider_status_tone,
};
use super::{ProviderTab, ProvidersView, detail_keys, detail_models, detail_side};

pub fn render_provider_detail(
    view: &ProvidersView,
    pv: &ProviderView,
    zh: bool,
    cx: &mut Context<ProvidersView>,
) -> impl IntoElement {
    let p = palette(cx);
    let prov = &pv.provider;
    let prov_id = prov.id.clone();
    let copy_url = prov.base_url.clone();
    let status_dto = view.statuses.get(&prov.id);
    let st = status_dto.map_or(
        if prov.enabled {
            ProviderStatus::Unverified
        } else {
            ProviderStatus::Disabled
        },
        |s| s.status,
    );
    let prov_models_cnt = view
        .models
        .iter()
        .filter(|m| m.model.provider_id == prov.id)
        .count();
    let prov_keys_cnt = view
        .keys
        .iter()
        .filter(|k| k.provider_id == prov.id)
        .count();

    let ent = cx.entity().clone();
    let models_tab_lbl = if zh {
        format!("模型 ({prov_models_cnt})")
    } else {
        format!("Models ({prov_models_cnt})")
    };

    let tabs = tab_bar_underline("prov-detail-tabs")
        .selected_index(view.provider_tab.index())
        .child(tab_item(if zh { "概览" } else { "Overview" }))
        .child(tab_item(models_tab_lbl))
        .child(tab_item(if zh { "配置" } else { "Config" }))
        .on_click(move |idx, _, cx| {
            let next = ProviderTab::from_index(*idx);
            ent.update(cx, |this, cx| {
                this.provider_tab = next;
                cx.notify();
            });
        });

    let resniff_id = prov_id.clone();
    let bench_id = prov_id.clone();
    let enable_id = prov_id;

    let header = v_col()
        .flex_shrink_0()
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            h_row()
                .justify_between()
                .gap(px(12.0))
                .px(px(20.0))
                .pt(px(12.0))
                .pb(px(8.0))
                .child(
                    v_col()
                        .flex_1()
                        .gap(px(3.0))
                        .min_w_0()
                        .child(
                            h_row()
                                .gap(px(7.0))
                                .flex_wrap()
                                .child(status_indicator(provider_status_shape(st), cx))
                                .child(
                                    div()
                                        .text_base()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(to_hsla(p.ink))
                                        .child(SharedString::from(prov.name.clone())),
                                )
                                .child(badge(
                                    provider_status_label(st, zh),
                                    provider_status_tone(st),
                                    cx,
                                ))
                                .child(badge(
                                    format_family(prov.gateway_family, zh),
                                    Tone::Accent,
                                    cx,
                                ))
                                .children(
                                    status_dto
                                        .and_then(|s| s.reasons.first())
                                        .map(|r| badge(format_reason(r, zh), Tone::Warning, cx)),
                                ),
                        )
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(
                                    div()
                                        .text_xs()
                                        .text_color(to_hsla(p.muted))
                                        .child(SharedString::from(prov.base_url.clone())),
                                )
                                .child(btn_icon("prov-copy-url", UiIcon::Copy).on_click(
                                    cx.listener(move |this, _, _, cx| {
                                        let _ = clipboard::set_text(&copy_url);
                                        this.status_msg = Some((
                                            Tone::Success,
                                            if zh {
                                                "基础 URL 已复制".into()
                                            } else {
                                                "Base URL copied".into()
                                            },
                                        ));
                                        cx.notify();
                                    }),
                                ))
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(to_hsla(p.faint))
                                        .child(SharedString::from(if zh {
                                            format!(
                                                "{prov_models_cnt} 个模型 · {prov_keys_cnt} 把密钥"
                                            )
                                        } else {
                                            format!(
                                                "{prov_models_cnt} models · {prov_keys_cnt} keys"
                                            )
                                        })),
                                ),
                        ),
                )
                .child(
                    h_row()
                        .flex_shrink_0()
                        .gap(px(6.0))
                        .children((!prov.enabled).then(|| {
                            btn_primary("prov-quick-enable", if zh { "启用" } else { "Enable" })
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.toggle_provider_enabled(enable_id.clone(), true, zh, cx);
                                }))
                        }))
                        .child(
                            btn_secondary(
                                "prov-resniff-btn",
                                if zh { "重新嗅探…" } else { "Re-scan…" },
                            )
                            .icon(icon_xs(UiIcon::Refresh))
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open_sniff_sheet(
                                        Some(resniff_id.clone()),
                                        None,
                                        None,
                                        window,
                                        cx,
                                    );
                                },
                            )),
                        )
                        .child(
                            btn_primary(
                                "prov-bench-btn",
                                if zh {
                                    "基准测试…"
                                } else {
                                    "Test Bench…"
                                },
                            )
                            .icon(icon_xs(UiIcon::Activity))
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    this.open_bench(Some(bench_id.clone()), Vec::new(), window, cx);
                                },
                            )),
                        ),
                ),
        )
        .child(h_row().justify_between().px(px(16.0)).child(tabs).children(
            view.status_msg.clone().map(|(tone, msg)| {
                h_row()
                    .flex_shrink_0()
                    .gap(px(3.0))
                    .child(badge(msg, tone, cx))
                    .child(
                        btn_icon("prov-clear-flash", UiIcon::Close).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.status_msg = None;
                                cx.notify();
                            },
                        )),
                    )
            }),
        ));

    let content: gpui_kit::AnyElement = match view.provider_tab {
        ProviderTab::Models => div()
            .flex_1()
            .min_h_0()
            .size_full()
            .child(detail_models::render_models_tab(view, pv, zh, cx))
            .into_any_element(),
        ProviderTab::Overview => div()
            .flex_1()
            .min_h_0()
            .p(px(20.0))
            .overflow_y_scrollbar()
            .child(render_overview_tab(view, pv, zh, cx))
            .into_any_element(),
        ProviderTab::Config => {
            let body: gpui_kit::AnyElement = if let Some(cfg) = &view.config_form {
                cfg.clone().into_any_element()
            } else {
                div().text_xs().child("Loading…").into_any_element()
            };
            div()
                .flex_1()
                .min_h_0()
                .p(px(20.0))
                .overflow_y_scrollbar()
                .child(body)
                .into_any_element()
        }
    };

    v_col().size_full().child(header).child(content)
}

fn render_overview_tab(
    view: &ProvidersView,
    pv: &ProviderView,
    zh: bool,
    cx: &mut Context<ProvidersView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let prov = &pv.provider;
    let prov_id = prov.id.clone();
    let prov_keys: Vec<_> = view
        .keys
        .iter()
        .filter(|k| k.provider_id == prov.id)
        .cloned()
        .collect();
    let has_enabled_key = prov_keys.iter().any(|k| k.enabled);

    let mut top_banners = v_col().gap(px(8.0));

    if prov.review_pending_since_ms.is_some() {
        let mark_id = prov_id.clone();
        top_banners = top_banners.child(
            card(cx)
                .p(px(11.0))
                .bg(to_hsla(p.warn_soft))
                .border_color(to_hsla(p.warn_line.composite_over(p.warn_soft)))
                .child(
                    h_row()
                        .justify_between()
                        .gap(px(8.0))
                        .child(div().text_xs().text_color(to_hsla(p.ink)).child(if zh {
                            "该服务商来自导入，尚未复核。复核前不会作为默认目标或备用目标。"
                        } else {
                            "Imported and unreviewed. Will not serve as fallback until marked reviewed."
                        }))
                        .child(
                            btn_primary(
                                "prov-mark-reviewed",
                                if zh { "标记为已复核" } else { "Mark reviewed" },
                            )
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.mark_provider_reviewed(mark_id.clone(), zh, cx);
                            })),
                        ),
                ),
        );
    }

    if let Some(drift) = &prov.model_drift
        && (!drift.added.is_empty() || !drift.removed.is_empty())
    {
        let apply_id = prov_id.clone();
        let ack_id = prov_id;
        top_banners = top_banners.child(
            card(cx)
                .p(px(11.0))
                .bg(to_hsla(p.accent_soft))
                .border_color(to_hsla(p.accent_line.composite_over(p.accent_soft)))
                .child(
                    h_row()
                        .justify_between()
                        .gap(px(8.0))
                        .flex_wrap()
                        .child(div().text_xs().text_color(to_hsla(p.ink)).child(
                            SharedString::from(if zh {
                                format!(
                                    "模型目录飘移提醒：服务商新增 {} 个模型，移除 {} 个模型。",
                                    drift.added.len(),
                                    drift.removed.len()
                                )
                            } else {
                                format!(
                                    "Model list drift: {} added, {} removed upstream.",
                                    drift.added.len(),
                                    drift.removed.len()
                                )
                            }),
                        ))
                        .child(
                            h_row()
                                .gap(px(5.0))
                                .children((!drift.added.is_empty()).then(|| {
                                    btn_primary(
                                        "ov-drift-apply",
                                        if zh {
                                            "添加新增模型"
                                        } else {
                                            "Add new models"
                                        },
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.apply_model_drift(apply_id.clone(), zh, cx);
                                        },
                                    ))
                                }))
                                .child(
                                    btn_secondary(
                                        "ov-drift-view",
                                        if zh {
                                            "查看模型 →"
                                        } else {
                                            "View models →"
                                        },
                                    )
                                    .on_click(cx.listener(
                                        |this, _, _, cx| {
                                            this.provider_tab = ProviderTab::Models;
                                            cx.notify();
                                        },
                                    )),
                                )
                                .child(
                                    btn_ghost(
                                        "ov-drift-ack",
                                        if zh { "标记已读" } else { "Dismiss" },
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.ack_model_drift(ack_id.clone(), zh, cx);
                                        },
                                    )),
                                ),
                        ),
                ),
        );
    }

    if !has_enabled_key && prov.auth != AuthStyle::None {
        top_banners = top_banners.child(
            card(cx).p(px(11.0)).bg(to_hsla(p.pane)).child(
                h_row()
                    .justify_between()
                    .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                        "还没有已启用的 API 密钥，添加密钥后即可刷新模型、探测与路由请求。"
                    } else {
                        "No enabled API key yet. Add a key to refresh models and route traffic."
                    }))
                    .child(
                        btn_primary(
                            "ov-add-key-banner",
                            if zh { "+ 添加密钥" } else { "+ Add key" },
                        )
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.adding_key_open = true;
                            cx.notify();
                        })),
                    ),
            ),
        );
    }

    let main_col = v_col()
        .flex_1()
        .min_w_0()
        .gap(px(14.0))
        .child(detail_keys::render_keys_card(
            view, prov, &prov_keys, zh, cx,
        ))
        .child(detail_side::render_endpoints_card(view, prov, zh, cx));

    let side_col = v_col()
        .w(px(296.0))
        .flex_shrink_0()
        .gap(px(12.0))
        .child(detail_side::render_balance_card(
            view, prov, &prov_keys, zh, cx,
        ))
        .child(detail_side::render_health_card(view, prov, zh, cx))
        .child(detail_side::render_recent_calls_card(view, zh, cx));

    v_col().gap(px(12.0)).child(top_banners).child(
        h_row()
            .items_start()
            .gap(px(16.0))
            .child(main_col)
            .child(side_col),
    )
}
