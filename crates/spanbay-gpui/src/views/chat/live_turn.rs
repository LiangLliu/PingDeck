//! Post-turn File Changes Summary Card, in-thread Plan block, hover-only Turn Footer, and
//! live-turn banners (`mcp_connecting`, `live_compactions`) for [`AssistantTurnView`]
//! (`.overhaul/chat-redesign-spec.md` §3.2, §6, §10.1).

use gpui_kit::{
    Context, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::core::agent::model::{FileChangeKind, FileChangeStatus};
use spanbay_desktop::core::model::hub::HubPart;
use spanbay_desktop::core::model::settings::ToolApproval;
use spanbay_desktop::services::clipboard;
use spanbay_desktop::state::{
    AssistantTurnView, GraphNodeStatus, PlanOutcomeKind, TurnFileChangeSummary, plan_outcome,
};

use crate::ui::{
    StatusShape, Tone, UiIcon, WaitKind, alert_banner, badge, btn_ghost, btn_icon, btn_primary,
    btn_secondary, h_row, icon_xs, labeled_divider, mono_font_family, palette, status_badge,
    status_indicator, to_hsla, v_col,
};

use super::ChatView;
use super::metrics::{format_tokens_int, render_turn_metrics_pill, stop_reason_label};

/// Renders live MCP-connecting banner and context-compaction dividers at the top of an active turn.
pub fn render_live_turn_banners(view: &ChatView, cx: &gpui_kit::App) -> Option<gpui_kit::Div> {
    if view.thread.mcp_connecting.is_empty() && view.thread.live_compactions.is_empty() {
        return None;
    }
    let ch = crate::ui::tr(cx).chat();
    let mut col = v_col().w_full().gap(px(6.0));
    if !view.thread.mcp_connecting.is_empty() {
        col = col.child(alert_banner(
            Tone::Neutral,
            ch.thread_mcp_connecting(view.thread.mcp_connecting.join(", ")),
            None,
            cx,
        ));
    }
    for (before, after) in &view.thread.live_compactions {
        col = col.child(labeled_divider(
            ch.thread_compacted_tokens(format_tokens_int(*before), format_tokens_int(*after)),
            cx,
        ));
    }
    Some(col)
}

/// Renders the Post-Turn File Changes Summary Card (`N个文件已更改 +X −Y [撤销本轮修改] [审查变更 ›]`).
pub fn render_turn_changes_card(
    turn: &AssistantTurnView,
    cx: &mut Context<ChatView>,
) -> Option<gpui_kit::Stateful<gpui_kit::Div>> {
    if turn.turn_changes.is_empty() {
        return None;
    }
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let mono = mono_font_family(cx);

    let total_files = turn.turn_changes.len();
    let total_added: u32 = turn.turn_changes.iter().map(|c| c.added).sum();
    let total_removed: u32 = turn.turn_changes.iter().map(|c| c.removed).sum();
    let all_undone = turn
        .turn_changes
        .iter()
        .all(|c| c.status == FileChangeStatus::Undone);
    let active_change_ids: Vec<String> = turn
        .turn_changes
        .iter()
        .filter(|c| c.status != FileChangeStatus::Undone)
        .flat_map(|c| c.change_ids.iter().cloned())
        .collect();
    let has_conflict = turn
        .turn_changes
        .iter()
        .any(|c| c.status == FileChangeStatus::Conflict);

    let head_title = if all_undone {
        ch.turn_changes_undone(total_files)
    } else {
        ch.turn_changes_title(total_files)
    };
    let tid = turn.turn_id.clone();
    let tid_rv = turn.turn_id.clone();

    let header = h_row()
        .w_full()
        .justify_between()
        .gap(px(8.0))
        .px(px(12.0))
        .py(px(7.0))
        .bg(to_hsla(p.pane))
        .border_b_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            h_row()
                .gap(px(8.0))
                .child(status_indicator(
                    if all_undone {
                        StatusShape::Stopped
                    } else if has_conflict {
                        StatusShape::Warn
                    } else {
                        StatusShape::Ok
                    },
                    cx,
                ))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(SharedString::from(head_title)),
                )
                .child(
                    h_row()
                        .gap(px(4.0))
                        .font_family(mono)
                        .text_xs()
                        .child(
                            div()
                                .text_color(to_hsla(p.ok))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(SharedString::from(format!("+{total_added}"))),
                        )
                        .child(
                            div()
                                .text_color(to_hsla(p.bad))
                                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                                .child(SharedString::from(format!("−{total_removed}"))),
                        ),
                ),
        )
        .child(
            h_row()
                .gap(px(6.0))
                .children((!all_undone && !active_change_ids.is_empty()).then(|| {
                    let ids = active_change_ids.clone();
                    btn_secondary(
                        SharedString::from(format!("turn-undo-btn-{tid}")),
                        ch.turn_undo_turn(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        if has_conflict {
                            this.open_undo_precheck(cx);
                        } else {
                            this.undo_changes_with_force(ids.clone(), false, cx);
                        }
                    }))
                }))
                .child(
                    btn_primary(
                        SharedString::from(format!("turn-review-btn-{tid_rv}")),
                        ch.turn_review_changes(),
                    )
                    .on_click(cx.listener(|this, _, _, cx| {
                        this.open_review(None, cx);
                    })),
                ),
        );

    let mut rows = v_col().w_full().bg(to_hsla(p.surface));
    for (idx, fc) in turn.turn_changes.iter().enumerate() {
        rows = rows.child(render_turn_change_file_row(
            &turn.turn_id,
            idx,
            fc,
            idx + 1 < total_files,
            cx,
        ));
    }

    let mut card = v_col()
        .id(SharedString::from(format!(
            "turn-changes-card-{}",
            turn.turn_id
        )))
        .w_full()
        .rounded(px(8.0))
        .overflow_hidden()
        .border_1()
        .border_color(to_hsla(p.line_strong.composite_over(p.surface)))
        .child(header)
        .child(rows);

    if !turn.untracked_paths.is_empty() {
        let warn_txt = ch.notice_untracked_changes(
            turn.untracked_paths.len(),
            turn.untracked_paths.join(ch.cost_list_sep()),
        );
        card = card.child(
            h_row()
                .w_full()
                .gap(px(6.0))
                .px(px(12.0))
                .py(px(5.0))
                .bg(to_hsla(p.warn_soft.composite_over(p.pane)))
                .border_t_1()
                .border_color(to_hsla(p.warn_line.composite_over(p.pane)))
                .text_size(px(11.0))
                .text_color(to_hsla(p.warn))
                .child(status_indicator(StatusShape::Warn, cx))
                .child(div().flex_1().min_w_0().child(SharedString::from(warn_txt))),
        );
    }

    Some(card)
}

