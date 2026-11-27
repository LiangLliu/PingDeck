//! Cost Limit Pause card and Queued Turns bar docked above the Chat composer.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::state::LiveCostPause;

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, btn_danger, btn_ghost, btn_primary, h_row, icon_xs, palette,
    status_indicator, to_hsla, top_accent_card, v_col,
};

use super::ChatView;

pub fn render_cost_pause(pause: &LiveCostPause, cx: &mut Context<ChatView>) -> impl IntoElement {
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
                                    .child(
                                        crate::ui::tr(cx)
                                            .chat()
                                            .gates_extra_run_paused_at_budget_limit(),
                                    ),
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
                        btn_danger("cost-stop", crate::ui::tr(cx).chat().gates_extra_stop_run())
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.continue_cost_pause(rid_stop.clone(), false, cx);
                            })),
                    )
                    .child(
                        btn_primary(
                            "cost-continue",
                            crate::ui::tr(cx).chat().gates_extra_continue(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.continue_cost_pause(rid_go.clone(), true, cx);
                        })),
                    ),
            ),
    )
}

pub fn render_queued_bar(view: &ChatView, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let mut box_el = v_col()
        .w_full()
        .rounded(px(10.0))
        .bg(to_hsla(p.pane))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .p(px(8.0))
        .gap(px(5.0))
        .child(
            h_row()
                .justify_between()
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(badge(
                            crate::ui::tr(cx)
                                .chat()
                                .gates_extra_queued(view.thread.queued_turns.len()),
                            if view.thread.queue_paused {
                                Tone::Warning
                            } else {
                                Tone::Accent
                            },
                            cx,
                        ))
                        .children(view.thread.queue_paused.then(|| {
                            div()
                                .text_xs()
                                .text_color(to_hsla(p.warn))
                                .child(crate::ui::tr(cx).chat().gates_extra_paused())
                        })),
                )
                .child(
                    h_row()
                        .gap(px(4.0))
                        .children(view.thread.queue_paused.then(|| {
                            btn_primary(
                                "chat-q-resume",
                                crate::ui::tr(cx).chat().gates_extra_resume(),
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.queue_resume(cx)))
                        }))
                        .child(
                            btn_ghost("chat-q-clear", crate::ui::tr(cx).chat().queue_clear())
                                .on_click(cx.listener(|this, _, _, cx| this.queue_clear(cx))),
                        ),
                ),
        );

    for (i, q) in view.thread.queued_turns.iter().enumerate() {
        let qid_st = q.id.clone();
        let qid_rm = q.id.clone();
        let preview: String = q
            .parts
            .iter()
            .filter_map(|part| match part {
                spanbay_desktop::core::model::hub::HubPart::Text { text, .. } => {
                    Some(text.as_str())
                }
                _ => None,
            })
            .collect::<Vec<_>>()
            .join(" ");
        box_el = box_el.child(
            h_row()
                .gap(px(6.0))
                .px(px(6.0))
                .py(px(4.0))
                .rounded(px(6.0))
                .bg(to_hsla(p.surface))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .text_xs()
                        .text_color(to_hsla(p.ink))
                        .truncate()
                        .child(format!("#{} {preview}", i + 1)),
                )
                .child(
                    btn_ghost(
                        SharedString::from(format!("chat-q-steer-{}", q.id)),
                        crate::ui::tr(cx).chat().gates_extra_steer_now(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.queue_steer(qid_st.clone(), cx);
                    })),
                )
                .child(
                    btn_ghost(SharedString::from(format!("chat-q-rm-{}", q.id)), "")
                        .icon(icon_xs(UiIcon::Close))
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.queue_remove(qid_rm.clone(), cx);
                        })),
                ),
        );
    }
    box_el
}
