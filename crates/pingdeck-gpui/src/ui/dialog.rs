//! Unified window-level modal dialog rendered via GPUI `deferred(anchored)` so every dialog
//! across PingDeck covers the entire window (sidebar + page card) cleanly without clipping or
//! layout seams, and works both inside `Root` and standalone headless tests.

use gpui_kit::{
    AnyElement, App, ClickEvent, ElementId, InteractiveElement as _, IntoElement,
    ParentElement as _, SharedString, StatefulInteractiveElement as _, Styled as _, Window,
    anchored, deferred, div, point, px,
};

use super::icons::UiIcon;
use super::primitives::{btn_icon, h_row, v_col};
use super::theme::{palette, to_hsla};

/// Renders a full-window modal dialog anchored at `(0, 0)` spanning the entire window viewport.
///
/// - Clicking outside the dialog card triggers `on_close`.
/// - The card uses natural height up to viewport-bounded `max_h` with zero stray overflow bands.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn modal_dialog(
    id: impl Into<SharedString>,
    title: impl Into<SharedString>,
    width_px: f32,
    body: AnyElement,
    footer: AnyElement,
    window: &Window,
    cx: &App,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let p = palette(cx);
    let vp = window.viewport_size();
    let id_s: SharedString = id.into();
    let max_card_w = (f32::from(vp.width) - 48.0).max(320.0);
    let card_w = width_px.min(max_card_w);
    let max_card_h = (f32::from(vp.height) - 72.0).clamp(240.0, 660.0);
    let on_close_btn = on_close.clone();

    let backdrop_id: ElementId = SharedString::from(format!("{id_s}-backdrop")).into();
    let card_id: ElementId = SharedString::from(format!("{id_s}-card")).into();
    let close_id: ElementId = SharedString::from(format!("{id_s}-close")).into();

    let card_el = div()
        .id(card_id)
        .w(px(card_w))
        .max_h(px(max_card_h))
        .rounded(px(12.0))
        .shadow_xl()
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(
            v_col()
                .w_full()
                .max_h(px(max_card_h))
                .rounded(px(12.0))
                .overflow_hidden()
                .bg(to_hsla(p.overlay))
                .border_1()
                .border_color(to_hsla(p.line_strong.composite_over(p.overlay)))
                .child(
                    h_row()
                        .w_full()
                        .flex_shrink_0()
                        .justify_between()
                        .gap(px(12.0))
                        .px(px(18.0))
                        .py(px(13.0))
                        .bg(to_hsla(p.overlay))
                        .border_b_1()
                        .border_color(to_hsla(p.line.composite_over(p.overlay)))
                        .child(
                            div()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(title.into()),
                        )
                        .child(btn_icon(close_id, UiIcon::Close).on_click(on_close_btn)),
                )
                .child(
                    div()
                        .w_full()
                        .min_h_0()
                        .p(px(18.0))
                        .bg(to_hsla(p.overlay))
                        .child(body),
                )
                .child(
                    h_row()
                        .w_full()
                        .flex_shrink_0()
                        .justify_end()
                        .gap(px(8.0))
                        .px(px(18.0))
                        .py(px(12.0))
                        .bg(to_hsla(p.pane))
                        .border_t_1()
                        .border_color(to_hsla(p.line.composite_over(p.pane)))
                        .child(footer),
                ),
        );

    deferred(
        anchored().position(point(px(0.0), px(0.0))).child(
            div()
                .id(backdrop_id)
                .occlude()
                .w(vp.width)
                .h(vp.height)
                .bg(to_hsla(p.scrim))
                .flex()
                .items_center()
                .justify_center()
                .p(px(24.0))
                .on_click(on_close)
                .child(card_el),
        ),
    )
    .with_priority(10)
    .into_any_element()
}
