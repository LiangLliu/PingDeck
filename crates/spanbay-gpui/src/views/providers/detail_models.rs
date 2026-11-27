//! Provider Models Tab: pinned top toolbar & filter chips, independently scrollable table
//! (`detail_models_table.rs`), and zero-layout-shift bottom-docked selection & undo bars.

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::app::ProviderView;
use spanbay_desktop::core::llm::probe::listing::{ModelKind, Presence};
use spanbay_desktop::core::llm::wire::AuthStyle;

use crate::ui::{
    UiIcon, btn_ghost, btn_icon, btn_primary, btn_secondary, card, filter_chip, h_row, icon_xs,
    page_slice, palette, search_input, text_input, to_hsla, v_col,
};

use super::detail_models_table::render_models_table;
use super::{ModelFilterKind, ProviderTab, ProvidersView};

pub fn render_models_tab(
    view: &ProvidersView,
    pv: &ProviderView,
    cx: &mut Context<ProvidersView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let prov = &pv.provider;
    let prov_id = prov.id.clone();
    let query = view
        .model_search_input
        .read(cx)
        .value()
        .trim()
        .to_lowercase();

    let all_prov_models: Vec<_> = view
        .models
        .iter()
        .filter(|m| m.model.provider_id == prov.id)
        .collect();

    // Map latest probe result per model_id for this provider's keys.
    let prov_key_ids: std::collections::HashSet<&str> = view
        .keys
        .iter()
        .filter(|k| k.provider_id == prov.id)
        .map(|k| k.id.as_str())
        .collect();
    let mut latest_probe: std::collections::HashMap<
        &str,
        &spanbay_desktop::core::llm::store::ProbeResult,
    > = std::collections::HashMap::new();
    for res in &view.probe_results {
        if !prov_key_ids.contains(res.key_id.as_str()) {
            continue;
        }
        if res.test != "chat" && res.test != "embeddings" && res.test != "stream" {
            continue;
        }
        latest_probe
            .entry(res.model_id.as_str())
            .and_modify(|cur| {
                if res.checked_at_ms > cur.checked_at_ms {
                    *cur = res;
                }
            })
            .or_insert(res);
    }

    let filtered: Vec<_> = all_prov_models
        .iter()
        .copied()
        .filter(|m| {
            if !query.is_empty() && !m.model.model_id.to_lowercase().contains(&query) {
                return false;
            }
            match view.model_filter {
                ModelFilterKind::All => true,
                ModelFilterKind::Chat => m.kind == ModelKind::Chat,
                ModelFilterKind::Embedding => m.kind == ModelKind::Embedding,
                ModelFilterKind::Image => m.kind == ModelKind::Image,
                ModelFilterKind::Failed => latest_probe
                    .get(m.model.model_id.as_str())
                    .is_some_and(|r| !r.ok),
                ModelFilterKind::New => {
                    m.model.first_seen_ms.is_some() && m.model.new_cleared_at_ms.is_none()
                }
                ModelFilterKind::Removed => m.model.presence == Presence::NotInProvider,
            }
        })
        .collect();

    let enabled_cnt = all_prov_models.iter().filter(|m| m.model.enabled).count();
    let total_cnt = all_prov_models.len();
    let (page_rows, _, _) = page_slice(&filtered, view.models_page, view.models_page_size);
    let visible_cnt = page_rows.len();
    let pid_ref = prov_id.clone();

    // Pinned top toolbar: search + refresh + add manual model.
    let toolbar = h_row()
        .justify_between()
        .gap(px(10.0))
        .child(
            div()
                .w(px(300.0))
                .child(search_input(&view.model_search_input, cx)),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(6.0))
                .child(
                    btn_secondary(
                        "pm-refresh",
                        if view.refreshing_models {
                            crate::ui::tr(cx)
                                .providers()
                                .prov_detail_models_refreshing()
                        } else {
                            crate::ui::tr(cx).providers().models_refresh()
                        },
                    )
                    .h(px(30.0))
                    .icon(icon_xs(UiIcon::Refresh))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if !this.refreshing_models {
                            this.refresh_provider_models(pid_ref.clone(), None, cx);
                        }
                    })),
                )
                .child(
                    btn_secondary(
                        "pm-bench-all",
                        crate::ui::tr(cx)
                            .providers()
                            .prov_detail_models_test_bench(),
                    )
                    .h(px(30.0))
                    .icon(icon_xs(UiIcon::Activity))
                    .on_click(cx.listener({
                        let pid_b = prov_id.clone();
                        move |this, _, window, cx| {
                            this.open_bench(Some(pid_b.clone()), Vec::new(), window, cx);
                        }
                    })),
                )
                .child(
                    btn_secondary(
                        "pm-add-toggle",
                        crate::ui::tr(cx).providers().prov_detail_models_add_model(),
                    )
                    .h(px(30.0))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.adding_model_open = !this.adding_model_open;
                        cx.notify();
                    })),
                ),
        );

    // Filter chips strip.
    let filter_defs = [
        (
            ModelFilterKind::All,
            crate::ui::tr(cx).providers().models_filter_all(),
        ),
        (
            ModelFilterKind::Chat,
            crate::ui::tr(cx).providers().prov_keytest_chat(),
        ),
        (
            ModelFilterKind::Embedding,
            crate::ui::tr(cx).providers().prov_detail_models_embed(),
        ),
        (
            ModelFilterKind::Image,
            crate::ui::tr(cx).providers().models_filter_image(),
        ),
        (
            ModelFilterKind::Failed,
            crate::ui::tr(cx).providers().models_filter_failed(),
        ),
        (
            ModelFilterKind::New,
            crate::ui::tr(cx).providers().models_filter_new(),
        ),
        (
            ModelFilterKind::Removed,
            crate::ui::tr(cx).providers().prov_detail_models_removed(),
        ),
    ];
    let mut chips = h_row().gap(px(5.0)).flex_wrap();
    for (i, (kind, lbl)) in filter_defs.into_iter().enumerate() {
        chips = chips.child(
            filter_chip(
                SharedString::from(format!("pm-flt-{i}")),
                lbl,
                None,
                view.model_filter == kind,
                cx,
            )
            .h(px(26.0))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.model_filter = kind;
                this.models_page = 0;
                cx.notify();
            })),
        );
    }
    let tr_p = crate::ui::tr(cx).providers();
    let pricing_pill_label = if prov.category == "local" && prov.auth == AuthStyle::None {
        tr_p.prov_pricing_local_free_badge().to_owned()
    } else {
        let base = view
            .pricing_of(&prov.id)
            .and_then(|pv| {
                pv.catalog_family_name
                    .as_deref()
                    .or(pv.catalog_family.as_deref())
            })
            .unwrap_or_else(|| tr_p.prov_pricing_models_pill_official());
        let summary = match &prov.quirks.relay_pricing {
            Some(rp) => match &rp.currency {
                Some(c) => format!("{base} · ×{} {c}", rp.multiplier),
                None => format!("{base} · ×{}", rp.multiplier),
            },
            None => base.to_owned(),
        };
        tr_p.prov_pricing_models_pill(summary)
    };

    let filters_row = h_row()
        .justify_between()
        .gap(px(8.0))
        .flex_wrap()
        .child(chips)
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(8.0))
                .child(
                    btn_ghost("pm-pricing-jump", pricing_pill_label)
                        .h(px(24.0))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.provider_tab = ProviderTab::Config;
                            cx.notify();
                        })),
                )
                .child(
                    div()
                        .flex_shrink_0()
                        .text_xs()
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(
                            tr_p.prov_detail_models_enabled_of_showing(
                                enabled_cnt,
                                total_cnt,
                                visible_cnt,
                            ),
                        )),
                ),
        );

    let mut top_controls = v_col()
        .flex_shrink_0()
        .px(px(20.0))
        .pt(px(16.0))
        .pb(px(10.0))
        .gap(px(10.0))
        .child(toolbar)
        .child(filters_row);

    // Inline manual model creation form (only opened when explicitly clicking "+ Add model" above).
    if view.adding_model_open {
        let pid_add = prov_id.clone();
        top_controls = top_controls.child(
            card(cx).p(px(10.0)).bg(to_hsla(p.pane)).child(
                h_row()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w(px(220.0))
                            .child(text_input(&view.new_model_id_input)),
                    )
                    .child(
                        div()
                            .w(px(120.0))
                            .child(text_input(&view.new_model_ctx_input)),
                    )
                    .child(
                        btn_primary(
                            "pm-add-save",
                            crate::ui::tr(cx)
                                .providers()
                                .prov_detail_models_add_model_2(),
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.add_manual_model(pid_add.clone(), window, cx);
                            },
                        )),
                    )
                    .child(
                        btn_icon("pm-add-close", UiIcon::Close).on_click(cx.listener(
                            |this, _, _, cx| {
                                this.adding_model_open = false;
                                cx.notify();
                            },
                        )),
                    ),
            ),
        );
    }

    let table_area = div()
        .flex_1()
        .min_h_0()
        .px(px(20.0))
        .pb(px(14.0))
        .child(render_models_table(
            view,
            &prov_id,
            &filtered,
            &latest_probe,
            total_cnt,
            cx,
        ));

    let mut root = v_col().size_full().child(top_controls).child(table_area);

    // Docked bottom bulk-selection bar: anchored below the scrolling table so toggling checkboxes
    // causes 0.0px layout shift on rows above and stays visible at any scroll position.
    if !view.selected_models.is_empty() {
        let sel_n = view.selected_models.len();
        let pid_en = prov_id.clone();
        let pid_dis = prov_id.clone();
        let pid_tst = prov_id;
        root = root.child(
            h_row()
                .flex_shrink_0()
                .justify_between()
                .gap(px(10.0))
                .px(px(20.0))
                .py(px(10.0))
                .bg(to_hsla(p.accent_soft))
                .border_t_1()
                .border_color(to_hsla(p.accent_line.composite_over(p.accent_soft)))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.accent_ink))
                        .child(SharedString::from(
                            crate::ui::tr(cx)
                                .providers()
                                .prov_detail_models_model_s_selected(sel_n),
                        )),
                )
                .child(
                    h_row()
                        .flex_shrink_0()
                        .gap(px(6.0))
                        .child(
                            btn_primary(
                                "pm-bulk-test",
                                crate::ui::tr(cx).providers().models_sel_test(),
                            )
                            .on_click(cx.listener(
                                move |this, _, window, cx| {
                                    let picked: Vec<String> =
                                        this.selected_models.iter().cloned().collect();
                                    this.open_bench(Some(pid_tst.clone()), picked, window, cx);
                                },
                            )),
                        )
                        .child(
                            btn_secondary(
                                "pm-bulk-en",
                                crate::ui::tr(cx).providers().prov_detail_models_enable(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.bulk_toggle_selected_models(pid_en.clone(), true, cx);
                                },
                            )),
                        )
                        .child(
                            btn_secondary(
                                "pm-bulk-dis",
                                crate::ui::tr(cx).providers().prov_detail_models_disable(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.bulk_toggle_selected_models(pid_dis.clone(), false, cx);
                                },
                            )),
                        )
                        .child(
                            btn_ghost(
                                "pm-bulk-clear",
                                crate::ui::tr(cx).providers().models_sel_clear(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                this.selected_models.clear();
                                cx.notify();
                            })),
                        ),
                ),
        );
    }

    // Docked bottom undo toast bar: zero layout shift when toggling individual model switches.
    if let Some((op_id, desc)) = &view.last_bulk_undo {
        let oid = op_id.clone();
        root = root.child(
            h_row()
                .flex_shrink_0()
                .justify_between()
                .gap(px(10.0))
                .px(px(20.0))
                .py(px(8.0))
                .bg(to_hsla(p.pane))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.ink))
                        .child(SharedString::from(desc.clone())),
                )
                .child(
                    h_row()
                        .flex_shrink_0()
                        .gap(px(5.0))
                        .child(
                            btn_secondary("pm-undo-bulk", crate::ui::tr(cx).common().undo())
                                .on_click(cx.listener(move |this, _, _, cx| {
                                    this.undo_bulk_models(oid.clone(), cx);
                                })),
                        )
                        .child(
                            btn_icon("pm-undo-close", UiIcon::Close).on_click(cx.listener(
                                |this, _, _, cx| {
                                    this.last_bulk_undo = None;
                                    cx.notify();
                                },
                            )),
                        ),
                ),
        );
    }

    root
}
