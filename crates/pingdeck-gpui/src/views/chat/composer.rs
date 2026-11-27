//! Bottom floating dock for AI Chat: open gates (AskUser / Plan), pending approval cards,
//! cost pause, queued turns bar, live StatusLine, attachment/skill chips, and rounded composer.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};

use crate::ui::{
    Anchor, Disableable as _, StatusShape, Tone, UiIcon, badge, borderless_multiline, btn_danger,
    btn_ghost, btn_icon, btn_secondary, h_row, icon_xs, palette, pill_btn, popover,
    popover_trigger_btn, progress_bar, round_icon_btn, section_heading, status_indicator, to_hsla,
    v_col,
};

use super::ChatView;
use super::composer_popovers::{
    render_effort_popover, render_model_popover, render_perm_popover, render_skills_popover,
};
use super::gates::{render_approval_gate, render_human_gate};
use super::gates_extra::{render_cost_pause, render_queued_bar};

pub fn render_dock(view: &ChatView, cx: &mut Context<ChatView>) -> impl IntoElement {
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
        dock = dock.child(render_human_gate(view, run_id, gate, cx));
    }
    for appr in &view.thread.pending_approvals {
        dock = dock.child(render_approval_gate(appr, cx));
    }
    if let Some(pause) = &view.thread.cost_pause {
        dock = dock.child(render_cost_pause(pause, cx));
    }
    if !view.thread.queued_turns.is_empty() {
        dock = dock.child(render_queued_bar(view, cx));
    }

    if running {
        let elapsed_s = view.run_started_at.map_or(0, |t| t.elapsed().as_secs());
        let steps = view.thread.active_run.as_ref().map_or(0, |r| r.iterations);
        let phase =
            if !view.thread.pending_approvals.is_empty() || !view.thread.open_gates.is_empty() {
                crate::ui::tr(cx).chat().composer_waiting_for_you()
            } else if !view.thread.tool_calls.is_empty()
                && view.thread.tool_calls.iter().any(|t| t.status.is_none())
            {
                crate::ui::tr(cx).chat().composer_running_tools()
            } else if !view.thread.streaming_reasoning.is_empty()
                && view.thread.streaming_text.is_empty()
            {
                crate::ui::tr(cx).chat().composer_thinking()
            } else {
                crate::ui::tr(cx).chat().composer_generating()
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
                            badge(crate::ui::tr(cx).chat().composer_plan(), Tone::Accent, cx)
                        })),
                )
                .child(
                    btn_danger("chat-status-stop", crate::ui::tr(cx).chat().card_stop())
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
                    crate::ui::tr(cx).chat().composer_auto(m.display_label()),
                    false,
                ),
                None => (crate::ui::tr(cx).chat().composer_no_model().into(), true),
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
                                .tooltip(crate::ui::tr(cx).chat().composer_attach_files())
                                .on_click(cx.listener(|this, _, _, cx| this.pick_attachments(cx))),
                        )
                        .child(render_skills_popover(view, cx))
                        .child(
                            pill_btn(
                                "chat-plan-toggle",
                                crate::ui::tr(cx).chat().plan_region(),
                                Some(UiIcon::ListChecks),
                                view.plan_mode,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_plan_mode(cx))),
                        )
                        .child(render_perm_popover(perm_mode, cx))
                        .child(
                            pill_btn(
                                "chat-web-toggle",
                                crate::ui::tr(cx).chat().composer_web(),
                                Some(UiIcon::Globe),
                                web_on,
                            )
                            .on_click(cx.listener(|this, _, _, cx| this.toggle_web_enabled(cx))),
                        )
                        .child(render_effort_popover(effort, missing_reasoning_quirk, cx)),
                )
                .child(
                    h_row()
                        .gap(px(6.0))
                        .child(render_context_meter(view, cx))
                        .child(render_model_popover(
                            view,
                            active_model_label,
                            no_model_configured,
                            cx,
                        ))
                        .child(if running {
                            h_row()
                                .gap(px(4.0))
                                .child(
                                    btn_ghost(
                                        "chat-queue-send",
                                        crate::ui::tr(cx).common().wire_queue(),
                                    )
                                    .on_click(cx.listener(
                                        |this, _, window, cx| {
                                            this.send_message(window, cx);
                                        },
                                    )),
                                )
                                .child(
                                    round_icon_btn("chat-steer", UiIcon::ArrowUp, Tone::Accent)
                                        .tooltip(
                                            crate::ui::tr(cx).chat().composer_steer_run_now_enter(),
                                        )
                                        .on_click(cx.listener(|this, _, window, cx| {
                                            this.steer_active_run(window, cx);
                                        })),
                                )
                                .into_any_element()
                        } else {
                            round_icon_btn("chat-send", UiIcon::ArrowUp, Tone::Accent)
                                .tooltip(crate::ui::tr(cx).chat().composer_send_enter())
                                .on_click(cx.listener(|this, _, window, cx| {
                                    this.send_message(window, cx);
                                }))
                                .into_any_element()
                        }),
                ),
        );

    dock.child(composer_card)
}

