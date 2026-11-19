//! Bottom floating dock for AI Chat: top-accented pending approval gate cards, cost pause
//! banner, and rounded composer card with native anchored popovers (permission mode, web
//! access, reasoning effort, context progress meter, real model selector, circular send/stop).

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::agent::model::ReasoningEffort;
use pingdeck_desktop::core::model::settings::ToolApproval;

use crate::ui::{
    Anchor, ScrollableElement as _, Tone, UiIcon, badge, borderless_multiline, h_row, icon_xs,
    list_row, palette, pill_btn, popover, progress_bar, round_icon_btn, section_heading, to_hsla,
    v_col,
};

use super::ChatView;
use super::gates::{render_approval_gate, render_cost_pause};

pub fn render_dock(view: &ChatView, zh: bool, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let running = view.is_run_active();

    let mut dock = v_col()
        .w_full()
        .max_w(px(760.0))
        .mx_auto()
        .gap(px(8.0))
        .px(px(20.0))
        .pt(px(6.0))
        .pb(px(14.0));

    // 1. Floating top-accented Approval Gate cards docked directly above the composer.
    for appr in &view.thread.pending_approvals {
        dock = dock.child(render_approval_gate(appr, zh, cx));
    }

    // 2. Floating Cost/Token Pause card if the run hit a limit.
    if let Some(pause) = &view.thread.cost_pause {
        dock = dock.child(render_cost_pause(pause, zh, cx));
    }

    // 3. Rounded 16px Composer Card with seamless borderless Textarea + native Popover bar.
    let perm_mode = view
        .session
        .as_ref()
        .map(|s| s.permission_mode)
        .unwrap_or_default();
    let web_on = view.session.as_ref().is_some_and(|s| s.web_enabled);
    let effort = view.session.as_ref().and_then(|s| s.reasoning_effort);
    let (active_model_label, no_model_configured) =
        match view.session.as_ref().and_then(|s| s.target.as_ref()) {
            Some(t) => (view.format_target_label(t), false),
            None => match view.available_models.first() {
                Some(m) => (
                    if zh {
                        format!("自动 · {}", m.display_label())
                    } else {
                        format!("Auto · {}", m.display_label())
                    },
                    false,
                ),
                None => (
                    if zh {
                        "未配置模型".into()
                    } else {
                        "No model".into()
                    },
                    true,
                ),
            },
        };
    let missing_reasoning_quirk = view.active_provider_missing_reasoning_quirk();

    let composer_card = v_col()
        .w_full()
        .rounded(px(16.0))
        .bg(to_hsla(p.surface))
        .border_1()
        .border_color(to_hsla(p.line_strong.composite_over(p.surface)))
        .shadow_sm()
        .px(px(12.0))
        .pt(px(8.0))
        .pb(px(8.0))
        .gap(px(6.0))
        .child(borderless_multiline(&view.composer))
        .child(
            h_row()
                .justify_between()
                .gap(px(8.0))
                .pt(px(4.0))
                .border_t_1()
                .border_color(to_hsla(p.line.composite_over(p.surface)))
                .child(
                    h_row()
                        .gap(px(4.0))
                        .flex_wrap()
                        .child(render_perm_popover(perm_mode, zh, cx))
                        .child(
                            pill_btn(
                                "chat-web-toggle",
                                if zh { "联网" } else { "Web" },
                                Some(UiIcon::Globe),
                                web_on,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_web_enabled(cx))),
                        )
                        .child(render_effort_popover(
                            effort,
                            missing_reasoning_quirk,
                            zh,
                            cx,
                        )),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(render_context_meter(view, cx))
                        .child(render_model_popover(
                            view,
                            active_model_label,
                            no_model_configured,
                            zh,
                            cx,
                        ))
                        .child(if running {
                            round_icon_btn("chat-stop", UiIcon::Stop, Tone::Danger)
                                .tooltip(if zh { "停止生成" } else { "Stop run" })
                                .on_click(cx.listener(|this, _, _, cx| this.stop_active_run(cx)))
                        } else {
                            round_icon_btn("chat-send", UiIcon::ArrowUp, Tone::Accent)
                                .tooltip(if zh { "发送 (Enter)" } else { "Send (Enter)" })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.send_message(window, cx);
                                }))
                        }),
                ),
        );

    dock.child(composer_card)
}

