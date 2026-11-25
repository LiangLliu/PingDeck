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
    let models_tab_lbl = crate::ui::tr(cx)
        .providers()
        .prov_detail_models(prov_models_cnt);

    let tabs = tab_bar_underline("prov-detail-tabs")
        .selected_index(view.provider_tab.index())
        .child(tab_item(crate::ui::tr(cx).providers().prov_tab_overview()))
        .child(tab_item(models_tab_lbl))
        .child(tab_item(crate::ui::tr(cx).providers().prov_detail_config()))
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
                                    provider_status_label(st, crate::ui::active_language(cx)),
                                    provider_status_tone(st),
                                    cx,
                                ))
                                .child(badge(
                                    format_family(
                                        prov.gateway_family,
                                        crate::ui::active_language(cx),
                                    ),
                                    Tone::Accent,
                                    cx,
                                ))
                                .children(status_dto.and_then(|s| s.reasons.first()).map(|r| {
                                    badge(
                                        format_reason(r, crate::ui::active_language(cx)),
                                        Tone::Warning,
                                        cx,
                                    )
                                })),
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
                                            crate::ui::tr(cx).providers().prov_pane_copied().into(),
                                        ));
                                        cx.notify();
                                    }),
                                ))
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(to_hsla(p.faint))
                                        .child(SharedString::from(
                                            crate::ui::tr(cx).providers().prov_detail_models_keys(
                                                prov_models_cnt,
                                                prov_keys_cnt,
                                            ),
                                        )),
                                ),
                        ),
                )
                .child(
                    h_row()
                        .flex_shrink_0()
                        .gap(px(6.0))
                        .children((!prov.enabled).then(|| {
                            btn_primary(
                                "prov-quick-enable",
                                crate::ui::tr(cx).providers().prov_detail_enable(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.toggle_provider_enabled(enable_id.clone(), true, cx);
                                },
                            ))
                        }))
                        .child(
                            btn_secondary(
                                "prov-resniff-btn",
                                crate::ui::tr(cx).providers().prov_detail_re_scan(),
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
                                crate::ui::tr(cx).providers().prov_detail_test_bench(),
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
            .child(detail_models::render_models_tab(view, pv, cx))
            .into_any_element(),
        ProviderTab::Overview => div()
            .flex_1()
            .min_h_0()
            .w_full()
            .p(px(20.0))
            .overflow_y_scrollbar()
            .child(div().w_full().child(render_overview_tab(view, pv, cx)))
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
                .w_full()
                .p(px(20.0))
                .overflow_y_scrollbar()
                .child(div().w_full().child(body))
                .into_any_element()
        }
    };

    v_col().size_full().child(header).child(content)
}

fn render_overview_tab(
    view: &ProvidersView,
    pv: &ProviderView,
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
                        .child(
                            div().text_xs().text_color(to_hsla(p.ink)).child(
                                crate::ui::tr(cx)
                                    .providers()
                                    .prov_detail_imported_and_unreviewed_will_not(),
                            ),
                        )
                        .child(
                            btn_primary(
                                "prov-mark-reviewed",
                                crate::ui::tr(cx).providers().prov_detail_mark_reviewed(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.mark_provider_reviewed(mark_id.clone(), cx);
                                },
                            )),
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
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.ink))
                                .child(SharedString::from(
                                    crate::ui::tr(cx)
                                        .providers()
                                        .prov_detail_model_list_drift_added_removed(
                                            drift.added.len(),
                                            drift.removed.len(),
                                        ),
                                )),
                        )
                        .child(
                            h_row()
                                .gap(px(5.0))
                                .children((!drift.added.is_empty()).then(|| {
                                    btn_primary(
                                        "ov-drift-apply",
                                        crate::ui::tr(cx).providers().models_drift_apply(),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.apply_model_drift(apply_id.clone(), cx);
                                        },
                                    ))
                                }))
                                .child(
                                    btn_secondary(
                                        "ov-drift-view",
                                        crate::ui::tr(cx).providers().prov_detail_view_models(),
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
                                        crate::ui::tr(cx).providers().prov_detail_dismiss(),
                                    )
                                    .on_click(cx.listener(
                                        move |this, _, _, cx| {
                                            this.ack_model_drift(ack_id.clone(), cx);
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
                    .child(
                        div().text_xs().text_color(to_hsla(p.muted)).child(
                            crate::ui::tr(cx)
                                .providers()
                                .prov_detail_no_enabled_api_key_yet(),
                        ),
                    )
                    .child(
                        btn_primary(
                            "ov-add-key-banner",
                            crate::ui::tr(cx).providers().prov_detail_add_key(),
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
        .child(detail_keys::render_keys_card(view, prov, &prov_keys, cx))
        .child(detail_side::render_endpoints_card(view, prov, cx));

    let side_col = v_col()
        .w(px(296.0))
        .flex_shrink_0()
        .gap(px(12.0))
        .child(detail_side::render_balance_card(view, prov, &prov_keys, cx))
        .child(detail_side::render_health_card(view, prov, cx))
        .child(detail_side::render_recent_calls_card(view, &prov.id, cx));

    v_col().gap(px(12.0)).child(top_banners).child(
        h_row()
            .items_start()
            .gap(px(16.0))
            .child(main_col)
            .child(side_col),
    )
}
