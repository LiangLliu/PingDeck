//! Step 1 (Required Provider Name, Required Base URL + Quick Presets, API Key with reveal
//! toggle, and Collapsible Advanced Options) for `SniffProviderSheet`.

use gpui_kit::{
    Context, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::app::llm_url_normalize_impl;

use crate::ui::{
    Anchor, ScrollableElement as _, UiIcon, choice_chip, field_row, h_row, icon_xs, menu_option,
    meta_pill, palette, popover, secret_input, select_pill, settings_sub_well, text_input, to_hsla,
    toggle_switch, tr, v_col,
};

use super::sniff_sheet::SniffProviderSheet;

const QUICK_CHIPS: [&str; 8] = [
    "deepseek",
    "moonshot",
    "siliconflow",
    "openrouter",
    "openai",
    "anthropic",
    "google_gemini",
    "ollama",
];

pub(super) fn render_step_one(
    sheet: &SniffProviderSheet,
    cx: &mut Context<SniffProviderSheet>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let pr = tr(cx).providers();

    let mut quick_row = h_row().flex_wrap().gap(px(5.0));
    for pid in QUICK_CHIPS {
        if let Some(pr_item) = sheet.presets.iter().find(|x| x.id == pid) {
            let id = pr_item.id.clone();
            let active = sheet.selected_preset_id.as_deref() == Some(pid);
            quick_row = quick_row.child(
                choice_chip(
                    SharedString::from(format!("sn-chip-{pid}")),
                    pr_item.name.clone(),
                    active,
                    cx,
                )
                .h(px(24.0))
                .px(px(8.0))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.pick_preset(id.clone(), window, cx);
                })),
            );
        }
    }

    let ent_all = cx.entity().clone();
    let all_presets = sheet.presets.clone();
    let active_pid = sheet.selected_preset_id.clone();
    let more_pop = popover("sn-more-presets")
        .p(px(6.0))
        .rounded(px(10.0))
        .anchor(Anchor::TopLeft)
        .trigger(
            select_pill("sn-more-trig", pr.sniff_picker_label(), false, cx)
                .h(px(24.0))
                .px(px(8.0)),
        )
        .content(move |_, _, pop_cx| {
            let pp = palette(pop_cx);
            let list_h = ((all_presets.len() as f32) * 30.0 + 8.0).clamp(80.0, 280.0);
            let mut col = v_col().w(px(244.0)).gap(px(2.0));
            for (i, pr_item) in all_presets.iter().enumerate() {
                let id = pr_item.id.clone();
                let sel = active_pid.as_deref() == Some(id.as_str());
                let ent = ent_all.clone();
                let pop_h = pop_cx.entity().clone();
                col = col.child(
                    menu_option(
                        SharedString::from(format!("sn-all-{i}")),
                        sel,
                        pop_cx,
                        move |_, window, cx| {
                            ent.update(cx, |this, cx| {
                                this.pick_preset(id.clone(), window, cx);
                            });
                            pop_h.update(cx, |s, cx| s.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(
                        div()
                            .text_xs()
                            .truncate()
                            .child(SharedString::from(pr_item.name.clone())),
                    )
                    .children(sel.then(|| {
                        div()
                            .flex_shrink_0()
                            .text_color(to_hsla(pp.accent))
                            .child(icon_xs(UiIcon::Check))
                    })),
                );
            }
            div()
                .id("sn-all-presets-scroll")
                .w(px(244.0))
                .max_h(px(list_h))
                .overflow_y_scrollbar()
                .child(col)
        });
    quick_row = quick_row.child(more_pop);

    let region_row = sheet
        .selected_preset_id
        .as_deref()
        .and_then(|pid| sheet.presets.iter().find(|item| item.id == pid))
        .filter(|item| item.regions.len() > 1)
        .map(|item| {
            let mut rr = h_row().flex_wrap().gap(px(5.0)).items_center().child(
                div()
                    .text_size(px(11.0))
                    .text_color(to_hsla(p.muted))
                    .child(pr.prov_sniff_sheet_region()),
            );
            for reg in &item.regions {
                let rid = reg.id.clone();
                let url = reg.base_url.clone();
                let active = sheet.selected_region_id.as_deref() == Some(rid.as_str());
                rr = rr.child(
                    choice_chip(
                        SharedString::from(format!("sn-reg-{rid}")),
                        reg.label.clone(),
                        active,
                        cx,
                    )
                    .h(px(22.0))
                    .px(px(8.0))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.selected_region_id = Some(rid.clone());
                        this.url_input
                            .update(cx, |i, cx| i.set_value(url.clone(), window, cx));
                        this.normalized = llm_url_normalize_impl(&url).ok();
                        cx.notify();
                    })),
                );
            }
            rr
        });

    let adv_toggle = h_row()
        .id("sn-adv-tog")
        .gap(px(5.0))
        .py(px(2.0))
        .cursor_pointer()
        .text_color(to_hsla(p.muted))
        .hover(|s| s.text_color(to_hsla(p.ink)))
        .on_click(cx.listener(|this, _, _, cx| {
            this.adv_open = !this.adv_open;
            cx.notify();
        }))
        .child(icon_xs(if sheet.adv_open {
            UiIcon::ChevronDown
        } else {
            UiIcon::ChevronRight
        }))
        .child(
            div()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(pr.prov_sniff_sheet_advanced()),
        );

    let adv_panel = sheet.adv_open.then(|| {
        settings_sub_well(cx).child(
            v_col()
                .w_full()
                .mb(px(-1.0))
                .child(field_row(
                    pr.prov_sniff_sheet_extended_scan_20_req_budget(),
                    None,
                    toggle_switch("sn-adv-ext", sheet.extended_scan).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.extended_scan = !this.extended_scan;
                            cx.notify();
                        },
                    )),
                    cx,
                ))
                .child(field_row(
                    pr.prov_sniff_sheet_verify_with_real_requests(),
                    None,
                    toggle_switch("sn-adv-bill", sheet.allow_billable).on_click(cx.listener(
                        |this, _, _, cx| {
                            this.allow_billable = !this.allow_billable;
                            cx.notify();
                        },
                    )),
                    cx,
                ))
                .child(field_row(
                    pr.prov_sniff_sheet_models_list_path(),
                    None,
                    div().w(px(220.0)).child(text_input(&sheet.adv_models_url)),
                    cx,
                ))
                .child(field_row(
                    pr.prov_sniff_sheet_proxy(),
                    None,
                    div().w(px(220.0)).child(text_input(&sheet.adv_proxy_url)),
                    cx,
                )),
        )
    });

    v_col()
        .w_full()
        .gap(px(14.0))
        // 1. Required first field: Provider Name
        .child(
            v_col()
                .w_full()
                .gap(px(5.0))
                .child(
                    h_row()
                        .w_full()
                        .justify_between()
                        .gap(px(8.0))
                        .child(
                            h_row()
                                .gap(px(3.0))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                        .text_color(to_hsla(p.ink_2))
                                        .child(pr.prov_sniff_sheet_provider_name_custom_optional()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(to_hsla(p.bad))
                                        .child("*"),
                                ),
                        )
                        .child(
                            div()
                                .text_size(px(10.5))
                                .text_color(to_hsla(p.faint))
                                .truncate()
                                .child(pr.prov_sniff_sheet_used_as_the_provider_model()),
                        ),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .child(text_input(&sheet.name_input)),
                ),
        )
        // 2. Required field: Base URL + inline Origin pill + Quick Presets
        .child(
            v_col()
                .w_full()
                .gap(px(6.0))
                .child(
                    h_row()
                        .w_full()
                        .justify_between()
                        .gap(px(8.0))
                        .child(
                            h_row()
                                .gap(px(3.0))
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                        .text_color(to_hsla(p.ink_2))
                                        .child(pr.prov_sniff_sheet_base_url()),
                                )
                                .child(
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                        .text_color(to_hsla(p.bad))
                                        .child("*"),
                                ),
                        )
                        .children(
                            sheet
                                .normalized
                                .as_ref()
                                .map(|n| meta_pill(format!("Origin: {}", n.origin), cx)),
                        ),
                )
                .child(div().w_full().min_w_0().child(text_input(&sheet.url_input)))
                .child(
                    v_col()
                        .w_full()
                        .gap(px(5.0))
                        .pt(px(2.0))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.faint))
                                .child(pr.prov_sniff_sheet_quick_presets()),
                        )
                        .child(quick_row)
                        .children(region_row),
                ),
        )
        // 3. API Key with built-in reveal toggle
        .child(
            v_col()
                .w_full()
                .gap(px(5.0))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink_2))
                        .child(pr.prov_sniff_sheet_api_key_stored_locally()),
                )
                .child(
                    div()
                        .w_full()
                        .min_w_0()
                        .child(secret_input(&sheet.key_input)),
                ),
        )
        // 4. Left-aligned collapsible Advanced Options
        .child(
            v_col()
                .w_full()
                .gap(px(8.0))
                .child(adv_toggle)
                .children(adv_panel),
        )
        // 5. Probe transparency caption
        .child(
            div()
                .text_size(px(11.0))
                .text_color(to_hsla(p.muted))
                .child(pr.prov_sniff_sheet_probe_hint()),
        )
}