fn render_perm_popover(
    mode: ToolApproval,
    zh: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let (label, glyph) = match mode {
        ToolApproval::Ask => (if zh { "每次询问" } else { "Ask" }, UiIcon::Shield),
        ToolApproval::AutoReadOnly => (
            if zh { "只读自动" } else { "Auto reads" },
            UiIcon::ShieldCheck,
        ),
        ToolApproval::AutoAll => (
            if zh { "完全权限" } else { "Full access" },
            UiIcon::ShieldAlert,
        ),
    };
    let chat_ref = cx.entity().clone();
    let options: [(ToolApproval, &str, &str); 3] = [
        (
            ToolApproval::Ask,
            if zh { "每次询问" } else { "Ask every time" },
            if zh {
                "所有工具调用前均需你确认"
            } else {
                "Confirm before every tool call"
            },
        ),
        (
            ToolApproval::AutoReadOnly,
            if zh { "只读自动" } else { "Auto read-only" },
            if zh {
                "读取类自动放行，写入和执行仍需审批"
            } else {
                "Auto-allow reads; ask for edits and exec"
            },
        ),
        (
            ToolApproval::AutoAll,
            if zh { "完全权限" } else { "Full access" },
            if zh {
                "自动放行常规工具（硬阻止与 OS 沙箱保持开启）"
            } else {
                "Auto-run tools (hard blocks & OS sandbox stay on)"
            },
        ),
    ];

    popover("chat-perm-popover")
        .anchor(Anchor::BottomLeft)
        .trigger(pill_btn(
            "chat-perm-trigger",
            label,
            Some(glyph),
            mode == ToolApproval::AutoAll,
        ))
        .content(move |_st, _window, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut box_el =
                v_col()
                    .w(px(270.0))
                    .gap(px(4.0))
                    .child(div().px(px(6.0)).pb(px(2.0)).child(section_heading(
                        if zh {
                            "工具权限模式"
                        } else {
                            "Permission mode"
                        },
                        pop_cx,
                    )));
            for (opt, title, desc) in options {
                let selected = opt == mode;
                let entity = chat_ref.clone();
                let pop_handle = pop_cx.entity().clone();
                box_el = box_el.child(
                    list_row(
                        SharedString::from(format!("perm-opt-{opt:?}")),
                        selected,
                        pop_cx,
                        move |_, window, cx| {
                            entity.update(cx, |this, cx| this.set_permission_mode(opt, cx));
                            pop_handle.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .child(
                        v_col()
                            .flex_1()
                            .gap(px(1.0))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::MEDIUM)
                                    .text_color(to_hsla(pop_p.ink))
                                    .child(title),
                            )
                            .child(div().text_xs().text_color(to_hsla(pop_p.muted)).child(desc)),
                    )
                    .children(selected.then(|| {
                        div()
                            .text_color(to_hsla(pop_p.accent))
                            .child(icon_xs(UiIcon::Check))
                    })),
                );
            }
            box_el
        })
}

