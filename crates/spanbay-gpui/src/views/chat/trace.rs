//! Structured tool-call spine rows and `ExploreGroup` trees for [`AssistantTurnView`]
//! (`docs/design.md`, `.overhaul/chat-redesign-spec.md` §3.2, §4, §5.2).
//!
//! - Single status glyph on the left of every step row, chevron (`›` / `⌄`) at the right end.
//! - Consecutive read-only calls render as one expandable `ExploreGroup` (`▸ 已探索 N个文件 · M次搜索`)
//!   with `└` tree sub-rows.
//! - File edit rows expose `编辑 path +X −Y [查看 Diff ›]` and open the right Review Panel.
//! - Shell rows hide exit code 0 and collapse output by default, while non-zero exit codes / failed
//!   or blocked calls auto-expand their error output.

use gpui_kit::{
    Context, InteractiveElement as _, ParentElement as _, SharedString,
    StatefulInteractiveElement as _, Styled as _, div, px,
};
use spanbay_desktop::state::{
    ExploreGroupView, GraphNodeStatus, ToolCategory, TranscriptDensity, TurnToolCallItem,
};

use crate::ui::{
    StatusShape, Tone, UiIcon, WaitKind, badge, btn_ghost, collapsible_box, exit_code_badge, h_row,
    icon_xs, mono_font_family, palette, status_indicator, status_label, status_visual,
    status_word_color, to_hsla, v_col,
};

use super::ChatView;
use super::media::render_result_media;
use super::metrics::format_duration_ms;
use super::run_actions::resolution_label;
use super::trace_detail::{
    extract_denied_reason, format_tech_details, localized_tool_error, localized_tool_verb,
    render_coded_error_action, render_inline_metric, render_interleaved_streams,
    render_trace_detail,
};
use super::types::ChatHandoff;

