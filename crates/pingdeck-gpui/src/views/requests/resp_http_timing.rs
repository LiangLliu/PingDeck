//! Timing waterfall popover & phase-row renderer for HTTP responses.

use gpui_kit::{Context, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::executor::ExecutionResult;
use pingdeck_desktop::core::model::phase::PhaseValue;

use crate::ui::{
    Anchor, Popover, btn_ghost, h_row, palette, popover, section_heading, to_hsla, v_col,
};

use super::RequestsView;

pub(super) fn render_timing_popover(
    resp: &ExecutionResult,
    cx: &mut Context<RequestsView>,
) -> Popover {
    let p = palette(cx);
    let t = resp.timing;
    let total = t.total_ms.max(1) as f32;
    let dns = t.dns_ms.ms().unwrap_or(0);
    let conn = t.connect_ms.ms().unwrap_or(0);
    let tls = t.tls_ms.ms().unwrap_or(0);
    let wait = t.wait_ms.unwrap_or(t.request_ms as u64);
    let recv = t.response_ms as u64;

    let pct =
        |v: u64| -> f32 { ((v as f32 / total) * 76.0).clamp(if v > 0 { 3.0 } else { 0.0 }, 76.0) };

    let trigger = btn_ghost(
        "resp-timing-trigger",
        SharedString::from(format!("{} ms ▾", resp.elapsed_ms)),
    )
    .h(px(24.0))
    .px(px(6.0))
    .child(
        h_row()
            .w(px(60.0))
            .h(px(5.0))
            .rounded(px(99.0))
            .overflow_hidden()
            .bg(to_hsla(p.sunken))
            .child(div().h_full().w(px(pct(dns) * 0.78)).bg(to_hsla(p.ph_dns)))
            .child(
                div()
                    .h_full()
                    .w(px(pct(conn) * 0.78))
                    .bg(to_hsla(p.ph_connect)),
            )
            .child(div().h_full().w(px(pct(tls) * 0.78)).bg(to_hsla(p.ph_tls)))
            .child(
                div()
                    .h_full()
                    .w(px(pct(wait) * 0.78))
                    .bg(to_hsla(p.ph_wait)),
            )
            .child(
                div()
                    .h_full()
                    .w(px(pct(recv) * 0.78))
                    .bg(to_hsla(p.ph_recv)),
            ),
    );

    popover("resp-timing-pop")
        .anchor(Anchor::TopLeft)
        .trigger(trigger)
        .content(move |_, _, pop_cx| {
            let pp = palette(pop_cx);
            v_col()
                .w(px(290.0))
                .gap(px(5.0))
                .p(px(4.0))
                .child(section_heading(
                    crate::ui::tr(pop_cx)
                        .exchange()
                        .resp_http_timing_timing_breakdown(),
                    pop_cx,
                ))
                .child(timing_row("DNS", t.dns_ms, total, pp.ph_dns, pp.sunken))
                .child(timing_row(
                    if t.connect_includes_tls {
                        "Connect + TLS"
                    } else {
                        "Connect"
                    },
                    t.connect_ms,
                    total,
                    pp.ph_connect,
                    pp.sunken,
                ))
                .child(timing_row("TLS", t.tls_ms, total, pp.ph_tls, pp.sunken))
                .child(timing_row(
                    crate::ui::tr(pop_cx)
                        .exchange()
                        .resp_http_timing_wait_ttfb(),
                    PhaseValue::Ms(wait),
                    total,
                    pp.ph_wait,
                    pp.sunken,
                ))
                .child(timing_row(
                    crate::ui::tr(pop_cx).exchange().resp_http_tabs_receive(),
                    PhaseValue::Ms(recv),
                    total,
                    pp.ph_recv,
                    pp.sunken,
                ))
        })
}

fn timing_row(
    label: &'static str,
    val: PhaseValue,
    total_ms: f32,
    color: pingdeck_desktop::theme::RgbaColor,
    track: pingdeck_desktop::theme::RgbaColor,
) -> gpui_kit::Div {
    let val_str = match val {
        PhaseValue::Ms(ms) => format!("{ms} ms"),
        PhaseValue::Reused => "Reused".into(),
        PhaseValue::NotNeeded => "—".into(),
        PhaseValue::NotMeasured => "N/A".into(),
    };
    let bar_w = val
        .ms()
        .map(|ms| {
            ((ms as f32 / total_ms.max(1.0)) * 78.0).clamp(if ms > 0 { 3.0 } else { 0.0 }, 78.0)
        })
        .unwrap_or(0.0);
    h_row()
        .justify_between()
        .gap(px(8.0))
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(6.0))
                .child(div().size(px(7.0)).rounded(px(99.0)).bg(to_hsla(color)))
                .child(div().text_xs().truncate().child(label)),
        )
        .child(
            div()
                .w(px(78.0))
                .h(px(6.0))
                .rounded(px(3.0))
                .bg(to_hsla(track))
                .overflow_hidden()
                .child(div().h_full().w(px(bar_w)).bg(to_hsla(color))),
        )
        .child(
            div()
                .w(px(52.0))
                .text_right()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(val_str),
        )
}