fn render_turn_change_file_row(
    turn_id: &str,
    idx: usize,
    fc: &TurnFileChangeSummary,
    has_border_b: bool,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let mono = mono_font_family(cx);
    let kind_label = match fc.kind {
        FileChangeKind::Create => ch.inspector_new(),
        FileChangeKind::Edit => ch.trace_verb_edit_file(),
    };
    let status_lbl = crate::views::runs::status::file_change_status_label(fc.status, cx);
    let status_tone = match fc.status {
        FileChangeStatus::Kept => Tone::Success,
        FileChangeStatus::Undone => Tone::Neutral,
        FileChangeStatus::Conflict => Tone::Danger,
        FileChangeStatus::Pending | FileChangeStatus::Applying => Tone::Warning,
    };
    let path_for_click = fc.path.clone();
    let path_for_diff = fc.path.clone();

    let mut row = h_row()
        .id(SharedString::from(format!("tcc-file-{turn_id}-{idx}")))
        .w_full()
        .justify_between()
        .gap(px(8.0))
        .px(px(12.0))
        .py(px(6.0))
        .cursor_pointer()
        .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.surface))))
        .on_click(cx.listener(move |this, _, _, cx| {
            this.open_review(Some(path_for_click.clone()), cx);
        }))
        .child(
            div()
                .flex_1()
                .min_w_0()
                .truncate()
                .font_family(mono.clone())
                .text_xs()
                .text_color(to_hsla(p.ink))
                .child(SharedString::from(fc.path.clone())),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(6.0))
                .child(badge(kind_label, Tone::Neutral, cx))
                .child(badge(status_lbl, status_tone, cx))
                .child(
                    h_row()
                        .gap(px(3.0))
                        .font_family(mono)
                        .text_xs()
                        .child(
                            div()
                                .text_color(to_hsla(p.ok))
                                .child(SharedString::from(format!("+{}", fc.added))),
                        )
                        .child(
                            div()
                                .text_color(to_hsla(p.bad))
                                .child(SharedString::from(format!("−{}", fc.removed))),
                        ),
                )
                .child(
                    btn_ghost(
                        SharedString::from(format!("tcc-diff-btn-{turn_id}-{idx}")),
                        ch.trace_view_diff(),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.open_review(Some(path_for_diff.clone()), cx);
                    })),
                ),
        );
    if has_border_b {
        row = row
            .border_b_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)));
    }
    row
}

