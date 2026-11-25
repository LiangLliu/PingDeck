//! Native `gpui-component` widget wrappers & PingDeck design-system primitives.
//!
//! All imports of `gpui_kit::component::*` remain strictly inside `src/ui/` (enforced by
//! `cargo xtask check-arch`). Feature views consume these semantic helpers instead of
//! binding directly to third-party widget types.

use gpui_kit::component::{
    Root, Sizable as _, Size,
    button::{Button, ButtonCustomVariant, ButtonVariants as _},
    checkbox::Checkbox,
    collapsible::Collapsible,
    input::{Input, InputState, Textarea, TextareaState},
    popover::Popover,
    progress::Progress,
    slider::{Slider, SliderState},
    spinner::Spinner,
    tab::{Tab, TabBar},
    tooltip::Tooltip,
};
use gpui_kit::{
    AnyElement, AnyView, App, AppContext as _, Div, ElementId, Entity, Hsla,
    InteractiveElement as _, IntoElement, ParentElement as _, SharedString, Stateful,
    StatefulInteractiveElement as _, Styled as _, Window, div, px, transparent_black,
};

use super::icons::{UiIcon, icon_sized, icon_xs};
use super::primitives::{Tone, h_row, v_col};
use super::theme::{palette, to_hsla};

/// Wraps the top-level shell view in `gpui-component`'s [`Root`] so native popovers, tooltips,
/// sheets, dialogs, notifications, and cross-element text selection work window-wide.
pub fn wrap_root(view: impl Into<AnyView>, window: &mut Window, cx: &mut App) -> Entity<Root> {
    let view = view.into();
    cx.new(|cx| Root::new(view, window, cx))
}

/// Non-color-only geometric status indicator matching `docs/design.md §2.2`.
///
/// Every semantic state has a distinct geometry so status remains legible under color-vision
/// deficiency: filled circle (`Ok`), haloed dot (`Live`), triangle (`Warn`), 45-degree diamond
/// (`Bad`), hollow ring (`Idle`), and rotating arc (`Run`).
#[derive(Debug, Clone, Copy, PartialEq, Eq)]
pub enum StatusShape {
    Ok,
    Live,
    Warn,
    Bad,
    Idle,
    Run,
}

/// Renders an accessible geometric status indicator.
#[must_use]
pub fn status_indicator(shape: StatusShape, cx: &App) -> AnyElement {
    let p = palette(cx);
    match shape {
        StatusShape::Ok => div()
            .size(px(7.0))
            .rounded(px(999.0))
            .bg(to_hsla(p.ok))
            .flex_shrink_0()
            .into_any_element(),
        StatusShape::Live => div()
            .size(px(12.0))
            .rounded(px(999.0))
            .bg(to_hsla(p.ok_soft))
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .child(div().size(px(6.0)).rounded(px(999.0)).bg(to_hsla(p.live)))
            .into_any_element(),
        StatusShape::Warn => div()
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .text_color(to_hsla(p.warn))
            .child(icon_xs(UiIcon::Triangle))
            .into_any_element(),
        StatusShape::Bad => div()
            .flex()
            .items_center()
            .justify_center()
            .flex_shrink_0()
            .text_color(to_hsla(p.bad))
            .child(icon_xs(UiIcon::Diamond))
            .into_any_element(),
        StatusShape::Idle => div()
            .size(px(8.0))
            .rounded(px(999.0))
            .border_2()
            .border_color(to_hsla(p.faint))
            .flex_shrink_0()
            .into_any_element(),
        StatusShape::Run => Spinner::new()
            .xsmall()
            .color(to_hsla(p.accent))
            .into_any_element(),
    }
}

/// Builds a native GPUI tooltip closure for any stateful element without exposing
/// `gpui_kit::component::tooltip::Tooltip` outside `src/ui/`.
pub fn text_tooltip(
    tip: impl Into<SharedString>,
) -> impl Fn(&mut Window, &mut App) -> AnyView + 'static {
    let tip_text: SharedString = tip.into();
    move |window, cx| Tooltip::new(tip_text.clone()).build(window, cx)
}

