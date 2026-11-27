//! HTTP Response header, status/timing waterfall popover, error/warning banners, and tab dispatch.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::app::current_time_ms;
use pingdeck_desktop::core::executor::BodyIncomplete;
use pingdeck_desktop::services::clipboard;
use pingdeck_desktop::time::format_relative_age;

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, alert_banner, badge, btn_ghost, btn_icon,
    btn_primary, btn_secondary, empty_state_with_actions, h_row, icon_xs, kbd_chip, kv_chip,
    palette, payload_format_picker, status_indicator, subtab_strip, tab_bar_underline, tab_item,
    to_hsla, top_accent_card, v_col,
};

use super::RequestsView;
use super::model::{HttpRespTab, RequestFailureInfo, format_bytes};
use super::resp_http_tabs::{
    render_resp_body, render_resp_cookies, render_resp_headers, render_resp_tests,
    render_resp_trace,
};
use super::resp_http_timing::render_timing_popover;

pub fn render_http_response(
    view: &RequestsView,
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
            .child(
                div().text_xs().text_color(to_hsla(p.muted)).child(
                    crate::ui::tr(cx)
                        .exchange()
                        .resp_http_waiting_for_response_click_stop(),
                ),
            )
            .into_any_element();
    }

    if let Some(fail) = &tab.failure
        && tab.last_http.is_none()
    {
        return render_failure_card(fail, cx).into_any_element();
    }

    let Some(resp) = &tab.last_http else {
        return empty_state_with_actions(
            UiIcon::Send,
            crate::ui::tr(cx).exchange().resp_http_ready_to_send(),
            crate::ui::tr(cx)
                .exchange()
                .resp_http_inspect_status_timing_waterfall_headers(),
            Some(
                h_row()
                    .gap(px(8.0))
                    .child(
                        btn_primary(
                            "resp-empty-send",
                            crate::ui::tr(cx).exchange().resp_ai_send_now(),
                        )
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
        .flex_shrink_0()
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
                .child(render_timing_popover(resp, cx))
                .child(kv_chip(
                    crate::ui::tr(cx).exchange().view_size(),
                    size_text,
                    cx,
                ))
                .child(badge(proto_text, Tone::Neutral, cx))
                .children(tab.last_http_at_ms.map(|ts| {
                    let lang = crate::ui::active_language(cx);
                    let age = format_relative_age(ts, current_time_ms(), lang);
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(age))
                }))
                .children(has_check_fail.then(|| {
                    badge(
                        crate::ui::tr(cx)
                            .exchange()
                            .resp_http_checks_failed(tests_passed, tests_total),
                        Tone::Danger,
                        cx,
                    )
                }))
                .children((!resp.history_saved).then(|| {
                    badge(
                        crate::ui::tr(cx).exchange().resp_http_not_in_history(),
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
                    btn_ghost(
                        "resp-save-file",
                        crate::ui::tr(cx).exchange().resp_http_save(),
                    )
                    .icon(icon_xs(UiIcon::Download))
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.save_response_body_to_file(cx);
                    })),
                )
                .child(
                    btn_secondary(
                        "resp-ask-agent",
                        crate::ui::tr(cx).exchange().resp_ai_ask_agent(),
                    )
                    .icon(icon_xs(UiIcon::Sparkles))
                    .on_click(cx.listener(|this, _, _, cx| this.handoff_ask_agent_http(cx))),
                )
                .child(
                    btn_icon("resp-copy-body", UiIcon::Copy)
                        .tooltip(crate::ui::tr(cx).exchange().resp_http_copy_response())
                        .on_click(cx.listener(|this, _, _, cx| {
                            let txt = this.response_editor.read(cx).value().to_string();
                            let _ = clipboard::set_text(&txt);
                            this.status_msg = Some((
                                Tone::Success,
                                crate::ui::tr(cx).exchange().resp_http_copied_body().into(),
                            ));
                            cx.notify();
                        })),
                ),
        );

    // 2. Response Sub-tabs + Pretty/Raw & Format Picker
    let ent_t = cx.entity().clone();
    let test_lbl = if tests_total > 0 {
        crate::ui::tr(cx)
            .exchange()
            .resp_http_tests(tests_passed, tests_total)
    } else {
        crate::ui::tr(cx).exchange().resp_ai_tests_2().into()
    };

    let tabs = tab_bar_underline("req-http-resp-tabs")
        .selected_index(tab.http_resp_tab.index())
        .child(tab_item(crate::ui::tr(cx).exchange().resp_http_body()))
        .child(tab_item(
            crate::ui::tr(cx)
                .exchange()
                .resp_ai_headers(resp.headers.len()),
        ))
        .child(tab_item(format!("Cookie ({})", resp.cookies.len())))
        .child(tab_item(crate::ui::tr(cx).exchange().trace_region()))
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
    let cur_wrap = tab.http_soft_wrap;
    let cur_fmt = tab.http_format;
    let ent_pr = cx.entity().clone();
    let ent_fm = cx.entity().clone();
    let ent_wr = cx.entity().clone();

    let format_controls = payload_format_picker(
        "resp",
        cur_fmt,
        cur_pretty,
        cur_wrap,
        move |pretty, window, cx| {
            ent_pr.update(cx, |this, cx| {
                this.set_http_response_format(cur_fmt, pretty, window, cx);
            });
        },
        move |fmt, window, cx| {
            ent_fm.update(cx, |this, cx| {
                this.set_http_response_format(fmt, true, window, cx);
            });
        },
        move |window, cx| {
            ent_wr.update(cx, |this, cx| {
                this.toggle_http_response_wrap(window, cx);
            });
        },
        cx,
    );

    let mut root = v_col().size_full().bg(to_hsla(p.surface)).child(resp_head);

    // Optional banners for incomplete / truncated / saved-to-file / failed retry.
    if let Some(reason) = resp.body_incomplete {
        let msg = match reason {
            BodyIncomplete::IdleTimeout => crate::ui::tr(cx)
                .exchange()
                .resp_http_body_ended_early_idle_timeout(),
            BodyIncomplete::TotalTimeout => crate::ui::tr(cx)
                .exchange()
                .resp_http_body_ended_early_total_timeout(),
            BodyIncomplete::Cancelled => crate::ui::tr(cx)
                .exchange()
                .resp_http_stopped_by_user_after_headers(),
        };
        root = root.child(warning_strip(msg, cx));
    } else if resp.body_truncated {
        root = root.child(warning_strip(
            crate::ui::tr(cx)
                .exchange()
                .resp_http_response_body_exceeded_capture_limit(),
            cx,
        ));
    }
    if let Some(saved_path) = &resp.saved_body_path {
        root = root.child(
            div().px(px(12.0)).pt(px(6.0)).child(alert_banner(
                Tone::Success,
                crate::ui::tr(cx)
                    .exchange()
                    .resp_http_streamed_to(saved_path),
                None,
                cx,
            )),
        );
    }

    let content: gpui_kit::AnyElement = match tab.http_resp_tab {
        HttpRespTab::Body => render_resp_body(view, cx).into_any_element(),
        HttpRespTab::Headers => render_resp_headers(resp, cx).into_any_element(),
        HttpRespTab::Cookies => render_resp_cookies(resp, cx).into_any_element(),
        HttpRespTab::Trace => render_resp_trace(resp, cx).into_any_element(),
        HttpRespTab::Tests => render_resp_tests(resp, cx).into_any_element(),
    };

    let right_fmt =
        (tab.http_resp_tab == HttpRespTab::Body).then(|| format_controls.into_any_element());

    root.child(subtab_strip(tabs, right_fmt, cx))
        .child(v_col().flex_1().min_h_0().w_full().child(content))
        .into_any_element()
}

fn render_failure_card(
    fail: &RequestFailureInfo,
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
                        crate::ui::tr(cx).exchange().resp_http_attempts(n),
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
        card_body = card_body.child(
            div().text_size(px(11.0)).text_color(to_hsla(p.warn)).child(
                crate::ui::tr(cx)
                    .exchange()
                    .resp_http_failed_to_record_this_failure(),
            ),
        );
    }

    card_body = card_body.child(
        h_row()
            .gap(px(8.0))
            .child(
                btn_secondary("fail-retry", crate::ui::tr(cx).exchange().resp_http_retry())
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.send_active_request(None, None, window, cx);
                    })),
            )
            .child(
                btn_ghost("fail-ask", crate::ui::tr(cx).exchange().resp_ai_ask_agent())
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

pub(super) fn warning_strip(msg: &str, cx: &Context<RequestsView>) -> gpui_kit::Div {
    div().w_full().px(px(12.0)).pt(px(6.0)).child(alert_banner(
        Tone::Warning,
        msg.to_owned(),
        None,
        cx,
    ))
}
