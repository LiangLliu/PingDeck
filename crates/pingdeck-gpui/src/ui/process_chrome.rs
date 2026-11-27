//! Reusable Process Graph UI chrome primitives inside the `src/ui/` boundary.
//!
//! Shared across Agent Workflow Monitor (`views/runs/`) and HTTP Request Flows (`views/flows/`):
//! - Swimlane band header labels & palette tint resolution
//! - Edge semantic label pills (conditional branches & `{{var}}` data-flow tags)
//! - Segmented waterfall timing bars (supports single-span Agent steps and multi-phase HTTP
//!   `DNS / TLS / TTFB / Body` bars)

use gpui_kit::{
    AnyElement, App, Hsla, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};
use pingdeck_desktop::state::{PositionedLaneBand, ProcessEdgeTone, ProcessTimingPhase};

use super::dag_canvas::ProcessLaneDraw;
use super::primitives::{h_row, v_col};
use super::theme::{palette, to_hsla};

/// Resolves deterministic low-contrast visual colors for one swimlane band.
#[must_use]
pub fn lane_draw_spec(band: &PositionedLaneBand, highlighted: bool, cx: &App) -> ProcessLaneDraw {
    let p = palette(cx);
    let accent = match band.accent_index % 4 {
        0 => to_hsla(p.accent),
        1 => to_hsla(p.ok),
        2 => to_hsla(p.warn),
        _ => to_hsla(p.muted),
    };
    let mut fill_color = if band.accent_index.is_multiple_of(2) {
        to_hsla(p.pane)
    } else {
        to_hsla(p.surface)
    };
    fill_color.a = if highlighted { 0.88 } else { 0.52 };
    let mut header_fill = to_hsla(p.pane);
    header_fill.a = 0.78;
    let border_color = to_hsla(p.line.composite_over(p.sunken));

    ProcessLaneDraw {
        x: band.x,
        y: band.y,
        w: band.w,
        h: band.h,
        header_w: band.header_w,
        header_h: band.header_h,
        horizontal: band.horizontal,
        fill_color,
        header_fill,
        border_color,
        accent_color: accent,
        highlighted,
    }
}

/// Renders the crisp title + count header overlay inside one [`PositionedLaneBand`].
#[must_use]
pub fn process_lane_header(band: &PositionedLaneBand, scale: f32, cx: &App) -> AnyElement {
    let p = palette(cx);
    if band.horizontal {
        v_col()
            .absolute()
            .left(px((band.x + 10.0) * scale))
            .top(px((band.y + 8.0) * scale))
            .w(px((band.header_w - 16.0).max(40.0) * scale))
            .gap(px(2.0 * scale))
            .child(
                div()
                    .text_size(px((11.5 * scale).clamp(9.5, 12.5)))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(to_hsla(p.ink_2))
                    .truncate()
                    .child(SharedString::from(band.title.clone())),
            )
            .child(
                div()
                    .text_size(px((10.0 * scale).clamp(8.5, 11.0)))
                    .text_color(to_hsla(p.faint))
                    .child(SharedString::from(format!("{}", band.node_count))),
            )
            .into_any_element()
    } else {
        h_row()
            .absolute()
            .left(px((band.x + 10.0) * scale))
            .top(px((band.y + 8.0) * scale))
            .w(px((band.header_w - 20.0).max(40.0) * scale))
            .justify_between()
            .gap(px(4.0))
            .child(
                div()
                    .flex_1()
                    .min_w_0()
                    .text_size(px((11.5 * scale).clamp(9.5, 12.5)))
                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                    .text_color(to_hsla(p.ink_2))
                    .truncate()
                    .child(SharedString::from(band.title.clone())),
            )
            .child(
                div()
                    .flex_shrink_0()
                    .text_size(px((10.0 * scale).clamp(8.5, 11.0)))
                    .text_color(to_hsla(p.faint))
                    .child(SharedString::from(format!("{}", band.node_count))),
            )
            .into_any_element()
    }
}

