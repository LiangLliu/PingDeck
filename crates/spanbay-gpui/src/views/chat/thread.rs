//! Redesigned chat turn blocks: compact right-aligned user pills with hover actions (~75% max width)
//! and cohesive left-aligned, borderless assistant turns (`.overhaul/chat-redesign-spec.md` §3–§6).

use std::time::Instant;

use crate::ui::{
    StatusShape, Tone, UiIcon, alert_banner, badge, btn_icon, h_row, icon_xs, labeled_divider,
    markdown_view, mono_font_family, palette, status_indicator, streaming_markdown_view, to_hsla,
    v_col,
};
use gpui_kit::{
    Context, InteractiveElement as _, IntoElement, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px, relative,
};
use spanbay_desktop::core::agent::model::{AgentMessage, SkillUndeliveredReason};
use spanbay_desktop::core::model::hub::HubPart;
use spanbay_desktop::services::clipboard;
use spanbay_desktop::state::{
    AssistantTurnView, LiveToolCall, TranscriptDensity, TurnActivityItem,
};

use super::ChatView;
use super::live_turn::{
    render_live_turn_banners, render_plan_block, render_turn_changes_card, render_turn_footer,
};
use super::media::{render_citations, render_sources, render_user_attachments};
use super::metrics::{format_duration_ms, format_effort_short};
use super::reasoning::{render_reasoning_block, render_sealed_reasoning_row};
use super::run_topology_subagent::render_live_subagent_group;
use super::trace::{render_explore_group, render_turn_tool_call};
use super::types::ChatModal;

pub(super) fn render_pending_user_bubble(text: &str, cx: &gpui_kit::App) -> impl IntoElement {
    let p = palette(cx);
    v_col().w_full().items_end().child(
        div()
            .max_w(relative(0.75))
            .px(px(14.0))
            .py(px(8.0))
            .rounded(px(14.0))
            .bg(to_hsla(p.bubble_user))
            .border_1()
            .border_color(to_hsla(p.line.composite_over(p.bubble_user)))
            .child(markdown_view("chat-u-pending", text.to_owned())),
    )
}

pub(super) fn render_user_message(
    view: &ChatView,
    msg: &AgentMessage,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let text = extract_text_parts(&msg.parts);
    let txt_cp = text.clone();
    let txt_ed = text.clone();
    let mid_ed = msg.id.clone();
    let mid_br = msg.id.clone();
    let mid_del = msg.id.clone();
    let idle = !view.is_run_active();
    let turn_start = msg.is_turn_boundary();
    let group_name = SharedString::from(format!("user-turn-{}", msg.id));

    let actions_bar = h_row()
        .gap(px(2.0))
        .opacity(0.0)
        .group_hover(group_name.clone(), |s| s.opacity(1.0))
        .child(
            btn_icon(SharedString::from(format!("u-cp-{}", msg.id)), UiIcon::Copy)
                .tooltip(crate::ui::tr(cx).common().copy())
                .on_click(cx.listener(move |this, _, _, cx| {
                    let _ = clipboard::set_text(&txt_cp);
                    this.status_msg =
                        Some((Tone::Success, crate::ui::tr(cx).common().copied().into()));
                    cx.notify();
                })),
        )
        .children((idle && turn_start).then(|| {
            btn_icon(
                SharedString::from(format!("u-ed-{}", msg.id)),
                UiIcon::Pencil,
            )
            .tooltip(crate::ui::tr(cx).chat().thread_edit_rerun())
            .on_click(cx.listener(move |this, _, window, cx| {
                this.modal_textarea.update(cx, |s, cx| {
                    s.set_value(txt_ed.clone(), window, cx);
                });
                this.active_modal = Some(ChatModal::EditMessage {
                    message_id: mid_ed.clone(),
                });
                cx.notify();
            }))
        }))
        .children(turn_start.then(|| {
            btn_icon(
                SharedString::from(format!("u-br-{}", msg.id)),
                UiIcon::Branch,
            )
            .tooltip(crate::ui::tr(cx).chat().thread_branch_here())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.branch_from_message(mid_br.clone(), cx);
            }))
        }))
        .children(idle.then(|| {
            btn_icon(
                SharedString::from(format!("u-dl-{}", msg.id)),
                UiIcon::Trash,
            )
            .tooltip(crate::ui::tr(cx).chat().thread_delete_2())
            .on_click(cx.listener(move |this, _, _, cx| {
                this.delete_message(mid_del.clone(), cx);
            }))
        }));

    v_col()
        .id(SharedString::from(format!("u-turn-{}", msg.id)))
        .group(group_name)
        .w_full()
        .items_end()
        .gap(px(4.0))
        .children(render_user_attachments(view, &msg.attachments, cx))
        .child(
            h_row()
                .w_full()
                .justify_end()
                .gap(px(8.0))
                .child(actions_bar)
                .child(
                    div()
                        .max_w(relative(0.75))
                        .px(px(14.0))
                        .py(px(8.0))
                        .rounded(px(14.0))
                        .bg(to_hsla(p.bubble_user))
                        .border_1()
                        .border_color(to_hsla(p.line.composite_over(p.bubble_user)))
                        .child(markdown_view(
                            SharedString::from(format!("chat-u-{}", msg.id)),
                            text,
                        )),
                ),
        )
        .children(view.thread.skills_undelivered.get(&msg.id).map(|skills| {
            let ch = crate::ui::tr(cx).chat();
            let mut col = v_col().items_end().gap(px(2.0));
            for s in skills {
                let line = match s.reason {
                    SkillUndeliveredReason::Unavailable => {
                        ch.skills_undelivered_unavailable(&s.name)
                    }
                    SkillUndeliveredReason::Unreadable => ch.skills_undelivered_unreadable(&s.name),
                    SkillUndeliveredReason::TooLong => ch.skills_undelivered_too_long(&s.name),
                };
                col = col.child(
                    h_row()
                        .gap(px(5.0))
                        .text_xs()
                        .text_color(to_hsla(p.warn))
                        .child(status_indicator(StatusShape::Warn, cx))
                        .child(SharedString::from(line)),
                );
            }
            col
        }))
}