/// Compact, theme-adaptive monochrome icon + value metric datum with a native hover tooltip.
///
/// Uses `currentColor` vector SVG icons tinted from the active OKLCH palette (never fixed-color
/// emoji) so legibility and visual harmony stay consistent across all 16 light and dark themes.
#[must_use]
pub fn metric_datum(
    id: impl Into<ElementId>,
    glyph: UiIcon,
    value: impl Into<SharedString>,
    tip: impl Into<SharedString>,
    tone: Tone,
    cx: &App,
) -> Stateful<Div> {
    let p = palette(cx);
    let fg = match tone {
        Tone::Neutral => to_hsla(p.muted),
        Tone::Accent => to_hsla(p.accent),
        Tone::Success => to_hsla(p.ok),
        Tone::Warning => to_hsla(p.warn),
        Tone::Danger => to_hsla(p.bad),
    };
    let hover_fg = match tone {
        Tone::Neutral => to_hsla(p.ink),
        _ => fg,
    };
    div()
        .id(id)
        .flex()
        .flex_row()
        .items_center()
        .gap(px(4.0))
        .text_size(px(11.0))
        .text_color(fg)
        .hover(move |s| s.text_color(hover_fg))
        .tooltip(text_tooltip(tip))
        .child(icon_sized(glyph, Size::Size(px(11.5))))
        .child(value.into())
}

/// Native anchored floating popover menu.
#[must_use]
pub fn popover(id: impl Into<ElementId>) -> Popover {
    Popover::new(id)
}

/// Native underline tab bar for sub-views and inspectors.
#[must_use]
pub fn tab_bar_underline(id: impl Into<ElementId>) -> TabBar {
    TabBar::new(id).underline().small()
}

/// Native segmented control tab bar (mode switches, view toggles).
#[must_use]
pub fn tab_bar_segmented(id: impl Into<ElementId>) -> TabBar {
    TabBar::new(id).segmented().small()
}

/// Single tab entry for [`tab_bar_underline`] or [`tab_bar_segmented`].
#[must_use]
pub fn tab_item(label: impl Into<SharedString>) -> Tab {
    Tab::new().label(label)
}

/// Native smooth collapsible disclosure container.
#[must_use]
pub fn collapsible_box(open: bool, motion_id: impl Into<ElementId>) -> Collapsible {
    Collapsible::new().open(open).motion_id(motion_id)
}

/// Native linear progress bar (0.0..=100.0).
#[must_use]
pub fn progress_bar(id: impl Into<ElementId>, pct: f32) -> Progress {
    Progress::new(id).value(pct.clamp(0.0, 100.0)).xsmall()
}

/// Compact activity spinner.
#[must_use]
pub fn spinner_xs() -> Spinner {
    Spinner::new().xsmall()
}

/// Native checkbox control.
#[must_use]
pub fn checkbox(id: impl Into<ElementId>, checked: bool) -> Checkbox {
    Checkbox::new(id).checked(checked).small()
}

/// Native horizontal slider control.
#[must_use]
pub fn slider(state: &Entity<SliderState>) -> Slider {
    Slider::new(state).horizontal()
}

/// Seamless borderless multi-line textarea (used inside rounded composer cards).
#[must_use]
pub fn borderless_multiline(state: &Entity<TextareaState>) -> Textarea {
    Textarea::new(state).appearance(false).bordered(false)
}

/// Sunken macOS-style search field with leading magnifying-glass icon, comfortable 30px height,
/// subtle sunken fill, hairline border, crisp 1px accent focus edge, and built-in clear button.
#[must_use]
pub fn search_input(state: &Entity<InputState>, cx: &App) -> Input {
    let p = palette(cx);
    Input::new(state)
        .small()
        .cleanable(true)
        .prefix(
            div()
                .mr(px(2.0))
                .text_color(to_hsla(p.faint))
                .flex()
                .items_center()
                .child(icon_xs(UiIcon::Search)),
        )
        .h(px(30.0))
        .px(px(10.0))
        .text_xs()
        .rounded(px(8.0))
        .bg(to_hsla(p.sunken))
        .border_color(to_hsla(p.line_strong.composite_over(p.sunken)))
}