/// Renders the hover-only Turn Footer (actions on the left, metrics pill + 7-item popover on the right).
pub fn render_turn_footer(
    view: &ChatView,
    turn: &AssistantTurnView,
    group_name: SharedString,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let ch = crate::ui::tr(cx).chat();
    let idle = !view.is_run_active();
    let copy_text = turn.final_markdown.clone();
    let retry_id = turn
        .first_assistant_id
        .clone()
        .or_else(|| turn.user_message_id.clone());
    let branch_id = turn
        .last_assistant_id
        .clone()
        .or_else(|| turn.first_assistant_id.clone());
    let delete_id = turn
        .last_assistant_id
        .clone()
        .or_else(|| turn.first_assistant_id.clone());
    let wf_run_id = turn.run_id.clone();
    let has_tools = turn.totals.visible_tool_count > 0;
    let has_active_changes = turn
        .turn_changes
        .iter()
        .any(|c| c.status != FileChangeStatus::Undone);
    let fallback_currency = view
        .thread
        .run_meter
        .as_ref()
        .and_then(|m| m.currency.as_deref());
    let metrics_pill = render_turn_metrics_pill(view, turn, fallback_currency, cx);
    let stop_chip = turn
        .stop_reason
        .and_then(|s| stop_reason_label(s, cx))
        .map(|lbl| badge(lbl, Tone::Warning, cx));

    let hover_bar = h_row()
        .flex_1()
        .min_w_0()
        .justify_between()
        .gap(px(6.0))
        .opacity(0.0)
        .group_hover(group_name, |s| s.opacity(1.0))
        .child(
            h_row()
                .gap(px(2.0))
                .child(
                    btn_icon(
                        SharedString::from(format!("a-cp-{}", turn.turn_id)),
                        UiIcon::Copy,
                    )
                    .tooltip(ch.thread_copy_2())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        let _ = clipboard::set_text(&copy_text);
                        this.status_msg = Some((
                            Tone::Success,
                            crate::ui::tr(cx).chat().thread_copied_2().into(),
                        ));
                        cx.notify();
                    })),
                )
                .children(idle.then_some(retry_id.clone()).flatten().map(|mid_rt| {
                    btn_icon(
                        SharedString::from(format!("a-rt-{}", turn.turn_id)),
                        UiIcon::Refresh,
                    )
                    .tooltip(ch.thread_regenerate())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.retry_message_with_undo(mid_rt.clone(), false, cx);
                    }))
                }))
                .children(
                    (idle && has_active_changes)
                        .then_some(retry_id)
                        .flatten()
                        .map(|mid_rt_undo| {
                            btn_ghost(
                                SharedString::from(format!("a-rt-undo-{}", turn.turn_id)),
                                ch.graph_action_undo_retry(),
                            )
                            .on_click(cx.listener(
                                move |this, _, _, cx| {
                                    this.retry_message_with_undo(mid_rt_undo.clone(), true, cx);
                                },
                            ))
                        }),
                )
                .children(branch_id.map(|mid_br| {
                    btn_icon(
                        SharedString::from(format!("a-br-{}", turn.turn_id)),
                        UiIcon::Branch,
                    )
                    .tooltip(ch.thread_branch_2())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.branch_from_message(mid_br.clone(), cx);
                    }))
                }))
                .children(has_tools.then_some(wf_run_id).flatten().map(|rid| {
                    let count = turn.totals.visible_tool_count;
                    btn_ghost(
                        SharedString::from(format!("a-wf-{}", turn.turn_id)),
                        ch.monitor_turn_pill(count),
                    )
                    .icon(icon_xs(UiIcon::Activity))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.open_workflow_modal(Some(rid.clone()), None, cx);
                    }))
                }))
                .children(idle.then_some(delete_id).flatten().map(|mid_dl| {
                    btn_icon(
                        SharedString::from(format!("a-dl-{}", turn.turn_id)),
                        UiIcon::Trash,
                    )
                    .tooltip(ch.thread_delete_2())
                    .on_click(cx.listener(move |this, _, _, cx| {
                        this.delete_message(mid_dl.clone(), cx);
                    }))
                })),
        )
        .children(metrics_pill);

    h_row()
        .w_full()
        .justify_between()
        .gap(px(8.0))
        .min_h(px(24.0))
        .child(hover_bar)
        .children(stop_chip)
}