/// Renders a compact centered floating label pill along a routed edge (conditional branch label or
/// `{{var}}` data-flow dependency tag).
#[must_use]
pub fn process_edge_label_pill(
    label: &str,
    pos: (f32, f32),
    tone: ProcessEdgeTone,
    scale: f32,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let (bg, fg, border): (Hsla, Hsla, Hsla) = match tone {
        ProcessEdgeTone::Completed => (
            to_hsla(p.ok_soft.composite_over(p.surface)),
            to_hsla(p.ok),
            to_hsla(p.ok_line.composite_over(p.surface)),
        ),
        ProcessEdgeTone::Warning => (
            to_hsla(p.warn_soft.composite_over(p.surface)),
            to_hsla(p.warn),
            to_hsla(p.warn_line.composite_over(p.surface)),
        ),
        ProcessEdgeTone::Danger => (
            to_hsla(p.bad_soft.composite_over(p.surface)),
            to_hsla(p.bad),
            to_hsla(p.bad_line.composite_over(p.surface)),
        ),
        ProcessEdgeTone::Active | ProcessEdgeTone::Data => (
            to_hsla(p.accent_soft.composite_over(p.surface)),
            to_hsla(p.accent_ink),
            to_hsla(p.accent_line.composite_over(p.surface)),
        ),
        ProcessEdgeTone::Neutral | ProcessEdgeTone::Skipped => (
            to_hsla(p.pane),
            to_hsla(p.muted),
            to_hsla(p.line_strong.composite_over(p.pane)),
        ),
    };

    let pill_w = ((label.chars().count() as f32 * 6.5) + 14.0).clamp(32.0, 140.0) * scale;
    let pill_h = (18.0 * scale).clamp(15.0, 20.0);
    div()
        .absolute()
        .left(px(pos.0 * scale - pill_w * 0.5))
        .top(px(pos.1 * scale - pill_h * 0.5))
        .w(px(pill_w))
        .h(px(pill_h))
        .flex()
        .items_center()
        .justify_center()
        .px(px(5.0 * scale))
        .rounded(px(9.0))
        .bg(bg)
        .border_1()
        .border_color(border)
        .text_size(px((9.5 * scale).clamp(8.5, 10.5)))
        .font_weight(gpui_kit::FontWeight::MEDIUM)
        .text_color(fg)
        .truncate()
        .child(SharedString::from(label.to_owned()))
        .into_any_element()
}

/// Renders a multi-phase segmented waterfall bar (used by HTTP Flow timelines for DNS/TLS/TTFB/Body
/// and extensible to any multi-phase process step).
#[must_use]
pub fn process_segmented_waterfall_bar(
    start_ratio: f32,
    width_ratio: f32,
    track_w: f32,
    phases: &[ProcessTimingPhase],
    fallback_tone: ProcessEdgeTone,
    cx: &App,
) -> AnyElement {
    let p = palette(cx);
    let left_px = (start_ratio.clamp(0.0, 0.98) * track_w).floor();
    let total_w = (width_ratio.clamp(0.0, 1.0) * track_w)
        .max(4.0)
        .min((track_w - left_px).max(4.0));

    let tone_fill = |tone: ProcessEdgeTone| -> Hsla {
        match tone {
            ProcessEdgeTone::Completed => to_hsla(p.ok),
            ProcessEdgeTone::Warning => to_hsla(p.warn),
            ProcessEdgeTone::Danger => to_hsla(p.bad),
            ProcessEdgeTone::Active | ProcessEdgeTone::Data => to_hsla(p.accent),
            ProcessEdgeTone::Neutral | ProcessEdgeTone::Skipped => to_hsla(p.faint),
        }
    };

    let sum_ms: u64 = phases.iter().map(|ph| ph.duration_ms).sum();
    let bar: AnyElement = if phases.is_empty() || sum_ms == 0 {
        div()
            .absolute()
            .left(px(left_px))
            .top(px(2.0))
            .w(px(total_w))
            .h(px(7.0))
            .rounded(px(3.5))
            .bg(tone_fill(fallback_tone))
            .into_any_element()
    } else {
        let mut segs = h_row()
            .absolute()
            .left(px(left_px))
            .top(px(2.0))
            .w(px(total_w))
            .h(px(7.0))
            .rounded(px(3.5))
            .overflow_hidden();
        for ph in phases {
            if ph.duration_ms == 0 {
                continue;
            }
            let sw = ((ph.duration_ms as f32 / sum_ms as f32) * total_w).max(1.5);
            segs = segs.child(div().h_full().w(px(sw)).bg(tone_fill(ph.tone)));
        }
        segs.into_any_element()
    };

    div()
        .relative()
        .w(px(track_w))
        .h(px(11.0))
        .rounded(px(4.0))
        .bg(to_hsla(p.sunken))
        .child(bar)
        .into_any_element()
}
