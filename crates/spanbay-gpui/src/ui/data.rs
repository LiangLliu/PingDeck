//! Unified data-display primitives: selectable choice chips, key-value table headers & rows,
//! read-only inspector KV lines, and KPI summary cards.

use gpui_kit::component::Sizable as _;
use gpui_kit::component::button::{Button, ButtonCustomVariant, ButtonVariants as _};
use gpui_kit::{App, Div, ElementId, ParentElement as _, SharedString, Styled as _, div, px};

use super::primitives::{card, h_row, v_col};
use super::theme::{is_compact, palette, to_hsla};

/// Crisp selectable option chip (replaces ad-hoc `"● label"` / `"✓ label"` ghost buttons).
///
/// Active chips use OKLCH `accent_soft` fill + `accent_line` hairline border + `accent_ink` text;
/// inactive chips use `surface` fill + subtle hairline border.
#[must_use]
pub fn choice_chip(
    id: impl Into<ElementId>,
    label: impl Into<SharedString>,
    selected: bool,
    cx: &App,
) -> Button {
    let p = palette(cx);
    let btn = if selected {
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
        let bg = to_hsla(p.surface);
        let hover_bg = to_hsla(p.hover.composite_over(p.surface));
        let border = to_hsla(p.line_strong.composite_over(p.surface));
        let fg = to_hsla(p.ink_2);
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
    };
    btn.xsmall()
        .h(px(26.0))
        .px(px(10.0))
        .rounded(px(6.0))
        .label(label)
}

/// Subtle column header strip for Key-Value tables and list grids.
/// Each column is `(label, fixed_width_px)` where `None` means `flex_1`.
#[must_use]
pub fn table_header(columns: &[(&str, Option<f32>)], cx: &App) -> Div {
    let p = palette(cx);
    let mut row = h_row()
        .w_full()
        .gap(px(8.0))
        .px(px(10.0))
        .py(px(4.0))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)));
    for &(label, width_opt) in columns {
        let mut cell = div()
            .text_size(px(11.0))
            .font_weight(gpui_kit::FontWeight::SEMIBOLD)
            .text_color(to_hsla(p.muted))
            .truncate()
            .child(SharedString::from(label.to_owned()));
        if let Some(w) = width_opt {
            cell = cell.w(px(w)).flex_shrink_0();
        } else {
            cell = cell.flex_1().min_w_0();
        }
        row = row.child(cell);
    }
    row
}

/// Consistent bordered data row for Key-Value tables, assertions, variables, and form rows.
#[must_use]
pub fn data_row(cx: &App) -> Div {
    let p = palette(cx);
    let compact = is_compact(cx);
    h_row()
        .w_full()
        .gap(px(8.0))
        .px(px(10.0))
        .py(px(if compact { 4.0 } else { 5.5 }))
        .rounded(px(6.0))
        .bg(to_hsla(p.pane))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
}

/// Compact read-only key/value row for Headers, Trace, Timing, and Metadata inspectors.
#[must_use]
pub fn kv_line(
    key: impl Into<SharedString>,
    value: impl Into<SharedString>,
    key_width_px: f32,
    cx: &App,
) -> Div {
    let p = palette(cx);
    h_row()
        .w_full()
        .gap(px(10.0))
        .px(px(10.0))
        .py(px(5.0))
        .rounded(px(5.0))
        .bg(to_hsla(p.pane))
        .child(
            div()
                .w(px(key_width_px))
                .flex_shrink_0()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(to_hsla(p.ink))
                .truncate()
                .child(key.into()),
        )
        .child(
            div()
                .flex_1()
                .min_w_0()
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .truncate()
                .child(value.into()),
        )
}

/// Elevated KPI metric card (`label`, prominent `value`, optional muted subtitle).
#[must_use]
pub fn kpi_card(
    label: impl Into<SharedString>,
    value: impl Into<SharedString>,
    subtitle: Option<SharedString>,
    cx: &App,
) -> Div {
    let p = palette(cx);
    let mut inner = v_col()
        .gap(px(4.0))
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(label.into()),
        )
        .child(
            div()
                .text_lg()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(p.ink))
                .truncate()
                .child(value.into()),
        );
    if let Some(sub) = subtitle {
        inner = inner.child(
            div()
                .text_size(px(11.0))
                .text_color(to_hsla(p.muted))
                .truncate()
                .child(sub),
        );
    }
    card(cx).flex_1().min_w_0().p(px(14.0)).child(inner)
}
