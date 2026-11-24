//! AI Response secondary tabs: Events, Raw, Headers, Timing, Tests, Usage/Cost, and Compare.

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use pingdeck_desktop::core::llm::ai_run::{AiResultDto, AiRunStatus};
use pingdeck_desktop::core::llm::capture_record::CapturedFrame;

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, badge, card, h_row, kv_chip, palette,
    section_heading, status_indicator, to_hsla, v_col,
};

use super::RequestsView;
use super::model::{AiRespTab, format_cost_short};

pub(super) fn render_ai_events(
    frames: &[CapturedFrame],
    cx: &Context<RequestsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let mut list = v_col()
        .size_full()
        .w_full()
        .p(px(12.0))
        .gap(px(3.0))
        .overflow_y_scrollbar();
    for f in frames.iter().take(400) {
        list = list.child(
            h_row()
                .w_full()
                .gap(px(8.0))
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(5.0))
                .bg(to_hsla(p.pane))
                .child(
                    div()
                        .w(px(36.0))
                        .text_xs()
                        .child(SharedString::from(format!("#{}", f.index))),
                )
                .child(
                    div()
                        .w(px(64.0))
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(format!("+{}ms", f.t_ms))),
                )
                .child(badge(f.event.clone(), Tone::Neutral, cx))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .truncate()
                        .child(SharedString::from(f.data.clone())),
                ),
        );
    }
    list.into_any_element()
}

pub(super) fn render_ai_raw(
    ai: &AiResultDto,
    zh: bool,
    cx: &Context<RequestsView>,
) -> impl IntoElement {
    let p = palette(cx);
    v_col()
        .size_full()
        .w_full()
        .p(px(12.0))
        .gap(px(10.0))
        .overflow_y_scrollbar()
        .child(
            card(cx).p(px(10.0)).child(
                v_col()
                    .gap(px(4.0))
                    .child(section_heading(
                        if zh {
                            "发送的请求（凭据已脱敏）"
                        } else {
                            "Sent Request"
                        },
                        cx,
                    ))
                    .child(div().text_xs().child(SharedString::from(format!(
                        "{} {}",
                        ai.request.method, ai.request.url
                    ))))
                    .children((!ai.dropped_headers.is_empty()).then(|| {
                        badge(
                            format!("核心丢弃受保护头: {}", ai.dropped_headers.join(", ")),
                            Tone::Warning,
                            cx,
                        )
                    }))
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .child(SharedString::from(ai.request.body.clone())),
                    ),
            ),
        )
        .children(
            ai.response_body
                .as_ref()
                .and_then(|b| b.text.as_ref())
                .map(|txt| {
                    card(cx).p(px(10.0)).child(
                        v_col()
                            .gap(px(4.0))
                            .child(section_heading(
                                if zh {
                                    "原始响应正文"
                                } else {
                                    "Raw Response"
                                },
                                cx,
                            ))
                            .child(div().text_xs().child(SharedString::from(txt.clone()))),
                    )
                }),
        )
}

pub(super) fn render_ai_headers(ai: &AiResultDto, cx: &Context<RequestsView>) -> impl IntoElement {
    let p = palette(cx);
    let mut list = v_col()
        .size_full()
        .w_full()
        .p(px(12.0))
        .gap(px(4.0))
        .overflow_y_scrollbar();
    for h in &ai.response_headers {
        list = list.child(
            h_row()
                .w_full()
                .gap(px(10.0))
                .px(px(8.0))
                .py(px(4.0))
                .rounded(px(5.0))
                .bg(to_hsla(p.pane))
                .child(
                    div()
                        .w(px(210.0))
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .truncate()
                        .child(SharedString::from(h.name.clone())),
                )
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .truncate()
                        .child(SharedString::from(h.value.clone())),
                ),
        );
    }
    list.into_any_element()
}

