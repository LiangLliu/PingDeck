//! Unified popover menu primitives (headers, dividers, icon menu items) and semantic HTTP
//! method color pills.

use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::{
    App, ClickEvent, Div, ElementId, Hsla, InteractiveElement as _, ParentElement as _,
    SharedString, Stateful, StatefulInteractiveElement as _, Styled as _, Window, div, px,
};

use super::icons::{UiIcon, icon_xs};
use super::primitives::{h_row, list_row};
use super::theme::{palette, to_hsla};

/// Semantic OKLCH foreground color for an HTTP method (`GET`, `POST`, `PUT`, `PATCH`, `DELETE`, ...).
#[must_use]
pub fn method_color(method: &str, cx: &App) -> Hsla {
    let p = palette(cx);
    match method.to_ascii_uppercase().as_str() {
        "GET" => to_hsla(p.m_get),
        "POST" => to_hsla(p.m_post),
        "PUT" => to_hsla(p.m_put),
        "PATCH" => to_hsla(p.m_patch),
        "DELETE" => to_hsla(p.m_delete),
        _ => to_hsla(p.muted),
    }
}

/// Bordered HTTP method dropdown trigger pill preserving each HTTP verb's OKLCH semantic color.
#[must_use]
pub fn method_pill(id: impl Into<ElementId>, method: &str, cx: &App) -> Button {
    let p = palette(cx);
    let upper = method.to_ascii_uppercase();
    let fg = method_color(&upper, cx);
    let bg = to_hsla(p.sunken);
    let hover_bg = to_hsla(p.hover.composite_over(p.sunken));
    let border = to_hsla(p.line_strong.composite_over(p.sunken));

    Button::new(id)
        .outline()
        .custom(
            ButtonCustomVariant::new(cx)
                .color(border)
                .foreground(fg)
                .hover(hover_bg)
                .active(hover_bg),
        )
        .bg(bg)
        .border_color(border)
        .text_color(fg)
        .xsmall()
        .h(px(28.0))
        .px(px(10.0))
        .rounded(px(7.0))
        .child(
            h_row()
                .gap(px(5.0))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(fg)
                        .child(SharedString::from(upper)),
                )
                .child(
                    div()
                        .text_color(to_hsla(p.faint))
                        .child(icon_xs(UiIcon::ChevronDown)),
                ),
        )
}

/// Optional trailing action link for [`popover_header`]: `(id, label, click_handler)`.
pub type PopoverHeaderAction = (
    ElementId,
    SharedString,
    Box<dyn Fn(&ClickEvent, &mut Window, &mut App) + 'static>,
);

/// Consistent popover menu header with muted title on left and optional accent link on right.
#[must_use]
pub fn popover_header(
    title: impl Into<SharedString>,
    action: Option<PopoverHeaderAction>,
    cx: &App,
) -> Div {
    let p = palette(cx);
    let mut row = h_row()
        .w_full()
        .justify_between()
        .gap(px(8.0))
        .px(px(8.0))
        .pt(px(2.0))
        .pb(px(6.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.overlay)))
        .child(
            div()
                .text_size(px(11.0))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(p.muted))
                .child(title.into()),
        );
    if let Some((act_id, act_label, handler)) = action {
        row = row.child(
            div()
                .id(act_id)
                .cursor_pointer()
                .text_size(px(11.0))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(to_hsla(p.accent_ink))
                .on_click(move |ev, window, cx| handler(ev, window, cx))
                .child(act_label),
        );
    }
    row
}

/// 1px hairline separator for popover and context menus.
#[must_use]
pub fn menu_divider(cx: &App) -> Div {
    let p = palette(cx);
    div()
        .w_full()
        .my(px(3.0))
        .h(px(1.0))
        .bg(to_hsla(p.line.composite_over(p.overlay)))
}

/// Standard clickable popover menu item with leading icon and optional danger red tint.
pub fn menu_item(
    id: impl Into<ElementId>,
    glyph: UiIcon,
    label: impl Into<SharedString>,
    danger: bool,
    cx: &App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let p = palette(cx);
    let mut row = list_row(id, false, cx, on_click).gap(px(7.0));
    if danger {
        let bad = to_hsla(p.bad);
        let bad_bg = to_hsla(p.bad_soft);
        row = row
            .text_color(bad)
            .hover(move |s| s.bg(bad_bg).text_color(bad));
    }
    row.child(icon_xs(glyph))
        .child(div().flex_1().text_xs().truncate().child(label.into()))
}
