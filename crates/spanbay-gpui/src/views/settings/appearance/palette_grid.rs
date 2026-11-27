//! 4×2 Eye-Care OKLCH surface palette miniature canvas grid for `Settings › Appearance`.
//!
//! Each miniature workspace card previews the palette's paper/dark surfaces live-tinted with
//! the user's active theme accent override.

use gpui_kit::{
    Context, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::theme::{PaletteId, resolve_palette_with_accent};

use crate::ui::{UiIcon, h_row, icon_xs, palette, to_hsla, v_col};

use super::super::SettingsView;
use super::{palette_name, palette_subtitle};

pub(super) fn render_palette_grid(
    view: &SettingsView,
    is_dark: bool,
    cx: &mut Context<SettingsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let active_accent = view.theme_prefs.accent;
    let mut swatches = v_col().w_full().gap(px(10.0));
    for chunk in PaletteId::ALL.chunks(4) {
        let mut row = h_row().w_full().gap(px(10.0));
        for &pid in chunk {
            let preview = resolve_palette_with_accent(pid, active_accent, is_dark, false);
            let selected = view.theme_prefs.palette == pid;
            let border = if selected {
                to_hsla(preview.accent)
            } else {
                to_hsla(preview.line_strong.composite_over(preview.surface))
            };

            let mini_preview =
                h_row()
                    .w_full()
                    .h(px(60.0))
                    .rounded(px(6.0))
                    .overflow_hidden()
                    .bg(to_hsla(preview.canvas))
                    .p(px(4.0))
                    .gap(px(4.0))
                    .child(
                        v_col()
                            .w(px(24.0))
                            .h_full()
                            .gap(px(3.0))
                            .pt(px(2.0))
                            .child(
                                div()
                                    .w(px(14.0))
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
                                    .w(px(16.0))
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
                                    .w(px(32.0))
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
                                    .child(div().w(px(20.0)).h(px(4.0)).rounded(px(2.0)).bg(
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
                                                    .w(px(40.0))
                                                    .h(px(5.0))
                                                    .rounded(px(2.0))
                                                    .bg(to_hsla(preview.ink)),
                                            )
                                            .child(
                                                div()
                                                    .w(px(54.0))
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
                    .min_w(px(136.0))
                    .p(px(7.0))
                    .rounded(px(9.0))
                    .bg(to_hsla(if selected { p.accent_soft } else { p.pane }))
                    .border_2()
                    .border_color(border)
                    .cursor_pointer()
                    .hover(|s| s.border_color(to_hsla(preview.accent)))
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.choose_palette(pid, window, cx);
                    }))
                    .child(
                        v_col().gap(px(6.0)).child(mini_preview).child(
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
