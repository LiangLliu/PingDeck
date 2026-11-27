//! HTTP Response inspector sub-tabs: Body (filter + extract), Headers, Cookies, Trace, Tests.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::executor::{CookieJarOutcome, CookieRejectReason, ExecutionResult};

use crate::ui::{
    ScrollableElement as _, Tone, alert_banner, badge, btn_ghost, btn_secondary, card, code_editor,
    data_row, empty_inline, h_row, kv_line, palette, section_heading, text_input, to_hsla, v_col,
};

use super::RequestsView;
use super::resp_http::warning_strip;

pub(super) fn render_resp_body(
    view: &RequestsView,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let Some(tab) = view.active_req() else {
        return v_col();
    };
    let filter_status = tab
        .http_jsonpath_result
        .as_ref()
        .map(|(ok, _, mode)| (*ok, mode.clone()));
    let has_filter = filter_status.is_some();
    let cur_fmt = tab.http_format;
    let cur_pretty = tab.http_pretty;
    let line_count = view.response_editor.read(cx).value().lines().count();

    v_col()
        .size_full()
        .min_h_0()
        .child(
            h_row()
                .w_full()
                .flex_shrink_0()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(6.0))
                .bg(to_hsla(p.pane))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(
                    div()
                        .w(px(260.0))
                        .child(text_input(&view.resp_filter_input)),
                )
                .child(
                    btn_secondary(
                        "resp-filter-go",
                        crate::ui::tr(cx).exchange().resp_http_tabs_filter(),
                    )
                    .on_click(
                        cx.listener(|this, _, window, cx| this.apply_jsonpath_filter(window, cx)),
                    ),
                )
                .children(has_filter.then(|| {
                    btn_ghost(
                        "resp-filter-reset",
                        crate::ui::tr(cx).exchange().resp_http_tabs_reset(),
                    )
                    .on_click(cx.listener(move |this, _, window, cx| {
                        this.resp_filter_input
                            .update(cx, |i, cx| i.set_value("", window, cx));
                        if let Some(t) = this.active_req_mut() {
                            t.http_jsonpath_query.clear();
                            t.http_jsonpath_result = None;
                        }
                        this.set_http_response_format(cur_fmt, cur_pretty, window, cx);
                    }))
                }))
                .children(filter_status.map(|(ok, mode)| {
                    badge(mode, if ok { Tone::Accent } else { Tone::Danger }, cx)
                }))
                .child(div().flex_1())
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(
                            crate::ui::tr(cx)
                                .exchange()
                                .resp_http_tabs_lines_utf_8(line_count),
                        )),
                )
                .child(
                    btn_ghost(
                        "resp-extract-env",
                        crate::ui::tr(cx).exchange().resp_http_tabs_extract_to_env(),
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_extract_variable_modal(window, cx);
                    })),
                ),
        )
        .child(
            v_col()
                .flex_1()
                .min_h_0()
                .w_full()
                .p(px(8.0))
                .child(code_editor(&view.response_editor).readonly(true)),
        )
}

pub(super) fn render_resp_headers(
    resp: &ExecutionResult,
    cx: &Context<RequestsView>,
) -> impl IntoElement {
    let mut list = v_col()
        .size_full()
        .w_full()
        .p(px(12.0))
        .gap(px(4.0))
        .overflow_y_scrollbar();
    if resp.headers.is_empty() {
        return list.child(empty_inline(
            crate::ui::tr(cx)
                .exchange()
                .resp_http_tabs_no_response_headers(),
            None,
            None,
            cx,
        ));
    }
    for h in &resp.headers {
        list = list.child(kv_line(h.key.clone(), h.value.clone(), 220.0, cx));
    }
    list
}

