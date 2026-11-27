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

/// Computes responsive viewport-percentage modal `(width_px, max_height_px)` from a baseline
/// `base_width_px` (authored for a 1280px window) so dialogs scale naturally in fullscreen without
/// bloating compact confirmations or cramping large wizards.
#[must_use]
pub fn responsive_modal_bounds(base_width_px: f32, vp_w: f32, vp_h: f32) -> (f32, f32) {
    let max_card_w = (vp_w - 64.0).max(320.0);
    let card_w = if base_width_px <= 500.0 {
        let pct_w = vp_w * (base_width_px / 1280.0);
        pct_w
            .clamp(base_width_px, (base_width_px * 1.25).min(600.0))
            .min(max_card_w)
    } else {
        let ratio = (base_width_px / 1200.0).clamp(0.56, 0.76);
        let pct_w = vp_w * ratio;
        pct_w.clamp(base_width_px, 1140.0).min(max_card_w)
    };
    let max_card_h = (vp_h * 0.84).clamp(320.0, (vp_h - 64.0).max(320.0));
    (card_w, max_card_h)
}

/// Computes responsive viewport-percentage slide-over sheet width from a 1280px baseline so
/// right-hand drawers scale proportionally in fullscreen without becoming cramped or overly wide.
#[must_use]
pub fn responsive_sheet_width(base_width_px: f32, vp_w: f32) -> f32 {
    let max_w = (vp_w - 64.0).max(360.0);
    let ratio = (base_width_px / 1280.0).clamp(0.42, 0.56);
    (vp_w * ratio).clamp(base_width_px, 920.0).min(max_w)
}

/// Renders a full-window modal dialog anchored at `(0, 0)` spanning the entire window viewport.
///
/// - Clicking outside the dialog card triggers `on_close`.
/// - Card width and max height scale proportionally with viewport size (so fullscreen layouts keep
///   balanced proportions while smaller windows remain unclipped).
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
    let (card_w, max_card_h) =
        responsive_modal_bounds(width_px, f32::from(vp.width), f32::from(vp.height));
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
                    v_col()
                        .w_full()
                        .flex_1()
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

/// Computes responsive viewport-percentage modal dimensions for large full-canvas modals
/// (e.g. the 90% Workflow Monitor overlay).
#[must_use]
pub fn viewport_modal_bounds(
    width_ratio: f32,
    height_ratio: f32,
    vp_w: f32,
    vp_h: f32,
) -> (f32, f32) {
    let max_w = (vp_w - 32.0).max(360.0);
    let max_h = (vp_h - 32.0).max(320.0);
    let w = (vp_w * width_ratio.clamp(0.4, 0.96)).clamp(840.0_f32.min(max_w), max_w);
    let h = (vp_h * height_ratio.clamp(0.4, 0.96)).clamp(560.0_f32.min(max_h), max_h);
    (w, h)
}

/// Renders a spacious viewport-proportional modal overlay anchored over the entire application
/// window (used for the 90% Workflow Monitor modal), with custom header/body/footer slots.
///
/// Clicking the outer scrim backdrop invokes `on_close`.
#[allow(clippy::too_many_arguments)]
#[must_use]
pub fn viewport_modal(
    id: impl Into<SharedString>,
    width_ratio: f32,
    height_ratio: f32,
    header: AnyElement,
    body: AnyElement,
    footer: AnyElement,
    window: &Window,
    cx: &App,
    on_close: impl Fn(&ClickEvent, &mut Window, &mut App) + Clone + 'static,
) -> AnyElement {
    let p = palette(cx);
    let vp = window.viewport_size();
    let id_s: SharedString = id.into();
    let (card_w, card_h) = viewport_modal_bounds(
        width_ratio,
        height_ratio,
        f32::from(vp.width),
        f32::from(vp.height),
    );
    let backdrop_id: ElementId = SharedString::from(format!("{id_s}-backdrop")).into();
    let card_id: ElementId = SharedString::from(format!("{id_s}-card")).into();

    let card_el = div()
        .id(card_id)
        .w(px(card_w))
        .h(px(card_h))
        .rounded(px(12.0))
        .shadow_xl()
        .on_click(|_, _, cx| cx.stop_propagation())
        .child(
            v_col()
                .size_full()
                .rounded(px(12.0))
                .overflow_hidden()
                .bg(to_hsla(p.surface))
                .border_1()
                .border_color(to_hsla(p.line_strong.composite_over(p.surface)))
                .child(header)
                .child(v_col().w_full().flex_1().min_h_0().child(body))
                .child(footer),
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
                .p(px(16.0))
                .on_click(on_close)
                .child(card_el),
        ),
    )
    .with_priority(10)
    .into_any_element()
}

