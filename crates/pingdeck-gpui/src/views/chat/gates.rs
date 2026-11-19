//! Floating top-accented Approval Gate and Cost Limit Pause cards docked above the Chat composer.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use pingdeck_desktop::core::agent::model::{ApprovalDecision, ApprovalScope};
use pingdeck_desktop::state::{LiveApproval, LiveCostPause};

use crate::ui::{
    StatusShape, Tone, badge, btn_danger, btn_primary, btn_secondary, h_row, palette,
    status_indicator, to_hsla, top_accent_card, v_col,
};

use super::ChatView;

pub fn render_approval_gate(
    appr: &LiveApproval,
    zh: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let tone = if appr.destructive {
        Tone::Danger
    } else {
        Tone::Warning
    };
    let run_once = appr.run_id.clone();
    let call_once = appr.call_id.clone();
    let run_sess = appr.run_id.clone();
    let call_sess = appr.call_id.clone();
    let run_deny = appr.run_id.clone();
    let call_deny = appr.call_id.clone();
    let preview = appr
        .full_text
        .as_deref()
        .filter(|s| !s.trim().is_empty())
        .unwrap_or(appr.arguments.as_str());

    top_accent_card(tone, cx).child(
        v_col()
            .p(px(12.0))
            .gap(px(8.0))
            .child(
                h_row()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        h_row()
                            .gap(px(7.0))
                            .child(status_indicator(
                                if appr.destructive {
                                    StatusShape::Bad
                                } else {
                                    StatusShape::Warn
                                },
                                cx,
                            ))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(to_hsla(p.ink))
                                    .child(SharedString::from(appr.summary.clone())),
                            ),
                    )
                    .child(
                        h_row()
                            .gap(px(4.0))
                            .child(badge(appr.name.clone(), Tone::Neutral, cx))
                            .child(badge(appr.risk.clone(), tone, cx)),
                    ),
            )
            .children((!preview.trim().is_empty()).then(|| {
                div()
                    .px(px(9.0))
                    .py(px(6.0))
                    .rounded(px(6.0))
                    .bg(to_hsla(p.sunken))
                    .text_xs()
                    .text_color(to_hsla(p.ink_2))
                    .child(SharedString::from(preview.trim().to_owned()))
            }))
            .child(
                h_row()
                    .justify_between()
                    .gap(px(8.0))
                    .child(
                        div()
                            .flex_1()
                            .text_xs()
                            .text_color(to_hsla(p.muted))
                            .truncate()
                            .child(SharedString::from(appr.reason.clone())),
                    )
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(
                                btn_danger(
                                    SharedString::from(format!("appr-deny-{call_deny}")),
                                    if zh { "拒绝" } else { "Deny" },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.answer_approval(
                                            run_deny.clone(),
                                            call_deny.clone(),
                                            ApprovalDecision::Deny,
                                            ApprovalScope::Once,
                                            cx,
                                        );
                                    },
                                )),
                            )
                            .child(
                                btn_secondary(
                                    SharedString::from(format!("appr-sess-{call_sess}")),
                                    if zh {
                                        "本对话允许"
                                    } else {
                                        "Allow in chat"
                                    },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.answer_approval(
                                            run_sess.clone(),
                                            call_sess.clone(),
                                            ApprovalDecision::Allow,
                                            ApprovalScope::Session,
                                            cx,
                                        );
                                    },
                                )),
                            )
                            .child(
                                btn_primary(
                                    SharedString::from(format!("appr-once-{call_once}")),
                                    if zh { "允许一次" } else { "Allow once" },
                                )
                                .on_click(cx.listener(
                                    move |this, _, _, cx| {
                                        this.answer_approval(
                                            run_once.clone(),
                                            call_once.clone(),
                                            ApprovalDecision::Allow,
                                            ApprovalScope::Once,
                                            cx,
                                        );
                                    },
                                )),
                            ),
                    ),
            ),
    )
}

pub fn render_cost_pause(
    pause: &LiveCostPause,
    zh: bool,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let rid_go = pause.run_id.clone();
    let rid_stop = pause.run_id.clone();
    let desc = if pause.is_tokens {
        format!("Tokens: {} / {}", pause.cost_micro, pause.cap_micro)
    } else {
        format!(
            "{} {:.4} / {:.4}",
            pause.currency,
            pause.cost_micro as f64 / 1_000_000.0,
            pause.cap_micro as f64 / 1_000_000.0
        )
    };
    top_accent_card(Tone::Warning, cx).child(
        h_row()
            .justify_between()
            .p(px(10.0))
            .gap(px(10.0))
            .child(
                h_row()
                    .gap(px(8.0))
                    .child(status_indicator(StatusShape::Warn, cx))
                    .child(
                        v_col()
                            .gap(px(1.0))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(to_hsla(p.ink))
                                    .child(if zh {
                                        "达到单次运行预算上限，已暂停"
                                    } else {
                                        "Run paused at budget limit"
                                    }),
                            )
                            .child(
                                div()
                                    .text_xs()
                                    .text_color(to_hsla(p.muted))
                                    .child(SharedString::from(desc)),
                            ),
                    ),
            )
            .child(
                h_row()
                    .gap(px(6.0))
                    .child(
                        btn_secondary("cost-stop", if zh { "停止" } else { "Stop" }).on_click(
                            cx.listener(move |this, _, _, cx| {
                                this.continue_cost_pause(rid_stop.clone(), false, cx);
                            }),
                        ),
                    )
                    .child(
                        btn_primary("cost-continue", if zh { "继续" } else { "Continue" })
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.continue_cost_pause(rid_go.clone(), true, cx);
                            })),
                    ),
            ),
    )
}