pub(super) fn render_resp_cookies(
    resp: &ExecutionResult,
    cx: &Context<RequestsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let (jar_lbl, jar_tone) = match resp.cookie_report.jar {
        CookieJarOutcome::Stored => (
            crate::ui::tr(cx)
                .exchange()
                .resp_http_tabs_stored_in_cookie_jar(),
            Tone::Success,
        ),
        CookieJarOutcome::NotStored => (
            crate::ui::tr(cx)
                .exchange()
                .resp_http_tabs_not_stored_in_jar(),
            Tone::Neutral,
        ),
        CookieJarOutcome::Failed => (
            crate::ui::tr(cx)
                .exchange()
                .resp_http_tabs_failed_to_write_cookie_jar(),
            Tone::Danger,
        ),
    };

    let mut list = v_col()
        .size_full()
        .w_full()
        .p(px(12.0))
        .gap(px(8.0))
        .overflow_y_scrollbar()
        .child(h_row().gap(px(8.0)).child(badge(jar_lbl, jar_tone, cx)));

    for ck in &resp.cookies {
        let mut flags = Vec::new();
        if ck.http_only {
            flags.push("HttpOnly");
        }
        if ck.secure {
            flags.push("Secure");
        }
        if let Some(ss) = &ck.same_site {
            flags.push(ss.as_str());
        }
        list = list.child(
            card(cx).p(px(8.0)).child(
                h_row()
                    .w_full()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .w(px(150.0))
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .truncate()
                            .child(SharedString::from(ck.name.clone())),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink_2))
                            .truncate()
                            .child(SharedString::from(ck.value.clone())),
                    )
                    .child(badge(
                        format!(
                            "{}{}",
                            ck.domain.as_deref().unwrap_or(""),
                            ck.path.as_deref().unwrap_or("/")
                        ),
                        Tone::Neutral,
                        cx,
                    ))
                    .children(
                        (!flags.is_empty()).then(|| badge(flags.join(" · "), Tone::Accent, cx)),
                    ),
            ),
        );
    }

    if !resp.cookie_report.rejected.is_empty() {
        list = list.child(section_heading(
            crate::ui::tr(cx)
                .exchange()
                .resp_http_tabs_rejected_set_cookie(),
            cx,
        ));
        let ex = crate::ui::tr(cx).exchange();
        for rej in &resp.cookie_report.rejected {
            let reason = match rej.reason {
                CookieRejectReason::Malformed => ex.cookie_malformed(),
                CookieRejectReason::ForeignDomain => ex.cookie_foreign_domain(),
                CookieRejectReason::PublicSuffix => ex.cookie_public_suffix(),
                CookieRejectReason::TooLarge => ex.cookie_too_large(),
            };
            list = list.child(alert_banner(
                Tone::Warning,
                format!("{reason}: {} ({})", rej.name, rej.host),
                None,
                cx,
            ));
        }
    }
    list
}

pub(super) fn render_resp_trace(
    resp: &ExecutionResult,
    cx: &Context<RequestsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let cc = &resp.call_chain;
    let mut col = v_col()
        .size_full()
        .w_full()
        .p(px(12.0))
        .gap(px(10.0))
        .overflow_y_scrollbar();

    // Wire timing waterfall card
    let t = resp.timing;
    let tot = t.total_ms.max(1) as f32;
    let phases: [(&str, Option<u64>, spanbay_desktop::theme::RgbaColor); 5] = [
        ("DNS", t.dns_ms.ms(), p.ph_dns),
        (
            if t.connect_includes_tls {
                "Connect + TLS"
            } else {
                "Connect"
            },
            t.connect_ms.ms(),
            p.ph_connect,
        ),
        ("TLS", t.tls_ms.ms(), p.ph_tls),
        (
            crate::ui::tr(cx).exchange().resp_http_tabs_wait_ttfb(),
            Some(t.wait_ms.unwrap_or(t.request_ms as u64)),
            p.ph_wait,
        ),
        (
            crate::ui::tr(cx).exchange().resp_http_tabs_receive(),
            Some(t.response_ms as u64),
            p.ph_recv,
        ),
    ];
    let mut wf = v_col().gap(px(5.0)).child(section_heading(
        crate::ui::tr(cx)
            .exchange()
            .resp_http_tabs_timing_waterfall_total_ms(t.total_ms),
        cx,
    ));
    for (lbl, ms_opt, color) in phases {
        let ms = ms_opt.unwrap_or(0);
        let w = ((ms as f32 / tot) * 200.0).clamp(if ms > 0 { 4.0 } else { 0.0 }, 200.0);
        wf = wf.child(
            h_row()
                .gap(px(8.0))
                .child(
                    div()
                        .w(px(110.0))
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(lbl),
                )
                .child(
                    div()
                        .w(px(200.0))
                        .h(px(7.0))
                        .rounded(px(4.0))
                        .bg(to_hsla(p.sunken))
                        .overflow_hidden()
                        .child(div().h_full().w(px(w)).bg(to_hsla(color))),
                )
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .child(ms_opt.map_or_else(|| "—".into(), |v| format!("{v} ms"))),
                ),
        );
    }
    col = col.child(card(cx).p(px(10.0)).child(wf));

    // Transport summary
    col = col.child(
        card(cx).p(px(10.0)).child(
            v_col()
                .gap(px(4.0))
                .child(section_heading(
                    crate::ui::tr(cx)
                        .exchange()
                        .resp_http_tabs_transport_connection(),
                    cx,
                ))
                .child(div().text_xs().child(SharedString::from(format!(
                    "Protocol: {} · Remote: {} · Attempts: {} · Proxy: {}",
                    cc.transport.protocol,
                    cc.transport.remote_addr.as_deref().unwrap_or("—"),
                    resp.attempt_count,
                    cc.transport.proxy_url.as_deref().unwrap_or("Direct"),
                ))))
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(format!(
                            "Auth: {} ({}) → {}",
                            cc.auth.auth_type, cc.auth.source, cc.auth.applied_to
                        ))),
                ),
        ),
    );

    if !cc.transport.redirect_hops.is_empty() {
        let mut hops = v_col().gap(px(3.0)).child(section_heading(
            crate::ui::tr(cx).exchange().resp_http_tabs_redirect_hops(),
            cx,
        ));
        for h in &cc.transport.redirect_hops {
            hops = hops.child(div().text_xs().child(SharedString::from(format!(
                "#{} {} {} → {} ({} ms)",
                h.hop, h.method, h.url, h.status_code, h.duration_ms
            ))));
        }
        col = col.child(card(cx).p(px(10.0)).child(hops));
    }

    if !cc.variables.is_empty() {
        let mut vars = v_col().gap(px(3.0)).child(section_heading(
            crate::ui::tr(cx)
                .exchange()
                .resp_http_tabs_resolved_variables(),
            cx,
        ));
        for v in &cc.variables {
            vars = vars.child(
                h_row()
                    .justify_between()
                    .child(div().text_xs().child(SharedString::from(format!(
                        "{{{{{}}}}} = {}",
                        v.name, v.resolved_value
                    ))))
                    .child(badge(format!("{:?}", v.source), Tone::Neutral, cx)),
            );
        }
        col = col.child(card(cx).p(px(10.0)).child(vars));
    }

    if let Some(sent) = &resp.sent_request {
        let mut hdr_box = v_col()
            .gap(px(3.0))
            .child(section_heading(
                crate::ui::tr(cx)
                    .exchange()
                    .resp_http_tabs_actual_wire_request(),
                cx,
            ))
            .child(
                div()
                    .text_xs()
                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                    .child(SharedString::from(format!("{} {}", sent.method, sent.url))),
            );
        for h in &sent.headers {
            hdr_box = hdr_box.child(
                div()
                    .text_size(px(11.0))
                    .text_color(to_hsla(p.muted))
                    .child(SharedString::from(format!("{}: {}", h.key, h.value))),
            );
        }
        col = col.child(card(cx).p(px(10.0)).child(hdr_box));
    }
    col
}

