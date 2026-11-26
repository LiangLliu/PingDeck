//! Left master sidebar pane for `ProvidersView`:
//! pinned Local Gateway & Test Bench cards, live search, provider status rows, and footer health summary.

use gpui_kit::{Context, IntoElement as _, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::llm::status::ProviderStatus;

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, badge, btn_ghost, btn_icon, btn_primary,
    card, h_row, icon_xs, list_row, palette, pane_footer, pane_header, search_input,
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
    let gw_running = view.overview.as_ref().is_some_and(|o| o.status.running);
    let gw_port = view
        .overview
        .as_ref()
        .map_or(view.gateway_settings.port, |o| o.status.port);
    let active_clients = view
        .gateway_clients
        .iter()
        .filter(|c| c.is_active())
        .count();

    let query = view.search_input.read(cx).value().trim().to_lowercase();

    let mut left_list = v_col()
        .flex_1()
        .min_h_0()
        .gap(px(3.0))
        .p(px(8.0))
        .overflow_y_scrollbar();

    // Pinned Local Gateway hero item.
    let gw_sub = if gw_running {
        crate::ui::tr(cx)
            .providers()
            .prov_sidebar_127_0_0_1_clients(gw_port, active_clients)
    } else {
        crate::ui::tr(cx)
            .providers()
            .prov_sidebar_stopped_clients(active_clients)
    };

    left_list = left_list.child(
        list_row(
            "prov-nav-gateway",
            view.selection == LeftSelection::Gateway,
            cx,
            cx.listener(|this, _, _, cx| {
                this.selection = LeftSelection::Gateway;
                cx.notify();
            }),
        )
        .justify_between()
        .py(px(7.0))
        .child(
            h_row()
                .gap(px(8.0))
                .min_w_0()
                .flex_1()
                .child(status_indicator(
                    if gw_running {
                        StatusShape::Live
                    } else {
                        StatusShape::Idle
                    },
                    cx,
                ))
                .child(
                    v_col()
                        .min_w_0()
                        .gap(px(1.0))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(crate::ui::tr(cx).providers().prov_gateway_local_gateway()),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(SharedString::from(gw_sub)),
                        ),
                ),
        )
        .child(badge(
            if gw_running { "ON" } else { "OFF" },
            if gw_running {
                Tone::Success
            } else {
                Tone::Neutral
            },
            cx,
        )),
    );

    // Pinned Model Test Bench item.
    let has_providers = !view.providers.is_empty();
    left_list = left_list.child(
        list_row(
            "prov-nav-bench",
            view.selection == LeftSelection::Bench,
            cx,
            cx.listener(|this, _, window, cx| {
                this.open_bench(None, Vec::new(), window, cx);
            }),
        )
        .justify_between()
        .py(px(7.0))
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
                                .text_color(to_hsla(if has_providers { p.ink } else { p.muted }))
                                .truncate()
                                .child(crate::ui::tr(cx).providers().prov_sidebar_test_bench()),
                        )
                        .child(
                            div()
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(
                                    crate::ui::tr(cx)
                                        .providers()
                                        .prov_sidebar_model_probe_matrix(),
                                ),
                        ),
                ),
        ),
    );

    // Configured providers section header.
    let filtered: Vec<_> = view
        .providers
        .iter()
        .filter(|pv| {
            if query.is_empty() {
                return true;
            }
            let pr = &pv.provider;
            pr.name.to_lowercase().contains(&query)
                || pr.base_url.to_lowercase().contains(&query)
                || pr.category.to_lowercase().contains(&query)
        })
        .collect();

    left_list = left_list.child(
        div()
            .px(px(8.0))
            .pt(px(10.0))
            .pb(px(3.0))
            .child(section_heading(
                crate::ui::tr(cx).providers().prov_sidebar_providers(),
                cx,
            )),
    );

    if view.providers.is_empty() {
        left_list = left_list.child(render_empty_onboarding(view, cx));
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
                            crate::ui::tr(cx)
                                .providers()
                                .prov_sidebar_no_providers_match(query),
                        )),
                )
                .child(
                    btn_ghost(
                        "prov-clear-search",
                        crate::ui::tr(cx).providers().prov_sidebar_clear(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.search_input
                            .update(cx, |i, cx| i.set_value("", window, cx));
                        cx.notify();
                    })),
                ),
        );
    } else {
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
                parts.push(
                    crate::ui::tr(cx)
                        .providers()
                        .prov_sidebar_models(enabled_models, total_models),
                );
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
                                            badge(
                                                crate::ui::tr(cx).providers().prov_sidebar_review(),
                                                Tone::Warning,
                                                cx,
                                            )
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
        foot_parts.push(crate::ui::tr(cx).providers().prov_sidebar_ok(ok_n));
    }
    if warn_n > 0 {
        foot_parts.push(crate::ui::tr(cx).providers().prov_sidebar_warn(warn_n));
    }
    if bad_n > 0 {
        foot_parts.push(crate::ui::tr(cx).providers().prov_sidebar_down(bad_n));
    }
    if unv_n > 0 {
        foot_parts.push(crate::ui::tr(cx).providers().prov_sidebar_unverified(unv_n));
    }
    let foot_text = if foot_parts.is_empty() {
        crate::ui::tr(cx)
            .providers()
            .prov_sidebar_click_add_to_configure_a()
            .into()
    } else {
        foot_parts.join(" · ")
    };

    let header = pane_header(
        crate::ui::tr(cx).providers().prov_title(),
        Some(
            btn_primary(
                "prov-add-open",
                crate::ui::tr(cx).providers().prov_first_go(),
            )
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
        .child(footer)
}

fn render_empty_onboarding(view: &ProvidersView, cx: &mut Context<ProvidersView>) -> gpui_kit::Div {
    let p = palette(cx);
    let mut chips = h_row().flex_wrap().gap(px(4.0));
    for pid in QUICK_PRESET_IDS {
        if let Some(preset) = view.presets.iter().find(|pr| pr.id == pid) {
            let preset_id = preset.id.clone();
            let name = preset.name.clone();
            chips = chips.child(
                btn_ghost(SharedString::from(format!("quick-preset-{pid}")), name)
                    .px(px(7.0))
                    .border_1()
                    .border_color(to_hsla(p.line_strong.composite_over(p.surface)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.open_sniff_sheet(None, None, Some(preset_id.clone()), window, cx);
                    })),
            );
        }
    }

    card(cx).p(px(10.0)).child(
        v_col()
            .gap(px(8.0))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(to_hsla(p.ink))
                    .child(crate::ui::tr(cx).providers().prov_state_empty_title()),
            )
            .child(
                div()
                    .text_size(px(11.0))
                    .text_color(to_hsla(p.muted))
                    .child(
                        crate::ui::tr(cx)
                            .providers()
                            .prov_sidebar_paste_any_base_url_to(),
                    ),
            )
            .child(
                h_row()
                    .gap(px(5.0))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .child(text_input(&view.quick_url_input)),
                    )
                    .child(
                        btn_primary(
                            "prov-quick-go",
                            crate::ui::tr(cx).providers().prov_sidebar_go(),
                        )
                        .on_click(cx.listener(|this, _, window, cx| {
                            let url = this.quick_url_input.read(cx).value().trim().to_owned();
                            let url_opt = (!url.is_empty()).then_some(url);
                            this.open_sniff_sheet(None, url_opt, None, window, cx);
                        })),
                    ),
            )
            .child(
                div()
                    .text_size(px(10.5))
                    .text_color(to_hsla(p.faint))
                    .child(crate::ui::tr(cx).providers().prov_state_empty_presets()),
            )
            .child(chips),
    )
}
