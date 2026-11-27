//! Titlebar Quick Theme & Code Colors popover (`tb-theme`).
//!
//! Lets users switch appearance mode, eye-care surface palette, theme accent color, and code
//! syntax highlighting theme in one click from anywhere in the workbench — with live single-frame
//! updates visible immediately behind the anchored popover.

use gpui_kit::{
    Context, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::model::settings::Theme as SettingsTheme;
use spanbay_desktop::state::SettingsSection;
use spanbay_desktop::theme::{
    AccentChoice, AccentPreset, PaletteId, SyntaxThemeId, resolve_palette_with_accent,
    resolve_syntax_palette,
};

use crate::ui::{
    Anchor, Popover, Tone, UiIcon, badge, choice_chip, h_row, icon_xs, menu_divider, menu_item,
    palette, popover, tab_bar_segmented, tab_item, text_tooltip, titlebar_icon_btn, to_hsla, tr,
    v_col,
};

use super::AppShell;

pub(super) fn render_theme_popover(shell: &AppShell, cx: &mut Context<AppShell>) -> Popover {
    let p = palette(cx);
    let sh = tr(cx).shell();
    let prefs = shell.state.theme_prefs();
    let active_mode = shell.state.settings.appearance.theme;
    let shell_ent = cx.entity().clone();

    let dot_color = (prefs.accent != AccentChoice::FollowPalette).then(|| to_hsla(p.accent));
    let trigger = titlebar_icon_btn(
        "tb-theme",
        if p.is_dark { UiIcon::Sun } else { UiIcon::Moon },
        dot_color,
        cx,
    )
    .tooltip(sh.titlebar_toggle_theme());

    popover("tb-theme-popover")
        .anchor(Anchor::TopRight)
        .trigger(trigger)
        .content(move |_state, _window, pop_cx| {
            let pp = palette(pop_cx);
            let is_dark = pp.is_dark;
            let sh = tr(pop_cx).shell();
            let tr_set = tr(pop_cx).settings();
            let mode_idx = match active_mode {
                SettingsTheme::System => 0,
                SettingsTheme::Light => 1,
                SettingsTheme::Dark => 2,
            };
            let mode_label = match active_mode {
                SettingsTheme::System => tr_set.appearance_system(),
                SettingsTheme::Light => tr_set.opt_appearance_theme_light(),
                SettingsTheme::Dark => tr_set.opt_appearance_theme_dark(),
            };

            let e_mode = shell_ent.clone();
            let mode_seg = tab_bar_segmented("tb-theme-mode-seg")
                .selected_index(mode_idx)
                .child(tab_item(tr_set.appearance_system()))
                .child(tab_item(tr_set.opt_appearance_theme_light()))
                .child(tab_item(tr_set.opt_appearance_theme_dark()))
                .on_click(move |idx, _, cx| {
                    let next = match *idx {
                        1 => SettingsTheme::Light,
                        2 => SettingsTheme::Dark,
                        _ => SettingsTheme::System,
                    };
                    e_mode.update(cx, |this, cx| this.set_theme_mode(next, cx));
                });

            // Compact 2×4 Eye-Care Surface Palettes grid
            let mut pal_grid = v_col().w_full().gap(px(5.0));
            for chunk in PaletteId::ALL.chunks(4) {
                let mut row = h_row().w_full().gap(px(5.0));
                for &pid in chunk {
                    let preview = resolve_palette_with_accent(pid, prefs.accent, is_dark, false);
                    let selected = prefs.palette == pid;
                    let e_pal = shell_ent.clone();
                    let label = palette_short_name(pid, pop_cx);
                    row = row.child(
                        div()
                            .id(SharedString::from(format!("tb-pal-{}", pid.id())))
                            .flex_1()
                            .px(px(6.0))
                            .py(px(5.0))
                            .rounded(px(6.0))
                            .bg(to_hsla(if selected { pp.accent_soft } else { pp.pane }))
                            .border_1()
                            .border_color(to_hsla(if selected {
                                pp.accent
                            } else {
                                pp.line_strong.composite_over(pp.pane)
                            }))
                            .cursor_pointer()
                            .on_click(move |_, _, cx| {
                                let mut next = prefs;
                                next.palette = pid;
                                e_pal.update(cx, |this, cx| this.set_theme_prefs(next, cx));
                            })
                            .child(
                                h_row()
                                    .gap(px(5.0))
                                    .child(
                                        h_row()
                                            .size(px(12.0))
                                            .rounded(px(999.0))
                                            .overflow_hidden()
                                            .flex_shrink_0()
                                            .border_1()
                                            .border_color(to_hsla(
                                                pp.line_strong.composite_over(pp.pane),
                                            ))
                                            .child(
                                                div()
                                                    .w(px(6.0))
                                                    .h_full()
                                                    .bg(to_hsla(preview.surface)),
                                            )
                                            .child(
                                                div()
                                                    .w(px(6.0))
                                                    .h_full()
                                                    .bg(to_hsla(preview.accent)),
                                            ),
                                    )
                                    .child(
                                        div()
                                            .text_size(px(10.5))
                                            .font_weight(if selected {
                                                gpui_kit::FontWeight::SEMIBOLD
                                            } else {
                                                gpui_kit::FontWeight::MEDIUM
                                            })
                                            .text_color(to_hsla(if selected {
                                                pp.accent_ink
                                            } else {
                                                pp.ink
                                            }))
                                            .truncate()
                                            .child(label),
                                    ),
                            ),
                    );
                }
                pal_grid = pal_grid.child(row);
            }

            // Compact Theme Accent swatch strip
            let e_follow = shell_ent.clone();
            let mut accent_row = h_row().flex_wrap().gap(px(6.0)).child(
                choice_chip(
                    "tb-acc-follow",
                    tr_set.appearance_accent_follow(),
                    prefs.accent == AccentChoice::FollowPalette,
                    pop_cx,
                )
                .on_click(move |_, _, cx| {
                    let mut next = prefs;
                    next.accent = AccentChoice::FollowPalette;
                    e_follow.update(cx, |this, cx| this.set_theme_prefs(next, cx));
                }),
            );
            for preset in AccentPreset::ALL {
                let selected = prefs.accent == AccentChoice::Preset(preset);
                let sw = preset.swatch_color(is_dark);
                let tip = accent_label(preset, pop_cx);
                let e_acc = shell_ent.clone();
                accent_row = accent_row.child(
                    div()
                        .id(SharedString::from(format!("tb-acc-{}", preset.id())))
                        .size(px(19.0))
                        .rounded(px(999.0))
                        .bg(to_hsla(sw))
                        .border_2()
                        .border_color(to_hsla(if selected { pp.ink } else { pp.overlay }))
                        .flex()
                        .items_center()
                        .justify_center()
                        .cursor_pointer()
                        .tooltip(text_tooltip(tip))
                        .on_click(move |_, _, cx| {
                            let mut next = prefs;
                            next.accent = AccentChoice::Preset(preset);
                            e_acc.update(cx, |this, cx| this.set_theme_prefs(next, cx));
                        })
                        .children(selected.then(|| {
                            div()
                                .text_color(to_hsla(pp.on_accent))
                                .child(icon_xs(UiIcon::Check))
                        })),
                );
            }

            // Compact 2×3 Code Syntax Theme cards
            let mut syn_grid = v_col().w_full().gap(px(5.0));
            for chunk in SyntaxThemeId::ALL.chunks(2) {
                let mut row = h_row().w_full().gap(px(5.0));
                for &st in chunk {
                    let syn = resolve_syntax_palette(st, &pp);
                    let selected = prefs.syntax_theme == st;
                    let e_syn = shell_ent.clone();
                    row = row.child(
                        div()
                            .id(SharedString::from(format!("tb-syn-{}", st.id())))
                            .flex_1()
                            .px(px(7.0))
                            .py(px(5.0))
                            .rounded(px(6.0))
                            .bg(to_hsla(if selected { pp.accent_soft } else { pp.pane }))
                            .border_1()
                            .border_color(to_hsla(if selected {
                                pp.accent
                            } else {
                                pp.line_strong.composite_over(pp.pane)
                            }))
                            .cursor_pointer()
                            .on_click(move |_, _, cx| {
                                let mut next = prefs;
                                next.syntax_theme = st;
                                e_syn.update(cx, |this, cx| this.set_theme_prefs(next, cx));
                            })
                            .child(
                                h_row()
                                    .justify_between()
                                    .gap(px(4.0))
                                    .child(
                                        div()
                                            .text_size(px(11.0))
                                            .font_weight(if selected {
                                                gpui_kit::FontWeight::SEMIBOLD
                                            } else {
                                                gpui_kit::FontWeight::MEDIUM
                                            })
                                            .text_color(to_hsla(if selected {
                                                pp.accent_ink
                                            } else {
                                                pp.ink
                                            }))
                                            .truncate()
                                            .child(syntax_label(st, pop_cx)),
                                    )
                                    .child(
                                        h_row()
                                            .gap(px(2.5))
                                            .flex_shrink_0()
                                            .child(
                                                div()
                                                    .size(px(6.0))
                                                    .rounded(px(999.0))
                                                    .bg(to_hsla(syn.key)),
                                            )
                                            .child(
                                                div()
                                                    .size(px(6.0))
                                                    .rounded(px(999.0))
                                                    .bg(to_hsla(syn.string)),
                                            )
                                            .child(
                                                div()
                                                    .size(px(6.0))
                                                    .rounded(px(999.0))
                                                    .bg(to_hsla(syn.number)),
                                            ),
                                    ),
                            ),
                    );
                }
                syn_grid = syn_grid.child(row);
            }

            let e_open = shell_ent.clone();
            let pop_handle = pop_cx.entity().clone();
            let sec_lbl = |text: &'static str| {
                div()
                    .text_size(px(10.5))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(to_hsla(pp.muted))
                    .child(text)
            };

            v_col()
                .w(px(312.0))
                .gap(px(8.0))
                .child(
                    h_row()
                        .w_full()
                        .justify_between()
                        .pb(px(4.0))
                        .border_b_1()
                        .border_color(to_hsla(pp.line.composite_over(pp.overlay)))
                        .child(
                            div()
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(pp.ink))
                                .child(sh.titlebar_theme_popover_title()),
                        )
                        .child(badge(mode_label, Tone::Accent, pop_cx)),
                )
                .child(
                    h_row()
                        .w_full()
                        .justify_between()
                        .gap(px(8.0))
                        .child(sec_lbl(sh.titlebar_theme_mode()))
                        .child(mode_seg),
                )
                .child(
                    v_col()
                        .w_full()
                        .gap(px(4.0))
                        .child(sec_lbl(sh.titlebar_theme_palette()))
                        .child(pal_grid),
                )
                .child(
                    v_col()
                        .w_full()
                        .gap(px(5.0))
                        .child(sec_lbl(sh.titlebar_theme_accent()))
                        .child(accent_row),
                )
                .child(
                    v_col()
                        .w_full()
                        .gap(px(4.0))
                        .child(sec_lbl(sh.titlebar_theme_syntax()))
                        .child(syn_grid),
                )
                .child(menu_divider(pop_cx))
                .child(menu_item(
                    "tb-theme-open-settings",
                    UiIcon::Settings,
                    sh.titlebar_theme_open_settings(),
                    false,
                    pop_cx,
                    move |_, window, cx| {
                        e_open.update(cx, |this, cx| {
                            this.open_settings_section(SettingsSection::Appearance, cx);
                        });
                        pop_handle.update(cx, |s, cx| s.dismiss(window, cx));
                    },
                ))
        })
}