fn render_effort_popover(
    effort: Option<ReasoningEffort>,
    missing_reasoning_quirk: bool,
    zh: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let label = match effort {
        None => {
            if zh {
                "推理: 自动"
            } else {
                "Effort: Auto"
            }
        }
        Some(ReasoningEffort::Minimal) => {
            if zh {
                "推理: 极简"
            } else {
                "Effort: Minimal"
            }
        }
        Some(ReasoningEffort::Low) => {
            if zh {
                "推理: 低"
            } else {
                "Effort: Low"
            }
        }
        Some(ReasoningEffort::Medium) => {
            if zh {
                "推理: 中"
            } else {
                "Effort: Medium"
            }
        }
        Some(ReasoningEffort::High) => {
            if zh {
                "推理: 高"
            } else {
                "Effort: High"
            }
        }
        Some(ReasoningEffort::Max) => {
            if zh {
                "推理: 最高"
            } else {
                "Effort: Max"
            }
        }
    };
    let chat_ref = cx.entity().clone();
    let opts: [(Option<ReasoningEffort>, &str); 6] = [
        (
            None,
            if zh {
                "自动 (模型默认)"
            } else {
                "Auto (model default)"
            },
        ),
        (
            Some(ReasoningEffort::Minimal),
            if zh { "极简" } else { "Minimal" },
        ),
        (Some(ReasoningEffort::Low), if zh { "低" } else { "Low" }),
        (
            Some(ReasoningEffort::Medium),
            if zh { "中" } else { "Medium" },
        ),
        (Some(ReasoningEffort::High), if zh { "高" } else { "High" }),
        (Some(ReasoningEffort::Max), if zh { "最高" } else { "Max" }),
    ];

    popover("chat-effort-popover")
        .anchor(Anchor::BottomLeft)
        .trigger(pill_btn(
            "chat-effort-trigger",
            label,
            Some(UiIcon::Brain),
            effort.is_some(),
        ))
        .content(move |_st, _window, pop_cx| {
            let pop_p = palette(pop_cx);
            let mut list = v_col().w(px(230.0)).gap(px(2.0));
            for (val, text) in opts {
                let selected = val == effort;
                let entity = chat_ref.clone();
                let pop_handle = pop_cx.entity().clone();
                list = list.child(
                    list_row(
                        SharedString::from(format!("effort-opt-{val:?}")),
                        selected,
                        pop_cx,
                        move |_, window, cx| {
                            entity.update(cx, |this, cx| this.set_reasoning_effort(val, cx));
                            pop_handle.update(cx, |st, cx| st.dismiss(window, cx));
                        },
                    )
                    .justify_between()
                    .child(div().text_xs().child(text))
                    .children(selected.then(|| {
                        div()
                            .text_color(to_hsla(pop_p.accent))
                            .child(icon_xs(UiIcon::Check))
                    })),
                );
            }
            if effort.is_none() && !missing_reasoning_quirk {
                list = list.child(
                    div()
                        .mt(px(4.0))
                        .pt(px(6.0))
                        .px(px(6.0))
                        .border_t_1()
                        .border_color(to_hsla(pop_p.line.composite_over(pop_p.surface)))
                        .text_size(px(11.0))
                        .text_color(to_hsla(pop_p.muted))
                        .child(if zh {
                            "自动模式不主动传思考参数（Claude 等默认不思考）；如需查看思考过程，建议选「中」或「高」。"
                        } else {
                            "Auto sends no thinking param (models like Claude skip thinking); pick Medium or High to view reasoning."
                        }),
                );
            }
            if missing_reasoning_quirk {
                list = list.child(
                    div()
                        .mt(px(4.0))
                        .pt(px(6.0))
                        .px(px(6.0))
                        .border_t_1()
                        .border_color(to_hsla(pop_p.line.composite_over(pop_p.surface)))
                        .text_size(px(11.0))
                        .text_color(to_hsla(pop_p.muted))
                        .child(if zh {
                            "当前自定义服务商未设置「推理参数格式」，可在「服务商 › 配置」开启以请求思考过程。"
                        } else {
                            "Custom provider has no reasoning format set; configure it in Providers › Config."
                        }),
                );
            }
            list
        })
}

