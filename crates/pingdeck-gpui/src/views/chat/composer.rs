//! Bottom floating dock for AI Chat: open gates (AskUser / Plan), pending approval cards,
//! cost pause, queued turns bar, live StatusLine, attachment/skill chips, and rounded composer.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, borderless_multiline, btn_danger, btn_ghost, btn_icon, h_row,
    icon_xs, palette, pill_btn, round_icon_btn, status_indicator, to_hsla, v_col,
};

use super::ChatView;
use super::composer_popovers::{
    render_context_meter, render_effort_popover, render_model_popover, render_perm_popover,
    render_skills_popover,
};
use super::gates::{render_approval_gate, render_human_gate};
use super::gates_extra::{render_cost_pause, render_queued_bar};

pub fn render_dock(view: &ChatView, zh: bool, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let running = view.is_run_active();

    let mut dock = v_col()
        .w_full()
        .max_w(px(1080.0))
        .mx_auto()
        .gap(px(8.0))
        .px(px(16.0))
        .pt(px(6.0))
        .pb(px(12.0));

    for (run_id, gate) in &view.thread.open_gates {
        dock = dock.child(render_human_gate(view, run_id, gate, zh, cx));
    }
    for appr in &view.thread.pending_approvals {
        dock = dock.child(render_approval_gate(appr, zh, cx));
    }
    if let Some(pause) = &view.thread.cost_pause {
        dock = dock.child(render_cost_pause(pause, zh, cx));
    }
    if !view.thread.queued_turns.is_empty() {
        dock = dock.child(render_queued_bar(view, zh, cx));
    }

    if running {
        let elapsed_s = view.run_started_at.map_or(0, |t| t.elapsed().as_secs());
        let steps = view.thread.active_run.as_ref().map_or(0, |r| r.iterations);
        let phase =
            if !view.thread.pending_approvals.is_empty() || !view.thread.open_gates.is_empty() {
                if zh {
                    "等待你确认"
                } else {
                    "Waiting for you"
                }
            } else if !view.thread.tool_calls.is_empty()
                && view.thread.tool_calls.iter().any(|t| t.status.is_none())
            {
                if zh {
                    "正在调用工具…"
                } else {
                    "Running tools…"
                }
            } else if !view.thread.streaming_reasoning.is_empty()
                && view.thread.streaming_text.is_empty()
            {
                if zh {
                    "正在深度思考…"
                } else {
                    "Thinking…"
                }
            } else if zh {
                "正在生成回复…"
            } else {
                "Generating…"
            };
        dock = dock.child(
            h_row()
                .justify_between()
                .px(px(10.0))
                .py(px(5.0))
                .rounded(px(8.0))
                .bg(to_hsla(p.pane))
                .child(
                    h_row()
                        .gap(px(8.0))
                        .child(status_indicator(StatusShape::Run, cx))
                        .child(div().text_xs().text_color(to_hsla(p.ink)).child(phase))
                        .child(
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.muted))
                                .child(format!("Step {steps} · {elapsed_s}s")),
                        )
                        .children(view.plan_mode.then(|| {
                            badge(if zh { "计划模式" } else { "Plan" }, Tone::Accent, cx)
                        })),
                )
                .child(
                    btn_danger("chat-status-stop", if zh { "停止运行" } else { "Stop run" })
                        .icon(icon_xs(UiIcon::Stop))
                        .on_click(cx.listener(|this, _, _, cx| this.stop_active_run(cx))),
                ),
        );
    }

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

    let mut composer_card = v_col()
        .w_full()
        .rounded(px(16.0))
        .bg(to_hsla(p.surface))
        .border_1()
        .border_color(to_hsla(p.line_strong.composite_over(p.surface)))
        .shadow_sm()
        .px(px(12.0))
        .pt(px(8.0))
        .pb(px(8.0))
        .gap(px(6.0));

    if !view.pending_attachments.is_empty() || !view.selected_skills.is_empty() {
        let mut chips = h_row().gap(px(4.0)).flex_wrap();
        for att in &view.pending_attachments {
            let aid = att.id.clone();
            chips = chips.child(
                h_row()
                    .gap(px(4.0))
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(6.0))
                    .bg(to_hsla(p.sunken))
                    .child(icon_xs(UiIcon::FileText))
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.ink))
                            .child(att.name.clone()),
                    )
                    .child(
                        btn_icon(
                            SharedString::from(format!("chat-att-rm-{}", att.id)),
                            UiIcon::Close,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.pending_attachments.retain(|a| a.id != aid);
                            cx.notify();
                        })),
                    ),
            );
        }
        for sk in &view.selected_skills {
            let sname = sk.clone();
            chips = chips.child(
                h_row()
                    .gap(px(4.0))
                    .px(px(6.0))
                    .py(px(2.0))
                    .rounded(px(6.0))
                    .bg(to_hsla(p.accent_soft))
                    .child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(p.accent_ink))
                            .child(format!("/{sk}")),
                    )
                    .child(
                        btn_icon(
                            SharedString::from(format!("chat-sk-rm-{sk}")),
                            UiIcon::Close,
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.selected_skills.retain(|s| s != &sname);
                            cx.notify();
                        })),
                    ),
            );
        }
        composer_card = composer_card.child(chips);
    }

    composer_card = composer_card
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
                        .child(
                            btn_icon("chat-attach-btn", UiIcon::Plus)
                                .tooltip(if zh {
                                    "添加附件文件"
                                } else {
                                    "Attach files"
                                })
                                .on_click(cx.listener(|this, _, _, cx| this.pick_attachments(cx))),
                        )
                        .child(render_skills_popover(view, zh, cx))
                        .child(
                            pill_btn(
                                "chat-plan-toggle",
                                if zh { "计划" } else { "Plan" },
                                Some(UiIcon::ListChecks),
                                view.plan_mode,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_plan_mode(cx))),
                        )
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
                            h_row()
                                .gap(px(4.0))
                                .child(
                                    btn_ghost("chat-queue-send", if zh { "排队" } else { "Queue" })
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.send_message(window, cx);
                                        })),
                                )
                                .child(
                                    round_icon_btn("chat-steer", UiIcon::ArrowUp, Tone::Accent)
                                        .tooltip(if zh {
                                            "立即插队引导 (⌘Enter)"
                                        } else {
                                            "Steer run now (⌘Enter)"
                                        })
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.steer_active_run(window, cx);
                                        })),
                                )
                                .into_any_element()
                        } else {
                            round_icon_btn("chat-send", UiIcon::ArrowUp, Tone::Accent)
                                .tooltip(if zh { "发送 (Enter)" } else { "Send (Enter)" })
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.send_message(window, cx);
                                }))
                                .into_any_element()
                        }),
                ),
        );

    dock.child(composer_card)
}
