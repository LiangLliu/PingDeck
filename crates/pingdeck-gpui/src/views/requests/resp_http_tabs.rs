//! HTTP Response inspector sub-tabs: Body (filter + extract), Headers, Cookies, Trace, Tests.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::executor::{CookieJarOutcome, CookieRejectReason, ExecutionResult};

use crate::ui::{
    ScrollableElement as _, Tone, alert_banner, badge, btn_ghost, btn_secondary, card, code_editor,
    data_row, empty_inline, h_row, kv_line, palette, section_heading, text_input, to_hsla, v_col,
};

use super::RequestsView;
use super::resp_http::warning_strip;

pub(super) fn render_resp_body(
    view: &RequestsView,
    zh: bool,
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

    v_col()
        .size_full()
        .child(
            h_row()
                .w_full()
                .gap(px(6.0))
                .px(px(10.0))
                .py(px(6.0))
                .bg(to_hsla(p.pane))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .child(
                    div()
                        .w(px(280.0))
                        .child(text_input(&view.resp_filter_input)),
                )
                .child(
                    btn_secondary("resp-filter-go", if zh { "筛选" } else { "Filter" }).on_click(
                        cx.listener(|this, _, window, cx| this.apply_jsonpath_filter(window, cx)),
                    ),
                )
                .children(filter_status.map(|(ok, mode)| {
                    badge(mode, if ok { Tone::Accent } else { Tone::Danger }, cx)
                }))
                .child(div().flex_1())
                .child(
                    btn_ghost(
                        "resp-extract-env",
                        if zh {
                            "提取为环境变量"
                        } else {
                            "Extract to env"
                        },
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.open_extract_variable_modal(window, cx);
                    })),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .p(px(8.0))
                .child(code_editor(&view.response_editor)),
        )
}

pub(super) fn render_resp_headers(
    resp: &ExecutionResult,
    zh: bool,
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
            if zh {
                "无响应头"
            } else {
                "No response headers"
            },
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
    zh: bool,
    cx: &Context<RequestsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let (jar_lbl, jar_tone) = match resp.cookie_report.jar {
        CookieJarOutcome::Stored => (
            if zh {
                "已存入工作区 Cookie 罐"
            } else {
                "Stored in Cookie Jar"
            },
            Tone::Success,
        ),
        CookieJarOutcome::NotStored => (
            if zh {
                "未存入 Cookie 罐"
            } else {
                "Not stored in jar"
            },
            Tone::Neutral,
        ),
        CookieJarOutcome::Failed => (
            if zh {
                "写入 Cookie 罐失败"
            } else {
                "Failed to write Cookie Jar"
            },
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
            if zh {
                "未保留的 Set-Cookie"
            } else {
                "Rejected Set-Cookie"
            },
            cx,
        ));
        for rej in &resp.cookie_report.rejected {
            let reason = match rej.reason {
                CookieRejectReason::Malformed => "格式无效",
                CookieRejectReason::ForeignDomain => "跨域名拒绝",
                CookieRejectReason::PublicSuffix => "公共后缀拒绝",
                CookieRejectReason::TooLarge => "超过 4096B 上限",
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
    zh: bool,
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

    // Transport summary
    col = col.child(
        card(cx).p(px(10.0)).child(
            v_col()
                .gap(px(4.0))
                .child(section_heading(
                    if zh {
                        "传输与连接追踪"
                    } else {
                        "Transport & Connection"
                    },
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
            if zh {
                "重定向跳转"
            } else {
                "Redirect Hops"
            },
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
            if zh {
                "本次解析的变量"
            } else {
                "Resolved Variables"
            },
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
                if zh {
                    "实际发送的请求（凭据已脱敏）"
                } else {
                    "Actual Wire Request"
                },
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
    zh: bool,
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
            &format!("脚本错误 ({}): {}", err.source, err.message),
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
                                if zh { "通过" } else { "Pass" }
                            } else if zh {
                                "未通过"
                            } else {
                                "Fail"
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
            if zh {
                "暂无测试结果"
            } else {
                "No test results"
            },
            Some(SharedString::from(if zh {
                "本次请求未配置声明式断言或 pm.test 测试。"
            } else {
                "No assertions or script tests ran for this request."
            })),
            None,
            cx,
        ));
    }
    col
}
