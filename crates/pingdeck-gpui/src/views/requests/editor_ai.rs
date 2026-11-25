//! AI API Request 6-tab Editor (Messages, Params, Tools, Raw JSON, Assertions, Scripts).

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};

use crate::ui::{
    ScrollableElement as _, Tone, UiIcon, badge, btn_ghost, btn_icon, btn_secondary, card,
    code_editor, field_row, h_row, multiline_input, palette, section_heading, tab_bar_segmented,
    tab_bar_underline, tab_item, text_input, to_hsla, toggle_switch, v_col,
};

use super::RequestsView;
pub use super::editor_ai_bar::render_ai_url_strip;
use super::editor_auth_net::render_assertions_tab;
use super::editor_http::render_scripts_tab;
use super::model::AiEdTab;

pub fn render_ai_editor(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> impl IntoElement {
    let p = palette(cx);
    let Some(tab) = view.active_req() else {
        return v_col().size_full();
    };
    let ent = cx.entity().clone();
    let msg_cnt = tab.ai_messages.len();
    let assert_cnt = tab.draft.assertions.len();

    let est_tokens: usize = tab
        .ai_messages
        .iter()
        .map(|m| (m.content.len() / 4).max(1))
        .sum();
    let meta_hint = if zh {
        format!(
            "约 {est_tokens} 输入 Token · {}",
            if tab.ai_params.stream {
                "流式输出已开"
            } else {
                "非流式"
            }
        )
    } else {
        format!(
            "~{est_tokens} in tok · {}",
            if tab.ai_params.stream {
                "stream on"
            } else {
                "non-stream"
            }
        )
    };

    let tabs = tab_bar_underline("req-ai-ed-tabs")
        .selected_index(tab.ai_ed_tab.index())
        .child(tab_item(if zh {
            format!("消息 ({msg_cnt})")
        } else {
            format!("Messages ({msg_cnt})")
        }))
        .child(tab_item(if zh { "参数" } else { "Params" }))
        .child(tab_item(if zh { "工具" } else { "Tools" }))
        .child(tab_item("Raw JSON"))
        .child(tab_item(if zh {
            format!("断言 ({assert_cnt})")
        } else {
            format!("Assertions ({assert_cnt})")
        }))
        .child(tab_item(if zh { "脚本" } else { "Scripts" }))
        .on_click(move |idx, window, cx| {
            let next = AiEdTab::from_index(*idx);
            ent.update(cx, |this, cx| {
                this.snapshot_active_tab(cx);
                if next == AiEdTab::RawJson {
                    this.ai_sync_builder_to_raw(window, cx);
                }
                if let Some(t) = this.active_req_mut() {
                    t.ai_ed_tab = next;
                }
                cx.notify();
            });
        });

    let body: gpui_kit::AnyElement = match tab.ai_ed_tab {
        AiEdTab::Messages => render_ai_messages_tab(view, zh, cx).into_any_element(),
        AiEdTab::Params => render_ai_params_tab(view, zh, cx).into_any_element(),
        AiEdTab::Tools => render_ai_tools_tab(view, zh, cx).into_any_element(),
        AiEdTab::RawJson => v_col()
            .size_full()
            .p(px(10.0))
            .gap(px(6.0))
            .child(
                h_row()
                    .justify_between()
                    .child(section_heading("Wire JSON Payload", cx))
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(
                                btn_ghost(
                                    "ai-raw-from-form",
                                    if zh {
                                        "从消息生成"
                                    } else {
                                        "From builder"
                                    },
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.ai_sync_builder_to_raw(window, cx);
                                    },
                                )),
                            )
                            .child(
                                btn_ghost(
                                    "ai-raw-to-form",
                                    if zh {
                                        "解析回消息"
                                    } else {
                                        "Parse to builder"
                                    },
                                )
                                .on_click(cx.listener(
                                    |this, _, window, cx| {
                                        this.ai_sync_raw_to_builder(window, cx);
                                    },
                                )),
                            ),
                    ),
            )
            .child(
                div()
                    .flex_1()
                    .min_h_0()
                    .child(code_editor(&view.ai_raw_editor)),
            )
            .into_any_element(),
        AiEdTab::Assertions => render_assertions_tab(view, zh, true, cx).into_any_element(),
        AiEdTab::Scripts => render_scripts_tab(view, zh, true, cx).into_any_element(),
    };

    v_col()
        .size_full()
        .bg(to_hsla(p.surface))
        .child(
            h_row()
                .justify_between()
                .px(px(12.0))
                .pt(px(4.0))
                .border_b_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(tabs)
                .child(
                    div()
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(meta_hint)),
                ),
        )
        .child(div().flex_1().min_h_0().w_full().child(body))
}

