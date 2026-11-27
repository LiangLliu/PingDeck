//! Band 1 of `Settings › Appearance`: unified Theme & Accent Studio.
//!
//! Combines three tightly related choices inside one cohesive studio without vertical stacking:
//! - Left top: 3 visual mini-window cards for System / Light / Dark appearance mode.
//! - Right top: Theme Accent Color picker (`Follow palette`, 8 OKLCH swatches, custom `#RRGGBB`).
//! - Bottom: 4×2 Eye-Care OKLCH surface palette canvas cards live-tinted with the active accent.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::model::settings::{
    SettingsSection as CoreSection, Theme as SettingsTheme,
};
use spanbay_desktop::theme::{
    AccentChoice, AccentPreset, format_hex_rgb, resolve_palette, resolve_palette_with_accent,
};

use crate::ui::{
    UiIcon, btn_ghost, btn_secondary, choice_chip, h_row, icon_xs, meta_pill, palette,
    settings_section_padded, settings_sub_well, text_input, text_tooltip, to_hsla, v_col,
};

use super::super::SettingsView;
use super::palette_grid::render_palette_grid;
use super::{accent_preset_name, palette_name};

pub(super) fn render_theme_studio(
    view: &SettingsView,
    is_dark: bool,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let tr_set = crate::ui::tr(cx).settings();

    let top_row = h_row()
        .w_full()
        .items_stretch()
        .gap(px(14.0))
        .child(render_mode_panel(view, cx))
        .child(render_accent_panel(view, is_dark, cx));

    let divider = div()
        .w_full()
        .h(px(1.0))
        .my(px(4.0))
        .bg(to_hsla(p.line.composite_over(p.surface)));

    let pal_header = h_row()
        .w_full()
        .justify_between()
        .gap(px(8.0))
        .child(
            h_row()
                .gap(px(6.0))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(tr_set.appearance_8_eye_care_oklch_palettes()),
                )
                .child(meta_pill(tr_set.page_defaults_local_only(), cx)),
        )
        .child(
            h_row()
                .gap(px(6.0))
                .child(div().size(px(8.0)).rounded(px(999.0)).bg(to_hsla(p.accent)))
                .child(
                    div()
                        .text_size(px(11.0))
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.muted))
                        .child(palette_name(view.theme_prefs.palette, cx)),
                ),
        );

    let body = v_col()
        .w_full()
        .gap(px(12.0))
        .child(top_row)
        .child(divider)
        .child(pal_header)
        .child(render_palette_grid(view, is_dark, cx));

    settings_section_padded(
        tr_set.appearance_sec_theme(),
        Some(
            btn_ghost("app-reset-sec", tr_set.page_defaults_reset())
                .icon(icon_xs(UiIcon::RotateCcw))
                .on_click(cx.listener(|this, _, window, cx| {
                    this.reset_appearance_all(window, cx);
                }))
                .into_any_element(),
        ),
        body,
        cx,
    )
}