/// Returns whether a [`TurnToolCallItem`] is currently expanded in `view`.
#[must_use]
pub fn is_tool_call_expanded(view: &ChatView, item: &TurnToolCallItem) -> bool {
    let default_open = item.should_auto_expand()
        || (view.density == TranscriptDensity::Verbose
            && item.pres.category != ToolCategory::FileRead);
    default_open ^ view.expanded_traces.contains(&item.call_id)
}
/// Renders a merged read-only `ExploreGroup` (`▸ 已探索 N个文件 · M次搜索`) and its `└` sub-tree.
pub fn render_explore_group(
    view: &ChatView,
    group: &ExploreGroupView,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let mono = mono_font_family(cx);
    let default_open = view.density == TranscriptDensity::Verbose;
    let open = default_open ^ view.expanded_traces.contains(&group.key);
    let head_text = if group.running {
        ch.turn_explore_running(group.files_read, group.searches_run)
    } else {
        ch.turn_explore_done(group.files_read, group.searches_run)
    };
    let key_toggle = group.key.clone();

    let header = div()
        .id(SharedString::from(format!("trace-exp-{}", group.key)))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .px(px(6.0))
        .py(px(3.5))
        .rounded(px(5.0))
        .cursor_pointer()
        .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.surface))))
        .on_click(cx.listener(move |this, _, _, cx| this.toggle_trace(key_toggle.clone(), cx)))
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(7.0))
                .child(status_indicator(
                    if group.running {
                        StatusShape::Run
                    } else {
                        StatusShape::Ok
                    },
                    cx,
                ))
                .child(
                    div()
                        .flex_1()
                        .min_w_0()
                        .truncate()
                        .text_xs()
                        .font_weight(gpui_kit::FontWeight::MEDIUM)
                        .text_color(to_hsla(p.ink_2))
                        .child(SharedString::from(head_text)),
                ),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(6.0))
                .children(group.duration_ms.map(|ms| {
                    div()
                        .font_family(mono.clone())
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(format_duration_ms(ms)))
                }))
                .child(div().text_color(to_hsla(p.faint)).child(icon_xs(if open {
                    UiIcon::ChevronDown
                } else {
                    UiIcon::ChevronRight
                }))),
        );

    let mut col = v_col().w_full().gap(px(2.0)).child(header);
    if open {
        let mut tree = v_col()
            .w_full()
            .ml(px(9.0))
            .pl(px(10.0))
            .py(px(2.0))
            .gap(px(1.5))
            .border_l_1()
            .border_color(to_hsla(p.line.composite_over(p.surface)));
        let has_workspace = view
            .session
            .as_ref()
            .and_then(|s| s.workspace_dir.as_deref())
            .is_some();

        for item in &group.items {
            let sub_open = view.expanded_traces.contains(&item.call_id);
            let verb = localized_tool_verb(&item.tool_name, cx);
            let cid = item.call_id.clone();
            let metric_el = item
                .pres
                .metric
                .as_ref()
                .map(|m| render_inline_metric(m, cx));

            let sub_row =
                div()
                    .id(SharedString::from(format!("trace-step-{}", item.call_id)))
                    .w_full()
                    .flex()
                    .flex_row()
                    .items_center()
                    .justify_between()
                    .gap(px(6.0))
                    .px(px(6.0))
                    .py(px(2.5))
                    .rounded(px(4.0))
                    .cursor_pointer()
                    .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.surface))))
                    .on_click(cx.listener(move |this, _, _, cx| this.toggle_trace(cid.clone(), cx)))
                    .child(
                        h_row()
                            .flex_1()
                            .min_w_0()
                            .gap(px(6.0))
                            .child(
                                div()
                                    .font_family(mono.clone())
                                    .text_xs()
                                    .text_color(to_hsla(p.faint))
                                    .child("└"),
                            )
                            .child(
                                div()
                                    .flex_shrink_0()
                                    .text_xs()
                                    .text_color(to_hsla(p.ink_2))
                                    .child(SharedString::from(verb.clone())),
                            )
                            .children((!item.pres.target.is_empty()).then(|| {
                                div()
                                    .flex_1()
                                    .min_w_0()
                                    .truncate()
                                    .font_family(mono.clone())
                                    .text_xs()
                                    .text_color(to_hsla(p.ink))
                                    .child(SharedString::from(item.pres.target.clone()))
                            })),
                    )
                    .child(
                        h_row()
                            .flex_shrink_0()
                            .gap(px(6.0))
                            .children(metric_el)
                            .children(item.duration_ms.map(|ms| {
                                div()
                                    .font_family(mono.clone())
                                    .text_size(px(11.0))
                                    .text_color(to_hsla(p.faint))
                                    .child(SharedString::from(format_duration_ms(ms)))
                            }))
                            .child(div().text_color(to_hsla(p.faint)).child(icon_xs(
                                if sub_open {
                                    UiIcon::ChevronDown
                                } else {
                                    UiIcon::ChevronRight
                                },
                            ))),
                    );
            tree = tree.child(sub_row);
            if sub_open {
                tree = tree.child(render_trace_detail(
                    &item.call_id,
                    &item.tool_name,
                    &verb,
                    &item.pres,
                    Tone::Neutral,
                    item.spilled,
                    has_workspace,
                    item.change_ref.clone(),
                    cx,
                ));
            }
        }
        col = col.child(tree);
    }
    col
}