pub(super) fn render_ai_timing(
    ai: &AiResultDto,
    zh: bool,
    cx: &Context<RequestsView>,
) -> impl IntoElement {
    let tps = ai
        .tokens_per_second
        .map(|v| format!("{v:.1} tok/s"))
        .unwrap_or_else(|| "—".into());
    v_col()
        .size_full()
        .w_full()
        .p(px(12.0))
        .gap(px(8.0))
        .overflow_y_scrollbar()
        .child(
            card(cx).p(px(12.0)).child(
                v_col()
                    .gap(px(6.0))
                    .child(section_heading(
                        if zh { "流式阶段耗时" } else { "Timing" },
                        cx,
                    ))
                    .child(
                        h_row()
                            .gap(px(8.0))
                            .flex_wrap()
                            .child(kv_chip("Queue", format!("{} ms", ai.timing.queue_ms), cx))
                            .child(kv_chip(
                                "TTFT",
                                ai.ttft_ms
                                    .map(|m| format!("{m} ms"))
                                    .unwrap_or_else(|| "—".into()),
                                cx,
                            ))
                            .child(kv_chip(
                                "Stream",
                                ai.stream_ms
                                    .map(|m| format!("{m} ms"))
                                    .unwrap_or_else(|| "—".into()),
                                cx,
                            ))
                            .child(kv_chip("Total", format!("{} ms", ai.total_ms), cx))
                            .child(kv_chip(if zh { "吞吐" } else { "Speed" }, tps, cx)),
                    ),
            ),
        )
}

pub(super) fn render_ai_tests(
    ai: &AiResultDto,
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
    for a in &ai.assertion_results {
        col = col.child(
            h_row()
                .justify_between()
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .bg(to_hsla(if a.passed { p.ok_soft } else { p.bad_soft }))
                .child(badge(
                    if a.passed { "Pass" } else { "Fail" },
                    if a.passed {
                        Tone::Success
                    } else {
                        Tone::Danger
                    },
                    cx,
                ))
                .child(
                    div()
                        .flex_1()
                        .pl(px(8.0))
                        .text_xs()
                        .child(SharedString::from(a.label.clone())),
                )
                .child(
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(a.message.clone())),
                ),
        );
    }
    for t in &ai.script_tests {
        col = col.child(
            h_row()
                .justify_between()
                .px(px(10.0))
                .py(px(6.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.pane))
                .child(badge(
                    if t.passed { "Pass" } else { "Fail" },
                    if t.passed {
                        Tone::Success
                    } else {
                        Tone::Danger
                    },
                    cx,
                ))
                .child(
                    div()
                        .flex_1()
                        .pl(px(8.0))
                        .text_xs()
                        .child(SharedString::from(t.name.clone())),
                ),
        );
    }
    if ai.assertion_results.is_empty() && ai.script_tests.is_empty() {
        col = col.child(div().text_xs().text_color(to_hsla(p.muted)).child(if zh {
            "暂无测试结果"
        } else {
            "No test results"
        }));
    }
    col
}

pub(super) fn render_ai_usage(
    ai: &AiResultDto,
    zh: bool,
    cx: &Context<RequestsView>,
) -> impl IntoElement {
    let u = &ai.usage;
    let cost_s = ai
        .cost
        .as_ref()
        .map(format_cost_short)
        .unwrap_or_else(|| ai.cost_note.clone().unwrap_or_else(|| "—".into()));
    v_col()
        .size_full()
        .w_full()
        .p(px(12.0))
        .gap(px(8.0))
        .overflow_y_scrollbar()
        .child(
            card(cx).p(px(12.0)).child(
                v_col()
                    .gap(px(8.0))
                    .child(
                        h_row()
                            .justify_between()
                            .child(section_heading(
                                if zh {
                                    "Token 与费用估算"
                                } else {
                                    "Usage & Estimated Cost"
                                },
                                cx,
                            ))
                            .child(badge(cost_s, Tone::Accent, cx)),
                    )
                    .child(
                        h_row()
                            .gap(px(8.0))
                            .flex_wrap()
                            .child(kv_chip(
                                "Input",
                                u.input_tokens.unwrap_or(0).to_string(),
                                cx,
                            ))
                            .child(kv_chip(
                                "Output",
                                u.output_tokens.unwrap_or(0).to_string(),
                                cx,
                            ))
                            .child(kv_chip(
                                "Cached",
                                u.cached_tokens.unwrap_or(0).to_string(),
                                cx,
                            ))
                            .child(kv_chip(
                                "Reasoning",
                                u.reasoning_tokens.unwrap_or(0).to_string(),
                                cx,
                            ))
                            .child(kv_chip(
                                "Total",
                                u.total_tokens.unwrap_or(0).to_string(),
                                cx,
                            )),
                    ),
            ),
        )
}

