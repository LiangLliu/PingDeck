//! Unified workspace layout primitives: master sidebar headers/footers, section headers,
//! settings rows, toolbars, sub-tab headers, and IDE-grade document tabs.

use gpui_kit::{
    AnyElement, App, ClickEvent, Div, ElementId, Entity, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, Stateful, StatefulInteractiveElement as _, Styled as _,
    Window, div, px,
};

use super::icons::UiIcon;
use super::primitives::{h_row, text_input, v_col};
use super::theme::{is_compact, palette, to_hsla};
use crate::ui::InputState;

/// Unified master-pane header (`title` on left, actions on right,
/// optional search/filter slot below) whose top bar height matches [`toolbar_strip`] so
/// split-pane horizontal dividers always align cleanly.
#[must_use]
pub fn pane_header(
    title: impl Into<SharedString>,
    right_actions: Option<AnyElement>,
    below: Option<AnyElement>,
    cx: &App,
) -> Div {
    let p = palette(cx);
    let compact = is_compact(cx);
    let top_h = if compact { 40.0 } else { 44.0 };
    let left = h_row().gap(px(6.0)).min_w_0().child(
        div()
            .text_sm()
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .text_color(to_hsla(p.ink))
            .truncate()
            .child(title.into()),
    );
    let mut top = h_row()
        .w_full()
        .h(px(top_h))
        .flex_shrink_0()
        .px(px(14.0))
        .justify_between()
        .gap(px(8.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(left);
    if let Some(actions) = right_actions {
        top = top.child(div().flex_shrink_0().child(actions));
    }
    let mut box_el = v_col().w_full().flex_shrink_0().child(top);
    if let Some(extra) = below {
        box_el = box_el.child(
            div()
                .w_full()
                .px(px(10.0))
                .pt(px(8.0))
                .pb(px(6.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(extra),
        );
    }
    box_el
}

/// Unified 34px master-pane footer strip (`left summary`, `right action`) aligned horizontally
/// with [`pane_header`] and clearing the bottom-left page card corner cleanly.
#[must_use]
pub fn pane_footer(left: impl IntoElement, right: Option<AnyElement>, cx: &App) -> Div {
    let p = palette(cx);
    let mut row = h_row()
        .w_full()
        .h(px(34.0))
        .flex_shrink_0()
        .justify_between()
        .gap(px(8.0))
        .px(px(14.0))
        .border_t_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(left);
    if let Some(r) = right {
        row = row.child(div().flex_shrink_0().child(r));
    }
    row
}

/// Complete master sidebar shell (`pane` surface, header, scrollable content, optional footer).
#[must_use]
pub fn master_pane(
    header: impl IntoElement,
    body: impl IntoElement,
    footer: Option<AnyElement>,
    cx: &App,
) -> Div {
    let p = palette(cx);
    let mut col = v_col()
        .size_full()
        .min_w_0()
        .bg(to_hsla(p.pane))
        .child(header)
        .child(v_col().flex_1().min_h_0().w_full().child(body));
    if let Some(f) = footer {
        col = col.child(f);
    }
    col
}

/// Section header with prominent 15px semibold title on left, optional right action/link, and
/// bottom hairline border — clearly elevated above 13px [`setting_row`] item labels.
#[must_use]
pub fn section_header(title: impl Into<SharedString>, right: Option<AnyElement>, cx: &App) -> Div {
    let p = palette(cx);
    h_row()
        .w_full()
        .justify_between()
        .gap(px(10.0))
        .pb(px(8.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(
            div()
                .text_size(px(15.0))
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(p.ink))
                .child(title.into()),
        )
        .children(right)
}

/// Compact subtle metadata pill (e.g. "Local only", "Read-only", "Auto").
#[must_use]
pub fn meta_pill(text: impl Into<SharedString>, cx: &App) -> Div {
    let p = palette(cx);
    div()
        .px(px(6.0))
        .py(px(1.5))
        .rounded(px(4.0))
        .bg(to_hsla(p.sunken))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.sunken)))
        .text_size(px(10.5))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(to_hsla(p.muted))
        .flex_shrink_0()
        .child(text.into())
}

/// Grouped inset Settings section card matching the redesigned Settings prototype:
/// - Quiet 12px semibold `muted` eyebrow header outside the card (with optional right action),
/// - Elevated `surface` rounded card (`r=10px`, crisp `1px` border) framing child [`setting_row`]s
///   with 16px horizontal padding and cleanly clipped trailing hairline divider.
#[must_use]
pub fn settings_section(
    title: impl Into<SharedString>,
    right: Option<AnyElement>,
    body: impl IntoElement,
    cx: &App,
) -> Div {
    let p = palette(cx);
    v_col()
        .w_full()
        .gap(px(7.0))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .min_h(px(24.0))
                .px(px(4.0))
                .gap(px(10.0))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.muted))
                        .child(title.into()),
                )
                .children(right),
        )
        .child(
            div()
                .w_full()
                .rounded(px(10.0))
                .overflow_hidden()
                .bg(to_hsla(p.surface))
                .border_1()
                .border_color(to_hsla(p.line_strong.composite_over(p.pane)))
                .child(v_col().w_full().px(px(16.0)).mb(px(-1.0)).child(body)),
        )
}

