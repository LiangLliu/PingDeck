//! AI Response pane: streaming progress, status header, merged reply / reasoning / tool-calls
//! / SSE frame chip strip inspector.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::llm::ai_run::{AiResultDto, AiRunStatus, CostNoteCode};
use pingdeck_desktop::services::clipboard;

use crate::ui::{
    ScrollableElement as _, StatusShape, Tone, UiIcon, alert_banner, badge, btn_ghost, btn_icon,
    btn_primary, btn_secondary, card, choice_chip, empty_state_with_actions, h_row, icon_xs,
    kbd_chip, kv_chip, markdown_view, palette, section_heading, status_indicator, subtab_strip,
    tab_bar_underline, tab_item, to_hsla, v_col,
};

use super::RequestsView;
use super::model::{AiRespTab, RequestsHandoff, format_cost_short};
use super::resp_ai_tabs::{
    render_ai_events, render_ai_headers, render_ai_raw, render_ai_tests, render_ai_timing,
    render_ai_usage, render_compare_pane,
};

pub fn render_ai_response(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::AnyElement {
    let p = palette(cx);
    let Some(tab) = view.active_req() else {
        return v_col().size_full().into_any_element();
    };

    if tab.ai_resp_tab == AiRespTab::Compare
        && (tab.ai_compare_running
            || tab.ai_compare_result.is_some()
            || !tab.ai_compare_live_rows.is_empty())
    {
        return render_compare_pane(view, zh, cx);
    }

    // Live streaming view while AI run is active before `Done`.
    if tab.sending && tab.last_ai.is_none() {
        let count = tab.ai_live_frames.len();
        let recent_start = count.saturating_sub(3);
        let recent: Vec<(u32, u64, String, String)> = tab.ai_live_frames[recent_start..]
            .iter()
            .map(|f| (f.index, f.t_ms, f.event.clone(), f.data.clone()))
            .collect();
        let mut live = v_col()
            .size_full()
            .p(px(14.0))
            .gap(px(10.0))
            .bg(to_hsla(p.surface))
            .child(
                h_row()
                    .gap(px(8.0))
                    .child(status_indicator(StatusShape::Run, cx))
                    .child(div().text_xs().child(SharedString::from(if zh {
                        format!("正在接收流式响应… 已收到 {count} 帧")
                    } else {
                        format!("Streaming response… {count} frames received")
                    }))),
            );
        if !recent.is_empty() {
            let mut frames_col = v_col().gap(px(4.0));
            for (idx, t_ms, ev, data) in recent {
                frames_col = frames_col.child(
                    h_row()
                        .gap(px(6.0))
                        .child(badge(format!("#{idx} +{t_ms}ms {ev}"), Tone::Neutral, cx))
                        .child(
                            div()
                                .flex_1()
                                .min_w_0()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .truncate()
                                .child(SharedString::from(data)),
                        ),
                );
            }
            live = live.child(card(cx).p(px(10.0)).child(frames_col));
        }
        return live.into_any_element();
    }

    let Some(ai) = &tab.last_ai else {
        if let Some(fail) = &tab.failure {
            return v_col()
                .size_full()
                .p(px(16.0))
                .bg(to_hsla(p.surface))
                .child(
                    card(cx).p(px(12.0)).child(
                        v_col()
                            .gap(px(6.0))
                            .child(badge(fail.kind_label.clone(), Tone::Danger, cx))
                            .child(
                                div()
                                    .text_xs()
                                    .child(SharedString::from(fail.message.clone())),
                            ),
                    ),
                )
                .into_any_element();
        }
        return empty_state_with_actions(
            UiIcon::Sparkles,
            if zh {
                "准备调试 AI 协议响应"
            } else {
                "Ready to inspect AI response"
            },
            if zh {
                "支持流式 SSE 帧逐帧查看、推理过程折叠、工具调用检视、费用估算与多模型横向对比。"
            } else {
                "Stream SSE frames, inspect reasoning and tool calls, estimate cost, and compare models."
            },
            Some(
                h_row()
                    .gap(px(8.0))
                    .child(
                        btn_primary("ai-resp-empty-send", if zh { "立即发送" } else { "Send now" })
                            .icon(icon_xs(UiIcon::Send))
                            .on_click(cx.listener(|this, _, window, cx| {
                                this.send_active_ai_request(None, window, cx);
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

    let status_tone = match ai.status {
        AiRunStatus::Done => Tone::Success,
        AiRunStatus::Stopped | AiRunStatus::TimedOut | AiRunStatus::Skipped => Tone::Warning,
        _ => Tone::Danger,
    };
    let status_lbl = match ai.http_status {
        Some(code) => format!("{code} {}", ai.status.label()),
        None => ai.status.label().to_owned(),
    };

    let tok_str = format!(
        "{}{} / {}",
        if ai.usage.estimated { "~" } else { "" },
        ai.usage.input_tokens.unwrap_or(0),
        ai.usage.output_tokens.unwrap_or(0)
    );
    let cost_str =
        ai.cost
            .as_ref()
            .map(format_cost_short)
            .unwrap_or_else(|| match ai.cost_note_code {
                Some(CostNoteCode::LocalFree) => if zh { "本地免费" } else { "Local free" }.into(),
                Some(CostNoteCode::NoPrice) => if zh { "未知单价" } else { "No price" }.into(),
                _ => "—".into(),
            });

    let log_req_id = ai.log_request_id.clone();
    let total_checks = ai.assertion_results.len() + ai.script_tests.len();
    let passed_checks = ai.assertion_results.iter().filter(|a| a.passed).count()
        + ai.script_tests.iter().filter(|t| t.passed).count();

    let head = h_row()
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
                .gap(px(6.0))
                .flex_wrap()
                .child(badge(status_lbl, status_tone, cx))
                .children((total_checks > 0).then(|| {
                    badge(
                        if zh {
                            format!("{passed_checks}/{total_checks} 断言")
                        } else {
                            format!("{passed_checks}/{total_checks} checks")
                        },
                        if passed_checks == total_checks {
                            Tone::Success
                        } else {
                            Tone::Danger
                        },
                        cx,
                    )
                }))
                .children(
                    ai.ttft_ms.map(|ms| {
                        kv_chip(if zh { "首字" } else { "TTFT" }, format!("{ms} ms"), cx)
                    }),
                )
                .child(kv_chip(
                    if zh { "总耗时" } else { "Total" },
                    format!("{} ms", ai.total_ms),
                    cx,
                ))
                .child(kv_chip("Token", tok_str, cx))
                .child(kv_chip(if zh { "费用" } else { "Cost" }, cost_str, cx))
                .children(
                    ai.streamed
                        .then(|| badge(format!("SSE · {} 帧", ai.event_count), Tone::Neutral, cx)),
                ),
        )
        .child(
            h_row()
                .gap(px(4.0))
                .flex_shrink_0()
                .children(log_req_id.map(|id| {
                    btn_ghost(
                        "ai-open-log",
                        if zh {
                            "在日志中打开 ↗"
                        } else {
                            "Open log ↗"
                        },
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.emit_handoff(RequestsHandoff::OpenUsageCall(id.clone()), cx);
                    }))
                }))
                .child(
                    btn_secondary("ai-ask-agent", if zh { "询问智能体" } else { "Ask Agent" })
                        .icon(icon_xs(UiIcon::Sparkles))
                        .on_click(cx.listener(|this, _, _, cx| this.handoff_ask_agent_ai(cx))),
                )
                .child(
                    btn_icon("ai-copy-reply", UiIcon::Copy)
                        .tooltip(if zh {
                            "复制合并回复"
                        } else {
                            "Copy reply"
                        })
                        .on_click(cx.listener(|this, _, _, cx| {
                            if let Some(r) = this.active_req().and_then(|t| t.last_ai.as_ref()) {
                                let _ = clipboard::set_text(&r.reply);
                                this.status_msg = Some((Tone::Success, "已复制 AI 回复".into()));
                                cx.notify();
                            }
                        })),
                ),
        );

    let ent = cx.entity().clone();
    let has_compare = tab.ai_compare_result.is_some() || !tab.ai_compare_live_rows.is_empty();
    let mut tabs = tab_bar_underline("req-ai-resp-tabs")
        .selected_index(tab.ai_resp_tab.index())
        .child(tab_item(if zh { "合并回复" } else { "Merged" }))
        .child(tab_item(if zh {
            format!("事件 ({})", ai.frames.frames.len())
        } else {
            format!("Events ({})", ai.frames.frames.len())
        }))
        .child(tab_item("Raw"))
        .child(tab_item(if zh {
            format!("响应头 ({})", ai.response_headers.len())
        } else {
            format!("Headers ({})", ai.response_headers.len())
        }))
        .child(tab_item(if zh { "耗时" } else { "Timing" }))
        .child(tab_item(if total_checks > 0 {
            if zh {
                format!("测试 ({passed_checks}/{total_checks})")
            } else {
                format!("Tests ({passed_checks}/{total_checks})")
            }
        } else {
            if zh { "测试" } else { "Tests" }.to_owned()
        }))
        .child(tab_item(if zh { "用量与费用" } else { "Usage" }));
    if has_compare {
        tabs = tabs.child(tab_item(if zh { "对比" } else { "Compare" }));
    }
    let tabs = tabs.on_click(move |idx, _, cx| {
        let next = AiRespTab::from_index(*idx);
        ent.update(cx, |this, cx| {
            if let Some(t) = this.active_req_mut() {
                t.ai_resp_tab = next;
            }
            cx.notify();
        });
    });

    let body: gpui_kit::AnyElement = match tab.ai_resp_tab {
        AiRespTab::Merged => render_ai_merged(view, ai, zh, cx).into_any_element(),
        AiRespTab::Events => render_ai_events(&ai.frames.frames, cx).into_any_element(),
        AiRespTab::Raw => render_ai_raw(ai, zh, cx).into_any_element(),
        AiRespTab::Headers => render_ai_headers(ai, cx).into_any_element(),
        AiRespTab::Timing => render_ai_timing(ai, zh, cx).into_any_element(),
        AiRespTab::Tests => render_ai_tests(ai, zh, cx).into_any_element(),
        AiRespTab::Usage => render_ai_usage(ai, zh, cx).into_any_element(),
        AiRespTab::Compare => render_compare_pane(view, zh, cx).into_any_element(),
    };

    v_col()
        .size_full()
        .bg(to_hsla(p.surface))
        .child(head)
        .child(subtab_strip(tabs, None, cx))
        .child(div().flex_1().min_h_0().w_full().child(body))
        .into_any_element()
}

fn render_ai_merged(
    view: &RequestsView,
    ai: &AiResultDto,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::AnyElement {
    let p = palette(cx);
    let Some(tab) = view.active_req() else {
        return v_col().into_any_element();
    };
    let mut col = v_col()
        .size_full()
        .w_full()
        .p(px(12.0))
        .gap(px(10.0))
        .overflow_y_scrollbar();

    if let Some(msg) = ai.message.as_ref().or(ai.stream_error.as_ref()) {
        col = col.child(alert_banner(Tone::Danger, msg.clone(), None, cx));
    }

    // Collapsible Reasoning / Thinking block
    if !ai.reasoning.is_empty() {
        let open = tab.ai_reasoning_open;
        let mut rbox = card(cx).p(px(10.0)).child(
            v_col()
                .gap(px(6.0))
                .child(
                    h_row()
                        .justify_between()
                        .child(h_row().gap(px(6.0)).child(icon_xs(UiIcon::Brain)).child(
                            section_heading(
                                if zh {
                                    "推理思考过程"
                                } else {
                                    "Reasoning / Thinking"
                                },
                                cx,
                            ),
                        ))
                        .child(
                            btn_ghost(
                                "ai-reason-tog",
                                if open {
                                    if zh { "收起" } else { "Collapse" }
                                } else if zh {
                                    "展开"
                                } else {
                                    "Expand"
                                },
                            )
                            .on_click(cx.listener(|this, _, _, cx| {
                                if let Some(t) = this.active_req_mut() {
                                    t.ai_reasoning_open = !t.ai_reasoning_open;
                                }
                                cx.notify();
                            })),
                        ),
                )
                .children(open.then(|| {
                    div()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(ai.reasoning.clone()))
                })),
        );
        let _ = &mut rbox;
        col = col.child(rbox);
    }

    // Merged reply box
    let finish = ai.finish_reason.as_deref().unwrap_or("—");
    col = col.child(
        card(cx).p(px(12.0)).child(
            v_col()
                .gap(px(8.0))
                .child(
                    h_row().justify_between().child(
                        h_row()
                            .gap(px(6.0))
                            .child(badge("assistant", Tone::Accent, cx))
                            .child(div().text_xs().text_color(to_hsla(p.muted)).child(
                                SharedString::from(format!(
                                    "{} · finish_reason: {finish}",
                                    ai.response_model.as_deref().unwrap_or(&ai.model)
                                )),
                            )),
                    ),
                )
                .child(markdown_view(
                    "ai-merged-md",
                    if ai.reply.is_empty() {
                        ai.notice.as_deref().unwrap_or("_(empty reply)_")
                    } else {
                        &ai.reply
                    },
                )),
        ),
    );

    // Tool Calls
    if !ai.tool_calls.is_empty() {
        let mut tc_col = v_col().gap(px(6.0)).child(section_heading(
            if zh {
                format!("工具调用 ({})", ai.tool_calls.len())
            } else {
                format!("Tool Calls ({})", ai.tool_calls.len())
            },
            cx,
        ));
        for tc in &ai.tool_calls {
            tc_col = tc_col.child(
                card(cx).p(px(8.0)).child(
                    v_col()
                        .gap(px(4.0))
                        .child(
                            h_row()
                                .gap(px(6.0))
                                .child(badge(tc.name.clone(), Tone::Accent, cx))
                                .child(
                                    div()
                                        .text_size(px(11.0))
                                        .text_color(to_hsla(p.faint))
                                        .child(SharedString::from(tc.id.clone())),
                                ),
                        )
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.ink_2))
                                .child(SharedString::from(tc.arguments.clone())),
                        ),
                ),
            );
        }
        col = col.child(tc_col);
    }

    // Embedding Vectors
    if !ai.vectors.is_empty() {
        let mut vcol = v_col().gap(px(4.0)).child(section_heading(
            if zh {
                "向量输出"
            } else {
                "Embedding Vectors"
            },
            cx,
        ));
        for (i, v) in ai.vectors.iter().enumerate() {
            let preview: Vec<String> = v.first_values.iter().map(|n| format!("{n:.4}")).collect();
            vcol = vcol.child(card(cx).p(px(8.0)).child(div().text_xs().child(
                SharedString::from(format!(
                    "#{i} ({} dims): [{}, …]",
                    v.dimensions,
                    preview.join(", ")
                )),
            )));
        }
        col = col.child(vcol);
    }

    // Interactive SSE Frame Chip Strip + Inspector
    if !ai.frames.frames.is_empty() {
        let selected_idx = tab.ai_selected_frame.unwrap_or(0);
        let mut chips = h_row().gap(px(4.0)).flex_wrap();
        for (i, frame) in ai.frames.frames.iter().take(24).enumerate() {
            let sel = i == selected_idx;
            chips = chips.child(
                choice_chip(
                    SharedString::from(format!("sse-chip-{i}")),
                    format!("#{} +{}ms", frame.index, frame.t_ms),
                    sel,
                    cx,
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    if let Some(t) = this.active_req_mut() {
                        t.ai_selected_frame = Some(i);
                    }
                    cx.notify();
                })),
            );
        }
        let inspected = ai.frames.frames.get(selected_idx);
        col = col.child(
            card(cx).p(px(10.0)).child(
                v_col()
                    .gap(px(6.0))
                    .child(section_heading(
                        if zh {
                            "SSE 帧（点击查看单帧载荷）"
                        } else {
                            "SSE Frames (click chip to inspect)"
                        },
                        cx,
                    ))
                    .child(chips)
                    .children(inspected.map(|f| {
                        v_col()
                            .gap(px(3.0))
                            .p(px(8.0))
                            .rounded(px(6.0))
                            .bg(to_hsla(p.sunken))
                            .child(
                                div()
                                    .text_size(px(11.0))
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(to_hsla(p.accent_ink))
                                    .child(SharedString::from(format!(
                                        "Frame #{} · +{} ms · {}",
                                        f.index, f.t_ms, f.event
                                    ))),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(to_hsla(p.ink))
                                    .child(SharedString::from(f.data.clone())),
                            )
                    })),
            ),
        );
    }
    col.into_any_element()
}
