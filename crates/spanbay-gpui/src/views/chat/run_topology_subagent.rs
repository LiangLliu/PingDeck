//! Inline live sub-agent group card rendered inside the Chat streaming turn (`trace.rs`).
//!
//! Clicking the header `Workflow` button launches the shared 90% viewport Workflow Monitor modal
//! focused directly on the active run.

use gpui_kit::{Context, IntoElement, ParentElement as _, SharedString, Styled as _, div, px};
use spanbay_desktop::core::agent::model::ToolCallStatus;
use spanbay_desktop::state::LiveToolCall;

use crate::ui::{
    StatusShape, Tone, UiIcon, badge, btn_danger, btn_ghost, h_row, icon_xs, palette,
    status_indicator, to_hsla, v_col,
};
use crate::views::runs::status::{
    format_elapsed_short, run_status_meta, subagent_kind_label, subagent_role_label,
};

use super::ChatView;
use super::trace::summarize_tool_args;

pub fn render_live_subagent_group(
    view: &ChatView,
    task_calls: &[&LiveToolCall],
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let parallel = task_calls.len() >= 2;
    let header_title = if parallel {
        ch.trace_parallel_subagents(task_calls.len())
    } else {
        ch.trace_subagent_call().to_owned()
    };
    let active_run_id = view.thread.active_run.as_ref().map(|r| r.run_id.clone());

    let mut group =
        v_col()
            .w_full()
            .rounded(px(10.0))
            .overflow_hidden()
            .bg(to_hsla(p.pane))
            .border_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .child(
                h_row()
                    .w_full()
                    .justify_between()
                    .px(px(10.0))
                    .py(px(7.0))
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
                    )
                    .child(
                        btn_ghost("trace-open-wf", ch.monitor_workflow_btn())
                            .icon(icon_xs(UiIcon::Activity))
                            .on_click(cx.listener(move |this, _, _, cx| {
                                this.open_workflow_modal(active_run_id.clone(), None, cx);
                            })),
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
        let (shape, tone, status_lbl) = if let Some(sub) = live_sub {
            run_status_meta(
                sub.summary.status,
                None,
                sub.summary.status_detail.as_deref(),
                cx,
            )
        } else {
            match call.status {
                None => (StatusShape::Run, Tone::Accent, ch.run_status_running()),
                Some(ToolCallStatus::Ok) => {
                    (StatusShape::Ok, Tone::Success, ch.run_status_completed())
                }
                Some(ToolCallStatus::Cancelled) => {
                    (StatusShape::Idle, Tone::Neutral, ch.run_status_cancelled())
                }
                Some(_) => (StatusShape::Bad, Tone::Danger, ch.run_status_failed()),
            }
        };
        let mut row = h_row()
            .w_full()
            .justify_between()
            .gap(px(8.0))
            .px(px(10.0))
            .py(px(7.0))
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .child(
                h_row()
                    .flex_1()
                    .min_w_0()
                    .gap(px(7.0))
                    .child(status_indicator(shape, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(SharedString::from(desc)),
                    )
                    .children(live_sub.map(|sub| {
                        badge(
                            subagent_kind_label(&sub.subagent_kind, cx),
                            Tone::Neutral,
                            cx,
                        )
                    }))
                    .children(
                        live_sub
                            .and_then(|sub| sub.summary.role)
                            .map(|r| badge(subagent_role_label(r, cx), Tone::Accent, cx)),
                    )
                    .children(live_sub.and_then(|sub| sub.current_tool.clone()).map(|t| {
                        div()
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.muted))
                            .child(SharedString::from(t))
                    })),
            )
            .child(
                h_row()
                    .flex_shrink_0()
                    .gap(px(6.0))
                    .child(badge(status_lbl, tone, cx))
                    .children(call.duration_ms.map(|ms| {
                        div()
                            .text_size(px(11.0))
                            .text_color(to_hsla(p.faint))
                            .child(SharedString::from(format_elapsed_short(ms)))
                    }))
                    .children(live_sub.filter(|s| !s.finished).map(|sub| {
                        let stop_id = sub.child_run_id.clone();
                        btn_danger(
                            SharedString::from(format!("tr-stop-sub-{}", sub.child_run_id)),
                            ch.monitor_stop_subagent(),
                        )
                        .on_click(cx.listener(move |this, _, _, cx| {
                            this.stop_run_by_id(stop_id.clone(), cx);
                        }))
                    })),
            );
        if let Some(sub) = live_sub
            && let Some(err) = &sub.summary.error
        {
            row = v_col().w_full().child(row).child(
                div()
                    .px(px(12.0))
                    .pb(px(6.0))
                    .text_size(px(11.0))
                    .text_color(to_hsla(p.bad))
                    .child(SharedString::from(err.clone())),
            );
        }
        group = group.child(row);
    }
    group
}