/// Returns whether `turn`'s activity spine is currently collapsed in `view`.
#[must_use]
pub fn is_turn_collapsed(view: &ChatView, turn: &AssistantTurnView) -> bool {
    if turn.is_live || turn.has_failure || turn.spine.is_empty() {
        return false;
    }
    let fold_key = format!("turn-fold-{}", turn.turn_id);
    turn.auto_collapsed ^ view.expanded_traces.contains(&fold_key)
}

/// Formats the 1-line summary for a collapsed turn's fold bar (`已工作 11.4s · 编辑 2个文件，已探索 3个文件，2次搜索`).
#[must_use]
pub fn format_turn_fold_summary(turn: &AssistantTurnView, cx: &gpui_kit::App) -> String {
    let ch = crate::ui::tr(cx).chat();
    let f = &turn.fold_stats;
    let dur = format_duration_ms(f.duration_ms);
    if f.edited_files > 0 {
        ch.turn_fold_all(&dur, f.edited_files, f.explored_files, f.searches)
    } else if f.explored_files > 0 || f.searches > 0 {
        ch.turn_fold_explore(&dur, f.explored_files, f.searches)
    } else {
        ch.turn_fold_tools(&dur, f.total_tools.max(1))
    }
}

/// Renders a cohesive, borderless, left-aligned assistant turn block (`AssistantTurnView`).
pub fn render_assistant_turn(
    view: &ChatView,
    turn: &AssistantTurnView,
    cx: &mut Context<ChatView>,
) -> impl IntoElement {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let mono = mono_font_family(cx);
    let collapsed = is_turn_collapsed(view, turn);
    let group_name = SharedString::from(format!("asst-turn-{}", turn.turn_id));

    let model_label = if turn.models_used.is_empty() {
        ch.thread_assistant().to_owned()
    } else {
        turn.models_used
            .iter()
            .map(|raw| view.format_raw_model_label(raw))
            .collect::<Vec<_>>()
            .join(" → ")
    };

    let header_timing = if turn.is_live {
        let steps = view.thread.active_run.as_ref().map_or_else(
            || turn.totals.step_count.max(1) as u32,
            |r| r.iterations.max(1),
        );
        let secs = view
            .run_started_at
            .map_or(turn.totals.duration_ms / 1_000, |s| {
                Instant::now().saturating_duration_since(s).as_secs()
            });
        Some(format!("· {}", ch.composer_step_elapsed(steps, secs)))
    } else if turn.totals.duration_ms > 0 {
        Some(format!("· {}", format_duration_ms(turn.totals.duration_ms)))
    } else {
        None
    };

    let can_fold_turn = !turn.is_live && !turn.has_failure && !turn.spine.is_empty();
    let tid_head_fold = turn.turn_id.clone();

    let mut turn_head = h_row()
        .id(SharedString::from(format!("turn-head-{}", turn.turn_id)))
        .w_full()
        .gap(px(6.0))
        .min_h(px(22.0))
        .child(
            div()
                .font_family(mono.clone())
                .text_xs()
                .font_weight(gpui_kit::FontWeight::SEMIBOLD)
                .text_color(to_hsla(p.ink_2))
                .child(SharedString::from(model_label)),
        )
        .children(turn.reasoning_effort.map(|eff| {
            badge(
                format_effort_short(eff, crate::ui::active_language(cx)),
                Tone::Neutral,
                cx,
            )
        }))
        .children(header_timing.map(|tm| {
            div()
                .font_family(mono.clone())
                .text_size(px(11.0))
                .text_color(to_hsla(p.faint))
                .child(SharedString::from(tm))
        }));
    if can_fold_turn {
        turn_head = turn_head
            .cursor_pointer()
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_turn_fold(tid_head_fold.clone(), cx);
            }));
    }

    let mut body = v_col()
        .id(SharedString::from(format!("asst-turn-{}", turn.turn_id)))
        .group(group_name.clone())
        .w_full()
        .gap(px(6.0));

    if turn.is_live
        && let Some(banners) = render_live_turn_banners(view, cx)
    {
        body = body.child(banners);
    }

    body = body.child(turn_head);

    if collapsed {
        let fold_summary = format_turn_fold_summary(turn, cx);
        let tid_fold = turn.turn_id.clone();
        let has_diff = turn.fold_stats.added_lines > 0 || turn.fold_stats.removed_lines > 0;
        let fold_bar = div()
            .id(SharedString::from(format!(
                "turn-fold-bar-{}",
                turn.turn_id
            )))
            .w_full()
            .flex()
            .flex_row()
            .items_center()
            .justify_between()
            .gap(px(8.0))
            .px(px(8.0))
            .py(px(4.5))
            .rounded(px(6.0))
            .bg(to_hsla(p.pane))
            .border_1()
            .border_color(to_hsla(p.line.composite_over(p.pane)))
            .cursor_pointer()
            .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.pane))))
            .on_click(cx.listener(move |this, _, _, cx| {
                this.toggle_turn_fold(tid_fold.clone(), cx);
            }))
            .child(
                h_row()
                    .flex_1()
                    .min_w_0()
                    .gap(px(7.0))
                    .child(status_indicator(StatusShape::Ok, cx))
                    .child(
                        div()
                            .flex_1()
                            .min_w_0()
                            .truncate()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.ink_2))
                            .child(SharedString::from(fold_summary)),
                    ),
            )
            .child(
                h_row()
                    .flex_shrink_0()
                    .gap(px(6.0))
                    .children(has_diff.then(|| {
                        h_row()
                            .gap(px(3.0))
                            .font_family(mono)
                            .text_size(px(11.0))
                            .child(div().text_color(to_hsla(p.ok)).child(SharedString::from(
                                format!("+{}", turn.fold_stats.added_lines),
                            )))
                            .child(div().text_color(to_hsla(p.bad)).child(SharedString::from(
                                format!("−{}", turn.fold_stats.removed_lines),
                            )))
                    }))
                    .child(
                        div()
                            .text_color(to_hsla(p.faint))
                            .child(icon_xs(UiIcon::ChevronRight)),
                    ),
            );
        body = body.child(fold_bar);
    } else if !turn.spine.is_empty() {
        let mut spine_col = v_col()
            .id(SharedString::from(format!("turn-spine-{}", turn.turn_id)))
            .w_full()
            .gap(px(2.0));
        for item in &turn.spine {
            match item {
                TurnActivityItem::Reasoning {
                    key,
                    text,
                    duration_ms,
                    is_live,
                    sealed,
                } => {
                    if *sealed {
                        spine_col = spine_col
                            .child(render_sealed_reasoning_row(key, cx).into_any_element());
                    } else {
                        let open = if *is_live {
                            !view.collapsed_live_reasoning
                        } else {
                            view.expanded_reasoning.contains(key)
                                || view.expanded_traces.contains(key)
                                || view.density == TranscriptDensity::Verbose
                        };
                        let k_click = key.clone();
                        let effective_dur = if *is_live {
                            view.thinking_started_at.map(|started| {
                                let end = view.thinking_ended_at.unwrap_or_else(Instant::now);
                                end.saturating_duration_since(started).as_millis() as u64
                            })
                        } else {
                            *duration_ms
                        };
                        spine_col = spine_col.child(
                            render_reasoning_block(
                                key.clone(),
                                open,
                                *is_live,
                                text.clone(),
                                effective_dur,
                                cx.listener(move |this, _, _, cx| {
                                    this.toggle_reasoning(k_click.clone(), cx)
                                }),
                                cx,
                            )
                            .into_any_element(),
                        );
                    }
                }
                TurnActivityItem::ExploreGroup(group) => {
                    spine_col = spine_col.child(render_explore_group(view, group, cx));
                }
                TurnActivityItem::ToolCall(tc) => {
                    spine_col = spine_col.child(render_turn_tool_call(view, tc, cx));
                }
                TurnActivityItem::SubagentGroup { calls, .. } => {
                    let refs: Vec<&LiveToolCall> = calls.iter().collect();
                    spine_col = spine_col
                        .child(render_live_subagent_group(view, &refs, cx).into_any_element());
                }
                TurnActivityItem::PlanBlock {
                    call_id,
                    args_json,
                    result_parts,
                    status,
                    actively_revising,
                    next_user_text,
                } => {
                    spine_col = spine_col.child(render_plan_block(
                        call_id,
                        args_json,
                        (!result_parts.is_empty()).then_some(result_parts.as_slice()),
                        *status,
                        *actively_revising,
                        next_user_text.as_deref(),
                        cx,
                    ));
                }
                TurnActivityItem::IntermediateText { key, markdown } => {
                    spine_col = spine_col.child(div().py(px(2.0)).child(markdown_view(
                        SharedString::from(format!("chat-inter-{key}")),
                        markdown.clone(),
                    )));
                }
                TurnActivityItem::SteerNote { message_id, text } => {
                    spine_col = spine_col.child(
                        div()
                            .id(SharedString::from(format!("steer-note-{message_id}")))
                            .w_full()
                            .px(px(8.0))
                            .py(px(4.0))
                            .rounded(px(5.0))
                            .bg(to_hsla(p.accent_soft))
                            .border_l_2()
                            .border_color(to_hsla(p.accent))
                            .text_xs()
                            .text_color(to_hsla(p.accent_ink))
                            .child(SharedString::from(ch.turn_steer_note(text))),
                    );
                }
                TurnActivityItem::CompactionDivider { .. } => {
                    spine_col = spine_col.child(labeled_divider(ch.thread_context_compacted(), cx));
                }
            }
        }
        body = body.child(spine_col);
    } else if turn.is_live && turn.final_markdown.is_empty() {
        body = body.child(
            h_row()
                .gap(px(8.0))
                .py(px(2.0))
                .text_xs()
                .text_color(to_hsla(p.muted))
                .child(status_indicator(StatusShape::Run, cx))
                .child(ch.thread_thinking()),
        );
    }

    if !turn.final_markdown.is_empty() {
        let md_id = SharedString::from(format!("chat-a-{}", turn.turn_id));
        let md_el = if turn.final_is_streaming {
            streaming_markdown_view(md_id, turn.final_markdown.clone()).into_any_element()
        } else {
            markdown_view(md_id, turn.final_markdown.clone()).into_any_element()
        };
        body = body.child(div().w_full().child(md_el));
    }

    if !turn.sources.is_empty() {
        body = body.child(render_sources(&turn.turn_id, &turn.sources, cx));
    } else if !turn.citations.is_empty() {
        body = body.child(render_citations(&turn.turn_id, &turn.citations, cx));
    }

    if let Some(changes_card) = render_turn_changes_card(turn, cx) {
        body = body.child(changes_card);
    }

    if let Some(err) = &turn.inline_error {
        body = body.child(alert_banner(
            Tone::Danger,
            SharedString::from(err.clone()),
            None,
            cx,
        ));
    }

    if !turn.is_live {
        body = body.child(render_turn_footer(view, turn, group_name, cx));
    }

    body
}

fn extract_text_parts(parts: &[HubPart]) -> String {
    parts
        .iter()
        .filter_map(|p| match p {
            HubPart::Text { text, .. } => Some(text.as_str()),
            _ => None,
        })
        .collect::<Vec<_>>()
        .join("\n")
}
