//! Inline sub-agent group card rendered inside an assistant turn's activity spine.
//!
//! Each sub-agent row explicitly surfaces its sub-model and role (`deepseek-v4-flash · 只读探索`),
//! current tool or elapsed/cost, and `[打开子对话]` / `[停止]`. Expanding a row shows its tool
//! steps (`└` tree style) without repeating empty todo lists (`.overhaul/chat-redesign-spec.md` §4 & §12).

use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::agent::model::AgentRunStatus;
use spanbay_desktop::state::{
    GraphNodeStatus, LiveToolCall, TranscriptDensity, map_run_status, map_tool_call_status,
};

use crate::ui::{
    Tone, UiIcon, badge, btn_danger, btn_ghost, exit_code_badge, h_row, icon_xs, mono_font_family,
    palette, status_indicator, status_row_wait, status_visual, to_hsla, v_col, wait_kind,
};
use crate::views::runs::status::{format_elapsed_short, subagent_kind_label, subagent_role_label};

use super::ChatView;
use super::metrics::format_cost_micro;
use super::trace_detail::{localized_tool_verb, summarize_tool_args};

pub fn render_live_subagent_group(
    view: &ChatView,
    task_calls: &[&LiveToolCall],
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let mono = mono_font_family(cx);
    let parallel = task_calls.len() >= 2;
    let header_title = if parallel {
        ch.trace_parallel_subagents(task_calls.len())
    } else {
        ch.trace_subagent_call().to_owned()
    };
    let mut group =
        v_col()
            .w_full()
            .rounded(px(8.0))
            .overflow_hidden()
            .bg(to_hsla(p.pane))
            .border_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .child(
                h_row()
                    .w_full()
                    .justify_between()
                    .px(px(10.0))
                    .py(px(6.0))
                    .border_b_1()
                    .border_color(to_hsla(p.line.composite_over(p.pane)))
                    .child(
                        h_row()
                            .gap(px(6.0))
                            .child(div().text_color(to_hsla(p.accent)).child(icon_xs(
                                if parallel {
                                    UiIcon::Branch
                                } else {
                                    UiIcon::Bot
                                },
                            )))
                            .child(
                                div()
                                    .text_xs()
                                    .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                    .text_color(to_hsla(p.ink))
                                    .child(SharedString::from(header_title)),
                            ),
                    ),
            );

    for call in task_calls {
        let live_sub = view
            .thread
            .subagents
            .values()
            .find(|s| s.parent_call_id == call.call_id);
        let desc = live_sub
            .map(|s| s.description.clone())
            .filter(|d| !d.is_empty())
            .unwrap_or_else(|| summarize_tool_args(&call.arguments));
        let status = live_sub.map_or_else(
            || {
                call.status
                    .map_or(GraphNodeStatus::Running, map_tool_call_status)
            },
            |sub| map_run_status(sub.summary.status),
        );
        let key = format!("sub-{}", call.call_id);
        let open =
            view.expanded_traces.contains(&key) || view.density == TranscriptDensity::Verbose;
        let meta = if live_sub.is_some_and(|s| !s.finished) {
            live_sub
                .and_then(|s| s.current_tool())
                .map(|t| ch.subagent_current(t))
                .unwrap_or_default()
        } else {
            [
                call.duration_ms.map(format_elapsed_short),
                live_sub.and_then(|s| {
                    format_cost_micro(s.summary.cost_micro_usd, s.summary.cost_currency.as_deref())
                }),
            ]
            .into_iter()
            .flatten()
            .collect::<Vec<_>>()
            .join(" · ")
        };
        let mut trailing = h_row().gap(px(4.0));
        if let Some(sub) = live_sub {
            let model_lbl = (!sub.summary.model_label.is_empty())
                .then(|| view.format_raw_model_label(&sub.summary.model_label));
            let kind_lbl = subagent_kind_label(&sub.subagent_kind, cx);
            let badge_txt = match model_lbl {
                Some(m) => format!("{m} · {kind_lbl}"),
                None => kind_lbl.to_owned(),
            };
            trailing = trailing
                .child(badge(badge_txt, Tone::Neutral, cx))
                .children(
                    sub.summary
                        .role
                        .map(|r| badge(subagent_role_label(r, cx), Tone::Accent, cx)),
                );
            if !sub.child_session_id.is_empty() {
                let child = sub.child_session_id.clone();
                trailing = trailing.child(
                    btn_ghost(
                        SharedString::from(format!("tr-open-sub-{}", sub.child_run_id)),
                        ch.subagent_open_chat(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.open_session(child.clone(), cx);
                    })),
                );
            }
            if !sub.finished {
                let stop_id = sub.child_run_id.clone();
                trailing = trailing.child(
                    btn_danger(
                        SharedString::from(format!("tr-stop-sub-{}", sub.child_run_id)),
                        ch.monitor_stop_subagent(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.stop_run_by_id(stop_id.clone(), cx);
                    })),
                );
            }
        }
        trailing = trailing.child(div().text_color(to_hsla(p.faint)).child(icon_xs(if open {
            UiIcon::ChevronDown
        } else {
            UiIcon::ChevronRight
        })));

        let wait = live_sub.map(|s| wait_kind(None, s.summary.status_detail.as_deref()));
        let row = status_row_wait(
            SharedString::from(format!("tr-sub-{}", call.call_id)),
            status,
            wait,
            desc,
            (!meta.is_empty()).then(|| SharedString::from(meta)),
            Some(trailing.into_any_element()),
            cx,
        )
        .cursor_pointer()
        .on_click(cx.listener(move |this, _, _, cx| this.toggle_trace(key.clone(), cx)));

        let mut cell = v_col()
            .w_full()
            .px(px(2.0))
            .py(px(2.0))
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .child(row);
        if let Some(sub) = live_sub {
            if let Some(err) = &sub.summary.error {
                let err_fg = match sub.summary.status {
                    AgentRunStatus::Failed => to_hsla(p.bad),
                    AgentRunStatus::Interrupted => to_hsla(p.warn),
                    _ => to_hsla(p.muted),
                };
                cell = cell.child(
                    div()
                        .px(px(12.0))
                        .pb(px(6.0))
                        .text_size(px(11.0))
                        .text_color(err_fg)
                        .child(SharedString::from(err.clone())),
                );
            }
            if open && !sub.tool_steps.is_empty() {
                let _ = (ch.subagent_steps(), ch.subagent_todos());
                let mut steps = v_col()
                    .w_full()
                    .pl(px(18.0))
                    .pr(px(8.0))
                    .pb(px(4.0))
                    .gap(px(2.0));
                for (ix, step) in sub.tool_steps.iter().enumerate() {
                    let st = step
                        .status
                        .map_or(GraphNodeStatus::Running, map_tool_call_status);
                    let verb = localized_tool_verb(&step.name, cx);
                    steps = steps.child(
                        h_row()
                            .id(SharedString::from(format!(
                                "tr-sub-step-{}-{ix}-{}",
                                sub.child_run_id, step.call_id
                            )))
                            .w_full()
                            .gap(px(6.0))
                            .py(px(2.0))
                            .text_xs()
                            .child(
                                div()
                                    .font_family(mono.clone())
                                    .text_color(to_hsla(p.faint))
                                    .child(if st == GraphNodeStatus::Running {
                                        status_indicator(status_visual(st).0, cx)
                                    } else {
                                        div().child("└").into_any_element()
                                    }),
                            )
                            .child(
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .text_color(to_hsla(p.ink_2))
                                    .child(SharedString::from(verb)),
                            )
                            .children(
                                step.exit_code
                                    .filter(|c| *c != 0)
                                    .map(|c| exit_code_badge(c, cx)),
                            )
                            .children(step.duration_ms.map(|ms| {
                                div()
                                    .font_family(mono.clone())
                                    .text_size(px(11.0))
                                    .text_color(to_hsla(p.faint))
                                    .child(SharedString::from(format_elapsed_short(ms)))
                            })),
                    );
                }
                cell = cell.child(steps);
            }
        }
        group = group.child(cell);
    }
    group
}