fn palette_short_name(pid: PaletteId, cx: &gpui_kit::App) -> &'static str {
    let s = tr(cx).settings();
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

fn accent_label(preset: AccentPreset, cx: &gpui_kit::App) -> &'static str {
    let s = tr(cx).settings();
    match preset {
        AccentPreset::Indigo => s.appearance_accent_indigo(),
        AccentPreset::Ocean => s.appearance_accent_ocean(),
        AccentPreset::Teal => s.appearance_accent_teal(),
        AccentPreset::Emerald => s.appearance_accent_emerald(),
        AccentPreset::Amber => s.appearance_accent_amber(),
        AccentPreset::Coral => s.appearance_accent_coral(),
        AccentPreset::Rose => s.appearance_accent_rose(),
        AccentPreset::Violet => s.appearance_accent_violet(),
    }
}

fn syntax_label(id: SyntaxThemeId, cx: &gpui_kit::App) -> &'static str {
    let s = tr(cx).settings();
    match id {
        SyntaxThemeId::Catppuccin => s.appearance_syntax_catppuccin_name(),
        SyntaxThemeId::RosePine => s.appearance_syntax_rose_pine_name(),
        SyntaxThemeId::TokyoNight => s.appearance_syntax_tokyo_night_name(),
        SyntaxThemeId::GitHub => s.appearance_syntax_github_name(),
        SyntaxThemeId::OnePro => s.appearance_syntax_one_pro_name(),
        SyntaxThemeId::FollowPalette => s.appearance_syntax_follow_name(),
    }
}
