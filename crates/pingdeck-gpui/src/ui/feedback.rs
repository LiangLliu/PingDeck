//! Unified empty states, inline status/warning/error banners, and dismissible flash strips.

use gpui_kit::{
    AnyElement, App, ClickEvent, Div, ElementId, IntoElement, ParentElement as _, SharedString,
    Styled as _, Window, div, px,
};

use super::icons::{UiIcon, icon};
use super::primitives::{Tone, btn_icon, h_row, v_col};
use super::theme::{palette, to_hsla};
use super::widgets::{StatusShape, status_indicator};

/// Centered empty state with subtle icon well, title, and centered helper copy.
#[must_use]
pub fn empty_state(
    glyph: UiIcon,
    title: impl Into<SharedString>,
    subtitle: impl Into<SharedString>,
    cx: &App,
) -> Div {
    empty_state_with_actions(glyph, title, subtitle, None, cx)
}

/// Centered empty state with optional CTA button row underneath.
#[must_use]
pub fn empty_state_with_actions(
    glyph: UiIcon,
    title: impl Into<SharedString>,
    subtitle: impl Into<SharedString>,
    actions: Option<AnyElement>,
    cx: &App,
) -> Div {
    let p = palette(cx);
    let mut box_el = v_col()
        .size_full()
        .items_center()
        .justify_center()
        .gap(px(10.0))
        .p(px(28.0))
        .child(
            div()
                .size(px(40.0))
                .flex()
                .items_center()
                .justify_center()
                .rounded(px(10.0))
                .bg(to_hsla(p.sunken))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.sunken)))
                .text_color(to_hsla(p.muted))
                .child(icon(glyph)),
        )
        .child(
            div()
                .text_sm()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(p.ink))
                .text_center()
                .child(title.into()),
        )
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .max_w(px(380.0))
                .text_center()
                .child(subtitle.into()),
        );
    if let Some(act) = actions {
        box_el = box_el.child(div().pt(px(4.0)).child(act));
    }
    box_el
}

/// Compact centered empty placeholder for narrow sidebars and nested panels.
#[must_use]
pub fn empty_inline(
    title: impl Into<SharedString>,
    subtitle: Option<SharedString>,
    actions: Option<AnyElement>,
    cx: &App,
) -> Div {
    let p = palette(cx);
    let mut col = v_col()
        .w_full()
        .items_center()
        .gap(px(8.0))
        .py(px(28.0))
        .px(px(14.0))
        .child(
            div()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(p.ink_2))
                .text_center()
                .child(title.into()),
        );
    if let Some(sub) = subtitle {
        col = col.child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .text_center()
                .child(sub),
        );
    }
    if let Some(act) = actions {
        col = col.child(div().pt(px(2.0)).child(act));
    }
    col
}

/// Semantic inline alert/warning/notice banner with OKLCH-paired soft background, hairline border,
/// accessible geometric status indicator, and optional right-side action.
#[must_use]
pub fn alert_banner(
    tone: Tone,
    message: impl Into<SharedString>,
    right_action: Option<AnyElement>,
    cx: &App,
) -> Div {
    let p = palette(cx);
    let (bg, border, fg, shape) = match tone {
        Tone::Danger => (
            to_hsla(p.bad_soft),
            to_hsla(p.bad_line.composite_over(p.bad_soft)),
            to_hsla(p.bad),
            StatusShape::Bad,
        ),
        Tone::Warning => (
            to_hsla(p.warn_soft),
            to_hsla(p.warn_line.composite_over(p.warn_soft)),
            to_hsla(p.warn),
            StatusShape::Warn,
        ),
        Tone::Success => (
            to_hsla(p.ok_soft),
            to_hsla(p.ok_line.composite_over(p.ok_soft)),
            to_hsla(p.ok),
            StatusShape::Ok,
        ),
        Tone::Accent => (
            to_hsla(p.accent_soft),
            to_hsla(p.accent_line.composite_over(p.accent_soft)),
            to_hsla(p.accent_ink),
            StatusShape::Live,
        ),
        Tone::Neutral => (
            to_hsla(p.sunken),
            to_hsla(p.line_strong.composite_over(p.sunken)),
            to_hsla(p.ink_2),
            StatusShape::Idle,
        ),
    };

    let mut row = h_row()
        .w_full()
        .justify_between()
        .gap(px(8.0))
        .px(px(10.0))
        .py(px(6.0))
        .rounded(px(7.0))
        .bg(bg)
        .border_1()
        .border_color(border)
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(8.0))
                .child(status_indicator(shape, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(fg)
                        .child(message.into()),
                ),
        );
    if let Some(act) = right_action {
        row = row.child(div().flex_shrink_0().child(act));
    }
    row
}

/// Dismissible semantic flash banner (for inline confirmations, validation errors, and notices).
#[must_use]
pub fn dismissible_banner(
    close_id: impl Into<ElementId>,
    tone: Tone,
    message: impl Into<SharedString>,
    on_dismiss: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
    cx: &App,
) -> Div {
    alert_banner(
        tone,
        message,
        Some(
            btn_icon(close_id, UiIcon::Close)
                .on_click(on_dismiss)
                .into_any_element(),
        ),
        cx,
    )
}