/// In-thread plan marker block for a `submit_plan` call with its outcome stamp.
pub fn render_plan_block(
    call_id: &str,
    args: &str,
    result_parts: Option<&[HubPart]>,
    fallback_status: GraphNodeStatus,
    actively_revising: bool,
    next_user_text: Option<&str>,
    cx: &gpui_kit::App,
) -> gpui_kit::Stateful<gpui_kit::Div> {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let parsed = serde_json::from_str::<serde_json::Value>(args).ok();
    let title = parsed
        .as_ref()
        .and_then(|v| v.get("title"))
        .and_then(|v| v.as_str())
        .filter(|s| !s.trim().is_empty())
        .map(str::to_owned);
    let steps = parsed
        .as_ref()
        .and_then(|v| v.get("tasks"))
        .and_then(|v| v.as_array())
        .map_or(0, Vec::len);
    let outcome = result_parts.and_then(plan_outcome);
    let (status, wait, stamp) = match outcome {
        Some(o) => match o.kind {
            PlanOutcomeKind::Approved => {
                let mode_lbl = match o.mode.unwrap_or(ToolApproval::AutoReadOnly) {
                    ToolApproval::Ask => ch.composer_perm_ask(),
                    ToolApproval::AutoReadOnly => ch.composer_popovers_auto_reads(),
                    ToolApproval::AutoAll => ch.composer_perm_full(),
                };
                (
                    GraphNodeStatus::Completed,
                    None,
                    Some(ch.plan_stamp_approved(mode_lbl)),
                )
            }
            PlanOutcomeKind::Revise => {
                let inline_note = result_parts.and_then(|pts| {
                    pts.iter().find_map(|p| match p {
                        HubPart::Text { text, .. } => text
                            .split_once("\n\nUser:")
                            .map(|(_, u)| u.trim())
                            .filter(|s| !s.is_empty()),
                        _ => None,
                    })
                });
                let note =
                    inline_note.or_else(|| next_user_text.map(str::trim).filter(|s| !s.is_empty()));
                let revise_status = if actively_revising {
                    GraphNodeStatus::Running
                } else if fallback_status.is_warning_or_error()
                    || fallback_status == GraphNodeStatus::Cancelled
                {
                    fallback_status
                } else {
                    GraphNodeStatus::Completed
                };
                (revise_status, None, note.map(|t| ch.plan_stamp_revise(t)))
            }
            PlanOutcomeKind::TimedOut => (
                GraphNodeStatus::TimedOut,
                None,
                Some(ch.plan_stamp_timed_out().to_owned()),
            ),
        },
        None => (
            fallback_status,
            (fallback_status == GraphNodeStatus::Waiting).then_some(WaitKind::Plan),
            None,
        ),
    };
    let heading = match title {
        Some(t) => format!("{} · {t}", ch.plan_region()),
        None => ch.plan_region().to_owned(),
    };
    v_col()
        .id(SharedString::from(format!("plan-block-{call_id}")))
        .p(px(10.0))
        .gap(px(4.0))
        .rounded(px(8.0))
        .bg(to_hsla(p.pane))
        .border_1()
        .border_color(to_hsla(p.line.composite_over(p.pane)))
        .child(
            h_row()
                .w_full()
                .justify_between()
                .gap(px(8.0))
                .child(
                    div()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                        .text_color(to_hsla(p.ink))
                        .child(SharedString::from(heading)),
                )
                .child(status_badge(status, wait, cx)),
        )
        .children((steps > 0).then(|| {
            div()
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(SharedString::from(ch.gates_steps(steps)))
        }))
        .children(stamp.map(|s| {
            div()
                .text_xs()
                .text_color(to_hsla(p.ink_2))
                .child(SharedString::from(s))
        }))
}