fn render_ai_messages_tab(
    view: &RequestsView,
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
        .gap(px(8.0))
        .overflow_y_scrollbar();

    for (idx, msg) in tab.ai_messages.iter().enumerate() {
        let tone = match msg.role.as_str() {
            "user" => Tone::Accent,
            "assistant" => Tone::Success,
            _ => Tone::Neutral,
        };
        col = col.child(
            card(cx).p(px(8.0)).child(
                h_row()
                    .w_full()
                    .items_start()
                    .gap(px(8.0))
                    .child(
                        btn_ghost(SharedString::from(format!("ai-role-{idx}")), "")
                            .px(px(2.0))
                            .tooltip(if zh {
                                "点击切换角色 (system / user / assistant)"
                            } else {
                                "Click to cycle role"
                            })
                            .child(badge(format!("{} ▾", msg.role), tone, cx))
                            .on_click(cx.listener(move |this, _, window, cx| {
                                this.ai_cycle_message_role(idx, window, cx);
                            })),
                    )
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .text_color(to_hsla(p.ink))
                            .child(SharedString::from(msg.content.clone())),
                    )
                    .child(
                        btn_icon(
                            SharedString::from(format!("ai-msg-del-{idx}")),
                            UiIcon::Trash,
                        )
                        .on_click(cx.listener(
                            move |this, _, window, cx| {
                                this.ai_remove_message(idx, window, cx);
                            },
                        )),
                    ),
            ),
        );
    }

    col.child(
        h_row()
            .w_full()
            .items_end()
            .gap(px(8.0))
            .child(
                div()
                    .flex_1()
                    .h(px(62.0))
                    .child(multiline_input(&view.ai_msg_new_input)),
            )
            .child(
                btn_secondary(
                    "ai-msg-add",
                    if zh {
                        "+ 添加消息"
                    } else {
                        "+ Add message"
                    },
                )
                .on_click(cx.listener(|this, _, window, cx| this.ai_add_message(window, cx))),
            ),
    )
    .into_any_element()
}

fn render_ai_params_tab(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::AnyElement {
    let Some(tab) = view.active_req() else {
        return v_col().into_any_element();
    };
    let eff_idx = match tab.ai_params.reasoning_effort.as_str() {
        "low" => 1,
        "medium" => 2,
        "high" => 3,
        _ => 0,
    };
    let ent_e = cx.entity().clone();

    v_col()
        .size_full()
        .w_full()
        .p(px(12.0))
        .gap(px(6.0))
        .overflow_y_scrollbar()
        .child(field_row(
            "Temperature",
            None,
            div().w(px(120.0)).child(text_input(&view.ai_temp_input)),
            cx,
        ))
        .child(field_row(
            "Max Tokens",
            None,
            div().w(px(120.0)).child(text_input(&view.ai_max_tok_input)),
            cx,
        ))
        .child(field_row(
            "Top P",
            None,
            div().w(px(120.0)).child(text_input(&view.ai_top_p_input)),
            cx,
        ))
        .child(field_row(
            if zh {
                "流式输出 (stream)"
            } else {
                "Stream output"
            },
            None,
            toggle_switch("ai-stream-sw", tab.ai_params.stream).on_change(cx.listener(
                |this, c: &bool, window, cx| {
                    if let Some(t) = this.active_req_mut() {
                        t.ai_params.stream = *c;
                    }
                    this.ai_sync_builder_to_raw(window, cx);
                },
            )),
            cx,
        ))
        .child(field_row(
            if zh {
                "推理强度"
            } else {
                "Reasoning effort"
            },
            None,
            tab_bar_segmented("ai-effort-seg")
                .selected_index(eff_idx)
                .child(tab_item("Auto"))
                .child(tab_item("Low"))
                .child(tab_item("Medium"))
                .child(tab_item("High"))
                .on_click(move |idx, window, cx| {
                    ent_e.update(cx, |this, cx| {
                        if let Some(t) = this.active_req_mut() {
                            t.ai_params.reasoning_effort = match *idx {
                                1 => "low".into(),
                                2 => "medium".into(),
                                3 => "high".into(),
                                _ => String::new(),
                            };
                        }
                        this.ai_sync_builder_to_raw(window, cx);
                    });
                }),
            cx,
        ))
        .child(field_row(
            if zh {
                "保留完整交换到日志"
            } else {
                "Keep exchange in log"
            },
            Some(SharedString::from(if zh {
                "开启后本次调用载荷写入本地日志，可在「用量与日志」中复核"
            } else {
                "Captures this exchange in Usage & Logs regardless of global policy"
            })),
            toggle_switch("ai-keep-log-sw", tab.ai_params.keep_exchange).on_change(cx.listener(
                |this, c: &bool, _, cx| {
                    if let Some(t) = this.active_req_mut() {
                        t.ai_params.keep_exchange = *c;
                    }
                    cx.notify();
                },
            )),
            cx,
        ))
        .into_any_element()
}

fn render_ai_tools_tab(
    view: &RequestsView,
    zh: bool,
    cx: &mut Context<RequestsView>,
) -> gpui_kit::Div {
    v_col()
        .size_full()
        .p(px(10.0))
        .gap(px(8.0))
        .child(
            h_row()
                .justify_between()
                .child(section_heading(
                    if zh {
                        "Function Calling 工具定义（JSON 数组）"
                    } else {
                        "Tool Definitions (JSON array)"
                    },
                    cx,
                ))
                .child(
                    btn_secondary(
                        "ai-tool-sample",
                        if zh {
                            "+ 插入示例工具"
                        } else {
                            "+ Insert sample tool"
                        },
                    )
                    .on_click(cx.listener(|this, _, window, cx| {
                        this.ai_insert_sample_tool(window, cx);
                    })),
                ),
        )
        .child(
            div()
                .flex_1()
                .min_h_0()
                .w_full()
                .child(code_editor(&view.ai_tools_editor)),
        )
}