/// Bordered dropdown trigger pill (`label ▾`) with trailing chevron and active highlight,
/// matching PingDeck's `.select-pill` design token.
#[must_use]
pub fn select_pill(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    active: bool,
    cx: &App,
) -> Button {
    let p = palette(cx);
    let chev_fg = if active {
        to_hsla(p.accent_ink)
    } else {
        to_hsla(p.faint)
    };
    let btn = if active {
        let bg = to_hsla(p.accent_soft);
        let border = to_hsla(p.accent_line.composite_over(p.accent_soft));
        let fg = to_hsla(p.accent_ink);
        Button::new(id)
            .outline()
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(border)
                    .foreground(fg)
                    .hover(bg)
                    .active(bg),
            )
            .bg(bg)
            .border_color(border)
            .text_color(fg)
    } else {
        Button::new(id).text_color(to_hsla(p.ink_2))
    };
    btn.xsmall().h(px(28.0)).px(px(9.0)).rounded(px(7.0)).child(
        h_row()
            .gap(px(5.0))
            .child(div().text_xs().child(label.into()))
            .child(
                div()
                    .text_color(chev_fg)
                    .child(icon_xs(UiIcon::ChevronDown)),
            ),
    )
}

/// Bordered toggle filter chip with leading icon and crisp active state.
#[must_use]
pub fn filter_chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    glyph: Option<UiIcon>,
    active: bool,
    cx: &App,
) -> Button {
    let p = palette(cx);
    let btn = if active {
        let bg = to_hsla(p.accent_soft);
        let border = to_hsla(p.accent_line.composite_over(p.accent_soft));
        let fg = to_hsla(p.accent_ink);
        Button::new(id)
            .outline()
            .custom(
                ButtonCustomVariant::new(cx)
                    .color(border)
                    .foreground(fg)
                    .hover(bg)
                    .active(bg),
            )
            .bg(bg)
            .border_color(border)
            .text_color(fg)
    } else {
        Button::new(id)
            .bg(to_hsla(p.pane))
            .text_color(to_hsla(p.ink_2))
    };
    let mut b = btn
        .xsmall()
        .h(px(30.0))
        .px(px(10.0))
        .rounded(px(7.0))
        .label(label);
    if let Some(g) = glyph {
        b = b.icon(icon_xs(g));
    }
    b
}

/// Full-width or rail-sized ghost button container suitable as a [`Popover::trigger`] with custom
/// inner child layout (avoids `Button::label`'s forced horizontal centering on sidebar rows).
#[must_use]
pub fn popover_trigger_btn(id: impl Into<ElementId>, content: impl IntoElement) -> Button {
    Button::new(id)
        .ghost()
        .xsmall()
        .rounded(px(7.0))
        .child(content)
}

/// Balanced 28x28 icon-only titlebar button with a centered 16px Lucide glyph, crisp theme-aware
/// hover/active highlight, and optional corner status dot (e.g. Global Proxy state).
///
/// Works seamlessly both as a direct button and as a [`Popover::trigger`].
#[must_use]
pub fn titlebar_icon_btn(
    id: impl Into<ElementId>,
    glyph: UiIcon,
    status_dot_color: Option<Hsla>,
    cx: &App,
) -> Button {
    let p = palette(cx);
    let hover_bg = to_hsla(p.hover.composite_over(p.canvas));
    let active_bg = to_hsla(p.press.composite_over(p.canvas));
    let fg = to_hsla(p.ink_2);
    let ring = to_hsla(p.canvas);
    let inner = div()
        .relative()
        .size_full()
        .flex()
        .items_center()
        .justify_center()
        .child(icon_sized(glyph, Size::Size(px(16.0))))
        .children(status_dot_color.map(|col| {
            div()
                .absolute()
                .bottom(px(3.5))
                .right(px(3.5))
                .size(px(6.0))
                .rounded(px(99.0))
                .bg(col)
                .border_1()
                .border_color(ring)
        }));
    Button::new(id)
        .ghost()
        .custom(
            ButtonCustomVariant::new(cx)
                .color(transparent_black())
                .foreground(fg)
                .hover(hover_bg)
                .active(active_bg),
        )
        .size(px(28.0))
        .px(px(0.0))
        .py(px(0.0))
        .rounded(px(7.0))
        .child(inner)
}

