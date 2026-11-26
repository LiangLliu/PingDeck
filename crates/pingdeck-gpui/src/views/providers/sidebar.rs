//! Left master sidebar pane & full-width onboarding hero for `ProvidersView`:
//! live-searchable configured Providers list, dedicated Model Test Bench entry,
//! and footer health summary.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::llm::status::ProviderStatus;

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_ghost, btn_icon, btn_primary, card,
    empty_inline, h_row, icon_xs, list_row, palette, pane_footer, pane_header, search_input,
    section_heading, status_indicator, text_input, to_hsla, v_col,
};

use super::format::{format_reason, provider_best_balance, provider_status_shape};
use super::{LeftSelection, ProvidersView};

const QUICK_PRESET_IDS: [&str; 8] = [
    "deepseek",
    "anthropic",
    "openai",
    "moonshot",
    "siliconflow",
    "google_gemini",
    "openrouter",
    "ollama",
];

pub fn render_sidebar(view: &ProvidersView, cx: &mut Context<ProvidersView>) -> gpui_kit::Div {
    let p = palette(cx);
    let pr = crate::ui::tr(cx).providers();
    let query = view.search_input.read(cx).value().trim().to_lowercase();

    let mut left_list = v_col()
        .flex_1()
        .min_h_0()
        .gap(px(3.0))
        .p(px(8.0))
        .overflow_y_scrollbar();

    let filtered: Vec<_> = view
        .providers
        .iter()
        .filter(|pv| {
            if query.is_empty() {
                return true;
            }
            let pr_rec = &pv.provider;
            pr_rec.name.to_lowercase().contains(&query)
                || pr_rec.base_url.to_lowercase().contains(&query)
                || pr_rec.category.to_lowercase().contains(&query)
        })
        .collect();

    if view.providers.is_empty() {
        left_list = left_list.child(empty_inline(
            pr.prov_state_empty_title(),
            Some(SharedString::from(
                pr.prov_sidebar_click_add_to_configure_a(),
            )),
            None,
            cx,
        ));
    } else if filtered.is_empty() {
        left_list = left_list.child(
            v_col()
                .items_center()
                .gap(px(6.0))
                .py(px(18.0))
                .px(px(10.0))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(
                            pr.prov_sidebar_no_providers_match(query),
                        )),
                )
                .child(
                    btn_ghost("prov-clear-search", pr.prov_sidebar_clear()).on_click(cx.listener(
                        |this, _, window, cx| {
                            this.search_input
                                .update(cx, |i, cx| i.set_value("", window, cx));
                            cx.notify();
                        },
                    )),
                ),
        );
    } else {
        left_list = left_list.child(
            div()
                .px(px(8.0))
                .pt(px(2.0))
                .pb(px(3.0))
                .child(section_heading(pr.prov_sidebar_providers(), cx)),
        );
        for pv in filtered {
            let prov = &pv.provider;
            let id = prov.id.clone();
            let sel = matches!(&view.selection, LeftSelection::Provider(s) if s == &prov.id);
            let status_dto = view.statuses.get(&prov.id);
            let st = status_dto.map_or(
                if prov.enabled {
                    ProviderStatus::Unverified
                } else {
                    ProviderStatus::Disabled
                },
                |s| s.status,
            );
            let prov_keys: Vec<_> = view
                .keys
                .iter()
                .filter(|k| k.provider_id == prov.id)
                .cloned()
                .collect();
            let enabled_models = view
                .models
                .iter()
                .filter(|m| m.model.provider_id == prov.id && m.model.enabled)
                .count();
            let total_models = view
                .models
                .iter()
                .filter(|m| m.model.provider_id == prov.id)
                .count();

            let mut parts: Vec<String> = Vec::new();
            if let Some(bal) =
                provider_best_balance(&prov_keys, &prov.category, crate::ui::active_language(cx))
            {
                parts.push(bal);
            }
            if let Some(reason) = status_dto.and_then(|s| s.reasons.first()) {
                parts.push(format_reason(reason, crate::ui::active_language(cx)));
            } else if let Some(ms) = status_dto.and_then(|s| s.latency_ms) {
                parts.push(format!("{ms} ms"));
            } else if total_models > 0 {
                parts.push(pr.prov_sidebar_models(enabled_models, total_models));
            } else {
                parts.push(prov.category.clone());
            }
            let subline = parts.join(" · ");
            let unreviewed = prov.review_pending_since_ms.is_some();

            left_list = left_list.child(
                list_row(
                    SharedString::from(format!("prov-row-{id}")),
                    sel,
                    cx,
                    cx.listener(move |this, _, window, cx| {
                        this.select_provider(id.clone(), window, cx);
                    }),
                )
                .justify_between()
                .gap(px(6.0))
                .py(px(6.0))
                .child(
                    h_row()
                        .gap(px(8.0))
                        .flex_1()
                        .min_w_0()
                        .child(status_indicator(provider_status_shape(st), cx))
                        .child(
                            v_col()
                                .flex_1()
                                .min_w_0()
                                .gap(px(1.0))
                                .child(
                                    h_row()
                                        .gap(px(5.0))
                                        .min_w_0()
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                                .text_color(to_hsla(if prov.enabled {
                                                    p.ink
                                                } else {
                                                    p.muted
                                                }))
                                                .truncate()
                                                .child(SharedString::from(prov.name.clone())),
                                        )
                                        .children(unreviewed.then(|| {
                                            badge(pr.prov_sidebar_review(), Tone::Warning, cx)
                                        })),
                                )
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(to_hsla(p.muted))
                                        .truncate()
                                        .child(SharedString::from(subline)),
                                ),
                        ),
                ),
            );
        }
    }

    // Pinned Model Test Bench tool entry right above the footer (provider-first list above).
    let has_providers = !view.providers.is_empty();
    let bench_slot = div()
        .flex_shrink_0()
        .px(px(8.0))
        .py(px(6.0))
        .border_t_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            list_row(
                "prov-nav-bench",
                view.selection == LeftSelection::Bench,
                cx,
                cx.listener(|this, _, window, cx| {
                    this.open_bench(None, Vec::new(), window, cx);
                }),
            )
            .justify_between()
            .py(px(6.0))
            .child(
                h_row()
                    .gap(px(8.0))
                    .min_w_0()
                    .flex_1()
                    .child(
                        div()
                            .text_color(to_hsla(p.accent_ink))
                            .child(icon_xs(UiIcon::Activity)),
                    )
                    .child(
                        v_col()
                            .min_w_0()
                            .gap(px(1.0))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(to_hsla(if has_providers {
                                        p.ink
                                    } else {
                                        p.muted
                                    }))
                                    .truncate()
                                    .child(pr.prov_sidebar_test_bench()),
                            )
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .text_color(to_hsla(p.muted))
                                    .truncate()
                                    .child(pr.prov_sidebar_model_probe_matrix()),
                            ),
                    ),
            ),
        );

    // Footer status breakdown.
    let (mut ok_n, mut warn_n, mut bad_n, mut unv_n) = (0usize, 0usize, 0usize, 0usize);
    for pv in &view.providers {
        match view.statuses.get(&pv.provider.id).map(|s| s.status) {
            Some(ProviderStatus::Healthy) => ok_n += 1,
            Some(ProviderStatus::NeedsAttention) => warn_n += 1,
            Some(ProviderStatus::Unreachable) => bad_n += 1,
            Some(ProviderStatus::Disabled) => {}
            _ => unv_n += 1,
        }
    }
    let mut foot_parts = Vec::new();
    if ok_n > 0 {
        foot_parts.push(pr.prov_sidebar_ok(ok_n));
    }
    if warn_n > 0 {
        foot_parts.push(pr.prov_sidebar_warn(warn_n));
    }
    if bad_n > 0 {
        foot_parts.push(pr.prov_sidebar_down(bad_n));
    }
    if unv_n > 0 {
        foot_parts.push(pr.prov_sidebar_unverified(unv_n));
    }
    let foot_text = if foot_parts.is_empty() {
        pr.prov_sidebar_click_add_to_configure_a().into()
    } else {
        foot_parts.join(" · ")
    };

    let header = pane_header(
        pr.prov_title(),
        Some(
            btn_primary("prov-add-open", pr.prov_first_go())
                .icon(icon_xs(UiIcon::Plus))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.open_sniff_sheet(None, None, None, window, cx);
                }))
                .into_any_element(),
        ),
        Some(search_input(&view.search_input, cx).into_any_element()),
        cx,
    );

    let footer = pane_footer(
        div()
            .text_size(px(11.0))
            .text_color(to_hsla(p.faint))
            .truncate()
            .child(SharedString::from(foot_text)),
        Some(
            btn_icon("prov-sidebar-refresh", UiIcon::Refresh)
                .on_click(cx.listener(|this, _, _, cx| this.reload_all(cx)))
                .into_any_element(),
        ),
        cx,
    );

    v_col()
        .size_full()
        .bg(to_hsla(p.pane))
        .child(header)
        .child(left_list)
        .child(bench_slot)
        .child(footer)
}

