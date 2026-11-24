//! HTTP Response header, status/timing waterfall popover, error/warning banners, and tab dispatch.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::executor::{BodyIncomplete, ExecutionResult};
use pingdeck_desktop::core::model::phase::PhaseValue;
use pingdeck_desktop::core::response_viewer::ResponseBodyFormat;
use pingdeck_desktop::services::clipboard;

use crate::ui::{
    Anchor, Popover, ScrollableElement as _, StatusShape, Tone, UiIcon, alert_banner, badge,
    btn_ghost, btn_icon, btn_primary, btn_secondary, empty_state_with_actions, h_row, icon_xs,
    kbd_chip, kv_chip, list_row, palette, popover, section_heading, select_pill, status_indicator,
    subtab_strip, tab_bar_segmented, tab_bar_underline, tab_item, to_hsla, top_accent_card, v_col,
};

use super::RequestsView;
use super::model::{ALL_RESPONSE_FORMATS, HttpRespTab, RequestFailureInfo, format_bytes};
use super::resp_http_tabs::{
    render_resp_body, render_resp_cookies, render_resp_headers, render_resp_tests,
    render_resp_trace,
};

pub fn render_http_response(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::AnyElement {
    let p = palette(cx);
    let Some(tab) = view.active_req() else {
        return v_col().size_full().into_any_element();
    };

    if tab.sending {
        return v_col()
            .size_full()
            .items_center()
            .justify_center()
            .gap(px(10.0))
            .bg(to_hsla(p.surface))
            .child(status_indicator(StatusShape::Run, cx))
            .child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
                "正在等待服务器响应…（可随时点击上方「停止请求」）"
            } else {
                "Waiting for response… (click Stop request above to cancel)"
            }))
            .into_any_element();
    }

    if let Some(fail) = &tab.failure
        && tab.last_http.is_none()
    {
        return render_failure_card(fail, zh, cx).into_any_element();
    }

    let Some(resp) = &tab.last_http else {
        return empty_state_with_actions(
            UiIcon::Send,
            if zh {
                "准备就绪，发送请求以检视实时响应"
            } else {
                "Ready to send"
            },
            if zh {
                "支持状态码、阶段耗时瀑布图、响应头、Cookie 罐追踪、JSONPath 筛选及声明式断言检查。"
            } else {
                "Inspect status, timing waterfall, headers, cookies, JSONPath filter, and assertions."
            },
            Some(
                h_row()
                    .gap(px(8.0))
                    .child(
                        btn_primary("resp-empty-send", if zh { "立即发送" } else { "Send now" })
                            .icon(icon_xs(UiIcon::Send))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.send_active_request(None, None, window, cx);
                            })),
                    )
                    .child(kbd_chip("⌘↵", cx))
                    .into_any_element(),
            ),
            cx,
        )
        .bg(to_hsla(p.surface))
        .into_any_element();
    };

    let status_tone = match resp.status_code {
        200..=299 => Tone::Success,
        300..=399 => Tone::Accent,
        400..=499 => Tone::Warning,
        _ => Tone::Danger,
    };

    let tests_total = resp.assertion_results.len() + resp.script_tests.len();
    let tests_passed = resp.assertion_results.iter().filter(|a| a.passed).count()
        + resp.script_tests.iter().filter(|t| t.passed).count();
    let tests_failed = tests_total.saturating_sub(tests_passed);
    let has_check_fail = tests_failed > 0 || resp.script_error.is_some();

    let size_text = if let Some(total) = resp.body_total_bytes {
        format!(
            "{} / {}",
            format_bytes(resp.body_size_bytes as u64),
            format_bytes(total)
        )
    } else if resp.body_truncated {
        format!("{}+", format_bytes(resp.body_size_bytes as u64))
    } else {
        format_bytes(resp.body_size_bytes as u64)
    };

    let proto_text = if resp.call_chain.transport.protocol.is_empty() {
        "HTTP".to_owned()
    } else {
        resp.call_chain.transport.protocol.clone()
    };

    // 1. Response Header Bar
    let resp_head = h_row()
        .w_full()
        .justify_between()
        .gap(px(8.0))
        .px(px(14.0))
        .py(px(7.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            h_row()
                .gap(px(8.0))
                .flex_wrap()
                .child(badge(
                    format!("{} {}", resp.status_code, resp.status_text),
                    status_tone,
                    cx,
                ))
                .child(render_timing_popover(resp, zh, cx))
                .child(kv_chip(if zh { "大小" } else { "Size" }, size_text, cx))
                .child(badge(proto_text, Tone::Neutral, cx))
                .children(has_check_fail.then(|| {
                    badge(
                        if zh {
                            format!("检查未通过 ({tests_passed}/{tests_total})")
                        } else {
                            format!("Checks failed ({tests_passed}/{tests_total})")
                        },
                        Tone::Danger,
                        cx,
                    )
                }))
                .children((!resp.history_saved).then(|| {
                    badge(
                        if zh {
                            "未写入历史"
                        } else {
                            "Not in history"
                        },
                        Tone::Warning,
                        cx,
                    )
                })),
        )
        .child(
            h_row()
                .gap(px(4.0))
                .flex_shrink_0()
                .child(
                    btn_ghost("resp-save-file", if zh { "保存正文" } else { "Save" })
                        .icon(icon_xs(UiIcon::Download))
                        .on_click(cx.listener(|this, _, _, cx| {
                            this.save_response_body_to_file(cx);
                        })),
                )
                .child(
                    btn_secondary(
                        "resp-ask-agent",
                        if zh { "询问智能体" } else { "Ask Agent" },
                    )
                    .icon(icon_xs(UiIcon::Sparkles))
                    .on_click(cx.listener(|this, _, _, cx| this.handoff_ask_agent_http(cx))),
                )
                .child(
                    btn_icon("resp-copy-body", UiIcon::Copy)
                        .tooltip(if zh {
                            "复制响应正文"
                        } else {
                            "Copy response"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            let txt = this.response_editor.read(cx).value().to_string();
                            let _ = clipboard::set_text(&txt);
                            this.status_msg = Some((Tone::Success, "已复制响应正文".into()));
                            cx.notify();
                        })),
                ),
        );

    // 2. Response Sub-tabs + Pretty/Raw & Format Picker
    let ent_t = cx.entity().clone();
    let test_lbl = if tests_total > 0 {
        if zh {
            format!("测试 ({tests_passed}/{tests_total})")
        } else {
            format!("Tests ({tests_passed}/{tests_total})")
        }
    } else if zh {
        "测试".into()
    } else {
        "Tests".into()
    };

    let tabs = tab_bar_underline("req-http-resp-tabs")
        .selected_index(tab.http_resp_tab.index())
        .child(tab_item(if zh { "正文" } else { "Body" }))
        .child(tab_item(if zh {
            format!("响应头 ({})", resp.headers.len())
        } else {
            format!("Headers ({})", resp.headers.len())
        }))
        .child(tab_item(format!("Cookie ({})", resp.cookies.len())))
        .child(tab_item(if zh { "追踪" } else { "Trace" }))
        .child(tab_item(test_lbl))
        .on_click(move |idx, _, cx| {
            let next = HttpRespTab::from_index(*idx);
            ent_t.update(cx, |this, cx| {
                if let Some(t) = this.active_req_mut() {
                    t.http_resp_tab = next;
                }
                cx.notify();
            });
        });

    let cur_pretty = tab.http_pretty;
    let cur_fmt = tab.http_format;
    let ent_pr = cx.entity().clone();
    let ent_fm = cx.entity().clone();

    let format_controls = h_row()
        .gap(px(6.0))
        .child(
            tab_bar_segmented("resp-pretty-seg")
                .selected_index(usize::from(!cur_pretty))
                .child(tab_item(if zh { "格式化" } else { "Pretty" }))
                .child(tab_item(if zh { "原文" } else { "Raw" }))
                .on_click(move |idx, window, cx| {
                    ent_pr.update(cx, |this, cx| {
                        this.set_http_response_format(cur_fmt, *idx == 0, window, cx);
                    });
                }),
        )
        .child(
            popover("resp-fmt-pop")
                .anchor(Anchor::TopRight)
                .trigger(select_pill(
                    "resp-fmt-btn",
                    format_label(cur_fmt),
                    false,
                    cx,
                ))
                .content(move |_, _, pop_cx| {
                    let mut col = v_col().w(px(120.0)).gap(px(2.0));
                    for fmt in ALL_RESPONSE_FORMATS {
                        let sel = fmt == cur_fmt;
                        let ent_c = ent_fm.clone();
                        let ph = pop_cx.entity().clone();
                        col = col.child(
                            list_row(
                                SharedString::from(format!("fmt-opt-{fmt:?}")),
                                sel,
                                pop_cx,
                                move |_, window, cx| {
                                    ent_c.update(cx, |this, cx| {
                                        this.set_http_response_format(fmt, true, window, cx);
                                    });
                                    ph.update(cx, |s, cx| s.dismiss(window, cx));
                                },
                            )
                            .child(div().text_xs().child(format_label(fmt))),
                        );
                    }
                    col
                }),
        );

    let mut root = v_col().size_full().bg(to_hsla(p.surface)).child(resp_head);

    // Optional banners for incomplete / truncated / saved-to-file / failed retry.
    if let Some(reason) = resp.body_incomplete {
        let msg = match reason {
            BodyIncomplete::IdleTimeout => {
                if zh {
                    "响应正文提前结束（空闲超时）；已保留状态码、响应头和已收到的正文。"
                } else {
                    "Body ended early (idle timeout); partial bytes kept."
                }
            }
            BodyIncomplete::TotalTimeout => {
                if zh {
                    "响应正文提前结束（达到总时限）；已保留已收到的部分。"
                } else {
                    "Body ended early (total timeout); partial bytes kept."
                }
            }
            BodyIncomplete::Cancelled => {
                if zh {
                    "用户停止了响应读取；已保留状态码、响应头和已收到的正文。"
                } else {
                    "Stopped by user after headers; partial bytes kept."
                }
            }
        };
        root = root.child(warning_strip(msg, cx));
    } else if resp.body_truncated {
        root = root.child(warning_strip(
            if zh {
                "响应正文超过捕获上限，当前显示已捕获前缀。可点击「保存正文」或「发送并保存到文件」获取完整内容。"
            } else {
                "Response body exceeded capture limit. Use Send & Save to File for the full payload."
            },
            cx,
        ));
    }
    if let Some(saved_path) = &resp.saved_body_path {
        root = root.child(div().px(px(12.0)).pt(px(6.0)).child(alert_banner(
            Tone::Success,
            format!("完整正文已流式写入：{saved_path}"),
            None,
            cx,
        )));
    }

    let content: gpui_kit::AnyElement = match tab.http_resp_tab {
        HttpRespTab::Body => render_resp_body(view, zh, cx).into_any_element(),
        HttpRespTab::Headers => render_resp_headers(resp, zh, cx).into_any_element(),
        HttpRespTab::Cookies => render_resp_cookies(resp, zh, cx).into_any_element(),
        HttpRespTab::Trace => render_resp_trace(resp, zh, cx).into_any_element(),
        HttpRespTab::Tests => render_resp_tests(resp, zh, cx).into_any_element(),
    };

    let right_fmt =
        (tab.http_resp_tab == HttpRespTab::Body).then(|| format_controls.into_any_element());

    root.child(subtab_strip(tabs, right_fmt, cx))
        .child(div().flex_1().min_h_0().w_full().child(content))
        .into_any_element()
}

