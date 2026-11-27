//! Spacious full-width run cards for `RunsView` (active 24h board cards & cross-chat history cards).

use gpui_kit::{
    AnyElement, Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::agent::dto::{AgentBoardRow, SessionRunHistoryItem};

use crate::ui::{
    Tone, UiIcon, badge, btn_danger, btn_ghost, btn_primary, btn_secondary, h_row, icon_xs,
    palette, status_indicator, to_hsla, v_col,
};

use super::RunsView;
use super::status::{
    folder_basename, format_elapsed_short, format_model_short, format_relative_age,
    format_tokens_compact, run_status_meta,
};
use super::types::WorkflowTab;

pub(super) fn render_board_row_card(
    row: &AgentBoardRow,
    max_steps: u32,
    now_ms: i64,
    cx: &mut Context<RunsView>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let (shape, tone, status_lbl) = run_status_meta(
        row.run.status,
        row.wait,
        row.run.status_detail.as_deref(),
        cx,
    );
    let title = if row.title.trim().is_empty() {
        ch.header_new_chat().to_owned()
    } else {
        row.title.clone()
    };
    let elapsed_ms = if row.run.status.is_active() {
        (now_ms - row.run.started_at_ms).max(row.awake_ms as i64) as u64
    } else {
        row.awake_ms
    };
    let age = format_relative_age(row.run.started_at_ms, now_ms, cx);
    let total_tok =
        row.run.usage.input_tokens.unwrap_or(0) + row.run.usage.output_tokens.unwrap_or(0);
    let sid_open = row.run.session_id.clone();
    let rid_open = row.run.run_id.clone();
    let sid_btn = row.run.session_id.clone();
    let rid_btn = row.run.run_id.clone();
    let sid_chat = row.run.session_id.clone();
    let rid_stop = row.run.run_id.clone();
    let sid_diff = row.run.session_id.clone();
    let rid_diff = row.run.run_id.clone();
    let is_active = row.run.status.is_active();
    let hover_border = to_hsla(p.accent_line.composite_over(p.surface));

    let subtitle = row
        .pending_approval
        .as_ref()
        .map(|a| (to_hsla(p.warn), a.summary.clone()))
        .or_else(|| row.run.error.as_ref().map(|e| (to_hsla(p.bad), e.clone())))
        .or_else(|| {
            row.last_answer
                .as_ref()
                .map(|a| (to_hsla(p.muted), a.clone()))
        });

    h_row()
        .id(SharedString::from(format!("runs-card-{}", row.run.run_id)))
        .w_full()
        .justify_between()
        .gap(px(14.0))
        .px(px(14.0))
        .py(px(11.0))
        .rounded(px(9.0))
        .bg(to_hsla(p.surface))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .cursor_pointer()
        .hover(move |s| s.border_color(hover_border))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.open_workflow_modal(sid_open.clone(), Some(rid_open.clone()), None, cx);
        }))
        .child(
            v_col()
                .flex_1()
                .min_w_0()
                .gap(px(5.0))
                .child(
                    h_row()
                        .w_full()
                        .gap(px(7.0))
                        .child(status_indicator(shape, cx))
                        .child(
                            div()
                                .min_w_0()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(SharedString::from(title)),
                        )
                        .child(
                            h_row()
                                .flex_shrink_0()
                                .gap(px(5.0))
                                .child(badge(status_lbl, tone, cx))
                                .children((row.plan_total > 0).then(|| {
                                    badge(
                                        ch.sidebar_board_plan(row.plan_done, row.plan_total),
                                        Tone::Accent,
                                        cx,
                                    )
                                }))
                                .children((row.subagent_counts.total > 0).then(|| {
                                    badge(
                                        if row.subagent_counts.active > 0 {
                                            ch.monitor_sub_active_badge(
                                                row.subagent_counts.active,
                                                row.subagent_counts.total,
                                            )
                                        } else {
                                            ch.monitor_sub_badge(row.subagent_counts.total)
                                        },
                                        Tone::Neutral,
                                        cx,
                                    )
                                })),
                        ),
                )
                .children(subtitle.map(|(col, txt)| {
                    div()
                        .w_full()
                        .text_xs()
                        .text_color(col)
                        .truncate()
                        .child(SharedString::from(txt))
                }))
                .child(
                    h_row()
                        .w_full()
                        .gap(px(10.0))
                        .flex_wrap()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(SharedString::from(format_model_short(&row.run.model_label)))
                        .children(row.folder.as_deref().map(|f| {
                            h_row()
                                .gap(px(3.0))
                                .child(icon_xs(UiIcon::Folder))
                                .child(SharedString::from(folder_basename(f).to_owned()))
                        }))
                        .child(SharedString::from(
                            ch.monitor_steps(row.run.iterations, max_steps),
                        ))
                        .child(SharedString::from(format_elapsed_short(elapsed_ms)))
                        .children((total_tok > 0).then(|| {
                            SharedString::from(format!("{} tok", format_tokens_compact(total_tok)))
                        }))
                        .children((row.run.cost_micro_usd > 0).then(|| {
                            let cur = row.cost_currency.as_deref().unwrap_or("USD");
                            SharedString::from(format!(
                                "{cur} {:.4}",
                                row.run.cost_micro_usd as f64 / 1_000_000.0
                            ))
                        }))
                        .child(SharedString::from(age)),
                ),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(6.0))
                .children((row.pending_changes.files > 0).then(|| {
                    btn_ghost(
                        SharedString::from(format!("runs-diff-{}", row.run.run_id)),
                        format!(
                            "{} +{} -{}",
                            ch.monitor_files_changed(row.pending_changes.files),
                            row.pending_changes.added,
                            row.pending_changes.removed
                        ),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.open_workflow_modal(
                            sid_diff.clone(),
                            Some(rid_diff.clone()),
                            Some(WorkflowTab::Changes),
                            cx,
                        );
                    }))
                }))
                .children(is_active.then(|| {
                    btn_danger(
                        SharedString::from(format!("runs-stop-{}", row.run.run_id)),
                        ch.card_stop(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.stop_run_by_id(rid_stop.clone(), cx);
                    }))
                }))
                .child(
                    btn_secondary(
                        SharedString::from(format!("runs-chat-{}", row.run.run_id)),
                        ch.monitor_open_chat(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.open_chat_session(sid_chat.clone(), cx);
                    })),
                )
                .child(
                    btn_primary(
                        SharedString::from(format!("runs-wf-{}", row.run.run_id)),
                        ch.monitor_open_workflow(),
                    )
                    .icon(icon_xs(UiIcon::Activity))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.open_workflow_modal(sid_btn.clone(), Some(rid_btn.clone()), None, cx);
                    })),
                ),
        )
        .into_any_element()
}