/// Padded grouped Settings section card for rich non-row bodies (palette grids, tables, textareas).
#[must_use]
pub fn settings_section_padded(
    title: impl Into<SharedString>,
    right: Option<AnyElement>,
    body: impl IntoElement,
    cx: &App,
) -> Div {
    let p = palette(cx);
    v_col()
        .w_full()
        .gap(px(7.0))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .min_h(px(24.0))
                .px(px(4.0))
                .gap(px(10.0))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.muted))
                        .child(title.into()),
                )
                .children(right),
        )
        .child(
            v_col()
                .w_full()
                .p(px(16.0))
                .gap(px(10.0))
                .rounded(px(10.0))
                .bg(to_hsla(p.surface))
                .border_1()
                .border_color(to_hsla(p.line_strong.composite_over(p.pane)))
                .child(body),
        )
}

/// Edge-to-edge grouped Settings section card for full-bleed tables, ordered lists, and
/// expandable panels (`overflow_hidden`, `r=10px`, crisp `1px` border).
#[must_use]
pub fn settings_section_flush(
    title: impl Into<SharedString>,
    right: Option<AnyElement>,
    body: impl IntoElement,
    cx: &App,
) -> Div {
    let p = palette(cx);
    v_col()
        .w_full()
        .gap(px(7.0))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .min_h(px(24.0))
                .px(px(4.0))
                .gap(px(10.0))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.muted))
                        .child(title.into()),
                )
                .children(right),
        )
        .child(
            div()
                .w_full()
                .rounded(px(10.0))
                .overflow_hidden()
                .bg(to_hsla(p.surface))
                .border_1()
                .border_color(to_hsla(p.line_strong.composite_over(p.pane)))
                .child(body),
        )
}

/// Recessed inset sub-panel inside a Settings card for nested dependent fields or live previews.
#[must_use]
pub fn settings_sub_well(cx: &App) -> Div {
    let p = palette(cx);
    v_col()
        .w_full()
        .p(px(12.0))
        .gap(px(8.0))
        .rounded(px(8.0))
        .bg(to_hsla(p.sunken))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.sunken)))
}

/// Form / settings row with label, optional metadata pill, optional helper description, and
/// right-aligned control (unified across Settings, Requests Network/Auth, and Container settings).
#[must_use]
pub fn setting_row(
    label: impl Into<SharedString>,
    meta: Option<SharedString>,
    desc: Option<SharedString>,
    control: impl IntoElement,
    cx: &App,
) -> Div {
    let p = palette(cx);
    let compact = is_compact(cx);
    let title_row = h_row()
        .gap(px(7.0))
        .flex_wrap()
        .child(
            div()
                .text_size(px(13.5))
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(to_hsla(p.ink))
                .child(label.into()),
        )
        .children(meta.map(|m| meta_pill(m, cx)));

    let mut left = v_col().flex_1().min_w_0().gap(px(2.5)).child(title_row);
    if let Some(d) = desc
        && !d.is_empty()
    {
        left = left.child(div().text_xs().text_color(to_hsla(p.muted)).child(d));
    }

    h_row()
        .w_full()
        .justify_between()
        .items_center()
        .gap(px(20.0))
        .py(px(if compact { 9.0 } else { 12.5 }))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(left)
        .child(div().flex_shrink_0().child(control))
}