/// Renders one single-line tool call row on the turn spine and its expandable detail body.
pub fn render_turn_tool_call(
    view: &ChatView,
    item: &TurnToolCallItem,
    cx: &mut Context<ChatView>,
) -> gpui_kit::Div {
    let p = palette(cx);
    let ch = crate::ui::tr(cx).chat();
    let mono = mono_font_family(cx);
    let call_id = item.call_id.as_str();
    let expanded = is_tool_call_expanded(view, item);
    let has_workspace = view
        .session
        .as_ref()
        .and_then(|s| s.workspace_dir.as_deref())
        .is_some();

    let base_verb = localized_tool_verb(&item.tool_name, cx);
    let (shape, tone) = status_visual(item.status);
    let show_word = !matches!(
        item.status,
        GraphNodeStatus::Running | GraphNodeStatus::Completed
    );
    let wait = item.is_waiting_approval.then_some(WaitKind::Approval);
    let word_fg = status_word_color(tone, cx);
    let coded_sentence = item
        .pres
        .error_code
        .as_deref()
        .and_then(|code| localized_tool_error(code, &base_verb, cx));
    let coded_action = item
        .pres
        .error_code
        .as_deref()
        .and_then(|code| render_coded_error_action(view, call_id, code, cx));
    let metric_el = item
        .pres
        .metric
        .as_ref()
        .map(|m| render_inline_metric(m, cx));

    let is_file_edit = item.pres.category == ToolCategory::FileEdit;
    let is_shell = item.pres.category == ToolCategory::Shell;

    let display_verb = if is_shell {
        match item.status {
            GraphNodeStatus::Running => ch.turn_shell_running().to_owned(),
            GraphNodeStatus::Failed => ch.turn_shell_failed().to_owned(),
            _ => ch.turn_shell_ran().to_owned(),
        }
    } else if is_file_edit && item.edit_count > 1 {
        ch.turn_edit_multi(&item.pres.target, item.edit_count)
    } else {
        base_verb.clone()
    };

    let target_text = if is_file_edit && item.edit_count > 1 {
        String::new()
    } else if is_shell && !item.pres.target.is_empty() {
        format!("$ {}", item.pres.target)
    } else {
        item.pres.target.clone()
    };

    let review_file_path = is_file_edit.then(|| {
        item.pres
            .file_path
            .clone()
            .or_else(|| (!item.pres.target.is_empty()).then(|| item.pres.target.clone()))
    });

    let cid_toggle = item.call_id.clone();
    let row_header = div()
        .id(SharedString::from(format!("trace-step-{call_id}")))
        .w_full()
        .flex()
        .flex_row()
        .items_center()
        .justify_between()
        .gap(px(8.0))
        .px(px(6.0))
        .py(px(3.5))
        .rounded(px(5.0))
        .cursor_pointer()
        .hover(|s| s.bg(to_hsla(p.hover.composite_over(p.surface))))
        .on_click(cx.listener(move |this, _, _, cx| this.toggle_trace(cid_toggle.clone(), cx)))
        .child(
            h_row()
                .flex_1()
                .min_w_0()
                .gap(px(7.0))
                .child(status_indicator(shape, cx))
                .children(if let Some(sentence) = coded_sentence {
                    vec![
                        div()
                            .flex_1()
                            .min_w_0()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.ink))
                            .truncate()
                            .child(SharedString::from(sentence)),
                    ]
                } else {
                    let mut parts = vec![
                        div()
                            .flex_shrink_0()
                            .text_xs()
                            .font_weight(gpui_kit::FontWeight::MEDIUM)
                            .text_color(to_hsla(p.ink_2))
                            .child(SharedString::from(display_verb)),
                    ];
                    if !target_text.is_empty() {
                        parts.push(
                            div()
                                .flex_1()
                                .min_w_0()
                                .font_family(mono.clone())
                                .text_xs()
                                .text_color(to_hsla(p.ink))
                                .truncate()
                                .child(SharedString::from(target_text)),
                        );
                    }
                    parts
                }),
        )
        .child(
            h_row()
                .flex_shrink_0()
                .gap(px(6.0))
                .children(coded_action)
                .children(metric_el)
                .children(review_file_path.map(|file_opt| {
                    btn_ghost(
                        SharedString::from(format!("trace-open-rv-{call_id}")),
                        format!("{} ›", ch.trace_view_diff()),
                    )
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.open_review(file_opt.clone(), cx);
                    }))
                }))
                .children(item.request_link.clone().map(|(ws_id, req_id)| {
                    btn_ghost(
                        SharedString::from(format!("trace-inline-req-{call_id}")),
                        ch.trace_open_request(),
                    )
                    .icon(icon_xs(UiIcon::ExternalLink))
                    .on_click(cx.listener(move |this, _, _, cx| {
                        cx.stop_propagation();
                        this.pending_handoff.set(Some(ChatHandoff::OpenRequest {
                            workspace_id: ws_id.clone(),
                            request_id: req_id.clone(),
                        }));
                        cx.notify();
                    }))
                }))
                .children(show_word.then(|| {
                    div()
                        .text_xs()
                        .text_color(word_fg)
                        .child(status_label(item.status, wait, cx))
                }))
                .children(
                    item.resolution
                        .and_then(|r| resolution_label(r, item.status, cx))
                        .map(|l| badge(l, Tone::Neutral, cx)),
                )
                .children(
                    item.exit_code
                        .filter(|c| *c != 0)
                        .map(|c| exit_code_badge(c, cx)),
                )
                .children(item.duration_ms.map(|ms| {
                    div()
                        .font_family(mono.clone())
                        .text_size(px(11.0))
                        .text_color(to_hsla(p.faint))
                        .child(SharedString::from(format_duration_ms(ms)))
                }))
                .child(
                    div()
                        .text_color(to_hsla(p.faint))
                        .child(icon_xs(if expanded {
                            UiIcon::ChevronDown
                        } else {
                            UiIcon::ChevronRight
                        })),
                ),
        );

    let mut el = v_col().w_full().gap(px(2.0)).child(row_header);

    if expanded {
        if item.status == GraphNodeStatus::Denied {
            let (reason, nudge) = extract_denied_reason(&item.result_parts, &item.streams);
            if let Some(reason) = reason {
                el = el.child(
                    div()
                        .px(px(12.0))
                        .pt(px(2.0))
                        .pb(px(2.0))
                        .text_xs()
                        .text_color(to_hsla(p.ink_2))
                        .child(SharedString::from(ch.trace_denied_reason(reason))),
                );
            }
            if let Some(nudge) = nudge {
                el = el.child(
                    h_row()
                        .px(px(12.0))
                        .pb(px(4.0))
                        .gap(px(6.0))
                        .text_xs()
                        .text_color(to_hsla(p.muted))
                        .child(status_indicator(StatusShape::Idle, cx))
                        .child(SharedString::from(nudge)),
                );
            }
        }
        el = el.child(render_trace_detail(
            call_id,
            &item.tool_name,
            &base_verb,
            &item.pres,
            tone,
            item.spilled,
            has_workspace,
            item.change_ref.clone(),
            cx,
        ));
        if !item.streams.is_empty() && item.status != GraphNodeStatus::Denied {
            el = el.child(render_interleaved_streams(&item.streams, cx));
        }
        if let Some(media) = render_result_media(view, call_id, &item.result_parts, cx) {
            el = el.child(div().px(px(10.0)).pb(px(6.0)).child(media));
        }
        if let Some(tech_text) = format_tech_details(
            item.exit_code,
            item.audit.as_ref(),
            item.resolution,
            item.raw_result_first_line.as_deref(),
        ) {
            let tech_key = format!("tech-{call_id}");
            let tech_open = view.expanded_traces.contains(&tech_key);
            let k = tech_key.clone();
            el = el.child(
                v_col()
                    .px(px(12.0))
                    .pb(px(6.0))
                    .gap(px(4.0))
                    .child(
                        btn_ghost(
                            SharedString::from(format!("trace-tech-btn-{call_id}")),
                            ch.trace_tech_details(),
                        )
                        .icon(icon_xs(if tech_open {
                            UiIcon::ChevronDown
                        } else {
                            UiIcon::ChevronRight
                        }))
                        .on_click(
                            cx.listener(move |this, _, _, cx| this.toggle_trace(k.clone(), cx)),
                        ),
                    )
                    .child(
                        collapsible_box(
                            tech_open,
                            SharedString::from(format!("trace-tech-col-{call_id}")),
                        )
                        .content(
                            div()
                                .px(px(8.0))
                                .py(px(6.0))
                                .rounded(px(6.0))
                                .bg(to_hsla(p.sunken))
                                .font_family(mono)
                                .text_size(px(11.0))
                                .text_color(to_hsla(p.muted))
                                .child(SharedString::from(tech_text)),
                        ),
                    ),
            );
        }
    }

    el
}
