//! Settings › Appearance page: native segmented mode/density switches, live font-scale slider,
//! and miniature app-layout OKLCH palette preview cards.

use gpui_kit::{
    Context, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::model::settings::{
    Density, SettingsSection as CoreSection, Theme as SettingsTheme,
};
use pingdeck_desktop::theme::{PaletteId, resolve_palette};

use crate::ui::{
    UiIcon, field_row, h_row, icon_xs, palette, section_heading, slider, tab_bar_segmented,
    tab_item, to_hsla, v_col,
};

use super::SettingsView;

pub fn render_appearance(
    view: &SettingsView,
    zh: bool,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
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
    let font_val = view.ui_font_slider.read(cx).value().end().round() as u8;

    let mut swatches = v_col().w_full().gap(px(10.0)).pt(px(8.0));
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

            // Miniature PingDeck shell preview: left rail + floating rounded card with pane & CTA.
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
                    .min_w(px(150.0))
                    .p(px(8.0))
                    .rounded(px(10.0))
                    .bg(to_hsla(p.surface))
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
                                    div()
                                        .text_xs()
                                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                                        .text_color(to_hsla(p.ink))
                                        .child(if zh { pid.label_zh() } else { pid.label_en() }),
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

    let entity_theme = cx.entity().clone();
    let entity_density = cx.entity().clone();

    v_col()
        .gap(px(6.0))
        .child(field_row(
            if zh { "色彩模式" } else { "Color mode" },
            Some(SharedString::from(if zh {
                "跟随系统外观或固定浅色/深色主题"
            } else {
                "Follow system appearance or lock light / dark"
            })),
            tab_bar_segmented("appearance-theme-seg")
                .selected_index(theme_idx)
                .child(tab_item(if zh { "跟随系统" } else { "System" }))
                .child(tab_item(if zh { "浅色" } else { "Light" }))
                .child(tab_item(if zh { "深色" } else { "Dark" }))
                .on_click(move |idx, _, cx| {
                    let mode = match *idx {
                        1 => "light",
                        2 => "dark",
                        _ => "system",
                    };
                    entity_theme.update(cx, |this, cx| {
                        this.patch_section(
                            CoreSection::Appearance,
                            serde_json::json!({ "theme": mode }),
                            cx,
                        );
                    });
                }),
            cx,
        ))
        .child(field_row(
            if zh { "界面密度" } else { "Density" },
            Some(SharedString::from(if zh {
                "紧凑模式缩小行距，适合大信息量调试"
            } else {
                "Compact reduces row padding for high-density views"
            })),
            tab_bar_segmented("appearance-density-seg")
                .selected_index(density_idx)
                .child(tab_item(if zh { "舒适" } else { "Comfortable" }))
                .child(tab_item(if zh { "紧凑" } else { "Compact" }))
                .on_click(move |idx, _, cx| {
                    let density = if *idx == 1 { "compact" } else { "comfortable" };
                    entity_density.update(cx, |this, cx| {
                        this.patch_section(
                            CoreSection::Appearance,
                            serde_json::json!({ "density": density }),
                            cx,
                        );
                    });
                }),
            cx,
        ))
        .child(field_row(
            if zh { "界面字号" } else { "UI font size" },
            Some(SharedString::from(if zh {
                "实时按比例缩放全局界面字体（11–18 px）"
            } else {
                "Proportionally scales interface typography live (11–18 px)"
            })),
            h_row()
                .gap(px(10.0))
                .w(px(210.0))
                .child(div().flex_1().child(slider(&view.ui_font_slider)))
                .child(
                    div()
                        .w(px(40.0))
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(format!("{font_val} px"))),
                ),
            cx,
        ))
        .child(div().pt(px(12.0)).child(section_heading(
            if zh {
                "8 套护眼 OKLCH 色系（单帧零闪烁切换）"
            } else {
                "8 Eye-Care OKLCH Palettes (Instant Single-Frame Switch)"
            },
            cx,
        )))
        .child(swatches)
}
