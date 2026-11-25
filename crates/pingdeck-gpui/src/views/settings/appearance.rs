//! Settings › Appearance page: color mode, density, UI & monospace font scale sliders,
//! reduced motion, sidebar presentation, and 8 miniature OKLCH eye-care palette preview cards.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::model::settings::{
    Density, ReduceMotion, SettingsSection as CoreSection, SidebarMode, Theme as SettingsTheme,
};
use pingdeck_desktop::theme::{PaletteId, resolve_palette};

use crate::ui::{
    Tone, UiIcon, badge, btn_ghost, h_row, icon_xs, palette, setting_row, settings_section,
    settings_section_padded, settings_sub_well, slider, tab_bar_segmented, tab_item, to_hsla,
    v_col,
};

use super::SettingsView;

pub fn render_appearance(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let app = &view.settings.appearance;
    let p = palette(cx);
    let is_dark = p.is_dark;

    let theme_idx = match app.theme {
        SettingsTheme::System => 0,
        SettingsTheme::Light => 1,
        SettingsTheme::Dark => 2,
    };
    let density_idx = match app.density {
        Density::Comfortable => 0,
        Density::Compact => 1,
    };
    let motion_idx = match app.reduce_motion {
        ReduceMotion::System => 0,
        ReduceMotion::On => 1,
        ReduceMotion::Off => 2,
    };
    let sidebar_idx = match app.sidebar {
        SidebarMode::Auto => 0,
        SidebarMode::Expanded => 1,
        SidebarMode::Rail => 2,
    };

    let ui_font_val = view.inputs.ui_font_slider.read(cx).value().end().round() as u8;
    let mono_font_val = view.inputs.mono_font_slider.read(cx).value().end().round() as u8;

    let ent_theme = cx.entity().clone();
    let ent_density = cx.entity().clone();
    let ent_motion = cx.entity().clone();
    let ent_sidebar = cx.entity().clone();

    v_col()
        .w_full()
        .gap(px(20.0))
        .child(settings_section(
            crate::ui::tr(cx).settings().appearance_theme_typography(),
            Some(
                btn_ghost(
                    "app-reset-sec",
                    crate::ui::tr(cx).settings().page_defaults_reset(),
                )
                .icon(icon_xs(UiIcon::RotateCcw))
                .on_click(cx.listener(|this, _, _, cx| {
                    this.reset_section(CoreSection::Appearance, cx);
                }))
                .into_any_element(),
            ),
            v_col()
                .w_full()
                .child(setting_row(
                    crate::ui::tr(cx).settings().appearance_color_mode(),
                    Some(SharedString::from(
                        crate::ui::tr(cx).settings().page_defaults_local_only(),
                    )),
                    Some(SharedString::from(
                        crate::ui::tr(cx)
                            .settings()
                            .appearance_follow_system_light_dark_appearance(),
                    )),
                    tab_bar_segmented("appearance-theme-seg")
                        .selected_index(theme_idx)
                        .child(tab_item(crate::ui::tr(cx).settings().appearance_system()))
                        .child(tab_item(
                            crate::ui::tr(cx).settings().opt_appearance_theme_light(),
                        ))
                        .child(tab_item(
                            crate::ui::tr(cx).settings().opt_appearance_theme_dark(),
                        ))
                        .on_click(move |idx, _, cx| {
                            let mode = match *idx {
                                1 => "light",
                                2 => "dark",
                                _ => "system",
                            };
                            ent_theme.update(cx, |this, cx| {
                                this.patch_section(
                                    CoreSection::Appearance,
                                    serde_json::json!({ "theme": mode }),
                                    cx,
                                );
                            });
                        }),
                    cx,
                ))
                .child(setting_row(
                    crate::ui::tr(cx).settings().appearance_density(),
                    None,
                    Some(SharedString::from(
                        crate::ui::tr(cx)
                            .settings()
                            .appearance_compact_tightens_row_padding_for(),
                    )),
                    tab_bar_segmented("appearance-density-seg")
                        .selected_index(density_idx)
                        .child(tab_item(
                            crate::ui::tr(cx)
                                .settings()
                                .opt_appearance_density_comfortable(),
                        ))
                        .child(tab_item(
                            crate::ui::tr(cx)
                                .settings()
                                .opt_appearance_density_compact(),
                        ))
                        .on_click(move |idx, _, cx| {
                            let density = if *idx == 1 { "compact" } else { "comfortable" };
                            ent_density.update(cx, |this, cx| {
                                this.patch_section(
                                    CoreSection::Appearance,
                                    serde_json::json!({ "density": density }),
                                    cx,
                                );
                            });
                        }),
                    cx,
                ))
                .child(setting_row(
                    crate::ui::tr(cx).settings().appearance_ui_font_size(),
                    Some(SharedString::from(
                        crate::ui::tr(cx).settings().page_defaults_local_only(),
                    )),
                    Some(SharedString::from(
                        crate::ui::tr(cx)
                            .settings()
                            .appearance_proportionally_scales_global_interface_typ(),
                    )),
                    h_row()
                        .gap(px(10.0))
                        .w(px(220.0))
                        .child(div().flex_1().child(slider(&view.inputs.ui_font_slider)))
                        .child(
                            div()
                                .w(px(42.0))
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .child(SharedString::from(format!("{ui_font_val} px"))),
                        ),
                    cx,
                ))
                .child(setting_row(
                    crate::ui::tr(cx)
                        .settings()
                        .appearance_monospace_font_size(),
                    Some(SharedString::from(
                        crate::ui::tr(cx).settings().page_defaults_local_only(),
                    )),
                    Some(SharedString::from(
                        crate::ui::tr(cx)
                            .settings()
                            .appearance_used_for_request_editors_response(),
                    )),
                    h_row()
                        .gap(px(10.0))
                        .w(px(220.0))
                        .child(div().flex_1().child(slider(&view.inputs.mono_font_slider)))
                        .child(
                            div()
                                .w(px(42.0))
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .child(SharedString::from(format!("{mono_font_val} px"))),
                        ),
                    cx,
                ))
                .child(render_live_preview(
                    ui_font_val,
                    mono_font_val,
                    app.density,
                    cx,
                ))
                .child(setting_row(
                    crate::ui::tr(cx).settings().appearance_sidebar_layout(),
                    None,
                    Some(SharedString::from(
                        crate::ui::tr(cx)
                            .settings()
                            .appearance_auto_remembers_your_b_toggle(),
                    )),
                    tab_bar_segmented("appearance-sidebar-seg")
                        .selected_index(sidebar_idx)
                        .child(tab_item(
                            crate::ui::tr(cx).settings().opt_network_http_version_auto(),
                        ))
                        .child(tab_item(crate::ui::tr(cx).settings().appearance_expanded()))
                        .child(tab_item(crate::ui::tr(cx).settings().appearance_rail()))
                        .on_click(move |idx, _, cx| {
                            let val = match *idx {
                                1 => "expanded",
                                2 => "rail",
                                _ => "auto",
                            };
                            ent_sidebar.update(cx, |this, cx| {
                                this.patch_section(
                                    CoreSection::Appearance,
                                    serde_json::json!({ "sidebar": val }),
                                    cx,
                                );
                            });
                        }),
                    cx,
                ))
                .child(setting_row(
                    crate::ui::tr(cx).settings().f_appearance_reduce_motion(),
                    None,
                    Some(SharedString::from(
                        crate::ui::tr(cx)
                            .settings()
                            .appearance_minimize_transitions_collapsible_animation(),
                    )),
                    tab_bar_segmented("appearance-motion-seg")
                        .selected_index(motion_idx)
                        .child(tab_item(crate::ui::tr(cx).settings().appearance_system()))
                        .child(tab_item(
                            crate::ui::tr(cx)
                                .settings()
                                .opt_appearance_reduce_motion_on(),
                        ))
                        .child(tab_item(crate::ui::tr(cx).settings().notify_off()))
                        .on_click(move |idx, _, cx| {
                            let val = match *idx {
                                1 => "on",
                                2 => "off",
                                _ => "system",
                            };
                            ent_motion.update(cx, |this, cx| {
                                this.patch_section(
                                    CoreSection::Appearance,
                                    serde_json::json!({ "reduce_motion": val }),
                                    cx,
                                );
                            });
                        }),
                    cx,
                )),
            cx,
        ))
        .child(settings_section_padded(
            crate::ui::tr(cx)
                .settings()
                .appearance_8_eye_care_oklch_palettes(),
            None,
            render_palette_grid(view, is_dark, cx),
            cx,
        ))
}