fn render_failure_card(
    fail: &RequestFailureInfo,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let tone = if fail.is_cancelled {
        Tone::Warning
    } else {
        Tone::Danger
    };
    let mut card_body = v_col()
        .p(px(14.0))
        .gap(px(8.0))
        .child(
            h_row()
                .justify_between()
                .child(badge(fail.kind_label.clone(), tone, cx))
                .children(fail.attempt_count.map(|n| {
                    badge(
                        if zh {
                            format!("已尝试 {n} 次")
                        } else {
                            format!("{n} attempts")
                        },
                        Tone::Neutral,
                        cx,
                    )
                })),
        )
        .child(
            div()
                .text_xs()
                .text_color(to_hsla(p.ink))
                .child(SharedString::from(fail.message.clone())),
        );

    if !fail.unresolved_keys.is_empty() {
        let mut chips = h_row().gap(px(6.0)).flex_wrap();
        for k in &fail.unresolved_keys {
            chips = chips.child(badge(format!("{{{{{k}}}}}"), Tone::Warning, cx));
        }
        card_body = card_body.child(chips);
    }
    if fail.history_unsaved {
        card_body = card_body.child(div().text_size(px(11.0)).text_color(to_hsla(p.warn)).child(
            if zh {
                "本次失败记录未能写入历史。"
            } else {
                "Failed to record this failure in history."
            },
        ));
    }

    card_body = card_body.child(
        h_row()
            .gap(px(8.0))
            .child(
                btn_secondary("fail-retry", if zh { "重试发送" } else { "Retry" }).on_click(
                    cx.listener(|this, _, window, cx| {
                        this.send_active_request(None, None, window, cx);
                    }),
                ),
            )
            .child(
                btn_ghost("fail-ask", if zh { "询问智能体" } else { "Ask Agent" })
                    .on_click(cx.listener(|this, _, _, cx| this.handoff_ask_agent_http(cx))),
            ),
    );

    v_col()
        .size_full()
        .p(px(16.0))
        .bg(to_hsla(p.surface))
        .overflow_y_scrollbar()
        .child(top_accent_card(tone, cx).child(card_body))
}