pub(super) fn render_compare_pane(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::AnyElement {
    let p = palette(cx);
    let Some(tab) = view.active_req() else {
        return v_col().into_any_element();
    };
    let rows = tab
        .ai_compare_result
        .as_ref()
        .map(|r| r.rows.as_slice())
        .unwrap_or(tab.ai_compare_live_rows.as_slice());

    let mut list = v_col()
        .size_full()
        .w_full()
        .p(px(12.0))
        .gap(px(6.0))
        .overflow_y_scrollbar()
        .child(
            h_row()
                .justify_between()
                .child(section_heading(
                    if zh {
                        format!("多模型对比结果（{} 个模型）", rows.len())
                    } else {
                        format!("Model Comparison ({} models)", rows.len())
                    },
                    cx,
                ))
                .children(
                    tab.ai_compare_running
                        .then(|| status_indicator(StatusShape::Run, cx)),
                ),
        );

    for (idx, r) in rows.iter().enumerate() {
        let tone = match r.status {
            AiRunStatus::Done => Tone::Success,
            AiRunStatus::Skipped | AiRunStatus::Stopped => Tone::Warning,
            _ => Tone::Danger,
        };
        let row_res = r.result.clone();
        list = list.child(
            div()
                .id(SharedString::from(format!("cmp-row-{idx}")))
                .cursor_pointer()
                .p(px(10.0))
                .rounded(px(8.0))
                .bg(to_hsla(p.pane))
                .border_1()
                .border_color(to_hsla(p.line.composite_over(p.pane)))
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(res) = &row_res
                        && let Some(t) = this.active_req_mut()
                    {
                        t.last_ai = Some((**res).clone());
                        t.ai_resp_tab = AiRespTab::Merged;
                        cx.notify();
                    }
                }))
                .child(
                    v_col()
                        .gap(px(4.0))
                        .child(
                            h_row()
                                .justify_between()
                                .gap(px(6.0))
                                .child(
                                    h_row()
                                        .gap(px(6.0))
                                        .child(badge(r.status.label(), tone, cx))
                                        .child(
                                            div()
                                                .text_xs()
                                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                                .child(SharedString::from(format!(
                                                    "{} / {}",
                                                    r.provider_name, r.model
                                                ))),
                                        )
                                        .children(
                                            r.fastest.then(|| badge("最快", Tone::Accent, cx)),
                                        )
                                        .children(
                                            r.cheapest.then(|| badge("最省", Tone::Success, cx)),
                                        ),
                                )
                                .child(
                                    h_row()
                                        .gap(px(6.0))
                                        .children(
                                            r.ttft_ms
                                                .map(|m| kv_chip("TTFT", format!("{m}ms"), cx)),
                                        )
                                        .children(
                                            r.total_ms
                                                .map(|m| kv_chip("Total", format!("{m}ms"), cx)),
                                        )
                                        .children(r.cost.as_ref().map(|c| {
                                            badge(format_cost_short(c), Tone::Neutral, cx)
                                        })),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(SharedString::from(if r.reply_line.is_empty() {
                                    r.message.clone().unwrap_or_else(|| "—".into())
                                } else {
                                    r.reply_line.clone()
                                })),
                        ),
                ),
        );
    }
    list.into_any_element()
}