fn render_palette_grid(
    view: &SettingsView,
    is_dark: bool,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let mut swatches = v_col().w_full().gap(px(10.0));
    for chunk in PaletteId::ALL.chunks(4) {
        let mut row = h_row().w_full().gap(px(10.0));
        for &pid in chunk {
            let preview = resolve_palette(pid, is_dark, false);
            let selected = view.active_palette == pid;
            let border = if selected {
                to_hsla(preview.accent)
            } else {
                to_hsla(preview.line_strong.composite_over(preview.surface))
            };

            let mini_preview =
                h_row()
                    .w_full()
                    .h(px(64.0))
                    .rounded(px(6.0))
                    .overflow_hidden()
                    .bg(to_hsla(preview.canvas))
                    .p(px(4.0))
                    .gap(px(4.0))
                    .child(
                        v_col()
                            .w(px(26.0))
                            .h_full()
                            .gap(px(3.0))
                            .pt(px(3.0))
                            .child(
                                div()
                                    .w(px(16.0))
                                    .h(px(4.0))
                                    .rounded(px(2.0))
                                    .bg(to_hsla(preview.accent)),
                            )
                            .child(
                                div()
                                    .w_full()
                                    .h(px(6.0))
                                    .rounded(px(3.0))
                                    .bg(to_hsla(preview.selected)),
                            )
                            .child(
                                div()
                                    .w(px(18.0))
                                    .h(px(4.0))
                                    .rounded(px(2.0))
                                    .bg(to_hsla(preview.faint)),
                            )
                            .child(
                                div()
                                    .w(px(14.0))
                                    .h(px(4.0))
                                    .rounded(px(2.0))
                                    .bg(to_hsla(preview.faint)),
                            ),
                    )
                    .child(
                        h_row()
                            .flex_1()
                            .h_full()
                            .rounded(px(5.0))
                            .overflow_hidden()
                            .bg(to_hsla(preview.surface))
                            .border_1()
                            .border_color(to_hsla(preview.page_edge.composite_over(preview.canvas)))
                            .child(
                                v_col()
                                    .w(px(34.0))
                                    .h_full()
                                    .bg(to_hsla(preview.pane))
                                    .p(px(4.0))
                                    .gap(px(3.0))
                                    .child(
                                        div()
                                            .w_full()
                                            .h(px(6.0))
                                            .rounded(px(2.0))
                                            .bg(to_hsla(preview.selected_row)),
                                    )
                                    .child(div().w(px(22.0)).h(px(4.0)).rounded(px(2.0)).bg(
                                        to_hsla(preview.line_strong.composite_over(preview.pane)),
                                    )),
                            )
                            .child(
                                v_col()
                                    .flex_1()
                                    .h_full()
                                    .p(px(5.0))
                                    .justify_between()
                                    .child(
                                        v_col()
                                            .gap(px(3.0))
                                            .child(
                                                div()
                                                    .w(px(42.0))
                                                    .h(px(5.0))
                                                    .rounded(px(2.0))
                                                    .bg(to_hsla(preview.ink)),
                                            )
                                            .child(
                                                div()
                                                    .w(px(58.0))
                                                    .h(px(4.0))
                                                    .rounded(px(2.0))
                                                    .bg(to_hsla(preview.muted)),
                                            ),
                                    )
                                    .child(
                                        h_row()
                                            .justify_between()
                                            .child(
                                                div()
                                                    .w(px(18.0))
                                                    .h(px(6.0))
                                                    .rounded(px(3.0))
                                                    .bg(to_hsla(preview.accent_soft)),
                                            )
                                            .child(
                                                div()
                                                    .w(px(22.0))
                                                    .h(px(7.0))
                                                    .rounded(px(3.0))
                                                    .bg(to_hsla(preview.accent)),
                                            ),
                                    ),
                            ),
                    );

            row = row.child(
                div()
                    .id(SharedString::from(format!("pal-swatch-{}", pid.id())))
                    .flex_1()
                    .min_w(px(140.0))
                    .p(px(8.0))
                    .rounded(px(10.0))
                    .bg(to_hsla(if selected { p.accent_soft } else { p.pane }))
                    .border_2()
                    .border_color(border)
                    .cursor_pointer()
                    .hover(|s| s.border_color(to_hsla(preview.accent)))
                    .on_click(cx.listener(move |this, _, _, cx| this.choose_palette(pid, cx)))
                    .child(
                        v_col().gap(px(7.0)).child(mini_preview).child(
                            h_row()
                                .justify_between()
                                .px(px(2.0))
                                .child(
                                    v_col()
                                        .gap(px(1.0))
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                                .text_color(to_hsla(p.ink))
                                                .child(palette_name(pid, cx)),
                                        )
                                        .child(
                                            div()
                                                .text_size(px(10.0))
                                                .text_color(to_hsla(p.faint))
                                                .child(palette_subtitle(pid, cx)),
                                        ),
                                )
                                .child(if selected {
                                    div()
                                        .text_color(to_hsla(preview.accent))
                                        .child(icon_xs(UiIcon::Check))
                                } else {
                                    div()
                                        .size(px(10.0))
                                        .rounded(px(999.0))
                                        .bg(to_hsla(preview.accent))
                                }),
                        ),
                    ),
            );
        }
        swatches = swatches.child(row);
    }
    swatches
}