/// Centered onboarding hero rendered in the main right workspace when zero providers exist yet.
pub fn render_onboarding_hero(
    view: &ProvidersView,
    cx: &mut Context<ProvidersView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let pr = crate::ui::tr(cx).providers();
    let mut chips = h_row().flex_wrap().gap(px(6.0));
    for pid in QUICK_PRESET_IDS {
        if let Some(preset) = view.presets.iter().find(|item| item.id == pid) {
            let preset_id = preset.id.clone();
            let name = preset.name.clone();
            chips = chips.child(
                btn_ghost(SharedString::from(format!("quick-preset-{pid}")), name)
                    .px(px(9.0))
                    .border_1()
                    .border_color(to_hsla(p.line_strong.composite_over(p.surface)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_sniff_sheet(None, None, Some(preset_id.clone()), window, cx);
                    })),
            );
        }
    }

    div()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .p(px(28.0))
        .child(
            card(cx).w(px(520.0)).max_w_full().p(px(22.0)).child(
                v_col()
                    .gap(px(12.0))
                    .child(
                        div()
                            .text_base()
                            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                            .text_color(to_hsla(p.ink))
                            .child(pr.prov_state_empty_title()),
                    )
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .child(pr.prov_sidebar_paste_any_base_url_to()),
                    )
                    .child(
                        h_row()
                            .gap(px(8.0))
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .child(text_input(&view.quick_url_input)),
                            )
                            .child(btn_primary("prov-quick-go", pr.prov_sidebar_go()).on_click(
                                cx.listener(|this, _, window, cx| {
                                    let url =
                                        this.quick_url_input.read(cx).value().trim().to_owned();
                                    let url_opt = (!url.is_empty()).then_some(url);
                                    this.open_sniff_sheet(None, url_opt, None, window, cx);
                                }),
                            )),
                    )
                    .child(
                        div()
                            .pt(px(4.0))
                            .text_xs()
                            .text_color(to_hsla(p.faint))
                            .child(pr.prov_state_empty_presets()),
                    )
                    .child(chips),
            ),
        )
}
