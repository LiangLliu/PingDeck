//! Composer context ring: usage trigger plus a popover with the core's per-part context breakdown,
//! "Compact now" gated by the core's `can_compact`, session totals and run notices.

use gpui_kit::{
    App, Context, Div, IntoElement, ParentElement as _, SharedString, Styled as _, div, px,
};
use spanbay_desktop::core::app::agent_context_breakdown_impl;
use spanbay_desktop::state::ChatNotice;

use crate::bridge::{runtime, spawn_async_into};
use crate::ui::{
    Anchor, Disableable as _, StatusShape, btn_secondary, h_row, palette, popover,
    popover_trigger_btn, progress_bar, section_heading, status_indicator, to_hsla, v_col,
};

use super::ChatView;
use super::metrics::{format_money, format_tokens_compact, format_tokens_int};

impl ChatView {
    /// Measures the next request's context; called when the popover opens.
    pub(super) fn load_context_breakdown(&mut self, cx: &mut Context<Self>) {
        let sid = self.thread.session_id.clone();
        if sid.is_empty() {
            if self.context_breakdown.take().is_some() {
                cx.notify();
            }
            return;
        }
        let sid_check = sid.clone();
        let task = spawn_async_into(
            cx,
            move |state| async move { agent_context_breakdown_impl(&state, &sid).await },
            move |this, res, cx| {
                if this.thread.session_id == sid_check {
                    this.context_breakdown = res.ok();
                    cx.notify();
                }
            },
        );
        self._tasks.push(task);
    }
}

pub fn render_context_meter(view: &ChatView, cx: &mut Context<ChatView>) -> impl IntoElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let used = view.thread.context_used_tokens;
    let limit = view.thread.context_limit_tokens.filter(|&l| l > 0);
    let running = view.is_run_active();
    let breakdown = view
        .context_breakdown
        .filter(|_| !running && view.thread.live_compactions.is_empty());
    let can_compact = breakdown.is_some_and(|b| b.can_compact) && !running;
    let keep_recent_turns = runtime(cx).state().settings().ai.agent.keep_recent_turns;
    // Amounts are never summed across currencies, and none is shown without a known currency.
    let costs: Vec<String> = view
        .session_costs
        .iter()
        .map(|c| format_money(c.cost_micro, &c.currency))
        .collect();
    let run_cost = view
        .thread
        .run_meter
        .as_ref()
        .filter(|m| m.cost_micro > 0)
        .and_then(|m| {
            m.currency
                .as_deref()
                .map(|cur| format_money(m.cost_micro, cur))
        });
    let session_totals = view.session.as_ref().map(|s| {
        (
            s.total_usage.input_tokens.unwrap_or(0),
            s.total_usage.output_tokens.unwrap_or(0),
            s.total_usage.cached_input_tokens.unwrap_or(0),
        )
    });
    let chat_ent = cx.entity();
    let open_ent = chat_ent.clone();

    let trigger_body = if let Some(lim) = limit {
        let pct = pct_of(used, lim);
        let fg = if pct >= 85.0 {
            to_hsla(p.bad)
        } else if pct >= 70.0 {
            to_hsla(p.warn)
        } else {
            to_hsla(p.muted)
        };
        h_row().gap(px(4.0)).child(
            div()
                .text_xs()
                .text_color(fg)
                .child(SharedString::from(format!("◔ {pct:.0}%"))),
        )
    } else {
        h_row().child(
            div()
                .text_xs()
                .text_color(to_hsla(p.faint))
                .child(ch.context_used(format_tokens_compact(used))),
        )
    };

    popover("chat-context-popover")
        .anchor(Anchor::BottomRight)
        .trigger(popover_trigger_btn("chat-context-trigger", trigger_body))
        .on_open_change(move |open, _, cx| {
            if *open {
                open_ent.update(cx, |this, cx| this.load_context_breakdown(cx));
            }
        })
        .content(move |_, _, pop_cx| {
            let pp = palette(pop_cx);
            let pch = crate::ui::tr(pop_cx).chat();
            let ent = chat_ent.clone();
            let pop_h = pop_cx.entity();
            let (shown_used, shown_limit) = breakdown.map_or((used, limit), |b| {
                (b.total, b.limit.filter(|&l| l > 0).or(limit))
            });
            let mut box_el = v_col()
                .w(px(270.0))
                .gap(px(8.0))
                .child(section_heading(pch.inspector_tabs_context_window(), pop_cx))
                .child(
                    div()
                        .text_sm()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(pp.ink))
                        .child(match shown_limit {
                            Some(l) => pch.context_used_of_limit(
                                format_tokens_int(shown_used),
                                format_tokens_int(l),
                            ),
                            None => pch.context_used(format_tokens_int(shown_used)),
                        }),
                )
                .children(shown_limit.map(|l| progress_bar("ctx-pop-bar", pct_of(shown_used, l))));

            if let Some(b) = breakdown {
                let parts = [
                    (pch.context_system(), b.system),
                    (pch.context_instructions(), b.instructions),
                    (pch.context_skills(), b.skills),
                    (pch.context_tool_definitions(), b.tool_definitions),
                    (pch.context_messages(), b.messages),
                    (pch.context_tool_results(), b.tool_results),
                    (pch.context_attachments(), b.attachments),
                ];
                let max_part = parts.iter().map(|(_, tok)| *tok).max().unwrap_or(0).max(1);
                let mut rows = v_col().gap(px(4.0));
                for (ix, (label, tokens)) in parts.into_iter().enumerate() {
                    rows = rows.child(ctx_part_row(
                        ix,
                        label,
                        format_tokens_int(tokens),
                        pct_of(tokens, max_part),
                        pop_cx,
                    ));
                }
                box_el = box_el.child(rows);
            }

            let compact_hint = if can_compact {
                breakdown
                    .and_then(|b| b.compact_turns)
                    .map(|t| pch.context_will_compact(t, keep_recent_turns))
            } else if running || breakdown.is_none() {
                None
            } else {
                Some(pch.context_cannot_compact(keep_recent_turns))
            };
            box_el = box_el.child(
                h_row()
                    .w_full()
                    .justify_between()
                    .gap(px(8.0))
                    .children(compact_hint.map(|hint| {
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_size(px(11.0))
                            .text_color(to_hsla(pp.muted))
                            .child(SharedString::from(hint))
                    }))
                    .child(
                        btn_secondary("chat-ctx-compact", pch.inspector_tabs_compact_context_now())
                            .disabled(!can_compact)
                            .on_click(move |_, w, cx| {
                                ent.update(cx, |this, cx| this.compact_context(cx));
                                pop_h.update(cx, |s, cx| s.dismiss(w, cx));
                            }),
                    ),
            );

            if let Some((in_tok, out_tok, cached)) = session_totals {
                let mut totals = v_col()
                    .gap(px(4.0))
                    .pt(px(6.0))
                    .border_t_1()
                    .border_color(to_hsla(pp.line.composite_over(pp.overlay)))
                    .child(section_heading(pch.inspector_tabs_session_totals(), pop_cx))
                    .child(ctx_metric_row(
                        pch.inspector_tabs_input_tokens(),
                        format_tokens_int(in_tok),
                        pop_cx,
                    ))
                    .child(ctx_metric_row(
                        pch.inspector_tabs_output_tokens(),
                        format_tokens_int(out_tok),
                        pop_cx,
                    ))
                    .child(ctx_metric_row(
                        pch.inspector_tabs_cached_input(),
                        format_tokens_int(cached),
                        pop_cx,
                    ));
                for c in &costs {
                    totals = totals.child(ctx_metric_row(
                        pch.inspector_tabs_estimated_cost(),
                        c.clone(),
                        pop_cx,
                    ));
                }
                if let Some(run) = &run_cost {
                    totals =
                        totals.child(ctx_metric_row(pch.context_this_run(), run.clone(), pop_cx));
                }
                box_el = box_el.child(totals);
            }
            box_el
        })
}