fn render_live_preview(
    ui_px: u8,
    mono_px: u8,
    density: Density,
    cx: &gpui_kit::App,
) -> gpui_kit::Div {
    let p = palette(cx);
    let pad = if density == Density::Compact {
        9.0
    } else {
        13.0
    };
    div()
        .py(px(12.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            settings_sub_well(cx).p(px(pad)).child(
                v_col()
                    .gap(px(if density == Density::Compact {
                        6.0
                    } else {
                        9.0
                    }))
                    .child(
                        h_row()
                            .justify_between()
                            .gap(px(8.0))
                            .child(
                                h_row()
                                    .gap(px(6.0))
                                    .child(badge("GET", Tone::Success, cx))
                                    .child(
                                        div()
                                            .text_size(px(f32::from(ui_px)))
                                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                                            .text_color(to_hsla(p.ink))
                                            .child("/v1/invoices?status=open"),
                                    ),
                            )
                            .child(
                                h_row()
                                    .gap(px(6.0))
                                    .child(badge("200 OK", Tone::Success, cx))
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .text_color(to_hsla(p.muted))
                                            .child(
                                                crate::ui::tr(cx)
                                                    .settings()
                                                    .appearance_148_ms_live_preview(),
                                            ),
                                    ),
                            ),
                    )
                    .child(
                        h_row()
                            .w_full()
                            .h(px(6.0))
                            .rounded(px(3.0))
                            .overflow_hidden()
                            .bg(to_hsla(p.surface))
                            .child(div().w(px(38.0)).h_full().bg(to_hsla(p.accent)))
                            .child(div().w(px(64.0)).h_full().bg(to_hsla(p.ok)))
                            .child(div().w(px(96.0)).h_full().bg(to_hsla(p.warn))),
                    )
                    .child(
                        div()
                            .px(px(10.0))
                            .py(px(7.0))
                            .rounded(px(6.0))
                            .bg(to_hsla(p.surface))
                            .border_1()
                            .border_color(to_hsla(p.line.composite_over(p.surface)))
                            .text_size(px(f32::from(mono_px)))
                            .text_color(to_hsla(p.ink_2))
                            .child(
                                "{\"id\":\"inv_9f2a\",\"status\":\"open\",\"total_cents\":4200}",
                            ),
                    ),
            ),
        )
}