#[cfg(test)]
mod tests {
    use super::{responsive_modal_bounds, responsive_sheet_width, viewport_modal_bounds};

    #[test]
    fn viewport_modal_uses_ninety_percent_and_clamps_safely() {
        let (w, h) = viewport_modal_bounds(0.90, 0.90, 1280.0, 800.0);
        assert!((w - 1152.0).abs() < 1.0);
        assert!((h - 720.0).abs() < 1.0);
        let (sw, sh) = viewport_modal_bounds(0.90, 0.90, 600.0, 480.0);
        assert!(sw <= 568.0);
        assert!(sh <= 448.0);
    }

    #[test]
    fn modal_bounds_scale_proportionally_in_fullscreen_and_clamp_safely() {
        // Large workspace modal (760px baseline): grows proportionally on 1728px / 1920px fullscreen
        // and lifts height beyond the legacy 660px ceiling.
        let (w_1280, h_1280) = responsive_modal_bounds(760.0, 1280.0, 800.0);
        let (w_1728, h_1728) = responsive_modal_bounds(760.0, 1728.0, 1080.0);
        let (w_1920, h_1920) = responsive_modal_bounds(760.0, 1920.0, 1080.0);
        let (w_2560, h_2560) = responsive_modal_bounds(760.0, 2560.0, 1440.0);

        assert!((760.0..900.0).contains(&w_1280));
        assert!(w_1728 > w_1280 && w_1728 >= 1050.0);
        assert!((w_1920 - 1140.0).abs() < 0.5);
        assert!((w_2560 - 1140.0).abs() < 0.5);
        assert!((h_1280 - 672.0).abs() < 0.5);
        assert!(h_1728 > 850.0 && (h_1920 - 907.2).abs() < 0.5);
        assert!(h_2560 > 1100.0);

        // Compact confirmation modal (440px baseline): grows gently without overstretching.
        let (cw_1280, _) = responsive_modal_bounds(440.0, 1280.0, 800.0);
        let (cw_1920, _) = responsive_modal_bounds(440.0, 1920.0, 1080.0);
        assert!((cw_1280 - 440.0).abs() < 0.5);
        assert!(cw_1920 > cw_1280 && cw_1920 <= 550.5);

        // Narrow window guard: never overflows viewport margins.
        let (nw, nh) = responsive_modal_bounds(760.0, 680.0, 500.0);
        assert!((nw - 616.0).abs() < 0.5);
        assert!(nh <= 436.0);
    }

    #[test]
    fn sheet_width_scales_proportionally_across_viewports() {
        let s_1280 = responsive_sheet_width(640.0, 1280.0);
        let s_1728 = responsive_sheet_width(640.0, 1728.0);
        let s_2560 = responsive_sheet_width(640.0, 2560.0);
        let s_narrow = responsive_sheet_width(640.0, 600.0);

        assert!((s_1280 - 640.0).abs() < 0.5);
        assert!(s_1728 > s_1280 && s_1728 >= 860.0);
        assert!((s_2560 - 920.0).abs() < 0.5);
        assert!((s_narrow - 536.0).abs() < 0.5);
    }
}