pub(super) fn render_resp_tests(
    resp: &ExecutionResult,
    cx: &Context<RequestsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let mut col = v_col()
        .size_full()
        .w_full()
        .p(px(12.0))
        .gap(px(6.0))
        .overflow_y_scrollbar();

    if let Some(err) = &resp.script_error {
        col = col.child(warning_strip(
            &crate::ui::tr(cx)
                .exchange()
                .script_error_banner(&err.source, &err.message),
            cx,
        ));
    }
    for a in &resp.assertion_results {
        col = col.child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(8.0))
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .bg(to_hsla(if a.passed { p.ok_soft } else { p.bad_soft }))
                .child(
                    h_row()
                        .gap(px(8.0))
                        .min_w_0()
                        .child(badge(
                            if a.passed {
                                crate::ui::tr(cx).exchange().resp_http_tabs_pass()
                            } else {
                                crate::ui::tr(cx).exchange().resp_http_tabs_fail()
                            },
                            if a.passed {
                                Tone::Success
                            } else {
                                Tone::Danger
                            },
                            cx,
                        ))
                        .child(div().text_xs().child(SharedString::from(a.label.clone()))),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(a.message.clone())),
                ),
        );
    }
    for t in &resp.script_tests {
        let tone = if t.skipped {
            Tone::Neutral
        } else if t.passed {
            Tone::Success
        } else {
            Tone::Danger
        };
        col = col.child(
            data_row(cx)
                .justify_between()
                .child(
                    h_row()
                        .gap(px(8.0))
                        .child(badge(
                            if t.skipped {
                                "Skip"
                            } else if t.passed {
                                "Pass"
                            } else {
                                "Fail"
                            },
                            tone,
                            cx,
                        ))
                        .child(div().text_xs().child(SharedString::from(t.name.clone()))),
                )
                .children(t.error.as_ref().map(|e| {
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.bad))
                        .child(SharedString::from(e.clone()))
                })),
        );
    }
    if resp.assertion_results.is_empty()
        && resp.script_tests.is_empty()
        && resp.script_error.is_none()
    {
        col = col.child(empty_inline(
            crate::ui::tr(cx).exchange().resp_ai_tabs_no_test_results(),
            Some(SharedString::from(
                crate::ui::tr(cx)
                    .exchange()
                    .resp_http_tabs_no_assertions_or_script_tests(),
            )),
            None,
            cx,
        ));
    }
    col
}