pub(super) fn render_history_item_card(
    item: &SessionRunHistoryItem,
    now_ms: i64,
    cx: &mut Context<RunsView>,
) -> AnyElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let (shape, tone, status_lbl) =
        run_status_meta(item.run.status, None, item.run.status_detail.as_deref(), cx);
    let title = if item.title.trim().is_empty() {
        ch.header_new_chat().to_owned()
    } else {
        item.title.clone()
    };
    let sid_open = item.run.session_id.clone();
    let rid_open = item.run.run_id.clone();
    let sid_chat = item.run.session_id.clone();
    let age = format_relative_age(item.run.started_at_ms, now_ms, cx);
    let hover_border = to_hsla(p.accent_line.composite_over(p.surface));

    h_row()
        .id(SharedString::from(format!("runs-hist-{}", item.run.run_id)))
        .w_full()
        .justify_between()
        .gap(px(12.0))
        .px(px(14.0))
        .py(px(10.0))
        .rounded(px(9.0))
        .bg(to_hsla(p.surface))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.surface)))
        .cursor_pointer()
        .hover(move |s| s.border_color(hover_border))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.open_workflow_modal(sid_open.clone(), Some(rid_open.clone()), None, cx);
        }))
        .child(
            v_col()
                .flex_1()
                .min_w_0()
                .gap(px(4.0))
                .child(
                    h_row()
                        .w_full()
                        .gap(px(7.0))
                        .child(status_indicator(shape, cx))
                        .child(
                            div()
                                .min_w_0()
                                .text_sm()
                                .font_weight(gpui_kit::FontWeight::MEDIUM)
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(SharedString::from(title)),
                        )
                        .child(
                            h_row()
                                .flex_shrink_0()
                                .gap(px(5.0))
                                .child(badge(status_lbl, tone, cx))
                                .children(
                                    item.has_task_graph.then(|| {
                                        badge(ch.gates_plan_view_graph(), Tone::Accent, cx)
                                    }),
                                )
                                .children((item.subagent_count > 0).then(|| {
                                    badge(
                                        ch.monitor_sub_badge(item.subagent_count),
                                        Tone::Neutral,
                                        cx,
                                    )
                                })),
                        ),
                )
                .children(item.prompt_preview.as_ref().map(|pr| {
                    div()
                        .w_full()
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .truncate()
                        .child(SharedString::from(pr.clone()))
                }))
                .child(
                    h_row()
                        .gap(px(10.0))
                        .text_xs()
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(format_model_short(
                            &item.run.model_label,
                        )))
                        .child(SharedString::from(
                            ch.monitor_steps_short(item.run.iterations),
                        ))
                        .child(SharedString::from(format_elapsed_short(item.awake_ms)))
                        .child(SharedString::from(age)),
                ),
        )
        .child(
            h_row().flex_shrink_0().gap(px(6.0)).child(
                btn_ghost(
                    SharedString::from(format!("runs-hchat-{}", item.run.run_id)),
                    ch.monitor_open_chat(),
                )
                .on_click(cx.listener(move |this, _, _, cx| {
                    cx.stop_propagation();
                    this.open_chat_session(sid_chat.clone(), cx);
                })),
            ),
        )
        .into_any_element()
}
