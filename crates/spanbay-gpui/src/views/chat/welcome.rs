//! Welcome empty state with clickable prompt starter cards for a fresh AI Chat thread.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};

use crate::ui::{UiIcon, app_icon, h_row, icon_xs, palette, to_hsla, v_col};

use super::ChatView;

pub fn render_welcome_empty(cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let starters: [(&str, &str, UiIcon); 3] = [
        (
            ch.starter_debug_title(),
            ch.starter_debug_prompt(),
            UiIcon::Send,
        ),
        (
            ch.starter_arch_title(),
            ch.starter_arch_prompt(),
            UiIcon::FolderOpen,
        ),
        (
            ch.starter_tests_title(),
            ch.starter_tests_prompt(),
            UiIcon::ListChecks,
        ),
    ];

    let mut grid = h_row().flex_wrap().gap(px(10.0)).pt(px(8.0));
    for (i, (title, prompt, glyph)) in starters.into_iter().enumerate() {
        grid = grid.child(
            div()
                .id(SharedString::from(format!("chat-starter-{i}")))
                .flex_1()
                .min_w(px(200.0))
                .p(px(12.0))
                .rounded(px(10.0))
                .bg(to_hsla(p.pane))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .cursor_pointer()
                .hover(|s| s.border_color(to_hsla(p.accent_line.composite_over(p.pane))))
                .on_click(cx.listener(move |this, _, window, cx| {
                    this.apply_starter(prompt, window, cx);
                }))
                .child(
                    v_col()
                        .gap(px(6.0))
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .text_xs()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .child(div().text_color(to_hsla(p.accent)).child(icon_xs(glyph)))
                                .child(title),
                        )
                        .child(div().text_xs().text_color(to_hsla(p.muted)).child(prompt)),
                ),
        );
    }

    v_col()
        .items_center()
        .pt(px(52.0))
        .gap(px(12.0))
        .child(app_icon(52.0))
        .child(
            div()
                .text_base()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(p.ink))
                .child(crate::ui::tr(cx).chat().welcome_how_can_spanbay_help()),
        )
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .max_w(px(460.0))
                .child(
                    crate::ui::tr(cx)
                        .chat()
                        .welcome_native_selectable_streaming_markdown_with(),
                ),
        )
        .child(div().w_full().pt(px(12.0)).child(grid))
}