fn render_mode_panel(view: &SettingsView, cx: &mut Context<SettingsView>) -> gpui_kit::Div {
    let p = palette(cx);
    let tr_set = crate::ui::tr(cx).settings();
    let active_mode = view.settings.appearance.theme;
    let light_p = resolve_palette_with_accent(
        view.theme_prefs.palette,
        view.theme_prefs.accent,
        false,
        false,
    );
    let dark_p = resolve_palette_with_accent(
        view.theme_prefs.palette,
        view.theme_prefs.accent,
        true,
        false,
    );

    let mode_card = |id: &'static str,
                     mode: SettingsTheme,
                     label: &'static str,
                     wire: &'static str,
                     preview: gpui_kit::Div,
                     cx: &mut Context<SettingsView>| {
        let selected = active_mode == mode;
        let border = if selected {
            to_hsla(p.accent)
        } else {
            to_hsla(p.line_strong.composite_over(p.sunken))
        };
        div()
            .id(id)
            .flex_1()
            .min_w(px(76.0))
            .p(px(6.0))
            .rounded(px(8.0))
            .bg(to_hsla(if selected { p.accent_soft } else { p.surface }))
            .border_2()
            .border_color(border)
            .cursor_pointer()
            .hover(|s| s.border_color(to_hsla(p.accent)))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.patch_section(
                    CoreSection::Appearance,
                    serde_json::json!({ "theme": wire }),
                    cx,
                );
            }))
            .child(
                v_col().gap(px(6.0)).child(preview).child(
                    h_row()
                        .justify_between()
                        .px(px(2.0))
                        .child(
                            div()
                                .text_size(px(11.0))
                                .font_weight(if selected {
                                    gpui_kit::FontWeight::SEMIBOLD
                                } else {
                                    gpui_kit::FontWeight::MEDIUM
                                })
                                .text_color(to_hsla(if selected { p.accent_ink } else { p.ink }))
                                .truncate()
                                .child(label),
                        )
                        .children(selected.then(|| {
                            div()
                                .text_color(to_hsla(p.accent))
                                .child(icon_xs(UiIcon::Check))
                        })),
                ),
            )
    };

    let mini_window = |pal: &spanbay_desktop::theme::ResolvedPalette| {
        h_row()
            .w_full()
            .h(px(40.0))
            .rounded(px(5.0))
            .overflow_hidden()
            .bg(to_hsla(pal.canvas))
            .p(px(3.0))
            .gap(px(3.0))
            .border_1()
            .border_color(to_hsla(pal.line.composite_over(pal.canvas)))
            .child(
                v_col()
                    .w(px(16.0))
                    .h_full()
                    .gap(px(3.0))
                    .pt(px(2.0))
                    .child(
                        div()
                            .w(px(10.0))
                            .h(px(4.0))
                            .rounded(px(2.0))
                            .bg(to_hsla(pal.accent)),
                    )
                    .child(
                        div()
                            .w(px(12.0))
                            .h(px(3.0))
                            .rounded(px(1.5))
                            .bg(to_hsla(pal.faint)),
                    ),
            )
            .child(
                v_col()
                    .flex_1()
                    .h_full()
                    .rounded(px(4.0))
                    .bg(to_hsla(pal.surface))
                    .p(px(4.0))
                    .justify_between()
                    .child(
                        div()
                            .w(px(26.0))
                            .h(px(4.0))
                            .rounded(px(2.0))
                            .bg(to_hsla(pal.ink)),
                    )
                    .child(
                        div()
                            .w(px(18.0))
                            .h(px(5.0))
                            .rounded(px(2.5))
                            .bg(to_hsla(pal.accent)),
                    ),
            )
    };

    let split_window = h_row()
        .w_full()
        .h(px(40.0))
        .rounded(px(5.0))
        .overflow_hidden()
        .border_1()
        .border_color(to_hsla(p.line_strong.composite_over(p.surface)))
        .child(
            v_col()
                .flex_1()
                .h_full()
                .bg(to_hsla(light_p.surface))
                .p(px(5.0))
                .justify_between()
                .child(
                    div()
                        .w(px(18.0))
                        .h(px(4.0))
                        .rounded(px(2.0))
                        .bg(to_hsla(light_p.ink)),
                )
                .child(
                    div()
                        .w(px(14.0))
                        .h(px(5.0))
                        .rounded(px(2.5))
                        .bg(to_hsla(light_p.accent)),
                ),
        )
        .child(
            v_col()
                .flex_1()
                .h_full()
                .bg(to_hsla(dark_p.surface))
                .p(px(5.0))
                .justify_between()
                .child(
                    div()
                        .w(px(18.0))
                        .h(px(4.0))
                        .rounded(px(2.0))
                        .bg(to_hsla(dark_p.ink)),
                )
                .child(
                    div()
                        .w(px(14.0))
                        .h(px(5.0))
                        .rounded(px(2.5))
                        .bg(to_hsla(dark_p.accent)),
                ),
        );

    settings_sub_well(cx)
        .flex_1()
        .min_w(px(260.0))
        .justify_between()
        .gap(px(10.0))
        .child(
            v_col()
                .gap(px(2.0))
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.ink))
                                .child(tr_set.appearance_color_mode()),
                        )
                        .child(meta_pill(tr_set.page_defaults_local_only(), cx)),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.muted))
                        .child(tr_set.appearance_follow_system_light_dark_appearance()),
                ),
        )
        .child(
            h_row()
                .w_full()
                .gap(px(8.0))
                .child(mode_card(
                    "app-mode-sys",
                    SettingsTheme::System,
                    tr_set.appearance_system(),
                    "system",
                    split_window,
                    cx,
                ))
                .child(mode_card(
                    "app-mode-light",
                    SettingsTheme::Light,
                    tr_set.opt_appearance_theme_light(),
                    "light",
                    mini_window(&light_p),
                    cx,
                ))
                .child(mode_card(
                    "app-mode-dark",
                    SettingsTheme::Dark,
                    tr_set.opt_appearance_theme_dark(),
                    "dark",
                    mini_window(&dark_p),
                    cx,
                )),
        )
}