fn render_model_popover(
    view: &ChatView,
    model_label: String,
    highlight_missing: bool,
    zh: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let models = view.available_models.clone();
    let current_target = view.session.as_ref().and_then(|s| s.target.clone());
    let chat_ref = cx.entity().clone();
    let count = models.len();

    popover("chat-model-popover")
        .anchor(Anchor::BottomRight)
        .trigger(
            pill_btn(
                "chat-model-trigger",
                SharedString::from(model_label),
                Some(if highlight_missing {
                    UiIcon::Warning
                } else {
                    UiIcon::Sparkles
                }),
                highlight_missing,
            )
            .max_w(px(280.0)),
        )
        .content(move |_st, _window, pop_cx| {
            let pop_p = palette(pop_cx);
            let header = h_row()
                .flex_shrink_0()
                .justify_between()
                .px(px(8.0))
                .pb(px(6.0))
                .border_b_1()
                .border_color(to_hsla(pop_p.line.composite_over(pop_p.surface)))
                .child(section_heading(
                    if zh {
                        "选择对话模型"
                    } else {
                        "Select model"
                    },
                    pop_cx,
                ))
                .children((count > 0).then(|| badge(count.to_string(), Tone::Neutral, pop_cx)));

            let body: gpui_kit::AnyElement = if models.is_empty() {
                div()
                    .px(px(8.0))
                    .py(px(12.0))
                    .text_xs()
                    .text_color(to_hsla(pop_p.muted))
                    .child(if zh {
                        "暂无已配置模型，请先前往 AI 服务商添加"
                    } else {
                        "No configured models yet — add one in Providers"
                    })
                    .into_any_element()
            } else {
                let scroll_h = ((models.len() as f32) * 32.0 + 8.0).clamp(64.0, 320.0);
                let mut rows = v_col().w_full().gap(px(2.0)).py(px(4.0));
                for (i, m) in models.iter().enumerate() {
                    let target = m.target();
                    let selected = current_target.as_ref() == Some(&target);
                    let entity = chat_ref.clone();
                    let pop_handle = pop_cx.entity().clone();
                    let pname = m.provider_name.clone();
                    let mid = m.model_id.clone();
                    rows = rows.child(
                        list_row(
                            SharedString::from(format!("model-opt-{i}")),
                            selected,
                            pop_cx,
                            move |_, window, cx| {
                                let t = target.clone();
                                entity.update(cx, |this, cx| {
                                    this.select_model(t, cx);
                                });
                                pop_handle.update(cx, |st, cx| st.dismiss(window, cx));
                            },
                        )
                        .justify_between()
                        .gap(px(8.0))
                        .child(
                            h_row()
                                .flex_1()
                                .min_w_0()
                                .gap(px(4.0))
                                .child(
                                    div()
                                        .flex_shrink_0()
                                        .text_xs()
                                        .text_color(to_hsla(pop_p.muted))
                                        .child(SharedString::from(format!("{pname} /"))),
                                )
                                .child(
                                    div()
                                        .flex_1()
                                        .min_w_0()
                                        .text_xs()
                                        .font_weight(if selected {
                                            gpui_kit::FontWeight::SEMIBOLD
                                        } else {
                                            gpui_kit::FontWeight::MEDIUM
                                        })
                                        .text_color(to_hsla(pop_p.ink))
                                        .truncate()
                                        .child(SharedString::from(mid)),
                                ),
                        )
                        .children(selected.then(|| {
                            div()
                                .flex_shrink_0()
                                .text_color(to_hsla(pop_p.accent))
                                .child(icon_xs(UiIcon::Check))
                        })),
                    );
                }
                div()
                    .w_full()
                    .h(px(scroll_h))
                    .overflow_y_scrollbar()
                    .id("chat-model-pop-scroll")
                    .child(rows)
                    .into_any_element()
            };

            v_col().w(px(340.0)).gap(px(2.0)).child(header).child(body)
        })
}

fn render_context_meter(view: &ChatView, cx: &gpui_kit::App) -> impl IntoElement {
    let p = palette(cx);
    let used = view.thread.context_used_tokens;
    let limit = view.thread.context_limit_tokens;
    let label = if let Some(lim) = limit.filter(|&l| l > 0) {
        let pct = ((used as f64 / lim as f64) * 100.0).min(100.0) as f32;
        return h_row()
            .gap(px(5.0))
            .child(div().w(px(44.0)).child(progress_bar("ctx-mini-bar", pct)))
            .child(
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(SharedString::from(format!("{:.0}%", pct))),
            );
    } else if used >= 1000 {
        format!("{:.1}k", used as f64 / 1000.0)
    } else {
        format!("{used} tok")
    };

    h_row().child(
        div()
            .text_xs()
            .text_color(to_hsla(p.faint))
            .child(SharedString::from(label)),
    )
}