/// Compact numeric input paired with an optional muted trailing unit label (e.g., `ms`, `s`, `%`).
#[must_use]
pub fn number_field(
    input: &Entity<InputState>,
    unit: Option<&'static str>,
    width_px: f32,
    cx: &App,
) -> Div {
    let p = palette(cx);
    h_row()
        .gap(px(6.0))
        .child(div().w(px(width_px)).child(text_input(input)))
        .children(unit.map(|u| {
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .flex_shrink_0()
                .child(u)
        }))
}

/// Standard surface toolbar strip matching [`pane_header`]'s top row height (`44px` / `40px`
/// compact) with consistent horizontal padding and bottom hairline divider.
#[must_use]
pub fn toolbar_strip(cx: &App) -> Div {
    let p = palette(cx);
    let compact = is_compact(cx);
    let min_h = if compact { 40.0 } else { 44.0 };
    h_row()
        .w_full()
        .min_h(px(min_h))
        .flex_shrink_0()
        .gap(px(8.0))
        .px(px(16.0))
        .py(px(6.0))
        .bg(to_hsla(p.surface))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
}

/// Unified underline sub-tab header bar (`tabs` on left, optional metadata/actions on right).
#[must_use]
pub fn subtab_strip(tabs: impl IntoElement, right: Option<AnyElement>, cx: &App) -> Div {
    let p = palette(cx);
    let mut row = h_row()
        .w_full()
        .flex_shrink_0()
        .justify_between()
        .gap(px(8.0))
        .px(px(12.0))
        .pt(px(4.0))
        .bg(to_hsla(p.surface))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .child(div().min_w_0().child(tabs));
    if let Some(r) = right {
        row = row.child(div().flex_shrink_0().pb(px(3.0)).child(r));
    }
    row
}

/// Sleek document workbench tab integrated cleanly with the surface below (`active` merges into
/// `--surface` with a crisp 2px top accent bar; `dirty` renders a subtle accent dot).
#[allow(clippy::too_many_arguments)]
pub fn doc_tab(
    id: impl Into<ElementId>,
    active: bool,
    dirty: bool,
    prefix: AnyElement,
    title: impl Into<SharedString>,
    close_btn: AnyElement,
    cx: &App,
    on_click: impl Fn(&ClickEvent, &mut Window, &mut App) + 'static,
) -> Stateful<Div> {
    let p = palette(cx);
    let hover_bg = to_hsla(p.hover.composite_over(p.pane));
    let mut tab = div()
        .id(id)
        .relative()
        .flex()
        .flex_row()
        .items_center()
        .flex_shrink_0()
        .gap(px(6.0))
        .h(px(30.0))
        .pl(px(10.0))
        .pr(px(4.0))
        .rounded_t(px(7.0))
        .cursor_pointer()
        .on_click(on_click);

    if active {
        tab = tab
            .bg(to_hsla(p.surface))
            .border_t_1()
            .border_l_1()
            .border_r_1()
            .border_color(to_hsla(p.line_strong.composite_over(p.surface)));
    } else {
        tab = tab.rounded(px(6.0)).hover(move |s| s.bg(hover_bg));
    }

    tab = tab
        .child(prefix)
        .child(
            div()
                .max_w(px(150.0))
                .text_xs()
                .font_weight(if active {
                    gpui_kit::FontWeight::SEMIBOLD
                } else {
                    gpui_kit::FontWeight::NORMAL
                })
                .text_color(if active {
                    to_hsla(p.ink)
                } else {
                    to_hsla(p.muted)
                })
                .truncate()
                .child(title.into()),
        )
        .children(dirty.then(|| {
            div()
                .size(px(6.0))
                .rounded(px(999.0))
                .bg(to_hsla(p.accent))
                .flex_shrink_0()
        }))
        .child(close_btn);

    if active {
        tab = tab.child(
            div()
                .absolute()
                .top(px(0.0))
                .left(px(8.0))
                .right(px(8.0))
                .h(px(2.0))
                .rounded(px(2.0))
                .bg(to_hsla(p.accent)),
        );
    }

    let _ = UiIcon::Close;
    tab
}