fn render_timing_popover(
    resp: &ExecutionResult,
    zh: bool,
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
                .w(px(250.0))
                .gap(px(5.0))
                .p(px(4.0))
                .child(section_heading(
                    if zh {
                        "阶段耗时拆解"
                    } else {
                        "Timing Breakdown"
                    },
                    pop_cx,
                ))
                .child(timing_row("DNS", t.dns_ms, pp.ph_dns))
                .child(timing_row(
                    if t.connect_includes_tls {
                        "Connect + TLS"
                    } else {
                        "Connect"
                    },
                    t.connect_ms,
                    pp.ph_connect,
                ))
                .child(timing_row("TLS", t.tls_ms, pp.ph_tls))
                .child(timing_row(
                    if zh {
                        "等待首包 (TTFB)"
                    } else {
                        "Wait (TTFB)"
                    },
                    PhaseValue::Ms(wait),
                    pp.ph_wait,
                ))
                .child(timing_row(
                    if zh { "接收正文" } else { "Receive" },
                    PhaseValue::Ms(recv),
                    pp.ph_recv,
                ))
        })
}

fn timing_row(
    label: &'static str,
    val: PhaseValue,
    color: pingdeck_desktop::theme::RgbaColor,
) -> gpui_kit::Div {
    let val_str = match val {
        PhaseValue::Ms(ms) => format!("{ms} ms"),
        PhaseValue::Reused => "Reused".into(),
        PhaseValue::NotNeeded => "—".into(),
        PhaseValue::NotMeasured => "N/A".into(),
    };
    h_row()
        .justify_between()
        .gap(px(8.0))
        .child(
            h_row()
                .gap(px(6.0))
                .child(div().size(px(7.0)).rounded(px(99.0)).bg(to_hsla(color)))
                .child(div().text_xs().child(label)),
        )
        .child(
            div()
                .text_xs()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .child(val_str),
        )
}

pub(super) fn warning_strip(msg: &str, cx: &Context<RequestsView>) -> gpui_kit::Div {
    div().w_full().px(px(12.0)).pt(px(6.0)).child(alert_banner(
        Tone::Warning,
        msg.to_owned(),
        None,
        cx,
    ))
}

fn format_label(fmt: ResponseBodyFormat) -> &'static str {
    match fmt {
        ResponseBodyFormat::Json => "JSON",
        ResponseBodyFormat::Xml => "XML",
        ResponseBodyFormat::Html => "HTML",
        ResponseBodyFormat::Yaml => "YAML",
        ResponseBodyFormat::Hex => "Hex",
        ResponseBodyFormat::Base64 => "Base64",
    }
}