/// Compact pill button suitable for toolbars and [`Popover::trigger`].
#[must_use]
pub fn pill_btn(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    glyph: Option<UiIcon>,
    active: bool,
) -> Button {
    let mut b = if active {
        Button::new(id).outline().xsmall().label(label)
    } else {
        Button::new(id).ghost().xsmall().label(label)
    };
    if let Some(g) = glyph {
        b = b.icon(icon_xs(g));
    }
    b
}

/// Circular icon-only action button (e.g., Chat composer Send / Stop).
#[must_use]
pub fn round_icon_btn(id: impl Into<ElementId>, glyph: UiIcon, tone: Tone) -> Button {
    let b = Button::new(id)
        .small()
        .rounded(px(999.0))
        .icon(icon_xs(glyph));
    match tone {
        Tone::Accent => b.primary(),
        Tone::Danger => b.danger(),
        _ => b.ghost(),
    }
}

/// Elevated card with a crisp 3px semantic top accent stripe (approval gates, cost pauses,
/// warning/danger callouts).
#[must_use]
pub fn top_accent_card(tone: Tone, cx: &App) -> Div {
    let p = palette(cx);
    let (stripe, bg, border) = match tone {
        Tone::Warning => (
            to_hsla(p.warn),
            to_hsla(p.surface),
            to_hsla(p.warn_line.composite_over(p.surface)),
        ),
        Tone::Danger => (
            to_hsla(p.bad),
            to_hsla(p.surface),
            to_hsla(p.bad_line.composite_over(p.surface)),
        ),
        Tone::Success => (
            to_hsla(p.ok),
            to_hsla(p.surface),
            to_hsla(p.ok_line.composite_over(p.surface)),
        ),
        Tone::Accent => (
            to_hsla(p.accent),
            to_hsla(p.surface),
            to_hsla(p.accent_line.composite_over(p.surface)),
        ),
        Tone::Neutral => (
            to_hsla(p.line_strong.composite_over(p.surface)),
            to_hsla(p.surface),
            to_hsla(p.line_strong.composite_over(p.surface)),
        ),
    };
    v_col()
        .rounded(px(12.0))
        .overflow_hidden()
        .bg(bg)
        .border_1()
        .border_color(border)
        .shadow_sm()
        .child(div().w_full().h(px(3.0)).bg(stripe))
}

/// Compact inline key-value chip for metadata strips.
#[must_use]
pub fn kv_chip(label: impl Into<SharedString>, value: impl Into<SharedString>, cx: &App) -> Div {
    let p = palette(cx);
    h_row()
        .gap(px(4.0))
        .px(px(7.0))
        .py(px(2.0))
        .rounded(px(6.0))
        .bg(to_hsla(p.sunken))
        .text_xs()
        .child(div().text_color(to_hsla(p.muted)).child(label.into()))
        .child(
            div()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(to_hsla(p.ink))
                .child(value.into()),
        )
}

/// Renders a compact, syntax-tinted unified diff preview (for tool call traces and file changes).
#[must_use]
pub fn diff_lines_preview(text: &str, max_lines: usize, cx: &App) -> Div {
    let p = palette(cx);
    let mut box_el = v_col()
        .w_full()
        .rounded(px(6.0))
        .overflow_hidden()
        .bg(to_hsla(p.sunken))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.sunken)))
        .py(px(4.0));

    let mut count = 0usize;
    let total = text.lines().count();
    for line in text.lines().take(max_lines) {
        count += 1;
        let (row_bg, fg) = if line.starts_with('+') && !line.starts_with("+++") {
            (Some(to_hsla(p.diff_add)), to_hsla(p.ok))
        } else if line.starts_with('-') && !line.starts_with("---") {
            (Some(to_hsla(p.diff_del)), to_hsla(p.bad))
        } else if line.starts_with("@@") {
            (Some(to_hsla(p.accent_soft)), to_hsla(p.accent_ink))
        } else {
            (None, to_hsla(p.ink_2))
        };
        let mut row = div()
            .px(px(8.0))
            .py(px(1.0))
            .text_xs()
            .text_color(fg)
            .child(SharedString::from(line.to_owned()));
        if let Some(bg) = row_bg {
            row = row.bg(bg);
        }
        box_el = box_el.child(row);
    }
    if total > count {
        box_el = box_el.child(
            div()
                .px(px(8.0))
                .pt(px(3.0))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(format!("… +{} lines", total - count))),
        );
    }
    box_el
}