fn palette_name(pid: PaletteId, cx: &gpui_kit::App) -> &'static str {
    let s = crate::ui::tr(cx).settings();
    match pid {
        PaletteId::Slate => s.palette_slate_name(),
        PaletteId::Ocean => s.palette_ocean_name(),
        PaletteId::Nord => s.palette_nord_name(),
        PaletteId::Sage => s.palette_sage_name(),
        PaletteId::Emerald => s.palette_emerald_name(),
        PaletteId::Amber => s.palette_amber_name(),
        PaletteId::Violet => s.palette_violet_name(),
        PaletteId::Graphite => s.palette_graphite_name(),
    }
}

fn palette_subtitle(pid: PaletteId, cx: &gpui_kit::App) -> &'static str {
    let s = crate::ui::tr(cx).settings();
    match pid {
        PaletteId::Slate => s.palette_slate_sub(),
        PaletteId::Ocean => s.palette_ocean_sub(),
        PaletteId::Nord => s.palette_nord_sub(),
        PaletteId::Sage => s.palette_sage_sub(),
        PaletteId::Emerald => s.palette_emerald_sub(),
        PaletteId::Amber => s.palette_amber_sub(),
        PaletteId::Violet => s.palette_violet_sub(),
        PaletteId::Graphite => s.palette_graphite_sub(),
    }
}