fn render_context_meter(view: &ChatView, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let used = view.thread.context_used_tokens;
    let limit = view.thread.context_limit_tokens;
    let cost_usd = view.thread.cost_micro_usd as f64 / 1_000_000.0;
    let currency = view
        .thread
        .cost_currency
        .clone()
        .unwrap_or_else(|| "USD".to_owned());
    let can_compact = !view.thread.session_id.is_empty() && !view.is_run_active();
    let sess_usage = view.session.as_ref().map(|s| {
        (
            s.total_usage.input_tokens.unwrap_or(0),
            s.total_usage.output_tokens.unwrap_or(0),
            s.total_usage.cached_input_tokens.unwrap_or(0),
        )
    });
    let notices = view.thread.notices.clone();
    let chat_ent = cx.entity().clone();

    let trigger_body = if let Some(lim) = limit.filter(|&l| l > 0) {
        let pct = ((used as f64 / lim as f64) * 100.0).min(100.0) as f32;
        h_row()
            .gap(px(5.0))
            .child(div().w(px(44.0)).child(progress_bar("ctx-mini-bar", pct)))
            .child(
                div()
                    .text_xs()
                    .text_color(to_hsla(p.muted))
                    .child(SharedString::from(format!("{pct:.0}%"))),
            )
    } else {
        let label = if used >= 1000 {
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
    };

    popover("chat-context-popover")
        .anchor(Anchor::BottomRight)
        .trigger(popover_trigger_btn("chat-context-trigger", trigger_body))
        .content(move |_, _, pop_cx| {
            let pp = palette(pop_cx);
            let pch = crate::ui::tr(pop_cx).chat();
            let ent = chat_ent.clone();
            let pop_h = pop_cx.entity().clone();
            let mut box_el = v_col()
                .w(px(270.0))
                .gap(px(8.0))
                .child(section_heading(pch.inspector_tabs_context_window(), pop_cx))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(pp.ink))
                        .child(SharedString::from(match limit {
                            Some(l) if l > 0 => format!("{used} / {l} tokens"),
                            _ => format!("{used} tokens"),
                        })),
                )
                .children(limit.filter(|&l| l > 0).map(|l| {
                    let pct = ((used as f64 / l as f64) * 100.0).min(100.0) as f32;
                    progress_bar("ctx-pop-bar", pct)
                }))
                .child(
                    btn_secondary("chat-ctx-compact", pch.inspector_tabs_compact_context_now())
                        .disabled(!can_compact)
                        .on_click(move |_, w, cx| {
                            ent.update(cx, |this, cx| this.compact_context(cx));
                            pop_h.update(cx, |s, cx| s.dismiss(w, cx));
                        }),
                );

            if let Some((in_tok, out_tok, cached)) = sess_usage {
                box_el = box_el.child(
                    v_col()
                        .gap(px(4.0))
                        .pt(px(6.0))
                        .border_t_1()
                        .border_color(to_hsla(pp.line.composite_over(pp.overlay)))
                        .child(section_heading(pch.inspector_tabs_session_totals(), pop_cx))
                        .child(ctx_metric_row(
                            pch.inspector_tabs_input_tokens(),
                            super::metrics::format_tokens_int(in_tok),
                            pop_cx,
                        ))
                        .child(ctx_metric_row(
                            pch.inspector_tabs_output_tokens(),
                            super::metrics::format_tokens_int(out_tok),
                            pop_cx,
                        ))
                        .child(ctx_metric_row(
                            pch.inspector_tabs_cached_input(),
                            super::metrics::format_tokens_int(cached),
                            pop_cx,
                        ))
                        .child(ctx_metric_row(
                            pch.inspector_tabs_estimated_cost(),
                            format!("{currency} {cost_usd:.4}"),
                            pop_cx,
                        )),
                );
            }
            if !notices.is_empty() {
                let mut n_col = v_col()
                    .gap(px(3.0))
                    .pt(px(6.0))
                    .border_t_1()
                    .border_color(to_hsla(pp.line.composite_over(pp.overlay)))
                    .child(section_heading(pch.inspector_tabs_notices(), pop_cx));
                for n in &notices {
                    n_col = n_col.child(
                        div()
                            .text_xs()
                            .text_color(to_hsla(pp.muted))
                            .child(SharedString::from(n.clone())),
                    );
                }
                box_el = box_el.child(n_col);
            }
            box_el
        })
}

fn ctx_metric_row(label: &'static str, value: String, cx: &gpui_kit::App) -> gpui_kit::Div {
    let p = palette(cx);
    h_row()
        .justify_between()
        .text_xs()
        .child(div().text_color(to_hsla(p.muted)).child(label))
        .child(
            div()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(to_hsla(p.ink))
                .child(SharedString::from(value)),
        )
}