/// A run notice in the interface language: typed per core code, the core's English text for an
/// unknown code.
pub(super) fn notice_text(n: &ChatNotice, cx: &App) -> String {
    let ch = crate::ui::tr(cx).chat();
    let a = |k: &str| n.args.get(k).map_or("", String::as_str);
    match n.code.as_deref() {
        Some("retrying") => ch.notice_retrying(a("reason"), a("delay_s"), a("attempt"), a("of")),
        Some("stall_retry") => ch.notice_stall_retry().to_owned(),
        Some("fallback") => ch.notice_fallback(a("from"), a("reason"), a("to")),
        Some("no_progress") => ch.notice_no_progress(a("signal")),
        Some("context_full") => ch.notice_context_full().to_owned(),
        Some("context_window_learned") => ch.notice_context_window_learned(a("tokens")),
        Some("compaction_failed") => ch.notice_compaction_failed().to_owned(),
        Some("native_search_lowered") => ch.notice_native_search_lowered().to_owned(),
        Some("process_exited") => ch.notice_process_exited(a("process"), a("command")),
        Some("step_limit_summary") => ch.notice_step_limit_summary(a("steps")),
        Some("untracked_changes") => ch.notice_untracked_changes(a("count"), a("paths")),
        _ => n.text.clone(),
    }
}

/// Inline run notices at the tail of the turn (`retrying`, `fallback`, `untracked_changes`, etc.).
pub(super) fn render_inline_notices(notices: &[ChatNotice], cx: &App) -> Option<Div> {
    if notices.is_empty() {
        return None;
    }
    let p = palette(cx);
    let mut col = v_col().w_full().gap(px(4.0));
    for n in notices {
        let is_warn = n.code.as_deref() == Some("untracked_changes");
        let (shape, fg) = if is_warn {
            (StatusShape::Warn, to_hsla(p.warn))
        } else {
            (StatusShape::Idle, to_hsla(p.muted))
        };
        col = col.child(
            h_row()
                .gap(px(6.0))
                .text_xs()
                .text_color(fg)
                .child(status_indicator(shape, cx))
                .child(SharedString::from(notice_text(n, cx))),
        );
    }
    Some(col)
}
fn pct_of(used: u64, limit: u64) -> f32 {
    ((used as f64 / limit as f64) * 100.0).min(100.0) as f32
}

fn ctx_metric_row(label: &'static str, value: String, cx: &App) -> Div {
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

fn ctx_part_row(ix: usize, label: &'static str, value: String, pct: f32, cx: &App) -> Div {
    let p = palette(cx);
    h_row()
        .w_full()
        .gap(px(8.0))
        .text_xs()
        .child(
            div()
                .w(px(88.0))
                .flex_shrink_0()
                .truncate()
                .text_color(to_hsla(p.muted))
                .child(label),
        )
        .child(div().flex_1().min_w_0().child(progress_bar(
            SharedString::from(format!("ctx-part-{ix}")),
            pct,
        )))
        .child(
            div()
                .flex_shrink_0()
                .font_weight(gpui_kit::FontWeight::MEDIUM)
                .text_color(to_hsla(p.ink))
                .child(SharedString::from(value)),
        )
}