fn render_accent_panel(
    view: &SettingsView,
    is_dark: bool,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let tr_set = crate::ui::tr(cx).settings();
    let tr_common = crate::ui::tr(cx).common();
    let active_accent = view.theme_prefs.accent;
    let base_pal = resolve_palette(view.theme_prefs.palette, is_dark, false);
    let active_hex = format_hex_rgb(p.accent);

    let follow_btn = choice_chip(
        "app-accent-follow",
        tr_set.appearance_accent_follow(),
        active_accent == AccentChoice::FollowPalette,
        cx,
    )
    .on_click(cx.listener(|this, _, window, cx| {
        this.choose_accent(AccentChoice::FollowPalette, window, cx);
    }));

    let mut swatches = h_row().flex_wrap().gap(px(7.0)).child(follow_btn);
    for preset in AccentPreset::ALL {
        let selected = active_accent == AccentChoice::Preset(preset);
        let sw_color = preset.swatch_color(is_dark);
        let tip = accent_preset_name(preset, cx);
        swatches = swatches.child(
            div()
                .id(SharedString::from(format!("app-accent-{}", preset.id())))
                .size(px(22.0))
                .rounded(px(999.0))
                .bg(to_hsla(sw_color))
                .border_2()
                .border_color(if selected {
                    to_hsla(p.ink)
                } else {
                    to_hsla(p.surface)
                })
                .flex()
                .items_center()
                .justify_center()
                .cursor_pointer()
                .tooltip(text_tooltip(tip))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.choose_accent(AccentChoice::Preset(preset), window, cx);
                }))
                .children(selected.then(|| {
                    div()
                        .text_color(to_hsla(p.on_accent))
                        .child(icon_xs(UiIcon::Check))
                })),
        );
    }

    let custom_swatch_bg = active_accent.preview_color(&base_pal);
    let custom_row = h_row()
        .w_full()
        .gap(px(6.0))
        .child(
            div()
                .text_size(px(11.0))
                .text_color(to_hsla(p.muted))
                .flex_shrink_0()
                .child(tr_set.appearance_accent_custom_label()),
        )
        .child(
            div()
                .size(px(18.0))
                .rounded(px(4.0))
                .flex_shrink_0()
                .bg(to_hsla(custom_swatch_bg))
                .border_1()
                .border_color(to_hsla(p.line_strong.composite_over(p.sunken))),
        )
        .child(
            div()
                .w(px(106.0))
                .child(text_input(&view.inputs.accent_hex)),
        )
        .child(
            btn_secondary("app-accent-apply", tr_common.apply()).on_click(cx.listener(
                |this, _, _, cx| {
                    this.apply_custom_accent_hex(cx);
                },
            )),
        )
        .child(
            div()
                .flex_1()
                .text_right()
                .text_size(px(10.5))
                .text_color(to_hsla(p.faint))
                .truncate()
                .child(SharedString::from(active_hex)),
        );

    settings_sub_well(cx)
        .flex_1()
        .min_w(px(280.0))
        .justify_between()
        .gap(px(10.0))
        .child(
            v_col()
                .gap(px(2.0))
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.ink))
                                .child(tr_set.appearance_accent_title()),
                        )
                        .child(meta_pill(tr_set.page_defaults_local_only(), cx)),
                )
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.muted))
                        .child(tr_set.appearance_accent_desc()),
                ),
        )
        .child(swatches)
        .child(custom_row)
}
